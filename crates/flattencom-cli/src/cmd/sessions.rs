/*
 * flattencom - CLI Cmd Sessions
 *
 * Retrieves shared session summaries and displays their state and ownership.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

//! `sessions`: list daemon sessions.

use std::io::Write;

use super::{CmdError, connect_daemon};
use crate::exit_code;

/// Entry point wrapping the async runtime for blocking callers.
pub fn run(
    json: bool,
    socket: Option<&std::path::Path>,
    out: &mut impl Write,
) -> Result<i32, CmdError> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| CmdError::new(flattencom_core::tr!("Failed to create runtime: {e}", e = e)))?;
    rt.block_on(run_async(json, socket, out))
}

/// Async implementation.
pub async fn run_async(
    json: bool,
    socket: Option<&std::path::Path>,
    out: &mut impl Write,
) -> Result<i32, CmdError> {
    let client = connect_daemon("flattencom-cli", socket).await?;
    let sessions: flattencom_proto::methods::ListSessionsResult = client
        .call_typed(
            "list_sessions",
            &serde_json::json!({}),
            std::time::Duration::from_secs(10),
        )
        .await
        .map_err(|e| CmdError::new(e.to_string()))?;
    if json {
        writeln!(
            out,
            "{}",
            serde_json::to_value(&sessions).unwrap_or_default()
        )
        .map_err(|e| CmdError::new(e.to_string()))?;
        return Ok(exit_code::OK);
    }
    if sessions.sessions.is_empty() {
        writeln!(out, "{}", flattencom_core::tr!("No active sessions"))
            .map_err(|e| CmdError::new(e.to_string()))?;
        return Ok(exit_code::OK);
    }
    writeln!(
        out,
        "{}",
        flattencom_core::tr!("{:<26} {:<18} {:<14} Owner", "Session ID", "Port", "State")
    )
    .map_err(|e| CmdError::new(e.to_string()))?;
    for s in &sessions.sessions {
        let state = match &s.state {
            flattencom_core::session::SessionState::Connected => "connected".to_owned(),
            flattencom_core::session::SessionState::Reconnecting { attempt } => {
                format!("reconnecting({attempt})")
            }
            flattencom_core::session::SessionState::Closed => "closed".to_owned(),
            flattencom_core::session::SessionState::Failed { error } => format!("failed({error})"),
        };
        writeln!(
            out,
            "{:<26} {:<18} {:<14} {}",
            &s.session_id[..s.session_id.len().min(24)],
            truncate(&s.path, 18),
            state,
            s.owner
        )
        .map_err(|e| CmdError::new(e.to_string()))?;
    }
    Ok(exit_code::OK)
}

fn truncate(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}
