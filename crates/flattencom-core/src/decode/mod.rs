/*
 * flattencom - Core Decode Mod
 *
 * Defines streaming decoder interfaces, specifications and the built-in decoder registry.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

//! Decoder framework: stateful [`Decoder`] trait and registry.
//!
//! Built-in decoders:
//!
//! | ID | Description |
//! |----|------|
//! | [`builtin::hex_dump`]("hex_dump") | Offset hexadecimal dump |
//! | `ascii_lines` | Text and visible control characters (default display decoder) |
//! | `utf8_lossy` | Lossy UTF-8 text |
//! | `json_lines` | Line-oriented JSON with flattened fields |
//! | `modbus_rtu` | CRC16 validation, parsed fields and readable exception codes |
//! | `nmea0183` | NMEA 0183 sentences and checksums |
//! | `cmd` | External decoder process using JSONL, implementable in any language |
//!
//! Each `feed` processes one chunk; protocols with partial frames or lines
//! retain their reassembly state inside the decoder.

pub mod builtin;
pub mod cmd;
pub mod modbus_rtu;
pub mod nmea0183;
pub mod wasm;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::FlattenError;
use crate::frame::{DecodedInfo, Direction};

/// Read-only frame decoding context.
pub struct ChunkCtx<'a> {
    /// Direction.
    pub dir: Direction,
    /// Wall-clock UNIX microseconds.
    pub t_us: i64,
    /// Monotonic microseconds since session start.
    pub mono_us: u64,
    /// Raw bytes.
    pub data: &'a [u8],
}

/// Stateful decoder implementing `Send` for session worker threads.
pub trait Decoder: Send {
    /// Registered decoder identifier.
    fn id(&self) -> &'static str;

    /// Process a frame and return `None` when there is no result.
    fn feed(&mut self, ctx: &ChunkCtx<'_>) -> Option<DecodedInfo>;
}

/// Decoder specification; options are passed to the selected decoder.
///
/// Example cmd options: `{"program": "python3", "args": ["decode.py"]}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DecoderSpec {
    /// Registered decoder name.
    pub name: String,
    /// Decoder options.
    #[serde(default)]
    pub options: serde_json::Value,
}

/// Registry of built-in and external process decoders.
#[derive(Debug, Default)]
pub struct DecoderRegistry;

/// All available decoder identifiers.
pub const DECODER_IDS: &[&str] = &[
    "ascii_lines",
    "hex_dump",
    "utf8_lossy",
    "json_lines",
    "modbus_rtu",
    "nmea0183",
    "cmd",
    "wasm",
];

impl DecoderRegistry {
    /// Create a registry.
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// One-line decoder description for UI and `tools/list`.
    #[must_use]
    pub fn describe(id: &str) -> Option<&'static str> {
        Some(crate::i18n::text(match id {
            "ascii_lines" => "Text with visible control characters",
            "hex_dump" => "Hex dump with offsets",
            "utf8_lossy" => "UTF-8 text with replacement for invalid bytes",
            "json_lines" => "JSON lines with top-level fields",
            "modbus_rtu" => "Modbus RTU with CRC16, fields and exception descriptions",
            "nmea0183" => "NMEA 0183 sentences and checksums",
            "cmd" => "External process plugin using JSONL",
            "wasm" => "Sandboxed WASM plugin with 16 MiB memory and instruction limits",
            _ => return None,
        }))
    }

    /// Construct a decoder from its specification.
    pub fn build(spec: &DecoderSpec) -> Result<Box<dyn Decoder>, FlattenError> {
        let unknown = || {
            FlattenError::Decode(crate::tr!(
                "Unknown decoder {name:?}; available: {ids}",
                name = spec.name,
                ids = DECODER_IDS.join(", ")
            ))
        };
        Ok(match spec.name.as_str() {
            "ascii_lines" => Box::new(builtin::AsciiLinesDecoder::default()),
            "hex_dump" => Box::new(builtin::HexDumpDecoder),
            "utf8_lossy" => Box::new(builtin::Utf8LossyDecoder),
            "json_lines" => Box::new(builtin::JsonLinesDecoder::default()),
            "modbus_rtu" => {
                let role = spec
                    .options
                    .get("role")
                    .and_then(serde_json::Value::as_str)
                    .map_or(Ok(modbus_rtu::Role::Auto), modbus_rtu::Role::parse)?;
                Box::new(modbus_rtu::ModbusRtuDecoder::with_role(role))
            }
            "nmea0183" => Box::new(nmea0183::NmeaDecoder::default()),
            "cmd" => Box::new(cmd::CmdDecoder::spawn(&spec.options)?),
            "wasm" => Box::new(wasm::WasmDecoder::load(&spec.options)?),
            _ => return Err(unknown()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 注册表构建_未知报错() {
        let spec = DecoderSpec {
            name: "nope".into(),
            options: serde_json::Value::Null,
        };
        let err = DecoderRegistry::build(&spec).map(|_| ()).unwrap_err();
        assert!(matches!(err, FlattenError::Decode(_)));
        assert!(err.to_string().contains("ascii_lines"));
    }

    #[test]
    fn 注册表构建_内建全部可用() {
        for id in DECODER_IDS
            .iter()
            .filter(|id| !matches!(**id, "cmd" | "wasm"))
        {
            let spec = DecoderSpec {
                name: (*id).into(),
                options: serde_json::Value::Null,
            };
            assert!(DecoderRegistry::build(&spec).is_ok(), "{id} 构建失败");
        }
        assert!(
            DecoderRegistry::describe("modbus_rtu")
                .unwrap()
                .contains("Modbus")
        );
    }
}
