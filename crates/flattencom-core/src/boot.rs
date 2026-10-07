/*
 * flattencom - Core Boot
 *
 * Recognizes fragmented boot banners and groups normal bootloader-to-kernel transitions.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

//! Bounded, chunk-independent boot banner recognition on the capture path.
use crate::frame::{Direction, Frame};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

/// A probable boot based on an explicit loader/kernel banner.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BootEvent {
    /// First chunk containing the banner.
    pub seq: u64,
    /// Host receive timestamp.
    pub t_us: i64,
    /// Session-local boot count.
    pub number: u64,
    /// Monotonic host time since the previous boot.
    pub interval_ms: Option<u64>,
    /// Observed banner, for evidence.
    pub banner: String,
}

#[derive(Default)]
pub(crate) struct BootDetector {
    line: Vec<u8>,
    anchor: Option<(u64, i64, u64)>,
    stage: u8,
    last_mono: Option<u64>,
    count: u64,
    events: VecDeque<BootEvent>,
    escape: u8,
    escape_len: usize,
}

impl BootDetector {
    pub(crate) fn connection_boundary(&mut self) {
        self.line.clear();
        self.anchor = None;
        self.stage = 0;
        self.escape = 0;
        self.escape_len = 0;
    }
    pub fn events(&self) -> Vec<BootEvent> {
        self.events.iter().cloned().collect()
    }
    pub fn feed(&mut self, frame: &Frame) -> Vec<BootEvent> {
        let mut found = Vec::new();
        if frame.dir != Direction::Rx {
            return found;
        }
        for &byte in frame.data.iter() {
            if self.escape != 0 {
                self.escape_len += 1;
                self.escape = match (self.escape, byte) {
                    (2, 0x40..=0x7e) | (3, 7) | (4, b'\\') => 0,
                    (3, 27) => 4,
                    (1, b'[') | (2, _) => 2,
                    (1, b']') | (3 | 4, _) => 3,
                    _ => 0,
                };
                if self.escape_len > 4096 {
                    self.escape = 0;
                }
                continue;
            }
            if byte == 27 {
                self.escape = 1;
                self.escape_len = 0;
                continue;
            }
            if byte != b'\r' && byte != b'\n' {
                self.anchor
                    .get_or_insert((frame.seq, frame.t_us, frame.mono_us));
                if self.line.len() < 4096 {
                    self.line.push(byte);
                }
                continue;
            }
            let text = String::from_utf8_lossy(&self.line);
            let text = text.trim();
            let text = if text.starts_with('[') {
                text.split_once(']').map_or(text, |(_, s)| s.trim_start())
            } else {
                text
            };
            let stage = if text.starts_with("U-Boot SPL ") || text.starts_with("ESP-ROM:") {
                1
            } else if text.starts_with("U-Boot ")
                || (text.starts_with("rst:0x") && text.contains("boot:"))
            {
                2
            } else if text.starts_with("Linux version ")
                || text.starts_with("*** Booting Zephyr OS")
            {
                3
            } else {
                0
            };
            if stage != 0
                && let Some((seq, t_us, mono)) = self.anchor
            {
                // Forward loader→kernel stages are one boot. An equal or earlier
                // stage is a new boot, even in a sub-second reset loop.
                let continuation = self.stage != 0
                    && stage > self.stage
                    && self
                        .last_mono
                        .is_some_and(|last| mono.saturating_sub(last) < 120_000_000);
                if !continuation {
                    self.count += 1;
                    let event = BootEvent {
                        seq,
                        t_us,
                        number: self.count,
                        interval_ms: self.last_mono.map(|last| mono.saturating_sub(last) / 1000),
                        banner: text.to_owned(),
                    };
                    self.last_mono = Some(mono);
                    self.events.push_back(event.clone());
                    while self.events.len() > 1000 {
                        self.events.pop_front();
                    }
                    found.push(event);
                }
                self.stage = stage;
            }
            self.line.clear();
            self.anchor = None;
        }
        found
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fragmented_multistage_boot_and_fast_reset_loop() {
        let mut detector = BootDetector::default();
        let bytes = b"\x1b[32mU-Boot SPL 2026\x1b[0m\r\nU-Boot 2026\n[ 0.0] Linux version 6.12\nLinux version 6.12\n";
        for (seq, byte) in bytes.iter().enumerate() {
            detector.feed(&Frame::new(
                seq as u64,
                Direction::Rx,
                vec![*byte],
                1000 + i64::try_from(seq).unwrap(),
                seq as u64 * 1000,
            ));
        }
        let events = detector.events();
        assert_eq!(events.len(), 2);
        assert_eq!(events[1].number, 2);
        assert!(events[1].interval_ms.unwrap() < 1000);
        detector.feed(&Frame::new(
            200,
            Direction::Tx,
            b"Linux version 6.12\n".to_vec(),
            2000,
            2000,
        ));
        assert_eq!(detector.events().len(), 2);
    }
}
