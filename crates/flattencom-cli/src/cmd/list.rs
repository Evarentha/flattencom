/*
 * flattencom - CLI Cmd List
 *
 * Prints physical port metadata as a table or machine-readable JSON.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

//! `list`: enumerate system serial ports.

use std::io::Write;

use super::CmdError;
use crate::exit_code;

/// Render a table or JSON.
pub fn run(json: bool, out: &mut impl Write) -> Result<i32, CmdError> {
    let ports = flattencom_core::discovery::list_ports().map_err(CmdError::from_core)?;
    if json {
        let v = serde_json::to_value(&ports).map_err(|e| CmdError::new(e.to_string()))?;
        writeln!(out, "{v}").map_err(|e| CmdError::new(e.to_string()))?;
        return Ok(exit_code::OK);
    }
    if ports.is_empty() {
        writeln!(
            out,
            "{}",
            flattencom_core::tr!("No serial ports found. Check the device connection and driver.")
        )
        .map_err(|e| CmdError::new(e.to_string()))?;
        return Ok(exit_code::OK);
    }
    // Table: path | type | VID:PID | manufacturer/product | serial number
    writeln!(
        out,
        "{}",
        flattencom_core::tr!(
            "{:<16} {:<8} {:<10} {:<24} Serial number",
            "Path",
            "Type",
            "VID:PID",
            "Device"
        )
    )
    .map_err(|e| CmdError::new(e.to_string()))?;
    for p in &ports {
        let vidpid = match (p.vid, p.pid) {
            (Some(v), Some(i)) => format!("{v:04X}:{i:04X}"),
            _ => "-".to_owned(),
        };
        let name = p
            .product
            .clone()
            .or_else(|| p.manufacturer.clone())
            .unwrap_or_else(|| p.friendly_name.clone());
        writeln!(
            out,
            "{:<16} {:<8} {:<10} {:<24} {}",
            p.path,
            p.kind.as_str(),
            vidpid,
            truncate(&name, 24),
            p.serial.clone().unwrap_or_default()
        )
        .map_err(|e| CmdError::new(e.to_string()))?;
    }
    Ok(exit_code::OK)
}

fn truncate(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}
