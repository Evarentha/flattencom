/*
 * flattencom - CLI Cmd Daemon
 *
 * Starts, stops and inspects the background service and reads its recent log entries.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

//! `daemon`: manage service lifecycle (start/stop/status/logs).

use std::io::Write;

use super::{CmdError, connect_daemon};
use crate::DaemonAction;
use crate::exit_code;

/// Entry point wrapping the async runtime for blocking callers.
pub fn run(
    action: DaemonAction,
    socket: Option<&std::path::Path>,
    json: bool,
    out: &mut impl Write,
) -> Result<i32, CmdError> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| CmdError::new(flattencom_core::tr!("Failed to create runtime: {e}", e = e)))?;
    rt.block_on(run_async(action, socket, json, out))
}

/// Async implementation.
pub async fn run_async(
    action: DaemonAction,
    socket: Option<&std::path::Path>,
    json: bool,
    out: &mut impl Write,
) -> Result<i32, CmdError> {
    match action {
        DaemonAction::Start => {
            // Connect with automatic startup, then print status to confirm.
            let client = connect_daemon("flattencom-cli", socket).await?;
            let info: flattencom_proto::methods::DaemonInfoResult = client
                .call_typed(
                    "daemon_info",
                    &serde_json::json!({}),
                    std::time::Duration::from_secs(10),
                )
                .await
                .map_err(|e| CmdError::new(e.to_string()))?;
            if json {
                writeln!(out, "{}", serde_json::to_value(&info).unwrap_or_default())
                    .map_err(|e| CmdError::new(e.to_string()))?;
            } else {
                writeln!(out, "{}", flattencom_core::tr!("Background service running\nVersion: {}\nUptime: {} seconds\nSessions: {}\nSocket: {}", info.version,
                    info.uptime_ms / 1000,
                    info.sessions,
                    info.socket))
                .map_err(|e| CmdError::new(e.to_string()))?;
            }
            Ok(exit_code::OK)
        }
        DaemonAction::Stop => {
            let client = connect_existing(socket).await?;
            client
                .call_typed::<_, serde_json::Value>(
                    "shutdown",
                    &serde_json::json!({}),
                    std::time::Duration::from_secs(10),
                )
                .await
                .map_err(|e| CmdError::new(e.to_string()))?;
            writeln!(
                out,
                "{}",
                if json {
                    "{\"ok\":true}"
                } else {
                    flattencom_core::i18n::text("Background service stop requested")
                }
            )
            .map_err(|e| CmdError::new(e.to_string()))?;
            Ok(exit_code::OK)
        }
        DaemonAction::Status => {
            let client = connect_existing(socket).await?;
            let info: flattencom_proto::methods::DaemonInfoResult = client
                .call_typed(
                    "daemon_info",
                    &serde_json::json!({}),
                    std::time::Duration::from_secs(10),
                )
                .await
                .map_err(|e| CmdError::new(e.to_string()))?;
            if json {
                writeln!(out, "{}", serde_json::to_value(&info).unwrap_or_default())
                    .map_err(|e| CmdError::new(e.to_string()))?;
            } else {
                writeln!(
                    out,
                    "{}",
                    flattencom_core::tr!("Version: {} (protocol v{})", info.version, info.proto)
                )
                .map_err(|e| CmdError::new(e.to_string()))?;
                writeln!(
                    out,
                    "{}",
                    flattencom_core::tr!("Uptime: {} seconds", info.uptime_ms / 1000)
                )
                .map_err(|e| CmdError::new(e.to_string()))?;
                writeln!(
                    out,
                    "{}",
                    flattencom_core::tr!("Sessions: {}", info.sessions)
                )
                .map_err(|e| CmdError::new(e.to_string()))?;
                writeln!(out, "{}", flattencom_core::tr!("Socket: {}", info.socket))
                    .map_err(|e| CmdError::new(e.to_string()))?;
                writeln!(
                    out,
                    "{}",
                    flattencom_core::tr!("Capabilities: {}", info.capabilities.join(", "))
                )
                .map_err(|e| CmdError::new(e.to_string()))?;
            }
            Ok(exit_code::OK)
        }
        DaemonAction::Logs { lines } => {
            let log_path = flattencom_proto::socket::log_dir().join("daemon.log");
            if !log_path.exists() {
                writeln!(
                    out,
                    "{}",
                    flattencom_core::tr!("Log file not found: {}", log_path.display())
                )
                .map_err(|e| CmdError::new(e.to_string()))?;
                return Ok(exit_code::OK);
            }
            let content = std::fs::read_to_string(&log_path).map_err(|e| {
                CmdError::new(flattencom_core::tr!("Failed to read log: {e}", e = e))
            })?;
            let tail: Vec<&str> = content
                .lines()
                .rev()
                .take(lines)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            for l in tail {
                writeln!(out, "{l}").map_err(|e| CmdError::new(e.to_string()))?;
            }
            Ok(exit_code::OK)
        }
    }
}

async fn connect_existing(
    socket: Option<&std::path::Path>,
) -> Result<flattencom_proto::DaemonClient, CmdError> {
    let mut config =
        flattencom_proto::ConnectConfig::new("flattencom-cli", std::time::Duration::from_secs(3));
    config.socket = socket.map(std::path::Path::to_path_buf);
    flattencom_proto::DaemonClient::connect_with(config)
        .await
        .map_err(|e| CmdError::new(e.to_string()))
}
