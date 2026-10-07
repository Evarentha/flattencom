/*
 * flattencom - CLI Terminal Process Tests
 *
 * Verifies signal cleanup and live configuration display using isolated PTYs.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

//! Each TUI runs in a child process so signal handlers never conflict with tests.
#![cfg(unix)]

use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::AsFd;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use nix::poll::{PollFd, PollFlags, poll};
use nix::pty::{Winsize, openpty};
use nix::sys::signal::{Signal, kill};
use nix::sys::termios::{LocalFlags, SetArg, Termios, tcgetattr, tcsetattr};
use nix::unistd::Pid;

struct Monitor {
    child: Child,
    master: File,
    slave: File,
    original: Termios,
    output: Vec<u8>,
}

impl Monitor {
    fn start() -> Self {
        let size = Winsize {
            ws_row: 24,
            ws_col: 120,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let pair = openpty(Some(&size), None).unwrap();
        let master = File::from(pair.master);
        let slave = File::from(pair.slave);
        let original = tcgetattr(&slave).unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_flattencom"))
            .args(["monitor", "virtual://echo"])
            .env("TERM", "xterm")
            .env("FLATTENCOM_LANG", "en")
            .stdin(Stdio::from(slave.try_clone().unwrap()))
            .stdout(Stdio::from(slave.try_clone().unwrap()))
            .stderr(Stdio::from(slave.try_clone().unwrap()))
            .spawn()
            .unwrap();
        let mut monitor = Self {
            child,
            master,
            slave,
            original,
            output: Vec::new(),
        };
        monitor.wait_for(|monitor| {
            String::from_utf8_lossy(&monitor.output).contains("virtual://echo @ 115200")
                && !tcgetattr(&monitor.slave)
                    .unwrap()
                    .local_flags
                    .contains(LocalFlags::ICANON)
        });
        monitor
    }

    fn read_output(&mut self) {
        let mut fds = [PollFd::new(self.master.as_fd(), PollFlags::POLLIN)];
        if poll(&mut fds, 20u16).unwrap() > 0 {
            let mut bytes = [0; 8192];
            let count = self.master.read(&mut bytes).unwrap();
            self.output.extend_from_slice(&bytes[..count]);
        }
    }

    fn wait_for(&mut self, condition: impl Fn(&Self) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !condition(self) {
            assert!(
                Instant::now() < deadline,
                "PTY output: {:?}",
                String::from_utf8_lossy(&self.output)
            );
            self.read_output();
        }
    }

    fn wait_exit(&mut self) -> ExitStatus {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            self.read_output();
            if let Some(status) = self.child.try_wait().unwrap() {
                // Drain any final terminal restoration sequence already queued.
                self.read_output();
                return status;
            }
            assert!(Instant::now() < deadline, "TUI failed to exit");
        }
    }
}

impl Drop for Monitor {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = tcsetattr(&self.slave, SetArg::TCSANOW, &self.original);
    }
}

#[test]
fn os_signals_restore_terminal_and_leave_alternate_screen() {
    for signal in [Signal::SIGINT, Signal::SIGTERM] {
        let mut monitor = Monitor::start();
        let pid = Pid::from_raw(i32::try_from(monitor.child.id()).unwrap());
        kill(pid, signal).unwrap();
        assert!(monitor.wait_exit().success(), "{signal:?}");
        assert_eq!(
            tcgetattr(&monitor.slave).unwrap(),
            monitor.original,
            "{signal:?}"
        );
        assert!(
            monitor
                .output
                .windows(8)
                .any(|bytes| bytes == b"\x1b[?1049l")
        );
    }
}

/// Reconstruct the title row, retaining cells omitted by incremental redraws.
fn title_text(output: &[u8]) -> String {
    let output = String::from_utf8_lossy(output);
    let escapes = regex::Regex::new(r"\x1b\[([0-9;?]*)([A-Za-z])").unwrap();
    let mut row = 1;
    let mut column = 0usize;
    let mut end = 0;
    let mut title = [' '; 120];
    for capture in escapes.captures_iter(&output) {
        let escape = capture.get(0).unwrap();
        for character in output[end..escape.start()].chars() {
            if row == 1
                && let Some(cell) = title.get_mut(column)
            {
                *cell = character;
            }
            column += 1;
        }
        if &capture[2] == "H" {
            let mut position = capture[1].split(';');
            row = position.next().unwrap().parse().unwrap_or(1);
            column = position
                .next()
                .unwrap_or("1")
                .parse::<usize>()
                .unwrap()
                .saturating_sub(1);
        }
        end = escape.end();
    }
    for character in output[end..].chars() {
        if row == 1
            && let Some(cell) = title.get_mut(column)
        {
            *cell = character;
        }
        column += 1;
    }
    title.iter().collect()
}

#[test]
fn live_baud_change_updates_terminal_title() {
    let mut monitor = Monitor::start();
    monitor.master.write_all(b"c57600\r").unwrap();
    monitor.wait_for(|monitor| title_text(&monitor.output).contains("virtual://echo @ 57600 |"));
    monitor.master.write_all(b"q").unwrap();
    assert!(monitor.wait_exit().success());
    assert_eq!(tcgetattr(&monitor.slave).unwrap(), monitor.original);
}
