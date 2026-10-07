/*
 * flattencom - flattencom Flattencom-Proto Src Socket
 *
 * Resolves local socket, named-pipe, token and state-directory paths on supported platforms.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

//! Local endpoint discovery shared by daemon and clients.
//!
//! | Platform | Transport | Path |
//! |------|------|------|
//! | Linux | Unix Domain Socket | `$XDG_RUNTIME_DIR/flattencom.sock`; fallback `/tmp/flattencom-<uid>.sock` |
//! | Windows | Named Pipe | \\.\pipe\flattencom.sock |
//!
//! Authentication combines local endpoint permissions with a token file;
//! hello must provide its value. See token_path.
//!
//! FLATTENCOM_SOCKET overrides the endpoint for tests or containers.

use std::path::{Path, PathBuf};

/// Daemon endpoint from the environment; see socket_path_opt for explicit overrides.
#[must_use]
pub fn socket_path() -> PathBuf {
    socket_path_opt(None)
}

/// Daemon endpoint with an optional explicit override; None uses environment defaults.
#[must_use]
pub fn socket_path_opt(override_path: Option<&Path>) -> PathBuf {
    if let Some(p) = override_path {
        return p.to_owned();
    }
    if let Ok(p) = std::env::var("FLATTENCOM_SOCKET")
        && !p.trim().is_empty()
    {
        return PathBuf::from(p);
    }
    #[cfg(windows)]
    {
        PathBuf::from(r"\\.\pipe\flattencom.sock")
    }
    #[cfg(not(windows))]
    {
        if let Ok(runtime) = std::env::var("XDG_RUNTIME_DIR")
            && !runtime.trim().is_empty()
        {
            return PathBuf::from(runtime).join("flattencom.sock");
        }
        let uid = linux_uid().unwrap_or(0);
        PathBuf::from(format!("/tmp/flattencom-{uid}.sock"))
    }
}

/// Token file created on first daemon startup with mode 0600 and supplied by clients in hello.
#[must_use]
pub fn token_path() -> PathBuf {
    token_path_opt(None)
}

/// Token path with an optional state-directory override.
#[must_use]
pub fn token_path_opt(state_override: Option<&Path>) -> PathBuf {
    state_dir_opt(state_override).join("daemon.token")
}

/// Daemon data directory for tokens and logs.
///
/// Linux uses XDG_STATE_HOME or ~/.local/state; Windows uses APPDATA.
#[must_use]
pub fn state_dir() -> PathBuf {
    state_dir_opt(None)
}

/// Daemon data directory with an optional explicit override.
#[must_use]
pub fn state_dir_opt(override_dir: Option<&Path>) -> PathBuf {
    if let Some(d) = override_dir {
        return d.to_owned();
    }
    if let Ok(p) = std::env::var("FLATTENCOM_STATE_DIR")
        && !p.trim().is_empty()
    {
        return PathBuf::from(p);
    }
    let base = std::env::var("XDG_STATE_HOME")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map(PathBuf::from)
        .or_else(dirs::state_dir)
        .or_else(dirs::data_dir)
        .unwrap_or_else(std::env::temp_dir);
    base.join("flattencom")
}

/// Log directory.
#[must_use]
pub fn log_dir() -> PathBuf {
    log_dir_opt(None)
}

/// Log directory with an optional state-directory override.
#[must_use]
pub fn log_dir_opt(state_override: Option<&Path>) -> PathBuf {
    state_dir_opt(state_override).join("logs")
}

#[cfg(target_os = "linux")]
fn linux_uid() -> Option<u32> {
    // Read the Linux UID from /proc/self/status without depending on libc.
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("Uid:") {
            return rest.split_whitespace().next().and_then(|s| s.parse().ok());
        }
    }
    None
}

#[cfg(not(target_os = "linux"))]
#[allow(dead_code)]
fn linux_uid() -> Option<u32> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 环境变量覆盖() {
        let explicit = Path::new("test-endpoint");
        assert_eq!(socket_path_opt(Some(explicit)), explicit);
        assert_eq!(
            token_path_opt(Some(Path::new("test-state"))),
            Path::new("test-state/daemon.token")
        );
    }

    #[test]
    fn 状态目录结构() {
        let d = state_dir();
        assert!(d.to_string_lossy().contains("flattencom"));
        assert!(log_dir().starts_with(&d));
        assert!(token_path().starts_with(&d));
    }
}
