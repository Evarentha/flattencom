/*
 * flattencom - flattencom Flattencom-Mcp Src Flash
 *
 * Invokes esptool with explicit firmware offsets and checks timeout, exit status and output.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

//! ESP32 flashing uses Espressif's esptool protocol implementation.
//! The port must first be released by the daemon. A raw file send is not flashing.
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt};

const STREAM_CAPTURE_BYTES: usize = 32 * 1024;

/// Keep a bounded prefix, but continue draining to prevent a full pipe deadlock.
async fn drain_output(mut stream: impl AsyncRead + Unpin) -> std::io::Result<String> {
    let mut retained = Vec::with_capacity(STREAM_CAPTURE_BYTES);
    let mut buffer = vec![0; 8192];
    let mut truncated = false;
    loop {
        let n = stream.read(&mut buffer).await?;
        if n == 0 {
            break;
        }
        let keep = n.min(STREAM_CAPTURE_BYTES - retained.len());
        retained.extend_from_slice(&buffer[..keep]);
        truncated |= keep < n;
    }
    let mut text = String::from_utf8_lossy(&retained).into_owned();
    if truncated {
        text.push_str("\n[output truncated]");
    }
    Ok(text)
}

async fn capture_command(
    command: &mut tokio::process::Command,
    timeout: Duration,
) -> Result<(std::process::ExitStatus, String), String> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("Install esptool (python -m pip install esptool): {e}"))?;
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let result = tokio::time::timeout(timeout, async {
        // Poll both drains and wait concurrently, including while output is discarded.
        tokio::try_join!(child.wait(), drain_output(stdout), drain_output(stderr))
    })
    .await;
    match result {
        Ok(Ok((status, stdout, stderr))) => Ok((status, format!("{stdout}\n{stderr}"))),
        result => {
            // Reap the direct child on errors as well as normal completion.
            let _ = child.kill().await;
            Err(match result {
                Ok(Err(error)) => error.to_string(),
                _ => "esptool timed out after 300s".into(),
            })
        }
    }
}

/// Explicit ESP32 flashing parameters. Offset depends on the built image layout.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct FlashParams {
    /// Serial port path.
    pub port: String,
    /// Firmware image path on the MCP server host.
    pub firmware_path: String,
    /// Flash byte offset, e.g. 0 for a merged image. Use the firmware build's map.
    pub offset: u32,
    /// Transfer rate; defaults to 460800.
    pub baud_rate: Option<u32>,
}

/// Completed esptool invocation, including its verified process exit status.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct FlashResult {
    /// Tool process exit code.
    pub exit_code: i32,
    /// Captured protocol progress and verification result.
    pub output: String,
}

/// Run installed esptool with a fixed argument vector, never through a shell.
pub async fn run(p: &FlashParams) -> Result<FlashResult, String> {
    if !Path::new(&p.firmware_path).is_file() {
        return Err("Firmware file does not exist".into());
    }
    let mut command = if let Some(executable) = std::env::var_os("FLATTENCOM_ESPTOOL_BIN") {
        tokio::process::Command::new(executable)
    } else {
        let mut command =
            tokio::process::Command::new(if cfg!(windows) { "python" } else { "python3" });
        command.args(["-m", "esptool"]);
        command
    };
    let (status, text) = capture_command(
        command
            .arg("--port")
            .arg(&p.port)
            .arg("--baud")
            .arg(p.baud_rate.unwrap_or(460_800).to_string())
            .arg("write-flash")
            .arg(format!("0x{:x}", p.offset))
            .arg(&p.firmware_path),
        Duration::from_secs(300),
    )
    .await?;
    if !status.success() {
        return Err(format!("esptool failed: {text}"));
    }
    Ok(FlashResult {
        exit_code: status.code().unwrap_or(0),
        output: text,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[tokio::test]
    async fn noisy_child_drains_both_pipes_and_preserves_exit_status() {
        let mut command = tokio::process::Command::new("sh");
        command.args(["-c", "i=0; while [ $i -lt 20000 ]; do printf 'stdout-data\\n'; printf 'stderr-data\\n' >&2; i=$((i+1)); done; exit 7"]);
        let (status, text) = capture_command(&mut command, Duration::from_secs(10))
            .await
            .unwrap();
        assert_eq!(status.code(), Some(7));
        assert!(text.starts_with("stdout-data\n"));
        assert!(text.contains("stderr-data\n"));
        assert_eq!(text.matches("[output truncated]").count(), 2);
        assert!(text.len() <= 2 * STREAM_CAPTURE_BYTES + 64);
    }

    #[tokio::test]
    async fn invalid_utf8_remains_bounded_and_small_output_is_complete() {
        assert_eq!(drain_output(&b"small\n"[..]).await.unwrap(), "small\n");
        let input = vec![255; 4 * STREAM_CAPTURE_BYTES];
        let text = drain_output(input.as_slice()).await.unwrap();
        assert!(text.ends_with("[output truncated]"));
        assert!(text.len() <= 3 * STREAM_CAPTURE_BYTES + 32);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn timeout_stops_and_reaps_a_continuously_writing_child() {
        let mut command = tokio::process::Command::new("sh");
        command.args(["-c", "while :; do printf 'still running\\n'; done"]);
        let error = tokio::time::timeout(
            Duration::from_secs(3),
            capture_command(&mut command, Duration::from_millis(50)),
        )
        .await
        .expect("child cleanup must finish")
        .unwrap_err();
        assert!(error.contains("timed out"));
    }
}
