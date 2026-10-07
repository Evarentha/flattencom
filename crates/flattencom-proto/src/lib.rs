/*
 * flattencom - flattencom Flattencom-Proto Src Lib
 *
 * Exports local RPC types, framing, endpoint discovery and service client APIs.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

//! # flattencom-proto
//!
//! Contract between flattencomd and Qt GUI, CLI and MCP clients:
//! JSON-RPC 2.0 NDJSON requests, responses, events and local endpoint discovery,
//! plus the connection client with automatic service startup.
//!
//! Protocol types are defined here; shared golden fixtures in tests/fixtures.rs
//! validate the Rust/C++ wire contract. See docs/PROTOCOL.md.

#![forbid(unsafe_code)]
#![allow(clippy::module_name_repetitions)]

pub mod client;
pub mod framing;
pub mod methods;
pub mod socket;
pub mod workbench;

pub use client::{
    AsyncStream, ClientError, ConnectConfig, DaemonClient, connect_or_spawn, connect_or_spawn_with,
};
pub use methods::{Event, PROTOCOL_VERSION, RpcError, RpcNotification, RpcRequest, RpcResponse};
pub use socket::{log_dir, socket_path, state_dir, token_path};
