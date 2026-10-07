/*
 * flattencom - CLI Tui Source
 *
 * Adapts direct engine sessions and service-backed sessions to one terminal data-source interface.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

//! TUI data sources: one interface for direct core sessions and shared daemon sessions.

use std::sync::Arc;
use std::time::Duration;

use flattencom_core::config::SerialConfig;
use flattencom_core::ids::SessionId;
use flattencom_core::session::SessionHandle;
use flattencom_core::stats::SessionStats;
use flattencom_core::transport::TransportRegistry;
use flattencom_proto::client::DaemonClient;
use flattencom_proto::methods::{FrameFormat, FramesPageOut, frame_out};

use super::FrameView;
use crate::cmd::CmdError;

/// TUI source interface for both backends; driven by `poll` in the event loop.
pub trait FrameSource: Send {
    /// Fetch frames and a status update; an empty status leaves the current caption unchanged.
    fn poll(&mut self, max_bytes: u64) -> Result<(Vec<FrameView>, String), String>;
    /// Send text verbatim; include `\r` explicitly when the device expects a line ending.
    fn send(&mut self, text: String) -> Result<(), String>;
    /// Clear the buffer and return the number of removed frames.
    fn clear_buffer(&mut self) -> u64;
    /// Port/session label for the title bar.
    fn label(&self) -> String;
    /// Update current serial settings.
    fn configure(&mut self, baud: u32) -> Result<(), String>;
    /// Switch the daemon or embedded decoder.
    fn decoder(&mut self, name: &str) -> Result<(), String>;
}

/// Statistics summary for the status line.
fn stats_line(stats: &SessionStats) -> String {
    flattencom_core::tr!(
        "RX {} B / {} frames / {:.0} B/s | TX {} B / {:.0} B/s | Errors {} | Evicted {} | Buffer {} frames",
        stats.rx_bytes,
        stats.rx_frames,
        stats.rx_rate_bps,
        stats.tx_bytes,
        stats.tx_rate_bps,
        stats.errors.total(),
        stats.dropped_rx,
        stats.buffer.frames
    )
}

// ---------------------------------------------------------------------------
// Direct embedded source (`flattencom monitor`)
// ---------------------------------------------------------------------------

/// Direct data source.
pub struct EmbeddedSource {
    session: SessionHandle,
    cursor: Option<u64>,
}

impl EmbeddedSource {
    /// Open a direct session.
    #[allow(clippy::too_many_arguments)]
    pub fn open(
        port: &str,
        baud: u32,
        data_bits: Option<u8>,
        parity: Option<&str>,
        stop_bits: Option<flattencom_core::config::StopBits>,
        flow: Option<&str>,
        record_to: Option<std::path::PathBuf>,
    ) -> Result<Self, CmdError> {
        let mut cfg = SerialConfig {
            baud,
            ..SerialConfig::new(port)
        };
        if let Some(d) = data_bits {
            cfg.data_bits = match d {
                5 => flattencom_core::config::DataBits::Five,
                6 => flattencom_core::config::DataBits::Six,
                7 => flattencom_core::config::DataBits::Seven,
                _ => flattencom_core::config::DataBits::Eight,
            };
        }
        if let Some(p) = parity {
            cfg.parity =
                serde_json::from_value(serde_json::Value::String(p.to_owned())).map_err(|e| {
                    CmdError::with_code(
                        flattencom_core::tr!("Invalid --parity value: {e}", e = e),
                        crate::exit_code::ARGS,
                    )
                })?;
        }
        if let Some(t) = stop_bits {
            cfg.stop_bits = t;
        }
        if let Some(fl) = flow {
            cfg.flow_control = serde_json::from_value(serde_json::Value::String(fl.to_owned()))
                .map_err(|e| {
                    CmdError::with_code(
                        flattencom_core::tr!("Invalid --flow value: {e}", e = e),
                        crate::exit_code::ARGS,
                    )
                })?;
        }
        if let Some(r) = record_to {
            cfg.record_to = Some(r);
        }
        let registry = Arc::new(TransportRegistry::new());
        let session =
            SessionHandle::open(SessionId::new(), cfg, registry).map_err(CmdError::from_core)?;
        Ok(Self {
            session,
            cursor: None,
        })
    }
}

impl FrameSource for EmbeddedSource {
    fn poll(&mut self, max_bytes: u64) -> Result<(Vec<FrameView>, String), String> {
        let page = self
            .session
            .read_window(self.cursor.take(), None, false, max_bytes);
        self.cursor = Some(page.next_seq);
        let frames = page
            .frames
            .iter()
            .map(|f| frame_out(f, FrameFormat::Decoded))
            .collect();
        let status = stats_line(&self.session.stats());
        Ok((frames, status))
    }

    fn send(&mut self, text: String) -> Result<(), String> {
        self.session
            .send(payload(&text)?)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    fn clear_buffer(&mut self) -> u64 {
        let n = self.session.clear_buffer();
        self.cursor = None;
        n
    }

    fn label(&self) -> String {
        let cfg = self.session.config();
        format!("{} @ {}", cfg.path, cfg.baud)
    }
    fn configure(&mut self, baud: u32) -> Result<(), String> {
        self.session
            .configure(flattencom_core::config::ConfigPatch {
                baud: Some(baud),
                ..Default::default()
            })
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    fn decoder(&mut self, name: &str) -> Result<(), String> {
        self.session
            .set_decoder(decoder_spec(name))
            .map_err(|e| e.to_string())
    }
}

// ---------------------------------------------------------------------------
// Attached daemon source (`flattencom attach`), shared with GUI/MCP
// ---------------------------------------------------------------------------

/// Attached data source.
pub struct AttachedSource {
    rt: tokio::runtime::Runtime,
    client: DaemonClient,
    session_id: String,
    port: String,
    cursor: Option<u64>,
}

impl AttachedSource {
    /// Construct with a runtime; `poll` uses block_on for local RPC.
    pub fn new(
        client: DaemonClient,
        session_id: String,
        port: String,
        rt: tokio::runtime::Runtime,
    ) -> Self {
        Self {
            rt,
            client,
            session_id,
            port,
            cursor: None,
        }
    }
}

impl FrameSource for AttachedSource {
    fn poll(&mut self, max_bytes: u64) -> Result<(Vec<FrameView>, String), String> {
        let client = &self.client;
        let session_id = &self.session_id;
        let since = self.cursor;
        self.rt
            .block_on(async move {
                let mut req = serde_json::json!({
                    "session_id": session_id,
                    "format": "decoded",
                    "max_bytes": max_bytes,
                });
                if let Some(s) = since {
                    req["since_seq"] = serde_json::json!(s);
                }
                let page: FramesPageOut = client
                    .call_typed("read_frames", &req, Duration::from_secs(10))
                    .await
                    .map_err(|e| e.to_string())?;
                let status = flattencom_core::tr!(
                    "Session {session_id} | Cursor {} | Evicted {} | {}",
                    page.next_seq,
                    page.dropped_rx,
                    if page.up_to_date {
                        "Up to date"
                    } else {
                        "Reading"
                    },
                    session_id = session_id
                );
                Ok((page.frames, status, page.next_seq))
            })
            .map(|(frames, status, next_seq)| {
                self.cursor = Some(next_seq);
                (frames, status)
            })
    }

    fn send(&mut self, text: String) -> Result<(), String> {
        let data = hex::encode_upper(payload(&text)?);
        let client = &self.client;
        let session_id = self.session_id.clone();
        self.rt.block_on(async move {
            client
                .call_typed::<_, serde_json::Value>(
                    "send",
                    &serde_json::json!({ "session_id": session_id, "hex": data, "newline": "none" }),
                    Duration::from_secs(10),
                )
                .await
                .map(|_| ())
                .map_err(|e| e.to_string())
        })
    }

    fn clear_buffer(&mut self) -> u64 {
        let client = &self.client;
        let session_id = self.session_id.clone();
        let cleared = self.rt.block_on(async move {
            client
                .call_typed::<_, flattencom_proto::methods::ClearedResult>(
                    "clear_buffer",
                    &serde_json::json!({ "session_id": session_id }),
                    Duration::from_secs(10),
                )
                .await
                .map_or(0, |r| r.cleared)
        });
        self.cursor = None;
        cleared
    }

    fn label(&self) -> String {
        format!(
            "attach {} ({})",
            self.port,
            &self.session_id[..self.session_id.len().min(8)]
        )
    }
    fn configure(&mut self, baud: u32) -> Result<(), String> {
        self.rt
            .block_on(self.client.call(
                "configure_session",
                serde_json::json!({"session_id": self.session_id, "baud": baud}),
            ))
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    fn decoder(&mut self, name: &str) -> Result<(), String> {
        self.rt
            .block_on(self.client.call(
                "set_decoder",
                serde_json::json!({"session_id": self.session_id, "spec": decoder_spec(name)}),
            ))
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}

fn decoder_spec(name: &str) -> Option<flattencom_core::decode::DecoderSpec> {
    (name != "none").then(|| flattencom_core::decode::DecoderSpec {
        name: name.to_owned(),
        options: serde_json::Value::Null,
    })
}

fn payload(text: &str) -> Result<Vec<u8>, String> {
    if let Some(hex) = text.strip_prefix("hex:") {
        return hex::decode(
            hex.chars()
                .filter(|c| !c.is_ascii_whitespace())
                .collect::<String>(),
        )
        .map_err(|e| e.to_string());
    }
    let mut bytes = Vec::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            bytes.extend_from_slice(c.to_string().as_bytes());
            continue;
        }
        match chars.next() {
            Some('r') => bytes.push(b'\r'),
            Some('n') => bytes.push(b'\n'),
            Some('t') => bytes.push(b'\t'),
            Some('0') => bytes.push(0),
            Some('\\') | None => bytes.push(b'\\'),
            Some(c) => {
                bytes.push(b'\\');
                bytes.extend_from_slice(c.to_string().as_bytes());
            }
        }
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_tracks_successful_configuration_and_preserves_rate_on_failure() {
        let mut source =
            EmbeddedSource::open("virtual://echo", 115_200, None, None, None, None, None).unwrap();
        assert_eq!(source.label(), "virtual://echo @ 115200");
        source.configure(57_600).unwrap();
        assert_eq!(source.label(), "virtual://echo @ 57600");
        assert!(source.configure(0).is_err());
        assert_eq!(source.label(), "virtual://echo @ 57600");
    }
}
