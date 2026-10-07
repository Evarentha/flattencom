/*
 * flattencom - Core Transport Virtual Port
 *
 * Implements virtual echo and rate-controlled text or binary generator transports.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

//! Hardware-free virtual ports under `virtual://` for tests and demonstrations.
//!
//! | URI | Behavior |
//! |-----|------|
//! | `virtual://echo` | Echo writes as RX after delay_ms, default 1 ms |
//! | `virtual://gen?bps=115200&pattern=ascii\|binary` | Rate-controlled test stream |
//!
//! Virtual ports are opened by URI and are not returned by physical `list_ports()` enumeration.
//! They support CI, hardware-free examples and GUI load tests; live baud changes
//! update generator throughput, matching physical-port configuration semantics.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::FlattenError;
use crate::config::SerialConfig;
use crate::transport::{PinStates, SerialTransport, TransportFactory, VIRTUAL_PREFIX};

/// Virtual-port factory.
#[derive(Debug, Default)]
pub struct VirtualFactory;

impl TransportFactory for VirtualFactory {
    fn open(&self, cfg: &SerialConfig) -> Result<Box<dyn SerialTransport>, FlattenError> {
        let rest = cfg.path.strip_prefix(VIRTUAL_PREFIX).unwrap_or_default();
        let (name, query) = match rest.split_once(['?', '#']) {
            Some((n, q)) => (n, q),
            None => (rest, ""),
        };
        let params = parse_query(query);
        match name {
            "echo" => {
                let delay = parse_usize(&params, "delay_ms")
                    .unwrap_or(1)
                    .clamp(0, 1_000);
                Ok(Box::new(EchoTransport::new(Duration::from_millis(
                    delay as u64,
                ))))
            }
            "gen" => {
                let bps = parse_usize(&params, "bps")
                    .unwrap_or(9_600)
                    .clamp(1, 12_000_000);
                let pattern = match params
                    .iter()
                    .find(|(k, _)| k == "pattern")
                    .map(|(_, v)| v.as_str())
                {
                    Some("binary") => Pattern::Binary,
                    _ => Pattern::Ascii,
                };
                Ok(Box::new(GenTransport::new(bps as u64, pattern, cfg.baud)))
            }
            other => Err(FlattenError::InvalidConfig {
                field: "path".into(),
                reason: crate::tr!(
                    "Unknown virtual port {other:?}; use virtual://echo or virtual://gen?bps=115200",
                    other = other
                ),
            }),
        }
    }

    fn kind(&self) -> &'static str {
        "virtual"
    }
}

/// Parse k=v&k2=v2 query pairs; the first duplicate key wins.
fn parse_query(q: &str) -> Vec<(String, String)> {
    q.split('&')
        .filter(|s| !s.is_empty())
        .filter_map(|kv| {
            kv.split_once('=')
                .map(|(k, v)| (k.to_owned(), url_decode(v)))
        })
        .collect()
}

/// Decode %XX escapes in query values.
fn url_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hex = std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("zz");
            if let Ok(v) = u8::from_str_radix(hex, 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn parse_usize(params: &[(String, String)], key: &str) -> Option<usize> {
    params
        .iter()
        .find(|(k, _)| k == key)
        .and_then(|(_, v)| v.parse().ok())
}

/// Generator pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pattern {
    /// Periodic text lines with increasing sequence numbers for loss inspection.
    Ascii,
    /// Incrementing bytes for exact integrity checks.
    Binary,
}

// ---------------------------------------------------------------------------
// echo: loopback
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
struct EchoState {
    pending: VecDeque<(Instant, Vec<u8>)>,
}

/// Loopback transport.
#[derive(Debug)]
pub struct EchoTransport {
    state: Arc<Mutex<EchoState>>,
    delay: Duration,
    pins: PinStates,
}

impl EchoTransport {
    #[must_use]
    fn new(delay: Duration) -> Self {
        Self {
            state: Arc::default(),
            delay,
            pins: PinStates::default(),
        }
    }
}

impl SerialTransport for EchoTransport {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, FlattenError> {
        let mut st = self.state.lock().expect("Echo state lock poisoned");
        if let Some((due, data)) = st.pending.front()
            && Instant::now() >= *due
        {
            let n = data.len().min(buf.len());
            buf[..n].copy_from_slice(&data[..n]);
            if n == data.len() {
                st.pending.pop_front();
            } else {
                // Split chunks that exceed the read buffer; retain the remainder and original due time.
                let rest = data[n..].to_vec();
                let due = *due;
                st.pending[0] = (due, rest);
            }
            return Ok(n);
        }
        Ok(0)
    }

    fn write(&mut self, data: &[u8]) -> Result<usize, FlattenError> {
        let due = Instant::now() + self.delay;
        self.state
            .lock()
            .expect("Echo state lock poisoned")
            .pending
            .push_back((due, data.to_vec()));
        Ok(data.len())
    }

    fn set_params(&mut self, _cfg: &SerialConfig) -> Result<(), FlattenError> {
        Ok(()) // Baud has no effect on loopback; accept it for configuration compatibility.
    }

    fn set_signals(&mut self, dtr: Option<bool>, rts: Option<bool>) -> Result<(), FlattenError> {
        if let Some(v) = dtr {
            self.pins.dtr = v;
        }
        if let Some(v) = rts {
            self.pins.rts = v;
        }
        Ok(())
    }

    fn read_signals(&mut self) -> Result<PinStates, FlattenError> {
        Ok(PinStates {
            cts: true,
            dtr_known: true,
            rts_known: true,
            dsr: true,
            dcd: true,
            ri: false,
            ..self.pins
        })
    }

    fn set_break(&mut self, _d: Duration) -> Result<(), FlattenError> {
        Ok(())
    }

    fn flush_rx(&mut self) -> Result<(), FlattenError> {
        self.state
            .lock()
            .expect("Echo state lock poisoned")
            .pending
            .clear();
        Ok(())
    }

    fn flush_tx(&mut self) -> Result<(), FlattenError> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// gen: rate-controlled source
// ---------------------------------------------------------------------------

/// Rate-controlled generator transport.
///
/// Track byte credit with nanosecond precision, accruing according to elapsed time;
/// polling frequency does not accumulate error, and each read consumes at most one second of credit.
#[derive(Debug)]
pub struct GenTransport {
    bps: u64,
    // The URI sets initial throughput independently of the serial configuration.
    configured_baud: u32,
    pattern: Pattern,
    credit_ns: u128,
    last: Instant,
    phase: u64,
    line_no: u64,
    line_buf: Vec<u8>,
    line_pos: usize,
    pins: PinStates,
}

impl GenTransport {
    #[must_use]
    fn new(bps: u64, pattern: Pattern, configured_baud: u32) -> Self {
        Self {
            bps,
            configured_baud,
            pattern,
            credit_ns: 0,
            last: Instant::now(),
            phase: 0,
            line_no: 0,
            line_buf: Vec::new(),
            line_pos: 0,
            pins: PinStates::default(),
        }
    }

    fn next_line(&mut self) -> Vec<u8> {
        self.line_no += 1;
        let lvl = (self.line_no % 256) as u8;
        format!(
            "FLATTENCOM-GEN seq={:06} t={}.{:03} lvl=0x{:02X} crc=0x{:04X}\r\n",
            self.line_no,
            self.line_no * 12,
            (self.line_no * 345) % 1000,
            lvl,
            self.line_no % 0x10000
        )
        .into_bytes()
    }

    fn fill_line(&mut self) {
        if self.line_pos >= self.line_buf.len() {
            self.line_buf = self.next_line();
            self.line_pos = 0;
        }
    }
}

impl SerialTransport for GenTransport {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, FlattenError> {
        let now = Instant::now();
        self.credit_ns += now.duration_since(self.last).as_nanos();
        self.last = now;
        let ns_per_byte: u128 = 8_000_000_000 / u128::from(self.bps.max(1));
        // Available byte credit, capped at one second per read.
        let due = (self.credit_ns / ns_per_byte).min(u128::from(self.bps) / 8 + 1) as usize;
        let mut n = 0usize;
        match self.pattern {
            Pattern::Binary => {
                while n < due && n < buf.len() {
                    buf[n] = (self.phase % 256) as u8;
                    self.phase += 1;
                    n += 1;
                }
            }
            Pattern::Ascii => {
                while n < due && n < buf.len() {
                    self.fill_line();
                    let take = (due - n)
                        .min(buf.len() - n)
                        .min(self.line_buf.len() - self.line_pos);
                    buf[n..n + take]
                        .copy_from_slice(&self.line_buf[self.line_pos..self.line_pos + take]);
                    self.line_pos += take;
                    n += take;
                }
            }
        }
        // Deduct only emitted bytes; retain credit that did not fit the read buffer.
        let consumed = n as u128 * ns_per_byte;
        self.credit_ns = self.credit_ns.saturating_sub(consumed);
        // Cap accumulated credit at one second of output.
        let cap = ns_per_byte * (u128::from(self.bps) / 8 + 1);
        if self.credit_ns > cap {
            self.credit_ns = cap;
        }
        Ok(n)
    }

    fn write(&mut self, data: &[u8]) -> Result<usize, FlattenError> {
        Ok(data.len()) // Generator writes are accepted and ignored.
    }

    fn set_params(&mut self, cfg: &SerialConfig) -> Result<(), FlattenError> {
        if cfg.baud != self.configured_baud {
            self.bps = cfg.baud.into();
            self.configured_baud = cfg.baud;
        }
        Ok(())
    }

    fn set_signals(&mut self, dtr: Option<bool>, rts: Option<bool>) -> Result<(), FlattenError> {
        if let Some(v) = dtr {
            self.pins.dtr = v;
        }
        if let Some(v) = rts {
            self.pins.rts = v;
        }
        Ok(())
    }

    fn read_signals(&mut self) -> Result<PinStates, FlattenError> {
        Ok(PinStates {
            cts: true,
            dtr_known: true,
            rts_known: true,
            dsr: true,
            dcd: true,
            ri: false,
            ..self.pins
        })
    }

    fn set_break(&mut self, _d: Duration) -> Result<(), FlattenError> {
        Ok(())
    }

    fn flush_rx(&mut self) -> Result<(), FlattenError> {
        self.credit_ns = 0;
        self.line_pos = 0;
        self.line_buf.clear();
        Ok(())
    }

    fn flush_tx(&mut self) -> Result<(), FlattenError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 回环读写() {
        let mut t = EchoTransport::new(Duration::from_millis(1));
        let mut buf = [0u8; 64];
        assert_eq!(t.read(&mut buf).unwrap(), 0);
        t.write(b"hello").unwrap();
        std::thread::sleep(Duration::from_millis(5));
        let n = t.read(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"hello");
    }

    #[test]
    fn 回环拆帧() {
        let mut t = EchoTransport::new(Duration::from_millis(0));
        t.write(b"0123456789").unwrap();
        let mut small = [0u8; 4];
        assert_eq!(t.read(&mut small).unwrap(), 4);
        assert_eq!(&small, b"0123");
        let mut rest = [0u8; 8];
        assert_eq!(t.read(&mut rest).unwrap(), 6);
        assert_eq!(&rest[..6], b"456789");
    }

    #[test]
    fn 生成器限速() {
        let mut t = GenTransport::new(80_000, Pattern::Binary, 115_200); // 80_000 bps = 10 KB/s
        let mut buf = vec![0u8; 4096];
        // An immediate read has almost no accumulated credit.
        let n0 = t.read(&mut buf).unwrap();
        assert!(n0 <= 16, "首发应当极小:{n0}");
        std::thread::sleep(Duration::from_millis(100));
        let n1 = t.read(&mut buf).unwrap();
        assert!((800..1_200).contains(&n1), "100ms ≈ 1000B,得到 {n1}");
        // Binary pattern increments each byte.
        assert_eq!(&buf[..8], &[0, 1, 2, 3, 4, 5, 6, 7]);
    }

    #[test]
    fn 生成器文本行() {
        let mut t = GenTransport::new(1_000_000, Pattern::Ascii, 115_200);
        let mut buf = vec![0u8; 1024];
        std::thread::sleep(Duration::from_millis(10));
        let n = t.read(&mut buf).unwrap();
        let s = String::from_utf8_lossy(&buf[..n]).into_owned();
        // A complete first line includes sequence, timestamp, level and checksum, ending in CRLF.
        assert!(
            s.contains("FLATTENCOM-GEN seq=000001 t=12.345 lvl=0x01 crc=0x0001\r\n"),
            "{}",
            s.chars().take(96).collect::<String>()
        );
        // The read buffer may split the final line; require at least one complete line.
        assert!(s.contains("\r\n"));
    }

    #[test]
    fn query_解析() {
        let params = parse_query("bps=115200&pattern=binary&x=%20");
        assert_eq!(params[0], ("bps".into(), "115200".into()));
        assert_eq!(params[1], ("pattern".into(), "binary".into()));
        assert_eq!(params[2], ("x".into(), " ".into()));
    }

    #[test]
    fn generator_preserves_uri_rate_until_configured_baud_changes() {
        fn sample(t: &mut GenTransport) -> usize {
            // Two seconds of credit makes the per-read cap deterministic without sleeping.
            t.credit_ns = Duration::from_secs(2).as_nanos();
            t.last = Instant::now();
            t.read(&mut [0; 16_384]).unwrap()
        }

        let mut cfg = SerialConfig::new("virtual://gen?bps=800&pattern=binary");
        let mut t = GenTransport::new(800, Pattern::Binary, cfg.baud);
        assert_eq!(sample(&mut t), 101);
        t.set_params(&cfg).unwrap();
        assert_eq!(sample(&mut t), 101);
        cfg.label = Some("renamed".into());
        t.set_params(&cfg).unwrap();
        assert_eq!(sample(&mut t), 101);
        cfg.read_timeout_ms = 20;
        t.set_params(&cfg).unwrap();
        assert_eq!(sample(&mut t), 101);

        cfg.baud = 1_600;
        t.set_params(&cfg).unwrap();
        assert_eq!(sample(&mut t), 201);
        cfg.label = None;
        t.set_params(&cfg).unwrap();
        assert_eq!(sample(&mut t), 201);
        cfg.baud = 115_200;
        t.set_params(&cfg).unwrap();
        assert_eq!(sample(&mut t), 14_401);
    }
}
