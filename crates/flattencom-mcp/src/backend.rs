/*
 * flattencom - MCP Backend Connection
 *
 * Serializes backend reconnection and retries only explicit read-only operations while retaining disconnect diagnostics.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

//! Serialized backend reconnection with read-only retry and retained diagnostics.
use std::sync::Arc;
use std::time::Duration;

use flattencom_proto::client::{
    ClientError, ConnectConfig, DaemonClient, DisconnectInfo, connect_or_spawn_with,
};
use serde::{Serialize, de::DeserializeOwned};
use tokio::sync::Mutex;

struct State {
    client: DaemonClient,
    generation: u64,
    disconnect: Option<DisconnectInfo>,
    last_error: Option<String>,
    retry_after: Option<tokio::time::Instant>,
    connected_at_ms: u64,
}

/// Shared by all tool calls and resource pollers in one MCP process.
#[derive(Clone)]
pub struct Backend {
    config: ConnectConfig,
    state: Arc<Mutex<State>>,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

fn read_only(method: &str) -> bool {
    matches!(
        method,
        "list_ports"
            | "get_port_info"
            | "list_sessions"
            | "read_frames"
            | "read_sent"
            | "get_log"
            | "get_decoder"
            | "list_decoders"
            | "get_triggers"
            | "get_trigger_fires"
            | "get_signals"
            | "get_stats"
            | "daemon_info"
            | "list_markers"
            | "read_selection"
            | "operation_status"
    )
}

impl Backend {
    pub fn new(client: DaemonClient, config: ConnectConfig) -> Self {
        Self {
            config,
            state: Arc::new(Mutex::new(State {
                client,
                generation: 0,
                disconnect: None,
                last_error: None,
                retry_after: None,
                connected_at_ms: now_ms(),
            })),
        }
    }

    async fn connection(&self) -> Result<DaemonClient, String> {
        // Hold this mutex only across connection establishment, never a tool RPC.
        let mut state = self.state.lock().await;
        if state.client.is_alive() {
            return Ok(state.client.clone());
        }
        if let Some(info) = state.client.disconnect_info() {
            state.disconnect = Some(info);
        }
        if state
            .retry_after
            .is_some_and(|next| tokio::time::Instant::now() < next)
        {
            return Err(state
                .last_error
                .clone()
                .unwrap_or_else(|| "Backend reconnect is cooling down".into()));
        }
        tracing::warn!(generation = state.generation, disconnect = ?state.disconnect, "Reconnecting MCP backend");
        state.client.close();
        match tokio::time::timeout(
            Duration::from_secs(10),
            connect_or_spawn_with(self.config.clone()),
        )
        .await
        {
            Ok(Ok(client)) => {
                state.client = client;
                state.generation += 1;
                state.connected_at_ms = now_ms();
                state.retry_after = None;
                state.last_error = None;
                tracing::warn!(
                    generation = state.generation,
                    at_ms = state.connected_at_ms,
                    "MCP backend reconnected"
                );
                Ok(state.client.clone())
            }
            result => {
                let cause = match result {
                    Ok(Err(error)) => error.to_string(),
                    _ => "Connection attempt exceeded 10 seconds".into(),
                };
                let message = format!("Backend disconnected; reconnect failed: {cause}");
                tracing::warn!(%cause, "MCP backend reconnect failed");
                state.last_error = Some(message.clone());
                state.retry_after = Some(tokio::time::Instant::now() + Duration::from_secs(1));
                Err(message)
            }
        }
    }

    pub async fn call<P: Serialize, R: DeserializeOwned>(
        &self,
        method: &str,
        params: &P,
        timeout: Duration,
    ) -> Result<R, String> {
        // Reconnect before dispatch is safe even for a write: it has not yet been sent.
        let client = self.connection().await?;
        match client.call_typed(method, params, timeout).await {
            Ok(result) => Ok(result),
            Err(error) => {
                let disconnected = !client.is_alive() || matches!(error, ClientError::Closed);
                let timed_out = matches!(&error, ClientError::Rpc(e) if e.code == flattencom_proto::methods::error_code::APP_BASE + 6);
                if disconnected && read_only(method) {
                    let next = self.connection().await?;
                    tracing::warn!(%method, "Retrying read-only RPC once after backend disconnect");
                    return next
                        .call_typed(method, params, timeout)
                        .await
                        .map_err(|e| e.to_string());
                }
                if !read_only(method) && (disconnected || timed_out) {
                    return Err(format!(
                        "{error}. Operation outcome is unknown; {method} was not replayed. Inspect device state before retrying."
                    ));
                }
                Err(error.to_string())
            }
        }
    }

    pub async fn status(&self) -> serde_json::Value {
        let state = self.state.lock().await;
        serde_json::json!({"connected":state.client.is_alive(),"generation":state.generation,
            "connected_at_ms":state.connected_at_ms,"last_disconnect":state.client.disconnect_info().or_else(|| state.disconnect.clone()),
            "last_reconnect_error":state.last_error})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    async fn handshake(
        listener: &tokio::net::UnixListener,
    ) -> (
        tokio::io::BufReader<tokio::net::unix::OwnedReadHalf>,
        tokio::net::unix::OwnedWriteHalf,
    ) {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
        let (stream, _) = listener.accept().await.unwrap();
        let (read, mut write) = stream.into_split();
        let mut read = tokio::io::BufReader::new(read);
        let mut line = String::new();
        read.read_line(&mut line).await.unwrap();
        let request: serde_json::Value = serde_json::from_str(&line).unwrap();
        let response = serde_json::json!({"jsonrpc":"2.0","id":request["id"],"result":{
            "daemon":"test","version":"1","proto":1,"capabilities":[]}});
        write
            .write_all(format!("{response}\n").as_bytes())
            .await
            .unwrap();
        (read, write)
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn disconnect_after_dispatch_retries_reads_but_never_writes() {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
        for method in ["get_stats", "send", "reset_device", "start_transfer"] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("backend.sock");
            let listener = tokio::net::UnixListener::bind(&path).unwrap();
            let server = tokio::spawn(async move {
                let (mut read, write) = handshake(&listener).await;
                let mut line = String::new();
                read.read_line(&mut line).await.unwrap();
                let first: serde_json::Value = serde_json::from_str(&line).unwrap();
                assert_eq!(first["method"], method);
                // The request may have executed, but no response is sent.
                drop(read);
                drop(write);
                let (mut read, mut write) = handshake(&listener).await;
                line.clear();
                read.read_line(&mut line).await.unwrap();
                let next: serde_json::Value = serde_json::from_str(&line).unwrap();
                assert_eq!(next["method"], "get_stats");
                let response =
                    serde_json::json!({"jsonrpc":"2.0","id":next["id"],"result":{"ok":true}});
                write
                    .write_all(format!("{response}\n").as_bytes())
                    .await
                    .unwrap();
            });
            let config = ConnectConfig::new("test", Duration::from_secs(1))
                .with_socket(path)
                .with_state_dir(dir.path());
            let client = DaemonClient::connect_with(config.clone()).await.unwrap();
            let backend = Backend::new(client, config);
            let result: Result<serde_json::Value, _> = backend
                .call(method, &serde_json::json!({}), Duration::from_secs(1))
                .await;
            if method == "get_stats" {
                assert_eq!(result.unwrap()["ok"], true);
            } else {
                assert!(result.unwrap_err().contains("was not replayed"));
                let query: serde_json::Value = backend
                    .call("get_stats", &serde_json::json!({}), Duration::from_secs(1))
                    .await
                    .unwrap();
                assert_eq!(query["ok"], true);
            }
            tokio::time::timeout(Duration::from_secs(3), server)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(backend.status().await["generation"], 1);
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn concurrent_callers_share_one_reconnection() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("backend.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let (finish, done) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            let first = handshake(&listener).await;
            let second = handshake(&listener).await;
            let _ = done.await;
            assert!(
                tokio::time::timeout(Duration::from_millis(50), listener.accept())
                    .await
                    .is_err()
            );
            drop((first, second));
        });
        let config = ConnectConfig::new("test", Duration::from_secs(1))
            .with_socket(path)
            .with_state_dir(dir.path());
        let client = DaemonClient::connect_with(config.clone()).await.unwrap();
        client.close();
        let backend = Backend::new(client, config);
        let mut calls = tokio::task::JoinSet::new();
        for _ in 0..16 {
            let backend = backend.clone();
            calls.spawn(async move {
                assert!(backend.connection().await.unwrap().is_alive());
            });
        }
        while let Some(result) = calls.join_next().await {
            result.unwrap();
        }
        assert_eq!(backend.status().await["generation"], 1);
        finish.send(()).unwrap();
        server.await.unwrap();
    }
    #[test]
    fn retry_policy_is_an_explicit_read_allowlist() {
        for method in [
            "send",
            "send_file",
            "reset_device",
            "start_transfer",
            "replay_log",
            "flash_esp32",
            "open_session",
            "close_session",
            "set_signals",
            "clear_buffer",
            "export_log",
            "unknown",
        ] {
            assert!(!read_only(method), "{method} must not be replayed");
        }
        for method in [
            "read_frames",
            "read_selection",
            "daemon_info",
            "list_sessions",
            "get_stats",
        ] {
            assert!(read_only(method));
        }
    }
}
