/*
 * flattencom - CLI Cmd Autobaud
 *
 * Samples candidate baud rates and reports received-text quality estimates.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

//! `autobaud`: sample and score candidate baud rates.

use std::io::Write;
use std::sync::Arc;
use std::time::{Duration, Instant};

use flattencom_core::autobaud::{self, BaudScore};
use flattencom_core::config::{ConfigPatch, SerialConfig};
use flattencom_core::ids::SessionId;
use flattencom_core::session::SessionHandle;
use flattencom_core::transport::TransportRegistry;

use super::CmdError;
use crate::exit_code;

/// Streaming equivalent of core `score_bytes`: all observed bytes contribute to
/// the score and totals, but only the first 32 printable characters are retained.
struct Sample {
    bytes: u64,
    frames: u64,
    printable: u64,
    nul: u64,
    newlines: u64,
    crlf: u64,
    previous_cr: bool,
    seen: [bool; 256],
    preview: String,
}

impl Sample {
    fn new() -> Self {
        Self {
            bytes: 0,
            frames: 0,
            printable: 0,
            nul: 0,
            newlines: 0,
            crlf: 0,
            previous_cr: false,
            seen: [false; 256],
            preview: String::with_capacity(32),
        }
    }

    fn push(&mut self, data: &[u8]) {
        self.frames += 1;
        self.bytes += data.len() as u64;
        for &byte in data {
            self.printable +=
                u64::from((0x20..=0x7e).contains(&byte) || matches!(byte, b'\r' | b'\n' | b'\t'));
            self.nul += u64::from(byte == 0);
            self.newlines += u64::from(byte == b'\n');
            self.crlf += u64::from(byte == b'\n' && self.previous_cr);
            self.previous_cr = byte == b'\r';
            self.seen[usize::from(byte)] = true;
            if self.preview.len() < 32 && (0x20..0x7f).contains(&byte) {
                self.preview.push(char::from(byte));
            }
        }
    }

    fn score(&self) -> f64 {
        if self.bytes == 0 {
            return 0.0;
        }
        let len = self.bytes as f64;
        let diversity = self.seen.iter().filter(|&&seen| seen).count() as f64 / 64.0;
        // core counts the final unterminated line too, including a trailing CR.
        let cr_end =
            (self.crlf + u64::from(self.previous_cr)) as f64 / (self.newlines as f64 + 1.0);
        self.printable as f64 / len * 0.5
            + cr_end * 0.2
            + diversity.min(1.0) * 0.2
            + (1.0 - self.nul as f64 / len) * 0.1
    }
}

/// Open, sample and score each rate, select the best candidate, then verify.
pub fn run(
    port: &str,
    duration_ms: u64,
    bauds: Option<&str>,
    json: bool,
    out: &mut impl Write,
) -> Result<i32, CmdError> {
    let candidates: Vec<u32> = match bauds {
        Some(s) => s
            .split(',')
            .filter_map(|t| t.trim().parse().ok())
            .filter(|&b| b > 0)
            .collect(),
        None => autobaud::candidate_bauds(),
    };
    if candidates.is_empty() {
        return Err(CmdError::with_code(
            "No valid baud rates in --bauds",
            exit_code::ARGS,
        ));
    }

    let registry = Arc::new(TransportRegistry::new());
    let cfg = SerialConfig::new(port);
    let session =
        SessionHandle::open(SessionId::new(), cfg, registry).map_err(CmdError::from_core)?;

    let mut scores: Vec<BaudScore> = Vec::new();
    for baud in &candidates {
        session
            .configure(ConfigPatch {
                baud: Some(*baud),
                ..Default::default()
            })
            .map_err(CmdError::from_core)?;
        session.flush_rx().map_err(CmdError::from_core)?;
        // Sampling window
        let start = Instant::now();
        let budget = Duration::from_millis(duration_ms);
        let mut sample = Sample::new();
        let mut since: Option<u64> = None;
        while start.elapsed() < budget {
            let rev = session.change_rev();
            let page = session.read_frames(since, 1 << 20);
            since = Some(page.next_seq);
            for f in &page.frames {
                if f.dir == flattencom_core::frame::Direction::Rx {
                    sample.push(&f.data);
                }
            }
            session.wait_change(
                rev,
                budget
                    .saturating_sub(start.elapsed())
                    .min(Duration::from_millis(20)),
            );
        }
        scores.push(BaudScore {
            baud: *baud,
            score: sample.score(),
            rx_frames: sample.frames,
            rx_bytes: sample.bytes,
            best_text: sample.preview,
        });
    }

    autobaud::rank(&mut scores);
    let best = &scores[0];

    if json {
        let v = serde_json::json!({
            "best": best,
            "scores": scores,
            "locked": best.score >= 0.6,
        });
        writeln!(out, "{v}").map_err(|e| CmdError::new(e.to_string()))?;
    } else {
        writeln!(
            out,
            "{}",
            flattencom_core::tr!("{:<10} {:<8} {:<8} Sample", "Baud", "Score", "Frames")
        )
        .map_err(|e| CmdError::new(e.to_string()))?;
        for s in &scores {
            writeln!(
                out,
                "{:<10} {:<8.3} {:<8} {}",
                s.baud, s.score, s.rx_frames, s.best_text
            )
            .map_err(|e| CmdError::new(e.to_string()))?;
        }
    }

    // Apply the best candidate when its score meets the threshold.
    if best.score >= 0.6 {
        session
            .configure(ConfigPatch {
                baud: Some(best.baud),
                ..Default::default()
            })
            .map_err(CmdError::from_core)?;
        if !json {
            writeln!(
                out,
                "{}",
                flattencom_core::tr!(
                    "Estimated baud rate: {}, score: {:.3}",
                    best.baud,
                    best.score
                )
            )
            .map_err(|e| CmdError::new(e.to_string()))?;
        }
    } else if !json {
        writeln!(
            out,
            "{}",
            flattencom_core::tr!(
                "Cannot determine baud rate. Check that the device is transmitting text."
            )
        )
        .map_err(|e| CmdError::new(e.to_string()))?;
    }
    session.close();
    out.flush().map_err(|e| CmdError::new(e.to_string()))?;
    Ok(exit_code::OK)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streaming_scores_match_core_at_every_split() {
        let all_bytes: Vec<u8> = (0..=255).collect();
        for data in [
            &b""[..],
            b"AT+CSQ\r\n+CSQ: 23,0\r\n\r\nOK\r\n",
            b"a\rb\n\r",
            &all_bytes,
        ] {
            for split in 0..=data.len() {
                let mut sample = Sample::new();
                sample.push(&data[..split]);
                sample.push(&data[split..]);
                assert!((sample.score() - autobaud::score_bytes(data)).abs() < 1e-12);
                assert_eq!(sample.bytes, data.len() as u64);
                assert_eq!(sample.frames, 2);
            }
        }
        let mut text = Sample::new();
        text.push(b"AT+CSQ\r\n+CSQ: 23,0\r\n\r\nOK\r\n");
        let mut noise = Sample::new();
        noise.push(&[0, 0xff, 0x81, 0, 0xfe, 0x11, 0, 0xc3]);
        assert!(text.score() > 0.7);
        assert!(noise.score() < 0.45);
    }

    #[test]
    fn long_sample_keeps_totals_without_growing_preview() {
        let mut sample = Sample::new();
        let bytes = [b'A'; 4096];
        for _ in 0..4096 {
            sample.push(&bytes);
        }
        assert_eq!(sample.bytes, 16 * 1024 * 1024);
        assert_eq!(sample.frames, 4096);
        assert_eq!(sample.preview, "A".repeat(32));
        assert_eq!(sample.preview.capacity(), 32);
        assert!((sample.score() - autobaud::score_bytes(b"A")).abs() < 1e-12);
    }

    #[test]
    fn output_failures_propagate() {
        struct Output(bool);
        impl Write for Output {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                if self.0 {
                    Ok(bytes.len())
                } else {
                    Err(std::io::ErrorKind::BrokenPipe.into())
                }
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Err(std::io::ErrorKind::BrokenPipe.into())
            }
        }
        for json in [false, true] {
            for flush in [false, true] {
                assert!(
                    run(
                        "virtual://echo",
                        0,
                        Some("115200"),
                        json,
                        &mut Output(flush)
                    )
                    .is_err()
                );
            }
        }
    }
}
