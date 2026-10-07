/*
 * flattencom - Core Autobaud
 *
 * Scores received text and ranks candidate baud rates for caller-managed detection.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

//! Baud detection helpers: score frame quality and rank candidates.
//!
//! The caller (CLI `autobaud` or MCP prompt) orchestrates detection:
//! open each rate, sample for N ms, call [`score_bytes`] and [`rank`], then `configure_port`.

use serde::{Deserialize, Serialize};

/// Common baud rates in sampling order.
#[must_use]
pub fn candidate_bauds() -> Vec<u32> {
    vec![
        9_600, 115_200, 57_600, 38_400, 19_200, 4_800, 2_400, 1_200, 230_400, 460_800, 921_600,
    ]
}

/// Frame-quality score from 0.0 to 1.0.
///
/// Text-stream features expected at a plausible baud rate:
/// - Printable-byte ratio (weight 0.5)
/// - Complete `\r\n` line endings (weight 0.2)
/// - Byte diversity (weight 0.2)
/// - NUL-density penalty (weight 0.1)
///
/// Incorrect rates typically produce nonprintable bytes and dense NULs, resulting in low scores.
#[must_use]
pub fn score_bytes(data: &[u8]) -> f64 {
    if data.is_empty() {
        return 0.0;
    }
    let len = data.len() as f64;
    let printable = data
        .iter()
        .filter(|&&b| (0x20..=0x7E).contains(&b) || b == b'\r' || b == b'\n' || b == b'\t')
        .count() as f64
        / len;
    let lines: Vec<&[u8]> = data.split(|&b| b == b'\n').collect();
    let cr_end = lines.iter().filter(|l| l.ends_with(b"\r")).count() as f64 / lines.len() as f64;
    let mut seen = [false; 256];
    for &b in data {
        seen[b as usize] = true;
    }
    let diversity = seen.iter().filter(|x| **x).count() as f64 / 64.0;
    let nul = data.iter().filter(|&&b| b == 0).count() as f64 / len;
    printable * 0.5 + cr_end * 0.2 + diversity.min(1.0) * 0.2 + (1.0 - nul) * 0.1
}

/// Sampling result for one baud rate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BaudScore {
    /// Candidate baud rate.
    pub baud: u32,
    /// Quality score (0..1).
    pub score: f64,
    /// Frames received during sampling.
    pub rx_frames: u64,
    /// Bytes received during sampling.
    pub rx_bytes: u64,
    /// Best sample fragment for review.
    pub best_text: String,
}

/// Sort by descending score.
pub fn rank(scores: &mut [BaudScore]) {
    scores.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 文本流得分高() {
        let text = b"AT+CSQ\r\n+CSQ: 23,0\r\n\r\nOK\r\n";
        let garbage = [
            0u8, 0xFF, 0x81, 0x00, 0xFE, 0x11, 0x00, 0xC3, 0xFF, 0x00, 0x44, 0x99,
        ];
        let s_text = score_bytes(text);
        let s_bad = score_bytes(&garbage);
        assert!(s_text > 0.7, "文本流应得高分:{s_text}");
        assert!(s_bad < 0.45, "乱码应得低分:{s_bad}");
        assert!(s_text > s_bad);
    }

    #[test]
    fn 排序降序() {
        let mut scores = vec![
            BaudScore {
                baud: 9_600,
                score: 0.3,
                rx_frames: 1,
                rx_bytes: 2,
                best_text: String::new(),
            },
            BaudScore {
                baud: 115_200,
                score: 0.9,
                rx_frames: 3,
                rx_bytes: 9,
                best_text: String::new(),
            },
        ];
        rank(&mut scores);
        assert_eq!(scores[0].baud, 115_200);
    }

    #[test]
    fn 候选含常见档位() {
        let b = candidate_bauds();
        assert!(b.contains(&115_200));
        assert!(b.contains(&9_600));
    }
}
