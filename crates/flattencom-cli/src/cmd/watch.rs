/*
 * flattencom - CLI Cmd Watch
 *
 * Polls serial device changes and emits connection or removal events.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

//! `watch`: stream hotplug events until Ctrl-C.

use std::io::Write;
use std::time::Duration;

use super::CmdError;
use crate::exit_code;

/// Poll port differences and print events; `--json` emits one JSON object per line.
pub fn run(interval_ms: u64, json: bool, out: &mut impl Write) -> Result<i32, CmdError> {
    let mut watcher =
        flattencom_core::discovery::HotplugWatcher::start(Duration::from_millis(interval_ms))
            .map_err(CmdError::from_core)?;
    if !json {
        writeln!(
            out,
            "{}",
            flattencom_core::tr!(
                "Watching ports every {interval_ms} ms; press Ctrl-C to exit",
                interval_ms = interval_ms
            )
        )
        .map_err(|e| CmdError::new(e.to_string()))?;
    }
    out.flush().map_err(|e| CmdError::new(e.to_string()))?;
    let stopped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let signal = stopped.clone();
    ctrlc::set_handler(move || signal.store(true, std::sync::atomic::Ordering::Relaxed))
        .map_err(|e| CmdError::new(e.to_string()))?;
    while !stopped.load(std::sync::atomic::Ordering::Relaxed) {
        match watcher.poll_once_until(|| stopped.load(std::sync::atomic::Ordering::Relaxed)) {
            Ok(Some(ev)) => {
                if json {
                    let v = serde_json::to_value(&ev).map_err(|e| CmdError::new(e.to_string()))?;
                    writeln!(out, "{v}").map_err(|e| CmdError::new(e.to_string()))?;
                } else {
                    match ev {
                        flattencom_core::discovery::HotplugEvent::Added { ports } => {
                            for p in ports {
                                let ts = flattencom_core::timefmt::fmt_clock_us(now_us());
                                writeln!(
                                    out,
                                    "[{ts}] + {path} {name}",
                                    path = p.path,
                                    name = p.friendly_name
                                )
                                .map_err(|e| CmdError::new(e.to_string()))?;
                            }
                        }
                        flattencom_core::discovery::HotplugEvent::Removed { paths } => {
                            for path in paths {
                                let ts = flattencom_core::timefmt::fmt_clock_us(now_us());
                                writeln!(out, "[{ts}] - {path}")
                                    .map_err(|e| CmdError::new(e.to_string()))?;
                            }
                        }
                    }
                }
                out.flush().map_err(|e| CmdError::new(e.to_string()))?;
            }
            Ok(None) => {}
            Err(e) => {
                let msg =
                    flattencom_core::tr!("Port polling failed: {e}; retrying in 3 seconds", e = e);
                if json {
                    writeln!(out, "{}", serde_json::json!({"error": msg}))
                        .map_err(|e| CmdError::new(e.to_string()))?;
                    out.flush().map_err(|e| CmdError::new(e.to_string()))?;
                } else {
                    eprintln!("{msg}");
                }
                for _ in 0..60 {
                    if stopped.load(std::sync::atomic::Ordering::Relaxed) {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        }
    }
    out.flush().map_err(|e| CmdError::new(e.to_string()))?;
    Ok(exit_code::OK)
}

fn now_us() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_micros() as i64)
}
