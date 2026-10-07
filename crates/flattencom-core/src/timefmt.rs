/*
 * flattencom - Core Timefmt
 *
 * Formats wall-clock and monotonic microsecond timestamps for display and export.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

//! Shared timestamp formatting for display.
//!
//! Frames carry wall-clock `t_us` and session-monotonic `mono_us`, both in microseconds.
//! Format both clocks for humans; relative time uses the monotonic clock and ignores wall-clock changes.

use chrono::{Local, TimeZone};

/// Wall clock as local `HH:MM:SS.mmm`.
#[must_use]
pub fn fmt_clock_us(t_us: i64) -> String {
    let secs = t_us.div_euclid(1_000_000);
    let nanos = t_us.rem_euclid(1_000_000) * 1000;
    Local
        .timestamp_opt(
            secs,
            u32::try_from(nanos).expect("microsecond remainder in range"),
        )
        .single()
        .map_or_else(
            || format!("@{t_us}"),
            |dt| dt.format("%H:%M:%S%.3f").to_string(),
        )
}

/// Wall clock as local `YYYY-MM-DD HH:MM:SS.ffffff`.
#[must_use]
pub fn fmt_full_us(t_us: i64) -> String {
    let secs = t_us.div_euclid(1_000_000);
    let nanos = t_us.rem_euclid(1_000_000) * 1000;
    Local
        .timestamp_opt(
            secs,
            u32::try_from(nanos).expect("microsecond remainder in range"),
        )
        .single()
        .map_or_else(
            || format!("@{t_us}"),
            |dt| dt.format("%Y-%m-%d %H:%M:%S%.6f").to_string(),
        )
}

/// Monotonic microseconds as `12.345s`, `1m02.345s` or `1h02m03s`.
#[must_use]
pub fn fmt_rel_us(mono_us: u64) -> String {
    let us = mono_us;
    if us < 60_000_000 {
        format!("{}.{:03}s", us / 1_000_000, (us % 1_000_000) / 1_000)
    } else if us < 3_600_000_000 {
        format!(
            "{}m{:02}.{:03}s",
            us / 60_000_000,
            (us % 60_000_000) / 1_000_000,
            (us % 1_000_000) / 1_000
        )
    } else {
        format!(
            "{}h{:02}m{:02}s",
            us / 3_600_000_000,
            (us % 3_600_000_000) / 60_000_000,
            (us % 60_000_000) / 1_000_000
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 相对时间格式() {
        assert_eq!(fmt_rel_us(12_345_000), "12.345s");
        assert_eq!(fmt_rel_us(62_345_000), "1m02.345s");
        assert_eq!(fmt_rel_us(3_723_000_000), "1h02m03s");
    }

    #[test]
    fn 墙钟格式() {
        // Microseconds for 2026-09-26 04:00:00 UTC.
        let t = 1_790_027_200_000_000i64;
        let s = fmt_clock_us(t);
        assert_eq!(s.len(), 12, "HH:MM:SS.mmm");
        assert!(s.ends_with(".000"));
        assert!(fmt_full_us(t).starts_with("20"));
    }
}
