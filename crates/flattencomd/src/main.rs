/*
 * flattencom - flattencom Flattencomd Src Main
 *
 * Starts the authenticated local service and manages connections, discovery and shutdown.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

//! Shared-session daemon owning serial ports for GUI, CLI and MCP clients.
//!
//! Startup modes:
//! - Manual: flattencomd or flattencom daemon start
//! - Automatic: client connect_or_spawn when needed
//!
//! Shutdown via RPC or signals closes sessions and removes the socket file.

#![forbid(unsafe_code)]
#![allow(clippy::too_many_lines)]

mod dispatch;
mod state;
mod wire;
mod workbench;

use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use tokio::io::{AsyncWriteExt, BufReader};
use tokio::sync::{Notify, mpsc};

use flattencom_proto::methods::{RpcError, RpcRequest, error_code};
use flattencom_proto::socket::{log_dir, socket_path, state_dir, token_path};

use state::DaemonState;

/// Maximum line size matching the protocol limit.
const MAX_LINE: usize = flattencom_proto::client::MAX_LINE_BYTES;

fn main() {
    flattencom_core::i18n::init();
    let language_args: Vec<String> = std::env::args().collect();
    for (index, arg) in language_args.iter().enumerate() {
        let value = if arg == "--lang" {
            language_args.get(index + 1).map(String::as_str)
        } else {
            arg.strip_prefix("--lang=")
        };
        if let Some(value) = value {
            if let Some(language) = flattencom_core::i18n::Language::parse(value) {
                flattencom_core::i18n::set_default(language);
            } else {
                eprintln!("Invalid language: {value}. Use en or zh-CN.");
                std::process::exit(2);
            }
        }
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("flattencomd {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_help();
        return;
    }
    let foreground = args.iter().any(|a| a == "--foreground" || a == "-f");
    let verbose = args.iter().any(|a| a == "--verbose" || a == "-v");
    let mut socket_override: Option<PathBuf> = None;
    let mut it = args.iter().peekable();
    while let Some(a) = it.next() {
        if (a == "--socket" || a == "-s")
            && let Some(p) = it.next()
        {
            socket_override = Some(PathBuf::from(p));
        }
    }

    // Log to rotating data-directory files and optionally stderr.
    init_logging(verbose, foreground);

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("Failed to create Tokio runtime");

    if let Err(e) = runtime.block_on(run(socket_override)) {
        eprintln!(
            "{}",
            flattencom_core::tr!("Failed to start flattencomd: {e}", e = e)
        );
        std::process::exit(1);
    }
}

fn print_help() {
    println!(
        "{}",
        flattencom_core::tr!(
            "flattencomd background service\nUsage: flattencomd [OPTIONS]\n\nOptions:\n  --lang en|zh-CN    Display language\n  --foreground, -f  Also write logs to stderr\n  --verbose, -v     Verbose logging\n  --socket <PATH>   Socket path (default {})\n  --version, -V     Print version\n  --help, -h        Print help",
            socket_path().display()
        )
    );
}

// ---------------------------------------------------------------------------
// Logging
// ---------------------------------------------------------------------------

/// Composite writer for a file and optional stderr.
struct CompositeWriter {
    file: Arc<Mutex<std::fs::File>>,
    stderr: bool,
}

impl Clone for CompositeWriter {
    fn clone(&self) -> Self {
        Self {
            file: Arc::clone(&self.file),
            stderr: self.stderr,
        }
    }
}

impl Write for CompositeWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if let Ok(mut f) = self.file.lock() {
            let _ = f.write_all(buf);
        }
        if self.stderr {
            let _ = std::io::stderr().write_all(buf);
        }
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        if let Ok(mut f) = self.file.lock() {
            let _ = f.flush();
        }
        Ok(())
    }
}

impl tracing_subscriber::fmt::MakeWriter<'_> for CompositeWriter {
    type Writer = CompositeWriter;
    fn make_writer(&self) -> Self::Writer {
        self.clone()
    }
}

fn init_logging(verbose: bool, foreground: bool) {
    use tracing_subscriber::EnvFilter;
    let dir = log_dir();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!(
            "{}",
            flattencom_core::tr!(
                "Failed to create log directory: {e}. Logs will be written to stderr.",
                e = e
            )
        );
        tracing_subscriber::fmt()
            .with_env_filter(
                EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
            )
            .with_writer(std::io::stderr)
            .init();
        return;
    }
    // On startup, rename logs exceeding 5 MB to .1.
    let log_path = dir.join("daemon.log");
    if let Ok(meta) = std::fs::metadata(&log_path)
        && meta.len() > 5 * 1024 * 1024
    {
        let _ = std::fs::rename(&log_path, dir.join("daemon.log.1"));
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .expect("Failed to open log file");
    let level = if verbose { "debug" } else { "info" };
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(level)),
        )
        .with_writer(CompositeWriter {
            file: Arc::new(Mutex::new(file)),
            stderr: verbose || foreground,
        })
        .with_target(false)
        .init();
}

// ---------------------------------------------------------------------------
// Authentication token
// ---------------------------------------------------------------------------

/// Load or create the authentication token with mode 0600.
fn load_or_create_token() -> Result<String, String> {
    if let Ok(t) = std::fs::read_to_string(token_path()) {
        let t = t.trim().to_owned();
        if !t.is_empty() {
            return Ok(t);
        }
    }
    let dir = state_dir();
    std::fs::create_dir_all(&dir)
        .map_err(|e| flattencom_core::tr!("Failed to create state directory: {e}", e = e))?;
    let token = uuid::Uuid::now_v7().to_string();
    let path = token_path();
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&path)
        .map_err(|e| flattencom_core::tr!("Failed to create token: {e}", e = e))?;
    file.write_all(token.as_bytes())
        .map_err(|e| flattencom_core::tr!("Failed to write token: {e}", e = e))?;
    file.sync_all().map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    tracing::info!(token_path = %path.display(), "Authentication token created");
    Ok(token)
}

// ---------------------------------------------------------------------------
// Main lifecycle
// ---------------------------------------------------------------------------

async fn run(socket_override: Option<PathBuf>) -> Result<(), String> {
    let socket = socket_override.unwrap_or_else(socket_path);
    std::fs::create_dir_all(state_dir()).map_err(|e| e.to_string())?;
    // Serialize concurrent GUI/MCP auto-start attempts for the whole process lifetime.
    let instance_lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(state_dir().join("daemon.lock"))
        .map_err(|e| e.to_string())?;
    if instance_lock.try_lock().is_err() {
        tracing::info!("A background service is already using this state directory");
        return Ok(());
    }
    let token = load_or_create_token()?;

    // Platform-specific single-instance check: a reachable endpoint means a daemon already exists.
    #[cfg(unix)]
    if let Ok(mut s) = tokio::net::UnixStream::connect(&socket).await {
        let _ = s.shutdown().await;
        println!(
            "{}",
            flattencom_core::tr!("Background service already running: {}", socket.display())
        );
        return Ok(());
    }
    #[cfg(windows)]
    if let Ok(s) = tokio::net::windows::named_pipe::ClientOptions::new().open(socket.as_os_str()) {
        drop(s);
        println!(
            "{}",
            flattencom_core::tr!("Background service already running: {}", socket.display())
        );
        return Ok(());
    }
    // Remove a stale Unix socket left by an abnormal exit.
    #[cfg(unix)]
    if socket.exists() {
        use std::os::unix::fs::FileTypeExt;
        if !std::fs::symlink_metadata(&socket)
            .map_err(|e| e.to_string())?
            .file_type()
            .is_socket()
        {
            return Err(flattencom_core::tr!(
                "Socket path is occupied by a regular file: {}",
                socket.display()
            ));
        }
        std::fs::remove_file(&socket)
            .map_err(|e| flattencom_core::tr!("Failed to remove stale socket: {e}", e = e))?;
    }

    let state = Arc::new(DaemonState::new(socket.clone(), token));

    // Platform-specific listener
    #[cfg(unix)]
    let (unlink_on_exit, listener) = {
        let listener = tokio::net::UnixListener::bind(&socket).map_err(|e| {
            flattencom_core::tr!("Failed to listen on {}: {e}", socket.display(), e = e)
        })?;
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600));
        }
        (true, ListenerKind::Unix(listener))
    };
    #[cfg(windows)]
    let (_unlink_on_exit, listener) = (false, ListenerKind::Pipe(socket.clone()));

    let shutdown = Arc::new(Notify::new());
    // Shutdown source 1: poll the RPC shutdown flag.
    {
        let state = Arc::clone(&state);
        let shutdown = Arc::clone(&shutdown);
        tokio::spawn(async move {
            loop {
                if state.is_shutting_down() {
                    shutdown.notify_one();
                    return;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        });
    }
    // Shutdown source 2: Ctrl-C/SIGINT.
    {
        let shutdown = Arc::clone(&shutdown);
        tokio::spawn(async move {
            #[cfg(unix)]
            let stopped = {
                let mut terminate =
                    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                        .expect("SIGTERM handler");
                tokio::select! { result = tokio::signal::ctrl_c() => result.is_ok(), _ = terminate.recv() => true }
            };
            #[cfg(windows)]
            let stopped = tokio::signal::ctrl_c().await.is_ok();
            if stopped {
                tracing::info!("Stop signal received");
                shutdown.notify_one();
            }
        });
    }
    // Background hotplug polling
    spawn_hotplug(Arc::clone(&state));

    tracing::info!(socket = %socket.display(), "flattencomd started (v{})", env!("CARGO_PKG_VERSION"));

    // Accept loop
    loop {
        let stream = tokio::select! {
            r = accept_one(&listener) => match r { Ok(s) => s, Err(e) => {
                tracing::warn!(error = %e, "Failed to accept connection");
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            }},
            () = shutdown.notified() => break,
        };
        let conn_id = state.alloc_conn_id();
        let state = Arc::clone(&state);
        tokio::spawn(async move {
            handle_conn(state, conn_id, stream).await;
        });
    }

    // Graceful shutdown: close all sessions and remove the socket file.
    tracing::info!("Stopping background service");
    state
        .shutting_down
        .store(true, std::sync::atomic::Ordering::Relaxed);
    {
        let sessions = state.sessions.lock().expect("Session table lock poisoned");
        let ids: Vec<_> = sessions.keys().copied().collect();
        drop(sessions);
        for id in ids {
            if state.close_session(&id).is_none() {
                tracing::warn!(session = %id, "Session already removed during shutdown");
            }
        }
    }
    #[cfg(unix)]
    if unlink_on_exit {
        let _ = std::fs::remove_file(&socket);
    }
    tracing::info!("Service stopped");
    drop(instance_lock);
    Ok(())
}

/// Platform listener variants.
enum ListenerKind {
    #[cfg(unix)]
    Unix(tokio::net::UnixListener),
    #[cfg(windows)]
    Pipe(PathBuf),
}

/// Accept a platform connection.
async fn accept_one(
    kind: &ListenerKind,
) -> std::io::Result<Box<dyn flattencom_proto::AsyncStream>> {
    match kind {
        #[cfg(unix)]
        ListenerKind::Unix(l) => {
            let (stream, _addr) = l.accept().await?;
            Ok(Box::new(stream) as Box<dyn flattencom_proto::AsyncStream>)
        }
        #[cfg(windows)]
        ListenerKind::Pipe(path) => {
            use tokio::net::windows::named_pipe::ServerOptions;
            let server = ServerOptions::new()
                .first_pipe_instance(false)
                .reject_remote_clients(true)
                .create(path)?;
            server.connect().await?;
            Ok(Box::new(server) as Box<dyn flattencom_proto::AsyncStream>)
        }
    }
}

// ---------------------------------------------------------------------------
// Connection handling
// ---------------------------------------------------------------------------

async fn handle_conn(
    state: Arc<DaemonState>,
    conn_id: u64,
    stream: Box<dyn flattencom_proto::AsyncStream>,
) {
    let (r, w) = tokio::io::split(stream);
    let mut reader = BufReader::with_capacity(64 * 1024, r);
    let (out_tx, out_rx) = mpsc::channel::<String>(state::OUT_BACKLOG_CAP);
    // Outbound writer mixes replies and notifications; an empty line requests shutdown.
    let writer = tokio::spawn(async move {
        let mut w = w;
        let mut out_rx = out_rx;
        while let Some(line) = out_rx.recv().await {
            if line.is_empty() {
                break;
            }
            let mut l = line;
            l.push('\n');
            if let Err(error) = w.write_all(l.as_bytes()).await {
                tracing::warn!(conn_id, %error, "RPC socket write failed");
                break;
            }
            if let Err(error) = w.flush().await {
                tracing::warn!(conn_id, %error, "RPC socket flush failed");
                break;
            }
        }
        let _ = w.shutdown().await;
    });

    // Handshake: the first message must be hello.
    let line = match tokio::time::timeout(
        Duration::from_secs(5),
        flattencom_proto::framing::read_line(&mut reader, MAX_LINE),
    )
    .await
    {
        Ok(Ok(Some(line))) => line,
        _ => return, // Timeout, IO error or peer closure
    };
    let hello: RpcRequest =
        match serde_json::from_str::<RpcRequest>(line.trim_end_matches(['\r', '\n'])) {
            Ok(r) if r.method == "hello" && r.jsonrpc == "2.0" => r,
            _ => {
                let _ = out_tx
                    .send(error_response(
                        0,
                        RpcError {
                            code: error_code::UNAUTHORIZED,
                            message: flattencom_core::i18n::text(
                                "The first message must be hello; see docs/PROTOCOL.md",
                            )
                            .into(),
                            data: serde_json::Value::Null,
                        },
                    ))
                    .await;
                return;
            }
        };
    // Dispatch hello for authentication and protocol-version validation.
    let language = hello
        .params
        .get("_language")
        .and_then(serde_json::Value::as_str)
        .and_then(flattencom_core::i18n::Language::parse)
        .unwrap_or(flattencom_core::i18n::Language::English);
    let hello_result = flattencom_core::i18n::with_language(language, || {
        dispatch::dispatch(&state, conn_id, "hello", hello.params.clone())
    });
    match hello_result {
        Ok(welcome) => {
            // Register the connection using the label supplied in hello.
            let label = hello
                .params
                .get("client")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown")
                .to_owned();
            state.register_conn(conn_id, label, out_tx.clone());
            let _ = out_tx.send(wire::response(hello.id, Ok(welcome))).await;
        }
        Err(e) => {
            let _ = out_tx.send(error_response(hello.id, e)).await;
            return;
        }
    }

    // Request dispatch loop
    let slow_calls = Arc::new(tokio::sync::Semaphore::new(2));
    loop {
        match flattencom_proto::framing::read_line(&mut reader, MAX_LINE).await {
            Ok(None) => {
                tracing::info!(conn_id, "RPC peer closed connection (EOF)");
                break;
            }
            Err(error) => {
                tracing::warn!(conn_id, %error, "RPC socket read/framing failed");
                break;
            }
            Ok(Some(buf)) => {
                let trimmed = buf.trim_end_matches(['\r', '\n']);
                if trimmed.is_empty() {
                    continue;
                }
                let req = match parse_request(trimmed) {
                    Ok(request) => request,
                    Err((id, error)) => {
                        let _ = out_tx.send(error_response(id, error)).await;
                        continue;
                    }
                };
                let state = Arc::clone(&state);
                let out = out_tx.clone();
                let id = req.id;
                let (method, params) = (req.method.clone(), req.params.clone());
                let slow = matches!(method.as_str(), "send_file" | "replay_log");
                let permit = if slow {
                    match slow_calls.clone().try_acquire_owned() {
                        Ok(permit) => Some(permit),
                        Err(_) => {
                            let _ = out
                                .send(error_response(
                                    id,
                                    RpcError {
                                        code: error_code::INVALID_PARAMS,
                                        message: "Long-operation limit reached".into(),
                                        data: serde_json::Value::Null,
                                    },
                                ))
                                .await;
                            continue;
                        }
                    }
                } else {
                    None
                };
                let language = params
                    .get("_language")
                    .and_then(serde_json::Value::as_str)
                    .and_then(flattencom_core::i18n::Language::parse)
                    .unwrap_or(flattencom_core::i18n::Language::English);
                // A supervisor awaits every blocking task, including compatibility waits.
                let task = tokio::task::spawn_blocking(move || {
                    let result = flattencom_core::i18n::with_language(language, || {
                        dispatch::dispatch(&state, conn_id, &method, params)
                    });
                    wire::response(id, result)
                });
                let delivery = async move {
                    // Keep compatibility admission bounded through outbound backpressure.
                    let _permit = permit;
                    deliver_reply(task, out, id, language).await;
                };
                if slow {
                    tokio::spawn(delivery);
                } else {
                    delivery.await;
                }
            }
        }
    }
    // Deregister the connection and its subscriptions.
    state.deregister_conn(conn_id);
    drop(out_tx);
    let _ = writer.await;
}

/// Always correlate a terminal dispatch outcome, without exposing panic payloads.
async fn deliver_reply(
    task: tokio::task::JoinHandle<String>,
    out: mpsc::Sender<String>,
    id: u64,
    language: flattencom_core::i18n::Language,
) {
    let line = task.await.unwrap_or_else(|error| {
        tracing::error!(id, %error, "RPC dispatch task failed");
        error_response(
            id,
            RpcError {
                code: error_code::INTERNAL,
                message: flattencom_core::i18n::with_language(language, || {
                    flattencom_core::i18n::text("Internal RPC error").to_owned()
                }),
                data: serde_json::Value::Null,
            },
        )
    });
    let _ = out.send(line).await;
}

/// Distinguish JSON syntax errors from invalid, but syntactically valid, envelopes.
fn parse_request(line: &str) -> Result<RpcRequest, (u64, RpcError)> {
    let value: serde_json::Value = serde_json::from_str(line).map_err(|_| {
        (
            0,
            RpcError {
                code: error_code::PARSE,
                message: flattencom_core::i18n::text("Request is not valid JSON").into(),
                data: serde_json::Value::Null,
            },
        )
    })?;
    let id = value
        .get("id")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let invalid = || {
        (
            id,
            RpcError {
                code: error_code::INVALID_REQUEST,
                message: "Invalid JSON-RPC request envelope".into(),
                data: serde_json::Value::Null,
            },
        )
    };
    let request: RpcRequest = serde_json::from_value(value).map_err(|_| invalid())?;
    if request.jsonrpc != "2.0" {
        return Err(invalid());
    }
    Ok(request)
}

fn error_response(id: u64, e: RpcError) -> String {
    wire::response(id, Err(e))
}

// ---------------------------------------------------------------------------
// Hotplug polling
// ---------------------------------------------------------------------------

fn spawn_hotplug(state: Arc<DaemonState>) {
    tokio::task::spawn_blocking(move || {
        let mut watcher =
            match flattencom_core::discovery::HotplugWatcher::start(Duration::from_millis(1500)) {
                Ok(w) => w,
                Err(e) => {
                    tracing::warn!(error = %e, "Cannot monitor device connections");
                    return;
                }
            };
        loop {
            if state.is_shutting_down() {
                return;
            }
            match watcher.poll_once() {
                Ok(Some(ev)) => {
                    tracing::info!(event = ?ev, "Port connection changed");
                    dispatch::broadcast_hotplug(&state, ev);
                }
                Ok(None) => {}
                Err(e) => {
                    tracing::warn!(error = %e, "Device polling failed; retrying in 3 seconds");
                    std::thread::sleep(Duration::from_secs(3));
                }
            }
        }
    });
}

#[cfg(test)]
mod dispatch_failure_tests {
    #[tokio::test]
    async fn panic_replies_are_correlated_for_serial_and_background_dispatch() {
        for background in [false, true] {
            let (out, mut replies) = tokio::sync::mpsc::channel(1);
            let task = tokio::task::spawn_blocking(|| -> String { panic!("private payload") });
            let delivery =
                super::deliver_reply(task, out, 73, flattencom_core::i18n::Language::English);
            if background {
                tokio::spawn(delivery);
            } else {
                delivery.await;
            }
            let line = tokio::time::timeout(std::time::Duration::from_secs(2), replies.recv())
                .await
                .unwrap()
                .unwrap();
            let response: serde_json::Value = serde_json::from_str(&line).unwrap();
            assert_eq!(response["id"], 73);
            assert_eq!(response["error"]["code"], -32603);
            assert_eq!(response["error"]["message"], "Internal RPC error");
            assert!(!line.contains("private payload"));
        }
    }
}
