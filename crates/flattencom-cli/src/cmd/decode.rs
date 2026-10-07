/*
 * flattencom - CLI Cmd Decode
 *
 * Decodes saved JSONL captures offline or lists available decoder implementations.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

//! `decode`: decode JSONL captures offline without hardware.
//! Stateful decoders maintain independent RX and TX streams, as in live sessions.

use std::io::Write;
use std::path::Path;

use super::CmdError;
use crate::exit_code;

/// Read, decode each frame, then print time, direction, decoded text and fields.
pub fn run(
    log: &Path,
    decoder: Option<&str>,
    list: bool,
    json: bool,
    out: &mut impl Write,
) -> Result<i32, CmdError> {
    if list || decoder.is_none() {
        // List the decoder catalog.
        let mut items: Vec<(&str, &str)> = flattencom_core::decode::DECODER_IDS
            .iter()
            .filter_map(|id| {
                flattencom_core::decode::DecoderRegistry::describe(id).map(|d| (*id, d))
            })
            .collect();
        items.sort_unstable();
        if json && list {
            writeln!(out, "{}", serde_json::json!({"decoders": items}))
                .map_err(|e| CmdError::new(e.to_string()))?;
            out.flush().map_err(|e| CmdError::new(e.to_string()))?;
            return Ok(exit_code::OK);
        }
        writeln!(
            out,
            "{}",
            flattencom_core::tr!("Available decoders (--decoder):")
        )
        .map_err(|e| CmdError::new(e.to_string()))?;
        for (id, desc) in items {
            writeln!(out, "  {id:<14} {desc}").map_err(|e| CmdError::new(e.to_string()))?;
        }
        if !list {
            writeln!(out).map_err(|e| CmdError::new(e.to_string()))?;
            return Err(CmdError::with_code(
                "Specify --decoder <name>, or use --list to list decoders",
                exit_code::ARGS,
            ));
        }
        out.flush().map_err(|e| CmdError::new(e.to_string()))?;
        return Ok(exit_code::OK);
    }
    let spec = flattencom_core::decode::DecoderSpec {
        // decoder.is_none() returned earlier; ascii_lines is only a fallback for type completeness.
        name: decoder.unwrap_or("ascii_lines").to_owned(),
        options: serde_json::Value::Null,
    };
    let mut rx_decoder =
        flattencom_core::decode::DecoderRegistry::build(&spec).map_err(CmdError::from_core)?;
    let mut tx_decoder =
        flattencom_core::decode::DecoderRegistry::build(&spec).map_err(CmdError::from_core)?;
    let mut frames = flattencom_core::record::stream_log(log).map_err(CmdError::from_core)?;
    for f in frames.by_ref() {
        let f = f.map_err(CmdError::from_core)?;
        let ctx = flattencom_core::decode::ChunkCtx {
            dir: f.dir,
            t_us: f.t_us,
            mono_us: f.mono_us,
            data: &f.data,
        };
        let decoder = if f.dir == flattencom_core::frame::Direction::Rx {
            &mut rx_decoder
        } else {
            &mut tx_decoder
        };
        if let Some(info) = decoder.feed(&ctx) {
            if json {
                writeln!(
                    out,
                    "{}",
                    serde_json::json!({"seq": f.seq, "decoded": info})
                )
                .map_err(|e| CmdError::new(e.to_string()))?;
                continue;
            }
            let t = flattencom_core::timefmt::fmt_clock_us(f.t_us);
            let dir = f.dir.as_char();
            let fields = info
                .fields
                .iter()
                .map(|fd| format!("{}={}", fd.name, fd.value))
                .collect::<Vec<_>>()
                .join(" ");
            writeln!(
                out,
                "[{t}] {dir} {text}{fields}",
                text = info.text,
                fields = if fields.is_empty() {
                    String::new()
                } else {
                    format!(" | {fields}")
                }
            )
            .map_err(|e| CmdError::new(e.to_string()))?;
        }
    }
    let skipped = frames.skipped();
    if skipped > 0 {
        writeln!(
            std::io::stderr().lock(),
            "{}",
            flattencom_core::tr!("Skipped {skipped} invalid records", skipped = skipped)
        )
        .map_err(|e| CmdError::new(e.to_string()))?;
    }
    out.flush().map_err(|e| CmdError::new(e.to_string()))?;
    Ok(exit_code::OK)
}

#[cfg(test)]
mod tests {
    use super::*;
    use flattencom_core::frame::{Direction, Frame};

    #[test]
    fn decode_emits_first_frame_before_reading_file_tail() {
        struct AppendOnOutput {
            log: std::fs::File,
            tail: Option<Vec<u8>>,
            output: Vec<u8>,
        }
        impl Write for AppendOnOutput {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                if let Some(tail) = self.tail.take() {
                    self.log.write_all(&tail)?;
                }
                self.output.write(bytes)
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut log = tempfile::NamedTempFile::new().unwrap();
        let first = Frame::new(1, Direction::Rx, b"first".to_vec(), 0, 0);
        let last = Frame::new(2, Direction::Rx, b"last".to_vec(), 0, 1);
        writeln!(log, "{}", serde_json::to_string(&first).unwrap()).unwrap();
        let mut tail = serde_json::to_vec(&last).unwrap();
        tail.push(b'\n');
        let mut output = AppendOnOutput {
            log: std::fs::OpenOptions::new()
                .append(true)
                .open(log.path())
                .unwrap(),
            tail: Some(tail),
            output: Vec::new(),
        };
        run(log.path(), Some("ascii_lines"), false, true, &mut output).unwrap();
        let results: Vec<serde_json::Value> = String::from_utf8(output.output)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0]["seq"], 1);
        assert_eq!(results[1]["seq"], 2);
    }

    #[test]
    fn offline_decoders_keep_interleaved_directions_separate() {
        let mut modbus = vec![1, 3, 2, 0, 10];
        modbus
            .extend_from_slice(&flattencom_core::decode::modbus_rtu::crc16(&modbus).to_le_bytes());
        for (name, rx, tx) in [
            (
                "json_lines",
                b"{\"rx\":1}\n".to_vec(),
                b"{\"tx\":2}\n".to_vec(),
            ),
            ("nmea0183", b"$GPGGA*56\n".to_vec(), b"$GPGGA*56\n".to_vec()),
            ("modbus_rtu", modbus.clone(), modbus),
        ] {
            let mut log = tempfile::NamedTempFile::new().unwrap();
            for (seq, dir, bytes) in [
                (1, Direction::Rx, &rx[..3]),
                (2, Direction::Tx, &tx[..3]),
                (3, Direction::Rx, &rx[3..]),
                (4, Direction::Tx, &tx[3..]),
            ] {
                let frame = Frame::new(seq, dir, bytes.to_vec(), 0, seq);
                writeln!(log, "{}", serde_json::to_string(&frame).unwrap()).unwrap();
            }
            let mut output = Vec::new();
            run(log.path(), Some(name), false, true, &mut output).unwrap();
            let results: Vec<serde_json::Value> = String::from_utf8(output)
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            assert_eq!(results.len(), 2, "{name}: {results:?}");
            for (result, seq) in results.iter().zip([3, 4]) {
                assert_eq!(result["seq"], seq, "{name}");
                assert_eq!(result["decoded"]["level"], "info", "{name}");
            }
            if name == "json_lines" {
                assert_eq!(results[0]["decoded"]["text"], "{\"rx\":1}");
                assert_eq!(results[1]["decoded"]["text"], "{\"tx\":2}");
            }
        }
    }

    #[test]
    fn output_errors_are_propagated() {
        struct BrokenOutput {
            flush_only: bool,
        }
        impl Write for BrokenOutput {
            fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
                if self.flush_only {
                    Ok(data.len())
                } else {
                    Err(std::io::ErrorKind::BrokenPipe.into())
                }
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Err(std::io::ErrorKind::BrokenPipe.into())
            }
        }
        let mut log = tempfile::NamedTempFile::new().unwrap();
        writeln!(
            log,
            "{}",
            serde_json::to_string(&Frame::new(1, Direction::Rx, b"hello".to_vec(), 0, 0)).unwrap()
        )
        .unwrap();
        for json in [false, true] {
            assert!(
                run(
                    log.path(),
                    None,
                    true,
                    json,
                    &mut BrokenOutput { flush_only: false }
                )
                .is_err()
            );
            for flush_only in [false, true] {
                assert!(
                    run(
                        log.path(),
                        Some("ascii_lines"),
                        false,
                        json,
                        &mut BrokenOutput { flush_only }
                    )
                    .is_err()
                );
            }
        }
    }
}
