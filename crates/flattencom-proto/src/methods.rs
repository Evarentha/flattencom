/*
 * flattencom - flattencom Flattencom-Proto Src Methods
 *
 * Defines JSON-RPC methods, parameter types, responses, notifications and frame projections.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

//! RPC method and event type catalog.
//!
//! UTF-8 NDJSON transport: one JSON-RPC 2.0 message per line, maximum 16 MiB.
//! Authentication requires hello with the token; success returns welcome.
//! Shared golden fixtures verify Rust/C++ compatibility; see docs/PROTOCOL.md.
//!
//! ## Method catalog
//!
//! | Category | Methods |
//! |------|------|
//! | Connection | hello -> welcome |
//! | Ports | list_ports, get_port_info |
//! | Sessions | open_session, close_session, list_sessions, configure_session |
//! | IO | send, send_file, read_frames, get_log, clear_buffer, flush_rx, flush |
//! | Decode/filter/triggers | set_decoder, get_decoder, set_filter, set_triggers, get_triggers, get_trigger_fires |
//! | Signals | set_signals, get_signals, send_break |
//! | Capture | export_log, replay_log |
//! | Subscriptions | subscribe, unsubscribe |
//! | Service | daemon_info, shutdown |

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use flattencom_core::config::SerialConfig;
use flattencom_core::decode::DecoderSpec;
use flattencom_core::discovery::PortInfo;
use flattencom_core::filter::FilterSpec;
use flattencom_core::frame::Frame;
use flattencom_core::session::SessionState;
use flattencom_core::stats::SessionStats;
use flattencom_core::trigger::{TriggerFire, TriggerSpec};

/// Protocol version, carried by hello/welcome; increment for incompatible changes.
pub const PROTOCOL_VERSION: u32 = 1;

/// Maximum UTF-8 byte length of the authenticated hello client label.
pub const MAX_CLIENT_LABEL_BYTES: usize = 256;

/// JSON-RPC and application error codes.
pub mod error_code {
    /// JSON parse error.
    pub const PARSE: i64 = -32700;
    /// Invalid request.
    pub const INVALID_REQUEST: i64 = -32600;
    /// Method not found.
    pub const METHOD_NOT_FOUND: i64 = -32601;
    /// Invalid parameters.
    pub const INVALID_PARAMS: i64 = -32602;
    /// Internal error.
    pub const INTERNAL: i64 = -32603;
    /// Authentication failed due to token mismatch.
    pub const UNAUTHORIZED: i64 = -32001;
    /// Fallback for unlisted core errors; data retains the original category.
    pub const APP_BASE: i64 = 100;
}

// ---------------------------------------------------------------------------
// Wire format
// ---------------------------------------------------------------------------

/// JSON-RPC request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RpcRequest {
    /// Always `"2.0"`.
    pub jsonrpc: String,
    /// Caller-selected request ID used to correlate responses.
    pub id: u64,
    /// Method name.
    pub method: String,
    /// Parameter object, optionally empty.
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub params: serde_json::Value,
}

/// RPC error object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RpcError {
    /// Error code; see error_code and docs/PROTOCOL.md.
    pub code: i64,
    /// Readable localized message with recovery guidance.
    pub message: String,
    /// Structured details including the core error category.
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub data: serde_json::Value,
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}", self.code, self.message)
    }
}

impl std::error::Error for RpcError {}

/// JSON-RPC response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RpcResponse {
    /// Always `"2.0"`.
    pub jsonrpc: String,
    /// ID of the corresponding request.
    pub id: u64,
    /// Result, mutually exclusive with error.
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub result: serde_json::Value,
    /// Error, mutually exclusive with result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

/// Server-to-client JSON-RPC notification.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RpcNotification {
    /// Always `"2.0"`.
    pub jsonrpc: String,
    /// Event name; see Event.
    pub method: String,
    /// Event parameters.
    #[serde(default)]
    pub params: serde_json::Value,
}

// ---------------------------------------------------------------------------
// hello / welcome
// ---------------------------------------------------------------------------

/// hello: authenticated connection handshake.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct HelloParams {
    /// Client label used for operation attribution; at most 256 UTF-8 bytes.
    pub client: String,
    /// Client version.
    #[serde(default)]
    pub version: String,
    /// Protocol version; must match PROTOCOL_VERSION.
    pub proto: u32,
    /// Authentication token from daemon.token.
    #[serde(default)]
    pub token: Option<String>,
}

/// welcome: handshake result and capability negotiation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct WelcomeResult {
    /// Daemon identifier.
    pub daemon: String,
    /// Daemon version.
    pub version: String,
    /// Protocol version.
    pub proto: u32,
    /// Capabilities available for clients to use.
    pub capabilities: Vec<String>,
}

// ---------------------------------------------------------------------------
// Ports
// ---------------------------------------------------------------------------

/// Port metadata and ownership state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PortEntry {
    /// Core PortInfo metadata.
    #[serde(flatten)]
    pub info: PortInfo,
    /// Whether a session in this service owns the port.
    pub busy: bool,
    /// IDs of sessions using this port.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sessions: Vec<String>,
}

/// Parameters for list_ports.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
pub struct ListPortsParams {}

/// Result of list_ports.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ListPortsResult {
    /// Port list sorted by path.
    pub ports: Vec<PortEntry>,
}

/// Parameters for get_port_info.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct GetPortInfoParams {
    /// Port path.
    pub path: String,
}

/// Result of get_port_info.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct GetPortInfoResult {
    /// Port details; missing ports return port_not_found.
    pub port: PortEntry,
}

// ---------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------

/// Session summary returned by session methods and events.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SessionSummary {
    /// Session ID.
    pub session_id: String,
    /// User-defined label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Port path.
    pub path: String,
    /// Owning client label.
    pub owner: String,
    /// Complete configuration.
    pub config: SerialConfig,
    /// Current state.
    pub state: SessionState,
    /// Statistics snapshot.
    pub stats: SessionStats,
}

/// open_session parameters; omitted serial settings default to 115200 8N1.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct OpenSessionParams {
    /// Physical port path (/dev/ttyUSB0 or COM3) or virtual URI (virtual://echo).
    pub path: String,
    /// Baud rate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baud: Option<u32>,
    /// Data bits: five, six, seven or eight.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_bits: Option<String>,
    /// Parity: none, odd, even, mark or space.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parity: Option<String>,
    /// Stop bits: one, one_point_five or two. one_point_five requires five data bits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_bits: Option<String>,
    /// Flow control: none, software or hardware.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flow_control: Option<String>,
    /// Read timeout in milliseconds, default 10, maximum 1000.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_timeout_ms: Option<u64>,
    /// Open exclusively.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclusive: Option<bool>,
    /// Session label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Record to one .log file containing traffic and annotations. Legacy .txt and JSONL paths remain supported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record_to: Option<String>,
    /// Create a new raw receive-stream file on connection (TX bytes are not included).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record_rx_to: Option<String>,
    /// Older text segments to retain. Omit to keep all session history.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record_keep_segments: Option<u32>,
    /// Maximum retained frame count.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub buffer_max_frames: Option<usize>,
    /// Maximum retained bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub buffer_max_bytes: Option<u64>,
}

/// Result of open_session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct OpenSessionResult {
    /// Session summary.
    pub session: SessionSummary,
    /// True when an existing connected/reconnecting session was reused.
    /// Its owner, parameters and recording paths are preserved.
    #[serde(default)]
    pub reused: bool,
}

/// Parameters for close_session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CloseSessionParams {
    /// Session ID.
    pub session_id: String,
    /// Force closing a session owned by another connection.
    #[serde(default)]
    pub force: bool,
}

/// Result of close_session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CloseSessionResult {
    /// Final statistics after worker drain and capture finalization.
    pub stats: SessionStats,
}

/// Result of list_sessions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ListSessionsResult {
    /// Session list.
    pub sessions: Vec<SessionSummary>,
}

/// configure_session parameters; update only supplied fields without reopening.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ConfigureSessionParams {
    /// Session ID.
    pub session_id: String,
    /// Baud rate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baud: Option<u32>,
    /// Data bits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_bits: Option<String>,
    /// Parity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parity: Option<String>,
    /// Stop bits: one, one_point_five or two. one_point_five requires five data bits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_bits: Option<String>,
    /// Flow control.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flow_control: Option<String>,
    /// Read timeout in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_timeout_ms: Option<u64>,
    /// Label override; explicit null clears it.
    #[serde(
        default,
        deserialize_with = "flattencom_core::config::nullable_label",
        skip_serializing_if = "Option::is_none"
    )]
    pub label: Option<Option<String>>,
}

/// Result of configure_session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ConfigureSessionResult {
    /// Complete configuration after the change.
    pub config: SerialConfig,
}

// ---------------------------------------------------------------------------
// Transmission and reception
// ---------------------------------------------------------------------------

/// Line-ending style for the send newline parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "lowercase")]
pub enum NewlineStyle {
    /// Append CRLF, commonly required by serial devices.
    #[default]
    Crlf,
    /// Append LF.
    Lf,
    /// Append CR for legacy devices.
    Cr,
    /// Send unchanged without a line ending.
    None,
}

impl NewlineStyle {
    /// Bytes to append.
    #[must_use]
    pub fn suffix(self) -> &'static [u8] {
        match self {
            Self::Crlf => b"\r\n",
            Self::Lf => b"\n",
            Self::Cr => b"\r",
            Self::None => b"",
        }
    }
}

/// Parameters for `send`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SendParams {
    /// Session ID.
    pub session_id: String,
    /// Text payload, mutually exclusive with hex.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
    /// Hexadecimal payload allowing whitespace; mutually exclusive with data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hex: Option<String>,
    /// Appended line ending, default crlf.
    #[serde(default)]
    pub newline: NewlineStyle,
}

/// Result of send.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SendResult {
    /// Transmitted bytes.
    pub bytes_sent: u64,
    /// Transmitted frame sequence.
    pub seq: u64,
}

/// Parameters for `send_file`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SendFileParams {
    /// Session ID.
    pub session_id: String,
    /// File path.
    pub path: String,
    /// Chunk size in bytes (default 1024, capped at 4096).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chunk_size: Option<usize>,
    /// Millisecond delay between chunks, default zero, for paced transfer to slower devices.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pacing_ms: Option<u64>,
}

/// Result of send_file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SendFileResult {
    /// Transmitted bytes.
    pub bytes_sent: u64,
    /// Transmitted chunk count.
    pub frames: u64,
}

/// Frame output format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "lowercase")]
pub enum FrameFormat {
    /// Base fields only: sequence, direction, timestamps and length.
    Raw,
    /// Base fields and hex bytes.
    Hex,
    /// Base fields, hex bytes and lossy text.
    Text,
    /// Complete output: hex, text, decoded results and fields (default).
    #[default]
    Decoded,
}

/// Parameters for `read_frames`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ReadFramesParams {
    /// Session ID.
    pub session_id: String,
    /// Inclusive next-unread sequence; None starts at the buffer head.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since_seq: Option<u64>,
    /// Restrict to the last N milliseconds, intersecting any since_seq bound.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_ms: Option<u64>,
    /// Soft output-size limit, default 256 KiB.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_bytes: Option<u64>,
    /// Select a bounded tail window while respecting any since_seq lower bound.
    #[serde(default)]
    pub tail: bool,
    /// Output format, default decoded.
    #[serde(default)]
    pub format: FrameFormat,
}

/// Output frame projected to the requested format.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FrameOut {
    /// Sequence number.
    pub seq: u64,
    /// Direction.
    pub dir: String,
    /// Client/trigger that initiated TX. Never inferred from device output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Wall-clock UNIX microseconds.
    pub t_us: i64,
    /// Monotonic microseconds since session start.
    pub mono_us: u64,
    /// Byte count.
    pub len: usize,
    /// Hexadecimal data when requested.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hex: Option<String>,
    /// Lossy UTF-8 text when requested.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Decoded text when decoded format is requested.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decoded_text: Option<String>,
    /// Decode level: info, warn or error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decoded_level: Option<String>,
    /// Decoded fields.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub decoded_fields: BTreeMap<String, String>,
}

/// Frame page shared by read_frames, get_log and frame events.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FramesPageOut {
    /// Frames after read filtering.
    pub frames: Vec<FrameOut>,
    /// Next inclusive since_seq cursor.
    pub next_seq: u64,
    /// Cumulative evicted frame count.
    pub dropped_rx: u64,
    /// Whether all currently available frames have been read.
    pub up_to_date: bool,
    /// Earliest sequence in the buffer.
    pub first_seq: Option<u64>,
    /// Latest sequence in the buffer.
    pub last_seq: Option<u64>,
}

/// get_log parameters for historical interval reads.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct GetLogParams {
    /// Session ID.
    pub session_id: String,
    /// Inclusive start sequence; None starts at the beginning.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_seq: Option<u64>,
    /// Exclusive end sequence; None reads to the end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_seq: Option<u64>,
    /// Output format.
    #[serde(default)]
    pub format: FrameFormat,
}

/// Parameters for clear_buffer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SessionIdParams {
    /// Session ID.
    pub session_id: String,
}

/// Result of clear_buffer or flush_rx.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ClearedResult {
    /// Cleared frame count.
    pub cleared: u64,
}

// ---------------------------------------------------------------------------
// Decoding, filtering and triggers
// ---------------------------------------------------------------------------

/// set_decoder parameters; spec=null disables decoding.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SetDecoderParams {
    /// Session ID.
    pub session_id: String,
    /// Decoder specification; null disables it.
    pub spec: Option<DecoderSpec>,
}

/// Result of get_decoder.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct GetDecoderResult {
    /// Current decoder specification, or null when disabled.
    pub spec: Option<DecoderSpec>,
}

/// set_filter parameters; filter=null removes filtering.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SetFilterParams {
    /// Session ID.
    pub session_id: String,
    /// Filter specification; null removes it.
    pub filter: Option<FilterSpec>,
}

/// Result of set_filter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SetFilterResult {
    /// Whether a read filter is active.
    pub active: bool,
}

/// set_triggers parameters replacing the entire trigger list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SetTriggersParams {
    /// Session ID.
    pub session_id: String,
    /// Trigger specifications.
    pub triggers: Vec<TriggerSpec>,
}

/// Result of set_triggers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SetTriggersResult {
    /// Installed trigger count.
    pub count: usize,
}

/// Result of get_triggers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct GetTriggersResult {
    /// Trigger specifications.
    pub triggers: Vec<TriggerSpec>,
}

/// Trigger-history result; reading does not clear retained events.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct GetTriggerFiresResult {
    /// Trigger history.
    pub fires: Vec<TriggerFire>,
}

/// Catalog of decoder names accepted by set_decoder.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DecoderCatalogResult {
    /// Decoder IDs and descriptions.
    pub decoders: BTreeMap<String, String>,
}

// ---------------------------------------------------------------------------
// Signals
// ---------------------------------------------------------------------------

/// Parameters for `set_signals`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SetSignalsParams {
    /// Session ID.
    pub session_id: String,
    /// DTR state; None leaves it unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dtr: Option<bool>,
    /// RTS state; None leaves it unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rts: Option<bool>,
}

/// Result of get_signals or set_signals.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SignalsResult {
    /// Control line states.
    pub pins: flattencom_core::transport::PinStates,
}

/// Parameters for `send_break`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SendBreakParams {
    /// Session ID.
    pub session_id: String,
    /// BREAK duration in milliseconds (default 100).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

/// Result of send_break or flush.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct OkResult {
    /// Whether the operation succeeded.
    pub ok: bool,
}

// ---------------------------------------------------------------------------
// Statistics and recording
// ---------------------------------------------------------------------------

/// Result of get_stats.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct GetStatsResult {
    /// Statistics snapshot.
    pub stats: SessionStats,
}

/// Export format parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "lowercase")]
pub enum ExportFormatParam {
    /// Human-readable text.
    #[default]
    Txt,
    /// Offset hexadecimal dump.
    Hex,
    /// CSV。
    Csv,
    /// JSONL using the capture schema.
    Jsonl,
    /// PCAP LINKTYPE_USER0: direction byte followed by serial payload.
    Pcap,
}

/// Parameters for `export_log`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ExportLogParams {
    /// Session ID.
    pub session_id: String,
    /// Format.
    #[serde(default)]
    pub format: ExportFormatParam,
    /// Export path; null generates a name in the daemon data directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Export all retained frames when true; otherwise apply the view filter.
    #[serde(default)]
    pub all: bool,
}

/// Result of export_log.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ExportLogResult {
    /// Exported file path.
    pub path: String,
    /// Frame count.
    pub frames: u64,
    /// File size in bytes.
    pub bytes: u64,
}

/// Replay parameters targeting an already-open session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ReplayLogParams {
    /// Target session identifier.
    pub session_id: String,
    /// Recording file path.
    pub path: String,
    /// Replay speed multiplier, default 1.0; 10 means ten times faster.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed: Option<f64>,
}

/// Result of replay_log.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ReplayLogResult {
    /// Replayed frames.
    pub frames_sent: u64,
    /// Replayed bytes.
    pub bytes_sent: u64,
    /// Skipped invalid line count.
    pub skipped: u64,
}

// ---------------------------------------------------------------------------
// Subscriptions
// ---------------------------------------------------------------------------

/// Subscribable event types.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SubKind {
    /// Batched frame stream on a 32 ms cadence.
    Frames,
    /// Statistics on a one-second cadence.
    Stats,
    /// State changes: opened, reconnecting, failed or closed.
    State,
    /// Trigger matches.
    TriggerFires,
}

/// Parameters for subscribe.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SubscribeParams {
    /// Session ID.
    pub session_id: String,
    /// Event kinds; null or empty means all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kinds: Option<Vec<SubKind>>,
}

/// Result of subscribe or unsubscribe.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SubscribeResult {
    /// Whether the operation succeeded.
    pub ok: bool,
}

// ---------------------------------------------------------------------------
// Daemon
// ---------------------------------------------------------------------------

/// Result of daemon_info.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DaemonInfoResult {
    /// Service identifier (flattencomd).
    pub daemon: String,
    /// Version.
    pub version: String,
    /// Protocol version.
    pub proto: u32,
    /// Uptime in milliseconds.
    pub uptime_ms: u64,
    /// Active session count.
    pub sessions: usize,
    /// Listening socket path.
    pub socket: String,
    /// Supported capabilities.
    pub capabilities: Vec<String>,
}

/// Result of shutdown.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ShutdownResult {
    /// Whether shutdown has started.
    pub ok: bool,
}

// ---------------------------------------------------------------------------
// Events: server-to-client notifications
// ---------------------------------------------------------------------------

/// Parameters for ports_changed events.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PortsChangedEvent {
    /// Added ports.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub added: Vec<PortInfo>,
    /// Removed port paths.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub removed: Vec<String>,
}

/// Parameters for session_state events.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SessionStateEvent {
    /// Session ID.
    pub session_id: String,
    /// New state: connected, reconnecting, closed or failed.
    pub state: SessionState,
}

/// Parameters for stats events.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StatsEvent {
    /// Session ID.
    pub session_id: String,
    /// Statistics snapshot.
    pub stats: SessionStats,
}

/// Parameters for trigger_fired events.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TriggerFiredEvent {
    /// Session ID.
    pub session_id: String,
    /// Trigger history.
    pub fires: Vec<TriggerFire>,
}

/// Parameters for buffer_overflow events.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct BufferOverflowEvent {
    /// Session ID.
    pub session_id: String,
    /// Frames evicted in this overflow event.
    pub dropped: u64,
}

/// Parsed client-side server events; see RpcNotification for their wire representation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum Event {
    /// Port additions/removals from hotplug discovery.
    PortsChanged(PortsChangedEvent),
    /// Session state changed.
    SessionState(SessionStateEvent),
    /// Frame batch.
    Frames {
        /// Session ID.
        session_id: String,
        /// Frames in decoded format.
        #[serde(flatten)]
        page: FramesPageOut,
    },
    /// Statistics update.
    Stats(StatsEvent),
    /// Trigger matches.
    TriggerFired(TriggerFiredEvent),
    /// Buffer overflow notification with visible eviction count.
    BufferOverflow(BufferOverflowEvent),
}

/// Project a core Frame into FrameOut according to the requested format.
#[must_use]
pub fn frame_out(frame: &Frame, format: FrameFormat) -> FrameOut {
    FrameOut {
        seq: frame.seq,
        dir: frame.dir.as_str().to_owned(),
        source: frame.source.clone(),
        t_us: frame.t_us,
        mono_us: frame.mono_us,
        len: frame.len(),
        hex: matches!(
            format,
            FrameFormat::Hex | FrameFormat::Text | FrameFormat::Decoded
        )
        .then(|| frame.hex_string(true)),
        text: matches!(format, FrameFormat::Text | FrameFormat::Decoded)
            .then(|| frame.text_lossy()),
        decoded_text: (format == FrameFormat::Decoded)
            .then(|| frame.decoded.as_ref().map(|d| d.text.clone()))
            .flatten(),
        decoded_level: (format == FrameFormat::Decoded)
            .then(|| {
                frame.decoded.as_ref().map(|d| {
                    match d.level {
                        flattencom_core::frame::DecodeLevel::Info => "info",
                        flattencom_core::frame::DecodeLevel::Warn => "warn",
                        flattencom_core::frame::DecodeLevel::Error => "error",
                    }
                    .to_owned()
                })
            })
            .flatten(),
        decoded_fields: (format == FrameFormat::Decoded)
            .then(|| {
                frame.decoded.as_ref().map(|d| {
                    d.fields
                        .iter()
                        .map(|f| (f.name.clone(), f.value.clone()))
                        .collect()
                })
            })
            .flatten()
            .unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 行结束符() {
        assert_eq!(NewlineStyle::Crlf.suffix(), b"\r\n");
        assert_eq!(NewlineStyle::Lf.suffix(), b"\n");
        assert_eq!(NewlineStyle::None.suffix(), b"");
        let v = serde_json::to_value(NewlineStyle::Crlf).unwrap();
        assert_eq!(v, serde_json::json!("crlf"));
    }

    #[test]
    fn 帧投影裁剪() {
        let mut f = Frame::new(
            3,
            flattencom_core::frame::Direction::Rx,
            b"AT\r\n".to_vec(),
            1,
            2,
        );
        f = f.with_decoded(Some(flattencom_core::frame::DecodedInfo::info(
            "ascii_lines",
            "AT␍␊".into(),
            vec![flattencom_core::frame::DecodeField {
                name: "len".into(),
                value: "4".into(),
            }],
        )));
        let raw = frame_out(&f, FrameFormat::Raw);
        assert_eq!(raw.hex, None);
        assert_eq!(raw.text, None);
        assert_eq!(raw.len, 4);
        let hex = frame_out(&f, FrameFormat::Hex);
        assert_eq!(hex.hex.as_deref(), Some("41 54 0D 0A"));
        assert_eq!(hex.text, None);
        let dec = frame_out(&f, FrameFormat::Decoded);
        assert_eq!(dec.text.as_deref(), Some("AT\r\n"));
        assert_eq!(dec.decoded_text.as_deref(), Some("AT␍␊"));
        assert_eq!(dec.decoded_fields.get("len").map(String::as_str), Some("4"));
    }

    #[test]
    fn 事件标记序列化() {
        let e = Event::SessionState(SessionStateEvent {
            session_id: "abc".into(),
            state: SessionState::Connected,
        });
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["method"], "session_state");
        assert_eq!(v["session_id"], "abc");
    }

    #[test]
    fn 请求线格式() {
        let r = RpcRequest {
            jsonrpc: "2.0".into(),
            id: 7,
            method: "list_ports".into(),
            params: serde_json::Value::Null,
        };
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains(r#""method":"list_ports""#));
    }
}
