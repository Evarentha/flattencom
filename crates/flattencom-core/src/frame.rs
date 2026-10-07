/*
 * flattencom - Core Frame
 *
 * Defines shared immutable RX/TX frames, timestamps, decoding results and byte serialization.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

//! Frame model: direction, decoding results and immutable snapshots.
//!
//! Design:
//! - `data` and `decoded` use `Arc`, making clones cheap for frequent event fan-out;
//! - `t_us` is wall-clock time for display/export; `mono_us` is monotonic for ordering and replay;
//! - Serialize `data` as hexadecimal for readable logs, RPC and captures.

use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Frame direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    /// Receive (device to host).
    Rx,
    /// Transmit (host to device).
    Tx,
}

impl Direction {
    /// Name.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Rx => "rx",
            Self::Tx => "tx",
        }
    }

    /// Single-character marker for compact table columns.
    #[must_use]
    pub fn as_char(self) -> char {
        match self {
            Self::Rx => '←',
            Self::Tx => '→',
        }
    }

    /// Parse a value from RPC or a capture.
    pub fn parse(s: &str) -> Result<Self, crate::FlattenError> {
        match s {
            "rx" | "RX" => Ok(Self::Rx),
            "tx" | "TX" => Ok(Self::Tx),
            other => Err(crate::FlattenError::InvalidConfig {
                field: "dir".into(),
                reason: crate::tr!("Expected rx or tx, got {other:?}", other = other),
            }),
        }
    }
}

/// Decode severity for display colors and client inspection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "lowercase")]
pub enum DecodeLevel {
    /// Informational result.
    #[default]
    Info,
    /// Suspected problem, such as questionable encoding or checksum failure.
    Warn,
    /// Confirmed protocol or CRC error.
    Error,
}

/// Decoded fields, preserving insertion order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DecodeField {
    /// Field name, such as `addr` or `fc`.
    pub name: String,
    /// Readable field value, such as `Read holding registers`.
    pub value: String,
}

/// Decoding result for one chunk.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DecodedInfo {
    /// Identifier of the decoder producing this result.
    pub decoder: String,
    /// Severity level.
    #[serde(default)]
    pub level: DecodeLevel,
    /// Readable text; multiple protocol frames may produce multiple lines.
    pub text: String,
    /// Structured fields.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<DecodeField>,
}

impl DecodedInfo {
    /// Informational result.
    #[must_use]
    pub fn info(decoder: &str, text: String, fields: Vec<DecodeField>) -> Self {
        Self {
            decoder: decoder.into(),
            level: DecodeLevel::Info,
            text,
            fields,
        }
    }
    /// Warning.
    #[must_use]
    pub fn warn(decoder: &str, text: String, fields: Vec<DecodeField>) -> Self {
        Self {
            decoder: decoder.into(),
            level: DecodeLevel::Warn,
            text,
            fields,
        }
    }
    /// Error.
    #[must_use]
    pub fn error(decoder: &str, text: String, fields: Vec<DecodeField>) -> Self {
        Self {
            decoder: decoder.into(),
            level: DecodeLevel::Error,
            text,
            fields,
        }
    }
}

/// Serial frame.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Frame {
    /// Monotonically increasing session sequence, preserved on reconnect and used for cursors/deduplication.
    pub seq: u64,
    /// System wall-clock time in UNIX microseconds.
    pub t_us: i64,
    /// Monotonic microseconds since session start.
    pub mono_us: u64,
    /// Direction.
    pub dir: Direction,
    /// Origin of a transmitted operation (client label / trigger), absent for RX.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Payload with hexadecimal serialization; see [`hex_data`].
    #[serde(with = "hex_data")]
    #[schemars(with = "String")]
    pub data: Arc<Vec<u8>>,
    /// Decoding result, or `None` when decoding is disabled or produces nothing.
    #[serde(default, with = "arc_decoded", skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<DecodedInfo>")]
    pub decoded: Option<Arc<DecodedInfo>>,
}

/// Serde adapter for `Option<Arc<DecodedInfo>>` without requiring Arc serde support.
pub mod arc_decoded {
    use std::sync::Arc;

    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    use super::DecodedInfo;

    /// Serialize the inner value.
    pub fn serialize<S: Serializer>(v: &Option<Arc<DecodedInfo>>, s: S) -> Result<S::Ok, S::Error> {
        match v {
            None => s.serialize_none(),
            Some(d) => DecodedInfo::serialize(d, s),
        }
    }

    /// Deserialize `Option<DecodedInfo>` and wrap it in `Arc`.
    pub fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<Option<Arc<DecodedInfo>>, D::Error> {
        let o = Option::<DecodedInfo>::deserialize(d)?;
        Ok(o.map(Arc::new))
    }
}

/// Serde adapter between `Arc<Vec<u8>>` and the shared capture/RPC hexadecimal format.
pub mod hex_data {
    use super::Arc;
    use serde::{Deserialize, Deserializer, Serializer};

    /// Serialize uppercase hexadecimal, matching display conventions.
    pub fn serialize<S: Serializer>(v: &Arc<Vec<u8>>, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&hex::encode_upper(v.as_slice()))
    }

    /// Deserialize hexadecimal bytes.
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Arc<Vec<u8>>, D::Error> {
        let s = String::deserialize(d)?;
        let bytes = hex::decode(s.trim()).map_err(serde::de::Error::custom)?;
        Ok(Arc::new(bytes))
    }
}

impl Frame {
    /// Construct a frame using timestamps supplied by `session`.
    #[must_use]
    pub fn new(seq: u64, dir: Direction, data: Vec<u8>, t_us: i64, mono_us: u64) -> Self {
        Self {
            seq,
            dir,
            source: None,
            data: Arc::new(data),
            t_us,
            mono_us,
            decoded: None,
        }
    }

    /// Attach decoding results.
    #[must_use]
    pub fn with_decoded(mut self, decoded: Option<DecodedInfo>) -> Self {
        self.decoded = decoded.map(Arc::new);
        self
    }

    /// Byte count.
    #[must_use]
    pub fn len(&self) -> usize {
        self.data.len()
    }

    /// Retention charge for frame structure and owned heap capacities.
    /// Shared Arc allocations are charged in full per retained frame; allocator
    /// bookkeeping and unused slots in the enclosing store are not included.
    #[must_use]
    pub fn storage_len(&self) -> usize {
        let arc_counters = 2 * std::mem::size_of::<usize>();
        let base = std::mem::size_of::<Self>()
            .saturating_add(std::mem::size_of::<Vec<u8>>())
            .saturating_add(arc_counters)
            .saturating_add(self.data.capacity())
            .saturating_add(self.source.as_ref().map_or(0, String::capacity));
        self.decoded.as_ref().map_or(base, |d| {
            d.fields.iter().fold(
                base.saturating_add(std::mem::size_of::<DecodedInfo>())
                    .saturating_add(arc_counters)
                    .saturating_add(d.text.capacity())
                    .saturating_add(d.decoder.capacity())
                    .saturating_add(
                        d.fields
                            .capacity()
                            .saturating_mul(std::mem::size_of::<DecodeField>()),
                    ),
                |bytes, field| {
                    bytes
                        .saturating_add(field.name.capacity())
                        .saturating_add(field.value.capacity())
                },
            )
        })
    }

    /// Conservative JSON expansion estimate for bounded wire responses.
    #[must_use]
    pub fn wire_size_hint(&self) -> usize {
        let strings = self.decoded.as_ref().map_or(0, |d| {
            d.fields.iter().fold(
                d.text.len().saturating_add(d.decoder.len()),
                |bytes, field| {
                    bytes
                        .saturating_add(field.name.len())
                        .saturating_add(field.value.len())
                },
            )
        });
        self.data
            .len()
            .saturating_add(self.source.as_ref().map_or(0, String::len))
            .saturating_add(strings)
            .saturating_mul(10)
            .saturating_add(512)
            // Empty decoder field names/values still have JSON object overhead.
            .saturating_add(
                self.decoded
                    .as_ref()
                    .map_or(0, |d| d.fields.len().saturating_mul(32)),
            )
    }

    /// Whether the frame is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Hexadecimal representation, spaced (`"41 54"`) or compact (`"4154"`).
    #[must_use]
    pub fn hex_string(&self, spaced: bool) -> String {
        if spaced {
            const DIGITS: &[u8; 16] = b"0123456789ABCDEF";
            let mut result = String::with_capacity(self.len().saturating_mul(3));
            for (i, byte) in self.data.iter().enumerate() {
                if i != 0 {
                    result.push(' ');
                }
                result.push(char::from(DIGITS[usize::from(byte >> 4)]));
                result.push(char::from(DIGITS[usize::from(byte & 15)]));
            }
            result
        } else {
            hex::encode_upper(self.data.as_slice())
        }
    }

    /// Lossy UTF-8 text; replace invalid sequences but preserve control characters.
    #[must_use]
    pub fn text_lossy(&self) -> String {
        String::from_utf8_lossy(self.data.as_slice()).into_owned()
    }

    /// Visible control-character text (`CR→␍ LF→␊ NUL→␀`, etc.).
    ///
    /// Default display text exposes normally invisible `\r\n` and `NUL` bytes,
    /// supporting protocol inspection. Search and filtering also use this text.
    #[must_use]
    pub fn text_visual(&self) -> String {
        let mut out = String::with_capacity(self.data.len());
        for c in String::from_utf8_lossy(&self.data).chars() {
            match c {
                '\u{0000}'..='\u{001F}' => {
                    out.push(char::from_u32(0x2400 + u32::from(c)).unwrap_or('\u{FFFD}'));
                }
                '\u{007F}' => out.push('\u{2421}'),
                _ => out.push(c),
            }
        }
        out
    }

    /// Printable-byte ratio (0.0..1.0) for baud detection and encoding guidance.
    #[must_use]
    pub fn printable_ratio(&self) -> f64 {
        if self.data.is_empty() {
            return 0.0;
        }
        let printable = self
            .data
            .iter()
            .filter(|&&b| (0x20..=0x7E).contains(&b) || b == b'\r' || b == b'\n' || b == b'\t')
            .count();
        f64::from(printable as u32) / f64::from(self.data.len() as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attribution_is_counted_in_storage_and_escaped_wire_estimates() {
        let mut frame = Frame::new(0, Direction::Tx, vec![0x41], 0, 0);
        let base = frame.storage_len();
        frame.source = Some("\0".repeat(262_144));
        assert_eq!(frame.storage_len(), base + 262_144);
        assert!(frame.wire_size_hint() >= serde_json::to_vec(&frame).unwrap().len());
        let frame = frame.with_decoded(Some(DecodedInfo::info(
            "decoder",
            "text".into(),
            vec![DecodeField {
                name: "name".into(),
                value: "value".into(),
            }],
        )));
        assert!(
            frame.storage_len()
                >= base + 262_144 + 7 + 4 + 4 + 5 + std::mem::size_of::<DecodeField>()
        );
        assert!(frame.wire_size_hint() >= serde_json::to_vec(&frame).unwrap().len());
    }

    #[test]
    fn wire_estimate_counts_empty_decoder_field_objects() {
        let frame =
            Frame::new(0, Direction::Rx, Vec::new(), 0, 0).with_decoded(Some(DecodedInfo::info(
                "",
                String::new(),
                vec![
                    DecodeField {
                        name: String::new(),
                        value: String::new()
                    };
                    1024
                ],
            )));
        assert!(frame.wire_size_hint() >= serde_json::to_vec(&frame).unwrap().len());
    }

    #[test]
    fn storage_charges_spare_capacity_without_inflating_wire_estimate() {
        let compact =
            Frame::new(0, Direction::Rx, vec![0x41], 0, 0).with_decoded(Some(DecodedInfo::info(
                "d",
                "t".into(),
                vec![DecodeField {
                    name: "n".into(),
                    value: "v".into(),
                }],
            )));
        let mut reserved = compact.clone();
        Arc::make_mut(&mut reserved.data).reserve(4096);
        let decoded = Arc::make_mut(reserved.decoded.as_mut().unwrap());
        decoded.decoder.reserve(2048);
        decoded.text.reserve(2048);
        decoded.fields.reserve(1000);
        decoded.fields[0].name.reserve(1024);
        decoded.fields[0].value.reserve(1024);
        assert!(
            reserved.storage_len()
                >= compact.storage_len() + 10_240 + 1000 * std::mem::size_of::<DecodeField>()
        );
        assert_eq!(reserved.wire_size_hint(), compact.wire_size_hint());
        assert_eq!(
            serde_json::to_value(&reserved).unwrap(),
            serde_json::to_value(&compact).unwrap()
        );
        reserved.source = Some(String::with_capacity(2048));
        assert!(
            reserved.storage_len()
                >= compact.storage_len() + 12_288 + 1000 * std::mem::size_of::<DecodeField>()
        );
    }

    #[test]
    fn 控制字符可视化() {
        let f = Frame::new(1, Direction::Rx, b"AT\r\n".to_vec(), 0, 0);
        assert_eq!(f.text_visual(), "AT␍␊");
        assert_eq!(f.hex_string(true), "41 54 0D 0A");
        assert_eq!(f.hex_string(false), "41540D0A");
    }

    #[test]
    fn 十六进制序列化往返() {
        let f = Frame::new(7, Direction::Tx, vec![0xDE, 0xAD, 0xBE, 0xEF], 123, 456)
            .with_decoded(Some(DecodedInfo::info("modbus_rtu", "x".into(), vec![])));
        let s = serde_json::to_string(&f).unwrap();
        assert!(s.contains(r#""data":"DEADBEEF""#), "{s}");
        assert!(s.contains(r#""dir":"tx""#));
        let back: Frame = serde_json::from_str(&s).unwrap();
        assert_eq!(back, f);
    }

    #[test]
    fn 可打印比例() {
        let text = Frame::new(1, Direction::Rx, b"hello world\r\n".to_vec(), 0, 0);
        assert!((text.printable_ratio() - 1.0).abs() < f64::EPSILON);
        let bin = Frame::new(1, Direction::Rx, vec![0x00, 0xFF, 0x80, 0x11], 0, 0);
        assert!(bin.printable_ratio() < 0.3);
    }

    #[test]
    fn 方向解析() {
        assert_eq!(Direction::parse("rx").unwrap(), Direction::Rx);
        assert_eq!(Direction::parse("TX").unwrap(), Direction::Tx);
        assert!(Direction::parse("zz").is_err());
    }
}
