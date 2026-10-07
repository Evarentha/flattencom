/*
 * flattencom - Core Ids
 *
 * Creates and parses UUIDv7 identifiers for serial sessions.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

//! Session identifiers.

use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// UUIDv7 session ID, ordered by creation time for logs and captures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionId(pub Uuid);

impl SessionId {
    /// Generate a session ID.
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }

    /// Parse an identifier from RPC parameters or a capture.
    pub fn parse(s: &str) -> Result<Self, crate::FlattenError> {
        Uuid::parse_str(s)
            .map(Self)
            .map_err(|e| crate::FlattenError::InvalidConfig {
                field: "session_id".into(),
                reason: e.to_string(),
            })
    }
}

impl Default for SessionId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.simple())
    }
}
