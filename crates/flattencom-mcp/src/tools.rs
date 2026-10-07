/*
 * flattencom - flattencom Flattencom-Mcp Src Tools
 *
 * Defines MCP tool schemas and adapts tool calls to serial service RPC operations.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

//! flattencom MCP tools adapting daemon RPC for clients.
//!
//! Tools use snake_case names and readable errors with recovery guidance;
//! daemon FlattenError messages are passed through.
//! Shared protocol types provide parameters/results and generated schemas where available.

use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::{tool, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use flattencom_core::decode::DecoderSpec;
use flattencom_core::filter::FilterSpec;
use flattencom_core::trigger::TriggerSpec;
use flattencom_proto::methods::{
    ClearedResult, CloseSessionResult, ConfigureSessionResult, DecoderCatalogResult,
    ExportLogResult, FramesPageOut, GetDecoderResult, GetPortInfoResult, GetStatsResult,
    GetTriggerFiresResult, GetTriggersResult, ListPortsResult, ListSessionsResult, OkResult,
    OpenSessionResult, ReplayLogResult, SendFileResult, SendResult, SetFilterResult,
    SetTriggersResult, SignalsResult,
};

use crate::server::FlattencomMcp;

// ---------------------------------------------------------------------------
// Tool parameters: omitted optional fields use application defaults.
// ---------------------------------------------------------------------------

/// Parameters for `open_port`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct OpenPortParams {
    /// Serial port path, such as /dev/ttyUSB0, COM3 or virtual://echo.
    pub path: String,
    /// Baud rate.
    pub baud_rate: Option<u32>,
    /// Data bits: five, six, seven or eight (default).
    pub data_bits: Option<String>,
    /// Parity: none (default), odd, even, mark or space.
    pub parity: Option<String>,
    /// Stop bits: one (default), one_point_five or two. one_point_five requires five data bits.
    pub stop_bits: Option<String>,
    /// Flow control: none (default), software or hardware.
    pub flow_control: Option<String>,
    /// Session label.
    pub label: Option<String>,
    /// Open the port exclusively.
    pub exclusive: Option<bool>,
    /// Record to one .log file including traffic and markers; legacy .txt and JSONL paths remain accepted.
    pub record_to: Option<String>,
    /// New raw RX output file. The daemon preserves device bytes without adding TX or timestamps.
    pub record_rx_to: Option<String>,
}

/// Parameters for `configure_port`; only supplied fields change.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ConfigurePortParams {
    /// Session ID.
    pub session_id: String,
    /// Baud rate.
    pub baud_rate: Option<u32>,
    /// Data bits.
    pub data_bits: Option<String>,
    /// Parity.
    pub parity: Option<String>,
    /// Stop bits: one (default), one_point_five or two. one_point_five requires five data bits.
    pub stop_bits: Option<String>,
    /// Flow control.
    pub flow_control: Option<String>,
    /// Label; null clears it.
    #[serde(default, deserialize_with = "flattencom_core::config::nullable_label")]
    pub label: Option<Option<String>>,
}

/// Parameters for `send`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SendToolParams {
    /// Session ID.
    pub session_id: String,
    /// Text payload; use either data or hex.
    pub data: Option<String>,
    /// Hexadecimal bytes; spaces allowed; mutually exclusive with data.
    pub hex: Option<String>,
    /// Line ending: crlf (default), lf, cr or none.
    pub newline: Option<String>,
}

/// Parameters for `send_file`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SendFileToolParams {
    /// Session ID.
    pub session_id: String,
    /// File path.
    pub path: String,
    /// Chunk size in bytes (default 1024, capped at 4096).
    pub chunk_size: Option<usize>,
    /// Delay between chunks in milliseconds (default 0).
    pub pacing_ms: Option<u64>,
}

/// Parameters for `read_frames`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ReadFramesToolParams {
    /// Session ID.
    pub session_id: String,
    /// Inclusive cursor from the previous next_seq; defaults to the beginning.
    pub since_seq: Option<u64>,
    /// Only include frames from the last N milliseconds.
    pub last_ms: Option<u64>,
    /// Soft output size limit in bytes (default 256 KiB).
    pub max_bytes: Option<u64>,
    /// Read a bounded tail when true; otherwise paginate by cursor.
    pub tail: Option<bool>,
    /// Output format: raw, hex, text or decoded (default).
    pub format: Option<String>,
}

/// Parameters for historical-range `get_log` reads.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GetLogToolParams {
    /// Session ID.
    pub session_id: String,
    /// Inclusive first sequence; defaults to the beginning.
    pub from_seq: Option<u64>,
    /// Exclusive end sequence; defaults to the end.
    pub to_seq: Option<u64>,
}

/// Session identifier parameters.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SessionOnlyParams {
    /// Session ID.
    pub session_id: String,
}

/// Explicit GUI-published evidence; immutable until revoked.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SelectionParams {
    /// ID copied by the GUI's share-selection action.
    pub selection_id: String,
}

/// Persistent annotation anchored to a captured chunk.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MarkerParams {
    pub session_id: String,
    pub label: String,
    pub seq: Option<u64>,
}

use flattencom_proto::workbench::TransferParams;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct OperationParams {
    pub operation_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ResetStep {
    pub dtr: Option<bool>,
    pub rts: Option<bool>,
    pub data: Option<String>,
    pub hex: Option<String>,
    pub delay_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ResetParams {
    pub session_id: String,
    /// Explicit board-specific sequence; no universal reset is assumed.
    pub steps: Vec<ResetStep>,
}

/// Parameters for `close_port`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ClosePortParams {
    /// Session ID.
    pub session_id: String,
    /// Set true to close a session owned by another client.
    pub force: Option<bool>,
}

/// Parameters for `set_decoder`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SetDecoderToolParams {
    /// Session ID.
    pub session_id: String,
    /// Decoder specification; null disables decoding.
    pub spec: Option<DecoderSpec>,
}

/// Parameters for `set_filter`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SetFilterToolParams {
    /// Session ID.
    pub session_id: String,
    /// Server-side read filter; null clears it.
    pub filter: Option<FilterSpec>,
}

/// Parameters for `set_triggers`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SetTriggersToolParams {
    /// Session ID.
    pub session_id: String,
    /// Trigger list, replacing the current configuration.
    pub triggers: Vec<TriggerSpec>,
}

/// Parameters for `set_signals`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SetSignalsToolParams {
    /// Session ID.
    pub session_id: String,
    /// DTR state; its physical effect depends on board wiring.
    pub dtr: Option<bool>,
    /// RTS state.
    pub rts: Option<bool>,
}

/// Parameters for `send_break`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SendBreakToolParams {
    /// Session ID.
    pub session_id: String,
    /// BREAK duration in milliseconds (default 100).
    pub duration_ms: Option<u64>,
}

/// Parameters for `export_log`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ExportLogToolParams {
    /// Session ID.
    pub session_id: String,
    /// Export format: txt (default), hex, csv or jsonl.
    pub format: Option<String>,
    /// Export path; defaults to a generated file in the service data directory.
    pub path: Option<String>,
    /// Export all retained frames when true, otherwise use the client filter.
    pub all: Option<bool>,
}

/// Parameters for `replay_log`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ReplayLogToolParams {
    /// Target session ID (must already be open).
    pub session_id: String,
    /// JSONL capture file path.
    pub path: String,
    /// Playback speed multiplier.
    pub speed: Option<f64>,
}

/// Port-path parameters.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PortPathParams {
    /// Serial port path.
    pub path: String,
}

// ---------------------------------------------------------------------------
// Tool routes
// ---------------------------------------------------------------------------

#[tool_router(router = tool_router, vis = "pub")]
impl FlattencomMcp {
    #[tool(
        description = "Read successful transmitted chunks, including timestamps, sequence numbers and source. Use next_seq as the next since_seq. Transmission does not confirm device execution."
    )]
    pub async fn read_sent(
        &self,
        p: Parameters<ReadFramesToolParams>,
    ) -> Result<Json<FramesPageOut>, String> {
        let mut req = json!({ "session_id": p.0.session_id });
        put_opt(&mut req, "since_seq", p.0.since_seq);
        put_opt(&mut req, "max_bytes", p.0.max_bytes);
        put_opt(&mut req, "format", p.0.format);
        self.rpc("read_sent", &req).await.map(Json)
    }
    #[tool(
        description = "Read an immutable GUI-published text selection. No surrounding log data is included. Revoked selections cannot be read."
    )]
    pub async fn read_selection(
        &self,
        p: Parameters<SelectionParams>,
    ) -> Result<Json<serde_json::Value>, String> {
        self.rpc("read_selection", &p.0).await.map(Json)
    }

    #[tool(
        description = "List annotations and detected boot events, including timestamps, sequence numbers and boot intervals."
    )]
    pub async fn list_markers(
        &self,
        p: Parameters<SessionOnlyParams>,
    ) -> Result<Json<serde_json::Value>, String> {
        self.rpc("list_markers", &p.0).await.map(Json)
    }

    #[tool(
        description = "Add a marker with client attribution to the same text capture. No separate file is created."
    )]
    pub async fn add_marker(
        &self,
        p: Parameters<MarkerParams>,
    ) -> Result<Json<serde_json::Value>, String> {
        self.rpc("add_marker", &p.0).await.map(Json)
    }

    #[tool(
        description = "Start a background chunked text, HEX or file transfer. Returns operation.id. Use operation_status for progress and cancel_operation to stop subsequent chunks."
    )]
    pub async fn start_transfer(
        &self,
        p: Parameters<TransferParams>,
    ) -> Result<Json<serde_json::Value>, String> {
        self.rpc("start_transfer", &p.0).await.map(Json)
    }

    #[tool(
        description = "Read transfer or reset progress and state: running, completed, cancelled or failed."
    )]
    pub async fn operation_status(
        &self,
        p: Parameters<OperationParams>,
    ) -> Result<Json<serde_json::Value>, String> {
        self.rpc("operation_status", &p.0).await.map(Json)
    }

    #[tool(
        description = "Request operation cancellation. A current write may finish first; poll operation_status for the final state."
    )]
    pub async fn cancel_operation(
        &self,
        p: Parameters<OperationParams>,
    ) -> Result<Json<serde_json::Value>, String> {
        self.rpc("cancel_operation", &p.0).await.map(Json)
    }

    #[tool(
        description = "Execute board-specific reset steps using DTR, RTS, text, HEX and delays. Use list_markers to inspect subsequent boot events."
    )]
    pub async fn reset_device(
        &self,
        p: Parameters<ResetParams>,
    ) -> Result<Json<serde_json::Value>, String> {
        let mut value = serde_json::to_value(p.0).map_err(|e| e.to_string())?;
        if let Some(steps) = value["steps"].as_array_mut() {
            for step in steps {
                if let Some(step) = step.as_object_mut() {
                    step.retain(|_, v| !v.is_null());
                }
            }
        }
        self.rpc("reset_device", &value).await.map(Json)
    }

    /// Wait for the driver transmit buffer to drain.
    #[tool(description = "Wait for the serial driver transmit buffer to empty.")]
    pub async fn flush(&self, p: Parameters<SessionOnlyParams>) -> Result<Json<OkResult>, String> {
        self.rpc("flush", &json!({"session_id": p.0.session_id}))
            .await
            .map(Json)
    }

    /// Clear unread driver receive data.
    #[tool(
        description = "Clear unread driver input and session RX buffers. Sequence numbers and TX history are preserved."
    )]
    pub async fn flush_rx(
        &self,
        p: Parameters<SessionOnlyParams>,
    ) -> Result<Json<ClearedResult>, String> {
        self.rpc("flush_rx", &json!({"session_id": p.0.session_id}))
            .await
            .map(Json)
    }
    /// ESP32 flashing through installed esptool, with its protocol and verification.
    #[tool(
        description = "Flash an ESP32 using installed esptool. Close the serial session first and obtain the correct offset from the firmware flash map. send_file is not a flashing protocol."
    )]
    pub async fn flash_esp32(
        &self,
        p: Parameters<crate::flash::FlashParams>,
    ) -> Result<Json<crate::flash::FlashResult>, String> {
        let sessions: ListSessionsResult = self.rpc("list_sessions", &json!({})).await?;
        if sessions.sessions.iter().any(|s| s.path == p.0.port) {
            return Err(flattencom_core::i18n::text(
                "Port is in use. Call close_port before flashing.",
            )
            .into());
        }
        crate::flash::run(&p.0).await.map(Json)
    }
    /// Enumerate port paths, friendly names, VID/PID and session usage without opening ports.
    #[tool(
        description = "List port paths, USB identifiers, manufacturers and session ownership without opening ports."
    )]
    pub async fn list_ports(&self) -> Result<Json<ListPortsResult>, String> {
        self.rpc("list_ports", &json!({})).await.map(Json)
    }

    /// Inspect a port and its sessions.
    #[tool(description = "Read device metadata and the sessions currently using a port.")]
    pub async fn get_port_info(
        &self,
        p: Parameters<PortPathParams>,
    ) -> Result<Json<GetPortInfoResult>, String> {
        self.rpc("get_port_info", &json!({ "path": p.0.path }))
            .await
            .map(Json)
    }

    /// Open a serial session; omitted settings default to 115200 8N1.
    #[tool(
        description = "Open or join a serial session. Existing connected or reconnecting sessions are reused with their original configuration, owner and captures (reused=true). New sessions default to 115200 8N1."
    )]
    pub async fn open_port(
        &self,
        p: Parameters<OpenPortParams>,
    ) -> Result<Json<OpenSessionResult>, String> {
        let mut req = json!({ "path": p.0.path });
        put_opt(&mut req, "baud", p.0.baud_rate);
        put_opt(&mut req, "data_bits", p.0.data_bits);
        put_opt(&mut req, "parity", p.0.parity);
        put_opt(&mut req, "stop_bits", p.0.stop_bits);
        put_opt(&mut req, "flow_control", p.0.flow_control);
        put_opt(&mut req, "label", p.0.label);
        put_opt(&mut req, "exclusive", p.0.exclusive);
        put_opt(&mut req, "record_to", p.0.record_to);
        put_opt(&mut req, "record_rx_to", p.0.record_rx_to);
        self.rpc("open_session", &req).await.map(Json)
    }

    /// Close a session.
    #[tool(description = "Close a serial session. Requires ownership or force=true.")]
    pub async fn close_port(
        &self,
        p: Parameters<ClosePortParams>,
    ) -> Result<Json<CloseSessionResult>, String> {
        self.rpc(
            "close_session",
            &json!({ "session_id": p.0.session_id, "force": p.0.force.unwrap_or(false) }),
        )
        .await
        .map(Json)
    }

    /// List current sessions.
    #[tool(
        description = "List background service sessions, including configuration, state and statistics."
    )]
    pub async fn list_sessions(&self) -> Result<Json<ListSessionsResult>, String> {
        self.rpc("list_sessions", &json!({})).await.map(Json)
    }

    /// Change live settings such as baud rate.
    #[tool(
        description = "Change baud rate, data bits, parity, stop bits, flow control or label without reopening the port."
    )]
    pub async fn configure_port(
        &self,
        p: Parameters<ConfigurePortParams>,
    ) -> Result<Json<ConfigureSessionResult>, String> {
        let mut req = json!({ "session_id": p.0.session_id });
        put_opt(&mut req, "baud", p.0.baud_rate);
        put_opt(&mut req, "data_bits", p.0.data_bits);
        put_opt(&mut req, "parity", p.0.parity);
        put_opt(&mut req, "stop_bits", p.0.stop_bits);
        put_opt(&mut req, "flow_control", p.0.flow_control);
        if let Some(l) = p.0.label {
            req["label"] = json!(l);
        }
        self.rpc("configure_session", &req).await.map(Json)
    }

    /// Send text or hexadecimal bytes.
    #[tool(
        description = "Send exactly one of text or hexadecimal bytes. newline accepts crlf, lf, cr or none. Returns bytes sent and the TX sequence number."
    )]
    pub async fn send(&self, p: Parameters<SendToolParams>) -> Result<Json<SendResult>, String> {
        let mut req = json!({ "session_id": p.0.session_id });
        put_opt(&mut req, "data", p.0.data);
        put_opt(&mut req, "hex", p.0.hex);
        put_opt(&mut req, "newline", p.0.newline);
        self.rpc("send", &req).await.map(Json)
    }

    /// Send a file with optional pacing for firmware data or scripts.
    #[tool(
        description = "Send a file in chunks (default 1024 bytes, capped at 4096), with optional pacing_ms between chunks. Use start_transfer for progress and cancellation."
    )]
    pub async fn send_file(
        &self,
        p: Parameters<SendFileToolParams>,
    ) -> Result<Json<SendFileResult>, String> {
        let mut req = json!({ "session_id": p.0.session_id, "path": p.0.path });
        put_opt(&mut req, "chunk_size", p.0.chunk_size);
        put_opt(&mut req, "pacing_ms", p.0.pacing_ms);
        self.rpc_slow("send_file", &req).await.map(Json)
    }

    /// Incremental frame reads, the primary receive API.
    #[tool(
        description = "Read frames incrementally using the previous next_seq as since_seq. last_ms limits the time window; format selects raw, hex, text or decoded. dropped_rx counts evicted frames."
    )]
    pub async fn read_frames(
        &self,
        p: Parameters<ReadFramesToolParams>,
    ) -> Result<Json<FramesPageOut>, String> {
        let mut req = json!({ "session_id": p.0.session_id });
        put_opt(&mut req, "since_seq", p.0.since_seq);
        put_opt(&mut req, "last_ms", p.0.last_ms);
        put_opt(&mut req, "max_bytes", p.0.max_bytes);
        put_opt(&mut req, "tail", p.0.tail);
        put_opt(&mut req, "format", p.0.format);
        self.rpc("read_frames", &req).await.map(Json)
    }

    /// Read a range of historical frames.
    #[tool(
        description = "Read buffered frames in the half-open sequence range [from_seq, to_seq). Omitted bounds include all retained frames."
    )]
    pub async fn get_log(
        &self,
        p: Parameters<GetLogToolParams>,
    ) -> Result<Json<FramesPageOut>, String> {
        let mut req = json!({ "session_id": p.0.session_id });
        put_opt(&mut req, "from_seq", p.0.from_seq);
        put_opt(&mut req, "to_seq", p.0.to_seq);
        self.rpc("get_log", &req).await.map(Json)
    }

    /// Clear the display buffer.
    #[tool(
        description = "Clear buffered frames without resetting sequence numbers or modifying capture files."
    )]
    pub async fn clear_buffer(
        &self,
        p: Parameters<SessionOnlyParams>,
    ) -> Result<Json<ClearedResult>, String> {
        self.rpc("clear_buffer", &json!({ "session_id": p.0.session_id }))
            .await
            .map(Json)
    }

    /// Decoder catalog.
    #[tool(description = "List available decoders and their descriptions.")]
    pub async fn list_decoders(&self) -> Result<Json<DecoderCatalogResult>, String> {
        self.rpc("list_decoders", &json!({})).await.map(Json)
    }

    /// Configure backend decoding to return structured fields.
    #[tool(
        description = "Set the session decoder. Modbus role accepts auto, master or slave. Set spec=null to disable decoding."
    )]
    pub async fn set_decoder(
        &self,
        p: Parameters<SetDecoderToolParams>,
    ) -> Result<Json<OkResult>, String> {
        let req = json!({ "session_id": p.0.session_id, "spec": p.0.spec });
        self.rpc("set_decoder", &req).await.map(Json)
    }

    /// Query the current decoder.
    #[tool(description = "Read the current decoder configuration, or null when disabled.")]
    pub async fn get_decoder(
        &self,
        p: Parameters<SessionOnlyParams>,
    ) -> Result<Json<GetDecoderResult>, String> {
        self.rpc("get_decoder", &json!({ "session_id": p.0.session_id }))
            .await
            .map(Json)
    }

    /// Set a server-side view filter to avoid fetching unrelated data.
    #[tool(
        description = "Set this client's read filter. Direction, text, regex and field conditions are combined with AND. Recording is unaffected. Set filter=null to clear it."
    )]
    pub async fn set_filter(
        &self,
        p: Parameters<SetFilterToolParams>,
    ) -> Result<Json<SetFilterResult>, String> {
        let req = json!({ "session_id": p.0.session_id, "filter": p.0.filter });
        self.rpc("set_filter", &req).await.map(Json)
    }

    /// Clear the view filter.
    #[tool(description = "Clear this client's read filter.")]
    pub async fn clear_filter(
        &self,
        p: Parameters<SessionOnlyParams>,
    ) -> Result<Json<SetFilterResult>, String> {
        self.rpc(
            "set_filter",
            &json!({ "session_id": p.0.session_id, "filter": null }),
        )
        .await
        .map(Json)
    }

    /// Configure triggers for automatic matching actions.
    #[tool(
        description = "Replace session triggers. Regex matches run respond (send bytes), highlight (record an event) or execute (run a program) actions."
    )]
    pub async fn set_triggers(
        &self,
        p: Parameters<SetTriggersToolParams>,
    ) -> Result<Json<SetTriggersResult>, String> {
        let req = json!({ "session_id": p.0.session_id, "triggers": p.0.triggers });
        self.rpc("set_triggers", &req).await.map(Json)
    }

    /// Query trigger specifications.
    #[tool(description = "Read configured session triggers.")]
    pub async fn get_triggers(
        &self,
        p: Parameters<SessionOnlyParams>,
    ) -> Result<Json<GetTriggersResult>, String> {
        self.rpc("get_triggers", &json!({ "session_id": p.0.session_id }))
            .await
            .map(Json)
    }

    /// Query trigger history.
    #[tool(description = "Read trigger matches and response status.")]
    pub async fn get_trigger_fires(
        &self,
        p: Parameters<SessionOnlyParams>,
    ) -> Result<Json<GetTriggerFiresResult>, String> {
        self.rpc(
            "get_trigger_fires",
            &json!({ "session_id": p.0.session_id }),
        )
        .await
        .map(Json)
    }

    /// Set DTR/RTS for board-specific reset or bootloader entry.
    #[tool(
        description = "Set DTR or RTS, leaving omitted lines unchanged. Returns line states and whether output states are known. Reset sequences depend on board wiring."
    )]
    pub async fn set_signals(
        &self,
        p: Parameters<SetSignalsToolParams>,
    ) -> Result<Json<SignalsResult>, String> {
        let mut req = json!({ "session_id": p.0.session_id });
        put_opt(&mut req, "dtr", p.0.dtr);
        put_opt(&mut req, "rts", p.0.rts);
        self.rpc("set_signals", &req).await.map(Json)
    }

    /// Read control-line states.
    #[tool(description = "Read DTR, RTS, CTS, DSR, DCD and RI states.")]
    pub async fn get_signals(
        &self,
        p: Parameters<SessionOnlyParams>,
    ) -> Result<Json<SignalsResult>, String> {
        self.rpc("get_signals", &json!({ "session_id": p.0.session_id }))
            .await
            .map(Json)
    }

    /// Send a BREAK signal.
    #[tool(description = "Send BREAK for duration_ms milliseconds (default 100).")]
    pub async fn send_break(
        &self,
        p: Parameters<SendBreakToolParams>,
    ) -> Result<Json<OkResult>, String> {
        let mut req = json!({ "session_id": p.0.session_id });
        put_opt(&mut req, "duration_ms", p.0.duration_ms);
        self.rpc("send_break", &req).await.map(Json)
    }

    /// Session rates, errors and eviction statistics.
    #[tool(
        description = "Read byte counts, rates, frame counts, reconnects, errors and buffer eviction counts."
    )]
    pub async fn get_stats(
        &self,
        p: Parameters<SessionOnlyParams>,
    ) -> Result<Json<GetStatsResult>, String> {
        self.rpc("get_stats", &json!({ "session_id": p.0.session_id }))
            .await
            .map(Json)
    }

    /// Export a session log.
    #[tool(description = "Export a session capture as txt, hex, csv or jsonl and return its path.")]
    pub async fn export_log(
        &self,
        p: Parameters<ExportLogToolParams>,
    ) -> Result<Json<ExportLogResult>, String> {
        let mut req = json!({ "session_id": p.0.session_id });
        put_opt(&mut req, "format", p.0.format);
        put_opt(&mut req, "path", p.0.path);
        put_opt(&mut req, "all", p.0.all);
        self.rpc("export_log", &req).await.map(Json)
    }

    /// Replay a capture into a session.
    #[tool(
        description = "Replay both RX and TX records from a JSONL file into the target session, scaling original intervals by speed."
    )]
    pub async fn replay_log(
        &self,
        p: Parameters<ReplayLogToolParams>,
    ) -> Result<Json<ReplayLogResult>, String> {
        let mut req = json!({ "session_id": p.0.session_id, "path": p.0.path });
        put_opt(&mut req, "speed", p.0.speed);
        self.rpc_slow("replay_log", &req).await.map(Json)
    }

    /// Background service information.
    #[tool(
        description = "Read the background service version, uptime, session count and capabilities."
    )]
    pub async fn daemon_info(&self) -> Result<Json<serde_json::Value>, String> {
        let mut info: serde_json::Value = self.rpc("daemon_info", &json!({})).await?;
        info["backend_connection"] = self.daemon.status().await;
        Ok(Json(info))
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Insert optional fields into a JSON object, skipping None.
fn put_opt<T: Serialize>(req: &mut serde_json::Value, key: &str, v: Option<T>) {
    if let Some(v) = v
        && let Ok(val) = serde_json::to_value(v)
    {
        req[key] = val;
    }
}
