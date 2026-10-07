/*
 * flattencom - CLI Cmd Send
 *
 * Builds text, hexadecimal or file payloads and optionally waits for a matching response.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

//! `send`: one-shot direct transmission with optional response matching.

use std::io::{Cursor, Read, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use flattencom_core::config::SerialConfig;
use flattencom_core::frame::Direction;
use flattencom_core::ids::SessionId;
use flattencom_core::transport::TransportRegistry;

use super::CmdError;
use crate::exit_code;

const RESPONSE_LIMIT: usize = 1024 * 1024;
const RESPONSE_TAIL: usize = 64 * 1024;

/// Lossy UTF-8 stream with a bounded matching window and an incomplete suffix.
#[derive(Default)]
struct ResponseText {
    text: String,
    pending: Vec<u8>,
}

impl ResponseText {
    fn push(&mut self, bytes: &[u8]) {
        // Bound temporary allocations even if a caller supplies an oversized frame.
        for chunk in bytes.chunks(4096) {
            self.pending.extend_from_slice(chunk);
            let mut rest = self.pending.as_slice();
            while !rest.is_empty() {
                match std::str::from_utf8(rest) {
                    Ok(valid) => {
                        self.text.push_str(valid);
                        rest = &[];
                    }
                    Err(error) => {
                        let valid = error.valid_up_to();
                        self.text
                            .push_str(std::str::from_utf8(&rest[..valid]).expect("valid prefix"));
                        rest = &rest[valid..];
                        if let Some(invalid) = error.error_len() {
                            self.text.push('\u{FFFD}');
                            rest = &rest[invalid..];
                        } else {
                            break;
                        }
                    }
                }
            }
            let consumed = self.pending.len() - rest.len();
            self.pending.drain(..consumed);
            self.trim();
        }
    }

    fn finish(&mut self) {
        if !self.pending.is_empty() {
            self.text.push('\u{FFFD}');
            self.pending.clear();
            self.trim();
        }
    }

    fn trim(&mut self) {
        if self.text.len() > RESPONSE_LIMIT {
            let mut start = self.text.len() - RESPONSE_TAIL;
            while !self.text.is_char_boundary(start) {
                start += 1;
            }
            self.text.drain(..start);
        }
    }
}

/// Open, send, optionally wait for a match, print the result, and close.
#[allow(clippy::too_many_arguments)]
pub fn run(
    port: &str,
    data: Option<String>,
    hex: Option<String>,
    file: Option<PathBuf>,
    baud: u32,
    newline: &str,
    expect: Option<&str>,
    timeout_ms: u64,
    json: bool,
    out: &mut impl Write,
) -> Result<i32, CmdError> {
    let payload = build_payload(data, hex, file, newline)?;
    let matcher = expect.map(regex::Regex::new).transpose().map_err(|e| {
        CmdError::with_code(
            flattencom_core::tr!("Invalid --expect expression: {e}", e = e),
            exit_code::ARGS,
        )
    })?;
    let registry = std::sync::Arc::new(TransportRegistry::new());
    let cfg = SerialConfig {
        baud,
        ..SerialConfig::new(port)
    };
    let session = flattencom_core::session::SessionHandle::open(SessionId::new(), cfg, registry)
        .map_err(CmdError::from_core)?;
    let (sent, seq) = send_payload(payload, |chunk| {
        session.send(chunk.to_vec()).map_err(CmdError::from_core)
    })?;

    if json {
        writeln!(
            out,
            "{}",
            serde_json::json!({ "sent": sent, "seq": seq, "port": port, "baud": baud })
        )
        .map_err(|e| CmdError::new(e.to_string()))?;
    } else {
        writeln!(
            out,
            "{}",
            flattencom_core::tr!("Sent {sent} bytes, sequence: {seq}", sent = sent, seq = seq)
        )
        .map_err(|e| CmdError::new(e.to_string()))?;
    }

    // Wait for a matching response.
    let mut result_code = exit_code::OK;
    if let Some(re) = matcher {
        let started = Instant::now();
        let budget = Duration::from_millis(timeout_ms);
        let mut since: Option<u64> = None;
        let mut matched = None;
        let mut response = ResponseText::default();
        let mut last_rx = None;
        loop {
            let rev = session.change_rev();
            let page = session.read_frames(since, 1 << 20);
            since = Some(page.next_seq);
            for f in &page.frames {
                if f.dir != Direction::Rx {
                    continue;
                }
                response.push(&f.data);
                last_rx = Some(f.clone());
                if re.is_match(&response.text) {
                    matched = Some(f.clone());
                    break;
                }
            }
            if matched.is_some() {
                break;
            }
            if started.elapsed() >= budget {
                response.finish();
                if re.is_match(&response.text) {
                    matched = last_rx;
                }
                break;
            }
            session.wait_change(
                rev,
                budget
                    .saturating_sub(started.elapsed())
                    .min(Duration::from_millis(50)),
            );
        }
        let response = response.text;
        match matched {
            Some(f) => {
                if json {
                    let t = flattencom_core::timefmt::fmt_clock_us(f.t_us);
                    writeln!(
                        out,
                        "{}",
                        serde_json::json!({
                            "matched": true, "t": t, "text": response, "hex": f.hex_string(true),
                        })
                    )
                    .map_err(|e| CmdError::new(e.to_string()))?;
                } else {
                    let t = flattencom_core::timefmt::fmt_clock_us(f.t_us);
                    writeln!(
                        out,
                        "{}",
                        flattencom_core::tr!(
                            "[{t}] Matched response: {response}",
                            response = response,
                            t = t
                        )
                    )
                    .map_err(|e| CmdError::new(e.to_string()))?;
                }
            }
            None => {
                if json {
                    writeln!(out, "{{\"matched\":false}}")
                        .map_err(|e| CmdError::new(e.to_string()))?;
                } else {
                    writeln!(
                        out,
                        "{}",
                        flattencom_core::tr!(
                            "Response timed out after {timeout_ms} ms; no match found",
                            timeout_ms = timeout_ms
                        )
                    )
                    .map_err(|e| CmdError::new(e.to_string()))?;
                }
                result_code = exit_code::TIMEOUT;
            }
        }
    }
    session.flush_tx().map_err(CmdError::from_core)?;
    session.close();
    out.flush().map_err(|e| CmdError::new(e.to_string()))?;
    Ok(result_code)
}

/// Validate and open a payload before opening the device. Files remain streaming;
/// the selected line ending is read only after successful input EOF.
fn build_payload(
    data: Option<String>,
    hex: Option<String>,
    file: Option<PathBuf>,
    newline: &str,
) -> Result<Box<dyn Read>, CmdError> {
    let binary = hex.is_some() || file.is_some();
    if usize::from(data.is_some()) + usize::from(hex.is_some()) + usize::from(file.is_some()) != 1 {
        return Err(CmdError::with_code(
            "Specify exactly one of data, --hex or --file",
            exit_code::ARGS,
        ));
    }
    let newline = newline.to_ascii_lowercase();
    let newline = if newline == "auto" {
        if binary { "none" } else { "crlf" }
    } else {
        &newline
    };
    let ending: &'static [u8] = match newline {
        "crlf" => b"\r\n",
        "lf" => b"\n",
        "cr" => b"\r",
        "none" | "" => b"",
        other => {
            return Err(CmdError::with_code(
                flattencom_core::tr!(
                    "Expected none, lf, crlf or cr for --newline, got {other:?}",
                    other = other
                ),
                exit_code::ARGS,
            ));
        }
    };
    let input: Box<dyn Read> = if let Some(h) = hex {
        let bytes = hex::decode(h.replace(' ', "")).map_err(|e| {
            CmdError::with_code(
                flattencom_core::tr!("Invalid --hex value: {e}", e = e),
                exit_code::ARGS,
            )
        })?;
        Box::new(Cursor::new(bytes))
    } else if let Some(d) = data {
        Box::new(Cursor::new(d.into_bytes()))
    } else if let Some(f) = file {
        Box::new(std::fs::File::open(&f).map_err(|e| {
            CmdError::with_code(
                flattencom_core::tr!("Failed to read {}: {e}", f.display(), e = e),
                exit_code::FAIL,
            )
        })?)
    } else {
        return Err(CmdError::with_code(
            "Missing payload: specify data, --hex or --file",
            exit_code::ARGS,
        ));
    };
    Ok(Box::new(input.chain(ending)))
}

/// Send bounded chunks, including short reads, without reading ahead of writes.
fn send_payload(
    mut input: impl Read,
    mut send: impl FnMut(&[u8]) -> Result<u64, CmdError>,
) -> Result<(u64, u64), CmdError> {
    let mut buffer = [0; 4096];
    let mut sent = 0;
    let mut seq = 0;
    loop {
        let mut count = 0;
        while count < buffer.len() {
            match input.read(&mut buffer[count..]) {
                Ok(0) => break,
                Ok(read) => count += read,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) => return Err(CmdError::new(error.to_string())),
            }
        }
        if count == 0 {
            break;
        }
        seq = send(&buffer[..count])?;
        sent += count as u64;
    }
    Ok((sent, seq))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_validation_and_newline_semantics() {
        for (data, hex, ending, expected) in [
            (Some("text"), None, "auto", &b"text\r\n"[..]),
            (None, Some("00 FF 41"), "auto", &b"\0\xffA"[..]),
            (Some("text"), None, "LF", &b"text\n"[..]),
            (None, Some("41"), "cr", &b"A\r"[..]),
        ] {
            let mut payload = build_payload(
                data.map(str::to_owned),
                hex.map(str::to_owned),
                None,
                ending,
            )
            .unwrap();
            let mut actual = Vec::new();
            payload.read_to_end(&mut actual).unwrap();
            assert_eq!(actual, expected);
        }
        for (data, hex, file, newline) in [
            (None, None, None, "auto"),
            (Some("x".into()), Some("41".into()), None, "auto"),
            (None, None, Some(PathBuf::from("missing-input")), "invalid"),
        ] {
            let error = run(
                "invalid-port",
                data,
                hex,
                file,
                115_200,
                newline,
                None,
                0,
                true,
                &mut Vec::new(),
            )
            .unwrap_err();
            assert_eq!(error.code, exit_code::ARGS);
        }
    }

    #[test]
    fn sparse_file_is_not_read_ahead_and_chunks_are_bounded() {
        let file = tempfile::NamedTempFile::new().unwrap();
        file.as_file().set_len(8 * 1024 * 1024 * 1024).unwrap();
        let payload = build_payload(None, None, Some(file.path().into()), "auto").unwrap();
        let mut calls = 0;
        let error = send_payload(payload, |chunk| {
            calls += 1;
            assert_eq!(chunk.len(), 4096);
            assert!(chunk.iter().all(|&byte| byte == 0));
            Err(CmdError::new("controlled send failure"))
        })
        .unwrap_err();
        assert_eq!(calls, 1);
        assert_eq!(error.message, "controlled send failure");
    }

    #[test]
    fn short_reads_interruptions_and_input_errors_are_preserved() {
        struct Input(usize);
        impl Read for Input {
            fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
                self.0 += 1;
                assert!(out.len() <= 4096);
                match self.0 {
                    1 => Err(std::io::ErrorKind::Interrupted.into()),
                    2 => {
                        out[..2].copy_from_slice(b"ok");
                        Ok(2)
                    }
                    _ => Err(std::io::Error::other("input failed")),
                }
            }
        }
        let mut sent = Vec::new();
        let error = send_payload(Input(0).chain(&b"\r\n"[..]), |chunk| {
            sent.extend_from_slice(chunk);
            Ok(1)
        })
        .unwrap_err();
        assert!(sent.is_empty(), "{:?}", sent.is_empty());
        assert_eq!(error.message, "input failed");
    }

    #[test]
    fn file_auto_is_binary_and_explicit_newline_is_appended_once() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        let data = vec![0xff; 9000];
        file.write_all(&data).unwrap();
        for ending in ["auto", "crlf"] {
            let payload = build_payload(None, None, Some(file.path().into()), ending).unwrap();
            let mut actual = Vec::new();
            let (count, _) = send_payload(payload, |chunk| {
                assert!(chunk.len() <= 4096);
                actual.extend_from_slice(chunk);
                Ok(7)
            })
            .unwrap();
            let mut expected = data.clone();
            if ending == "crlf" {
                expected.extend_from_slice(b"\r\n");
            }
            assert_eq!(actual, expected);
            assert_eq!(count, expected.len() as u64);
        }
    }

    #[test]
    fn output_write_and_flush_failures_are_errors() {
        struct Output(bool);
        impl Write for Output {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                if self.0 {
                    Ok(bytes.len())
                } else {
                    Err(std::io::ErrorKind::BrokenPipe.into())
                }
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Err(std::io::ErrorKind::BrokenPipe.into())
            }
        }
        for json in [false, true] {
            for flush in [false, true] {
                assert!(
                    run(
                        "virtual://echo",
                        Some("x".into()),
                        None,
                        None,
                        115_200,
                        "auto",
                        None,
                        0,
                        json,
                        &mut Output(flush)
                    )
                    .is_err()
                );
            }
        }
    }

    #[test]
    fn maximum_timeout_does_not_overflow_instant() {
        let result = run(
            "virtual://echo",
            Some("ok".into()),
            None,
            None,
            115_200,
            "auto",
            Some("ok"),
            u64::MAX,
            true,
            &mut Vec::new(),
        )
        .unwrap();
        assert_eq!(result, exit_code::OK);
    }

    #[test]
    fn response_decodes_every_utf8_split_and_invalid_sequence() {
        for bytes in [
            "aé设备🦀z".as_bytes(),
            b"a\xff\xe8\xae!\xc0\x80z",
            b"a\xf0\x9f",
        ] {
            for split in 0..=bytes.len() {
                let mut response = ResponseText::default();
                response.push(&bytes[..split]);
                response.push(&bytes[split..]);
                response.finish();
                assert_eq!(
                    response.text,
                    String::from_utf8_lossy(bytes),
                    "split {split}"
                );
                assert!(
                    response.pending.is_empty(),
                    "{:?}",
                    response.pending.is_empty()
                );
            }
            let mut response = ResponseText::default();
            for byte in bytes {
                response.push(&[*byte]);
                assert!(response.pending.len() <= 3);
            }
            response.finish();
            assert_eq!(response.text, String::from_utf8_lossy(bytes));
        }
    }

    #[test]
    fn incomplete_utf8_is_not_replaced_until_stream_finishes() {
        let mut response = ResponseText::default();
        response.push(&[0xe8, 0xae]);
        assert!(response.text.is_empty(), "{:?}", response.text.is_empty());
        response.push(&[0xbe]);
        assert_eq!(response.text, "设");
        response.push(&[0xf0, 0x9f]);
        assert_eq!(response.text, "设");
        response.finish();
        assert_eq!(response.text, "设�");
    }

    #[test]
    fn response_window_trims_at_utf8_boundary_and_preserves_pending_suffix() {
        let mut response = ResponseText::default();
        response.push("设".repeat(RESPONSE_LIMIT / 3).as_bytes());
        assert_eq!(response.text.len(), RESPONSE_LIMIT - 1);
        response.push("设".as_bytes());
        assert!(response.text.len() <= RESPONSE_TAIL);
        assert!(response.text.chars().all(|c| c == '设'));
        response.push(&[0xe5, 0xa4]);
        assert_eq!(response.pending, [0xe5, 0xa4]);
        response.push(&[0x87]);
        assert!(response.text.ends_with("设备"));
        response.push(&vec![b'A'; RESPONSE_LIMIT * 2]);
        assert!(response.text.len() <= RESPONSE_LIMIT);
        assert!(
            response.pending.is_empty(),
            "{:?}",
            response.pending.is_empty()
        );
    }
}
