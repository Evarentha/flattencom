/*
 * flattencom - Core Session
 *
 * Owns serial reader and writer threads, reconnects, frame history, recording and trigger execution.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

//! Session runtime and worker coordination.
//!
//! ## Thread model
//!
//! ```text
//! Driver reader --> bounded RX queue --> receive processor
//!  transport read     Arc<Mutex> transport     command writer
//!  receive processing --> sequence allocation under session lock
//!  decode/trigger/store                       write --> TX frame
//! ```
//!
//! - Reader and writer share one serial handle (`Arc<Mutex<>>`, short read timeout).
//!   Live configuration affects both directions without cloning OS handles.
//! - Allocate shared RX/TX sequences under the session lock; reconnect never resets them.
//! - Driver reads feed a bounded receive-processing queue; decoder and disk
//!   workers do not hold the session-state mutex during external IO.
//! - Connection generations reset RX/TX reassembly in processing order. Reconnect
//!   retains history; RX flush has a separate generation for view exclusion.
//! - The last serial worker retires decoders and finalizes capture after RX drain
//!   and TX commit. Failed sessions keep their state and configuration for inspection.
//!
//! ## Change-wait protocol for daemon and direct clients
//!
//! State changes increment `rev` and call `notify_all`; clients retain their observed revision,
//! wait through [`SessionHandle::wait_change`], then fetch frames incrementally.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::Builder;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::FlattenError;
use crate::config::{ConfigPatch, SerialConfig};
use crate::decode::{ChunkCtx, Decoder, DecoderRegistry, DecoderSpec};
use crate::filter::{CompiledFilter, FilterSpec};
use crate::frame::{Direction, Frame};
use crate::ids::SessionId;
use crate::record::{ExportFormat, ExportResult, Recorder, export as export_frames};
use crate::stats::{Counters, SessionStats};
use crate::store::FrameStore;
use crate::transport::SerialTransport;
use crate::trigger::{self, TriggerFire, TriggerSpec};

pub use crate::transport::PinStates;

/// Command queue capacity; full trigger-response queues report errors without blocking RX.
const CMD_CHANNEL_CAP: usize = 256;

/// Command response timeout.
const REPLY_TIMEOUT: Duration = Duration::from_secs(5);
struct Command {
    payload: Cmd,
    deadline: Instant,
    cancelled: Arc<AtomicBool>,
    #[cfg(test)]
    dispatched: Option<SyncSender<()>>,
    #[cfg(test)]
    configuring: Option<SyncSender<()>>,
}
impl Command {
    fn new(command: Cmd) -> Self {
        Self {
            payload: command,
            deadline: Instant::now() + REPLY_TIMEOUT,
            cancelled: Arc::default(),
            #[cfg(test)]
            dispatched: None,
            #[cfg(test)]
            configuring: None,
        }
    }
}
static ACTIVE_TRIGGER_COMMANDS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// Session state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum SessionState {
    /// Connected and ready for I/O.
    Connected,
    /// Reconnecting at the given attempt number.
    Reconnecting {
        /// Attempt number, starting at 1.
        attempt: u32,
    },
    /// Closed normally or after reconnect attempts failed.
    Closed,
    /// Unrecoverable error.
    Failed {
        /// Error description.
        error: String,
    },
}

/// Incremental frame page.
///
/// `next_seq` is the right boundary of the inspected window, including filtered-out frames;
/// use it as the next cursor to avoid duplicates and omissions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FramesPage {
    /// Returned frames after session filtering.
    pub frames: Vec<Frame>,
    /// Cursor for the next `since_seq` request.
    pub next_seq: u64,
    /// Cumulative buffer-eviction count.
    pub dropped_rx: u64,
    /// Whether the caller has caught up with currently available data.
    pub up_to_date: bool,
    /// Earliest retained sequence, possibly nonzero after eviction.
    pub first_seq: Option<u64>,
    /// Latest retained sequence.
    pub last_seq: Option<u64>,
}

/// Writer commands with optional response channels.
enum Cmd {
    Write {
        data: Vec<u8>,
        source: String,
        reply: Option<std::sync::mpsc::Sender<Result<u64, FlattenError>>>,
    },
    SetSignals {
        dtr: Option<bool>,
        rts: Option<bool>,
        reply: Option<std::sync::mpsc::Sender<Result<PinStates, FlattenError>>>,
    },
    ReadSignals {
        reply: Option<std::sync::mpsc::Sender<Result<PinStates, FlattenError>>>,
    },
    SetBreak {
        duration: Duration,
        reply: Option<std::sync::mpsc::Sender<Result<(), FlattenError>>>,
    },
    FlushRx {
        reply: Option<std::sync::mpsc::Sender<Result<u64, FlattenError>>>,
    },
    FlushTx {
        connection: u64,
        reply: Option<std::sync::mpsc::Sender<Result<(), FlattenError>>>,
    },
    Configure {
        patch: ConfigPatch,
        reply: Option<std::sync::mpsc::Sender<Result<SerialConfig, FlattenError>>>,
    },
}

/// Mutable session state accessed under the session mutex.
struct Inner {
    boots: crate::boot::BootDetector,
    store: FrameStore,
    // TX history is independent of noisy RX and receive-buffer clears.
    sent: FrameStore,
    counters: Counters,
    state: SessionState,
    decoder_config: Option<DecoderSpec>,
    read_filter: Option<CompiledFilter>,
    triggers: Vec<trigger::Trigger>,
    fires: VecDeque<TriggerFire>,
    capture: Option<crate::capture_worker::CaptureWorker>,
    capture_errors: Arc<Mutex<Vec<String>>>,
    config: SerialConfig,
    // Sampled under the transport lock and checked under the state lock at RX commit.
    rx_generation: u64,
    // Independent of view flushes: reconnect retains history but restarts reassembly.
    connection_generation: u64,
    // RX processor order, independent of interleaved TX completion generations.
    capture_rx_connection: Option<u64>,
    rev: u64,
}

impl Inner {
    /// Enqueue recording and retain the frame; report recorder failures independently of serial IO.
    fn ingest_and_record(&mut self, frame: Frame, retain: bool, connection: u64) {
        if frame.dir == Direction::Rx {
            if self
                .capture_rx_connection
                .is_some_and(|old| old != connection)
            {
                self.boots.connection_boundary();
                if let Some(capture) = &mut self.capture {
                    capture.rx_boundary();
                }
            }
            self.capture_rx_connection = Some(connection);
        }
        if let Some(capture) = &mut self.capture {
            capture.frame(frame.clone());
        }
        for event in self.boots.feed(&frame) {
            if let Some(capture) = &mut self.capture {
                let _ = capture.annotation(
                    event.t_us,
                    "BOOT",
                    &format!("Boot #{}: {}", event.number, event.banner),
                    "capture-detector",
                    Some(event.seq),
                );
            }
        }
        if frame.dir == Direction::Tx {
            self.sent.push(frame.clone());
        }
        if retain {
            self.store.push(frame);
        }
    }
}

/// State shared by workers and client handles.
#[derive(Default)]
struct Decoders {
    rx: Option<Box<dyn Decoder>>,
    tx: Option<Box<dyn Decoder>>,
    spec: Option<DecoderSpec>,
    rx_generation: Option<(u64, u64)>,
    tx_generation: Option<u64>,
}
struct Shared {
    inner: Mutex<Inner>,
    decoders: Mutex<Decoders>,
    changed: Condvar,
    closed: AtomicBool,
    workers: AtomicUsize,
    #[cfg(test)]
    decoder_built: Mutex<Option<SyncSender<()>>>,
    start: Instant,
}

impl Shared {
    fn decode(&self, frame: Frame, connection: u64, rx_generation: u64) -> Frame {
        let mut decoders = self.decoders.lock().expect("decoder state");
        let reset = if frame.dir == Direction::Rx {
            let generation = (connection, rx_generation);
            let reset = decoders
                .rx_generation
                .is_some_and(|previous| previous != generation);
            decoders.rx_generation = Some(generation);
            reset
        } else {
            let reset = decoders
                .tx_generation
                .is_some_and(|previous| previous != connection);
            decoders.tx_generation = Some(connection);
            reset
        };
        if reset {
            // Reset in each processor's order, not in the reconnecting reader:
            // old queued RX and successful in-flight TX still use their old decoder.
            let rebuilt = decoders
                .spec
                .as_ref()
                .map(DecoderRegistry::build)
                .transpose();
            let decoder = if frame.dir == Direction::Rx {
                &mut decoders.rx
            } else {
                &mut decoders.tx
            };
            match rebuilt {
                Ok(rebuilt) => *decoder = rebuilt,
                Err(error) => {
                    *decoder = None;
                    return frame.with_decoded(Some(crate::frame::DecodedInfo::error(
                        "decoder",
                        format!("Decoder reset failed: {error}"),
                        vec![],
                    )));
                }
            }
        }
        let decoder = if frame.dir == Direction::Rx {
            &mut decoders.rx
        } else {
            &mut decoders.tx
        };
        let info = decoder.as_mut().and_then(|d| {
            d.feed(&ChunkCtx {
                dir: frame.dir,
                t_us: frame.t_us,
                mono_us: frame.mono_us,
                data: &frame.data,
            })
        });
        frame.with_decoded(info)
    }
    /// Current wall-clock and monotonic timestamps in microseconds.
    fn clocks_now(&self) -> (i64, u64) {
        let t_us = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_micros() as i64);
        (t_us, self.start.elapsed().as_micros() as u64)
    }

    fn lock_inner(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().expect("Session lock poisoned")
    }

    fn worker_finished(&self) {
        self.closed.store(true, Ordering::Relaxed);
        if self.workers.fetch_sub(1, Ordering::AcqRel) != 1 {
            return;
        }
        // The reader includes its drained RX processor; the writer has committed
        // every successfully sent tail. No producer can use capture after this point.
        let retired = std::mem::take(&mut *self.decoders.lock().expect("decoder state"));
        drop(retired);
        let capture = self.lock_inner().capture.take();
        drop(capture);
        let mut g = self.lock_inner();
        if !matches!(g.state, SessionState::Failed { .. }) {
            g.state = SessionState::Closed;
        }
        g.rev += 1;
        drop(g);
        self.changed.notify_all();
    }
}

/// The last serial worker finalizes resources without joining itself or spawning a coordinator.
struct WorkerCompletion(Arc<Shared>);
impl Drop for WorkerCompletion {
    fn drop(&mut self) {
        self.0.worker_finished();
    }
}

/// Shared session core; closing the final handle stops the workers.
struct SessionCore {
    id: SessionId,
    shared: Arc<Shared>,
    cmd_tx: Arc<Mutex<Option<SyncSender<Command>>>>,
    threads: Mutex<Vec<std::thread::JoinHandle<()>>>,
}

impl SessionCore {
    fn close_now(&self) {
        if !self.shared.closed.swap(true, Ordering::Relaxed) {
            // Remove the client sender; both workers observe the close flag,
            // and the writer uses recv_timeout, so a full queue cannot block close.
            self.cmd_tx.lock().expect("Command lock poisoned").take();
            self.shared.changed.notify_all();
        }
        // Join outside the state/transport locks. When close returns the port is
        // released, which is important for flash tools and immediate reopen.
        // Keep shutdown ownership until capture finalization completes. Other close
        // callers must not mistake removed join handles for completed shutdown.
        let mut threads = self.threads.lock().expect("Thread handle lock poisoned");
        for thread in threads.drain(..) {
            let _ = thread.join();
        }
        // The last joined worker has also finalized capture and retired decoders.
        drop(threads);
    }
}

impl Drop for SessionCore {
    fn drop(&mut self) {
        // Close only after all handles sharing this core have been released.
        // Worker-owned Arc<Shared> references do not affect this decision.
        self.close_now();
    }
}

/// Cloneable session handle; the final handle closes the session.
#[derive(Clone)]
pub struct SessionHandle {
    core: Arc<SessionCore>,
}

impl std::fmt::Debug for SessionHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionHandle")
            .field("id", &self.core.id)
            .field("state", &self.core.shared.lock_inner().state)
            .finish()
    }
}

impl SessionHandle {
    /// Add an annotation to the human-readable capture without modifying device bytes.
    pub fn record_annotation(
        &self,
        t_us: i64,
        kind: &str,
        label: &str,
        source: &str,
        seq: Option<u64>,
    ) -> Result<(), FlattenError> {
        let mut g = self.core.shared.lock_inner();
        let receipt = g
            .capture
            .as_mut()
            .map(|c| c.annotation_receipt(t_us, kind, label, source, seq))
            .transpose()?;
        drop(g);
        if let Some(receipt) = receipt {
            receipt
                .recv_timeout(REPLY_TIMEOUT)
                .map_err(|e| FlattenError::io(e.to_string()))?
                .map_err(FlattenError::io)?;
        }
        Ok(())
    }
    /// Successful transmitted chunks in capture order, independent of RX filters/eviction.
    /// A TX frame means transport write success, not device command execution.
    pub fn read_sent(&self, cursor: Option<u64>, max_bytes: u64) -> FramesPage {
        let g = self.core.shared.lock_inner();
        let mut frames = Vec::new();
        let mut bytes = 0;
        let mut next_seq = g.store.next_seq();
        for frame in g.sent.iter().filter(|f| f.seq >= cursor.unwrap_or(0)) {
            let size = frame.wire_size_hint() as u64;
            if frames.len() >= 128 || (!frames.is_empty() && bytes + size > max_bytes) {
                next_seq = frame.seq;
                break;
            }
            bytes += size;
            frames.push(frame.clone());
        }
        FramesPage {
            frames,
            next_seq,
            dropped_rx: g.sent.dropped(),
            up_to_date: next_seq == g.store.next_seq(),
            first_seq: g.sent.first_seq(),
            last_seq: g.sent.last_seq(),
        }
    }
    /// Boot recognition continues when displays are paused or detached.
    pub fn boot_events(&self) -> Vec<crate::boot::BootEvent> {
        self.core.shared.lock_inner().boots.events()
    }
    /// Validate settings, open the transport through its factory, and start workers.
    pub fn open(
        id: SessionId,
        config: SerialConfig,
        factory: Arc<dyn crate::transport::TransportFactory>,
    ) -> Result<Self, FlattenError> {
        config.validate()?;
        let config = factory.prepare_config(&config);
        // Prepare sinks before opening the serial device (opening can reset a board).
        // If saving cannot start, report the error instead of silently losing boot logs.
        let rx_recorder = config
            .record_rx_to
            .as_ref()
            .map(Recorder::open_rx)
            .transpose()?;
        let recorder = match &config.record_to {
            Some(p) if !crate::record::is_readable(p) => Some(Recorder::open(p)?),
            Some(_) | None => None,
        };
        let mut readable = config
            .record_to
            .as_ref()
            .filter(|p| crate::record::is_readable(p))
            .map(Recorder::open_readable)
            .transpose()?;
        if let Some(recorder) = &mut readable {
            recorder.set_retention(config.record_keep_segments);
        }
        let transport = factory.open(&config)?;
        let sinks: Vec<_> = [recorder, rx_recorder, readable]
            .into_iter()
            .flatten()
            .collect();
        let capture = if sinks.is_empty() {
            None
        } else {
            Some(crate::capture_worker::CaptureWorker::start(sinks)?)
        };
        let transport = Arc::new(Mutex::new(transport));
        let (cmd_tx, cmd_rx) = sync_channel::<Command>(CMD_CHANNEL_CAP);
        let inner = Inner {
            boots: crate::boot::BootDetector::default(),
            store: FrameStore::new(&config.buffer),
            sent: FrameStore::new(&crate::config::BufferPolicy {
                max_frames: 2000,
                max_bytes: 8 * 1024 * 1024,
            }),
            counters: Counters::new(),
            state: SessionState::Connected,
            decoder_config: None,
            read_filter: None,
            triggers: Vec::new(),
            fires: VecDeque::new(),
            capture_errors: capture.as_ref().map_or_else(
                || Arc::new(Mutex::new(Vec::new())),
                crate::capture_worker::CaptureWorker::failure_state,
            ),
            capture,
            config: config.clone(),
            rx_generation: 0,
            connection_generation: 0,
            capture_rx_connection: None,
            rev: 0,
        };
        let shared = Arc::new(Shared {
            inner: Mutex::new(inner),
            decoders: Mutex::new(Decoders::default()),
            changed: Condvar::new(),
            closed: AtomicBool::new(false),
            workers: AtomicUsize::new(2),
            #[cfg(test)]
            decoder_built: Mutex::new(None),
            start: Instant::now(),
        });
        // Reader, writer and handle each retain the command sender; protect handle ownership with a mutex.
        let cmd_tx_handle = cmd_tx.clone();
        let reader_shared = Arc::clone(&shared);
        let reader_transport = Arc::clone(&transport);
        let writer_shared = Arc::clone(&shared);
        let writer_transport = Arc::clone(&transport);
        let writer_commands = cmd_tx.clone();
        let reader = Builder::new()
            .name(format!("flattencom-reader-{id}"))
            .spawn(move || {
                let _completion = WorkerCompletion(reader_shared.clone());
                reader_loop(reader_shared, reader_transport, cmd_tx, factory);
            })
            .map_err(|e| {
                FlattenError::io(crate::tr!("Failed to start reader thread: {e}", e = e))
            })?;
        let writer = Builder::new()
            .name(format!("flattencom-writer-{id}"))
            .spawn(move || {
                let _completion = WorkerCompletion(writer_shared.clone());
                writer_loop(writer_shared, writer_transport, cmd_rx, writer_commands);
            });
        let writer = match writer {
            Ok(writer) => writer,
            Err(e) => {
                shared.closed.store(true, Ordering::Relaxed);
                shared.worker_finished(); // Release the unstarted writer's reserved slot.
                let _ = reader.join();
                return Err(FlattenError::io(e.to_string()));
            }
        };
        // Workers are joined on close; capture is drained after serial workers.
        Ok(Self {
            core: Arc::new(SessionCore {
                id,
                shared,
                cmd_tx: Arc::new(Mutex::new(Some(cmd_tx_handle))),
                threads: Mutex::new(vec![reader, writer]),
            }),
        })
    }

    /// Session ID.
    #[must_use]
    pub fn id(&self) -> SessionId {
        self.core.id
    }

    /// Current configuration snapshot.
    #[must_use]
    pub fn config(&self) -> SerialConfig {
        self.core.shared.lock_inner().config.clone()
    }

    /// Current state.
    #[must_use]
    pub fn state(&self) -> SessionState {
        self.core.shared.lock_inner().state.clone()
    }

    /// Statistics snapshot.
    #[must_use]
    pub fn stats(&self) -> SessionStats {
        let now = Instant::now();
        let g = self.core.shared.lock_inner();
        SessionStats {
            rx_bytes: g.counters.rx_bytes,
            tx_bytes: g.counters.tx_bytes,
            rx_frames: g.counters.rx_frames,
            tx_frames: g.counters.tx_frames,
            reconnects: g.counters.reconnects,
            dropped_rx: g.store.dropped(),
            recording: crate::record::RecordingStatus {
                rx_path: g.config.record_rx_to.clone(),
                jsonl_path: g
                    .config
                    .record_to
                    .clone()
                    .filter(|p| !crate::record::is_readable(p)),
                readable_path: g
                    .config
                    .record_to
                    .as_deref()
                    .filter(|p| crate::record::is_readable(p))
                    .map(std::path::Path::to_path_buf),
                active: matches!(
                    g.state,
                    SessionState::Connected | SessionState::Reconnecting { .. }
                ) && g.capture.as_ref().is_some_and(|c| c.errors().is_empty()),
                errors: g.capture_errors.lock().expect("capture errors").clone(),
            },
            errors: g.counters.errors,
            rx_rate_bps: g.counters.rx_rate.rate_bps(now),
            tx_rate_bps: g.counters.tx_rate.rate_bps(now),
            buffer: crate::store::BufferLevel {
                frames: g.store.len(),
                bytes: g.store.bytes(),
                dropped: g.store.dropped(),
                first_seq: g.store.first_seq(),
                last_seq: g.store.last_seq(),
            },
        }
    }

    /// Send a frame and return its TX sequence.
    pub fn send(&self, data: Vec<u8>) -> Result<u64, FlattenError> {
        self.send_from(data, "embedded-cli".into())
    }

    /// Send with an explicit operation source, persisted in the structured capture.
    /// If reconnect interrupts a partial write, record its successful prefix and
    /// return an error without sending the suffix on the replacement connection.
    pub fn send_from(&self, data: Vec<u8>, source: String) -> Result<u64, FlattenError> {
        if data.len() > 1024 * 1024 {
            return Err(FlattenError::InvalidConfig {
                field: "data".into(),
                reason: "single send limited to 1 MiB; use send_file for larger streams".into(),
            });
        }
        self.roundtrip(
            |reply| Cmd::Write {
                data,
                source,
                reply: Some(reply),
            },
            |r| r,
        )
    }

    /// Set DTR/RTS, preserving omitted lines, and return all pin states.
    pub fn set_signals(
        &self,
        dtr: Option<bool>,
        rts: Option<bool>,
    ) -> Result<PinStates, FlattenError> {
        self.roundtrip(
            |reply| Cmd::SetSignals {
                dtr,
                rts,
                reply: Some(reply),
            },
            |r| r,
        )
    }

    /// Read control-line states.
    pub fn read_signals(&self) -> Result<PinStates, FlattenError> {
        self.roundtrip(|reply| Cmd::ReadSignals { reply: Some(reply) }, |r| r)
    }

    /// Send a BREAK signal.
    pub fn send_break(&self, duration: Duration) -> Result<(), FlattenError> {
        if duration.is_zero() || duration > Duration::from_secs(5) {
            return Err(FlattenError::InvalidConfig {
                field: "break".into(),
                reason: "duration must be 1..=5000ms".into(),
            });
        }
        self.roundtrip(
            |reply| Cmd::SetBreak {
                duration,
                reply: Some(reply),
            },
            |r| r,
        )
    }

    /// Clear driver RX and retained RX frames; return the number of retained frames removed.
    /// Already received chunks still contribute to recording and statistics, but
    /// queued or decoding chunks from before this boundary cannot reenter the view.
    /// RX decoder reassembly restarts before the first post-boundary chunk;
    /// the configured decoder specification and TX decoder state are preserved.
    pub fn flush_rx(&self) -> Result<u64, FlattenError> {
        self.roundtrip(|reply| Cmd::FlushRx { reply: Some(reply) }, |r| r)
    }

    /// Wait for the driver transmit buffer to drain within the command budget.
    /// A connection change during the operation fails instead of querying a replacement buffer.
    pub fn flush_tx(&self) -> Result<(), FlattenError> {
        self.flush_tx_until(Instant::now() + REPLY_TIMEOUT)
    }

    fn flush_tx_until(&self, deadline: Instant) -> Result<(), FlattenError> {
        let connection = self.core.shared.lock_inner().connection_generation;
        loop {
            match self.roundtrip_until(
                deadline,
                |reply| Cmd::FlushTx {
                    connection,
                    reply: Some(reply),
                },
                |r| r,
            ) {
                Err(FlattenError::Timeout(_)) if Instant::now() < deadline => {
                    std::thread::sleep(
                        Duration::from_millis(5)
                            .min(deadline.saturating_duration_since(Instant::now())),
                    );
                }
                result => return result,
            }
        }
    }

    /// Apply a live configuration patch and return the updated snapshot.
    pub fn configure(&self, patch: ConfigPatch) -> Result<SerialConfig, FlattenError> {
        if patch.is_empty() {
            return Ok(self.config());
        }
        self.roundtrip(
            |reply| Cmd::Configure {
                patch,
                reply: Some(reply),
            },
            |r| r,
        )
    }

    /// Set a decoder, or disable decoding with `None`; closed sessions reject changes.
    pub fn set_decoder(&self, spec: Option<DecoderSpec>) -> Result<(), FlattenError> {
        if self.core.shared.closed.load(Ordering::Relaxed) {
            return Err(FlattenError::SessionClosed(self.core.id.to_string()));
        }
        let compiled = match &spec {
            Some(s) => Some(DecoderRegistry::build(s)?),
            None => None,
        };
        let tx_compiled = spec.as_ref().map(DecoderRegistry::build).transpose()?;
        #[cfg(test)]
        if let Some(built) = self.core.shared.decoder_built.lock().unwrap().take() {
            let _ = built.send(());
        }
        let mut decoders = self.core.shared.decoders.lock().expect("decoder state");
        // Builds run outside locks. Shutdown may have completed while they ran.
        if self.core.shared.closed.load(Ordering::Relaxed) {
            return Err(FlattenError::SessionClosed(self.core.id.to_string()));
        }
        let retired = std::mem::replace(
            &mut *decoders,
            Decoders {
                rx: compiled,
                tx: tx_compiled,
                spec: spec.clone(),
                rx_generation: None,
                tx_generation: None,
            },
        );
        let mut g = self.core.shared.lock_inner();
        g.decoder_config = spec;
        g.rev += 1;
        drop(g);
        drop(decoders);
        drop(retired);
        self.core.shared.changed.notify_all();
        Ok(())
    }

    /// Current decoder identifier.
    #[must_use]
    pub fn decoder_spec(&self) -> Option<String> {
        self.core
            .shared
            .lock_inner()
            .decoder_config
            .as_ref()
            .map(|d| d.name.clone())
    }

    /// Complete decoder specification, including role and plugin options.
    pub fn decoder_config(&self) -> Option<DecoderSpec> {
        self.core.shared.lock_inner().decoder_config.clone()
    }

    /// Set a session read filter; recording, triggers and statistics are unaffected.
    pub fn set_read_filter(&self, spec: Option<FilterSpec>) -> Result<(), FlattenError> {
        let compiled = match &spec {
            Some(s) if !s.is_empty() => Some(CompiledFilter::compile(s)?),
            _ => None,
        };
        let mut g = self.core.shared.lock_inner();
        g.read_filter = compiled;
        g.rev += 1;
        drop(g);
        self.core.shared.changed.notify_all();
        Ok(())
    }

    /// Replace all triggers.
    pub fn set_triggers(&self, specs: Vec<TriggerSpec>) -> Result<(), FlattenError> {
        let mut compiled = Vec::with_capacity(specs.len());
        for s in specs {
            compiled.push(trigger::Trigger::compile(s)?);
        }
        let mut g = self.core.shared.lock_inner();
        g.triggers = compiled;
        g.rev += 1;
        drop(g);
        self.core.shared.changed.notify_all();
        Ok(())
    }

    /// Trigger specifications for RPC/MCP inspection.
    #[must_use]
    pub fn trigger_specs(&self) -> Vec<TriggerSpec> {
        self.core
            .shared
            .lock_inner()
            .triggers
            .iter()
            .map(|t| t.spec.clone())
            .collect()
    }

    /// Read trigger history without clearing it.
    #[must_use]
    pub fn trigger_fires(&self) -> Vec<TriggerFire> {
        self.core
            .shared
            .lock_inner()
            .fires
            .iter()
            .cloned()
            .collect()
    }

    /// Take and clear trigger history.
    #[must_use]
    pub fn drain_trigger_fires(&self) -> Vec<TriggerFire> {
        let mut g = self.core.shared.lock_inner();
        std::mem::take(&mut g.fires).into_iter().collect()
    }

    /// Read incrementally; `None` starts at the buffer head and `max_bytes` is a soft limit.
    #[must_use]
    pub fn read_frames(&self, since_seq: Option<u64>, max_bytes: u64) -> FramesPage {
        let mut since = since_seq;
        let mut out = Vec::new();
        let mut out_bytes = 0u64;
        let mut up_to_date = false;
        let g = self.core.shared.lock_inner();
        let next_total = g.store.next_seq();
        loop {
            let remaining = max_bytes.saturating_sub(out_bytes).max(1);
            let (frames, next) = g.store.read_since(since, remaining);
            let progressed = next > since.unwrap_or(0);
            for f in frames {
                let pass = g.read_filter.as_ref().is_none_or(|flt| flt.matches(&f));
                if pass {
                    out_bytes += f.len() as u64;
                    out.push(f);
                }
            }
            since = Some(next);
            if next >= next_total {
                up_to_date = true;
                break;
            }
            if !progressed || out_bytes >= max_bytes {
                break;
            }
        }
        FramesPage {
            frames: out,
            next_seq: since.unwrap_or(0),
            dropped_rx: g.store.dropped(),
            up_to_date,
            first_seq: g.store.first_seq(),
            last_seq: g.store.last_seq(),
        }
    }

    /// Read a bounded window with time filtering before byte limits and optional tail selection.
    pub fn read_window(
        &self,
        cursor: Option<u64>,
        min_time: Option<i64>,
        tail: bool,
        max_bytes: u64,
    ) -> FramesPage {
        self.read_window_filtered(cursor, min_time, tail, max_bytes, None)
    }

    /// Read through a client-specific filter without changing other clients' views.
    pub fn read_window_filtered(
        &self,
        cursor: Option<u64>,
        min_time: Option<i64>,
        tail: bool,
        max_bytes: u64,
        view_filter: Option<&CompiledFilter>,
    ) -> FramesPage {
        let g = self.core.shared.lock_inner();
        let matches = |f: &&Frame| {
            f.seq >= cursor.unwrap_or(0)
                && min_time.is_none_or(|t| f.t_us >= t)
                && view_filter.is_none_or(|filt| filt.matches(f))
                && g.read_filter
                    .as_ref()
                    .is_none_or(|filter| filter.matches(f))
        };
        let mut bytes = 0u64;
        let mut frames = Vec::new();
        let mut next_seq = g.store.next_seq();
        if tail {
            for frame in g.store.iter().rev().filter(matches) {
                if frames.len() >= 512
                    || (!frames.is_empty() && bytes + frame.wire_size_hint() as u64 > max_bytes)
                {
                    break;
                }
                bytes += frame.wire_size_hint() as u64;
                frames.push(frame.clone());
            }
            frames.reverse();
        } else {
            for frame in g.store.iter_since(cursor.unwrap_or(0)).filter(matches) {
                if frames.len() >= 512
                    || (!frames.is_empty() && bytes + frame.wire_size_hint() as u64 > max_bytes)
                {
                    next_seq = frame.seq;
                    break;
                }
                bytes += frame.wire_size_hint() as u64;
                frames.push(frame.clone());
            }
        }
        FramesPage {
            frames,
            next_seq,
            dropped_rx: g.store.dropped(),
            up_to_date: next_seq >= g.store.next_seq(),
            first_seq: g.store.first_seq(),
            last_seq: g.store.last_seq(),
        }
    }

    /// State revision used by change waiters.
    #[must_use]
    pub fn change_rev(&self) -> u64 {
        self.core.shared.lock_inner().rev
    }

    /// Wait up to `timeout` for a revision different from `seen_rev`; return whether it changed.
    pub fn wait_change(&self, seen_rev: u64, timeout: Duration) -> bool {
        let g = self.core.shared.lock_inner();
        let (g, _res) = self
            .core
            .shared
            .changed
            .wait_timeout_while(g, timeout, |i| i.rev == seen_rev)
            .expect("Session lock poisoned");
        g.rev != seen_rev
    }

    /// Clear retained frames and return their count; sequences keep increasing.
    #[must_use]
    pub fn clear_buffer(&self) -> u64 {
        let mut g = self.core.shared.lock_inner();
        let n = g.store.clear() as u64;
        g.rev += 1;
        drop(g);
        self.core.shared.changed.notify_all();
        n
    }

    /// Export the buffer, optionally applying the session view filter.
    pub fn export_log(
        &self,
        format: ExportFormat,
        path: &std::path::Path,
        apply_filter: bool,
    ) -> Result<ExportResult, FlattenError> {
        let g = self.core.shared.lock_inner();
        let mut frames = g.store.read_range(None, None, u64::MAX);
        if apply_filter && let Some(flt) = &g.read_filter {
            frames.retain(|f| flt.matches(f));
        }
        drop(g);
        export_frames(&frames, format, path)
    }

    /// Export one human-readable file from an immutable buffer snapshot.
    pub fn export_readable(&self, path: &std::path::Path) -> Result<ExportResult, FlattenError> {
        let frames: Vec<_> = self
            .core
            .shared
            .lock_inner()
            .store
            .iter()
            .cloned()
            .collect();
        export_frames(&frames, ExportFormat::Txt, path)
    }

    /// Export history through an independent view filter.
    pub fn export_filtered(
        &self,
        format: ExportFormat,
        path: &std::path::Path,
        filter: Option<&CompiledFilter>,
    ) -> Result<ExportResult, FlattenError> {
        let frames: Vec<_> = self
            .core
            .shared
            .lock_inner()
            .store
            .iter()
            .filter(|f| filter.is_none_or(|filter| filter.matches(f)))
            .cloned()
            .collect();
        export_frames(&frames, format, path)
    }

    /// Close and join the session workers, releasing the serial handle and decoder
    /// processes and finalizing capture. Retain configuration and terminal failure state.
    pub fn close(&self) {
        self.core.close_now();
    }

    /// Wait for Closed/Failed; on timeout return the current state.
    /// Failure can be observed while final data is draining; use `close` to wait
    /// for resource cleanup and complete capture finalization.
    #[must_use]
    pub fn wait_closed(&self, timeout: Duration) -> SessionState {
        let deadline = Instant::now() + timeout;
        loop {
            let state = self.state();
            if matches!(state, SessionState::Closed | SessionState::Failed { .. }) {
                return state;
            }
            if Instant::now() >= deadline {
                return state;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Queue a command and await its reply with shared timeout/closed-session handling.
    fn roundtrip<T>(
        &self,
        build: impl FnOnce(std::sync::mpsc::Sender<Result<T, FlattenError>>) -> Cmd,
        map: impl FnOnce(Result<T, FlattenError>) -> Result<T, FlattenError>,
    ) -> Result<T, FlattenError> {
        self.roundtrip_until(Instant::now() + REPLY_TIMEOUT, build, map)
    }

    fn roundtrip_until<T>(
        &self,
        deadline: Instant,
        build: impl FnOnce(std::sync::mpsc::Sender<Result<T, FlattenError>>) -> Cmd,
        map: impl FnOnce(Result<T, FlattenError>) -> Result<T, FlattenError>,
    ) -> Result<T, FlattenError> {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut cmd = Command::new(build(tx));
        cmd.deadline = deadline;
        let cancelled = cmd.cancelled.clone();
        {
            let sender = self.core.cmd_tx.lock().expect("Command lock poisoned");
            if self.core.shared.closed.load(Ordering::Relaxed) {
                return Err(FlattenError::SessionClosed(self.core.id.to_string()));
            }
            if Instant::now() >= deadline {
                return Err(FlattenError::Timeout(
                    "Serial command budget expired before enqueue".into(),
                ));
            }
            sender
                .as_ref()
                .ok_or_else(|| FlattenError::SessionClosed(self.core.id.to_string()))?
                .try_send(cmd)
                .map_err(|error| match error {
                    std::sync::mpsc::TrySendError::Full(_) => {
                        FlattenError::Timeout("Serial command queue is full".into())
                    }
                    std::sync::mpsc::TrySendError::Disconnected(_) => {
                        FlattenError::SessionClosed(self.core.id.to_string())
                    }
                })?;
        }
        match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(r) if Instant::now() < deadline => map(r),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected)
                if self.core.shared.closed.load(Ordering::Relaxed) =>
            {
                Err(FlattenError::SessionClosed(self.core.id.to_string()))
            }
            Ok(_) | Err(_) => {
                cancelled.store(true, Ordering::Relaxed);
                Err(FlattenError::Timeout(
                    crate::i18n::text("Timed out waiting for the serial command response").into(),
                ))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Reader worker
// ---------------------------------------------------------------------------

fn execute_trigger(program: String, args: Vec<String>) {
    if ACTIVE_TRIGGER_COMMANDS.fetch_add(1, Ordering::AcqRel) >= 4 {
        ACTIVE_TRIGGER_COMMANDS.fetch_sub(1, Ordering::AcqRel);
        tracing::warn!("Trigger skipped: four external actions are already running");
        return;
    }
    if std::thread::Builder::new()
        .name("flattencom-trigger".into())
        .spawn(move || {
            struct Release;
            impl Drop for Release {
                fn drop(&mut self) {
                    ACTIVE_TRIGGER_COMMANDS.fetch_sub(1, Ordering::AcqRel);
                }
            }
            let _release = Release;
            let mut command = std::process::Command::new(&program);
            command
                .args(args)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            let Ok(mut process) = command.spawn() else {
                tracing::warn!(%program, "Failed to start trigger program");
                return;
            };
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                match process.try_wait() {
                    Ok(Some(status)) => {
                        if !status.success() {
                            tracing::warn!(%program, %status, "Trigger action failed");
                        }
                        break;
                    }
                    Err(_) => {
                        let _ = process.kill();
                        let _ = process.wait();
                        break;
                    }
                    Ok(None) if Instant::now() >= deadline => {
                        let _ = process.kill();
                        let _ = process.wait();
                        break;
                    }
                    Ok(None) => std::thread::sleep(Duration::from_millis(10)),
                }
            }
        })
        .is_err()
    {
        ACTIVE_TRIGGER_COMMANDS.fetch_sub(1, Ordering::AcqRel);
    }
}

struct DisconnectedTransport;
impl SerialTransport for DisconnectedTransport {
    fn read(&mut self, _: &mut [u8]) -> Result<usize, FlattenError> {
        Err(FlattenError::io("port reconnecting"))
    }
    fn write(&mut self, _: &[u8]) -> Result<usize, FlattenError> {
        Err(FlattenError::io("port reconnecting; write was not sent"))
    }
    fn set_params(&mut self, _: &SerialConfig) -> Result<(), FlattenError> {
        Err(FlattenError::io("port reconnecting"))
    }
    fn set_signals(&mut self, _: Option<bool>, _: Option<bool>) -> Result<(), FlattenError> {
        Err(FlattenError::io("port reconnecting"))
    }
    fn read_signals(&mut self) -> Result<PinStates, FlattenError> {
        Err(FlattenError::io("port reconnecting"))
    }
    fn set_break(&mut self, _: Duration) -> Result<(), FlattenError> {
        Err(FlattenError::io("port reconnecting"))
    }
    fn flush_rx(&mut self) -> Result<(), FlattenError> {
        Err(FlattenError::io("port reconnecting"))
    }
    fn flush_tx(&mut self) -> Result<(), FlattenError> {
        Err(FlattenError::io("port reconnecting"))
    }
}

fn reader_loop(
    shared: Arc<Shared>,
    transport: Arc<Mutex<Box<dyn SerialTransport>>>,
    cmd_tx: SyncSender<Command>,
    factory: Arc<dyn crate::transport::TransportFactory>,
) {
    // Drain the driver independently from decoding. A bounded backlog converts
    // overload into an explicit failed session instead of unbounded allocation.
    let (received, incoming) = sync_channel::<(Frame, u64, u64)>(256);
    let processing = shared.clone();
    let processing_commands = cmd_tx.clone();
    let processor = Builder::new()
        .name("flattencom-rx-process".into())
        .spawn(move || {
            while let Ok((frame, generation, connection)) = incoming.recv() {
                process_received(
                    &processing,
                    &processing_commands,
                    frame,
                    generation,
                    connection,
                );
            }
        });
    let Ok(processor) = processor else {
        let mut g = shared.lock_inner();
        g.state = SessionState::Failed {
            error: "Cannot start receive processor".into(),
        };
        g.rev += 1;
        shared.closed.store(true, Ordering::Relaxed);
        drop(g);
        shared.changed.notify_all();
        return;
    };
    let mut attempt: u32 = 0;
    let mut buf = vec![0u8; 4096];
    'outer: loop {
        if shared.closed.load(Ordering::Relaxed) {
            break;
        }
        let (res, generation, connection) = {
            let mut t = transport.lock().expect("Serial port lock poisoned");
            if shared.closed.load(Ordering::Relaxed) {
                break;
            }
            let res = t.read(&mut buf);
            // Flush holds the same transport lock through generation advancement,
            // so a completed read can never be tagged with a post-flush generation.
            let g = shared.lock_inner();
            (res, g.rx_generation, g.connection_generation)
        };
        match res {
            Ok(0) => {
                // Idle reads already time out; add 1 ms to avoid spinning.
                std::thread::sleep(Duration::from_millis(1));
            }
            Ok(n) => {
                attempt = 0; // Successful reads reset the reconnect budget; merely reopening does not.
                let (t_us, mono_us) = shared.clocks_now();
                let data = buf[..n].to_vec();
                if received
                    .try_send((
                        Frame::new(0, Direction::Rx, data, t_us, mono_us),
                        generation,
                        connection,
                    ))
                    .is_err()
                {
                    let mut g = shared.lock_inner();
                    g.state = SessionState::Failed {
                        error: "Receive processing backlog exceeded 256 chunks; capture stopped"
                            .into(),
                    };
                    g.counters.errors.record("Receive processing overrun");
                    g.rev += 1;
                    drop(g);
                    *transport.lock().expect("Serial port lock poisoned") =
                        Box::new(DisconnectedTransport);
                    break;
                }
            }
            Err(e) => {
                // Release the failed OS handle before attempting an exclusive reopen.
                *transport.lock().expect("Serial port lock poisoned") =
                    Box::new(DisconnectedTransport);
                // Record errors and decide under the state lock; back off and reopen outside it.
                // attempt counts consecutive failure cycles and resets only after a successful read.
                // Reopening alone must not permit endless retries on a port that repeatedly fails reads.
                let (policy, cfg, give_up) = {
                    let mut g = shared.lock_inner();
                    g.counters.errors.record(&e.to_string());
                    let policy = g.config.auto_reconnect;
                    let cfg = g.config.clone();
                    attempt += 1;
                    let exceeded = match policy {
                        crate::config::ReconnectPolicy::Enabled { max_attempts, .. } => {
                            attempt > max_attempts
                        }
                        crate::config::ReconnectPolicy::Disabled => true,
                    };
                    if exceeded {
                        g.state = SessionState::Failed {
                            error: crate::tr!(
                                "{e} (reconnect limit reached after {attempt} attempts)",
                                attempt = attempt,
                                e = e
                            ),
                        };
                    } else {
                        g.state = SessionState::Reconnecting { attempt };
                    }
                    g.rev += 1;
                    (policy, cfg, exceeded)
                };
                shared.changed.notify_all();
                if give_up {
                    break 'outer;
                }
                // Reopen loop: back off, try opening, and stop after exhausting the attempt budget.
                // Never read the failed transport again while retrying its replacement.
                loop {
                    let deadline = Instant::now()
                        + Duration::from_millis(policy.backoff_ms(attempt.saturating_sub(1)));
                    while Instant::now() < deadline {
                        if shared.closed.load(Ordering::Relaxed) {
                            break 'outer;
                        }
                        std::thread::sleep(
                            Duration::from_millis(10)
                                .min(deadline.saturating_duration_since(Instant::now())),
                        );
                    }
                    if shared.closed.load(Ordering::Relaxed) {
                        break 'outer;
                    }
                    match factory
                        .reconnect_config(&cfg)
                        .and_then(|next| factory.open(&next).map(|transport| (next, transport)))
                    {
                        Ok((next, new_t)) => {
                            let mut current = transport.lock().expect("Serial port lock poisoned");
                            if shared.closed.load(Ordering::Relaxed) {
                                break 'outer;
                            }
                            *current = new_t;
                            {
                                let mut g = shared.lock_inner();
                                g.connection_generation += 1;
                                g.counters.reconnects += 1;
                                g.config.path = next.path;
                                g.state = SessionState::Connected;
                                g.rev += 1;
                            }
                            drop(current);
                            shared.changed.notify_all();
                            break; // Return to reads; retain the failure budget until a read succeeds.
                        }
                        Err(e2) => {
                            attempt += 1;
                            let exceeded = match policy {
                                crate::config::ReconnectPolicy::Enabled {
                                    max_attempts, ..
                                } => attempt > max_attempts,
                                crate::config::ReconnectPolicy::Disabled => true,
                            };
                            {
                                let mut g = shared.lock_inner();
                                if exceeded {
                                    g.state = SessionState::Failed {
                                        error: crate::tr!(
                                            "Read failed: {e}; reconnect failed after {attempt} attempts: {e2}",
                                            attempt = attempt,
                                            e = e,
                                            e2 = e2
                                        ),
                                    };
                                } else {
                                    // Publish the latest retry number so GUI/CLI can show progress.
                                    g.state = SessionState::Reconnecting { attempt };
                                }
                                g.rev += 1;
                                drop(g);
                                shared.changed.notify_all();
                            }
                            if exceeded {
                                break 'outer;
                            }
                        }
                    }
                }
            }
        }
    }
    // Terminal reader failures also stop the writer and reject new commands.
    shared.closed.store(true, Ordering::Relaxed);
    drop(received);
    let _ = processor.join();
    // WorkerCompletion publishes normal closure after both serial workers and
    // capture have finished; failures remain inspectable throughout cleanup.
}

fn process_received(
    shared: &Shared,
    cmd_tx: &SyncSender<Command>,
    frame: Frame,
    generation: u64,
    connection: u64,
) {
    let closing = shared.closed.load(Ordering::Relaxed);
    let mut frame = if closing {
        frame
    } else {
        shared.decode(frame, connection, generation)
    };
    let mut g = shared.lock_inner();
    frame.seq = g.store.alloc_seq();
    let fires = if shared.closed.load(Ordering::Relaxed) {
        Vec::new()
    } else {
        trigger::evaluate(&mut g.triggers, &frame)
    };
    for hit in fires {
        for (program, args) in hit.commands {
            execute_trigger(program, args);
        }
        let mut responded = true;
        for data in hit.responses {
            responded &= cmd_tx
                .try_send(Command::new(Cmd::Write {
                    data,
                    source: format!("trigger:{}", hit.trigger_id),
                    reply: None,
                }))
                .is_ok();
        }
        if !responded {
            g.counters
                .errors
                .record("Trigger response dropped: queue is full");
        }
        g.fires.push_back(TriggerFire {
            trigger_id: hit.trigger_id,
            seq: frame.seq,
            t_us: frame.t_us,
            matched: hit.matched,
            responded,
        });
        while g.fires.len() > 100 {
            g.fires.pop_front();
        }
    }
    g.counters.rx_bytes += frame.len() as u64;
    g.counters.rx_frames += 1;
    g.counters.rx_rate.push(Instant::now(), frame.len() as u64);
    let retain = generation == g.rx_generation;
    g.ingest_and_record(frame, retain, connection);
    g.rev += 1;
    drop(g);
    shared.changed.notify_all();
}

// ---------------------------------------------------------------------------
// Writer worker
// ---------------------------------------------------------------------------

fn writer_loop(
    shared: Arc<Shared>,
    transport: Arc<Mutex<Box<dyn SerialTransport>>>,
    cmd_rx: Receiver<Command>,
    cmd_tx: SyncSender<Command>,
) {
    while !shared.closed.load(Ordering::Relaxed) {
        let cmd = match cmd_rx.recv_timeout(Duration::from_millis(50)) {
            Ok(cmd) => cmd,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                continue;
            }
            Err(_) => break,
        };
        if cmd.cancelled.load(Ordering::Relaxed) || Instant::now() >= cmd.deadline {
            continue;
        }
        #[cfg(test)]
        if let Some(dispatched) = &cmd.dispatched {
            let _ = dispatched.send(());
        }
        match cmd.payload {
            Cmd::Write {
                data,
                source,
                reply,
            } => {
                let mut connection = None;
                let (sent, failure) = write_all(
                    &transport,
                    &data,
                    &shared,
                    &cmd.cancelled,
                    cmd.deadline,
                    &mut connection,
                );
                let mut data = data;
                data.truncate(sent);
                let mut seq = None;
                if sent > 0 {
                    let (t_us, mono_us) = shared.clocks_now();
                    let mut frame = Frame::new(0, Direction::Tx, data, t_us, mono_us);
                    frame.source = Some(source);
                    let mut frame =
                        shared.decode(frame, connection.expect("successful write generation"), 0);
                    let mut g = shared.lock_inner();
                    let frame_seq = g.store.alloc_seq();
                    frame.seq = frame_seq;
                    // Trigger-generated TX must not recursively trigger itself.
                    if !shared.closed.load(Ordering::Relaxed)
                        && !frame
                            .source
                            .as_deref()
                            .is_some_and(|s| s.starts_with("trigger:"))
                    {
                        let hits = trigger::evaluate(&mut g.triggers, &frame);
                        for hit in hits {
                            for (program, args) in hit.commands {
                                execute_trigger(program, args);
                            }
                            let mut responded = true;
                            for data in hit.responses {
                                responded &= cmd_tx
                                    .try_send(Command::new(Cmd::Write {
                                        data,
                                        source: format!("trigger:{}", hit.trigger_id),
                                        reply: None,
                                    }))
                                    .is_ok();
                            }
                            g.fires.push_back(TriggerFire {
                                trigger_id: hit.trigger_id,
                                seq: frame.seq,
                                t_us: frame.t_us,
                                matched: hit.matched,
                                responded,
                            });
                            while g.fires.len() > 100 {
                                g.fires.pop_front();
                            }
                        }
                    }
                    let len = frame.len() as u64;
                    g.counters.tx_bytes += len;
                    g.counters.tx_frames += 1;
                    g.counters.tx_rate.push(Instant::now(), len);
                    g.ingest_and_record(
                        frame,
                        true,
                        connection.expect("successful write generation"),
                    );
                    g.rev += 1;
                    drop(g);
                    shared.changed.notify_all();
                    seq = Some(frame_seq);
                }
                let result = if let Some(error) = failure {
                    shared
                        .lock_inner()
                        .counters
                        .errors
                        .record(&error.to_string());
                    Err(FlattenError::io(crate::tr!(
                        "Write failed after {sent} bytes: {error}",
                        error = error,
                        sent = sent
                    )))
                } else {
                    seq.ok_or_else(|| FlattenError::InvalidConfig {
                        field: "data".into(),
                        reason: "empty transmission".into(),
                    })
                };
                if let Some(r) = reply {
                    let _ = r.send(result);
                }
            }
            Cmd::SetSignals { dtr, rts, reply } => {
                let r =
                    with_live_transport(&shared, &transport, &cmd.cancelled, cmd.deadline, |t| {
                        t.set_signals(dtr, rts).and_then(|()| t.read_signals())
                    });
                if let Some(rx) = reply {
                    let _ = rx.send(r);
                }
            }
            Cmd::ReadSignals { reply } => {
                let r =
                    with_live_transport(&shared, &transport, &cmd.cancelled, cmd.deadline, |t| {
                        t.read_signals()
                    });
                if let Some(rx) = reply {
                    let _ = rx.send(r);
                }
            }
            Cmd::SetBreak { duration, reply } => {
                let r =
                    with_live_transport(&shared, &transport, &cmd.cancelled, cmd.deadline, |t| {
                        t.set_break(duration)
                    });
                if let Some(rx) = reply {
                    let _ = rx.send(r);
                }
            }
            Cmd::FlushRx { reply } => {
                let r =
                    with_live_transport(&shared, &transport, &cmd.cancelled, cmd.deadline, |t| {
                        t.flush_rx().map(|()| {
                            let mut g = shared.lock_inner();
                            g.rx_generation += 1;
                            let n = g.store.clear_rx() as u64;
                            g.rev += 1;
                            drop(g);
                            shared.changed.notify_all();
                            n
                        })
                    });
                if let Some(rx) = reply {
                    let _ = rx.send(r);
                }
            }
            Cmd::FlushTx { connection, reply } => {
                let r =
                    with_live_transport(&shared, &transport, &cmd.cancelled, cmd.deadline, |t| {
                        if shared.lock_inner().connection_generation != connection {
                            return Err(FlattenError::io(crate::i18n::text(
                                "Connection changed during TX flush",
                            )));
                        }
                        check_command_live(&shared, &cmd.cancelled, cmd.deadline)?;
                        t.flush_tx()
                    });
                if let Some(rx) = reply {
                    let _ = rx.send(r);
                }
            }
            Cmd::Configure { patch, reply } => {
                let r =
                    with_live_transport(&shared, &transport, &cmd.cancelled, cmd.deadline, |t| {
                        #[cfg(test)]
                        if let Some(configuring) = &cmd.configuring {
                            let _ = configuring.send(());
                        }
                        let previous = shared.lock_inner().config.clone();
                        let mut new_cfg = previous.clone();
                        patch.apply_to(&mut new_cfg);
                        new_cfg.validate()?;
                        check_command_live(&shared, &cmd.cancelled, cmd.deadline)?;
                        if let Err(e) = t.set_params(&new_cfg) {
                            // Roll back an operation already attempted even if its budget expired.
                            let _ = t.set_params(&previous);
                            return Err(e);
                        }
                        // Publish before another read can fail and snapshot reconnect settings.
                        let mut g = shared.lock_inner();
                        g.config = new_cfg.clone();
                        g.rev += 1;
                        drop(g);
                        shared.changed.notify_all();
                        Ok(new_cfg)
                    });
                if let Some(rx) = reply {
                    let _ = rx.send(r);
                }
            }
        }
    }
    // Explicit flush requests drain the driver. Shutdown must not call an
    // unbounded OS tcdrain when a disconnected peer has hardware CTS deasserted.
}

/// Validate after transport-lock contention, immediately before starting driver IO.
fn with_live_transport<T>(
    shared: &Shared,
    transport: &Mutex<Box<dyn SerialTransport>>,
    cancelled: &AtomicBool,
    deadline: Instant,
    operation: impl FnOnce(&mut dyn SerialTransport) -> Result<T, FlattenError>,
) -> Result<T, FlattenError> {
    let mut transport = transport.lock().expect("Serial port lock poisoned");
    check_command_live(shared, cancelled, deadline)?;
    operation(&mut **transport)
}

fn check_command_live(
    shared: &Shared,
    cancelled: &AtomicBool,
    deadline: Instant,
) -> Result<(), FlattenError> {
    if shared.closed.load(Ordering::Relaxed) {
        return Err(FlattenError::SessionClosed(
            "Serial session is closed".into(),
        ));
    }
    if cancelled.load(Ordering::Relaxed) || Instant::now() >= deadline {
        return Err(FlattenError::Timeout(
            "Serial command expired before driver operation".into(),
        ));
    }
    Ok(())
}

/// Write all bytes, advancing after partial writes; zero-byte writes are errors.
fn write_all(
    transport: &Arc<Mutex<Box<dyn SerialTransport>>>,
    data: &[u8],
    shared: &Shared,
    cancelled: &AtomicBool,
    command_deadline: Instant,
    connection: &mut Option<u64>,
) -> (usize, Option<FlattenError>) {
    let mut off = 0usize;
    let deadline = command_deadline.min(Instant::now() + Duration::from_secs(4));
    while off < data.len() {
        if Instant::now() >= deadline
            || shared.closed.load(Ordering::Relaxed)
            || cancelled.load(Ordering::Relaxed)
        {
            return (
                off,
                Some(FlattenError::Timeout(
                    crate::i18n::text("Serial write timed out after 4 seconds").into(),
                )),
            );
        }
        let mut t = transport.lock().expect("Serial port lock poisoned");
        if Instant::now() >= deadline
            || shared.closed.load(Ordering::Relaxed)
            || cancelled.load(Ordering::Relaxed)
        {
            return (
                off,
                Some(FlattenError::Timeout(
                    "Serial command expired before write".into(),
                )),
            );
        }
        let generation = shared.lock_inner().connection_generation;
        if let Err(error) = check_command_live(shared, cancelled, deadline) {
            return (off, Some(error));
        }
        if connection.is_some_and(|previous| previous != generation) {
            // Never send the suffix of one command to a different connection.
            // Its successful prefix is still decoded/recorded with the original stamp.
            return (
                off,
                Some(FlattenError::io("Connection changed during write")),
            );
        }
        let n = match t.write(&data[off..]) {
            Ok(n) => n,
            Err(e) => return (off, Some(e)),
        };
        if n == 0 {
            return (
                off,
                Some(FlattenError::io("Serial write returned zero bytes")),
            );
        }
        *connection = Some(generation);
        off += n;
    }
    (off, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A one-shot gate with a bounded wait, so a failed assertion cannot strand workers.
    struct TestGate {
        entered: std::sync::mpsc::Sender<()>,
        release: Receiver<()>,
    }

    impl TestGate {
        fn new() -> (Self, Receiver<()>, std::sync::mpsc::Sender<()>) {
            let (entered, receipt) = std::sync::mpsc::channel();
            let (release, wait) = std::sync::mpsc::channel();
            (
                Self {
                    entered,
                    release: wait,
                },
                receipt,
                release,
            )
        }

        fn wait(self) {
            self.entered.send(()).unwrap();
            self.release.recv_timeout(Duration::from_secs(10)).unwrap();
        }
    }

    struct ProbeFactory {
        mock: MockFactory,
        read_gate: Mutex<Option<TestGate>>,
        reads: std::sync::mpsc::Sender<()>,
        effects: Arc<Mutex<Vec<&'static str>>>,
        released: Arc<AtomicBool>,
    }

    impl ProbeFactory {
        fn new(gate: Option<TestGate>) -> (Arc<Self>, Receiver<()>) {
            let (reads, receipt) = std::sync::mpsc::channel();
            (
                Arc::new(Self {
                    mock: MockFactory::script(vec![]),
                    read_gate: Mutex::new(gate),
                    reads,
                    effects: Arc::default(),
                    released: Arc::default(),
                }),
                receipt,
            )
        }
    }

    impl TransportFactory for ProbeFactory {
        fn open(&self, cfg: &SerialConfig) -> Result<Box<dyn SerialTransport>, FlattenError> {
            Ok(Box::new(ProbeTransport {
                inner: self.mock.open(cfg)?,
                read_gate: self.read_gate.lock().unwrap().take(),
                reads: self.reads.clone(),
                effects: self.effects.clone(),
                released: self.released.clone(),
            }))
        }

        fn kind(&self) -> &'static str {
            "probe"
        }
    }

    struct ProbeTransport {
        inner: Box<dyn SerialTransport>,
        read_gate: Option<TestGate>,
        reads: std::sync::mpsc::Sender<()>,
        effects: Arc<Mutex<Vec<&'static str>>>,
        released: Arc<AtomicBool>,
    }

    impl Drop for ProbeTransport {
        fn drop(&mut self) {
            self.released.store(true, Ordering::SeqCst);
        }
    }

    impl SerialTransport for ProbeTransport {
        fn read(&mut self, buf: &mut [u8]) -> Result<usize, FlattenError> {
            if let Some(gate) = self.read_gate.take() {
                gate.wait();
            }
            let result = self.inner.read(buf);
            if matches!(result, Ok(n) if n > 0) {
                let _ = self.reads.send(());
            }
            result
        }
        fn write(&mut self, data: &[u8]) -> Result<usize, FlattenError> {
            self.effects.lock().unwrap().push("write");
            self.inner.write(data)
        }
        fn set_params(&mut self, cfg: &SerialConfig) -> Result<(), FlattenError> {
            self.effects.lock().unwrap().push("configure");
            self.inner.set_params(cfg)
        }
        fn set_signals(
            &mut self,
            dtr: Option<bool>,
            rts: Option<bool>,
        ) -> Result<(), FlattenError> {
            self.effects.lock().unwrap().push("set_signals");
            self.inner.set_signals(dtr, rts)
        }
        fn read_signals(&mut self) -> Result<PinStates, FlattenError> {
            self.effects.lock().unwrap().push("read_signals");
            self.inner.read_signals()
        }
        fn set_break(&mut self, duration: Duration) -> Result<(), FlattenError> {
            self.effects.lock().unwrap().push("break");
            self.inner.set_break(duration)
        }
        fn flush_rx(&mut self) -> Result<(), FlattenError> {
            self.effects.lock().unwrap().push("flush_rx");
            self.inner.flush_rx()
        }
        fn flush_tx(&mut self) -> Result<(), FlattenError> {
            self.effects.lock().unwrap().push("flush_tx");
            self.inner.flush_tx()
        }
    }

    struct GatedDecoder(Option<TestGate>);
    impl Decoder for GatedDecoder {
        fn id(&self) -> &'static str {
            "gated"
        }
        fn feed(&mut self, _: &ChunkCtx<'_>) -> Option<crate::frame::DecodedInfo> {
            if let Some(gate) = self.0.take() {
                gate.wait();
            }
            None
        }
    }

    struct GatedForwardDecoder {
        gate: Option<TestGate>,
        decoder: Box<dyn Decoder>,
    }

    #[test]
    fn partial_write_never_crosses_a_connection_generation() {
        // Advance the epoch inside a deterministic transport probe after accepting
        // one byte. The next transport acquisition must reject the command suffix.
        struct EpochTransport {
            shared: Arc<Shared>,
            writes: Arc<Mutex<Vec<Vec<u8>>>>,
        }
        impl SerialTransport for EpochTransport {
            fn read(&mut self, _: &mut [u8]) -> Result<usize, FlattenError> {
                Ok(0)
            }
            fn write(&mut self, data: &[u8]) -> Result<usize, FlattenError> {
                self.writes.lock().unwrap().push(data[..1].to_vec());
                self.shared.lock_inner().connection_generation += 1;
                Ok(1)
            }
            fn set_params(&mut self, _: &SerialConfig) -> Result<(), FlattenError> {
                Ok(())
            }
            fn set_signals(
                &mut self,
                _: Option<bool>,
                _: Option<bool>,
            ) -> Result<(), FlattenError> {
                Ok(())
            }
            fn read_signals(&mut self) -> Result<PinStates, FlattenError> {
                Ok(PinStates::default())
            }
            fn set_break(&mut self, _: Duration) -> Result<(), FlattenError> {
                Ok(())
            }
            fn flush_rx(&mut self) -> Result<(), FlattenError> {
                Ok(())
            }
            fn flush_tx(&mut self) -> Result<(), FlattenError> {
                Ok(())
            }
        }
        let factory = Arc::new(MockFactory::script(vec![]));
        let session = open_mock(&factory);
        session.close();
        let shared = session.core.shared.clone();
        shared.closed.store(false, Ordering::Relaxed);
        let writes = Arc::new(Mutex::new(Vec::new()));
        let transport: Arc<Mutex<Box<dyn SerialTransport>>> =
            Arc::new(Mutex::new(Box::new(EpochTransport {
                shared: shared.clone(),
                writes: writes.clone(),
            })));
        let mut connection = None;
        let (sent, error) = write_all(
            &transport,
            b"AB",
            &shared,
            &AtomicBool::new(false),
            Instant::now() + REPLY_TIMEOUT,
            &mut connection,
        );
        shared.closed.store(true, Ordering::Relaxed);
        assert_eq!(sent, 1);
        assert_eq!(connection, Some(0));
        assert!(error.unwrap().to_string().contains("Connection changed"));
        assert_eq!(*writes.lock().unwrap(), vec![b"A".to_vec()]);
    }
    impl Decoder for GatedForwardDecoder {
        fn id(&self) -> &'static str {
            "gated-forward"
        }
        fn feed(&mut self, ctx: &ChunkCtx<'_>) -> Option<crate::frame::DecodedInfo> {
            if let Some(gate) = self.gate.take() {
                gate.wait();
            }
            self.decoder.feed(ctx)
        }
    }

    #[test]
    fn reconnect_resets_decoders_in_rx_queue_and_tx_completion_order() {
        let directory = tempfile::tempdir().unwrap();
        let raw = directory.path().join("reconnect.bin");
        let (factory, reads) = ProbeFactory::new(None);
        let session = SessionHandle::open(
            SessionId::new(),
            SerialConfig {
                record_rx_to: Some(raw.clone()),
                auto_reconnect: ReconnectPolicy::Enabled {
                    max_attempts: 2,
                    initial_ms: 1,
                    max_ms: 1,
                },
                ..SerialConfig::new("probe")
            },
            factory.clone(),
        )
        .unwrap();
        let spec = DecoderSpec {
            name: "json_lines".into(),
            options: serde_json::json!({}),
        };
        session.set_decoder(Some(spec)).unwrap();
        session.send(b"{\"old_tx\":\"".to_vec()).unwrap();
        let (gate, entered, release) = TestGate::new();
        {
            let mut decoders = session.core.shared.decoders.lock().unwrap();
            let decoder = decoders.rx.take().unwrap();
            decoders.rx = Some(Box::new(GatedForwardDecoder {
                gate: Some(gate),
                decoder,
            }));
        }
        factory
            .mock
            .extend_script(vec![ReadStep::Data(b"{\"old_rx\":true}\n".to_vec())]);
        entered.recv_timeout(Duration::from_secs(2)).unwrap();
        reads.recv_timeout(Duration::from_secs(2)).unwrap();
        let sending = session.clone();
        let sender = std::thread::spawn(move || sending.send(b"complete\"}\n".to_vec()));
        // The bytes are on the old transport, but TX decode is waiting on RX.
        assert!(wait_until(2000, || factory
            .mock
            .writes
            .lock()
            .unwrap()
            .len()
            == 2));
        factory.mock.extend_script(vec![
            ReadStep::Data(b"{\"stale_rx\":\"".to_vec()),
            ReadStep::Fail(FlattenError::io("reconnect boundary")),
        ]);
        reads.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(wait_until(2000, || session.stats().reconnects == 1));
        factory.mock.extend_script(vec![ReadStep::Data(
            b"new\"}\n{\"new_rx\":true}\n".to_vec(),
        )]);
        reads.recv_timeout(Duration::from_secs(2)).unwrap();
        release.send(()).unwrap();
        let seq = sender.join().unwrap().unwrap();
        let tx = session.read_sent(Some(seq), 4096).frames.remove(0);
        assert!(
            tx.decoded
                .unwrap()
                .fields
                .iter()
                .any(|f| f.name == "old_tx" && f.value == "complete")
        );
        assert!(wait_until(2000, || session.stats().rx_frames == 3));
        let rx: Vec<_> = session
            .read_frames(None, 16384)
            .frames
            .into_iter()
            .filter(|f| f.dir == Direction::Rx)
            .collect();
        assert_eq!(rx.len(), 3, "reconnect must preserve old history");
        assert!(
            rx[0]
                .decoded
                .as_ref()
                .unwrap()
                .fields
                .iter()
                .any(|f| f.name == "old_rx")
        );
        let last = rx[2].decoded.as_ref().unwrap();
        assert!(!last.fields.iter().any(|f| f.name == "stale_rx"));
        assert!(last.fields.iter().any(|f| f.name == "new_rx"));
        // Another boundary leaves an actual TX fragment to discard.
        session.send(b"{\"stale_tx\":\"".to_vec()).unwrap();
        factory
            .mock
            .extend_script(vec![ReadStep::Fail(FlattenError::io("second boundary"))]);
        assert!(wait_until(2000, || session.stats().reconnects == 2));
        let seq = session
            .send(b"new\"}\n{\"new_tx\":true}\n".to_vec())
            .unwrap();
        let tx = session.read_sent(Some(seq), 4096).frames.remove(0);
        let info = tx.decoded.unwrap();
        assert!(!info.fields.iter().any(|f| f.name == "stale_tx"));
        assert!(info.fields.iter().any(|f| f.name == "new_tx"));
        session.close();
        assert_eq!(
            std::fs::read(raw).unwrap(),
            b"{\"old_rx\":true}\n{\"stale_rx\":\"new\"}\n{\"new_rx\":true}\n"
        );
    }

    #[test]
    fn terminal_failure_finalizes_capture_after_in_flight_tx_commit() {
        let directory = tempfile::tempdir().unwrap();
        let readable = directory.path().join("failure.log");
        let raw = directory.path().join("failure.bin");
        let (factory, reads) = ProbeFactory::new(None);
        let session = SessionHandle::open(
            SessionId::new(),
            SerialConfig {
                record_to: Some(readable.clone()),
                record_rx_to: Some(raw.clone()),
                auto_reconnect: ReconnectPolicy::Disabled,
                ..SerialConfig::new("probe")
            },
            factory.clone(),
        )
        .unwrap();
        factory
            .mock
            .extend_script(vec![ReadStep::Data(b"tail\xe4\xb8".to_vec())]);
        reads.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(wait_until(2000, || session.stats().rx_frames == 1));
        let (gate, entered, release) = TestGate::new();
        session.core.shared.decoders.lock().unwrap().tx = Some(Box::new(GatedDecoder(Some(gate))));
        let sending = session.clone();
        let sender = std::thread::spawn(move || sending.send(b"TX-TAIL".to_vec()));
        entered.recv_timeout(Duration::from_secs(2)).unwrap();
        factory
            .mock
            .extend_script(vec![ReadStep::Fail(FlattenError::io("terminal failure"))]);
        assert!(wait_until(2000, || matches!(
            session.state(),
            SessionState::Failed { .. }
        )));
        assert!(
            session.core.shared.lock_inner().capture.is_some(),
            "writer still owns an uncommitted successful tail"
        );
        release.send(()).unwrap();
        sender.join().unwrap().unwrap();
        // No close call: failure must finalize while this inspection handle lives.
        assert!(wait_until(2000, || session
            .core
            .threads
            .lock()
            .unwrap()
            .iter()
            .all(std::thread::JoinHandle::is_finished)));
        assert!(session.core.shared.lock_inner().capture.is_none());
        let text = std::fs::read_to_string(readable).unwrap();
        assert!(text.contains("TX-TAIL"), "{text}");
        assert!(text.contains("\\xE4\\xB8"), "{text}");
        assert_eq!(std::fs::read(raw).unwrap(), b"tail\xe4\xb8");
        assert_eq!(session.stats().tx_bytes, 7);
        assert!(!session.stats().recording.active);
        assert!(
            session.stats().recording.errors.is_empty(),
            "{:?}",
            session.stats().recording.errors.is_empty()
        );
        session.close();
        assert!(matches!(session.state(), SessionState::Failed { .. }));
    }

    #[test]
    fn decoder_built_before_close_cannot_be_installed_after_shutdown() {
        let factory = Arc::new(MockFactory::script(vec![]));
        let session = open_mock(&factory);
        let (built, ready) = sync_channel(0);
        *session.core.shared.decoder_built.lock().unwrap() = Some(built);
        let guard = session.core.shared.decoders.lock().unwrap();
        let installing = session.clone();
        let installer = std::thread::spawn(move || {
            installing.set_decoder(Some(DecoderSpec {
                name: "json_lines".into(),
                options: serde_json::json!({}),
            }))
        });
        ready.recv_timeout(Duration::from_secs(2)).unwrap();
        let closing = session.clone();
        let closer = std::thread::spawn(move || closing.close());
        assert!(wait_until(2000, || session
            .core
            .shared
            .closed
            .load(Ordering::Relaxed)));
        drop(guard);
        assert!(matches!(
            installer.join().unwrap(),
            Err(FlattenError::SessionClosed(_))
        ));
        closer.join().unwrap();
        assert!(session.decoder_config().is_none());
        let decoders = session.core.shared.decoders.lock().unwrap();
        assert!(decoders.rx.is_none() && decoders.tx.is_none());
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn close_reaps_plugins_preserves_spec_and_rejects_late_decoder_installation() {
        let directory = tempfile::tempdir().unwrap();
        let pids = directory.path().join("pids");
        let factory = Arc::new(MockFactory::script(vec![]));
        let session = open_mock(&factory);
        let spec = DecoderSpec {
            name: "cmd".into(),
            options: serde_json::json!({
                "program":"python3", "args":["-c", "import os,sys\nwith open(sys.argv[1], 'a') as f: f.write(str(os.getpid())+'\\n')\nfor line in sys.stdin: print('null',flush=True)", pids]
            }),
        };
        session.set_decoder(Some(spec.clone())).unwrap();
        assert!(wait_until(2000, || std::fs::read_to_string(&pids)
            .unwrap_or_default()
            .lines()
            .count()
            == 2));
        let children: Vec<u32> = std::fs::read_to_string(&pids)
            .unwrap()
            .lines()
            .map(|s| s.parse().unwrap())
            .collect();
        session.close();
        assert_eq!(session.state(), SessionState::Closed);
        assert_eq!(session.decoder_config(), Some(spec.clone()));
        assert!(
            children
                .iter()
                .all(|pid| !std::path::Path::new(&format!("/proc/{pid}")).exists())
        );
        assert!(matches!(
            session.set_decoder(Some(spec)),
            Err(FlattenError::SessionClosed(_))
        ));
        assert!(matches!(
            session.set_decoder(None),
            Err(FlattenError::SessionClosed(_))
        ));
        assert_eq!(std::fs::read_to_string(pids).unwrap().lines().count(), 2);
    }

    #[test]
    fn flush_retries_reject_replacement_transport_before_querying_it() {
        let factory = Arc::new(MockFactory::script(vec![]));
        let session = open_mock(&factory);
        let shared = session.core.shared.clone();
        let (commands, incoming) = sync_channel::<Command>(2);
        *session.core.cmd_tx.lock().unwrap() = Some(commands.clone());
        let caller = session.clone();
        let client = std::thread::spawn(move || caller.flush_tx());
        let first = incoming.recv_timeout(Duration::from_secs(2)).unwrap();
        let Cmd::FlushTx {
            connection,
            reply: Some(reply),
        } = first.payload
        else {
            panic!("flush expected")
        };
        assert_eq!(connection, 0);
        // Replace the connection after its nonempty buffer was queried, then
        // dispatch the caller's retry through the real writer.
        shared.lock_inner().connection_generation += 1;
        reply
            .send(Err(FlattenError::Timeout("still draining".into())))
            .unwrap();
        let (probe, _) = ProbeFactory::new(None);
        let transport = Arc::new(Mutex::new(probe.open(&session.config()).unwrap()));
        let worker_shared = shared.clone();
        let writer =
            std::thread::spawn(move || writer_loop(worker_shared, transport, incoming, commands));
        let error = client.join().unwrap().unwrap_err();
        assert!(
            error
                .to_string()
                .contains("Connection changed during TX flush")
        );
        assert!(
            probe.effects.lock().unwrap().is_empty(),
            "replacement must not be queried"
        );
        session.close();
        writer.join().unwrap();
    }

    #[test]
    fn capture_boundaries_follow_queued_rx_and_ignore_late_old_tx() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("boundaries.log");
        let raw = directory.path().join("boundaries.bin");
        let factory = Arc::new(MockFactory::script(vec![]));
        let session = open_mock(&factory);
        session.close();
        let shared = &session.core.shared;
        let mut g = shared.lock_inner();
        g.capture = Some(
            crate::capture_worker::CaptureWorker::start(vec![
                Recorder::open_readable(&path).unwrap(),
                Recorder::open_rx(&raw).unwrap(),
            ])
            .unwrap(),
        );
        let mut seq = 0;
        let mut ingest = |g: &mut Inner, dir, bytes: &[u8], connection| {
            g.ingest_and_record(
                Frame::new(
                    seq,
                    dir,
                    bytes.to_vec(),
                    i64::try_from(seq).unwrap(),
                    seq * 1000,
                ),
                true,
                connection,
            );
            seq += 1;
        };
        // Old queued frames are still processed after the reader has reconnected.
        g.connection_generation = 1;
        ingest(&mut g, Direction::Rx, b"U-Boot SPL old\nold-tail\xe4", 0);
        ingest(&mut g, Direction::Rx, b"\xb8", 0);
        // Incomplete UTF-8 is finalized; new forward-stage boot is a separate boot.
        ingest(&mut g, Direction::Rx, b"Linux version new\n\xe4", 1);
        ingest(&mut g, Direction::Tx, b"OLD-TX", 0);
        ingest(&mut g, Direction::Rx, b"\xb8\xad\r", 1);
        // Old CR must not swallow new connection's first LF.
        ingest(&mut g, Direction::Rx, b"\nnew-line\n\x1b]0;unfinished", 2);
        ingest(
            &mut g,
            Direction::Rx,
            b"Linux version recovered\nvisible\n",
            3,
        );
        let events = g.boots.events();
        assert_eq!(events.len(), 3);
        assert_eq!(events[2].number, 3);
        assert!(events[1].interval_ms.is_some());
        let capture = g.capture.take();
        drop(g);
        drop(capture);
        let text = std::fs::read_to_string(path).unwrap();
        assert!(text.contains("old-tail\n"), "{text}");
        assert!(text.contains("\\xE4\\xB8\n"), "{text}");
        assert!(
            text.contains("中\n"),
            "late old TX must not reset new RX UTF-8: {text}"
        );
        assert!(
            text.contains("RX #5] \n"),
            "new LF must not merge with old CR: {text}"
        );
        assert!(text.contains("Linux version recovered\n"), "{text}");
        assert!(text.contains("visible\n"), "{text}");
        assert!(text.find("OLD-TX").unwrap() < text.find('中').unwrap());
        assert_eq!(std::fs::read(raw).unwrap(), b"U-Boot SPL old\nold-tail\xe4\xb8Linux version new\n\xe4\xb8\xad\r\nnew-line\n\x1b]0;unfinishedLinux version recovered\nvisible\n");
    }

    #[test]
    fn reconnect_discards_old_ansi_state_after_draining_queued_rx() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("reconnect.log");
        let (factory, reads) = ProbeFactory::new(None);
        let session = SessionHandle::open(
            SessionId::new(),
            SerialConfig {
                record_to: Some(path.clone()),
                auto_reconnect: ReconnectPolicy::Enabled {
                    max_attempts: 2,
                    initial_ms: 1,
                    max_ms: 1,
                },
                ..SerialConfig::new("probe")
            },
            factory.clone(),
        )
        .unwrap();
        let (gate, entered, release) = TestGate::new();
        session.core.shared.decoders.lock().unwrap().rx = Some(Box::new(GatedDecoder(Some(gate))));
        factory
            .mock
            .extend_script(vec![ReadStep::Data(b"old queued\n".to_vec())]);
        entered.recv_timeout(Duration::from_secs(2)).unwrap();
        reads.recv_timeout(Duration::from_secs(2)).unwrap();
        factory.mock.extend_script(vec![
            ReadStep::Data(b"\x1b]0;old title".to_vec()),
            ReadStep::Fail(FlattenError::io("disconnect")),
        ]);
        reads.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(wait_until(2000, || session.stats().reconnects == 1));
        factory.mock.extend_script(vec![ReadStep::Data(
            b"Linux version NEW-BOOT\nvisible\n".to_vec(),
        )]);
        reads.recv_timeout(Duration::from_secs(2)).unwrap();
        release.send(()).unwrap();
        assert!(wait_until(2000, || session.stats().rx_frames == 3));
        session.close();
        let text = std::fs::read_to_string(path).unwrap();
        assert!(text.contains("old queued\n"), "{text}");
        assert!(text.contains("Linux version NEW-BOOT\n"), "{text}");
        assert!(text.contains("visible\n"), "{text}");
        assert_eq!(session.boot_events().len(), 1);
    }

    #[test]
    fn flush_tx_retries_share_one_deadline_and_expired_calls_are_not_queued() {
        let factory = Arc::new(MockFactory::script(vec![]));
        let session = open_mock(&factory);
        let (commands, incoming) = sync_channel::<Command>(2);
        *session.core.cmd_tx.lock().unwrap() = Some(commands);
        let (entered, retry) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel();
        let deadline = Instant::now() + Duration::from_millis(200);
        let server = std::thread::spawn(move || {
            let first = incoming.recv_timeout(Duration::from_secs(2)).unwrap();
            assert_eq!(first.deadline, deadline);
            let Cmd::FlushTx {
                reply: Some(reply), ..
            } = first.payload
            else {
                panic!("flush expected")
            };
            reply
                .send(Err(FlattenError::Timeout("still draining".into())))
                .unwrap();
            let second = incoming.recv_timeout(Duration::from_secs(2)).unwrap();
            assert_eq!(second.deadline, deadline);
            entered.send(()).unwrap();
            released.recv_timeout(Duration::from_secs(2)).unwrap();
            assert!(second.cancelled.load(Ordering::Relaxed));
            let Cmd::FlushTx {
                reply: Some(reply), ..
            } = second.payload
            else {
                panic!("flush expected")
            };
            assert!(
                reply.send(Ok(())).is_err(),
                "late success must not be accepted"
            );
            assert!(incoming.try_recv().is_err());
        });
        let caller = session.clone();
        let (done, result) = std::sync::mpsc::channel();
        let client =
            std::thread::spawn(move || done.send(caller.flush_tx_until(deadline)).unwrap());
        retry.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(matches!(
            result.recv_timeout(Duration::from_secs(1)).unwrap(),
            Err(FlattenError::Timeout(_))
        ));
        assert!(matches!(
            session.flush_tx_until(deadline),
            Err(FlattenError::Timeout(_))
        ));
        release.send(()).unwrap();
        client.join().unwrap();
        server.join().unwrap();
        session.close();
    }

    #[test]
    fn configure_rechecks_liveness_after_state_lock_contention() {
        for cancel in [false, true] {
            let factory = Arc::new(MockFactory::script(vec![]));
            let session = open_mock(&factory);
            session.close();
            let shared = session.core.shared.clone();
            shared.closed.store(false, Ordering::Relaxed);
            let (probe, _) = ProbeFactory::new(None);
            let transport = Arc::new(Mutex::new(probe.open(&session.config()).unwrap()));
            let (commands, incoming) = sync_channel(1);
            let (reply, result) = std::sync::mpsc::channel();
            let (configuring, entered) = sync_channel(0);
            let mut command = Command::new(Cmd::Configure {
                patch: ConfigPatch {
                    baud: Some(9600),
                    ..Default::default()
                },
                reply: Some(reply),
            });
            command.configuring = Some(configuring);
            command.deadline = Instant::now() + Duration::from_millis(100);
            let deadline = command.deadline;
            let cancelled = command.cancelled.clone();
            let guard = shared.lock_inner();
            commands.send(command).unwrap();
            let worker_shared = shared.clone();
            let worker_commands = commands.clone();
            let writer = std::thread::spawn(move || {
                writer_loop(worker_shared, transport, incoming, worker_commands);
            });
            entered.recv_timeout(Duration::from_secs(2)).unwrap();
            if cancel {
                cancelled.store(true, Ordering::Relaxed);
            } else {
                std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
            }
            drop(guard);
            assert!(matches!(
                result.recv_timeout(Duration::from_secs(2)).unwrap(),
                Err(FlattenError::Timeout(_))
            ));
            assert!(probe.effects.lock().unwrap().is_empty());
            assert_eq!(session.config().baud, SerialConfig::new("mock").baud);
            shared.closed.store(true, Ordering::Relaxed);
            writer.join().unwrap();
        }
    }

    #[test]
    fn flush_rx_resets_rx_reassembly_preserving_tx_spec_and_capture() {
        let directory = tempfile::tempdir().unwrap();
        let raw = directory.path().join("decoder.bin");
        let factory = Arc::new(MockFactory::script(vec![]));
        let session = SessionHandle::open(
            SessionId::new(),
            SerialConfig {
                record_rx_to: Some(raw.clone()),
                ..SerialConfig::new("mock")
            },
            as_dyn(&factory),
        )
        .unwrap();
        let spec = DecoderSpec {
            name: "json_lines".into(),
            options: serde_json::json!({}),
        };
        session.set_decoder(Some(spec.clone())).unwrap();
        session.send(b"{\"tx\":\"KEPT\"".to_vec()).unwrap();
        factory.extend_script(vec![ReadStep::Data(b"{\"before_flush\":\"OLD\"".to_vec())]);
        assert!(wait_until(2000, || session.stats().rx_frames == 1));
        assert_eq!(session.flush_rx().unwrap(), 1);
        factory.extend_script(vec![ReadStep::Data(
            b"}\n{\"after_flush\":\"NEW\"}\n".to_vec(),
        )]);
        assert!(wait_until(2000, || session.stats().rx_frames == 2));
        let page = session.read_frames(None, 4096);
        let rx = page.frames.iter().find(|f| f.dir == Direction::Rx).unwrap();
        let decoded = rx.decoded.as_ref().unwrap();
        assert!(!decoded.text.contains("OLD"));
        assert!(!decoded.fields.iter().any(|f| f.name == "before_flush"));
        assert!(
            decoded
                .fields
                .iter()
                .any(|f| f.name == "after_flush" && f.value == "NEW")
        );
        let seq = session.send(b"}\n".to_vec()).unwrap();
        let tx = session.read_sent(Some(seq), 4096).frames.remove(0);
        assert!(
            tx.decoded
                .as_ref()
                .unwrap()
                .fields
                .iter()
                .any(|f| f.name == "tx" && f.value == "KEPT")
        );
        assert_eq!(session.decoder_config(), Some(spec));
        session.close();
        assert_eq!(
            std::fs::read(raw).unwrap(),
            b"{\"before_flush\":\"OLD\"}\n{\"after_flush\":\"NEW\"}\n"
        );
    }

    #[test]
    fn concurrent_close_waits_for_transport_and_recorded_rx_tail() {
        let directory = tempfile::tempdir().unwrap();
        let jsonl = directory.path().join("tail.jsonl");
        let raw = directory.path().join("tail.bin");
        let (gate, entered, release) = TestGate::new();
        let (factory, _) = ProbeFactory::new(Some(gate));
        factory
            .mock
            .extend_script(vec![ReadStep::Data(b"TAIL\n".to_vec())]);
        let session = SessionHandle::open(
            SessionId::new(),
            SerialConfig {
                record_to: Some(jsonl.clone()),
                record_rx_to: Some(raw.clone()),
                ..SerialConfig::new("probe")
            },
            factory.clone(),
        )
        .unwrap();
        entered.recv_timeout(Duration::from_secs(2)).unwrap();
        let (done, completed) = std::sync::mpsc::channel();
        let first = session.clone();
        let first_done = done.clone();
        let first = std::thread::spawn(move || {
            first.close();
            first_done.send(()).unwrap();
        });
        assert!(wait_until(2000, || session
            .core
            .shared
            .closed
            .load(Ordering::Relaxed)));
        let second = session.clone();
        let (started, starting) = std::sync::mpsc::channel();
        let second = std::thread::spawn(move || {
            started.send(()).unwrap();
            second.close();
            done.send(()).unwrap();
        });
        starting.recv_timeout(Duration::from_secs(2)).unwrap();
        let premature = completed.recv_timeout(Duration::from_millis(100));
        let still_owned = !factory.released.load(Ordering::SeqCst);
        release.send(()).unwrap();
        first.join().unwrap();
        second.join().unwrap();
        assert!(
            premature.is_err(),
            "all close callers must wait for shutdown completion"
        );
        assert!(still_owned);
        assert!(factory.released.load(Ordering::SeqCst));
        let view = session.read_frames(None, 4096).frames;
        let (recorded, skipped) = crate::record::read_log(&jsonl).unwrap();
        assert_eq!(skipped, 0);
        assert_eq!(recorded, view);
        assert_eq!(recorded.len(), 1);
        assert_eq!(&**recorded[0].data, b"TAIL\n");
        assert_eq!(std::fs::read(raw).unwrap(), b"TAIL\n");
        assert_eq!(session.stats().rx_bytes, 5);
        assert!(!session.stats().recording.active);
        assert!(
            session.stats().recording.errors.is_empty(),
            "{:?}",
            session.stats().recording.errors.is_empty()
        );
    }

    #[test]
    fn controls_recheck_expiry_cancellation_and_close_after_transport_contention() {
        for reason in ["deadline", "cancelled", "closed"] {
            for operation in [
                "signals",
                "read_signals",
                "break",
                "flush_rx",
                "flush_tx",
                "configure",
            ] {
                let (gate, entered, release) = TestGate::new();
                let (factory, _) = ProbeFactory::new(Some(gate));
                let session = SessionHandle::open(
                    SessionId::new(),
                    SerialConfig::new("probe"),
                    factory.clone(),
                )
                .unwrap();
                entered.recv_timeout(Duration::from_secs(2)).unwrap();
                // Normalize reply types by observing command completion through the sender's drop.
                let (unit, unit_rx) = std::sync::mpsc::channel();
                let (pins, pins_rx) = std::sync::mpsc::channel();
                let (count, count_rx) = std::sync::mpsc::channel();
                let (config, config_rx) = std::sync::mpsc::channel();
                let payload = match operation {
                    "signals" => Cmd::SetSignals {
                        dtr: Some(true),
                        rts: Some(true),
                        reply: Some(pins),
                    },
                    "read_signals" => Cmd::ReadSignals { reply: Some(pins) },
                    "break" => Cmd::SetBreak {
                        duration: Duration::from_millis(1),
                        reply: Some(unit),
                    },
                    "flush_rx" => Cmd::FlushRx { reply: Some(count) },
                    "flush_tx" => Cmd::FlushTx {
                        connection: 0,
                        reply: Some(unit),
                    },
                    "configure" => Cmd::Configure {
                        patch: ConfigPatch {
                            baud: Some(9600),
                            ..Default::default()
                        },
                        reply: Some(config),
                    },
                    _ => unreachable!(),
                };
                let (dispatched, dispatch) = sync_channel(0);
                let mut command = Command::new(payload);
                command.dispatched = Some(dispatched);
                if reason == "deadline" {
                    command.deadline = Instant::now() + Duration::from_millis(100);
                }
                let deadline = command.deadline;
                let cancelled = command.cancelled.clone();
                session
                    .core
                    .cmd_tx
                    .lock()
                    .unwrap()
                    .as_ref()
                    .unwrap()
                    .try_send(command)
                    .unwrap();
                dispatch.recv_timeout(Duration::from_secs(2)).unwrap();
                match reason {
                    "deadline" => {
                        std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
                    }
                    "cancelled" => cancelled.store(true, Ordering::Relaxed),
                    "closed" => session.core.shared.closed.store(true, Ordering::Relaxed),
                    _ => unreachable!(),
                }
                release.send(()).unwrap();
                let timeout = Duration::from_secs(2);
                let error = match operation {
                    "signals" | "read_signals" => {
                        pins_rx.recv_timeout(timeout).unwrap().unwrap_err()
                    }
                    "break" | "flush_tx" => unit_rx.recv_timeout(timeout).unwrap().unwrap_err(),
                    "flush_rx" => count_rx.recv_timeout(timeout).unwrap().unwrap_err(),
                    "configure" => config_rx.recv_timeout(timeout).unwrap().unwrap_err(),
                    _ => unreachable!(),
                };
                assert!(matches!(
                    error,
                    FlattenError::Timeout(_) | FlattenError::SessionClosed(_)
                ));
                assert!(
                    factory.effects.lock().unwrap().is_empty(),
                    "{operation}: {reason}"
                );
                assert_eq!(session.config().baud, SerialConfig::new("probe").baud);
                assert_eq!(session.core.shared.lock_inner().rx_generation, 0);
                session.close();
            }
        }
    }

    #[test]
    fn flush_rx_excludes_queued_and_decoding_chunks_but_preserves_capture() {
        let directory = tempfile::tempdir().unwrap();
        let jsonl = directory.path().join("flush.jsonl");
        let raw = directory.path().join("flush.bin");
        let (factory, reads) = ProbeFactory::new(None);
        let session = SessionHandle::open(
            SessionId::new(),
            SerialConfig {
                record_to: Some(jsonl.clone()),
                record_rx_to: Some(raw.clone()),
                ..SerialConfig::new("probe")
            },
            factory.clone(),
        )
        .unwrap();
        session.send(b"TX".to_vec()).unwrap();
        factory
            .mock
            .extend_script(vec![ReadStep::Data(b"RETAINED\n".to_vec())]);
        reads.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(wait_until(2000, || session.stats().rx_frames == 1));
        let (gate, entered, release) = TestGate::new();
        session.core.shared.decoders.lock().unwrap().rx = Some(Box::new(GatedDecoder(Some(gate))));
        factory.mock.extend_script(vec![
            ReadStep::Data(b"DECODING\n".to_vec()),
            ReadStep::Data(b"QUEUED\n".to_vec()),
        ]);
        entered.recv_timeout(Duration::from_secs(2)).unwrap();
        for _ in 0..2 {
            reads.recv_timeout(Duration::from_secs(2)).unwrap();
        }
        assert_eq!(session.flush_rx().unwrap(), 1);
        let view = session.read_frames(None, 4096).frames;
        assert_eq!(view.len(), 1);
        assert_eq!(view[0].dir, Direction::Tx);
        factory
            .mock
            .extend_script(vec![ReadStep::Data(b"AFTER\n".to_vec())]);
        reads.recv_timeout(Duration::from_secs(2)).unwrap();
        release.send(()).unwrap();
        assert!(wait_until(2000, || session.stats().rx_frames == 4));
        let view = session.read_frames(None, 4096).frames;
        assert_eq!(view.len(), 2);
        assert_eq!(view[1].text_lossy(), "AFTER\n");
        assert!(view[1].seq > view[0].seq);
        session.close();
        let (recorded, skipped) = crate::record::read_log(&jsonl).unwrap();
        assert_eq!(skipped, 0);
        assert_eq!(recorded.len(), 5);
        assert!(
            recorded
                .windows(2)
                .all(|frames| frames[0].seq < frames[1].seq)
        );
        assert_eq!(
            std::fs::read(raw).unwrap(),
            b"RETAINED\nDECODING\nQUEUED\nAFTER\n"
        );
        assert_eq!(session.stats().rx_bytes, 31);
    }

    #[test]
    fn closing_during_decode_suppresses_late_triggers_but_records_rx() {
        let factory = Arc::new(MockFactory::script(vec![]));
        let session = open_mock(&factory);
        session
            .set_triggers(vec![TriggerSpec {
                id: "late".into(),
                direction: Direction::Rx,
                regex: "TAIL".into(),
                actions: vec![trigger::TriggerAction::Respond {
                    data: Some("ACK".into()),
                    hex: None,
                }],
                min_interval_ms: 0,
            }])
            .unwrap();
        let (gate, entered, release) = TestGate::new();
        session.core.shared.decoders.lock().unwrap().rx = Some(Box::new(GatedDecoder(Some(gate))));
        factory.extend_script(vec![ReadStep::Data(b"TAIL".to_vec())]);
        entered.recv_timeout(Duration::from_secs(2)).unwrap();
        let closing = session.clone();
        let closer = std::thread::spawn(move || closing.close());
        assert!(wait_until(2000, || session
            .core
            .shared
            .closed
            .load(Ordering::Relaxed)));
        release.send(()).unwrap();
        closer.join().unwrap();
        assert!(
            session.trigger_fires().is_empty(),
            "{:?}",
            session.trigger_fires().is_empty()
        );
        assert!(factory.writes.lock().unwrap().is_empty());
        assert_eq!(session.stats().rx_bytes, 4);
        assert_eq!(
            session.read_frames(None, 4096).frames[0].text_lossy(),
            "TAIL"
        );
    }

    #[test]
    fn terminal_reader_failure_stops_writer_and_rejects_commands() {
        let factory = Arc::new(MockFactory::script(vec![ReadStep::Fail(FlattenError::io(
            "unplugged",
        ))]));
        let session = SessionHandle::open(
            SessionId::new(),
            SerialConfig {
                auto_reconnect: ReconnectPolicy::Disabled,
                ..SerialConfig::new("mock")
            },
            as_dyn(&factory),
        )
        .unwrap();
        assert!(wait_until(2000, || session
            .core
            .threads
            .lock()
            .unwrap()
            .iter()
            .all(std::thread::JoinHandle::is_finished)));
        assert!(matches!(session.state(), SessionState::Failed { .. }));
        assert!(matches!(
            session.send(b"late".to_vec()),
            Err(FlattenError::SessionClosed(_))
        ));
        assert!(factory.writes.lock().unwrap().is_empty());
        session.close();
    }

    #[test]
    fn expired_commands_are_not_transmitted() {
        let session = SessionHandle::open(
            SessionId::new(),
            SerialConfig::new("virtual://echo"),
            Arc::new(crate::transport::TransportRegistry::new()),
        )
        .unwrap();
        let (reply, receipt) = std::sync::mpsc::channel();
        let mut command = Command::new(Cmd::Write {
            data: b"expired".to_vec(),
            source: "test".into(),
            reply: Some(reply),
        });
        command.deadline = Instant::now();
        session
            .core
            .cmd_tx
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .try_send(command)
            .unwrap();
        assert!(receipt.recv_timeout(Duration::from_secs(1)).is_err());
        assert_eq!(session.stats().tx_bytes, 0);
        session.close();
    }
    #[test]
    fn tx_triggers_fire_without_recursive_responses() {
        let session = SessionHandle::open(
            SessionId::new(),
            SerialConfig::new("virtual://echo"),
            Arc::new(crate::transport::TransportRegistry::new()),
        )
        .unwrap();
        session
            .set_triggers(vec![
                serde_json::from_value(serde_json::json!({
                    "id":"tx", "direction":"tx", "regex":"PING", "min_interval_ms":0,
                    "actions":[{"action":"respond","data":"PING"}]
                }))
                .unwrap(),
            ])
            .unwrap();
        session.send(b"PING".to_vec()).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(session.trigger_fires().len(), 1);
        assert_eq!(session.stats().tx_frames, 2);
        session.close();
    }
    #[test]
    fn full_command_queue_returns_without_waiting() {
        let (tx, _rx) = sync_channel(1);
        tx.try_send(Command::new(Cmd::FlushTx {
            connection: 0,
            reply: None,
        }))
        .unwrap();
        let session = SessionHandle::open(
            SessionId::new(),
            SerialConfig::new("virtual://echo"),
            Arc::new(crate::transport::TransportRegistry::new()),
        )
        .unwrap();
        *session.core.cmd_tx.lock().unwrap() = Some(tx);
        let started = Instant::now();
        assert!(session.send(b"not queued".to_vec()).is_err());
        assert!(started.elapsed() < Duration::from_millis(100));
        session.close();
    }
    use crate::config::ReconnectPolicy;
    use crate::transport::TransportFactory;
    use crate::transport::mock::{MockFactory, ReadStep};

    /// Wait for a predicate with a bounded test deadline.
    fn wait_until(deadline_ms: u64, mut pred: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_millis(deadline_ms);
        loop {
            if pred() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn open_mock(factory: &Arc<MockFactory>) -> SessionHandle {
        let f: Arc<dyn TransportFactory> = factory.clone();
        SessionHandle::open(SessionId::new(), SerialConfig::new("mock"), f).unwrap()
    }

    #[test]
    fn sent_history_survives_rx_eviction_clear_and_paginates() {
        let factory = Arc::new(MockFactory::script(vec![]));
        let mut config = SerialConfig::new("mock");
        config.buffer.max_frames = 2;
        let s = SessionHandle::open(SessionId::new(), config, as_dyn(&factory)).unwrap();
        let first = s.send_from(b"first\r\n".to_vec(), "GUI#1".into()).unwrap();
        let second = s.send_from(vec![0, 255], "MCP#2".into()).unwrap();
        factory.extend_script(vec![
            ReadStep::Data(b"a".to_vec()),
            ReadStep::Data(b"b".to_vec()),
            ReadStep::Data(b"c".to_vec()),
        ]);
        assert!(wait_until(2000, || s.stats().rx_frames == 3));
        assert!(
            s.read_frames(None, 65536)
                .frames
                .iter()
                .all(|f| f.dir == Direction::Rx)
        );
        let _ = s.clear_buffer();
        let page = s.read_sent(None, 1);
        assert_eq!(page.frames.len(), 1);
        assert_eq!(page.frames[0].seq, first);
        assert_eq!(page.frames[0].source.as_deref(), Some("GUI#1"));
        assert_eq!(page.next_seq, second);
        assert!(!page.up_to_date);
        let next = s.read_sent(Some(page.next_seq), 65536);
        assert_eq!(*next.frames[0].data, vec![0, 255]);
        assert!(next.up_to_date);
        assert!(
            s.read_sent(Some(next.next_seq), 65536).frames.is_empty(),
            "{:?}",
            s.read_sent(Some(next.next_seq), 65536).frames.is_empty()
        );
        s.close();
    }

    /// Coerce the mock factory to a transport-factory trait object for tests.
    fn as_dyn(factory: &Arc<MockFactory>) -> Arc<dyn TransportFactory> {
        factory.clone()
    }

    #[test]
    fn 收发与序号单调() {
        let factory = Arc::new(MockFactory::script(vec![
            ReadStep::Data(b"HELLO".to_vec()),
            ReadStep::Data(b"WORLD".to_vec()),
        ]));
        let s = open_mock(&factory);
        let tx_seq = s.send(b"ATZ".to_vec()).unwrap();
        assert!(
            wait_until(2000, || {
                s.stats().rx_frames >= 2 && s.stats().tx_frames >= 1
            }),
            "应完成 2 RX + 1 TX:{:?}",
            s.stats()
        );
        let page = s.read_frames(None, 1 << 20);
        // Sequences increase strictly regardless of RX/TX arrival order.
        for w in page.frames.windows(2) {
            assert_eq!(w[1].seq, w[0].seq + 1);
        }
        // Verify all three recorded direction markers.
        assert!(
            page.frames
                .iter()
                .any(|f| f.dir == Direction::Rx && f.text_lossy() == "HELLO")
        );
        assert!(
            page.frames
                .iter()
                .any(|f| f.dir == Direction::Rx && f.text_lossy() == "WORLD")
        );
        assert!(
            page.frames
                .iter()
                .any(|f| f.dir == Direction::Tx && f.text_lossy() == "ATZ")
        );
        // The sequence acknowledged by send exists in retained frames.
        assert!(
            page.frames
                .iter()
                .any(|f| f.seq == tx_seq && f.dir == Direction::Tx)
        );
        // The factory observed the write.
        assert_eq!(*factory.writes.lock().unwrap(), vec![b"ATZ".to_vec()]);
        s.close();
        let state = s.wait_closed(Duration::from_secs(2));
        assert!(matches!(state, SessionState::Closed), "{state:?}");
    }

    #[test]
    fn 增量拉取与等待协议() {
        let factory = Arc::new(MockFactory::script(vec![]));
        let s = open_mock(&factory);
        let rev0 = s.change_rev(); // An idle session has a stable revision.
        factory.extend_script(vec![ReadStep::Data(b"AAA".to_vec())]); // Inject data.
        assert!(wait_until(2000, || s.change_rev() > rev0), "rev 应推进");
        let page = s.read_frames(None, 1 << 20);
        assert_eq!(page.frames.len(), 1);
        assert_eq!(*page.frames[0].data, b"AAA".to_vec());
        // Advancing the cursor prevents duplicate reads.
        let page2 = s.read_frames(Some(page.next_seq), 1 << 20);
        assert!(page2.frames.is_empty(), "{:?}", page2.frames.is_empty());
        assert!(page2.up_to_date);
        s.close();
    }

    #[test]
    fn 解码器全链路() {
        let factory = Arc::new(MockFactory::script(vec![]));
        let s = open_mock(&factory);
        s.set_decoder(Some(DecoderSpec {
            name: "json_lines".into(),
            options: serde_json::Value::Null,
        }))
        .unwrap();
        factory.extend_script(vec![ReadStep::Data(
            b"{\"lvl\":\"warn\",\"code\":7}\n".to_vec(),
        )]);
        assert!(
            wait_until(2000, || {
                s.read_frames(None, 1 << 20)
                    .frames
                    .iter()
                    .any(|f| f.decoded.is_some())
            }),
            "应产出解码结果"
        );
        let page = s.read_frames(None, 1 << 20);
        let rx = page.frames.iter().find(|f| f.decoded.is_some()).unwrap();
        let dec = rx.decoded.as_ref().unwrap();
        assert!(dec.text.contains("warn"));
        assert!(dec.fields.iter().any(|f| f.name == "lvl"));
        // The decoder can be removed.
        s.set_decoder(None).unwrap();
        assert_eq!(s.decoder_spec(), None);
        s.close();
    }

    #[test]
    fn 过滤器只影响视图() {
        let factory = Arc::new(MockFactory::script(vec![]));
        factory.extend_script(vec![
            ReadStep::Data(b"BOOT OK\n".to_vec()),
            ReadStep::Data(b"ERROR 5\n".to_vec()),
        ]);
        let s = open_mock(&factory);
        s.set_read_filter(Some(FilterSpec {
            contains: Some("ERROR".into()),
            ..Default::default()
        }))
        .unwrap();
        assert!(
            wait_until(2000, || s.stats().rx_frames >= 2),
            "两帧都应入缓冲(过滤只影响视图)"
        );
        let page = s.read_frames(None, 1 << 20);
        assert_eq!(page.frames.len(), 1, "视图只剩 ERROR 帧");
        assert!(page.frames[0].text_lossy().contains("ERROR"));
        // Full export ignores the view filter when apply_filter=false.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("all.jsonl");
        s.export_log(ExportFormat::Jsonl, &path, false).unwrap();
        let (frames, _) = crate::record::read_log(&path).unwrap();
        assert_eq!(frames.len(), 2, "导出应为全量 2 帧");
        s.close();
    }

    #[test]
    fn 触发器应答与记录() {
        let factory = Arc::new(MockFactory::script(vec![]));
        let s = open_mock(&factory);
        s.set_triggers(vec![TriggerSpec {
            id: "ack".into(),
            direction: Direction::Rx,
            regex: "ERR".into(),
            actions: vec![trigger::TriggerAction::Respond {
                data: Some("ACK\r\n".into()),
                hex: None,
            }],
            min_interval_ms: 0,
        }])
        .unwrap();
        factory.extend_script(vec![ReadStep::Data(b"ERR 1".to_vec())]);
        assert!(
            wait_until(2000, || {
                s.read_frames(None, 1 << 20)
                    .frames
                    .iter()
                    .any(|f| f.dir == Direction::Tx && f.text_lossy().contains("ACK"))
            }),
            "应答 TX 应被记录:{:?}",
            s.trigger_fires()
        );
        let fires = s.trigger_fires();
        assert!(!fires.is_empty(), "应有触发记录");
        assert!(fires[0].responded);
        assert_eq!(fires[0].matched, "ERR");
        // Taking the history clears it.
        assert!(
            !s.drain_trigger_fires().is_empty(),
            "{:?}",
            s.drain_trigger_fires().is_empty()
        );
        assert!(
            s.trigger_fires().is_empty(),
            "{:?}",
            s.trigger_fires().is_empty()
        );
        s.close();
    }

    #[test]
    fn 读故障注入_自动重连() {
        // Initial open succeeds (skip_first=1); reopening after a read error succeeds.
        let factory = Arc::new(MockFactory::script(vec![]).skip_first_opens(1));
        factory.extend_script(vec![
            ReadStep::Fail(FlattenError::io("模拟拔出")),
            ReadStep::Data(b"RECOVERED".to_vec()),
        ]);
        let s = SessionHandle::open(
            SessionId::new(),
            SerialConfig {
                auto_reconnect: ReconnectPolicy::Enabled {
                    max_attempts: 5,
                    initial_ms: 10,
                    max_ms: 50,
                },
                ..SerialConfig::new("mock")
            },
            as_dyn(&factory),
        )
        .unwrap();
        assert!(
            wait_until(3000, || {
                s.read_frames(None, 1 << 20)
                    .frames
                    .iter()
                    .any(|f| f.text_lossy().contains("RECOVERED"))
            }),
            "重连后应收到数据:{:?}",
            s.stats()
        );
        assert_eq!(s.stats().reconnects, 1, "重连恰好一次");
        assert_eq!(factory.open_count(), 2, "open 恰好两次");
        assert!(matches!(s.state(), SessionState::Connected));
        s.close();
    }

    #[test]
    fn 重连放弃进入失败态() {
        // Initial open succeeds; two injected reopen failures exhaust max_attempts=2.
        let factory = Arc::new(MockFactory::script(vec![]).skip_first_opens(1));
        factory.extend_script(vec![ReadStep::Fail(FlattenError::io("致命"))]);
        factory.fail_opens(vec![
            FlattenError::io("重开失败 1"),
            FlattenError::io("重开失败 2"),
        ]);
        let s = SessionHandle::open(
            SessionId::new(),
            SerialConfig {
                auto_reconnect: ReconnectPolicy::Enabled {
                    max_attempts: 2,
                    initial_ms: 5,
                    max_ms: 20,
                },
                ..SerialConfig::new("mock")
            },
            as_dyn(&factory),
        )
        .unwrap();
        let state = s.wait_closed(Duration::from_secs(5));
        assert!(
            matches!(state, SessionState::Failed { .. }),
            "应进入 Failed:{state:?}"
        );
    }

    #[test]
    fn 禁止重连则立即失败() {
        let factory = Arc::new(MockFactory::script(vec![]));
        factory.extend_script(vec![ReadStep::Fail(FlattenError::io("致命"))]);
        let s = SessionHandle::open(
            SessionId::new(),
            SerialConfig {
                auto_reconnect: ReconnectPolicy::Disabled,
                ..SerialConfig::new("mock")
            },
            as_dyn(&factory),
        )
        .unwrap();
        let state = s.wait_closed(Duration::from_secs(2));
        assert!(matches!(state, SessionState::Failed { .. }), "{state:?}");
    }

    #[test]
    fn 在线变参() {
        let factory = Arc::new(MockFactory::script(vec![]));
        let s = open_mock(&factory);
        let cfg = s
            .configure(ConfigPatch {
                baud: Some(9_600),
                label: Some(Some("demo".into())),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(cfg.baud, 9_600);
        assert_eq!(cfg.label.as_deref(), Some("demo"));
        assert_eq!(s.config().baud, 9_600);
        assert_eq!(s.config().label.as_deref(), Some("demo"));
        assert!(
            s.configure(ConfigPatch {
                baud: Some(0),
                ..Default::default()
            })
            .is_err()
        );
        assert_eq!(
            s.config().baud,
            9_600,
            "invalid changes must not corrupt the active configuration"
        );
        s.close();
    }

    #[test]
    fn 控制线与信号() {
        let factory = Arc::new(MockFactory::script(vec![]));
        let s = open_mock(&factory);
        let pins = s.set_signals(Some(true), Some(false)).unwrap();
        assert!(pins.dtr);
        assert!(!pins.rts);
        let pins = s.read_signals().unwrap();
        assert!(pins.dtr);
        s.close();
    }

    #[test]
    fn 清空缓冲保留序号() {
        let factory = Arc::new(MockFactory::script(vec![]));
        factory.extend_script(vec![
            ReadStep::Data(b"AAA".to_vec()),
            ReadStep::Data(b"BBB".to_vec()),
        ]);
        let s = open_mock(&factory);
        assert!(wait_until(2000, || s.stats().rx_frames >= 2));
        let last_before = s.stats().buffer.last_seq;
        assert!(s.clear_buffer() >= 2);
        let page = s.read_frames(None, 1 << 20);
        assert!(page.frames.is_empty(), "{:?}", page.frames.is_empty());
        // New frames continue above the prior sequence numbers.
        factory.extend_script(vec![ReadStep::Data(b"CCC".to_vec())]);
        assert!(wait_until(2000, || s.stats().buffer.last_seq.unwrap_or(0)
            > last_before.unwrap_or(0)));
        s.close();
    }

    #[test]
    fn 关闭后发送报会话关闭() {
        let factory = Arc::new(MockFactory::script(vec![]));
        let s = open_mock(&factory);
        s.close();
        let _ = s.wait_closed(Duration::from_secs(2));
        let r = s.send(b"x".to_vec());
        assert!(matches!(r, Err(FlattenError::SessionClosed(_))), "{r:?}");
    }

    #[test]
    fn 句柄克隆共享会话() {
        let factory = Arc::new(MockFactory::script(vec![]));
        let s = open_mock(&factory);
        let s2 = s.clone();
        // Sending through either clone is visible through both handles.
        let _ = s2.send(b"X".to_vec()).unwrap();
        assert!(wait_until(2000, || s.stats().tx_frames >= 1));
        // Dropping one clone does not close the session.
        drop(s2);
        let r = s.send(b"Y".to_vec());
        assert!(r.is_ok(), "克隆释放不应关闭会话:{r:?}");
        s.close();
    }

    #[test]
    fn 环形缓冲溢出计数可见() {
        let factory = Arc::new(MockFactory::script(vec![]));
        let cfg = SerialConfig {
            buffer: crate::config::BufferPolicy {
                max_frames: 8,
                max_bytes: 1 << 20,
            },
            ..SerialConfig::new("mock")
        };
        let s = SessionHandle::open(SessionId::new(), cfg, as_dyn(&factory)).unwrap();
        let steps: Vec<ReadStep> = (0..20u32)
            .map(|i| ReadStep::Data(vec![0x41; 8 + i as usize]))
            .collect();
        factory.extend_script(steps);
        assert!(
            wait_until(3000, || s.stats().dropped_rx > 0),
            "应产生溢出计数:{:?}",
            s.stats()
        );
        let stats = s.stats();
        assert!(stats.dropped_rx > 0);
        assert!(stats.rx_frames >= 20);
        // Retained frame count respects its limit.
        assert!(
            stats.buffer.frames <= 8,
            "缓冲不应超上限:{:?}",
            stats.buffer
        );
        s.close();
    }

    #[test]
    fn 录制器旁路全量落盘() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("session.jsonl");
        let factory = Arc::new(MockFactory::script(vec![]));
        factory.extend_script(vec![ReadStep::Data(b"DATA-1\n".to_vec())]);
        let cfg = SerialConfig {
            record_to: Some(log.clone()),
            ..SerialConfig::new("mock")
        };
        let s = SessionHandle::open(SessionId::new(), cfg, as_dyn(&factory)).unwrap();
        let _ = s.send(b"CMD-1".to_vec()).unwrap();
        assert!(wait_until(2000, || s.stats().rx_frames >= 1));
        s.close();
        let _ = s.wait_closed(Duration::from_secs(2));
        let (frames, skipped) = crate::record::read_log(&log).unwrap();
        assert_eq!(skipped, 0);
        assert!(frames.iter().any(|f| f.dir == Direction::Rx));
        assert!(frames.iter().any(|f| f.dir == Direction::Tx));
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn recording_starts_at_first_rx_and_flushes_while_view_is_idle() {
        let dir = tempfile::tempdir().unwrap();
        let rx_path = dir.path().join("boot.log");
        let jsonl = dir.path().join("boot.jsonl");
        // Data is immediately available on open, before any GUI reads occur.
        let factory = Arc::new(MockFactory::script(vec![
            ReadStep::Data(b"BOOT\r".to_vec()),
            ReadStep::Data(b"\nlogin: ".to_vec()),
        ]));
        let config = SerialConfig {
            record_to: Some(jsonl.clone()),
            record_rx_to: Some(rx_path.clone()),
            ..SerialConfig::new("mock")
        };
        let session = SessionHandle::open(SessionId::new(), config, as_dyn(&factory)).unwrap();
        session
            .set_read_filter(Some(FilterSpec {
                contains: Some("never matches".into()),
                ..Default::default()
            }))
            .unwrap();
        session.send(b"command".to_vec()).unwrap();
        assert!(wait_until(2000, || std::fs::read(&rx_path).unwrap()
            == b"BOOT\r\nlogin: "));
        // No further data arrives: both files must flush without closing the session.
        let (frames, skipped) = crate::record::read_log(&jsonl).unwrap();
        assert_eq!(skipped, 0);
        assert!(frames.iter().any(|f| f.dir == Direction::Tx));
        assert!(
            session.read_frames(None, 4096).frames.is_empty(),
            "{:?}",
            session.read_frames(None, 4096).frames.is_empty()
        );
        let _ = session.clear_buffer();
        assert_eq!(std::fs::read(&rx_path).unwrap(), b"BOOT\r\nlogin: ");
        assert!(session.stats().recording.active);
        assert!(
            session.stats().recording.errors.is_empty(),
            "{:?}",
            session.stats().recording.errors.is_empty()
        );
        assert!(session.stats().recording.readable_path.is_none());
        session.close();
        assert!(!session.stats().recording.active);
    }

    #[test]
    fn readable_export_creates_exactly_one_file() {
        let dir = tempfile::tempdir().unwrap();
        let factory = Arc::new(MockFactory::script(vec![]));
        let session = open_mock(&factory);
        session
            .send_from(b"status\r\n".to_vec(), "GUI#7".into())
            .unwrap();
        let path = dir.path().join("capture.txt");
        let text = session.export_readable(&path).unwrap();
        assert_eq!(text.frames, 1);
        let readable = std::fs::read_to_string(&path).unwrap();
        assert!(readable.contains("status\\r\\n"));
        assert!(readable.contains("GUI#7"));
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        session.close();
    }

    #[test]
    fn recording_path_failure_prevents_opening_device() {
        let directory = tempfile::tempdir().unwrap();
        let blocked = directory.path().join("not-a-directory");
        std::fs::write(&blocked, b"file").unwrap();
        let factory = Arc::new(MockFactory::script(vec![]));
        let config = SerialConfig {
            record_rx_to: Some(blocked.join("boot.log")),
            ..SerialConfig::new("mock")
        };
        assert!(SessionHandle::open(SessionId::new(), config, as_dyn(&factory)).is_err());
        assert_eq!(
            factory.open_count(),
            0,
            "do not reset hardware when recording cannot start"
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn recording_write_failure_is_visible_in_statistics() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("full.jsonl");
        std::os::unix::fs::symlink("/dev/full", &path).unwrap();
        let factory = Arc::new(MockFactory::script(vec![]));
        let config = SerialConfig {
            record_to: Some(path),
            ..SerialConfig::new("mock")
        };
        let session = SessionHandle::open(SessionId::new(), config, as_dyn(&factory)).unwrap();
        factory.extend_script(vec![ReadStep::Data(b"boot".to_vec())]);
        assert!(wait_until(2000, || !session
            .stats()
            .recording
            .errors
            .is_empty()));
        assert!(!session.stats().recording.active);
        session.close();
        assert!(!session.stats().recording.active);
        assert!(
            !session.stats().recording.errors.is_empty(),
            "{:?}",
            session.stats().recording.errors.is_empty()
        );
    }
}
