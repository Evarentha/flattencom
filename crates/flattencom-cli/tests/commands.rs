/*
 * flattencom - flattencom Flattencom-Cli Tests Commands
 *
 * Checks CLI payload fidelity, delayed replies, language selection and help output.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

//! Regression coverage for late serial replies and binary-exact output.
use std::process::Command;

#[cfg(unix)]
#[test]
fn native_file_path_bytes_survive_language_selection_and_payload_loading() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let root = tempfile::tempdir().unwrap();
    let path = root
        .path()
        .join(OsString::from_vec(b"payload-\xff.bin".to_vec()));
    std::fs::write(&path, b"A\0\xffB").unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_flattencom"))
        .args(["--lang=zh-CN", "send", "virtual://echo", "--file"])
        .arg(&path)
        .args(["--expect", "B", "--json"])
        .output()
        .unwrap();
    assert!(result.status.success(), "{result:?}");
    let records: Vec<serde_json::Value> = String::from_utf8(result.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(records[0]["sent"], 4);
    assert_eq!(records[1]["hex"], "41 00 FF 42");
}

#[cfg(unix)]
#[test]
fn invalid_language_arguments_are_errors_instead_of_panics() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    for args in [
        vec![OsString::from("--lang"), OsString::from_vec(vec![0xff])],
        vec![OsString::from_vec(b"--lang=\xff".to_vec())],
        vec![OsString::from("--lang=unknown")],
    ] {
        let result = Command::new(env!("CARGO_BIN_EXE_flattencom"))
            .args(args)
            .args(["decode", "--list"])
            .output()
            .unwrap();
        assert_eq!(result.status.code(), Some(5), "{result:?}");
        assert!(!result.stderr.is_empty(), "{:?}", result.stderr.is_empty());
        assert!(!String::from_utf8_lossy(&result.stderr).contains("panicked"));
    }
}

#[test]
fn expect_matches_utf8_split_at_transport_chunk_boundary() {
    let payload = format!("{}设备", "A".repeat(4095));
    let result = Command::new(env!("CARGO_BIN_EXE_flattencom"))
        .args([
            "send",
            "virtual://echo",
            &payload,
            "--expect",
            "设备",
            "--timeout-ms",
            "1000",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let lines: Vec<serde_json::Value> = String::from_utf8(result.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(lines[1]["matched"], true);
    assert!(lines[1]["text"].as_str().unwrap().contains("设备"));
    assert!(!lines[1]["text"].as_str().unwrap().contains('\u{FFFD}'));
}

#[test]
fn delayed_echo_and_binary_default_are_exact() {
    let result = Command::new(env!("CARGO_BIN_EXE_flattencom"))
        .args([
            "send",
            "virtual://echo?delay_ms=30",
            "PING",
            "--expect",
            "PING",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let values: Vec<serde_json::Value> = String::from_utf8(result.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(values[1]["text"], "PING\r\n");
    let result = Command::new(env!("CARGO_BIN_EXE_flattencom"))
        .args(["send", "virtual://echo", "--hex", "00 FF 41", "--json"])
        .output()
        .unwrap();
    assert!(result.status.success());
    let result: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(result["sent"], 3);
}
#[test]
fn language_defaults_and_payload_preservation() {
    let exe = env!("CARGO_BIN_EXE_flattencom");
    let help = std::process::Command::new(exe)
        .arg("--help")
        .env_remove("FLATTENCOM_LANG")
        .output()
        .unwrap();
    assert!(help.status.success());
    let text = String::from_utf8(help.stdout).unwrap();
    assert!(text.contains("List serial ports"));
    assert!(!text.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)));
    let help = std::process::Command::new(exe)
        .args(["--lang", "zh-CN", "--help"])
        .output()
        .unwrap();
    assert!(String::from_utf8(help.stdout).unwrap().contains("列出串口"));
    let output = std::process::Command::new(exe)
        .args([
            "--lang",
            "zh-CN",
            "send",
            "virtual://echo",
            "设备输出",
            "--expect",
            "设备输出",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("已发送"));
    assert!(text.contains("设备输出"));
}
#[cfg(target_os = "linux")]
#[test]
fn receive_reports_output_failure() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_flattencom"))
        .args([
            "receive",
            "virtual://gen?bps=115200",
            "--duration",
            "1",
            "--output",
            "/dev/full",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!output.stderr.is_empty(), "{:?}", output.stderr.is_empty());
}

#[cfg(target_os = "linux")]
#[test]
fn daemon_logs_reports_output_failure_for_existing_and_missing_logs() {
    let root = tempfile::tempdir().unwrap();
    for existing in [false, true] {
        if existing {
            std::fs::create_dir(root.path().join("logs")).unwrap();
            std::fs::write(root.path().join("logs/daemon.log"), b"audit log entry\n").unwrap();
        }
        let result = Command::new(env!("CARGO_BIN_EXE_flattencom"))
            .args(["daemon", "logs"])
            .env("FLATTENCOM_STATE_DIR", root.path())
            .stdout(
                std::fs::OpenOptions::new()
                    .write(true)
                    .open("/dev/full")
                    .unwrap(),
            )
            .output()
            .unwrap();
        assert_eq!(result.status.code(), Some(1));
        assert!(!result.stderr.is_empty(), "{:?}", result.stderr.is_empty());
    }
}
