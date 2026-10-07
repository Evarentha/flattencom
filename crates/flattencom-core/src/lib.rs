/*
 * flattencom - Core Lib
 *
 * Exports the synchronous serial engine modules and their shared public abstractions.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

//! # flattencom-core
//!
//! Core domain library: serial sessions, decoding, filters, triggers, capture, replay and discovery.
//! Synchronous engine with blocking IO and dedicated threads; no async runtime or UI dependency.
//! CLI/TUI, daemon and MCP reuse this library's domain logic.
//!
//! ## Core abstractions
//!
//! - [`transport`]: serial transport abstraction; physical serialport and virtual test channels
//! - [`session::SessionHandle`]: session workers, ring buffer and monotonic frame sequences
//! - [`frame::Frame`]: inexpensive immutable snapshots with dual timestamps and decoding results
//! - [`store::FrameStore`]: count/byte-bounded buffer with visible eviction accounting
//! - [`decode`]: extensible hex, ASCII, Modbus RTU, NMEA, JSON and process decoders
//!
//! ## Quick example
//!
//! ```no_run
//! use std::sync::Arc;
//! use flattencom_core::{session::SessionHandle, config::SerialConfig, ids::SessionId,
//!                      transport::{TransportFactory, TransportRegistry}};
//!
//! let registry = Arc::new(TransportRegistry::new());
//! // virtual://echo requires no hardware and can verify the basic workflow.
//! let cfg = SerialConfig::new("virtual://echo");
//! let session = SessionHandle::open(SessionId::new(), cfg, registry).unwrap();
//! session.send(b"AT\r\n".to_vec()).unwrap();
//! let page = session.read_frames(None, 64 * 1024);
//! assert!(!page.frames.is_empty());
//! session.close();
//! ```

// Native serial framing requires a borrowed OS descriptor and Windows DCB calls.
// Only those small, documented adapter functions opt out of this default lint.
#![deny(unsafe_code)]

pub mod autobaud;
pub mod boot;
pub mod capture_sink;
mod capture_worker;
pub mod config;
pub mod decode;
pub mod discovery;
pub mod error;
pub mod filter;
pub mod frame;
pub mod i18n;
pub mod ids;
mod messages;
pub mod record;
pub mod session;
pub mod stats;
pub mod store;
pub mod timefmt;
pub mod transcript;
pub mod transport;
pub mod trigger;

pub use error::FlattenError;
