/*
 * flattencom - flattencom Flattencomd Src Workbench
 *
 * Manages annotations, immutable selections and cancellable transfer or reset operations.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

//! Persistent session annotations, explicit AI selections and cancellable operations.
use crate::state::DaemonState;
use flattencom_core::{ids::SessionId, session::SessionHandle};
use flattencom_proto::methods::RpcError;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::io::Read;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Bounded workbench state owned by the daemon; annotations also append to disk.
#[derive(Default)]
pub struct Workbench {
    notes: Mutex<HashMap<String, VecDeque<Value>>>,
    selections: Mutex<HashMap<String, Value>>,
    jobs: Mutex<HashMap<String, Arc<Job>>>,
}
struct Job {
    status: Mutex<Value>,
    cancel: AtomicBool,
}

/// Each array gets half the response budget, leaving ample room for the RPC envelope.
const MARKER_ARRAY_BYTES: usize = 4 * 1024 * 1024;
const MAX_MARKER_KIND_BYTES: usize = 64;

#[derive(Deserialize)]
struct MarkerMetadata {
    kind: Option<String>,
    seq: Option<u64>,
}

#[derive(Deserialize)]
struct SelectionMetadata {
    from_seq: Option<u64>,
    to_seq: Option<u64>,
}

/// Retain the newest entries within the actual escaped JSON byte budget.
fn bounded_entries(entries: Vec<Value>) -> (Vec<Value>, usize) {
    let total = entries.len();
    let mut retained = Vec::new();
    let mut bytes = 2; // Array brackets.
    for entry in entries.into_iter().rev() {
        let size = serde_json::to_vec(&entry).expect("JSON value").len() + 1;
        if bytes + size > MARKER_ARRAY_BYTES {
            break;
        }
        bytes += size;
        retained.push(entry);
    }
    retained.reverse();
    let omitted = total - retained.len();
    (retained, omitted)
}

fn marker_list(mut markers: Vec<Value>, boots: Vec<Value>) -> Value {
    markers.sort_by_key(|m| m["t_us"].as_i64().unwrap_or(0));
    let (markers, markers_omitted) = bounded_entries(markers);
    let (boots, boots_omitted) = bounded_entries(boots);
    json!({"markers":markers,"boots":boots,"truncated":markers_omitted > 0 || boots_omitted > 0,
        "markers_omitted":markers_omitted,"boots_omitted":boots_omitted})
}

impl Job {
    fn increment(&self, key: &str, count: u64) {
        let mut status = self.status.lock().expect("job");
        status[key] = json!(status[key].as_u64().unwrap_or(0) + count);
    }

    /// Count acknowledged chunks immediately, and count a frame only when complete.
    /// Core errors do not report the failed command's partial byte count, so the
    /// acknowledged byte count becomes a lower bound after a send error.
    fn send_frame(
        &self,
        data: &[u8],
        stopped: impl Fn() -> bool,
        mut send: impl FnMut(&[u8]) -> Result<(), String>,
    ) -> Result<bool, String> {
        if stopped() {
            return Ok(false);
        }
        for chunk in data.chunks(4096) {
            if stopped() {
                return Ok(false);
            }
            if let Err(error) = send(chunk) {
                self.status.lock().expect("job")["bytes_sent_exact"] = json!(false);
                return Err(error);
            }
            self.increment("bytes_sent", chunk.len() as u64);
        }
        self.increment("frames_sent", 1);
        Ok(true)
    }
}

fn error(message: impl Into<String>) -> RpcError {
    RpcError {
        code: -32602,
        message: {
            let message = message.into();
            flattencom_core::i18n::text(&message).to_owned()
        },
        data: Value::Null,
    }
}
fn string<'a>(p: &'a Value, key: &str) -> Result<&'a str, RpcError> {
    p.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| error(flattencom_core::tr!("missing {key}", key = key)))
}
fn session(state: &DaemonState, p: &Value) -> Result<SessionHandle, RpcError> {
    let id = SessionId::parse(string(p, "session_id")?).map_err(crate::dispatch::rpc_err)?;
    state
        .sessions
        .lock()
        .expect("sessions")
        .get(&id)
        .map(|s| s.handle.clone())
        .ok_or_else(|| error("session not found"))
}
fn timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_micros()).unwrap_or(u64::MAX))
}

impl Workbench {
    fn add_note(
        &self,
        session: &SessionHandle,
        label: &str,
        source: &str,
        kind: &str,
        anchor: Option<u64>,
    ) -> Result<Value, RpcError> {
        if label.len() > 4096 {
            return Err(error("marker label exceeds 4096 bytes"));
        }
        if kind.len() > MAX_MARKER_KIND_BYTES {
            return Err(error("marker kind exceeds 64 UTF-8 bytes"));
        }
        let stats = session.stats();
        let seq = anchor.or(stats.buffer.last_seq);
        let note = json!({"id":uuid::Uuid::now_v7().to_string(),"session_id":session.id().to_string(),"label":label,"kind":kind,"source":source,"t_us":timestamp(),"seq":seq});
        // Keep annotation order consistent with the in-memory marker list.
        let mut notes = self.notes.lock().expect("notes");
        session
            .record_annotation(note["t_us"].as_i64().unwrap_or(0), kind, label, source, seq)
            .map_err(crate::dispatch::rpc_err)?;
        let entries = notes.entry(session.id().to_string()).or_default();
        entries.push_back(note.clone());
        while entries.len() > 2000 {
            entries.pop_front();
        }
        Ok(note)
    }

    fn start_job(
        self: &Arc<Self>,
        state: &Arc<DaemonState>,
        conn: u64,
        p: Value,
        reset: bool,
    ) -> Result<Arc<Job>, RpcError> {
        let session = session(state, &p)?;
        if reset
            && [
                "path",
                "data",
                "hex",
                "replay",
                "speed",
                "chunk_size",
                "pacing_ms",
            ]
            .iter()
            .any(|key| p.get(*key).is_some_and(|v| !v.is_null()))
        {
            return Err(error(
                "reset accepts explicit steps, not transfer parameters",
            ));
        }
        if !reset {
            let _: flattencom_proto::workbench::TransferParams =
                serde_json::from_value(p.clone()).map_err(|e| error(e.to_string()))?;
            if p.get("steps").is_some_and(|v| !v.is_null()) {
                return Err(error("transfer does not accept reset steps"));
            }
        }
        let replay = p.get("replay").and_then(Value::as_bool).unwrap_or(false);
        if replay && p.get("path").and_then(Value::as_str).is_none() {
            return Err(error("Replay requires a file"));
        }
        let speed = p.get("speed").and_then(Value::as_f64).unwrap_or(1.0);
        if replay && (!speed.is_finite() || speed <= 0.0) {
            return Err(error("Replay speed must be finite and positive"));
        }
        let source = state.client_source(conn);
        let sid = session.id().to_string();
        let mut file = if let Some(path) = p.get("path").and_then(Value::as_str) {
            if !std::fs::metadata(path)
                .map_err(|e| error(e.to_string()))?
                .is_file()
            {
                return Err(error("transfer requires a regular file"));
            }
            Some(std::fs::File::open(path).map_err(|e| error(e.to_string()))?)
        } else {
            None
        };
        if !reset
            && ["path", "data", "hex"]
                .iter()
                .filter(|key| p.get(**key).is_some_and(|v| !v.is_null()))
                .count()
                != 1
        {
            return Err(error("supply exactly one of path, data or hex"));
        }
        if let Some(file) = &file
            && !file.metadata().map_err(|e| error(e.to_string()))?.is_file()
        {
            return Err(error("transfer requires a regular file"));
        }
        let data = if reset || replay {
            Vec::new()
        } else if let Some(text) = p.get("data").and_then(Value::as_str) {
            text.as_bytes().to_vec()
        } else if let Some(text) = p.get("hex").and_then(Value::as_str) {
            hex::decode(
                text.chars()
                    .filter(|c| !c.is_ascii_whitespace())
                    .collect::<String>(),
            )
            .map_err(|e| error(e.to_string()))?
        } else if file.is_some() {
            Vec::new()
        } else {
            return Err(error("supply path, data or hex"));
        };
        let steps = p
            .get("steps")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if reset && (steps.is_empty() || steps.len() > 32) {
            return Err(error(
                "reset requires 1..32 explicit steps; there is no universal board reset",
            ));
        }
        for step in &steps {
            let object = step
                .as_object()
                .ok_or_else(|| error("reset step must be an object"))?;
            if object.is_empty()
                || object
                    .keys()
                    .any(|key| !["dtr", "rts", "data", "hex", "delay_ms"].contains(&key.as_str()))
            {
                return Err(error("unknown or empty reset step"));
            }
            for key in ["dtr", "rts"] {
                if step.get(key).is_some_and(|v| !v.is_boolean()) {
                    return Err(error(flattencom_core::tr!(
                        "{key} must be boolean",
                        key = key
                    )));
                }
            }
            for key in ["data", "hex"] {
                if step
                    .get(key)
                    .is_some_and(|v| v.as_str().is_none_or(|s| s.len() > 4096))
                {
                    return Err(error(flattencom_core::tr!(
                        "{key} must be a string of at most 4096 bytes",
                        key = key
                    )));
                }
            }
            if step.get("delay_ms").is_some_and(|v| v.as_u64().is_none()) {
                return Err(error("delay_ms must be a nonnegative integer"));
            }
            if step.get("delay_ms").and_then(Value::as_u64).unwrap_or(0) > 5000 {
                return Err(error("reset step delay must be <=5000ms"));
            }
            if let Some(hex) = step.get("hex").and_then(Value::as_str) {
                hex::decode(hex.replace(' ', "")).map_err(|e| error(e.to_string()))?;
            }
        }
        let total = if reset {
            steps
                .iter()
                .map(|step| {
                    step.get("data").and_then(Value::as_str).map_or(0, str::len)
                        + step
                            .get("hex")
                            .and_then(Value::as_str)
                            .map_or(0, |text| text.replace(' ', "").len() / 2)
                })
                .sum::<usize>() as u64
        } else {
            file.as_ref()
                .map_or(Ok(data.len() as u64), |f| f.metadata().map(|m| m.len()))
                .map_err(|e| error(e.to_string()))?
        };
        let id = uuid::Uuid::now_v7().to_string();
        let job = Arc::new(Job {
            status: Mutex::new(
                json!({"id":id,"session_id":sid,"source":source,"kind":if reset{"reset"}else if replay{"replay"}else{"send"},"state":"running","bytes_sent":0,"bytes_sent_exact":true,"input_bytes":0,"frames_sent":0,"skipped":0,"total_bytes":total,"started_us":timestamp()}),
            ),
            cancel: AtomicBool::new(false),
        });
        {
            let mut jobs = self.jobs.lock().expect("jobs");
            if jobs.values().any(|j| {
                let s = j.status.lock().expect("job");
                s["session_id"] == sid && s["state"] == "running"
            }) {
                return Err(error("this session already has a running operation"));
            }
            if jobs.len() >= 128 {
                jobs.retain(|_, j| j.status.lock().expect("job")["state"] == "running");
            }
            if jobs.len() >= 128 {
                return Err(error("operation limit reached"));
            }
            let marker = if reset {
                Some(
                    self.add_note(
                        &session,
                        p.get("label")
                            .and_then(Value::as_str)
                            .unwrap_or("Reset requested"),
                        &source,
                        "reset",
                        None,
                    )?,
                )
            } else {
                None
            };
            jobs.insert(id.clone(), job.clone());
            job.status.lock().expect("job")["marker"] = marker.unwrap_or(Value::Null);
        }
        // Keep a handle independent of the bounded public operation registry.
        let result = job.clone();
        let state = state.clone();
        std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(300);
            let stopped = || {
                job.cancel.load(Ordering::Relaxed)
                    || state.is_shutting_down()
                    || Instant::now() >= deadline
            };
            let delay = |ms: u64| {
                let end = Instant::now() + Duration::from_millis(ms);
                while Instant::now() < end && !stopped() {
                    std::thread::sleep(Duration::from_millis(10));
                }
            };
            let work = (|| -> Result<(), String> {
                if replay {
                    use std::io::BufRead;
                    let mut reader =
                        std::io::BufReader::new(file.take().ok_or("Replay requires a file")?);
                    let mut previous = None;
                    loop {
                        if stopped() {
                            break;
                        }
                        let mut line = Vec::new();
                        let n = reader
                            .by_ref()
                            .take(16 * 1024 * 1024 + 1)
                            .read_until(b'\n', &mut line)
                            .map_err(|e| e.to_string())?;
                        if n == 0 {
                            break;
                        }
                        job.increment("input_bytes", n as u64);
                        if n > 16 * 1024 * 1024 {
                            return Err("Replay record exceeds 16 MiB".into());
                        }
                        // Validate ignored JSON fields too: serde's byte parser can
                        // skip their strings without checking UTF-8.
                        let Ok(text) = std::str::from_utf8(&line) else {
                            job.increment("skipped", 1);
                            continue;
                        };
                        let Ok(frame) = serde_json::from_str::<flattencom_core::frame::Frame>(text)
                        else {
                            job.increment("skipped", 1);
                            continue;
                        };
                        if let Some(before) = previous {
                            delay(
                                flattencom_core::record::replay_delay(before, frame.mono_us, speed)
                                    .as_millis() as u64,
                            );
                        }
                        previous = Some(frame.mono_us);
                        if stopped() {
                            break;
                        }
                        if !job.send_frame(&frame.data, stopped, |chunk| {
                            session
                                .send_from(chunk.to_vec(), source.clone())
                                .map(|_| ())
                                .map_err(|e| e.to_string())
                        })? {
                            break;
                        }
                    }
                } else if reset {
                    for step in steps {
                        if stopped() {
                            break;
                        }
                        if step.get("dtr").is_some() || step.get("rts").is_some() {
                            session
                                .set_signals(
                                    step.get("dtr").and_then(Value::as_bool),
                                    step.get("rts").and_then(Value::as_bool),
                                )
                                .map_err(|e| e.to_string())?;
                        }
                        if let Some(text) = step.get("data").and_then(Value::as_str) {
                            job.increment("input_bytes", text.len() as u64);
                            if !job.send_frame(text.as_bytes(), stopped, |chunk| {
                                session
                                    .send_from(chunk.to_vec(), source.clone())
                                    .map(|_| ())
                                    .map_err(|e| e.to_string())
                            })? {
                                break;
                            }
                        }
                        if let Some(text) = step.get("hex").and_then(Value::as_str) {
                            let data =
                                hex::decode(text.replace(' ', "")).map_err(|e| e.to_string())?;
                            job.increment("input_bytes", data.len() as u64);
                            if !job.send_frame(&data, stopped, |chunk| {
                                session
                                    .send_from(chunk.to_vec(), source.clone())
                                    .map(|_| ())
                                    .map_err(|e| e.to_string())
                            })? {
                                break;
                            }
                        }
                        delay(step.get("delay_ms").and_then(Value::as_u64).unwrap_or(0));
                    }
                } else {
                    let mut buffer = vec![
                        0;
                        p.get("chunk_size")
                            .and_then(Value::as_u64)
                            .unwrap_or(1024)
                            .clamp(1, 4096) as usize
                    ];
                    let mut offset = 0;
                    loop {
                        if stopped() {
                            break;
                        }
                        let size = if let Some(file) = &mut file {
                            file.read(&mut buffer).map_err(|e| e.to_string())?
                        } else {
                            let n = buffer.len().min(data.len() - offset);
                            buffer[..n].copy_from_slice(&data[offset..offset + n]);
                            offset += n;
                            n
                        };
                        if size == 0 {
                            break;
                        }
                        job.increment("input_bytes", size as u64);
                        if !job.send_frame(&buffer[..size], stopped, |chunk| {
                            session
                                .send_from(chunk.to_vec(), source.clone())
                                .map(|_| ())
                                .map_err(|e| e.to_string())
                        })? {
                            break;
                        }
                        delay(
                            p.get("pacing_ms")
                                .and_then(Value::as_u64)
                                .unwrap_or(0)
                                .min(5000),
                        );
                    }
                }
                Ok(())
            })();
            let mut status = job.status.lock().expect("job");
            status["finished_us"] = json!(timestamp());
            status["state"] = json!(if work.is_err() {
                "failed"
            } else if stopped() {
                "cancelled"
            } else {
                "completed"
            });
            if let Err(e) = work {
                status["error"] = json!(e);
            }
        });
        Ok(result)
    }
    pub(crate) fn close_session(&self, id: &str) {
        self.notes.lock().expect("notes").remove(id);
        for job in self.jobs.lock().expect("jobs").values() {
            if job.status.lock().expect("job")["session_id"] == id {
                job.cancel.store(true, Ordering::Relaxed);
            }
        }
    }
}

/// Compatibility adapter: synchronous callers share the same bounded job engine.
pub(crate) fn run_transfer(
    state: &Arc<DaemonState>,
    conn: u64,
    mut params: Value,
    replay: bool,
) -> Result<Value, RpcError> {
    if !params.is_object() {
        return Err(error("Transfer parameters must be an object"));
    }
    params["replay"] = json!(replay);
    let job = state.workbench.start_job(state, conn, params, false)?;
    wait_transfer(&job, replay)
}

fn wait_transfer(job: &Job, replay: bool) -> Result<Value, RpcError> {
    loop {
        let status = job.status.lock().expect("job").clone();
        match status["state"].as_str() {
            Some("running") => std::thread::sleep(Duration::from_millis(10)),
            Some("completed") => {
                return Ok(if replay {
                    json!({"frames_sent":status["frames_sent"],"bytes_sent":status["bytes_sent"],"skipped":status["skipped"]})
                } else {
                    json!({"frames":status["frames_sent"],"bytes_sent":status["bytes_sent"]})
                });
            }
            _ => {
                return Err(error(
                    status["error"]
                        .as_str()
                        .unwrap_or("Operation cancelled or deadline exceeded"),
                ));
            }
        }
    }
}

/// Dispatch workbench methods; bounded snapshots never read unselected raw history for AI.
pub fn dispatch(
    state: &Arc<DaemonState>,
    conn: u64,
    method: &str,
    p: &Value,
) -> Result<Value, RpcError> {
    let work = &state.workbench;
    match method {
        "add_marker" => {
            let s = session(state, p)?;
            let metadata: MarkerMetadata =
                serde_json::from_value(p.clone()).map_err(|e| error(e.to_string()))?;
            work.add_note(
                &s,
                string(p, "label")?,
                &state.client_source(conn),
                metadata.kind.as_deref().unwrap_or("manual"),
                metadata.seq,
            )
            .map(|m| json!({"marker":m}))
        }
        "list_markers" => {
            let s = session(state, p)?;
            let mut markers = work
                .notes
                .lock()
                .expect("notes")
                .get(&s.id().to_string())
                .cloned()
                .unwrap_or_default();
            let boots = s.boot_events();
            for boot in &boots {
                markers.push_back(json!({"id":format!("boot-{}",boot.number),"kind":"boot","seq":boot.seq,"t_us":boot.t_us,"label":boot.banner,"source":"capture-detector","number":boot.number,"interval_ms":boot.interval_ms}));
            }
            Ok(marker_list(
                markers.into_iter().collect(),
                boots
                    .into_iter()
                    .map(|boot| serde_json::to_value(boot).expect("boot event"))
                    .collect(),
            ))
        }
        "publish_selection" => {
            let s = session(state, p)?;
            let metadata: SelectionMetadata =
                serde_json::from_value(p.clone()).map_err(|e| error(e.to_string()))?;
            let text = string(p, "text")?;
            if text.len() > 256 * 1024 {
                return Err(error("selection exceeds 256 KiB; select a smaller range"));
            }
            let selection = json!({"id":uuid::Uuid::now_v7().to_string(),"session_id":s.id().to_string(),"text":text,"from_seq":metadata.from_seq,"to_seq":metadata.to_seq,"published_us":timestamp(),"source":state.client_source(conn)});
            let id = selection["id"].as_str().expect("id").to_owned();
            let mut selections = work.selections.lock().expect("selections");
            if selections.len() >= 32 {
                return Err(error(
                    "32 selections already published; revoke_selection before publishing more",
                ));
            }
            selections.insert(id, selection.clone());
            Ok(json!({"selection":selection}))
        }
        "read_selection" => work
            .selections
            .lock()
            .expect("selections")
            .get(string(p, "selection_id")?)
            .cloned()
            .map(|s| json!({"selection":s}))
            .ok_or_else(|| error("selection expired or revoked")),
        "revoke_selection" => Ok(
            json!({"removed":work.selections.lock().expect("selections").remove(string(p,"selection_id")?).is_some()}),
        ),
        "start_transfer" | "reset_device" => {
            let job = work.start_job(state, conn, p.clone(), method == "reset_device")?;
            let status = job.status.lock().expect("job").clone();
            Ok(json!({"operation":status}))
        }
        "operation_status" | "cancel_operation" => {
            let jobs = work.jobs.lock().expect("jobs");
            let job = jobs
                .get(string(p, "operation_id")?)
                .ok_or_else(|| error("operation not found"))?;
            if method == "cancel_operation" {
                job.cancel.store(true, Ordering::Relaxed);
            }
            let result = job.status.lock().expect("job").clone();
            Ok(json!({"operation":result}))
        }
        _ => Err(error("unknown workbench method")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compatibility_transfer_rejects_nonobject_parameters() {
        let state = Arc::new(DaemonState::new("unused".into(), "token".into()));
        for method in ["send_file", "replay_log"] {
            for params in [Value::Null, json!([]), json!("bad"), json!(true), json!(42)] {
                let error = crate::dispatch::dispatch(&state, 1, method, params).unwrap_err();
                assert_eq!(error.code, -32602);
            }
        }
        assert!(state.workbench.jobs.lock().unwrap().is_empty());
    }

    fn test_job() -> Job {
        Job {
            status: Mutex::new(json!({"bytes_sent":0,"bytes_sent_exact":true,"frames_sent":0})),
            cancel: AtomicBool::new(false),
        }
    }

    #[test]
    fn compatibility_waiter_survives_completed_job_registry_pruning() {
        let dir = tempfile::tempdir().unwrap();
        let state = Arc::new(DaemonState::new(dir.path().join("socket"), "token".into()));
        let sid = state
            .open_session("test".into(), 1, SerialConfig::new("virtual://echo"))
            .unwrap()
            .session
            .session_id;
        for i in 0..127 {
            let job = test_job();
            job.status.lock().unwrap()["state"] = json!("completed");
            state
                .workbench
                .jobs
                .lock()
                .unwrap()
                .insert(format!("old-{i}"), Arc::new(job));
        }
        let job = state
            .workbench
            .start_job(&state, 1, json!({"session_id":sid,"data":"first"}), false)
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while job.status.lock().unwrap()["state"] == "running" {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        let id = job.status.lock().unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned();
        assert_eq!(state.workbench.jobs.lock().unwrap().len(), 128);
        let next = state
            .workbench
            .start_job(&state, 1, json!({"session_id":sid,"data":"next"}), false)
            .unwrap();
        assert!(!state.workbench.jobs.lock().unwrap().contains_key(&id));
        // Observe the first completion only after another job has pruned it.
        assert_eq!(
            wait_transfer(&job, false).unwrap(),
            json!({"bytes_sent":5,"frames":1})
        );
        assert_eq!(wait_transfer(&next, false).unwrap()["bytes_sent"], 4);
        let _ = state.close_session(&SessionId::parse(&sid).unwrap());
    }

    #[test]
    fn marker_and_selection_metadata_are_typed_and_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let state = Arc::new(DaemonState::new(dir.path().join("socket"), "token".into()));
        let sid = state
            .open_session("test".into(), 1, SerialConfig::new("virtual://echo"))
            .unwrap()
            .session
            .session_id;
        for invalid in [
            json!("x".repeat(1024 * 1024)),
            json!(-1),
            json!(1.5),
            json!(true),
            json!({}),
        ] {
            assert!(
                dispatch(
                    &state,
                    1,
                    "add_marker",
                    &json!({"session_id":sid,"label":"test","seq":invalid})
                )
                .is_err()
            );
            for key in ["from_seq", "to_seq"] {
                let mut params = json!({"session_id":sid,"text":"test"});
                params[key] = invalid.clone();
                assert!(dispatch(&state, 1, "publish_selection", &params).is_err());
            }
        }
        for kind in [
            json!("x".repeat(1024 * 1024)),
            json!("é".repeat(33)),
            json!(10),
        ] {
            assert!(
                dispatch(
                    &state,
                    1,
                    "add_marker",
                    &json!({"session_id":sid,"label":"test","kind":kind})
                )
                .is_err()
            );
        }
        for metadata in [
            json!({}),
            json!({"kind":null,"seq":null}),
            json!({"kind":"é".repeat(32),"seq":u64::MAX}),
        ] {
            let mut params = metadata;
            params["session_id"] = json!(sid);
            params["label"] = json!("test");
            assert!(dispatch(&state, 1, "add_marker", &params).is_ok());
        }
        let selection = dispatch(
            &state,
            1,
            "publish_selection",
            &json!({"session_id":sid,"text":"test","from_seq":null,"to_seq":u64::MAX}),
        )
        .unwrap();
        assert!(selection["selection"]["from_seq"].is_null());
        assert_eq!(selection["selection"]["to_seq"], u64::MAX);
        let _ = state.close_session(&SessionId::parse(&sid).unwrap());
    }

    #[test]
    fn marker_lists_budget_escaped_json_and_report_omitted_old_entries() {
        let label = "\u{0001}".repeat(4096);
        let markers = (0..3000).map(|i| json!({"id":i,"t_us":i,"label":label,"kind":"\u{0002}".repeat(64),"source":"\u{0003}".repeat(256)})).collect();
        let boots = (0..1000)
            .map(|i| json!({"number":i,"t_us":i,"banner":label}))
            .collect();
        let result = marker_list(markers, boots);
        assert_eq!(result["truncated"], true);
        for (key, omitted_key, total) in [
            ("markers", "markers_omitted", 3000),
            ("boots", "boots_omitted", 1000),
        ] {
            let entries = result[key].as_array().unwrap();
            assert!(!entries.is_empty(), "{:?}", entries.is_empty());
            let omitted = result[omitted_key].as_u64().unwrap() as usize;
            assert!(omitted > 0);
            assert_eq!(entries.len() + omitted, total);
            assert_eq!(entries.first().unwrap()["t_us"], omitted);
            assert_eq!(entries.last().unwrap()["t_us"], total - 1);
            assert!(serde_json::to_vec(entries).unwrap().len() <= MARKER_ARRAY_BYTES);
        }
        let response = json!({"jsonrpc":"2.0","id":u64::MAX,"result":result});
        assert!(serde_json::to_vec(&response).unwrap().len() < 8 * 1024 * 1024 + 1024);
        let small = marker_list(vec![json!({"t_us":1})], vec![]);
        assert_eq!(small["truncated"], false);
        assert_eq!(small["markers_omitted"], 0);
        assert_eq!(small["boots_omitted"], 0);
    }

    #[test]
    fn cancelled_and_failed_partial_frames_preserve_acknowledged_progress() {
        let job = test_job();
        let mut transmitted = 0;
        let complete = job
            .send_frame(
                &vec![0; 65536],
                || job.cancel.load(Ordering::Relaxed),
                |chunk| {
                    transmitted += chunk.len();
                    job.cancel.store(true, Ordering::Relaxed);
                    Ok(())
                },
            )
            .unwrap();
        assert!(!complete);
        assert_eq!(transmitted, 4096);
        assert_eq!(job.status.lock().unwrap()["bytes_sent"], transmitted);
        assert_eq!(job.status.lock().unwrap()["frames_sent"], 0);
        assert_eq!(job.status.lock().unwrap()["bytes_sent_exact"], true);

        let job = test_job();
        let mut calls = 0;
        let error = job
            .send_frame(
                &vec![0; 16384],
                || false,
                |_| {
                    calls += 1;
                    if calls == 2 {
                        Err("partial write failure".into())
                    } else {
                        Ok(())
                    }
                },
            )
            .unwrap_err();
        assert_eq!(error, "partial write failure");
        assert_eq!(job.status.lock().unwrap()["bytes_sent"], 4096);
        assert_eq!(job.status.lock().unwrap()["frames_sent"], 0);
        assert_eq!(job.status.lock().unwrap()["bytes_sent_exact"], false);
    }

    #[test]
    fn replay_counts_skipped_input_and_rejects_mixed_jobs_before_starting() {
        let dir = tempfile::tempdir().unwrap();
        let state = Arc::new(DaemonState::new(dir.path().join("socket"), "token".into()));
        let sid = state
            .open_session("test".into(), 1, SerialConfig::new("virtual://echo"))
            .unwrap()
            .session
            .session_id;
        for (method, extra) in [
            ("start_transfer", json!({"data":"x","replay":true})),
            ("start_transfer", json!({"data":"x","steps":[{"dtr":true}]})),
            (
                "reset_device",
                json!({"steps":[{"data":"x"}],"replay":true}),
            ),
            (
                "reset_device",
                json!({"steps":[{"data":"x"}],"path":"missing"}),
            ),
        ] {
            let mut params = extra;
            params["session_id"] = json!(sid);
            assert!(dispatch(&state, 1, method, &params).is_err());
            assert!(state.workbench.jobs.lock().unwrap().is_empty());
        }
        let path = dir.path().join("replay.jsonl");
        let text =
            "invalid\n\n{\"seq\":0,\"dir\":\"tx\",\"t_us\":0,\"mono_us\":0,\"data\":\"4142\"}\n";
        let mut input = b"{\"seq\":0,\"dir\":\"tx\",\"t_us\":0,\"mono_us\":0,\"data\":\"BAD0\",\"ignored\":\"\xff\"}\n".to_vec();
        input.extend_from_slice(text.as_bytes());
        std::fs::write(&path, &input).unwrap();
        let started = dispatch(
            &state,
            1,
            "start_transfer",
            &json!({"session_id":sid,"path":path,"replay":true}),
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let status = dispatch(
                &state,
                1,
                "operation_status",
                &json!({"operation_id":started["operation"]["id"]}),
            )
            .unwrap();
            let operation = &status["operation"];
            if operation["state"] != "running" {
                assert_eq!(operation["state"], "completed");
                assert_eq!(operation["input_bytes"], input.len());
                assert_eq!(operation["total_bytes"], input.len());
                assert_eq!(operation["skipped"], 3);
                assert_eq!(operation["bytes_sent"], 2);
                assert_eq!(operation["frames_sent"], 1);
                let sent = session(&state, &json!({"session_id":sid}))
                    .unwrap()
                    .read_sent(None, 65536);
                assert_eq!(sent.frames.len(), 1);
                assert_eq!(sent.frames[0].data.as_slice(), b"AB");
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        let _ = state.close_session(&SessionId::parse(&sid).unwrap());
    }
    #[test]
    fn replay_job_is_cancellable_during_timing_gap() {
        let dir = tempfile::tempdir().unwrap();
        let state = Arc::new(DaemonState::new(dir.path().join("socket"), "token".into()));
        let opened = state
            .open_session("test".into(), 1, SerialConfig::new("virtual://echo"))
            .unwrap();
        let sid = opened.session.session_id;
        let path = dir.path().join("replay.jsonl");
        let mut text = String::new();
        for i in 0..2 {
            text.push_str(
                &json!({"seq":i,"dir":"tx","t_us":i,"mono_us":i*60_000_000_u64,"data":"41"})
                    .to_string(),
            );
            text.push('\n');
        }
        std::fs::write(&path, text).unwrap();
        let job = dispatch(
            &state,
            1,
            "start_transfer",
            &json!({"session_id":sid,"path":path,"replay":true}),
        )
        .unwrap();
        let id = job["operation"]["id"].clone();
        let params = json!({"operation_id":id});
        std::thread::sleep(Duration::from_millis(50));
        dispatch(&state, 1, "cancel_operation", &params).unwrap();
        let start = Instant::now();
        loop {
            let status = dispatch(&state, 1, "operation_status", &params).unwrap();
            if status["operation"]["state"] != "running" {
                assert_eq!(status["operation"]["state"], "cancelled");
                break;
            }
            assert!(start.elapsed() < Duration::from_secs(2));
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = state.close_session(&SessionId::parse(&sid).unwrap());
    }
    #[test]
    fn missing_parameter_is_localized_before_formatting() {
        use flattencom_core::i18n::{Language, with_language};
        let key = "custom_设备";
        let zh = with_language(Language::Chinese, || string(&json!({}), key).unwrap_err());
        assert_eq!(zh.message, "缺少参数 custom_设备");
        let en = with_language(Language::English, || string(&json!({}), key).unwrap_err());
        assert_eq!(en.message, "missing custom_设备");
    }
    use flattencom_core::config::SerialConfig;
    #[test]
    fn snapshot_revocation_transfer_cancel_and_reset_validation() {
        let dir = tempfile::tempdir().unwrap();
        let state = Arc::new(DaemonState::new(dir.path().join("socket"), "token".into()));
        let mut config = SerialConfig::new("virtual://echo");
        config.record_to = Some(dir.path().join("capture.txt"));
        let sid = state
            .open_session("test-gui".into(), 1, config)
            .unwrap()
            .session
            .session_id;
        let selection = dispatch(
            &state,
            1,
            "publish_selection",
            &json!({"session_id":sid,"text":"only selected substring","from_seq":5,"to_seq":6}),
        )
        .unwrap();
        let selection_id = selection["selection"]["id"].clone();
        let read = dispatch(
            &state,
            2,
            "read_selection",
            &json!({"selection_id":selection_id}),
        )
        .unwrap();
        assert_eq!(read["selection"]["text"], "only selected substring");
        dispatch(
            &state,
            1,
            "revoke_selection",
            &json!({"selection_id":selection_id}),
        )
        .unwrap();
        assert!(
            dispatch(
                &state,
                2,
                "read_selection",
                &json!({"selection_id":selection_id})
            )
            .is_err()
        );
        assert!(
            dispatch(
                &state,
                1,
                "reset_device",
                &json!({"session_id":sid,"steps":[{"dtr":"false"}]})
            )
            .is_err()
        );
        assert!(
            dispatch(
                &state,
                1,
                "reset_device",
                &json!({"session_id":sid,"steps":[{"typo":true}]})
            )
            .is_err()
        );
        let start = dispatch(
            &state,
            1,
            "start_transfer",
            &json!({"session_id":sid,"data":"x".repeat(10000),"chunk_size":100,"pacing_ms":100}),
        )
        .unwrap();
        let operation_id = start["operation"]["id"].clone();
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let status = dispatch(
                &state,
                1,
                "operation_status",
                &json!({"operation_id":operation_id}),
            )
            .unwrap();
            if status["operation"]["bytes_sent"].as_u64().unwrap() > 0 {
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        dispatch(
            &state,
            1,
            "cancel_operation",
            &json!({"operation_id":operation_id}),
        )
        .unwrap();
        loop {
            let status = dispatch(
                &state,
                1,
                "operation_status",
                &json!({"operation_id":operation_id}),
            )
            .unwrap();
            if status["operation"]["state"] != "running" {
                assert_eq!(status["operation"]["state"], "cancelled");
                assert!(status["operation"]["bytes_sent"].as_u64().unwrap() < 10000);
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        dispatch(
            &state,
            1,
            "add_marker",
            &json!({"session_id":sid,"label":"button pressed"}),
        )
        .unwrap();
        assert!(
            std::fs::read_to_string(dir.path().join("capture.txt"))
                .unwrap()
                .contains("button pressed")
        );
        let handle = session(&state, &json!({"session_id":sid})).unwrap();
        let frames = handle.read_frames(None, 65536).frames;
        assert!(
            frames
                .iter()
                .filter(|f| f.dir == flattencom_core::frame::Direction::Tx)
                .all(|f| f.source.as_deref() == Some("disconnected#1"))
        );
        let reset=dispatch(&state,1,"reset_device",&json!({"session_id":sid,"steps":[{"data":"\nU-Boot SPL 2026\nU-Boot 2026\nLinux version 6.12\nLinux version 6.12\n"}]})).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let events = handle.boot_events();
            if events.len() == 2 {
                assert_eq!(events[1].number, 2);
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        let status = dispatch(
            &state,
            1,
            "operation_status",
            &json!({"operation_id":reset["operation"]["id"]}),
        )
        .unwrap();
        assert_eq!(status["operation"]["state"], "completed");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("capture.txt"))
                .unwrap()
                .matches(" BOOT anchor=")
                .count(),
            2
        );
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            1,
            "one capture, no sidecars"
        );
        let path = dir.path().join("payload.bin");
        std::fs::write(&path, [0, 255, 13, 10]).unwrap();
        let transfer = dispatch(
            &state,
            1,
            "start_transfer",
            &json!({"session_id":sid,"path":path,"chunk_size":2}),
        )
        .unwrap();
        loop {
            let status = dispatch(
                &state,
                1,
                "operation_status",
                &json!({"operation_id":transfer["operation"]["id"]}),
            )
            .unwrap();
            if status["operation"]["state"] != "running" {
                assert_eq!(status["operation"]["state"], "completed");
                assert_eq!(status["operation"]["bytes_sent"], 4);
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        handle.close();
    }
}
