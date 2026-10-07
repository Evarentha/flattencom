/*
 * flattencom - Core Decode Nmea0183
 *
 * Reassembles NMEA sentences, parses fields and verifies XOR checksums.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

//! NMEA 0183 decoder: parse `$GPGGA,...*HH` sentences and verify XOR checksums.
//!
//! Buffer split sentences across frames; pass non-`$` lines through as Warn.

use crate::decode::builtin::BoundedLines;
use crate::decode::{ChunkCtx, Decoder};
use crate::frame::{DecodeField, DecodeLevel, DecodedInfo};

/// NMEA 0183 decoder.
/// Lines may contain up to 4096 bytes before LF; oversized lines are skipped through LF.
#[derive(Debug, Default)]
pub struct NmeaDecoder {
    /// Incomplete line retained across frames.
    lines: BoundedLines,
}

/// Line-buffer limit for unterminated malformed input.
const CARRY_CAP: usize = 4096;

impl Decoder for NmeaDecoder {
    fn id(&self) -> &'static str {
        "nmea0183"
    }

    fn feed(&mut self, ctx: &ChunkCtx<'_>) -> Option<DecodedInfo> {
        if ctx.data.is_empty() {
            return None;
        }
        let mut texts = Vec::new();
        let mut fields: Vec<DecodeField> = Vec::new();
        let mut level = DecodeLevel::Info;
        let mut statements = 0u32;
        for line in self.lines.feed(ctx.data, CARRY_CAP) {
            let line = match line {
                Ok(line) => line,
                Err(preview) => {
                    level = DecodeLevel::Error;
                    texts.push(crate::tr!(
                        "Line exceeded {CARRY_CAP} bytes and was discarded: {bad:.48}...",
                        CARRY_CAP = CARRY_CAP,
                        bad = String::from_utf8_lossy(&preview)
                    ));
                    continue;
                }
            };
            // NMEA is ASCII. Never repair received bytes before verifying their checksum.
            if !line.is_ascii() {
                level = DecodeLevel::Error;
                texts.push(crate::tr!(
                    "Invalid NMEA sentence: {line}",
                    line = String::from_utf8_lossy(&line)
                ));
                continue;
            }
            let line = std::str::from_utf8(&line).expect("ASCII line");
            let line = line.trim_end_matches('\r');
            if line.is_empty() {
                continue;
            }
            if !line.starts_with('$') {
                level = if level == DecodeLevel::Error {
                    level
                } else {
                    DecodeLevel::Warn
                };
                texts.push(crate::tr!("Not an NMEA sentence: {line}", line = line));
                continue;
            }
            match parse_sentence(line) {
                Some((talker, stype, checksum_ok, checksum)) => {
                    statements += 1;
                    if !checksum_ok && level != DecodeLevel::Error {
                        level = DecodeLevel::Error;
                    }
                    if statements == 1 {
                        fields.push(DecodeField {
                            name: "talker".into(),
                            value: talker,
                        });
                        fields.push(DecodeField {
                            name: "type".into(),
                            value: stype,
                        });
                        fields.push(DecodeField {
                            name: "checksum".into(),
                            value: if checksum_ok {
                                "OK".into()
                            } else {
                                format!("BAD({checksum:02X})")
                            },
                        });
                    }
                    let mark = if checksum_ok { "" } else { "Checksum failed: " };
                    texts.push(format!("{mark}{line}"));
                }
                None => {
                    if level != DecodeLevel::Error {
                        level = DecodeLevel::Warn;
                    }
                    texts.push(crate::tr!("Invalid NMEA sentence: {line}", line = line));
                }
            }
        }
        if texts.is_empty() {
            return None;
        }
        Some(DecodedInfo {
            decoder: "nmea0183".into(),
            level,
            text: texts.join("\n"),
            fields,
        })
    }
}

/// Parse `$`, a five-character identifier, body, `*`, and two hexadecimal checksum digits.
fn parse_sentence(line: &str) -> Option<(String, String, bool, u8)> {
    let body_end = line.find('*')?;
    if !line.starts_with('$') || body_end < 6 || body_end + 3 != line.len() {
        return None;
    }
    let ident = line.get(1..6)?;
    if ident.len() < 5
        || !ident
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
    {
        return None;
    }
    let talker: String = ident[..2].to_owned();
    let stype: String = ident[2..5].to_owned();
    let checksum_hex = line.get(body_end + 1..body_end + 3)?;
    let checksum = u8::from_str_radix(checksum_hex, 16).ok()?;
    // Checksum covers bytes between $ and *.
    let xor = line
        .as_bytes()
        .iter()
        .take(body_end)
        .skip(1)
        .fold(0u8, |acc, &b| acc ^ b);
    Some((talker, stype, xor == checksum, xor))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::Direction;

    fn ctx(data: &[u8]) -> ChunkCtx<'_> {
        ChunkCtx {
            dir: Direction::Rx,
            t_us: 0,
            mono_us: 0,
            data,
        }
    }

    #[test]
    fn coalesced_sentences_and_overflow_recovery() {
        let sentence = b"$GPGGA*56\n";
        let mut decoder = NmeaDecoder::default();
        let wire = sentence.repeat(600);
        let result = decoder.feed(&ctx(&wire)).unwrap();
        assert_eq!(result.level, DecodeLevel::Info);
        assert_eq!(result.text.lines().count(), 600);

        let result = decoder.feed(&ctx(&vec![b'x'; CARRY_CAP + 1])).unwrap();
        assert_eq!(result.level, DecodeLevel::Error);
        assert!(decoder.feed(&ctx(b"$GPGGA*56")).is_none());
        let result = decoder.feed(&ctx(b"\n$GPGGA*56\n")).unwrap();
        assert_eq!(result.level, DecodeLevel::Info);
        assert_eq!(result.text, "$GPGGA*56");
    }

    #[test]
    fn malformed_sentences_do_not_consume_first_parsed_fields() {
        use crate::filter::{CompiledFilter, FieldFilter, FilterSpec};
        use crate::frame::Frame;

        let filter = CompiledFilter::compile(&FilterSpec {
            fields: vec![FieldFilter {
                name: "type".into(),
                contains: "GGA".into(),
            }],
            ..Default::default()
        })
        .unwrap();
        for prefix in ["$bad\n", "$GPGGA*ZZ\n", "$bad\n$bad\n"] {
            let wire = format!("{prefix}$GPGGA*56\n");
            for split in 0..=prefix.len() {
                let mut decoder = NmeaDecoder::default();
                let _ = decoder.feed(&ctx(&wire.as_bytes()[..split]));
                let info = decoder.feed(&ctx(&wire.as_bytes()[split..])).unwrap();
                assert!(
                    info.fields
                        .iter()
                        .any(|f| f.name == "checksum" && f.value == "OK")
                );
                if split == 0 {
                    assert_eq!(info.level, DecodeLevel::Warn);
                    assert!(info.text.contains("Invalid NMEA sentence"));
                }
                assert!(
                    filter.matches(
                        &Frame::new(0, Direction::Rx, wire.as_bytes().to_vec(), 0, 0)
                            .with_decoded(Some(info))
                    )
                );
            }
        }
    }

    #[test]
    fn non_ascii_bytes_cannot_be_repaired_into_valid_checksums() {
        for sentence in [
            b"$GPGGA,\xff*97\n".as_slice(),
            "$GPGGA,�*97\n".as_bytes(),
            b"$GPGGA,\xff*AF\n".as_slice(),
        ] {
            for split in 0..sentence.len() {
                let mut decoder = NmeaDecoder::default();
                assert!(decoder.feed(&ctx(&sentence[..split])).is_none());
                let result = decoder.feed(&ctx(&sentence[split..])).unwrap();
                assert_eq!(result.level, DecodeLevel::Error);
                assert!(result.fields.is_empty(), "{:?}", result.fields.is_empty());
                assert!(result.text.contains("Invalid NMEA sentence"));
                let next = decoder.feed(&ctx(b"$GPGGA*56\n")).unwrap();
                assert_eq!(next.level, DecodeLevel::Info);
                assert!(
                    next.fields
                        .iter()
                        .any(|f| f.name == "checksum" && f.value == "OK")
                );
            }
        }
    }

    #[test]
    fn checksum_must_terminate_sentence() {
        assert!(parse_sentence("$GPGGA*56").is_some());
        assert!(parse_sentence("$GPGGA*56garbage").is_none());
        assert!(parse_sentence("$GPGGA*56$GPGGA*56").is_none());
    }

    #[test]
    fn 解析合法语句() {
        // $GPGGA sentence with checksum 47: XOR bytes between $ and *.
        let sentence = "$GPGGA,123519,4807.038,N,01131.000,E,1,08,0.9,545.4,M,46.9,M,,*47";
        let mut d = NmeaDecoder::default();
        let info = d.feed(&ctx(format!("{sentence}\r\n").as_bytes())).unwrap();
        assert_eq!(info.level, DecodeLevel::Info);
        assert!(
            info.fields
                .iter()
                .any(|f| f.name == "type" && f.value == "GGA")
        );
        assert!(
            info.fields
                .iter()
                .any(|f| f.name == "talker" && f.value == "GP")
        );
        assert!(
            info.fields
                .iter()
                .any(|f| f.name == "checksum" && f.value == "OK")
        );
    }

    #[test]
    fn 校验失败报错() {
        let sentence = "$GPGGA,123519,4807.038,N,01131.000,E,1,08,0.9,545.4,M,46.9,M,,*00";
        let mut d = NmeaDecoder::default();
        let info = d.feed(&ctx(format!("{sentence}\r\n").as_bytes())).unwrap();
        assert_eq!(info.level, DecodeLevel::Error);
        assert!(info.text.contains("Checksum failed"));
    }

    #[test]
    fn 跨帧拆分与杂行() {
        let a = "$GPRMC,081836,A";
        let b = ",3751.65,S,14507.36,E,000.0,360.0,130998,011.3,E*62\r\n";
        let noise = "random line\r\n";
        let mut d = NmeaDecoder::default();
        assert!(d.feed(&ctx(a.as_bytes())).is_none()); // Partial sentence: no result.
        let info = d.feed(&ctx(b.as_bytes())).unwrap();
        assert!(info.text.contains("GPRMC"));
        let info = d.feed(&ctx(noise.as_bytes())).unwrap();
        assert!(info.text.contains("Not an NMEA sentence"));
    }
}
