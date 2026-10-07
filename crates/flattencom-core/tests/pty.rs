/*
 * flattencom - flattencom Flattencom-Core Tests Pty
 *
 * Tests serial I/O, throughput, configuration and reconnect failures through Linux pseudo-terminals.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

//! Linux PTY integration tests exercise kernel serial IO without physical hardware.
//!
//! openpty creates a linked pair; the session opens `/dev/pts/N` on the slave side,
//! and the test uses the master to verify settings, sequences, throughput and reconnect budgets.
//!
//! Two PTY-specific considerations:
//! - **Lifetime**: the kernel destroys a PTY once all master/slave handles close;
//!   tests retain a slave keepalive handle so its path remains present.
//! - **Flow control**: the roughly 4 KiB PTY input buffer requires producer-side
//!   backpressure to avoid overflow; physical serial links are paced by baud rate.

#![cfg(target_os = "linux")]

use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::sync::Arc;
use std::time::{Duration, Instant};

use flattencom_core::config::{BufferPolicy, ReconnectPolicy, SerialConfig};
use flattencom_core::frame::Direction;
use flattencom_core::ids::SessionId;
use flattencom_core::session::{SessionHandle, SessionState};
use flattencom_core::transport::TransportRegistry;

use nix::pty::openpty;

/// Create a PTY pair: master file, slave path and slave keepalive handle.
fn pty_pair() -> (std::fs::File, String, std::fs::File) {
    let pair = openpty(None, None).expect("openpty 失败");
    // Resolve the slave path via /proc/self/fd; nix 0.29 openpty returns OwnedFd handles.
    let slave_path = std::fs::read_link(format!("/proc/self/fd/{}", pair.slave.as_raw_fd()))
        .expect("反查 slave 路径");
    let slave = slave_path.to_string_lossy().into_owned();
    let keepalive = std::fs::File::from(pair.slave); // Keep the PTY alive.
    let master = std::fs::File::from(pair.master);
    (master, slave, keepalive)
}

fn open_session(slave: &str, buffer: BufferPolicy) -> SessionHandle {
    let registry = Arc::new(TransportRegistry::new());
    let cfg = SerialConfig {
        buffer,
        ..SerialConfig::new(slave)
    };
    SessionHandle::open(SessionId::new(), cfg, registry).expect("打开 PTY 会话失败")
}

/// Wait until enough RX bytes have arrived.
fn wait_rx(session: &SessionHandle, min_bytes: u64, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if session.stats().rx_bytes >= min_bytes {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn pty_基础收发() {
    let (mut master, slave, _keep) = pty_pair();
    let session = open_session(&slave, BufferPolicy::default());

    // Write the master and read RX frames through the slave session.
    master.write_all(b"HELLO-PTY\r\n").unwrap();
    master.flush().unwrap();
    assert!(
        wait_rx(&session, 11, Duration::from_secs(3)),
        "应收到 11B:{:?}",
        session.stats()
    );
    let page = session.read_frames(None, 1 << 20);
    let rx = page
        .frames
        .iter()
        .find(|f| f.dir == Direction::Rx)
        .expect("无 RX 帧");
    assert_eq!(*rx.data, b"HELLO-PTY\r\n".to_vec());

    // Send through the session and read from the master.
    session.send(b"PONG\r\n".to_vec()).unwrap();
    let mut buf = [0u8; 64];
    let n = master.read(&mut buf).unwrap();
    assert_eq!(&buf[..n], b"PONG\r\n");

    session.close();
}

#[test]
fn pty_吞吐压测不丢字节() {
    let (master, slave, _keep) = pty_pair();
    // Share the master through Arc so it outlives the writing thread until assertions finish.
    // Otherwise dropping the master closes the PTY, discards trailing bytes and causes EIO.
    let master = Arc::new(std::sync::Mutex::new(master));
    // Predictable byte stream: 1000 chunks of 1 KiB each.
    const CHUNKS: usize = 1_000;
    const CHUNK: usize = 1_024;
    let session = open_session(
        &slave,
        BufferPolicy {
            max_frames: 100_000,
            max_bytes: 32 * 1024 * 1024,
        },
    );

    // Apply consumption-based backpressure to avoid overflowing the PTY's 4 KiB input buffer;
    // physical serial traffic is instead paced by its baud rate.
    let s2 = session.clone();
    let writer_master = Arc::clone(&master);
    let writer = std::thread::spawn(move || {
        let mut offset = 0usize;
        let mut master = writer_master.lock().unwrap();
        for i in 0..CHUNKS {
            let written_before = (i * CHUNK) as u64;
            while s2.stats().rx_bytes + 2 * 1024 < written_before {
                std::thread::sleep(Duration::from_millis(1));
            }
            let mut chunk = vec![0u8; CHUNK];
            for b in &mut chunk {
                *b = ((offset + i * CHUNK) % 256) as u8;
                offset += 1;
            }
            master.write_all(&chunk).unwrap();
            master.flush().unwrap();
        }
    });

    let total: u64 = (CHUNKS * CHUNK) as u64;
    let ok = wait_rx(&session, total, Duration::from_secs(30));
    let writer_done = writer.is_finished();
    assert!(
        ok,
        "应全部到达:{:?}/{total},writer 完成={writer_done},状态={:?}",
        session.stats(),
        session.state()
    );
    writer.join().unwrap();

    let stats = session.stats();
    assert_eq!(stats.rx_bytes, total, "字节数精确一致");
    assert_eq!(stats.dropped_rx, 0, "缓冲不溢出不丢帧(32MiB 容量)");
    assert_eq!(stats.errors.total(), 0, "无读错误:{:?}", stats.errors);

    // Reassemble frames in sequence order and compare every byte with the transmitted pattern.
    let page = session.read_frames(None, 256 * 1024 * 1024);
    let mut stream = Vec::with_capacity(total as usize);
    for f in &page.frames {
        if f.dir == Direction::Rx {
            stream.extend_from_slice(&f.data);
        }
    }
    assert_eq!(stream.len() as u64, total, "拼接长度一致");
    for (i, &b) in stream.iter().enumerate() {
        assert_eq!(b, (i % 256) as u8, "第 {i} 字节不匹配(序号/内容错乱)");
    }
    session.close();
}

#[test]
fn pty_读故障预算收敛() {
    // Open the slave while the master lives; reopening after master closure returns ENXIO.
    // Close the master so reads fail despite the retained slave path,
    // then verify reconnect attempts exhaust the budget rather than looping forever.
    let (master, slave, _keep) = pty_pair();
    let registry = Arc::new(TransportRegistry::new());
    let cfg = SerialConfig {
        auto_reconnect: ReconnectPolicy::Enabled {
            max_attempts: 3,
            initial_ms: 5,
            max_ms: 20,
        },
        ..SerialConfig::new(&slave)
    };
    let session = SessionHandle::open(SessionId::new(), cfg, registry).unwrap();
    drop(master); // Simulate unplugging.
    let started = Instant::now();
    let state = session.wait_closed(Duration::from_secs(30));
    assert!(
        matches!(state, SessionState::Failed { .. }),
        "超过重试次数后应为 Failed: {state:?}"
    );
    // Reopening after master closure fails with ENXIO and must not count as a successful reconnect.
    // Termination should be well within the retry budget: 5/10/20 ms backoff plus open overhead.
    assert!(started.elapsed() < Duration::from_secs(10), "收敛应迅速");
    assert!(
        session.stats().errors.total() >= 1,
        "应记录过错误:{:?}",
        session.stats().errors
    );
}

#[test]
fn pty_在线变参() {
    let (mut master, slave, _keep) = pty_pair();
    let session = open_session(&slave, BufferPolicy::default());
    let cfg = session
        .configure(flattencom_core::config::ConfigPatch {
            baud: Some(230_400),
            label: Some(Some("pty-test".into())),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(cfg.baud, 230_400);
    assert_eq!(session.config().label.as_deref(), Some("pty-test"));
    // IO still works after reconfiguration.
    master.write_all(b"AFTER-CONFIG\r\n").unwrap();
    master.flush().unwrap();
    assert!(wait_rx(&session, 14, Duration::from_secs(3)));
    session.close();
}
