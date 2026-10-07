/*
 * flattencom - Core Stats
 *
 * Tracks rolling transfer rates, error categories and consistent session statistics.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

//! Rate measurement and statistics snapshots.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Sliding-window rate measurement; default one second, amortized O(1) insertion.
///
/// Window averages are easier to interpret than EWMA for burst/idle serial traffic,
/// matching the expected behavior of debugging instruments.
#[derive(Debug)]
pub struct RateMeter {
    window: Duration,
    samples: VecDeque<(Instant, u64)>,
}

impl RateMeter {
    /// Construct a meter with the given window.
    #[must_use]
    pub fn new(window: Duration) -> Self {
        Self {
            window,
            samples: VecDeque::new(),
        }
    }

    /// Append a byte-count sample.
    pub fn push(&mut self, now: Instant, bytes: u64) {
        self.samples.push_back((now, bytes));
        self.trim(now);
    }

    /// Current bytes per second.
    #[must_use]
    pub fn rate_bps(&self, now: Instant) -> f64 {
        let cutoff = now.checked_sub(self.window);
        let sum: u64 = self
            .samples
            .iter()
            .filter(|(t, _)| cutoff.is_none_or(|c| *t >= c))
            .map(|(_, n)| *n)
            .sum();
        sum as f64 / self.window.as_secs_f64()
    }

    /// Remove expired samples.
    fn trim(&mut self, now: Instant) {
        let cutoff = now.checked_sub(self.window);
        if let Some(c) = cutoff {
            while let Some((t, _)) = self.samples.front() {
                if *t < c {
                    self.samples.pop_front();
                } else {
                    break;
                }
            }
        }
    }
}

/// Error counts classified from driver diagnostic text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
pub struct ErrorStats {
    /// Framing errors.
    pub framing: u64,
    /// Parity errors.
    pub parity: u64,
    /// Buffer overrun errors.
    pub overrun: u64,
    /// Other.
    pub other: u64,
}

impl ErrorStats {
    /// Classify errors heuristically from diagnostic text.
    pub fn record(&mut self, raw: &str) {
        let l = raw.to_ascii_lowercase();
        if l.contains("framing") || l.contains("frame") {
            self.framing += 1;
        } else if l.contains("parity") {
            self.parity += 1;
        } else if l.contains("overrun") || l.contains("overflow") {
            self.overrun += 1;
        } else {
            self.other += 1;
        }
    }

    /// Total error count.
    #[must_use]
    pub fn total(&self) -> u64 {
        self.framing + self.parity + self.overrun + self.other
    }
}

/// Consistent session statistics snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SessionStats {
    /// Total received bytes.
    pub rx_bytes: u64,
    /// Total transmitted bytes.
    pub tx_bytes: u64,
    /// Total received frames.
    pub rx_frames: u64,
    /// Total transmitted frames.
    pub tx_frames: u64,
    /// Reconnect count.
    pub reconnects: u64,
    /// Cumulative frames evicted from the ring buffer.
    pub dropped_rx: u64,
    /// Error counts by category.
    pub errors: ErrorStats,
    /// Receive rate in bytes per second over a one-second window.
    pub rx_rate_bps: f64,
    /// Transmit rate in bytes per second over a one-second window.
    pub tx_rate_bps: f64,
    /// Buffer usage.
    pub buffer: crate::store::BufferLevel,
    /// Recording paths and failures, independent of display pause/filter state.
    #[serde(default)]
    pub recording: crate::record::RecordingStatus,
}

impl Default for SessionStats {
    fn default() -> Self {
        Self {
            rx_bytes: 0,
            tx_bytes: 0,
            rx_frames: 0,
            tx_frames: 0,
            reconnects: 0,
            dropped_rx: 0,
            errors: ErrorStats::default(),
            rx_rate_bps: 0.0,
            tx_rate_bps: 0.0,
            buffer: crate::store::BufferLevel::none(),
            recording: crate::record::RecordingStatus::default(),
        }
    }
}

/// Mutable internal counters, separate from the public [`SessionStats`] snapshot.
#[derive(Debug)]
pub(crate) struct Counters {
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub rx_frames: u64,
    pub tx_frames: u64,
    pub reconnects: u64,
    pub rx_rate: RateMeter,
    pub tx_rate: RateMeter,
    pub errors: ErrorStats,
}

impl Counters {
    /// Create counters with a one-second rate window.
    pub(crate) fn new() -> Self {
        Self {
            rx_bytes: 0,
            tx_bytes: 0,
            rx_frames: 0,
            tx_frames: 0,
            reconnects: 0,
            rx_rate: RateMeter::new(Duration::from_secs(1)),
            tx_rate: RateMeter::new(Duration::from_secs(1)),
            errors: ErrorStats::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 速率窗口正确() {
        let mut m = RateMeter::new(Duration::from_secs(1));
        let t0 = Instant::now();
        m.push(t0, 100);
        m.push(t0 + Duration::from_millis(500), 100);
        let r = m.rate_bps(t0 + Duration::from_millis(600));
        assert!((r - 200.0).abs() < 1.0, "200B/s,得到 {r}");
        // The rate returns to zero after samples leave the window.
        let r2 = m.rate_bps(t0 + Duration::from_millis(2_000));
        assert!(r2.abs() < 1.0, "过期归零,得到 {r2}");
    }

    #[test]
    fn 错误归类() {
        let mut e = ErrorStats::default();
        e.record("framing error at byte 3");
        e.record("parity error");
        e.record("overrun error");
        e.record("something else");
        assert_eq!(e.framing, 1);
        assert_eq!(e.parity, 1);
        assert_eq!(e.overrun, 1);
        assert_eq!(e.other, 1);
        assert_eq!(e.total(), 4);
    }
}
