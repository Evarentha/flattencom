/*
 * flattencom - flattencom Flattencom-Proto Src Client
 *
 * Connects and authenticates clients, correlates RPC replies and starts the service when needed.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

//! Tokio daemon client: connect, authenticate, correlate replies, publish events and start the service.
//!
//! ## Architecture
//!
//! ```text
//! call() --> request-ID table --> writer task --> socket
//!                              │
//! reader task --> parse line --> ID-matched oneshot response
//!                                             --> method notification --> broadcast<Event>
//!                event consumers in GUI/CLI/MCP each own a receiver
//! ```
//!
//! ## Automatic startup
//!
//! connect_or_spawn resolves flattencomd through FLATTENCOMD_BIN,
//! its sibling directory and PATH, launches detached with redirected logs, then retries with backoff.
//! Set FLATTENCOM_NO_AUTOSPAWN=1 for containers or restricted environments.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{broadcast, mpsc, oneshot};

use crate::methods::{
    Event, HelloParams, PROTOCOL_VERSION, RpcError, RpcNotification, RpcResponse, WelcomeResult,
    error_code,
};
use crate::socket;

/// Shared stream abstraction for Unix sockets and Windows Named Pipes.
pub trait AsyncStream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T> AsyncStream for T where T: AsyncRead + AsyncWrite + Unpin + Send {}

/// Boxed platform stream.
type BoxStream = Box<dyn AsyncStream>;
/// Split stream read half.
type BoxReadHalf = tokio::io::ReadHalf<BoxStream>;
/// Split stream write half.
type BoxWriteHalf = tokio::io::WriteHalf<BoxStream>;

/// Default request timeout.
pub const DEFAULT_CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// Maximum NDJSON line size, matching the daemon.
pub const MAX_LINE_BYTES: usize = 16 * 1024 * 1024;

/// Explicit connection configuration; environment variables supply convenient defaults.
///
/// Tests, containers and multiple-daemon scenarios should set socket/state_dir explicitly,
/// avoiding process-global environment mutation and set_var in tests.
#[derive(Debug, Clone)]
pub struct ConnectConfig {
    /// Client label used for session ownership and logs.
    pub label: String,
    /// Daemon endpoint; None uses the socket_path environment default.
    pub socket: Option<std::path::PathBuf>,
    /// State directory containing the token; None uses the state_dir default.
    pub state_dir: Option<std::path::PathBuf>,
    /// Connect/handshake timeout.
    pub timeout: Duration,
}

impl ConnectConfig {
    /// Construct configuration from environment defaults.
    #[must_use]
    pub fn new(label: impl Into<String>, timeout: Duration) -> Self {
        Self {
            label: label.into(),
            socket: None,
            state_dir: None,
            timeout,
        }
    }

    /// Set the socket path.
    #[must_use]
    pub fn with_socket(mut self, socket: impl Into<std::path::PathBuf>) -> Self {
        self.socket = Some(socket.into());
        self
    }

    /// Set the token/state directory.
    #[must_use]
    pub fn with_state_dir(mut self, dir: impl Into<std::path::PathBuf>) -> Self {
        self.state_dir = Some(dir.into());
        self
    }
}

/// Client error.
#[derive(Debug, Clone, PartialEq)]
pub enum ClientError {
    /// Connection failed because the service is unavailable or the endpoint is inaccessible.
    Connect(String),
    /// RPC method, parameter or device error including recovery guidance.
    Rpc(RpcError),
    /// Invalid protocol message.
    Protocol(String),
    /// Client is closed.
    Closed,
}
impl std::error::Error for ClientError {}
impl From<RpcError> for ClientError {
    fn from(error: RpcError) -> Self {
        Self::Rpc(error)
    }
}
impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Connect(error)=>f.write_str(&flattencom_core::tr!("Cannot connect to background service: {0}. Run flattencom daemon start to start it.",error)),
            Self::Rpc(error)=>f.write_str(&error.message),
            Self::Protocol(error)=>f.write_str(&flattencom_core::tr!("Protocol error: {0}",error)),
            Self::Closed=>f.write_str(flattencom_core::i18n::text("Client closed")),
        }
    }
}

impl ClientError {
    /// Error code, preserving RPC codes.
    #[must_use]
    pub fn code(&self) -> i64 {
        match self {
            Self::Connect(_) => error_code::APP_BASE - 1,
            Self::Rpc(e) => e.code,
            Self::Protocol(_) => error_code::INVALID_REQUEST,
            Self::Closed => error_code::APP_BASE - 2,
        }
    }
}

/// Response channel type.
type Reply = oneshot::Sender<Result<serde_json::Value, RpcError>>;

struct PendingCall {
    id: u64,
    table: Arc<Mutex<HashMap<u64, Reply>>>,
}

/// First terminal transport observation, retained for diagnostics.
#[derive(Debug, Clone, Serialize)]
pub struct DisconnectInfo {
    /// UNIX milliseconds when the failure was observed.
    pub at_ms: u64,
    /// Transport phase and error; never includes request payloads or tokens.
    pub reason: String,
}

#[derive(Default)]
struct ConnectionHealth {
    failure: Mutex<Option<DisconnectInfo>>,
    closed: tokio::sync::Notify,
}
impl ConnectionHealth {
    fn stop(&self, reason: String, pending: &Mutex<HashMap<u64, Reply>>) {
        let mut failure = self.failure.lock().expect("connection health");
        if failure.is_some() {
            return;
        }
        let at_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
        tracing::warn!(at_ms, %reason, "Background RPC connection closed");
        *failure = Some(DisconnectInfo {
            at_ms,
            reason: reason.clone(),
        });
        for (_, reply) in pending.lock().expect("pending calls").drain() {
            let _ = reply.send(Err(RpcError {
                code: error_code::APP_BASE - 2,
                message: format!("Background service disconnected: {reason}"),
                data: serde_json::Value::Null,
            }));
        }
        self.closed.notify_waiters();
    }
}
impl Drop for PendingCall {
    fn drop(&mut self) {
        if let Ok(mut table) = self.table.lock() {
            table.remove(&self.id);
        }
    }
}

/// Cloneable client sharing one daemon connection.
#[derive(Clone)]
pub struct DaemonClient {
    to_writer: mpsc::Sender<String>,
    pending: Arc<Mutex<HashMap<u64, Reply>>>,
    events: broadcast::Sender<Arc<Event>>,
    next_id: Arc<AtomicU64>,
    label: Arc<str>,
    health: Arc<ConnectionHealth>,
}

impl DaemonClient {
    /// Connect and authenticate using the default flattencom-client label.
    pub async fn connect(timeout: Duration) -> Result<Self, ClientError> {
        Self::connect_with_label("flattencom-client", timeout).await
    }

    /// Connect and authenticate using a custom label and environment endpoint.
    pub async fn connect_with_label(label: &str, timeout: Duration) -> Result<Self, ClientError> {
        Self::connect_with(ConnectConfig::new(label, timeout)).await
    }

    /// Connect and authenticate with explicit endpoint, state directory, label and timeout.
    pub async fn connect_with(config: ConnectConfig) -> Result<Self, ClientError> {
        let path = socket::socket_path_opt(config.socket.as_deref());
        let stream = connect_stream(&path, config.timeout)
            .await
            .map_err(|e| ClientError::Connect(format!("{}:{e}", path.display())))?;
        let (read_half, write_half) = tokio::io::split(stream);
        let health = Arc::new(ConnectionHealth::default());
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let client = Self {
            to_writer: spawn_writer(write_half, health.clone(), pending.clone()),
            pending,
            events: broadcast::channel(1024).0,
            next_id: Arc::new(AtomicU64::new(1)),
            label: config.label.clone().into(),
            health,
        };
        spawn_reader(
            read_half,
            Arc::clone(&client.pending),
            client.events.clone(),
            client.health.clone(),
        );
        // Handshake
        let hello = HelloParams {
            client: config.label,
            version: env!("CARGO_PKG_VERSION").to_owned(),
            proto: PROTOCOL_VERSION,
            token: read_token(config.state_dir.as_deref()),
        };
        let welcome: WelcomeResult = match client.call_typed("hello", &hello, config.timeout).await
        {
            Ok(welcome) => welcome,
            Err(e) => {
                client.close();
                return Err(e);
            }
        };
        if welcome.proto != PROTOCOL_VERSION {
            client.close();
            return Err(ClientError::Protocol(flattencom_core::tr!(
                "Protocol version mismatch: service {} / client {}",
                welcome.proto,
                PROTOCOL_VERSION
            )));
        }
        Ok(client)
    }

    /// Client label for logs and session ownership.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Subscribe to events with an independent receiver per consumer.
    #[must_use]
    pub fn subscribe_events(&self) -> broadcast::Receiver<Arc<Event>> {
        self.events.subscribe()
    }

    /// Whether the transport is still alive.
    #[must_use]
    pub fn is_alive(&self) -> bool {
        !self.to_writer.is_closed() && self.disconnect_info().is_none()
    }

    /// Read the terminal connection observation, if disconnected.
    pub fn disconnect_info(&self) -> Option<DisconnectInfo> {
        self.health
            .failure
            .lock()
            .expect("connection health")
            .clone()
    }

    /// Close idempotently; subsequent call returns ClientError::Closed.
    pub fn close(&self) {
        self.health
            .stop("Client closed locally".into(), &self.pending);
    }

    /// Untyped call with the default timeout.
    pub async fn call(
        &self,
        method: &str,
        params: impl Serialize,
    ) -> Result<serde_json::Value, ClientError> {
        self.call_typed::<_, serde_json::Value>(method, &params, DEFAULT_CALL_TIMEOUT)
            .await
    }

    /// Typed call with an explicit timeout.
    pub async fn call_typed<P: Serialize, R: DeserializeOwned>(
        &self,
        method: &str,
        params: &P,
        timeout: Duration,
    ) -> Result<R, ClientError> {
        let mut params =
            serde_json::to_value(params).map_err(|e| ClientError::Protocol(e.to_string()))?;
        if let Some(object) = params.as_object_mut() {
            object
                .entry("_language")
                .or_insert_with(|| serde_json::json!(flattencom_core::i18n::language().code()));
        }
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        let _cleanup = PendingCall {
            id,
            table: self.pending.clone(),
        };
        let line = serde_json::to_string(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }))
        .map_err(|e| ClientError::Protocol(e.to_string()))?;
        if line.len() >= MAX_LINE_BYTES {
            return Err(ClientError::Protocol(
                "request exceeds message limit".into(),
            ));
        }
        if !self.is_alive() {
            self.pending
                .lock()
                .expect("Response table lock poisoned")
                .remove(&id);
            return Err(ClientError::Closed);
        }
        self.pending
            .lock()
            .expect("Response table lock poisoned")
            .insert(id, tx);
        if !self.is_alive() {
            return Err(ClientError::Closed);
        }
        self.to_writer
            .try_send(line)
            .map_err(|e| ClientError::Connect(format!("RPC output unavailable: {e}")))?;
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(result)) => {
                let value = result.map_err(ClientError::Rpc)?;
                serde_json::from_value(value).map_err(|e| {
                    ClientError::Protocol(flattencom_core::tr!(
                        "Failed to decode {method} result: {e}",
                        e = e,
                        method = method
                    ))
                })
            }
            Ok(Err(_)) => {
                // Reader/writer ended before the reply; remove pending state.
                self.pending
                    .lock()
                    .expect("Response table lock poisoned")
                    .remove(&id);
                Err(ClientError::Closed)
            }
            Err(_) => {
                self.pending
                    .lock()
                    .expect("Response table lock poisoned")
                    .remove(&id);
                Err(ClientError::Rpc(RpcError {
                    code: error_code::APP_BASE + 6,
                    message: flattencom_core::tr!(
                        "{method} request timed out ({timeout:?})",
                        method = method,
                        timeout = timeout
                    ),
                    data: serde_json::Value::Null,
                }))
            }
        }
    }
}

/// Read the token; missing files produce None for the daemon to reject.
fn read_token(state_override: Option<&std::path::Path>) -> Option<String> {
    std::fs::read_to_string(socket::token_path_opt(state_override))
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
}

// ---------------------------------------------------------------------------
// Platform connection
// ---------------------------------------------------------------------------

// The Windows branch opens the named pipe synchronously, so the await only
// exists on Unix; keeping one signature avoids duplicating the call site.
#[cfg_attr(windows, allow(clippy::unused_async))]
async fn connect_stream(path: &std::path::Path, timeout: Duration) -> std::io::Result<BoxStream> {
    #[cfg(unix)]
    {
        let s = tokio::time::timeout(timeout, tokio::net::UnixStream::connect(path))
            .await
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "connect timeout"))??;
        Ok(Box::new(s) as BoxStream)
    }
    #[cfg(windows)]
    {
        let _ = timeout;
        use tokio::net::windows::named_pipe::ClientOptions;
        let pipe = ClientOptions::new().open(path.as_os_str())?;
        Ok(Box::new(pipe) as BoxStream)
    }
}

fn spawn_writer(
    mut write_half: BoxWriteHalf,
    health: Arc<ConnectionHealth>,
    pending: Arc<Mutex<HashMap<u64, Reply>>>,
) -> mpsc::Sender<String> {
    let (tx, mut rx) = mpsc::channel::<String>(64);
    tokio::spawn(async move {
        loop {
            let closed = health.closed.notified();
            tokio::pin!(closed);
            closed.as_mut().enable();
            if health.failure.lock().expect("connection health").is_some() {
                break;
            }
            let Some(mut line) =
                (tokio::select! { () = &mut closed => break, line = rx.recv() => line })
            else {
                break;
            };
            if line.is_empty() {
                break; // Graceful shutdown
            }
            line.push('\n');
            let result = tokio::select! {
                () = &mut closed => break,
                result = async { write_half.write_all(line.as_bytes()).await?; write_half.flush().await } => result,
            };
            if let Err(error) = result {
                health.stop(format!("socket write: {error}"), &pending);
                break;
            }
        }
        health.stop("RPC writer stopped".into(), &pending);
        // Shut down the write half to notify the peer.
        let _ = write_half.shutdown().await;
    });
    tx
}

fn spawn_reader(
    read_half: BoxReadHalf,
    pending: Arc<Mutex<HashMap<u64, Reply>>>,
    events: broadcast::Sender<Arc<Event>>,
    health: Arc<ConnectionHealth>,
) {
    tokio::spawn(async move {
        let mut reader = BufReader::with_capacity(256 * 1024, read_half);
        loop {
            let closed = health.closed.notified();
            tokio::pin!(closed);
            closed.as_mut().enable();
            if health.failure.lock().expect("connection health").is_some() {
                break;
            }
            let result = tokio::select! {
                () = &mut closed => break,
                result = crate::framing::read_line(&mut reader, MAX_LINE_BYTES) => result,
            };
            match result {
                Ok(None) => {
                    health.stop("socket read: EOF".into(), &pending);
                    break;
                }
                Err(error) => {
                    health.stop(format!("socket read/framing: {error}"), &pending);
                    break;
                }
                Ok(Some(line)) => {
                    let trimmed = line.trim_end_matches(['\r', '\n']);
                    if trimmed.is_empty() {
                        continue;
                    }
                    // Response or notification
                    let v: serde_json::Value = match serde_json::from_str(trimmed) {
                        Ok(v) => v,
                        Err(error) => {
                            health.stop(format!("RPC JSON parse: {error}"), &pending);
                            break;
                        }
                    };
                    if v.get("id").is_some() {
                        // Malformed replies are terminal protocol failures. Silently
                        // ignoring them strands pending calls until their timeout.
                        let valid = v["jsonrpc"] == "2.0"
                            && (v.get("result").is_some() ^ v.get("error").is_some())
                            && v.get("error").is_none_or(serde_json::Value::is_object);
                        let response = serde_json::from_value::<RpcResponse>(v);
                        let resp = match response {
                            Ok(resp) if valid => resp,
                            _ => {
                                health.stop("Malformed RPC response".into(), &pending);
                                break;
                            }
                        };
                        if let Some(tx) = pending
                            .lock()
                            .expect("Response table lock poisoned")
                            .remove(&resp.id)
                        {
                            let result = if let Some(e) = resp.error {
                                Err(e)
                            } else {
                                Ok(resp.result)
                            };
                            let _ = tx.send(result);
                        }
                    } else if let Ok(note) = serde_json::from_value::<RpcNotification>(v) {
                        // Merge method and params into the method-tagged Event representation.
                        let mut merged = serde_json::Map::new();
                        merged.insert("method".into(), serde_json::Value::String(note.method));
                        if let serde_json::Value::Object(p) = note.params {
                            merged.extend(p);
                        }
                        if let Ok(ev) =
                            serde_json::from_value::<Event>(serde_json::Value::Object(merged))
                        {
                            let _ = events.send(Arc::new(ev));
                        }
                    }
                }
            }
        }
        health.stop("RPC reader stopped".into(), &pending);
    });
}

// ---------------------------------------------------------------------------
// Automatic startup
// ---------------------------------------------------------------------------

/// Connect using environment defaults; start and retry if necessary.
pub async fn connect_or_spawn(timeout_per_try: Duration) -> Result<DaemonClient, ClientError> {
    connect_or_spawn_with(ConnectConfig::new("flattencom-client", timeout_per_try)).await
}

/// Connect with explicit settings; propagate endpoint variables to an automatically started daemon.
pub async fn connect_or_spawn_with(config: ConnectConfig) -> Result<DaemonClient, ClientError> {
    let autospawn = std::env::var("FLATTENCOM_NO_AUTOSPAWN")
        .map_or(true, |v| !matches!(v.trim(), "1" | "true" | "yes"));
    // Try direct connection first; the service is usually already running.
    match DaemonClient::connect_with(config.clone()).await {
        Ok(c) => return Ok(c),
        Err(first_err) => {
            // An authenticated service can reject our token/version/label. Starting
            // another service cannot repair that response and hides its useful error.
            if !autospawn || !matches!(first_err, ClientError::Connect(_)) {
                return Err(first_err);
            }
            tracing::info!("Background service is not running; starting it");
            if let Err(e) = spawn_daemon_detached(&config) {
                tracing::warn!(error = %e, "Automatic service start failed");
                return Err(first_err);
            }
        }
    }
    // Retry with backoff after startup.
    let mut backoff = 100u64;
    for _ in 0..8 {
        tokio::time::sleep(Duration::from_millis(backoff)).await;
        backoff = (backoff * 2).min(800);
        match DaemonClient::connect_with(config.clone()).await {
            Ok(c) => return Ok(c),
            Err(error) if !matches!(error, ClientError::Connect(_)) => return Err(error),
            Err(e) => {
                tracing::debug!(error = %e, "Connection failed after service start; retrying");
            }
        }
    }
    Err(ClientError::Connect(
        flattencom_core::i18n::text(
            "Cannot connect after service start; check the flattencomd executable and logs",
        )
        .into(),
    ))
}

/// Locate and launch the daemon detached, passing configured endpoint variables.
fn spawn_daemon_detached(config: &ConnectConfig) -> Result<(), ClientError> {
    use std::process::{Command, Stdio};
    let exe = locate_daemon().ok_or_else(|| {
        ClientError::Connect(
            flattencom_core::i18n::text(
                "flattencomd not found; set FLATTENCOMD_BIN or add it to PATH",
            )
            .into(),
        )
    })?;
    let log_path = socket::log_dir_opt(config.state_dir.as_deref()).join("daemon-stdout.log");
    if let Some(dir) = log_path.parent() {
        std::fs::create_dir_all(dir).ok();
    }
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|e| {
            ClientError::Connect(flattencom_core::tr!(
                "Failed to open service log: {e}",
                e = e
            ))
        })?;
    let mut cmd = Command::new(&exe);
    cmd.current_dir(std::env::temp_dir())
        .stdin(Stdio::null())
        .stdout(
            log.try_clone()
                .map_err(|e| ClientError::Connect(e.to_string()))?,
        )
        .stderr(log);
    // Explicit settings override the child environment so it listens on the same endpoint.
    if let Some(s) = &config.socket {
        cmd.env("FLATTENCOM_SOCKET", s);
    }
    if let Some(d) = &config.state_dir {
        cmd.env("FLATTENCOM_STATE_DIR", d);
    }
    // Unix uses a new process group; Windows starts detached without a console.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NO_WINDOW);
    }
    let child = cmd.spawn().map_err(|e| {
        ClientError::Connect(flattencom_core::tr!(
            "Failed to start {}: {e}",
            exe.display(),
            e = e
        ))
    })?;
    tracing::info!(pid = child.id(), "Started flattencomd");
    // Detach so this process exiting does not stop the daemon.
    drop(child);
    Ok(())
}

/// Locate flattencomd via FLATTENCOMD_BIN, sibling executable directory, then PATH.
fn locate_daemon() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("FLATTENCOMD_BIN") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        #[cfg(windows)]
        let cand = dir.join("flattencomd.exe");
        #[cfg(not(windows))]
        let cand = dir.join("flattencomd");
        if cand.is_file() {
            return Some(cand);
        }
    }
    // Search PATH.
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        #[cfg(windows)]
        let cand = dir.join("flattencomd.exe");
        #[cfg(not(windows))]
        let cand = dir.join("flattencomd");
        if cand.is_file() {
            return Some(cand);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn malformed_response_terminates_connection_and_fails_pending_call() {
        for line in [
            r#"{"jsonrpc":"1.0","id":1,"result":{}}"#,
            r#"{"jsonrpc":"2.0","id":1}"#,
            r#"{"jsonrpc":"2.0","id":1,"result":{},"error":{"code":1,"message":"bad"}}"#,
            r#"{"jsonrpc":"2.0","id":"wrong","result":{}}"#,
            r#"{"jsonrpc":"2.0","id":1,"error":null}"#,
        ] {
            let (stream, mut peer) = tokio::io::duplex(4096);
            let (read, _write) = tokio::io::split(Box::new(stream) as BoxStream);
            let health = Arc::new(ConnectionHealth::default());
            let (reply, receipt) = oneshot::channel();
            let pending = Arc::new(Mutex::new(HashMap::from([(1, reply)])));
            spawn_reader(
                read,
                pending.clone(),
                broadcast::channel(1).0,
                health.clone(),
            );
            peer.write_all(format!("{line}\n").as_bytes())
                .await
                .unwrap();
            let error = tokio::time::timeout(Duration::from_secs(1), receipt)
                .await
                .unwrap()
                .unwrap()
                .unwrap_err();
            assert!(error.message.contains("Malformed RPC response"));
            assert!(pending.lock().unwrap().is_empty());
            assert!(health.failure.lock().unwrap().is_some());
        }
    }
}
