/*
 * flattencom - Capture Writer Worker
 *
 * Writes capture records on a bounded worker queue and reports disk failures.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

//! Bounded capture writer; slow disks never hold the session state mutex.
use crate::{FlattenError, frame::Frame, record::Recorder};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

enum Entry {
    Frame(Frame),
    RxBoundary,
    Note(
        i64,
        String,
        String,
        String,
        Option<u64>,
        Option<mpsc::Sender<Result<(), String>>>,
    ),
}

/// A failed or saturated sink is disabled explicitly instead of silently losing records.
pub(crate) struct CaptureWorker {
    tx: Option<mpsc::SyncSender<Entry>>,
    errors: Arc<Mutex<Vec<String>>>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl CaptureWorker {
    pub(crate) fn start(mut sinks: Vec<Recorder>) -> Result<Self, FlattenError> {
        let (tx, rx) = mpsc::sync_channel(64);
        let errors = Arc::new(Mutex::new(Vec::new()));
        let failures = errors.clone();
        let thread = std::thread::Builder::new()
            .name("flattencom-capture".into())
            .spawn(move || {
                loop {
                    let result = match rx.recv_timeout(Duration::from_millis(50)) {
                        Ok(Entry::Frame(frame)) => sinks
                            .iter_mut()
                            .try_for_each(|sink| sink.write(&frame).map(|_| ())),
                        Ok(Entry::RxBoundary) => {
                            sinks.iter_mut().try_for_each(Recorder::rx_boundary)
                        }
                        Ok(Entry::Note(time, kind, label, source, seq, reply)) => {
                            let result = sinks.iter_mut().try_for_each(|sink| {
                                sink.annotation(time, &kind, &label, &source, seq)
                            });
                            if let Some(reply) = reply {
                                let _ = reply
                                    .send(result.as_ref().copied().map_err(ToString::to_string));
                            }
                            result
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            sinks.iter_mut().try_for_each(Recorder::flush_due)
                        }
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    };
                    if let Err(error) = result {
                        failures
                            .lock()
                            .expect("capture errors")
                            .push(error.to_string());
                        break;
                    }
                }
                for sink in &mut sinks {
                    if let Err(error) = sink.finish() {
                        failures
                            .lock()
                            .expect("capture errors")
                            .push(error.to_string());
                    }
                }
            })
            .map_err(|e| FlattenError::io(e.to_string()))?;
        Ok(Self {
            tx: Some(tx),
            errors,
            thread: Some(thread),
        })
    }
    fn submit(&mut self, entry: Entry) -> Result<(), FlattenError> {
        if !self.errors().is_empty() {
            return Err(FlattenError::io("Capture stopped after a write failure"));
        }
        if self
            .tx
            .as_ref()
            .is_none_or(|tx| tx.try_send(entry).is_err())
        {
            let message = "Capture stopped: disk writer queue is full or unavailable";
            self.errors
                .lock()
                .expect("capture errors")
                .push(message.into());
            self.tx.take();
            return Err(FlattenError::io(message));
        }
        Ok(())
    }
    pub(crate) fn frame(&mut self, frame: Frame) {
        let _ = self.submit(Entry::Frame(frame));
    }
    pub(crate) fn rx_boundary(&mut self) {
        let _ = self.submit(Entry::RxBoundary);
    }
    pub(crate) fn annotation(
        &mut self,
        time: i64,
        kind: &str,
        label: &str,
        source: &str,
        seq: Option<u64>,
    ) -> Result<(), FlattenError> {
        self.submit(Entry::Note(
            time,
            kind.into(),
            label.into(),
            source.into(),
            seq,
            None,
        ))
    }
    pub(crate) fn annotation_receipt(
        &mut self,
        time: i64,
        kind: &str,
        label: &str,
        source: &str,
        seq: Option<u64>,
    ) -> Result<mpsc::Receiver<Result<(), String>>, FlattenError> {
        let (tx, rx) = mpsc::channel();
        self.submit(Entry::Note(
            time,
            kind.into(),
            label.into(),
            source.into(),
            seq,
            Some(tx),
        ))?;
        Ok(rx)
    }
    pub(crate) fn errors(&self) -> Vec<String> {
        self.errors.lock().expect("capture errors").clone()
    }
    pub(crate) fn failure_state(&self) -> Arc<Mutex<Vec<String>>> {
        self.errors.clone()
    }
    pub(crate) fn finish(&mut self) {
        self.tx.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
impl Drop for CaptureWorker {
    fn drop(&mut self) {
        self.finish();
    }
}
