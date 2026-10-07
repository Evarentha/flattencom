/*
 * flattencom - flattencom Flattencomd Src State
 *
 * Coordinates session ownership, device reuse, per-client filters and bounded event delivery.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

//! Daemon state: sessions, connections, event fan-out and frame/statistics/trigger/state pumps.
//!
//! ## Concurrency model
//!
//! - Tokio connection tasks dispatch blocking core requests through spawn_blocking and return replies;
//! - Dedicated session pump threads batch frames and publish events every 32 ms;
//! - Standard mutexes protect state shared by pump threads and Tokio tasks;
//! - Bounded outbound channels drop excess notifications; clients recover frames using cursors.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::mpsc::Sender;

use flattencom_core::config::SerialConfig;
use flattencom_core::ids::SessionId;
use flattencom_core::session::{SessionHandle, SessionState};
use flattencom_core::transport::{TransportFactory, TransportRegistry};

use flattencom_proto::methods::{
    FrameFormat, OpenSessionResult, RpcNotification, SessionSummary, frame_out,
};

/// Maximum outbound channel backlog for bounded slow-client memory usage.
pub const OUT_BACKLOG_CAP: usize = 64;

/// Frame-event batch cadence.
const FRAME_TICK: Duration = Duration::from_millis(32);

/// Statistics-event cadence.
const STATS_TICK: Duration = Duration::from_secs(1);

/// Maximum bytes in one frame-event batch.
const FRAME_BATCH_BYTES: u64 = 64 * 1024;

/// Session entry.
pub struct SessionEntry {
    /// Core-engine session handle.
    pub handle: SessionHandle,
    /// Owning client label.
    pub owner: String,
    /// Connection ID of the opener.
    pub owner_conn: u64,
    /// Stable normalized device identity, captured before open (survives unplug).
    port_key: String,
}

impl SessionEntry {
    fn summary(&self) -> SessionSummary {
        let config = self.handle.config();
        SessionSummary {
            session_id: self.handle.id().to_string(),
            label: config.label.clone(),
            path: config.path.clone(),
            owner: self.owner.clone(),
            config,
            state: self.handle.state(),
            stats: self.handle.stats(),
        }
    }
}

// Match Linux by-id symlinks to their tty device; COM names are case-insensitive
// and may include the Win32 device prefix. Virtual URIs retain exact identity.
fn port_key(path: &str) -> String {
    if TransportRegistry::is_virtual(path) {
        return path.to_owned();
    }
    #[cfg(windows)]
    {
        path.strip_prefix(r"\\.\")
            .unwrap_or(path)
            .to_ascii_uppercase()
    }
    #[cfg(not(windows))]
    {
        std::fs::canonicalize(path)
            .map_or_else(|_| path.to_owned(), |p| p.to_string_lossy().into_owned())
    }
}

/// Connection entry.
pub struct ConnEntry {
    /// Client label supplied by hello.
    pub label: String,
    /// Outbound replies and notifications correlated by JSON-RPC IDs and method names.
    pub out: Sender<String>,
    /// Session-to-event-kind subscriptions; an empty set subscribes to all kinds.
    pub subs: HashMap<SessionId, HashSet<String>>,
    /// Independent client view filters that never modify capture data.
    pub filters: HashMap<SessionId, flattencom_core::filter::CompiledFilter>,
}

/// Global daemon state.
pub struct DaemonState {
    /// Notes, explicit AI selections and cancellable long-running operations.
    pub workbench: Arc<crate::workbench::Workbench>,
    /// Session list.
    pub sessions: Mutex<HashMap<SessionId, SessionEntry>>,
    /// Serializes opening transactions without holding the session table during IO.
    opening: Mutex<()>,
    /// Connection table.
    pub conns: Mutex<HashMap<u64, ConnEntry>>,
    /// Connection ID allocator.
    next_conn: AtomicU64,
    /// Physical/virtual transport factory.
    pub factory: Arc<dyn TransportFactory>,
    /// Service start time.
    pub started: Instant,
    /// Shutdown flag set by the shutdown operation.
    pub shutting_down: AtomicBool,
    /// Listening socket path.
    pub socket_path: PathBuf,
    /// Authentication token.
    pub token: String,
    /// Version string.
    pub version: String,
}

impl DaemonState {
    /// Connection-scoped operation attribution, derived from the authenticated connection.
    pub fn client_source(&self, id: u64) -> String {
        let label = self
            .conns
            .lock()
            .expect("connection lock")
            .get(&id)
            .map_or_else(|| "disconnected".into(), |c| c.label.clone());
        format!("{label}#{id}")
    }
    /// Construct state with a transport registry, token and endpoint.
    #[must_use]
    pub fn new(socket_path: PathBuf, token: String) -> Self {
        Self {
            workbench: Arc::default(),
            sessions: Mutex::new(HashMap::new()),
            opening: Mutex::new(()),
            conns: Mutex::new(HashMap::new()),
            next_conn: AtomicU64::new(1),
            factory: Arc::new(TransportRegistry::new()),
            started: Instant::now(),
            shutting_down: AtomicBool::new(false),
            socket_path,
            token,
            version: env!("CARGO_PKG_VERSION").to_owned(),
        }
    }

    /// Allocate a connection ID.
    #[must_use]
    pub fn alloc_conn_id(&self) -> u64 {
        self.next_conn.fetch_add(1, Ordering::SeqCst)
    }

    /// Register a connection.
    pub fn register_conn(&self, conn_id: u64, label: String, out: Sender<String>) {
        self.conns
            .lock()
            .expect("Connection table lock poisoned")
            .insert(
                conn_id,
                ConnEntry {
                    label,
                    out,
                    subs: HashMap::new(),
                    filters: HashMap::new(),
                },
            );
    }

    /// Deregister a connection and clear all its subscriptions.
    pub fn deregister_conn(&self, conn_id: u64) {
        self.conns
            .lock()
            .expect("Connection table lock poisoned")
            .remove(&conn_id);
    }

    /// Set a connection's subscription for one session.
    pub fn set_subscription(
        &self,
        conn_id: u64,
        session_id: SessionId,
        kinds: HashSet<String>,
    ) -> Result<(), flattencom_core::FlattenError> {
        self.update_session_view(conn_id, session_id, |conn| {
            conn.subs.insert(session_id, kinds);
        })
    }

    /// Replace a filter only while its session is still registered.
    pub fn set_view_filter(
        &self,
        conn_id: u64,
        session_id: SessionId,
        filter: Option<flattencom_core::filter::CompiledFilter>,
    ) -> Result<(), flattencom_core::FlattenError> {
        self.update_session_view(conn_id, session_id, |conn| {
            if let Some(filter) = filter {
                conn.filters.insert(session_id, filter);
            } else {
                conn.filters.remove(&session_id);
            }
        })
    }

    // Session membership and view insertion share the same transaction as closure.
    // Always acquire sessions before conns; never hold either lock across device IO.
    fn update_session_view(
        &self,
        conn_id: u64,
        session_id: SessionId,
        update: impl FnOnce(&mut ConnEntry),
    ) -> Result<(), flattencom_core::FlattenError> {
        let sessions = self.sessions.lock().expect("Session table lock poisoned");
        if !sessions.contains_key(&session_id) {
            return Err(flattencom_core::FlattenError::SessionNotFound(
                session_id.to_string(),
            ));
        }
        if let Some(conn) = self
            .conns
            .lock()
            .expect("Connection table lock poisoned")
            .get_mut(&conn_id)
        {
            update(conn);
        }
        Ok(())
    }

    /// Remove a subscription.
    pub fn clear_subscription(&self, conn_id: u64, session_id: SessionId) {
        if let Some(conn) = self
            .conns
            .lock()
            .expect("Connection table lock poisoned")
            .get_mut(&conn_id)
        {
            conn.subs.remove(&session_id);
        }
    }

    /// Clone a lightweight client filter without holding the connection lock across session access.
    pub fn view_filter(
        &self,
        conn_id: u64,
        session_id: &SessionId,
    ) -> Option<flattencom_core::filter::CompiledFilter> {
        self.conns
            .lock()
            .expect("Connection table lock poisoned")
            .get(&conn_id)
            .and_then(|c| c.filters.get(session_id))
            .cloned()
    }

    /// Project and publish frames through each client's filter.
    pub fn push_frames(&self, id: &SessionId, page: &flattencom_core::session::FramesPage) {
        for conn in self
            .conns
            .lock()
            .expect("Connection table lock poisoned")
            .values()
        {
            if !conn
                .subs
                .get(id)
                .is_some_and(|kinds| kinds.is_empty() || kinds.contains("frames"))
            {
                continue;
            }
            let frames: Vec<_> = page
                .frames
                .iter()
                .filter(|f| conn.filters.get(id).is_none_or(|filter| filter.matches(f)))
                .map(|f| frame_out(f, FrameFormat::Decoded))
                .collect();
            let note = RpcNotification {
                jsonrpc: "2.0".into(),
                method: "frames".into(),
                params: serde_json::json!({
                    "session_id": id.to_string(), "frames": frames, "next_seq": page.next_seq,
                    "dropped_rx": page.dropped_rx, "up_to_date": page.up_to_date,
                    "first_seq": page.first_seq, "last_seq": page.last_seq,
                }),
            };
            if let Some(line) = crate::wire::notification(&note) {
                push_out(conn, &line);
            }
        }
    }

    /// Broadcast one notification line to all connections.
    pub fn broadcast(&self, line: &str) {
        for conn in self
            .conns
            .lock()
            .expect("Connection table lock poisoned")
            .values()
        {
            push_out(conn, line);
        }
    }

    /// Publish to subscribers of a session and event kind; empty kind sets match all events.
    pub fn push_subscribed(&self, session_id: &SessionId, kind: &str, line: &str) {
        for conn in self
            .conns
            .lock()
            .expect("Connection table lock poisoned")
            .values()
        {
            match conn.subs.get(session_id) {
                Some(kinds) if kinds.is_empty() || kinds.contains(kind) => {
                    push_out(conn, line);
                }
                Some(_) | None => {}
            }
        }
    }

    /// Check for subscribers before preparing trigger notifications.
    #[must_use]
    pub fn has_subscriber(&self, session_id: &SessionId, kind: &str) -> bool {
        self.conns.lock().expect("Connection table lock poisoned").values().any(
            |c| matches!(c.subs.get(session_id), Some(ks) if ks.is_empty() || ks.contains(kind)),
        )
    }

    /// Session summary list.
    #[must_use]
    pub fn session_summaries(&self) -> Vec<SessionSummary> {
        let sessions = self.sessions.lock().expect("Session table lock poisoned");
        let mut out: Vec<SessionSummary> = sessions.values().map(SessionEntry::summary).collect();
        out.sort_by(|a, b| a.session_id.cmp(&b.session_id));
        out
    }

    /// Active sessions for a device path, including supported path aliases.
    pub fn sessions_for_port(&self, path: &str) -> Vec<String> {
        let key = port_key(path);
        self.sessions
            .lock()
            .expect("Session table lock poisoned")
            .values()
            .filter(|entry| {
                (port_key(&entry.handle.config().path) == key || entry.handle.config().path == path)
                    && matches!(
                        entry.handle.state(),
                        SessionState::Connected | SessionState::Reconnecting { .. }
                    )
            })
            .map(|entry| entry.handle.id().to_string())
            .collect()
    }

    /// Atomically reuse an active session or open a new one. Reuse never opens
    /// recording files, changes parameters, transfers ownership or starts a pump.
    pub fn open_session(
        &self,
        owner: String,
        owner_conn: u64,
        mut config: SerialConfig,
    ) -> Result<OpenSessionResult, flattencom_core::FlattenError> {
        config.validate()?;
        let _opening = self.opening.lock().expect("session opening");
        let key = port_key(&config.path);
        config.device_identity = flattencom_core::discovery::list_ports()
            .ok()
            .and_then(|ports| {
                ports
                    .iter()
                    .find(|p| port_key(&p.path) == key)
                    .and_then(flattencom_core::discovery::DeviceIdentity::from_port)
                    .filter(|identity| identity.resolve(&ports).is_ok())
            });
        let sessions = self.sessions.lock().expect("Session table lock poisoned");
        if let Some(entry) = sessions.values().find(|entry| {
            let existing = entry.handle.config();
            let same = match (&existing.device_identity, &config.device_identity) {
                (Some(a), Some(b)) => a == b,
                _ => port_key(&existing.path) == key || existing.path == config.path,
            };
            same && matches!(
                entry.handle.state(),
                SessionState::Connected | SessionState::Reconnecting { .. }
            )
        }) {
            return Ok(OpenSessionResult {
                session: entry.summary(),
                reused: true,
            });
        }
        // Failed sessions remain inspectable but no longer own a serial handle.
        let failed: Vec<_> = sessions
            .values()
            .filter(|entry| entry.port_key == key || entry.handle.config().path == config.path)
            .map(|entry| entry.handle.clone())
            .collect();
        drop(sessions);
        for handle in failed {
            handle.close();
        }
        if self.is_shutting_down() {
            return Err(flattencom_core::FlattenError::io("Service stopping"));
        }
        let id = SessionId::new();
        let factory: Arc<dyn TransportFactory> = self.factory.clone();
        let handle = SessionHandle::open(id, config, factory)?;
        let entry = SessionEntry {
            handle,
            owner,
            owner_conn,
            port_key: key,
        };
        let session = entry.summary();
        let mut sessions = self.sessions.lock().expect("Session table lock poisoned");
        if self.is_shutting_down() {
            drop(sessions);
            entry.handle.close();
            return Err(flattencom_core::FlattenError::io("Service stopping"));
        }
        sessions.insert(id, entry);
        Ok(OpenSessionResult {
            session,
            reused: false,
        })
    }

    /// Remove a session, close its workers and notify clients.
    #[must_use]
    pub fn close_session(
        &self,
        session_id: &SessionId,
    ) -> Option<flattencom_core::stats::SessionStats> {
        let entry = {
            let mut sessions = self.sessions.lock().expect("Session table lock poisoned");
            let entry = sessions.remove(session_id);
            let mut conns = self.conns.lock().expect("Connection table lock poisoned");
            for conn in conns.values_mut() {
                conn.filters.remove(session_id);
                conn.subs.remove(session_id);
            }
            entry
        };
        entry.map(|e| {
            self.workbench.close_session(&session_id.to_string());
            e.handle.close();
            let stats = e.handle.stats();
            // Publish a final closed state even if the pump does not emit another notification.
            self.notify_state(session_id, &SessionState::Closed);
            stats
        })
    }

    /// Broadcast session state changes to all connections.
    pub fn notify_state(&self, session_id: &SessionId, state: &SessionState) {
        let note = RpcNotification {
            jsonrpc: "2.0".into(),
            method: "session_state".into(),
            params: serde_json::json!({
                "session_id": session_id.to_string(),
                "state": state,
            }),
        };
        if let Some(line) = crate::wire::notification(&note) {
            self.broadcast(&line);
        }
    }

    /// Whether the service is shutting down.
    #[must_use]
    pub fn is_shutting_down(&self) -> bool {
        self.shutting_down.load(Ordering::Relaxed)
    }

    /// Start the frame/statistics/trigger/state/overflow pump for a session.
    pub fn spawn_pump(self: &Arc<Self>, session_id: SessionId) {
        let state = Arc::clone(self);
        let handle = {
            let sessions = state.sessions.lock().expect("Session table lock poisoned");
            sessions.get(&session_id).map(|e| e.handle.clone())
        };
        let Some(handle) = handle else { return };
        std::thread::Builder::new()
            .name(format!("flattencom-pump-{session_id}"))
            .spawn(move || pump_loop(state, session_id, handle))
            .expect("Failed to start event pump");
    }
}

/// Nonblocking outbound notification with slow-client backpressure handling.
fn push_out(conn: &ConnEntry, line: &str) {
    if conn.out.try_send(line.to_owned()).is_err() {
        tracing::debug!(client = %conn.label, "Event queue full or connection closed; use cursor reads to retrieve retained frames");
    }
}

/// Session event-pump loop.
fn pump_loop(state: Arc<DaemonState>, session_id: SessionId, handle: SessionHandle) {
    let mut cursor: Option<u64> = None;
    let mut last_state = handle.state();
    let mut last_dropped: u64 = 0;
    let mut last_stats_at = Instant::now();
    let mut last_stats = handle.stats();
    let mut fire_cursor = None;
    loop {
        // Wait for the next 32 ms sampling tick.
        std::thread::sleep(FRAME_TICK);
        {
            // 1) Broadcast state changes to all connections.
            let st = handle.state();
            if st != last_state {
                last_state = st.clone();
                state.notify_state(&session_id, &st);
            }
            // 2) Publish frame batches to frame subscribers.
            let page = handle.read_window(cursor, None, false, FRAME_BATCH_BYTES);
            cursor = Some(page.next_seq);
            if !page.frames.is_empty() {
                state.push_frames(&session_id, &page);
            }
            // 3) Publish visible overflow counts as separate statistics notifications.
            if page.dropped_rx > last_dropped {
                let dropped = page.dropped_rx - last_dropped;
                last_dropped = page.dropped_rx;
                let note = RpcNotification {
                    jsonrpc: "2.0".into(),
                    method: "buffer_overflow".into(),
                    params: serde_json::json!({
                        "session_id": session_id.to_string(),
                        "dropped": dropped,
                    }),
                };
                if let Some(line) = crate::wire::notification(&note) {
                    state.push_subscribed(&session_id, "stats", &line);
                }
            }
            // 4) Publish trigger matches to trigger_fires subscribers.
            //       Only prepare push events when subscribed; retain history for get_trigger_fires.
            //       Push and pull consumers must not clear each other's retained records.
            if state.has_subscriber(&session_id, "trigger_fires") {
                let fires: Vec<_> = handle
                    .trigger_fires()
                    .into_iter()
                    .filter(|f| fire_cursor.is_none_or(|s| f.seq > s))
                    .collect();
                if let Some(fire) = fires.last() {
                    fire_cursor = Some(fire.seq);
                }
                if !fires.is_empty() {
                    let note = RpcNotification {
                        jsonrpc: "2.0".into(),
                        method: "trigger_fired".into(),
                        params: serde_json::json!({
                            "session_id": session_id.to_string(),
                            "fires": fires,
                        }),
                    };
                    if let Some(line) = crate::wire::notification(&note) {
                        state.push_subscribed(&session_id, "trigger_fires", &line);
                    }
                }
            }
        }
        // 5) Publish statistics on the one-second cadence.
        if last_stats_at.elapsed() >= STATS_TICK {
            last_stats_at = Instant::now();
            let stats = handle.stats();
            if stats != last_stats {
                last_stats = stats.clone();
                let note = RpcNotification {
                    jsonrpc: "2.0".into(),
                    method: "stats".into(),
                    params: serde_json::json!({
                        "session_id": session_id.to_string(),
                        "stats": stats,
                    }),
                };
                if let Some(line) = crate::wire::notification(&note) {
                    state.push_subscribed(&session_id, "stats", &line);
                }
            }
        }
        // On termination, publish any final state change and stop the pump.
        if matches!(
            last_state,
            SessionState::Closed | SessionState::Failed { .. }
        ) {
            let st = handle.state();
            if st != last_state {
                state.notify_state(&session_id, &st);
            }
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flattencom_core::transport::mock::{MockFactory, ReadStep};

    #[test]
    fn closing_session_retires_views_even_when_updates_race() {
        let state = Arc::new(DaemonState::new("unused".into(), "token".into()));
        let (out, _receiver) = tokio::sync::mpsc::channel(64);
        for conn in 1..=2 {
            state.register_conn(conn, "test".into(), out.clone());
        }
        let filter = flattencom_core::filter::CompiledFilter::compile(
            &flattencom_core::filter::FilterSpec {
                contains: Some("x".repeat(1024)),
                ..Default::default()
            },
        )
        .unwrap();
        for _ in 0..12 {
            let opened = state
                .open_session("test".into(), 1, SerialConfig::new("virtual://echo"))
                .unwrap();
            let id = SessionId::parse(&opened.session.session_id).unwrap();
            for conn in 1..=2 {
                state.set_subscription(conn, id, HashSet::new()).unwrap();
                state
                    .set_view_filter(conn, id, Some(filter.clone()))
                    .unwrap();
            }
            let gate = Arc::new(std::sync::Barrier::new(3));
            let updates: Vec<_> = (1..=2)
                .map(|conn| {
                    let state = state.clone();
                    let gate = gate.clone();
                    let filter = filter.clone();
                    std::thread::spawn(move || {
                        gate.wait();
                        for _ in 0..50 {
                            let _ = state.set_view_filter(conn, id, Some(filter.clone()));
                            let _ = state.set_subscription(conn, id, HashSet::new());
                        }
                    })
                })
                .collect();
            gate.wait();
            assert!(state.close_session(&id).is_some());
            for update in updates {
                update.join().unwrap();
            }
            // A compilation started before close must not insert its result afterward.
            assert!(state.set_view_filter(1, id, Some(filter.clone())).is_err());
            assert!(state.set_subscription(1, id, HashSet::new()).is_err());
            for conn in state.conns.lock().unwrap().values() {
                assert!(conn.filters.is_empty());
                assert!(conn.subs.is_empty());
            }
        }
    }

    struct TailFactory {
        entered: std::sync::mpsc::Sender<()>,
        release: Arc<Mutex<std::sync::mpsc::Receiver<()>>>,
    }

    struct TailTransport {
        inner: Box<dyn flattencom_core::transport::SerialTransport>,
        entered: Option<std::sync::mpsc::Sender<()>>,
        release: Arc<Mutex<std::sync::mpsc::Receiver<()>>>,
    }

    impl TransportFactory for TailFactory {
        fn open(
            &self,
            config: &SerialConfig,
        ) -> Result<
            Box<dyn flattencom_core::transport::SerialTransport>,
            flattencom_core::FlattenError,
        > {
            Ok(Box::new(TailTransport {
                inner: MockFactory::script(vec![]).open(config)?,
                entered: Some(self.entered.clone()),
                release: self.release.clone(),
            }))
        }
        fn kind(&self) -> &'static str {
            "controlled-tail"
        }
    }

    impl flattencom_core::transport::SerialTransport for TailTransport {
        fn read(&mut self, buf: &mut [u8]) -> Result<usize, flattencom_core::FlattenError> {
            if let Some(entered) = self.entered.take() {
                entered.send(()).unwrap();
                self.release
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap();
                buf[..4].copy_from_slice(b"TAIL");
                Ok(4)
            } else {
                Ok(0)
            }
        }
        fn write(&mut self, data: &[u8]) -> Result<usize, flattencom_core::FlattenError> {
            self.inner.write(data)
        }
        fn set_params(
            &mut self,
            config: &SerialConfig,
        ) -> Result<(), flattencom_core::FlattenError> {
            self.inner.set_params(config)
        }
        fn set_signals(
            &mut self,
            dtr: Option<bool>,
            rts: Option<bool>,
        ) -> Result<(), flattencom_core::FlattenError> {
            self.inner.set_signals(dtr, rts)
        }
        fn read_signals(
            &mut self,
        ) -> Result<flattencom_core::transport::PinStates, flattencom_core::FlattenError> {
            self.inner.read_signals()
        }
        fn set_break(&mut self, duration: Duration) -> Result<(), flattencom_core::FlattenError> {
            self.inner.set_break(duration)
        }
        fn flush_rx(&mut self) -> Result<(), flattencom_core::FlattenError> {
            self.inner.flush_rx()
        }
        fn flush_tx(&mut self) -> Result<(), flattencom_core::FlattenError> {
            self.inner.flush_tx()
        }
    }

    #[test]
    fn close_returns_statistics_after_in_flight_tail_and_capture_finalize() {
        close_with_tail(false);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn close_returns_final_capture_failure() {
        close_with_tail(true);
    }

    fn close_with_tail(fail_capture: bool) {
        let dir = tempfile::tempdir().unwrap();
        let (entered, reading) = std::sync::mpsc::channel();
        let (release, gate) = std::sync::mpsc::channel();
        let mut state = DaemonState::new(dir.path().join("socket"), "token".into());
        state.factory = Arc::new(TailFactory {
            entered,
            release: Arc::new(Mutex::new(gate)),
        });
        let mut config = SerialConfig::new("tail-uart");
        let path = dir.path().join(if fail_capture {
            "capture.jsonl"
        } else {
            "capture.log"
        });
        #[cfg(target_os = "linux")]
        if fail_capture {
            std::os::unix::fs::symlink("/dev/full", &path).unwrap();
        }
        config.record_to = Some(path.clone());
        let opened = state.open_session("test".into(), 1, config).unwrap();
        let id = SessionId::parse(&opened.session.session_id).unwrap();
        let handle = state.sessions.lock().unwrap()[&id].handle.clone();
        reading.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(handle.stats().rx_bytes, 0);
        assert!(
            handle.stats().recording.errors.is_empty(),
            "{:?}",
            handle.stats().recording.errors.is_empty()
        );
        let state = Arc::new(state);
        let (finished, result) = std::sync::mpsc::channel();
        let closing = std::thread::spawn(move || {
            finished.send(state.close_session(&id).unwrap()).unwrap();
        });
        assert!(result.recv_timeout(Duration::from_millis(50)).is_err());
        release.send(()).unwrap();
        let stats = result.recv_timeout(Duration::from_secs(3)).unwrap();
        closing.join().unwrap();
        assert_eq!(stats.rx_bytes, 4);
        let final_stats = handle.stats();
        assert_eq!(stats.rx_bytes, final_stats.rx_bytes);
        assert_eq!(stats.tx_bytes, final_stats.tx_bytes);
        assert_eq!(stats.recording, final_stats.recording);
        assert!(!stats.recording.active);
        if fail_capture {
            assert!(
                !stats.recording.errors.is_empty(),
                "{:?}",
                stats.recording.errors.is_empty()
            );
        } else {
            assert!(std::fs::read_to_string(path).unwrap().contains("TAIL"));
        }
    }

    #[test]
    fn concurrent_open_reuses_physical_port_without_changing_config_or_recording() {
        let dir = tempfile::tempdir().unwrap();
        let factory = Arc::new(MockFactory::script(vec![]));
        let mut state = DaemonState::new(dir.path().join("socket"), "token".into());
        state.factory = factory.clone();
        let state = Arc::new(state);
        let config = SerialConfig {
            baud: 9_600,
            label: Some("original".into()),
            record_rx_to: Some(dir.path().join("original.log")),
            ..SerialConfig::new("test-uart")
        };
        let original = state.open_session("GUI".into(), 1, config.clone()).unwrap();
        assert!(!original.reused);
        let start = Arc::new(std::sync::Barrier::new(8));
        let threads: Vec<_> = (0..8)
            .map(|i| {
                let state = state.clone();
                let start = start.clone();
                let unused_log = dir.path().join(format!("unused-{i}.log"));
                std::thread::spawn(move || {
                    start.wait();
                    state
                        .open_session(
                            "AI".into(),
                            2,
                            SerialConfig {
                                record_rx_to: Some(unused_log),
                                ..SerialConfig::new("test-uart")
                            },
                        )
                        .unwrap()
                })
            })
            .collect();
        for thread in threads {
            let result = thread.join().unwrap();
            assert!(result.reused);
            assert_eq!(result.session.session_id, original.session.session_id);
            assert_eq!(result.session.config, config);
            assert_eq!(result.session.owner, "GUI");
        }
        assert_eq!(factory.open_count(), 1);
        assert_eq!(state.session_summaries().len(), 1);
        for i in 0..8 {
            assert!(!dir.path().join(format!("unused-{i}.log")).exists());
        }
        let id = SessionId::parse(&original.session.session_id).unwrap();
        assert_eq!(
            state.sessions.lock().unwrap().get(&id).unwrap().owner_conn,
            1
        );
        assert!(state.close_session(&id).is_some());
        let fresh = state
            .open_session("CLI".into(), 3, SerialConfig::new("test-uart"))
            .unwrap();
        assert!(!fresh.reused);
        assert_ne!(fresh.session.session_id, original.session.session_id);
        assert_eq!(factory.open_count(), 2);
    }

    #[test]
    fn reconnecting_session_is_joinable() {
        let factory = Arc::new(MockFactory::script(vec![]));
        let mut state = DaemonState::new(PathBuf::from("test-socket"), "token".into());
        state.factory = factory.clone();
        let first = state
            .open_session("GUI".into(), 1, SerialConfig::new("test-uart"))
            .unwrap();
        factory.fail_opens(vec![flattencom_core::FlattenError::io("unplugged")]);
        factory.extend_script(vec![ReadStep::Fail(flattencom_core::FlattenError::io(
            "unplugged",
        ))]);
        let handle = state
            .sessions
            .lock()
            .unwrap()
            .values()
            .next()
            .unwrap()
            .handle
            .clone();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !matches!(handle.state(), SessionState::Reconnecting { .. }) {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        let joined = state
            .open_session("AI".into(), 2, SerialConfig::new("test-uart"))
            .unwrap();
        assert!(joined.reused);
        assert_eq!(joined.session.session_id, first.session.session_id);
        handle.close();
        // Closed entries may still be retained for inspection; a new connection is new.
        let _ = state.close_session(&handle.id());
    }

    #[test]
    fn external_busy_error_is_not_converted_to_shared_success() {
        let factory = Arc::new(MockFactory::script(vec![]));
        factory.fail_opens(vec![flattencom_core::FlattenError::PortBusy {
            path: "other-uart".into(),
            hint: "external program".into(),
        }]);
        let mut state = DaemonState::new(PathBuf::from("test-socket"), "token".into());
        state.factory = factory;
        assert!(matches!(
            state.open_session("GUI".into(), 1, SerialConfig::new("other-uart")),
            Err(flattencom_core::FlattenError::PortBusy { .. })
        ));
        assert!(
            state.session_summaries().is_empty(),
            "{:?}",
            state.session_summaries().is_empty()
        );
    }

    #[test]
    fn simultaneous_first_connections_create_only_one_session() {
        let factory = Arc::new(MockFactory::script(vec![]));
        let mut state = DaemonState::new(PathBuf::from("test-socket"), "token".into());
        state.factory = factory.clone();
        let state = Arc::new(state);
        let barrier = Arc::new(std::sync::Barrier::new(4));
        let tasks: Vec<_> = (1..=4)
            .map(|connection| {
                let state = state.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    state
                        .open_session("client".into(), connection, SerialConfig::new("test-uart"))
                        .unwrap()
                })
            })
            .collect();
        let results: Vec<_> = tasks.into_iter().map(|task| task.join().unwrap()).collect();
        assert_eq!(results.iter().filter(|r| !r.reused).count(), 1);
        assert!(
            results
                .iter()
                .all(|r| r.session.session_id == results[0].session.session_id)
        );
        assert_eq!(factory.open_count(), 1);
    }

    #[test]
    fn failed_session_is_retained_but_a_new_connection_is_created() {
        let factory = Arc::new(MockFactory::script(vec![ReadStep::Fail(
            flattencom_core::FlattenError::io("disconnected"),
        )]));
        let mut state = DaemonState::new(PathBuf::from("test-socket"), "token".into());
        state.factory = factory.clone();
        let config = SerialConfig {
            auto_reconnect: flattencom_core::config::ReconnectPolicy::Disabled,
            ..SerialConfig::new("test-uart")
        };
        let first = state.open_session("GUI".into(), 1, config.clone()).unwrap();
        let handle = state
            .sessions
            .lock()
            .unwrap()
            .values()
            .next()
            .unwrap()
            .handle
            .clone();
        assert!(matches!(
            handle.wait_closed(Duration::from_secs(2)),
            SessionState::Failed { .. }
        ));
        let next = state.open_session("GUI".into(), 1, config).unwrap();
        assert!(!next.reused);
        assert_ne!(next.session.session_id, first.session.session_id);
        assert_eq!(factory.open_count(), 2);
        assert_eq!(state.session_summaries().len(), 2);
    }

    #[test]
    #[cfg(unix)]
    fn linux_symlink_paths_have_same_device_key() {
        let dir = tempfile::tempdir().unwrap();
        let device = dir.path().join("tty-device");
        std::fs::write(&device, b"").unwrap();
        let alias = dir.path().join("by-id");
        std::os::unix::fs::symlink(&device, &alias).unwrap();
        assert_eq!(
            port_key(device.to_str().unwrap()),
            port_key(alias.to_str().unwrap())
        );
    }

    #[test]
    #[cfg(windows)]
    fn windows_com_names_have_same_device_key() {
        assert_eq!(port_key("com10"), port_key(r"\\.\COM10"));
    }
}
