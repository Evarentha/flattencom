/*
 * flattencom - flattencom Flattencomd Src Dispatch
 *
 * Routes RPC methods to shared session operations and serializes results and errors.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

//! Method dispatch: request, core operation, result.
//!
//! Blocking core operations run in spawn_blocking, including mutex acquisition and command replies;
//! send_file and replay_log may also wait for paced background jobs.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use flattencom_core::config::{ConfigPatch, SerialConfig};
use flattencom_core::discovery::list_ports as core_list_ports;
use flattencom_core::ids::SessionId;
use flattencom_core::record::ExportFormat;
use flattencom_core::session::{FramesPage, SessionHandle};
use flattencom_core::timefmt;
use flattencom_proto::methods::RpcNotification;
use flattencom_proto::socket::state_dir;

use flattencom_proto::methods::{
    ClearedResult, CloseSessionParams, CloseSessionResult, ConfigureSessionParams,
    ConfigureSessionResult, DaemonInfoResult, DecoderCatalogResult, ExportFormatParam,
    ExportLogParams, ExportLogResult, FrameFormat, FrameOut, FramesPageOut, GetDecoderResult,
    GetLogParams, GetPortInfoParams, GetPortInfoResult, GetStatsResult, GetTriggerFiresResult,
    GetTriggersResult, HelloParams, ListPortsResult, ListSessionsResult, OkResult,
    OpenSessionParams, PROTOCOL_VERSION, PortEntry, ReadFramesParams, RpcError, SendBreakParams,
    SendParams, SendResult, SessionIdParams, SetDecoderParams, SetFilterParams, SetFilterResult,
    SetSignalsParams, SetTriggersParams, SetTriggersResult, ShutdownResult, SignalsResult, SubKind,
    SubscribeParams, SubscribeResult, WelcomeResult, error_code, frame_out,
};

use crate::state::DaemonState;

// ---------------------------------------------------------------------------
// Error mapping and helpers
// ---------------------------------------------------------------------------

/// Convert core errors to RPC errors, preserving codes and structured categories.
pub fn rpc_err(e: flattencom_core::FlattenError) -> RpcError {
    RpcError {
        code: e.code(),
        message: e.to_string(),
        data: e.to_json(),
    }
}

fn bad_params(method: &str, e: serde_json::Error) -> RpcError {
    RpcError {
        code: error_code::INVALID_PARAMS,
        message: flattencom_core::tr!(
            "Invalid parameters for {method}: {e}",
            e = e,
            method = method
        ),
        data: serde_json::Value::Null,
    }
}

fn method_not_found(method: &str) -> RpcError {
    RpcError {
        code: error_code::METHOD_NOT_FOUND,
        message: flattencom_core::tr!(
            "Unknown method {method:?}; see docs/PROTOCOL.md",
            method = method
        ),
        data: serde_json::Value::Null,
    }
}

/// Deserialize request parameters.
fn params_of<P>(method: &str, v: &serde_json::Value) -> Result<P, RpcError>
where
    P: serde::de::DeserializeOwned,
{
    serde_json::from_value(v.clone()).map_err(|e| bad_params(method, e))
}

/// Serialize a result.
fn json<T: serde::Serialize>(v: T) -> Result<serde_json::Value, RpcError> {
    serde_json::to_value(v).map_err(|e| RpcError {
        code: error_code::INTERNAL,
        message: flattencom_core::tr!("Failed to serialize result: {e}", e = e),
        data: serde_json::Value::Null,
    })
}

/// Parse a session identifier.
fn session_id(v: &serde_json::Value) -> Result<SessionId, RpcError> {
    #[derive(serde::Deserialize)]
    struct P {
        session_id: String,
    }
    let p: P = serde_json::from_value(v.clone()).map_err(|e| bad_params("session_id", e))?;
    SessionId::parse(&p.session_id).map_err(rpc_err)
}

/// Retrieve a session handle or report that it does not exist.
fn session(state: &DaemonState, id: &SessionId) -> Result<SessionHandle, RpcError> {
    let sessions = state.sessions.lock().expect("Session table lock poisoned");
    sessions.get(id).map(|e| e.handle.clone()).ok_or_else(|| {
        rpc_err(flattencom_core::FlattenError::SessionNotFound(
            id.to_string(),
        ))
    })
}

/// Deserialize protocol strings into core enums using shared serde names.
fn parse_enum<T>(s: Option<&String>) -> Result<Option<T>, RpcError>
where
    T: serde::de::DeserializeOwned,
{
    match s {
        None => Ok(None),
        Some(s) => serde_json::from_value(serde_json::Value::String(s.clone()))
            .map(Some)
            .map_err(|e| RpcError {
                code: error_code::INVALID_PARAMS,
                message: flattencom_core::tr!("Invalid enum value {s:?}: {e}", e = e, s = s),
                data: serde_json::Value::Null,
            }),
    }
}

/// Project a frame page for output.
fn page_out(page: &FramesPage, format: FrameFormat) -> FramesPageOut {
    FramesPageOut {
        frames: page
            .frames
            .iter()
            .map(|f| frame_out(f, format))
            .collect::<Vec<FrameOut>>(),
        next_seq: page.next_seq,
        dropped_rx: page.dropped_rx,
        up_to_date: page.up_to_date,
        first_seq: page.first_seq,
        last_seq: page.last_seq,
    }
}

/// Current wall-clock microseconds.
fn now_us() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_micros() as i64)
}

// ---------------------------------------------------------------------------
// Main dispatch
// ---------------------------------------------------------------------------

/// Blocking spawn_blocking entry point; conn_id identifies ownership and client scope.
pub fn dispatch(
    state: &Arc<DaemonState>,
    conn_id: u64,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, RpcError> {
    match method {
        "save_report" => {
            let path = params["path"].as_str().ok_or_else(|| {
                rpc_err(flattencom_core::FlattenError::io("Report path is required"))
            })?;
            let text = params["text"].as_str().ok_or_else(|| {
                rpc_err(flattencom_core::FlattenError::io("Report text is required"))
            })?;
            flattencom_core::capture_sink::atomic_output(std::path::Path::new(path), |file| {
                use std::io::Write;
                file.write_all(text.as_bytes())
                    .map_err(|e| flattencom_core::FlattenError::io(e.to_string()))
            })
            .map_err(rpc_err)?;
            json(OkResult { ok: true })
        }
        "add_marker" | "list_markers" | "publish_selection" | "read_selection"
        | "revoke_selection" | "start_transfer" | "reset_device" | "operation_status"
        | "cancel_operation" => crate::workbench::dispatch(state, conn_id, method, &params),
        // Connection
        "hello" => hello(state, &params),
        // Ports
        "list_ports" => list_ports(state),
        "get_port_info" => get_port_info(state, &params),
        // Sessions
        "open_session" => open_session(state, conn_id, &params),
        "close_session" => close_session(state, conn_id, &params),
        "list_sessions" => list_sessions(state),
        "configure_session" => attributed(
            state,
            conn_id,
            "configure_session",
            &params,
            configure_session(state, &params),
        ),
        // Transmission and reception
        "send" => send(state, conn_id, &params),
        "send_file" => crate::workbench::run_transfer(state, conn_id, params, false),
        "read_frames" => read_frames(state, conn_id, &params),
        "read_sent" => {
            let p: ReadFramesParams = params_of("read_sent", &params)?;
            let id = SessionId::parse(&p.session_id).map_err(rpc_err)?;
            let page = session(state, &id)?.read_sent(
                p.since_seq,
                p.max_bytes.unwrap_or(65536).clamp(1, 1024 * 1024),
            );
            json(page_out(&page, p.format))
        }
        "get_log" => get_log(state, conn_id, &params),
        "clear_buffer" => clear_buffer(state, &params),
        "flush_rx" => flush_rx(state, &params),
        "flush" => flush(state, &params),
        // Decoders, filters and triggers
        "set_decoder" => set_decoder(state, &params),
        "get_decoder" => get_decoder(state, &params),
        "list_decoders" => list_decoders(),
        "set_filter" => set_filter(state, conn_id, &params),
        "set_triggers" => set_triggers(state, &params),
        "get_triggers" => get_triggers(state, &params),
        "get_trigger_fires" => get_trigger_fires(state, &params),
        // Control lines
        "set_signals" => attributed(
            state,
            conn_id,
            "set_signals",
            &params,
            set_signals(state, &params),
        ),
        "get_signals" => get_signals(state, &params),
        "send_break" => attributed(
            state,
            conn_id,
            "send_break",
            &params,
            send_break(state, &params),
        ),
        // Statistics and recording
        "get_stats" => get_stats(state, &params),
        "export_log" => export_log(state, conn_id, &params),
        "export_readable" => {
            let p: ExportLogParams = params_of("export_readable", &params)?;
            let id = SessionId::parse(&p.session_id).map_err(rpc_err)?;
            let path = p.path.ok_or_else(|| {
                rpc_err(flattencom_core::FlattenError::InvalidConfig {
                    field: "path".into(),
                    reason: "Export path is required".into(),
                })
            })?;
            let text = session(state, &id)?
                .export_readable(std::path::Path::new(&path))
                .map_err(rpc_err)?;
            json(serde_json::json!({"path":text.path,"frames":text.frames}))
        }
        "replay_log" => crate::workbench::run_transfer(state, conn_id, params, true),
        // Subscriptions
        "subscribe" => subscribe(state, conn_id, &params),
        "unsubscribe" => unsubscribe(state, conn_id, &params),
        // Daemon management
        "daemon_info" => daemon_info(state),
        "shutdown" => shutdown(state),
        _ => Err(method_not_found(method)),
    }
}

fn attributed(
    state: &Arc<DaemonState>,
    conn: u64,
    method: &str,
    params: &serde_json::Value,
    result: Result<serde_json::Value, RpcError>,
) -> Result<serde_json::Value, RpcError> {
    let mut value = result?;
    let note = serde_json::json!({"session_id":params["session_id"],"kind":"operation","label":format!("{method}: {params}")});
    if let Err(error) = crate::workbench::dispatch(state, conn, "add_marker", &note) {
        value["annotation_error"] = serde_json::json!(error.message);
    }
    Ok(value)
}

fn hello(state: &DaemonState, v: &serde_json::Value) -> Result<serde_json::Value, RpcError> {
    let p: HelloParams = serde_json::from_value(v.clone()).map_err(|e| bad_params("hello", e))?;
    if p.proto != PROTOCOL_VERSION {
        return Err(RpcError {
            code: error_code::INVALID_PARAMS,
            message: flattencom_core::tr!(
                "Protocol version mismatch: client {} / service {}",
                p.proto,
                PROTOCOL_VERSION
            ),
            data: serde_json::Value::Null,
        });
    }
    if p.token.as_deref() != Some(state.token.as_str()) {
        return Err(RpcError {
            code: error_code::UNAUTHORIZED,
            message: flattencom_core::i18n::text("Authentication failed: token mismatch. Use daemon.token from the current state directory.").into(),
            data: serde_json::Value::Null,
        });
    }
    if p.client.len() > flattencom_proto::methods::MAX_CLIENT_LABEL_BYTES {
        return Err(RpcError {
            code: error_code::INVALID_PARAMS,
            message: "Client label exceeds 256 UTF-8 bytes".into(),
            data: serde_json::json!({"field":"client","max_bytes":flattencom_proto::methods::MAX_CLIENT_LABEL_BYTES}),
        });
    }
    json(WelcomeResult {
        daemon: "flattencomd".into(),
        version: state.version.clone(),
        proto: PROTOCOL_VERSION,
        capabilities: capabilities(),
    })
}

#[must_use]
pub fn capabilities() -> Vec<String> {
    vec![
        "virtual_ports".into(), // virtual://echo / virtual://gen
        "decoders".into(),      // Built-in and process decoders
        "triggers".into(),      // Automatic responses and event recording
        "hotplug".into(),       // Port hotplug polling
        "replay".into(),        // Capture replay
        "export".into(),        // Log export
        "signals".into(),       // DTR/RTS/BREAK control
        "workbench".into(),     // markers, boots, selections, cancellable operations
        "device_identity".into(),
        "sent_history".into(),
    ]
}

fn list_ports(state: &DaemonState) -> Result<serde_json::Value, RpcError> {
    let ports = core_list_ports().map_err(rpc_err)?;
    let entries: Vec<PortEntry> = ports
        .into_iter()
        .map(|info| {
            let sessions_using = state.sessions_for_port(&info.path);
            PortEntry {
                info,
                busy: !sessions_using.is_empty(),
                sessions: sessions_using,
            }
        })
        .collect();
    json(ListPortsResult { ports: entries })
}

fn get_port_info(
    state: &DaemonState,
    v: &serde_json::Value,
) -> Result<serde_json::Value, RpcError> {
    let p: GetPortInfoParams = params_of("get_port_info", v)?;
    let ports = core_list_ports().map_err(rpc_err)?;
    let info = ports
        .into_iter()
        .find(|i| i.path == p.path)
        .ok_or_else(|| {
            rpc_err(flattencom_core::FlattenError::PortNotFound {
                path: p.path.clone(),
            })
        })?;
    let sessions_using = state.sessions_for_port(&p.path);
    json(GetPortInfoResult {
        port: PortEntry {
            info,
            busy: !sessions_using.is_empty(),
            sessions: sessions_using,
        },
    })
}

fn open_session(
    state: &Arc<DaemonState>,
    conn_id: u64,
    v: &serde_json::Value,
) -> Result<serde_json::Value, RpcError> {
    let p: OpenSessionParams = params_of("open_session", v)?;
    let mut cfg = SerialConfig::new(&p.path);
    if let Some(b) = p.baud {
        cfg.baud = b;
    }
    if let Some(x) = parse_enum(p.data_bits.as_ref())? {
        cfg.data_bits = x;
    }
    if let Some(x) = parse_enum(p.parity.as_ref())? {
        cfg.parity = x;
    }
    if let Some(x) = parse_enum(p.stop_bits.as_ref())? {
        cfg.stop_bits = x;
    }
    if let Some(x) = parse_enum(p.flow_control.as_ref())? {
        cfg.flow_control = x;
    }
    if let Some(x) = p.read_timeout_ms {
        cfg.read_timeout_ms = x;
    }
    if let Some(x) = p.exclusive {
        cfg.exclusive = x;
    }
    if let Some(x) = p.label.clone() {
        cfg.label = Some(x);
    }
    if let Some(x) = p.record_to.clone() {
        cfg.record_to = Some(PathBuf::from(x));
    }
    cfg.record_rx_to = p.record_rx_to.map(PathBuf::from);
    cfg.record_keep_segments = p.record_keep_segments;
    if let Some(x) = p.buffer_max_frames {
        cfg.buffer.max_frames = x;
    }
    if let Some(x) = p.buffer_max_bytes {
        cfg.buffer.max_bytes = x;
    }
    let owner = state
        .conns
        .lock()
        .expect("Connection table lock poisoned")
        .get(&conn_id)
        .map_or_else(|| "unknown".into(), |c| c.label.clone());
    let result = state.open_session(owner, conn_id, cfg).map_err(rpc_err)?;
    if !result.reused {
        state.spawn_pump(SessionId::parse(&result.session.session_id).map_err(rpc_err)?);
    }
    json(result)
}

fn close_session(
    state: &Arc<DaemonState>,
    conn_id: u64,
    v: &serde_json::Value,
) -> Result<serde_json::Value, RpcError> {
    let p: CloseSessionParams = params_of("close_session", v)?;
    let id = SessionId::parse(&p.session_id).map_err(rpc_err)?;
    // Only the owner may close normally; other clients must explicitly force closure.
    let owner_conn = {
        let sessions = state.sessions.lock().expect("Session table lock poisoned");
        sessions.get(&id).map(|e| e.owner_conn)
    };
    let Some(owner_conn) = owner_conn else {
        return Err(rpc_err(flattencom_core::FlattenError::SessionNotFound(
            id.to_string(),
        )));
    };
    if owner_conn != conn_id && !p.force {
        return Err(RpcError {
            code: error_code::INVALID_PARAMS,
            message: flattencom_core::i18n::text(
                "Only the session owner can close it; use force: true to override",
            )
            .into(),
            data: serde_json::Value::Null,
        });
    }
    let stats = state.close_session(&id).ok_or_else(|| {
        rpc_err(flattencom_core::FlattenError::SessionNotFound(
            id.to_string(),
        ))
    })?;
    json(CloseSessionResult { stats })
}

fn list_sessions(state: &Arc<DaemonState>) -> Result<serde_json::Value, RpcError> {
    json(ListSessionsResult {
        sessions: state.session_summaries(),
    })
}

fn configure_session(
    state: &Arc<DaemonState>,
    v: &serde_json::Value,
) -> Result<serde_json::Value, RpcError> {
    let p: ConfigureSessionParams = params_of("configure_session", v)?;
    let id = SessionId::parse(&p.session_id).map_err(rpc_err)?;
    let s = session(state, &id)?;
    let patch = ConfigPatch {
        baud: p.baud,
        data_bits: parse_enum(p.data_bits.as_ref())?,
        parity: parse_enum(p.parity.as_ref())?,
        stop_bits: parse_enum(p.stop_bits.as_ref())?,
        flow_control: parse_enum(p.flow_control.as_ref())?,
        read_timeout_ms: p.read_timeout_ms,
        label: p.label,
    };
    let cfg = s.configure(patch).map_err(rpc_err)?;
    json(ConfigureSessionResult { config: cfg })
}

fn send(
    state: &Arc<DaemonState>,
    conn_id: u64,
    v: &serde_json::Value,
) -> Result<serde_json::Value, RpcError> {
    let p: SendParams = params_of("send", v)?;
    if p.data.is_some() == p.hex.is_some() {
        return Err(RpcError {
            code: error_code::INVALID_PARAMS,
            message: "Exactly one of data or hex is required".into(),
            data: serde_json::Value::Null,
        });
    }
    let id = SessionId::parse(&p.session_id).map_err(rpc_err)?;
    let s = session(state, &id)?;
    let mut bytes: Vec<u8> = if let Some(h) = &p.hex {
        hex::decode(
            h.chars()
                .filter(|c| !c.is_ascii_whitespace())
                .collect::<String>(),
        )
        .map_err(|e| RpcError {
            code: error_code::INVALID_PARAMS,
            message: flattencom_core::tr!("Invalid hex parameter: {e}", e = e),
            data: serde_json::Value::Null,
        })?
    } else if let Some(d) = &p.data {
        d.as_bytes().to_vec()
    } else {
        Vec::new()
    };
    if p.hex.is_none() || v.get("newline").is_some() {
        bytes.extend_from_slice(p.newline.suffix());
    }
    let n = bytes.len() as u64;
    let seq = s
        .send_from(bytes, state.client_source(conn_id))
        .map_err(rpc_err)?;
    json(SendResult { bytes_sent: n, seq })
}

fn read_frames(
    state: &Arc<DaemonState>,
    conn_id: u64,
    v: &serde_json::Value,
) -> Result<serde_json::Value, RpcError> {
    let p: ReadFramesParams = params_of("read_frames", v)?;
    let id = SessionId::parse(&p.session_id).map_err(rpc_err)?;
    let s = session(state, &id)?;
    let max_bytes = p.max_bytes.unwrap_or(256 * 1024).clamp(1, 1024 * 1024);
    let cutoff = p.last_ms.map(|ms| {
        now_us().saturating_sub(i64::try_from(ms.saturating_mul(1000)).unwrap_or(i64::MAX))
    });
    let filter = state.view_filter(conn_id, &id);
    let page = s.read_window_filtered(p.since_seq, cutoff, p.tail, max_bytes, filter.as_ref());
    json(page_out(&page, p.format))
}

fn get_log(
    state: &Arc<DaemonState>,
    conn_id: u64,
    v: &serde_json::Value,
) -> Result<serde_json::Value, RpcError> {
    let p: GetLogParams = params_of("get_log", v)?;
    let id = SessionId::parse(&p.session_id).map_err(rpc_err)?;
    let s = session(state, &id)?;
    let filter = state.view_filter(conn_id, &id);
    let page = s.read_window_filtered(p.from_seq, None, false, 256 * 1024, filter.as_ref());
    let mut out = page_out(&page, p.format);
    if let Some(to) = p.to_seq {
        out.frames.retain(|f| f.seq < to);
        out.next_seq = out.next_seq.min(to);
    }
    json(out)
}

fn clear_buffer(
    state: &Arc<DaemonState>,
    v: &serde_json::Value,
) -> Result<serde_json::Value, RpcError> {
    let id = session_id(v)?;
    let s = session(state, &id)?;
    let n = s.clear_buffer();
    json(ClearedResult { cleared: n })
}

fn flush_rx(
    state: &Arc<DaemonState>,
    v: &serde_json::Value,
) -> Result<serde_json::Value, RpcError> {
    let id = session_id(v)?;
    let s = session(state, &id)?;
    let n = s.flush_rx().map_err(rpc_err)?;
    json(ClearedResult { cleared: n })
}

fn flush(state: &Arc<DaemonState>, v: &serde_json::Value) -> Result<serde_json::Value, RpcError> {
    let id = session_id(v)?;
    let s = session(state, &id)?;
    // flush waits for pending driver transmission via flush_tx.
    s.flush_tx().map_err(rpc_err)?;
    json(OkResult { ok: true })
}

fn set_decoder(
    state: &Arc<DaemonState>,
    v: &serde_json::Value,
) -> Result<serde_json::Value, RpcError> {
    let p: SetDecoderParams = params_of("set_decoder", v)?;
    let id = SessionId::parse(&p.session_id).map_err(rpc_err)?;
    let s = session(state, &id)?;
    s.set_decoder(p.spec).map_err(rpc_err)?;
    json(OkResult { ok: true })
}

fn get_decoder(
    state: &Arc<DaemonState>,
    v: &serde_json::Value,
) -> Result<serde_json::Value, RpcError> {
    let id = session_id(v)?;
    let s = session(state, &id)?;
    let spec = s.decoder_config();
    json(GetDecoderResult { spec })
}

fn list_decoders() -> Result<serde_json::Value, RpcError> {
    let mut decoders = std::collections::BTreeMap::new();
    for id in flattencom_core::decode::DECODER_IDS {
        if let Some(desc) = flattencom_core::decode::DecoderRegistry::describe(id) {
            decoders.insert((*id).to_owned(), desc.to_owned());
        }
    }
    json(DecoderCatalogResult { decoders })
}

fn set_filter(
    state: &Arc<DaemonState>,
    conn_id: u64,
    v: &serde_json::Value,
) -> Result<serde_json::Value, RpcError> {
    let p: SetFilterParams = params_of("set_filter", v)?;
    let id = SessionId::parse(&p.session_id).map_err(rpc_err)?;
    session(state, &id)?;
    let compiled = p
        .filter
        .as_ref()
        .map(flattencom_core::filter::CompiledFilter::compile)
        .transpose()
        .map_err(rpc_err)?;
    state
        .set_view_filter(conn_id, id, compiled)
        .map_err(rpc_err)?;
    json(SetFilterResult {
        active: p.filter.is_some(),
    })
}

fn set_triggers(
    state: &Arc<DaemonState>,
    v: &serde_json::Value,
) -> Result<serde_json::Value, RpcError> {
    let p: SetTriggersParams = params_of("set_triggers", v)?;
    let id = SessionId::parse(&p.session_id).map_err(rpc_err)?;
    let s = session(state, &id)?;
    let count = p.triggers.len();
    s.set_triggers(p.triggers).map_err(rpc_err)?;
    json(SetTriggersResult { count })
}

fn get_triggers(
    state: &Arc<DaemonState>,
    v: &serde_json::Value,
) -> Result<serde_json::Value, RpcError> {
    let id = session_id(v)?;
    let s = session(state, &id)?;
    json(GetTriggersResult {
        triggers: s.trigger_specs(),
    })
}

fn get_trigger_fires(
    state: &Arc<DaemonState>,
    v: &serde_json::Value,
) -> Result<serde_json::Value, RpcError> {
    let id = session_id(v)?;
    let s = session(state, &id)?;
    json(GetTriggerFiresResult {
        fires: s.trigger_fires(),
    })
}

fn set_signals(
    state: &Arc<DaemonState>,
    v: &serde_json::Value,
) -> Result<serde_json::Value, RpcError> {
    let p: SetSignalsParams = params_of("set_signals", v)?;
    let id = SessionId::parse(&p.session_id).map_err(rpc_err)?;
    let s = session(state, &id)?;
    let pins = s.set_signals(p.dtr, p.rts).map_err(rpc_err)?;
    json(SignalsResult { pins })
}

fn get_signals(
    state: &Arc<DaemonState>,
    v: &serde_json::Value,
) -> Result<serde_json::Value, RpcError> {
    let id = session_id(v)?;
    let s = session(state, &id)?;
    let pins = s.read_signals().map_err(rpc_err)?;
    json(SignalsResult { pins })
}

fn send_break(
    state: &Arc<DaemonState>,
    v: &serde_json::Value,
) -> Result<serde_json::Value, RpcError> {
    let p: SendBreakParams = params_of("send_break", v)?;
    let id = SessionId::parse(&p.session_id).map_err(rpc_err)?;
    let s = session(state, &id)?;
    let d = Duration::from_millis(p.duration_ms.unwrap_or(100).clamp(1, 5_000));
    s.send_break(d).map_err(rpc_err)?;
    json(OkResult { ok: true })
}

fn get_stats(
    state: &Arc<DaemonState>,
    v: &serde_json::Value,
) -> Result<serde_json::Value, RpcError> {
    let id = session_id(v)?;
    let s = session(state, &id)?;
    json(GetStatsResult { stats: s.stats() })
}

fn export_log(
    state: &Arc<DaemonState>,
    conn_id: u64,
    v: &serde_json::Value,
) -> Result<serde_json::Value, RpcError> {
    let p: ExportLogParams = params_of("export_log", v)?;
    let id = SessionId::parse(&p.session_id).map_err(rpc_err)?;
    let s = session(state, &id)?;
    let fmt = match p.format {
        ExportFormatParam::Txt => ExportFormat::Txt,
        ExportFormatParam::Hex => ExportFormat::Hex,
        ExportFormatParam::Csv => ExportFormat::Csv,
        ExportFormatParam::Jsonl => ExportFormat::Jsonl,
        ExportFormatParam::Pcap => ExportFormat::Pcap,
    };
    let path = p.path.map_or_else(
        || {
            state_dir().join("exports").join(format!(
                "session-{}-{}.{}",
                id,
                timefmt::fmt_clock_us(now_us()).replace(':', ""),
                fmt.ext()
            ))
        },
        PathBuf::from,
    );
    let filter = if p.all {
        None
    } else {
        state.view_filter(conn_id, &id)
    };
    let r = s
        .export_filtered(fmt, &path, filter.as_ref())
        .map_err(rpc_err)?;
    json(ExportLogResult {
        path: r.path.to_string_lossy().into_owned(),
        frames: r.frames,
        bytes: r.bytes,
    })
}

fn subscribe(
    state: &Arc<DaemonState>,
    conn_id: u64,
    v: &serde_json::Value,
) -> Result<serde_json::Value, RpcError> {
    let p: SubscribeParams = params_of("subscribe", v)?;
    let id = SessionId::parse(&p.session_id).map_err(rpc_err)?;
    // Validate that the session exists.
    session(state, &id)?;
    let kinds: HashSet<String> = match p.kinds {
        Some(ks) => ks
            .into_iter()
            .map(|k| match k {
                SubKind::Frames => "frames".to_owned(),
                SubKind::Stats => "stats".to_owned(),
                SubKind::State => "state".to_owned(),
                SubKind::TriggerFires => "trigger_fires".to_owned(),
            })
            .collect(),
        None => HashSet::new(), // Empty set means all event kinds.
    };
    state
        .set_subscription(conn_id, id, kinds)
        .map_err(rpc_err)?;
    json(SubscribeResult { ok: true })
}

fn unsubscribe(
    state: &Arc<DaemonState>,
    conn_id: u64,
    v: &serde_json::Value,
) -> Result<serde_json::Value, RpcError> {
    let p: SessionIdParams = params_of("unsubscribe", v)?;
    let id = SessionId::parse(&p.session_id).map_err(rpc_err)?;
    state.clear_subscription(conn_id, id);
    json(SubscribeResult { ok: true })
}

fn daemon_info(state: &Arc<DaemonState>) -> Result<serde_json::Value, RpcError> {
    let sessions = state
        .sessions
        .lock()
        .expect("Session table lock poisoned")
        .len();
    json(DaemonInfoResult {
        daemon: "flattencomd".into(),
        version: state.version.clone(),
        proto: PROTOCOL_VERSION,
        uptime_ms: state.started.elapsed().as_millis() as u64,
        sessions,
        socket: state.socket_path.to_string_lossy().into_owned(),
        capabilities: capabilities(),
    })
}

fn shutdown(state: &Arc<DaemonState>) -> Result<serde_json::Value, RpcError> {
    state
        .shutting_down
        .store(true, std::sync::atomic::Ordering::Relaxed);
    json(ShutdownResult { ok: true })
}

/// Broadcast hotplug events from the poller to all connections.
pub fn broadcast_hotplug(state: &Arc<DaemonState>, ev: flattencom_core::discovery::HotplugEvent) {
    let (added, removed) = match ev {
        flattencom_core::discovery::HotplugEvent::Added { ports } => (ports, Vec::new()),
        flattencom_core::discovery::HotplugEvent::Removed { paths } => (Vec::new(), paths),
    };
    let note = RpcNotification {
        jsonrpc: "2.0".into(),
        method: "ports_changed".into(),
        params: serde_json::json!({ "added": added, "removed": removed }),
    };
    if let Some(line) = crate::wire::notification(&note) {
        state.broadcast(&line);
    }
}
