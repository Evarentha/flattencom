/*
 * flattencom - Workbench Transfer Contracts
 *
 * Defines shared cancellable transfer and replay parameters for service and MCP clients.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

//! Shared workbench transfer contracts used by service and MCP clients.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Cancellable transfer or timed JSONL replay.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TransferParams {
    /// Target session identifier.
    pub session_id: String,
    /// Input file; mutually exclusive with data and hex.
    pub path: Option<String>,
    /// Exact UTF-8 payload.
    pub data: Option<String>,
    /// Hexadecimal payload.
    pub hex: Option<String>,
    /// Maximum bytes per transport command, capped at 4096.
    pub chunk_size: Option<u64>,
    /// Delay between chunks, capped at 5000 milliseconds.
    pub pacing_ms: Option<u64>,
    /// Interpret path as a timed JSONL capture.
    #[serde(default)]
    pub replay: bool,
    /// Replay time multiplier; must be finite and positive.
    pub speed: Option<f64>,
}
