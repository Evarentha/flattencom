/*
 * flattencom - CLI Cmd Replay
 *
 * Sends recorded frames to a direct serial connection with scaled timing.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

//! `replay`: send a JSONL capture to a port with scaled original timing.

use std::io::Write;
use std::path::Path;

use flattencom_core::config::SerialConfig;
use flattencom_core::ids::SessionId;
use flattencom_core::record;
use flattencom_core::session::SessionHandle;
use flattencom_core::transport::TransportRegistry;

use super::CmdError;
use crate::exit_code;

/// Read, send frames with their timing, then print statistics.
pub fn run(
    log: &Path,
    port: &str,
    baud: u32,
    speed: f64,
    json: bool,
    out: &mut impl Write,
) -> Result<i32, CmdError> {
    if !speed.is_finite() || speed <= 0.0 {
        return Err(CmdError::with_code(
            "--speed must be positive",
            exit_code::ARGS,
        ));
    }
    let mut frames = record::stream_log(log).map_err(CmdError::from_core)?;
    let first = frames.next().transpose().map_err(CmdError::from_core)?;
    if first.is_none() {
        let skipped = frames.skipped();
        if json {
            writeln!(out, "{}", serde_json::json!({ "frames_sent": 0, "bytes_sent": 0, "skipped": skipped, "speed": speed }))
                .map_err(|e| CmdError::new(e.to_string()))?;
        } else {
            writeln!(
                out,
                "{}",
                flattencom_core::tr!(
                    "No data to replay; skipped {skipped} invalid records",
                    skipped = skipped
                )
            )
            .map_err(|e| CmdError::new(e.to_string()))?;
        }
        out.flush().map_err(|e| CmdError::new(e.to_string()))?;
        return Ok(exit_code::OK);
    }
    let registry = std::sync::Arc::new(TransportRegistry::new());
    let cfg = SerialConfig {
        baud,
        ..SerialConfig::new(port)
    };
    let session =
        SessionHandle::open(SessionId::new(), cfg, registry).map_err(CmdError::from_core)?;

    let mut prev: Option<u64> = None;
    let mut sent: u64 = 0;
    let mut bytes: u64 = 0;
    for f in first.into_iter().map(Ok).chain(frames.by_ref()) {
        let f = f.map_err(CmdError::from_core)?;
        if let Some(p) = prev {
            let delay = record::replay_delay(p, f.mono_us, speed);
            if !delay.is_zero() {
                std::thread::sleep(delay);
            }
        }
        prev = Some(f.mono_us);
        session
            .send((*f.data).clone())
            .map_err(CmdError::from_core)?;
        sent += 1;
        bytes += f.len() as u64;
    }
    session.flush_tx().map_err(CmdError::from_core)?;
    session.close();
    let skipped = frames.skipped();
    if json {
        writeln!(out, "{}", serde_json::json!({ "frames_sent": sent, "bytes_sent": bytes, "skipped": skipped, "speed": speed }))
            .map_err(|e| CmdError::new(e.to_string()))?;
    } else {
        writeln!(out, "{}", flattencom_core::tr!("Replay complete: {sent} frames, {bytes} bytes, speed {speed}, skipped {skipped} invalid records", bytes = bytes, sent = sent, skipped = skipped, speed = speed))
        .map_err(|e| CmdError::new(e.to_string()))?;
    }
    out.flush().map_err(|e| CmdError::new(e.to_string()))?;
    Ok(exit_code::OK)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slow_replay_uses_requested_speed_on_virtual_port() {
        use flattencom_core::frame::{Direction, Frame};

        let mut log = tempfile::NamedTempFile::new().unwrap();
        for (seq, mono_us) in [(0, 0), (1, 1_000)] {
            let frame = Frame::new(seq, Direction::Tx, b"A".to_vec(), 0, mono_us);
            writeln!(log, "{}", serde_json::to_string(&frame).unwrap()).unwrap();
        }
        let mut output = Vec::new();
        let started = std::time::Instant::now();
        run(
            log.path(),
            "virtual://echo",
            115_200,
            0.001,
            true,
            &mut output,
        )
        .unwrap();
        assert!(started.elapsed() >= std::time::Duration::from_secs(1));
        let result: serde_json::Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(result["speed"], 0.001);
        assert_eq!(result["frames_sent"], 2);
        assert_eq!(result["bytes_sent"], 2);
    }

    #[test]
    fn empty_and_all_invalid_json_replay_emit_zero_stats_without_opening_port() {
        for (contents, skipped) in [(b"".as_slice(), 0), (b"\xff\n{bad}\n \t\n".as_slice(), 2)] {
            let mut log = tempfile::NamedTempFile::new().unwrap();
            log.write_all(contents).unwrap();
            let mut output = Vec::new();
            assert_eq!(
                run(
                    log.path(),
                    "nonexistent-port",
                    115_200,
                    2.0,
                    true,
                    &mut output
                )
                .unwrap(),
                exit_code::OK
            );
            let result: serde_json::Value = serde_json::from_slice(&output).unwrap();
            assert_eq!(
                result,
                serde_json::json!({"frames_sent":0,"bytes_sent":0,"skipped":skipped,"speed":2.0})
            );
        }
    }

    #[test]
    fn empty_replay_propagates_output_failure() {
        struct BrokenOutput;
        impl Write for BrokenOutput {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let log = tempfile::NamedTempFile::new().unwrap();
        assert!(run(log.path(), "unused", 115_200, 1.0, false, &mut BrokenOutput).is_err());
    }
}
