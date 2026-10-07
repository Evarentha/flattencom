/*
 * flattencom - Core Decode Cmd
 *
 * Runs JSONL decoder subprocesses with bounded queues, timeouts and failure isolation.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

//! Process decoder (`cmd`): implement a plugin in any language without rebuilding the application.
//!
//! ## Plugin protocol (JSONL over stdin/stdout)
//!
//! Input: one line per frame.
//! `{"hex":"41540D","dir":"rx","t_us":1790000000000000,"mono_us":1200}`
//!
//! Output: exactly one reply per input line, optionally an empty result.
//! `{"text":"AT␍","level":"info","fields":{"proto":"at"}}`
//! - `text`: display text, empty by default
//! - `level`: `info`, `warn` or `error`; defaults to `info`
//! - `fields`: name-to-value object, empty by default
//!
//! A `null` output line means no decoding result for this frame.
//! Plugins must reply within their bounded timeout; waiting delays frame processing.
//! Process exit or IO failure disables the plugin without closing the serial session.
//! Plugins run in a Unix process group or Windows Job Object. Disabling or dropping
//! a decoder terminates its group/job, reaps the child and joins the I/O worker.
//! Unix pipe I/O is cancellable even if a descendant creates a separate session.

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use process_wrap::std::{ChildWrapper, CommandWrap};
use serde::{Deserialize, Serialize};

use crate::FlattenError;
use crate::decode::{ChunkCtx, Decoder};
use crate::frame::{DecodeField, DecodeLevel, DecodedInfo};

/// Process-plugin decoder.
pub struct CmdDecoder {
    program: String,
    child: Option<PluginProcess>,
    worker: Option<std::thread::JoinHandle<()>>,
    cancelled: Arc<AtomicBool>,
    dead: bool,
    input: Option<SyncSender<String>>,
    output: Receiver<Result<String, String>>,
    timeout: Duration,
}

/// Own process-tree cleanup even when pipe setup or worker creation fails.
struct PluginProcess(Box<dyn ChildWrapper>);

impl Drop for PluginProcess {
    fn drop(&mut self) {
        let _ = self.0.start_kill();
        let _ = self.0.wait();
    }
}

#[derive(Serialize)]
struct PluginInput<'a> {
    hex: &'a str,
    dir: &'static str,
    t_us: i64,
    mono_us: u64,
}

#[derive(Deserialize, Default)]
struct PluginOutput {
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    level: Option<String>,
    #[serde(default)]
    fields: Option<std::collections::BTreeMap<String, String>>,
}

impl CmdDecoder {
    /// Start the plugin process using its options.
    ///
    /// Options: `{"program": "python3", "args": ["my_decode.py"]}`
    /// `args` is optional; resolve the program through `PATH`.
    pub fn spawn(options: &serde_json::Value) -> Result<Self, FlattenError> {
        let program = options
            .get("program")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned();
        if program.is_empty() {
            return Err(FlattenError::InvalidConfig {
                field: "options.program".into(),
                reason: crate::i18n::text("The cmd decoder requires a program path").into(),
            });
        }
        let args: Vec<String> = options
            .get("args")
            .and_then(serde_json::Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        let mut cmd = Command::new(&program);
        cmd.args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut cmd = CommandWrap::from(cmd);
        #[cfg(unix)]
        cmd.wrap(process_wrap::std::ProcessGroup::leader());
        // Assign the suspended child to a job before it can spawn descendants.
        #[cfg(windows)]
        {
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            // The wrapped PROCESS_CREATION_FLAGS type belongs to the windows crate,
            // which this crate does not depend on, so its `default()` is unnameable.
            #[allow(clippy::default_trait_access)]
            let mut flags = process_wrap::std::CreationFlags(Default::default());
            flags.0.0 = CREATE_NO_WINDOW;
            cmd.wrap(flags);
            cmd.wrap(process_wrap::std::JobObject);
        }
        let mut child = PluginProcess(cmd.spawn().map_err(|e| {
            FlattenError::Decode(crate::tr!("Failed to start plugin {program:?}: {e}. Check that the file exists and is executable.", e = e, program = program))
        })?);
        let mut stdin = child
            .0
            .stdin()
            .take()
            .ok_or_else(|| FlattenError::Decode("plugin stdin unavailable".into()))?;
        let mut stdout = child
            .0
            .stdout()
            .take()
            .ok_or_else(|| FlattenError::Decode("plugin stdout unavailable".into()))?;
        #[cfg(unix)]
        {
            set_nonblocking(&stdin).map_err(|e| FlattenError::Decode(e.to_string()))?;
            set_nonblocking(&stdout).map_err(|e| FlattenError::Decode(e.to_string()))?;
        }
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancelled = cancelled.clone();
        let (input, requests) = mpsc::sync_channel::<String>(1);
        let (results, output) = mpsc::sync_channel(1);
        let worker = std::thread::Builder::new()
            .name("flattencom-decoder-io".into())
            .spawn(move || {
                let mut pending = Vec::new();
                while let Ok(line) = requests.recv() {
                    let result = (|| -> Result<String, String> {
                        let mut request = line.into_bytes();
                        request.push(b'\n');
                        let mut written = 0;
                        while written < request.len() {
                            let n = cancellable_io(&worker_cancelled, || {
                                stdin.write(&request[written..])
                            })?;
                            if n == 0 {
                                return Err("plugin stdin closed".into());
                            }
                            written += n;
                        }
                        let bytes = loop {
                            if let Some(end) = pending.iter().position(|&b| b == b'\n') {
                                break pending.drain(..=end).collect();
                            }
                            if pending.len() >= 1024 * 1024 {
                                return Err("plugin response exceeds 1 MiB".into());
                            }
                            let mut chunk = [0; 4096];
                            let limit = chunk.len().min(1024 * 1024 - pending.len());
                            let n = cancellable_io(&worker_cancelled, || {
                                stdout.read(&mut chunk[..limit])
                            })?;
                            if n == 0 {
                                return Err("plugin EOF".into());
                            }
                            pending.extend_from_slice(&chunk[..n]);
                        };
                        String::from_utf8(bytes).map_err(|e| e.to_string())
                    })();
                    if results.send(result).is_err() {
                        break;
                    }
                }
            })
            .map_err(|e| FlattenError::Decode(e.to_string()))?;
        let timeout = Duration::from_millis(
            options
                .get("timeout_ms")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(200)
                .clamp(1, 2000),
        );
        Ok(Self {
            program,
            child: Some(child),
            worker: Some(worker),
            cancelled,
            dead: false,
            input: Some(input),
            output,
            timeout,
        })
    }

    fn mark_dead(&mut self, reason: &str) -> Option<DecodedInfo> {
        if self.dead {
            None
        } else {
            self.dead = true;
            self.shutdown();
            tracing::warn!(plugin = %self.program, %reason, "Decoder plugin disabled");
            Some(DecodedInfo::error(
                "cmd",
                crate::tr!(
                    "Plugin {prog} disabled: {reason}",
                    prog = self.program,
                    reason = reason
                ),
                Vec::new(),
            ))
        }
    }

    fn shutdown(&mut self) {
        self.cancelled.store(true, Ordering::Relaxed);
        self.input.take();
        // Terminate the group/job; Unix I/O also checks cancellation when a
        // descendant escaped the group and retained a pipe. Consume the process
        // handle once to avoid signalling a reused group ID.
        self.child.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(unix)]
fn set_nonblocking(fd: &impl std::os::fd::AsFd) -> std::io::Result<()> {
    use nix_io::fcntl::{FcntlArg, OFlag, fcntl};
    let flags = OFlag::from_bits_truncate(fcntl(fd, FcntlArg::F_GETFL)?);
    fcntl(fd, FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK))?;
    Ok(())
}

fn cancellable_io(
    cancelled: &AtomicBool,
    mut io: impl FnMut() -> std::io::Result<usize>,
) -> Result<usize, String> {
    loop {
        if cancelled.load(Ordering::Relaxed) {
            return Err("plugin I/O cancelled".into());
        }
        match io() {
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(2));
            }
            result => return result.map_err(|e| e.to_string()),
        }
    }
}

impl Decoder for CmdDecoder {
    fn id(&self) -> &'static str {
        "cmd"
    }

    fn feed(&mut self, ctx: &ChunkCtx<'_>) -> Option<DecodedInfo> {
        if self.dead {
            return None;
        }
        let input = PluginInput {
            hex: &hex::encode(ctx.data),
            dir: ctx.dir.as_str(),
            t_us: ctx.t_us,
            mono_us: ctx.mono_us,
        };
        let line = serde_json::to_string(&input)
            .map_err(|e| FlattenError::internal(e.to_string()))
            .ok()?;
        if self
            .input
            .as_ref()
            .is_none_or(|input| input.try_send(line).is_err())
        {
            return self.mark_dead("Plugin request queue unavailable");
        }
        let buf = match self.output.recv_timeout(self.timeout) {
            Ok(Ok(line)) => line,
            Ok(Err(e)) => return self.mark_dead(&e),
            Err(e) => return self.mark_dead(&crate::tr!("Plugin timed out or exited: {e}", e = e)),
        };
        if buf.trim() == "null" {
            return None;
        }
        let out: PluginOutput = match serde_json::from_str(buf.trim()) {
            Ok(o) => o,
            Err(e) => {
                let snippet: String = buf.trim().chars().take(64).collect();
                return Some(DecodedInfo::error(
                    "cmd",
                    crate::tr!(
                        "Invalid JSON from plugin: {e}; output: {snippet}",
                        e = e,
                        snippet = snippet
                    ),
                    Vec::new(),
                ));
            }
        };
        let text = out.text.unwrap_or_default();
        if text.is_empty()
            && out
                .fields
                .as_ref()
                .is_none_or(std::collections::BTreeMap::is_empty)
        {
            return None; // Empty result: produce nothing.
        }
        let level = match out.level.as_deref() {
            Some("warn") => DecodeLevel::Warn,
            Some("error") => DecodeLevel::Error,
            _ => DecodeLevel::Info,
        };
        let fields = out
            .fields
            .unwrap_or_default()
            .into_iter()
            .map(|(name, value)| DecodeField { name, value })
            .collect();
        Some(DecodedInfo {
            decoder: "cmd".into(),
            level,
            text,
            fields,
        })
    }
}

impl Drop for CmdDecoder {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::Direction;

    #[cfg(target_os = "linux")]
    fn ctx(data: &[u8]) -> ChunkCtx<'_> {
        ChunkCtx {
            dir: Direction::Rx,
            t_us: 0,
            mono_us: 0,
            data,
        }
    }

    /// Portable echo-plugin specification for Windows/Linux tests.
    /// The helper reads one line and replies with one line; avoid relying on optional system tools.
    /// Use available Python or shell helpers rather than assuming rustc exists in the runtime environment.
    #[cfg(target_os = "linux")]
    fn echo_plugin_spec() -> serde_json::Value {
        let py = r#"
import sys, json
for line in sys.stdin:
    line = line.strip()
    if not line: continue
    try:
        obj = json.loads(line)
    except Exception:
        print(json.dumps({"text": "bad input", "level": "error"})); sys.stdout.flush(); continue
    print(json.dumps({"text": "RX:%d 字节" % (len(bytes.fromhex(obj["hex"]))), "fields": {"len": str(len(bytes.fromhex(obj["hex"])))}}))
    sys.stdout.flush()
"#;
        serde_json::json!({
            "program": "python3",
            "args": ["-c", py],
        })
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn 插件_解码往返() {
        let options = echo_plugin_spec();
        let mut d = CmdDecoder::spawn(&options).expect("python3 插件启动失败(环境需 python3)");
        let info = d.feed(&ctx(b"hello")).unwrap();
        assert_eq!(info.text, "RX:5 字节");
        assert!(
            info.fields
                .iter()
                .any(|f| f.name == "len" && f.value == "5")
        );
        assert_eq!(info.level, DecodeLevel::Info);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn stalled_plugin_is_disabled_within_its_budget() {
        let mut decoder = CmdDecoder::spawn(&serde_json::json!({
            "program": "python3", "args": ["-c", "import time; time.sleep(60)"], "timeout_ms": 50,
        }))
        .unwrap();
        let pid = decoder.child.as_ref().unwrap().0.id();
        let started = std::time::Instant::now();
        assert_eq!(
            decoder.feed(&ctx(b"test")).unwrap().level,
            DecodeLevel::Error
        );
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(decoder.feed(&ctx(b"test")).is_none());
        assert!(decoder.input.is_none());
        assert!(decoder.worker.is_none());
        // A killed but unreaped child still has a /proc entry on Linux.
        assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn inherited_stdout_descendant_is_terminated_on_timeout_and_drop() {
        let program = r"
import json, subprocess, sys, time
sys.stdin.readline()
child = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(60)'])
print(json.dumps({'text': str(child.pid)}), flush=True)
if sys.argv[1] == 'stall':
    time.sleep(60)
";
        for (timeout, exit_parent) in [(false, false), (true, false), (true, true)] {
            let mut decoder = CmdDecoder::spawn(&serde_json::json!({
                "program": "python3",
                "args": ["-c", program, if exit_parent { "exit" } else { "stall" }],
                "timeout_ms": 1000,
            }))
            .unwrap();
            let descendant: u32 = decoder.feed(&ctx(b"ready")).unwrap().text.parse().unwrap();
            let leader = decoder.child.as_ref().unwrap().0.id();
            let started = std::time::Instant::now();
            if timeout {
                decoder.timeout = Duration::from_millis(25);
                assert_eq!(
                    decoder.feed(&ctx(b"stall")).unwrap().level,
                    DecodeLevel::Error
                );
                assert!(decoder.worker.is_none());
                assert!(decoder.child.is_none());
            }
            drop(decoder);
            assert!(started.elapsed() < Duration::from_secs(2));
            assert!(!std::path::Path::new(&format!("/proc/{leader}")).exists());
            // Orphans are reaped by init, which may run after the group signal
            // returns, so poll: the descendant must stop executing and reach a
            // zombie state (or disappear) within the deadline.
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            loop {
                match std::fs::read_to_string(format!("/proc/{descendant}/stat")) {
                    Err(_) => break,
                    Ok(status) => {
                        let state = status.rsplit_once(") ").unwrap().1;
                        if state.starts_with('Z') {
                            break;
                        }
                        assert!(
                            std::time::Instant::now() < deadline,
                            "descendant still executing after the group was killed: {status}"
                        );
                        std::thread::sleep(Duration::from_millis(5));
                    }
                }
            }
        }
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn escaped_descendant_cannot_hold_worker_open() {
        let program = r"
import json, subprocess, sys, time
sys.stdin.readline()
child = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(60)'], start_new_session=True)
print(json.dumps({'text': str(child.pid)}), flush=True)
time.sleep(60)
";
        // Cover both a pending stdout read and stdin backpressure beyond pipe capacity.
        for payload_size in [5, 1024 * 1024] {
            let mut decoder = CmdDecoder::spawn(&serde_json::json!({
                "program": "python3",
                "args": ["-c", program],
                "timeout_ms": 1000,
            }))
            .unwrap();
            let descendant = decoder.feed(&ctx(b"ready")).unwrap().text;
            decoder.timeout = Duration::from_millis(25);
            let (done, completion) = mpsc::channel();
            let worker = std::thread::spawn(move || {
                let result = decoder.feed(&ctx(&vec![b'x'; payload_size])).unwrap();
                assert_eq!(result.level, DecodeLevel::Error);
                assert!(decoder.worker.is_none());
                drop(decoder);
                done.send(()).unwrap();
            });
            let bounded = completion.recv_timeout(Duration::from_secs(2)).is_ok();
            // The escaped session is deliberately outside our process group. Clean
            // it up only after observing whether decoder cleanup completed unaided.
            let _ = Command::new("kill").args(["-KILL", &descendant]).status();
            worker.join().unwrap();
            assert!(bounded, "escaped stdout owner blocked decoder cleanup");
        }
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn 插件_程序不存在报错() {
        let options = serde_json::json!({ "program": "definitely-not-exist-xyz", "args": [] });
        assert!(CmdDecoder::spawn(&options).is_err());
    }

    #[test]
    fn 插件_缺_program_报错() {
        assert!(CmdDecoder::spawn(&serde_json::json!({})).is_err());
    }

    #[test]
    fn 插件_dir_序列化() {
        let i = PluginInput {
            hex: "41",
            dir: Direction::Tx.as_str(),
            t_us: 1,
            mono_us: 2,
        };
        let s = serde_json::to_string(&i).unwrap();
        assert!(s.contains(r#""dir":"tx""#));
    }
}
