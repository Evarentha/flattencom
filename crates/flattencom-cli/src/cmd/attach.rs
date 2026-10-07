/*
 * flattencom - CLI Cmd Attach
 *
 * Selects a shared service session and launches its interactive terminal monitor.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

//! `attach`: join a daemon session shared with GUI and MCP clients.

use std::io::Write;
use std::time::Duration;

use super::{CmdError, connect_daemon};
use crate::exit_code;
use crate::tui;

/// Entry point: select a session and enter the shared TUI monitor.
pub fn run(session: Option<String>, socket: Option<&std::path::Path>) -> Result<i32, CmdError> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|e| CmdError::new(flattencom_core::tr!("Failed to create runtime: {e}", e = e)))?;
    let target = rt.block_on(run_async(session, socket))?;
    match target {
        Some((client, sid, port)) => tui::run_attached(client, sid, port, rt),
        None => Ok(exit_code::OK),
    }
}

/// Async implementation: select a session, listing candidates if necessary, then enter the TUI.
async fn run_async(
    session: Option<String>,
    socket: Option<&std::path::Path>,
) -> Result<Option<(flattencom_proto::DaemonClient, String, String)>, CmdError> {
    let client = connect_daemon("flattencom-cli-attach", socket).await?;
    let sessions: flattencom_proto::methods::ListSessionsResult = client
        .call_typed(
            "list_sessions",
            &serde_json::json!({}),
            Duration::from_secs(10),
        )
        .await
        .map_err(|e| CmdError::new(e.to_string()))?;

    let sid = match session {
        Some(s) => s,
        None => {
            if sessions.sessions.is_empty() {
                return Err(CmdError::new(
                    "No sessions available. Open a port using the GUI or MCP first.",
                ));
            }
            if sessions.sessions.len() == 1 {
                sessions.sessions[0].session_id.clone()
            } else {
                // List available choices.
                let mut out = std::io::stdout().lock();
                writeln!(
                    out,
                    "{}",
                    flattencom_core::tr!("Multiple sessions found; specify a session ID:")
                )
                .map_err(|e| CmdError::new(e.to_string()))?;
                for s in &sessions.sessions {
                    let state = match &s.state {
                        flattencom_core::session::SessionState::Connected => "connected",
                        flattencom_core::session::SessionState::Reconnecting { .. } => {
                            "reconnecting"
                        }
                        flattencom_core::session::SessionState::Closed => "closed",
                        flattencom_core::session::SessionState::Failed { .. } => "failed",
                    };
                    writeln!(
                        out,
                        "  {}  {}  [{}]  {}",
                        s.session_id, s.path, state, s.owner
                    )
                    .map_err(|e| CmdError::new(e.to_string()))?;
                }
                return Ok(None);
            }
        }
    };

    // Check that the session exists.
    let found = sessions.sessions.iter().find(|s| s.session_id == sid);
    let Some(s) = found else {
        return Err(CmdError::new(flattencom_core::tr!(
            "Session {sid:?} not found. Run flattencom sessions to list sessions.",
            sid = sid
        )));
    };
    let port = s.path.clone();

    // Shared TUI monitoring
    Ok(Some((client, sid, port)))
}
