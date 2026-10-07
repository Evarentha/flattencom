/*
 * flattencom - Core Transcript
 *
 * Renders readable RX lines, escaped TX commands and timestamped annotations across chunks.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

//! Human-readable capture rendering, independent of transport chunk boundaries.
use crate::frame::{Direction, Frame};
use std::fmt::Write;

/// UTC timestamp used in readable captures.
pub fn timestamp(t_us: i64) -> String {
    chrono::DateTime::from_timestamp_micros(t_us).map_or_else(
        || t_us.to_string(),
        |t| t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
    )
}

/// Stateful text renderer. RX lines span reads; TX and annotations have explicit headers.
#[derive(Debug, Default)]
pub struct Transcript {
    pending: Vec<u8>,
    after_cr: bool,
    line_open: bool,
    column: usize,
    escape: u8,
    escape_len: usize,
    last_header: String,
}

impl Transcript {
    /// Finish the old RX fragment before consuming a replacement connection.
    pub(crate) fn rx_boundary(&mut self) -> String {
        let tail = self.finish();
        *self = Self::default();
        tail
    }

    /// Render a captured frame; TX never feeds the RX decoder.
    pub fn frame(&mut self, frame: &Frame) -> String {
        let header = format!("[{} RX #{}] ", timestamp(frame.t_us), frame.seq);
        if frame.dir == Direction::Tx {
            let mut output = self.interrupt();
            let source = display_bytes(frame.source.as_deref().unwrap_or("unknown").as_bytes());
            let _ = writeln!(
                output,
                "[{} TX #{} source={} bytes={}]",
                timestamp(frame.t_us),
                frame.seq,
                source,
                frame.len()
            );
            // Commands are shown as text with explicit escaped line endings; binary is reversible HEX escapes.
            output.push_str("    ");
            output.push_str(&display_bytes(&frame.data));
            output.push('\n');
            return output;
        }
        self.last_header.clone_from(&header);
        self.pending.extend_from_slice(&frame.data);
        let mut text = String::new();
        loop {
            match std::str::from_utf8(&self.pending) {
                Ok(valid) => {
                    text.push_str(valid);
                    self.pending.clear();
                    break;
                }
                Err(error) => {
                    let valid = error.valid_up_to();
                    text.push_str(
                        std::str::from_utf8(&self.pending[..valid]).expect("valid prefix"),
                    );
                    self.pending.drain(..valid);
                    if let Some(size) = error.error_len() {
                        for byte in self.pending.drain(..size) {
                            let _ = write!(text, "\\x{byte:02X}");
                        }
                    } else {
                        break;
                    }
                }
            }
        }
        let mut output = String::new();
        for ch in text.chars() {
            if self.escape != 0 {
                self.escape_len += 1;
                self.escape = match (self.escape, ch) {
                    (2, '@'..='~') | (3, '\u{7}') | (4, '\\') => 0,
                    (3, '\u{1b}') => 4,
                    (1, '[') | (2, _) => 2,
                    (1, ']') | (3 | 4, _) => 3,
                    _ => 0,
                };
                if self.escape_len > 4096 {
                    self.escape = 0;
                }
                continue;
            }
            if ch == '\u{1b}' {
                self.escape = 1;
                self.escape_len = 0;
                continue;
            }
            if ch == '\n' && self.after_cr {
                self.after_cr = false;
                continue;
            }
            self.after_cr = ch == '\r';
            if !self.line_open {
                output.push_str(&header);
                self.line_open = true;
                self.column = 0;
            }
            if ch == '\r' || ch == '\n' {
                output.push('\n');
                self.line_open = false;
                self.column = 0;
            } else {
                if self.column >= 16384 {
                    output.push('\n');
                    output.push_str(&header);
                    self.column = 0;
                }
                if ch.is_control() && ch != '\t' {
                    let _ = write!(output, "\\u{{{:04X}}}", u32::from(ch));
                } else {
                    output.push(ch);
                }
                self.column += 1;
            }
        }
        output
    }

    fn interrupt(&mut self) -> String {
        if self.line_open {
            self.line_open = false;
            self.column = 0;
            "\n".into()
        } else {
            String::new()
        }
    }

    /// Insert a timestamped event without mixing it into received text.
    pub fn annotation(
        &mut self,
        t_us: i64,
        kind: &str,
        label: &str,
        source: &str,
        seq: Option<u64>,
    ) -> String {
        let mut output = self.interrupt();
        let _ = writeln!(
            output,
            "[{} {} anchor={} source={}] {}",
            timestamp(t_us),
            kind.escape_default(),
            seq.map_or_else(|| "none".into(), |s| s.to_string()),
            display_bytes(source.as_bytes()),
            display_bytes(label.as_bytes())
        );
        output
    }

    /// Finalize an incomplete byte sequence at close or at the end of an export.
    pub fn finish(&mut self) -> String {
        let mut output = String::new();
        if !self.pending.is_empty() {
            if !self.line_open {
                output.push_str(&self.last_header);
                self.line_open = true;
            }
            for byte in self.pending.drain(..) {
                let _ = write!(output, "\\x{byte:02X}");
            }
        }
        output.push_str(&self.interrupt());
        output
    }
}

/// Single-line representation for sent data, with explicit control characters.
pub fn display_bytes(data: &[u8]) -> String {
    let mut output = String::new();
    let mut remaining = data;
    while !remaining.is_empty() {
        let (valid, invalid) = match std::str::from_utf8(remaining) {
            Ok(_) => (remaining.len(), 0),
            Err(e) => (
                e.valid_up_to(),
                e.error_len().unwrap_or(remaining.len() - e.valid_up_to()),
            ),
        };
        for ch in std::str::from_utf8(&remaining[..valid])
            .expect("valid prefix")
            .chars()
        {
            match ch {
                '\r' => output.push_str("\\r"),
                '\n' => output.push_str("\\n"),
                '\t' => output.push_str("\\t"),
                '\\' => output.push_str("\\\\"),
                ch if ch.is_control() => {
                    let _ = write!(output, "\\u{{{:04X}}}", u32::from(ch));
                }
                ch => output.push(ch),
            }
        }
        for byte in &remaining[valid..valid + invalid] {
            let _ = write!(output, "\\x{byte:02X}");
        }
        remaining = &remaining[valid + invalid..];
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn annotations_and_tx_preserve_split_rx_utf8_and_crlf_state() {
        let mut render = Transcript::default();
        let mut text = render.frame(&Frame::new(0, Direction::Rx, vec![0xe4], 0, 0));
        text += &render.annotation(1, "MARK", "checkpoint\n", "test", Some(0));
        text += &render.frame(&Frame::new(1, Direction::Tx, vec![0xff, b'\n'], 1, 1));
        text += &render.frame(&Frame::new(2, Direction::Rx, vec![0xb8, 0xad, b'\r'], 2, 2));
        text += &render.annotation(3, "MARK", "after CR", "test", Some(2));
        text += &render.frame(&Frame::new(3, Direction::Rx, b"\nnext\n".to_vec(), 3, 3));
        text += &render.finish();
        assert!(text.contains("checkpoint\\n\n"), "{text}");
        assert!(text.contains("    \\xFF\\n\n"), "{text}");
        assert!(text.contains("中\n"), "{text}");
        assert!(text.contains("next\n"), "{text}");
        assert_eq!(text.matches(" RX #").count(), 2, "{text}");
        assert!(
            render.finish().is_empty(),
            "{:?}",
            render.finish().is_empty()
        );
    }

    #[test]
    fn ansi_sequences_and_incomplete_utf8_tail_span_arbitrary_chunks() {
        let input = b"\x1b[31mred\x1b[0m\x1b]0;title\x07\r\n\xe4\xb8";
        for split in 0..=input.len() {
            let mut render = Transcript::default();
            let mut text =
                render.frame(&Frame::new(0, Direction::Rx, input[..split].to_vec(), 0, 0));
            text += &render.frame(&Frame::new(1, Direction::Rx, input[split..].to_vec(), 1, 1));
            text += &render.finish();
            assert!(text.contains("red\n"), "split={split}: {text}");
            assert!(text.ends_with("\\xE4\\xB8\n"), "split={split}: {text}");
            assert!(!text.contains("title"), "split={split}: {text}");
            assert!(!text.contains('\x1b'), "split={split}: {text}");
            assert_eq!(text.matches(" RX #").count(), 2, "split={split}: {text}");
            assert!(
                render.finish().is_empty(),
                "{:?}",
                render.finish().is_empty()
            );
        }
    }

    #[test]
    fn split_utf8_crlf_and_interleaved_commands_remain_readable() {
        let bytes = "中文\r\nsecond line\n".as_bytes();
        for split in 0..=bytes.len() {
            let mut render = Transcript::default();
            let mut text =
                render.frame(&Frame::new(0, Direction::Rx, bytes[..split].to_vec(), 0, 0));
            text += &render.frame(&Frame::new(1, Direction::Rx, bytes[split..].to_vec(), 1, 1));
            text += &render.finish();
            assert!(text.contains("中文\n"), "{text}");
            assert!(text.contains("second line\n"));
            assert_eq!(text.matches(" RX #").count(), 2, "{text}");
        }
        let mut render = Transcript::default();
        let mut text = render.frame(&Frame::new(0, Direction::Rx, b"prompt> ".to_vec(), 0, 0));
        let mut tx = Frame::new(1, Direction::Tx, b"ls\r\n".to_vec(), 1, 1);
        tx.source = Some("GUI#1".into());
        text += &render.frame(&tx);
        text += &render.frame(&Frame::new(
            2,
            Direction::Rx,
            b"\x1b[31merror\x1b[0m\n".to_vec(),
            2,
            2,
        ));
        text += &render.annotation(3, "MARK", "pressed button", "GUI#1", Some(2));
        assert!(text.contains("source=GUI#1"));
        assert!(text.contains("ls\\r\\n"));
        assert!(!text.contains('\u{1b}'));
        assert!(text.contains("pressed button"));
        assert_eq!(display_bytes(&[0xff, 0, 13, 10]), "\\xFF\\u{0000}\\r\\n");
    }
}
