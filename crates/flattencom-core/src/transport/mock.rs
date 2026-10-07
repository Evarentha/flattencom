/*
 * flattencom - Core Transport Mock
 *
 * Provides scripted transport reads, write capture and deterministic fault injection.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

//! Scripted transport and fault injection for tests.
//!
//! Test session reconnect, triggers, sequences and recording deterministically without hardware.
//! Reads follow a script, writes are captured, and opens can fail on selected attempts;
//! skip initial opens to exercise failures during reconnect rather than initial startup.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::FlattenError;
use crate::config::SerialConfig;
use crate::transport::{PinStates, SerialTransport, TransportFactory};

/// One scripted read: bytes or an injected failure.
#[derive(Debug, Clone)]
pub enum ReadStep {
    /// Return a data chunk.
    Data(Vec<u8>),
    /// Inject a fatal read error to exercise reconnect.
    Fail(FlattenError),
}

/// Fault-injection factory with scripted reads and configurable open failures.
///
/// Writes are shared across the factory so tests can observe transport instances owned by workers.
#[derive(Debug, Clone)]
pub struct MockFactory {
    script: Arc<Mutex<VecDeque<ReadStep>>>,
    open_failures: Arc<Mutex<VecDeque<FlattenError>>>,
    skip_first_opens: usize,
    opens: Arc<AtomicUsize>,
    /// All captured writes, exposed through the factory for tests.
    pub writes: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl MockFactory {
    /// Construct a factory that returns `steps` in read order.
    #[must_use]
    pub fn script(steps: Vec<ReadStep>) -> Self {
        Self {
            script: Arc::new(Mutex::new(steps.into())),
            open_failures: Arc::new(Mutex::new(VecDeque::new())),
            skip_first_opens: 0,
            opens: Arc::new(AtomicUsize::new(0)),
            writes: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Append read steps while the session is running.
    pub fn extend_script(&self, steps: Vec<ReadStep>) {
        self.script
            .lock()
            .expect("Mock lock poisoned")
            .extend(steps);
    }

    /// Insert a read step at the front.
    pub fn push_script_front(&self, step: ReadStep) {
        self.script
            .lock()
            .expect("Mock lock poisoned")
            .push_front(step);
    }

    /// Fail the next N opens, then resume successful opens.
    pub fn fail_opens(&self, failures: Vec<FlattenError>) {
        self.open_failures
            .lock()
            .expect("Mock lock poisoned")
            .extend(failures);
    }

    /// Skip N initial opens before injecting failures; N=1 permits initial startup
    /// and deterministically exercises reconnect exhaustion.
    #[must_use]
    pub fn skip_first_opens(mut self, n: usize) -> Self {
        self.skip_first_opens = n;
        self
    }

    /// Total open attempts.
    pub fn open_count(&self) -> usize {
        self.opens.load(Ordering::SeqCst)
    }
}

impl TransportFactory for MockFactory {
    fn open(&self, _cfg: &SerialConfig) -> Result<Box<dyn SerialTransport>, FlattenError> {
        let n = self.opens.fetch_add(1, Ordering::SeqCst);
        if n >= self.skip_first_opens
            && let Some(e) = self
                .open_failures
                .lock()
                .expect("Mock lock poisoned")
                .pop_front()
        {
            return Err(e);
        }
        Ok(Box::new(ScriptedTransport {
            script: Arc::clone(&self.script),
            writes: Arc::clone(&self.writes),
            pins: PinStates::default(),
        }))
    }

    fn kind(&self) -> &'static str {
        "mock"
    }
}

/// Scripted transport instance.
#[derive(Debug)]
pub struct ScriptedTransport {
    script: Arc<Mutex<VecDeque<ReadStep>>>,
    writes: Arc<Mutex<Vec<Vec<u8>>>>,
    pins: PinStates,
}

impl SerialTransport for ScriptedTransport {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, FlattenError> {
        let mut script = self.script.lock().expect("Mock lock poisoned");
        match script.pop_front() {
            None => Ok(0), // Empty script behaves like repeated idle timeouts.
            Some(ReadStep::Data(data)) if data.is_empty() => Ok(0),
            Some(ReadStep::Data(data)) => {
                let n = data.len().min(buf.len());
                buf[..n].copy_from_slice(&data[..n]);
                if n < data.len() {
                    // Put unread remainder back at the front when it exceeds the 4096-byte read buffer.
                    script.push_front(ReadStep::Data(data[n..].to_vec()));
                }
                Ok(n)
            }
            Some(ReadStep::Fail(e)) => Err(e),
        }
    }

    fn write(&mut self, data: &[u8]) -> Result<usize, FlattenError> {
        self.writes
            .lock()
            .expect("Mock lock poisoned")
            .push(data.to_vec());
        Ok(data.len())
    }

    fn set_params(&mut self, _cfg: &SerialConfig) -> Result<(), FlattenError> {
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
        Ok(self.pins)
    }

    fn set_break(&mut self, _d: Duration) -> Result<(), FlattenError> {
        Ok(())
    }

    fn flush_rx(&mut self) -> Result<(), FlattenError> {
        self.script.lock().expect("Mock lock poisoned").clear();
        Ok(())
    }

    fn flush_tx(&mut self) -> Result<(), FlattenError> {
        Ok(())
    }
}
