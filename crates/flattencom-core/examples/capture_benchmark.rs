/*
 * flattencom - flattencom Flattencom-Core Examples Capture Benchmark
 *
 * Measures synthetic capture throughput and verifies every generated binary byte.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

//! Bounded capture benchmark: synthetic 1 Mbaud/8N1 payload rate and byte validation.
use flattencom_core::{
    config::SerialConfig, ids::SessionId, session::SessionHandle, transport::TransportRegistry,
};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let seconds = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(5);
    // 1 Mbaud, 8N1 = 100,000 payload B/s. Generator bps measures payload bits.
    let session = SessionHandle::open(
        SessionId::new(),
        SerialConfig::new("virtual://gen?bps=800000&pattern=binary"),
        Arc::new(TransportRegistry::new()),
    )?;
    let started = Instant::now();
    let mut cursor = None;
    let mut received = 0u64;
    while started.elapsed() < Duration::from_secs(seconds) {
        let rev = session.change_rev();
        let page = session.read_frames(cursor, 64 * 1024);
        cursor = Some(page.next_seq);
        for frame in page.frames {
            for byte in frame.data.iter() {
                assert_eq!(
                    *byte,
                    (received % 256) as u8,
                    "capture corruption at {received}"
                );
                received += 1;
            }
        }
        session.wait_change(rev, Duration::from_millis(20));
    }
    session.close();
    let stats = session.stats();
    assert_eq!(stats.dropped_rx, 0);
    assert!(
        received > seconds * 90_000,
        "capture throughput below 90% of expected rate"
    );
    println!(
        "{}",
        serde_json::json!({"elapsed_s":started.elapsed().as_secs_f64(),"validated_bytes":received,"dropped":stats.dropped_rx,"frames":stats.rx_frames,"simulated_baud_8n1":1_000_000})
    );
    Ok(())
}
