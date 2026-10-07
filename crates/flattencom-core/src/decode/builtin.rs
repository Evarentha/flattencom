/*
 * flattencom - Core Decode Builtin
 *
 * Decodes hexadecimal dumps, visible text, streaming UTF-8 and JSON lines.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

//! Built-in display decoders: hex dump, visible text, lossy UTF-8 and JSON lines.

use crate::decode::{ChunkCtx, Decoder};
use crate::frame::{DecodeField, DecodeLevel, DecodedInfo};
use std::fmt::Write as _;

// ---------------------------------------------------------------------------
// hex_dump
// ---------------------------------------------------------------------------

/// Offset hexadecimal dump with 16 bytes per line.
#[derive(Debug, Default)]
pub struct HexDumpDecoder;

impl Decoder for HexDumpDecoder {
    fn id(&self) -> &'static str {
        "hex_dump"
    }

    fn feed(&mut self, ctx: &ChunkCtx<'_>) -> Option<DecodedInfo> {
        let data = ctx.data;
        if data.is_empty() {
            return None;
        }
        let mut text = String::new();
        for (i, chunk) in data.chunks(16).enumerate() {
            let mut hexs = String::new();
            let mut ascii = String::new();
            for &b in chunk {
                write!(hexs, "{b:02X} ").expect("String write");
                ascii.push(if (0x20..0x7F).contains(&b) {
                    char::from_u32(u32::from(b)).unwrap_or('.')
                } else {
                    '.'
                });
            }
            for _ in chunk.len()..16 {
                hexs.push_str("   ");
            }
            writeln!(text, "{:04X}  {hexs} {ascii}", i * 16).expect("String write");
        }
        text.pop(); // Remove the trailing newline.
        Some(DecodedInfo::info(
            "hex_dump",
            text,
            vec![DecodeField {
                name: "len".into(),
                value: data.len().to_string(),
            }],
        ))
    }
}

// ---------------------------------------------------------------------------
// ascii_lines (default display decoder)
// ---------------------------------------------------------------------------

/// Text display exposes control characters (`␍`, `␊`, `␀`) so protocol details remain visible.
///
/// Mark suspected binary frames as Warn when many bytes are nonprintable,
/// making non-text protocols apparent to both users and clients.
#[derive(Debug, Default)]
pub struct AsciiLinesDecoder {
    _priv: (),
}

impl Decoder for AsciiLinesDecoder {
    fn id(&self) -> &'static str {
        "ascii_lines"
    }

    fn feed(&mut self, ctx: &ChunkCtx<'_>) -> Option<DecodedInfo> {
        let data = ctx.data;
        if data.is_empty() {
            return None;
        }
        // Equivalent to text_visual: expose all control characters.
        let mut text = String::with_capacity(data.len());
        for &b in data {
            match b {
                0x00..=0x1F => {
                    text.push(char::from_u32(0x2400 + u32::from(b)).unwrap_or('\u{FFFD}'));
                }
                0x7F => text.push('\u{2421}'),
                _ => text.push(char::from_u32(u32::from(b)).unwrap_or('\u{FFFD}')),
            }
        }
        let ratio = printable_ratio(data);
        let level = if data.len() >= 4 && ratio < 0.7 {
            DecodeLevel::Warn
        } else {
            DecodeLevel::Info
        };
        let fields = vec![
            DecodeField {
                name: "len".into(),
                value: data.len().to_string(),
            },
            DecodeField {
                name: "printable".into(),
                value: format!("{ratio:.2}"),
            },
        ];
        Some(DecodedInfo {
            decoder: "ascii_lines".into(),
            level,
            text,
            fields,
        })
    }
}

/// Printable-byte ratio.
fn printable_ratio(data: &[u8]) -> f64 {
    if data.is_empty() {
        return 0.0;
    }
    let n = data
        .iter()
        .filter(|&&b| (0x20..=0x7E).contains(&b) || b == b'\r' || b == b'\n' || b == b'\t')
        .count();
    f64::from(n as u32) / f64::from(data.len() as u32)
}

// ---------------------------------------------------------------------------
// utf8_lossy
// ---------------------------------------------------------------------------

/// Lossy UTF-8 text preserving control characters for an unmodified text view.
#[derive(Debug, Default)]
pub struct Utf8LossyDecoder;

impl Decoder for Utf8LossyDecoder {
    fn id(&self) -> &'static str {
        "utf8_lossy"
    }

    fn feed(&mut self, ctx: &ChunkCtx<'_>) -> Option<DecodedInfo> {
        if ctx.data.is_empty() {
            return None;
        }
        let cow = String::from_utf8_lossy(ctx.data);
        let has_lossy = cow.contains('\u{FFFD}');
        let level = if has_lossy {
            DecodeLevel::Warn
        } else {
            DecodeLevel::Info
        };
        Some(DecodedInfo {
            decoder: "utf8_lossy".into(),
            level,
            text: cow.into_owned(),
            fields: Vec::new(),
        })
    }
}

// ---------------------------------------------------------------------------
// json_lines
// ---------------------------------------------------------------------------

/// JSON lines: flatten top-level scalars and one nested level; mark invalid lines Error.
/// Lines may contain up to 64 KiB before LF; oversized lines are skipped through LF.
/// Invalid UTF-8 is rejected; lossy text is used only for diagnostics.
#[derive(Debug, Default)]
pub struct JsonLinesDecoder {
    /// Incomplete line retained across frames so split JSON objects can be decoded.
    lines: BoundedLines,
}

/// Byte-oriented line assembly with a per-line limit excluding LF (including CR).
/// Report overflow once, then discard through LF before accepting another line.
#[derive(Debug, Default)]
pub(super) struct BoundedLines {
    carry: Vec<u8>,
    discarding: bool,
}

impl BoundedLines {
    pub(super) fn feed(&mut self, data: &[u8], limit: usize) -> Vec<Result<Vec<u8>, Vec<u8>>> {
        let mut lines = Vec::new();
        for part in data.split_inclusive(|&b| b == b'\n') {
            let complete = part.ends_with(b"\n");
            let bytes = if complete {
                &part[..part.len() - 1]
            } else {
                part
            };
            if self.discarding {
                self.discarding = !complete;
                continue;
            }
            if bytes.len() > limit - self.carry.len() {
                let preview = self.carry.iter().chain(bytes).take(64).copied().collect();
                self.carry.clear();
                self.discarding = !complete;
                lines.push(Err(preview));
            } else {
                self.carry.extend_from_slice(bytes);
                if complete {
                    lines.push(Ok(std::mem::take(&mut self.carry)));
                }
            }
        }
        lines
    }
}

impl Decoder for JsonLinesDecoder {
    fn id(&self) -> &'static str {
        "json_lines"
    }

    fn feed(&mut self, ctx: &ChunkCtx<'_>) -> Option<DecodedInfo> {
        if ctx.data.is_empty() {
            return None;
        }
        let mut level = DecodeLevel::Info;
        let mut fields = Vec::new();
        let mut texts = Vec::new();
        let mut objects = 0u32;
        for line in self.lines.feed(ctx.data, 64 * 1024) {
            let line = match line {
                Ok(line) => line,
                Err(preview) => {
                    level = DecodeLevel::Error;
                    texts.push(crate::tr!(
                        "Line exceeded 64 KiB and was discarded: {}...",
                        String::from_utf8_lossy(&preview)
                    ));
                    continue;
                }
            };
            let line = match std::str::from_utf8(&line) {
                Ok(line) => line,
                Err(_) => {
                    level = DecodeLevel::Error;
                    texts.push(crate::tr!(
                        "Invalid JSON: {line}",
                        line = String::from_utf8_lossy(&line)
                    ));
                    continue;
                }
            };
            let line = line.trim_end_matches('\r').trim();
            if line.is_empty() {
                continue;
            }
            match serde_json::from_str::<serde_json::Value>(line) {
                Ok(v) => {
                    objects += 1;
                    if objects == 1 {
                        fields = flatten_fields(&v, 0);
                    }
                    texts.push(compact(&v));
                }
                Err(_) => {
                    level = DecodeLevel::Error;
                    texts.push(crate::tr!("Invalid JSON: {line}", line = line));
                }
            }
        }
        if objects == 0 && level == DecodeLevel::Info {
            // Empty or incomplete lines produce no result.
            return None;
        }
        Some(DecodedInfo {
            decoder: "json_lines".into(),
            level,
            text: texts.join("\n"),
            fields,
        })
    }
}

/// Flatten top-level scalars and one nested level (`a.b`); render arrays as bounded JSON text.
fn flatten_fields(v: &serde_json::Value, depth: u32) -> Vec<DecodeField> {
    let Some(map) = v.as_object() else {
        return vec![DecodeField {
            name: "value".into(),
            value: compact(v),
        }];
    };
    let mut out = Vec::new();
    for (k, val) in map {
        match val {
            serde_json::Value::Object(inner) if depth == 0 => {
                for (k2, v2) in inner {
                    out.push(DecodeField {
                        name: format!("{k}.{k2}"),
                        value: compact(v2),
                    });
                }
            }
            _ => out.push(DecodeField {
                name: k.clone(),
                value: compact(val),
            }),
        }
    }
    out.truncate(24); // Bound field count to keep unusually large records manageable.
    out
}

/// Compact JSON representation; remove string quotes for direct value display.
fn compact(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => {
            let t: String = s.chars().take(64).collect();
            t
        }
        other => {
            let s = serde_json::to_string(other).unwrap_or_else(|_| "?".into());
            s.chars().take(64).collect()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::Direction;

    #[test]
    fn invalid_utf8_is_not_repaired_into_valid_json() {
        let mut decoder = JsonLinesDecoder::default();
        let result = decoder.feed(&ctx(b"{\"x\":\"\xff\"}\n")).unwrap();
        assert_eq!(result.level, DecodeLevel::Error);
        assert!(result.fields.is_empty(), "{:?}", result.fields.is_empty());
        assert!(result.text.contains("Invalid JSON"));
        let result = decoder.feed(&ctx("{\"x\":\"�\"}\n".as_bytes())).unwrap();
        assert_eq!(result.level, DecodeLevel::Info);
        assert_eq!(result.fields[0].value, "�");
    }

    #[test]
    fn json_limit_is_per_line_at_transport_boundaries() {
        let mut wire = vec![b' '; 64 * 1024 - 4];
        wire.extend_from_slice(b"true\nfalse\n");
        for split in [0, 1, wire.len() - 4096, 64 * 1024 - 1, 64 * 1024] {
            let mut decoder = JsonLinesDecoder::default();
            assert!(decoder.feed(&ctx(&wire[..split])).is_none());
            let result = decoder.feed(&ctx(&wire[split..])).unwrap();
            assert_eq!(result.level, DecodeLevel::Info, "split {split}");
            assert_eq!(result.text, "true\nfalse");
        }
    }

    #[test]
    fn json_overflow_discards_until_newline_and_recovers() {
        let mut decoder = JsonLinesDecoder::default();
        let result = decoder.feed(&ctx(&vec![b'x'; 64 * 1024 + 1])).unwrap();
        assert_eq!(result.level, DecodeLevel::Error);
        assert!(
            decoder.lines.carry.is_empty(),
            "{:?}",
            decoder.lines.carry.is_empty()
        );
        assert!(decoder.feed(&ctx(b"true")).is_none());
        let result = decoder.feed(&ctx(b"\nfalse\n")).unwrap();
        assert_eq!(result.level, DecodeLevel::Info);
        assert_eq!(result.text, "false");

        let mut wire = vec![b'x'; 64 * 1024 + 1];
        wire.extend_from_slice(b"\ntrue\n");
        let result = decoder.feed(&ctx(&wire)).unwrap();
        assert_eq!(result.level, DecodeLevel::Error);
        assert!(result.text.ends_with("\ntrue"));
    }

    #[test]
    fn utf8_codepoint_split_across_transport_reads() {
        let bytes = "{\"value\":\"中文\"}\n".as_bytes();
        let split = bytes.iter().position(|&b| b >= 0x80).unwrap() + 1;
        let mut decoder = JsonLinesDecoder::default();
        assert!(decoder.feed(&ctx(&bytes[..split])).is_none());
        let decoded = decoder.feed(&ctx(&bytes[split..])).unwrap();
        assert!(
            decoded
                .fields
                .iter()
                .any(|f| f.name == "value" && f.value == "中文")
        );
    }

    fn ctx(data: &[u8]) -> ChunkCtx<'_> {
        ChunkCtx {
            dir: Direction::Rx,
            t_us: 0,
            mono_us: 0,
            data,
        }
    }

    #[test]
    fn hex_转储() {
        let mut d = HexDumpDecoder;
        let info = d.feed(&ctx(b"AT\r\n")).unwrap();
        assert!(info.text.contains("41 54 0D 0A"));
        assert!(info.text.contains("AT.."));
    }

    #[test]
    fn ascii_可视化与二进制嫌疑() {
        let mut d = AsciiLinesDecoder::default();
        let info = d.feed(&ctx(b"AT\r\nOK")).unwrap();
        assert_eq!(info.text, "AT␍␊OK");
        assert_eq!(info.level, DecodeLevel::Info);
        let bin = d.feed(&ctx(&[0x00, 0xFF, 0x81, 0x02, 0xFE, 0x11])).unwrap();
        assert_eq!(bin.level, DecodeLevel::Warn);
    }

    #[test]
    fn utf8_宽松() {
        let mut d = Utf8LossyDecoder;
        let info = d.feed(&ctx(b"hello")).unwrap();
        assert_eq!(info.text, "hello");
        let bad = d.feed(&ctx(&[0xFF, 0xFE])).unwrap();
        assert_eq!(bad.level, DecodeLevel::Warn);
    }

    #[test]
    fn json_行解析与半行保留() {
        let mut d = JsonLinesDecoder::default();
        let info = d.feed(&ctx(b"{\"level\":\"warn\",\"code\":7}\n")).unwrap();
        assert_eq!(info.level, DecodeLevel::Info);
        assert!(info.text.contains("\"level\":\"warn\""), "{}", info.text);
        assert!(
            info.fields
                .iter()
                .any(|f| f.name == "level" && f.value == "warn")
        );
        assert!(
            info.fields
                .iter()
                .any(|f| f.name == "code" && f.value == "7")
        );
        // Partial object without a newline: retain it and return no result.
        assert!(d.feed(&ctx(b"{\"a\":1,\"b\":{\"c\":2}")).is_none());
        // The next frame completes the object; flatten the nested field as b.c.
        let info = d.feed(&ctx(b"}\n")).unwrap();
        assert_eq!(info.level, DecodeLevel::Info);
        assert!(info.fields.iter().any(|f| f.name == "a" && f.value == "1"));
        assert!(
            info.fields
                .iter()
                .any(|f| f.name == "b.c" && f.value == "2")
        );
        // Invalid JSON line produces Error.
        let info = d.feed(&ctx(b"oops\n")).unwrap();
        assert_eq!(info.level, DecodeLevel::Error);
        assert!(info.text.contains("Invalid JSON"));
    }
}
