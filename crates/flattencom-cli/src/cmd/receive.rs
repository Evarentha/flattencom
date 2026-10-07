/*
 * flattencom - CLI Cmd Receive
 *
 * Streams received serial bytes to stdout or a file until timeout or interruption.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

//! `receive`: stream received output to stdout for use in pipelines.

use std::io::Write;

use std::time::{Duration, Instant};

use flattencom_core::config::SerialConfig;
use flattencom_core::frame::Direction;
use flattencom_core::ids::SessionId;
use flattencom_core::session::{SessionHandle, SessionState};
use flattencom_core::transport::TransportRegistry;

use super::CmdError;
use crate::exit_code;

/// Open, print RX as text or hexadecimal, and stop on duration expiry or Ctrl-C.
/// A terminal session failure closes the session and returns an error.
pub fn run(
    port: &str,
    baud: u32,
    hex: bool,
    output: Option<&std::path::Path>,
    duration: Option<u64>,
    json: bool,
    out: &mut impl Write,
) -> Result<i32, CmdError> {
    // Optional append-only output file
    let mut file = output.map(|p| {
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir).ok();
        }
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(p)
    });
    if let Some(Err(e)) = &file {
        let msg = flattencom_core::tr!("Failed to open output file: {e}", e = e);
        return Err(CmdError::new(msg));
    }

    let registry = std::sync::Arc::new(TransportRegistry::new());
    let cfg = SerialConfig {
        baud,
        ..SerialConfig::new(port)
    };
    let session =
        SessionHandle::open(SessionId::new(), cfg, registry).map_err(CmdError::from_core)?;

    let started = Instant::now();
    let duration = duration.map(Duration::from_secs);
    let mut since: Option<u64> = None;
    // Exit cleanly on Ctrl-C.
    let stopped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let signal = stopped.clone();
    ctrlc::set_handler(move || {
        signal.store(true, std::sync::atomic::Ordering::Relaxed);
    })
    .map_err(|e| {
        CmdError::new(flattencom_core::tr!(
            "Failed to install Ctrl-C handler: {e}",
            e = e
        ))
    })?;

    loop {
        if let SessionState::Failed { error } = session.state() {
            session.close();
            return Err(CmdError::new(error));
        }
        if stopped.load(std::sync::atomic::Ordering::Relaxed)
            || duration.is_some_and(|d| started.elapsed() >= d)
        {
            break;
        }
        let rev = session.change_rev();
        let page = session.read_frames(since, 1 << 20);
        since = Some(page.next_seq);
        for f in &page.frames {
            if f.dir != Direction::Rx {
                continue;
            }
            let line = if json {
                serde_json::to_string(f).map_err(|e| CmdError::new(e.to_string()))?
            } else if hex {
                format!(
                    "{} {}",
                    flattencom_core::timefmt::fmt_clock_us(f.t_us),
                    f.hex_string(true)
                )
            } else {
                format!(
                    "{} {}",
                    flattencom_core::timefmt::fmt_clock_us(f.t_us),
                    f.text_lossy()
                )
            };
            writeln!(out, "{line}").map_err(|e| CmdError::new(e.to_string()))?;
            if let Some(Ok(file)) = file.as_mut() {
                writeln!(file, "{line}").map_err(|e| CmdError::new(e.to_string()))?;
            }
        }
        if !page.frames.is_empty() {
            out.flush().map_err(|e| CmdError::new(e.to_string()))?;
        }
        // Wait for new frames or the 32 ms timeout.
        session.wait_change(rev, Duration::from_millis(32));
    }
    let state = session.state();
    session.close();
    if let SessionState::Failed { error } = state {
        return Err(CmdError::new(error));
    }
    Ok(exit_code::OK)
}
