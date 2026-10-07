/*
 * flattencom - CLI Cmd
 *
 * Dispatches CLI commands and maps core or service errors to process exit codes.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

//! Command dispatch and implementations.
//!
//! Direct commands embed the core engine; shared commands use daemon RPC.
//! Override the endpoint with `--socket`; disable automatic startup with `FLATTENCOM_NO_AUTOSPAWN=1`.

#![allow(clippy::too_many_lines)]

pub mod attach;
pub mod autobaud;
pub mod daemon;
pub mod decode;
pub mod list;
pub mod receive;
pub mod replay;
pub mod send;
pub mod sessions;
pub mod watch;

use std::io::Write;

use clap::CommandFactory;
use flattencom_core::FlattenError;
use flattencom_proto::client::{ConnectConfig, DaemonClient};

use crate::Commands;
use crate::exit_code;

/// Command error: readable message and exit code; see main.rs for exit-code conventions.
#[derive(Debug, Clone)]
pub struct CmdError {
    /// Message including recovery guidance.
    pub message: String,
    /// Process exit code.
    pub code: i32,
}

impl CmdError {
    /// Construct an error, classifying its exit code from the message.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        let m = message.into();
        let code = classify(&m);
        Self {
            message: flattencom_core::i18n::text(&m).to_owned(),
            code,
        }
    }

    /// Construct an error with an explicit exit code.
    #[must_use]
    pub fn with_code(message: impl Into<String>, code: i32) -> Self {
        Self {
            message: {
                let message = message.into();
                flattencom_core::i18n::text(&message).to_owned()
            },
            code,
        }
    }

    /// Convert a core error while preserving its category.
    #[must_use]
    pub fn from_core(e: FlattenError) -> Self {
        let code = match e {
            FlattenError::PortBusy { .. } => exit_code::BUSY,
            FlattenError::PermissionDenied { .. } => exit_code::PERMISSION,
            FlattenError::Timeout(_) => exit_code::TIMEOUT,
            FlattenError::InvalidConfig { .. } => exit_code::ARGS,
            _ => exit_code::FAIL,
        };
        Self {
            message: e.to_string(),
            code,
        }
    }
}

/// Classify the exit code from the message text.
fn classify(message: &str) -> i32 {
    let l = message.to_lowercase();
    if l.contains("占用") || l.contains("busy") {
        exit_code::BUSY
    } else if l.contains("权限") || l.contains("permission") {
        exit_code::PERMISSION
    } else if l.contains("超时") || l.contains("timeout") || l.contains("无响应") {
        exit_code::TIMEOUT
    } else {
        exit_code::FAIL
    }
}

impl std::fmt::Display for CmdError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl From<String> for CmdError {
    fn from(s: String) -> Self {
        Self::new(s)
    }
}

impl From<FlattenError> for CmdError {
    fn from(e: FlattenError) -> Self {
        Self::from_core(e)
    }
}

/// Main dispatcher; returns the process exit code.
pub fn dispatch(cli: crate::Cli, mut out: impl Write) -> i32 {
    let json = cli.json;
    let socket = cli.socket.clone();
    let result: Result<i32, CmdError> = match cli.command {
        Commands::List => list::run(json, &mut out),
        Commands::Watch { interval_ms } => watch::run(interval_ms, json, &mut out),
        Commands::Monitor {
            port,
            baud,
            data_bits,
            parity,
            stop_bits,
            flow,
            record_to,
        } => crate::tui::run_monitor(port, baud, data_bits, parity, stop_bits, flow, record_to),
        Commands::Send {
            port,
            data,
            hex,
            file,
            baud,
            newline,
            expect,
            timeout_ms,
        } => send::run(
            &port,
            data,
            hex,
            file,
            baud,
            &newline,
            expect.as_deref(),
            timeout_ms,
            json,
            &mut out,
        ),
        Commands::Receive {
            port,
            baud,
            hex,
            output,
            duration,
        } => receive::run(
            &port,
            baud,
            hex,
            output.as_deref(),
            duration,
            json,
            &mut out,
        ),
        Commands::Attach { session } => attach::run(session, socket.as_deref()),
        Commands::Sessions => sessions::run(json, socket.as_deref(), &mut out),
        Commands::Rpc { method, params } => run_rpc(&method, &params, socket.as_deref(), &mut out),
        Commands::Autobaud {
            port,
            duration_ms,
            bauds,
        } => autobaud::run(&port, duration_ms, bauds.as_deref(), json, &mut out),
        Commands::Decode { log, decoder, list } => decode::run(
            log.as_deref().unwrap_or_else(|| std::path::Path::new("")),
            decoder.as_deref(),
            list,
            json,
            &mut out,
        ),
        Commands::Replay {
            log,
            port,
            baud,
            speed,
        } => replay::run(&log, &port, baud, speed, json, &mut out),
        Commands::Daemon { action } => daemon::run(action, socket.as_deref(), json, &mut out),
        Commands::Completion { shell } => {
            let mut buf = Vec::new();
            clap_complete::generate(shell, &mut crate::Cli::command(), "flattencom", &mut buf);
            out.write_all(&buf)
                .map(|()| exit_code::OK)
                .map_err(|e| CmdError::new(e.to_string()))
        }
    };
    let result = result.and_then(|code| {
        out.flush().map_err(|e| CmdError::new(e.to_string()))?;
        Ok(code)
    });
    match result {
        Ok(code) => code,
        Err(e) => {
            if json {
                let _ = writeln!(
                    out,
                    "{}",
                    serde_json::json!({"error": e.message, "exit_code": e.code})
                );
            } else {
                eprintln!("{}", flattencom_core::tr!("Error: {e}", e = e));
            }
            e.code
        }
    }
}

fn run_rpc(
    method: &str,
    params: &str,
    socket: Option<&std::path::Path>,
    out: &mut impl Write,
) -> Result<i32, CmdError> {
    let params: serde_json::Value = serde_json::from_str(params)
        .map_err(|e| CmdError::with_code(e.to_string(), exit_code::ARGS))?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| CmdError::new(e.to_string()))?;
    let result = rt.block_on(async {
        let client = connect_daemon("flattencom-cli", socket).await?;
        client
            .call(method, params)
            .await
            .map_err(|e| CmdError::new(e.to_string()))
    })?;
    writeln!(out, "{result}").map_err(|e| CmdError::new(e.to_string()))?;
    Ok(exit_code::OK)
}

/// Connect to the daemon with shared configuration and automatic startup.
pub async fn connect_daemon(
    label: &str,
    socket: Option<&std::path::Path>,
) -> Result<DaemonClient, CmdError> {
    let cfg = ConnectConfig::new(label, std::time::Duration::from_secs(3));
    let cfg = match socket {
        Some(p) => cfg.with_socket(p),
        None => cfg,
    };
    flattencom_proto::client::connect_or_spawn_with(cfg)
        .await
        .map_err(|e| {
            CmdError::new(flattencom_core::tr!(
                "Cannot connect to background service: {e}",
                e = e
            ))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FailedOutput {
        flush_only: bool,
    }
    impl Write for FailedOutput {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self.flush_only {
                Ok(bytes.len())
            } else {
                Err(std::io::ErrorKind::BrokenPipe.into())
            }
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Err(std::io::ErrorKind::BrokenPipe.into())
        }
    }

    #[test]
    fn display_commands_fail_on_write_and_final_flush_errors() {
        for args in [
            vec!["flattencom", "completion", "bash"],
            vec!["flattencom", "list"],
            vec!["flattencom", "list", "--json"],
            vec!["flattencom", "watch"],
        ] {
            for flush_only in [false, true] {
                let cli = crate::parse_cli(args.iter().map(|s| (*s).into()).collect()).unwrap();
                assert_eq!(
                    dispatch(cli, FailedOutput { flush_only }),
                    exit_code::FAIL,
                    "{args:?}"
                );
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn shared_commands_propagate_output_errors_with_isolated_rpc_peer() {
        use std::io::{BufRead, BufReader};
        use std::os::unix::net::UnixListener;
        use std::time::Duration;

        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("mock.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let peer = std::thread::spawn(move || {
            for _ in 0..16 {
                let (stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut input = BufReader::new(stream.try_clone().unwrap());
                let mut output = stream;
                for _ in 0..2 {
                    let mut line = String::new();
                    input.read_line(&mut line).unwrap();
                    let request: serde_json::Value = serde_json::from_str(&line).unwrap();
                    let result = match request["method"].as_str().unwrap() {
                        "hello" => {
                            serde_json::json!({"daemon":"mock", "version":"1", "proto":request["params"]["proto"], "capabilities":[]})
                        }
                        "list_sessions" => serde_json::json!({"sessions":[]}),
                        "daemon_info" => {
                            serde_json::json!({"daemon":"mock", "version":"1", "proto":1, "uptime_ms":0, "sessions":0, "socket":"mock", "capabilities":[]})
                        }
                        "shutdown" => serde_json::json!({"ok":true}),
                        method => panic!("unexpected method {method}"),
                    };
                    writeln!(
                        output,
                        "{}",
                        serde_json::json!({"jsonrpc":"2.0", "id":request["id"], "result":result})
                    )
                    .unwrap();
                }
            }
        });
        for command in [
            vec!["sessions"],
            vec!["daemon", "start"],
            vec!["daemon", "status"],
            vec!["daemon", "stop"],
        ] {
            for json in [false, true] {
                for flush_only in [false, true] {
                    let mut args = vec![
                        "flattencom".into(),
                        "--socket".into(),
                        socket.as_os_str().to_owned(),
                    ];
                    args.extend(command.iter().map(std::ffi::OsString::from));
                    if json {
                        args.push("--json".into());
                    }
                    let cli = crate::parse_cli(args).unwrap();
                    assert_eq!(dispatch(cli, FailedOutput { flush_only }), exit_code::FAIL);
                }
            }
        }
        peer.join().unwrap();
    }
}
