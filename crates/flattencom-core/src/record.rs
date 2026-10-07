/*
 * flattencom - Core Record
 *
 * Writes readable, raw and structured captures and supports retention, export and replay input.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

//! Capture recording, TXT/HEX/CSV/JSONL exports and recorded-frame replay.
//!
//! Recorded JSONL lines serialize [`Frame`] with hexadecimal bytes, sharing the schema
//! with RPC/MCP so offline decoders can consume captures directly.

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::FlattenError;
use crate::frame::Frame;

/// Live recording state, shared by GUI/CLI/MCP statistics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
pub struct RecordingStatus {
    /// Raw receive-stream path, if configured.
    pub rx_path: Option<PathBuf>,
    /// Structured RX/TX capture path, if configured.
    pub jsonl_path: Option<PathBuf>,
    /// Human-readable capture path, when a .log or legacy .txt recording is selected.
    #[serde(default)]
    pub readable_path: Option<PathBuf>,
    /// Whether at least one file is currently being recorded.
    pub active: bool,
    /// File errors; a failed sink stops recording and is never silently resumed.
    pub errors: Vec<String>,
}

/// Select readable recording by extension. Legacy explicit JSONL paths remain supported.
pub fn is_readable(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("log") || ext.eq_ignore_ascii_case("txt"))
}

/// Export format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "lowercase")]
pub enum ExportFormat {
    /// Readable text with timestamps and RX/TX direction.
    #[default]
    Txt,
    /// Classic offset hexadecimal dump: 16 bytes per line and ASCII column.
    Hex,
    /// CSV with seq,dir,t_us,mono_us,text,hex columns and a header.
    Csv,
    /// JSONL using the same frame schema as captures.
    Jsonl,
    /// PCAP v2.4, LINKTYPE_USER0: direction byte (0=RX, 1=TX) followed by serial bytes.
    Pcap,
}

impl ExportFormat {
    /// Name.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Txt => "txt",
            Self::Hex => "hex",
            Self::Csv => "csv",
            Self::Jsonl => "jsonl",
            Self::Pcap => "pcap",
        }
    }

    /// Parse an RPC format parameter.
    pub fn parse(s: &str) -> Result<Self, FlattenError> {
        match s {
            "txt" | "text" => Ok(Self::Txt),
            "hex" | "hexdump" => Ok(Self::Hex),
            "csv" => Ok(Self::Csv),
            "jsonl" | "json" => Ok(Self::Jsonl),
            "pcap" => Ok(Self::Pcap),
            other => Err(FlattenError::InvalidConfig {
                field: "format".into(),
                reason: crate::tr!(
                    "Expected txt, hex, csv, jsonl or pcap, got {other:?}",
                    other = other
                ),
            }),
        }
    }

    /// File extension.
    #[must_use]
    pub fn ext(self) -> &'static str {
        match self {
            Self::Txt => "log",
            Self::Hex => "hex",
            Self::Csv => "csv",
            Self::Jsonl => "jsonl",
            Self::Pcap => "pcap",
        }
    }
}

/// Export result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ExportResult {
    /// Output file path.
    pub path: PathBuf,
    /// Number of frames written.
    pub frames: u64,
    /// Output file size in bytes.
    pub bytes: u64,
}

/// Continuous recorder: JSONL retains RX/TX, raw output only RX; flush dirty data every 100 ms.
///
/// Recording receives the complete frame stream before view filtering or eviction,
/// preserving replay data independently of the bounded memory buffer.
#[derive(Debug)]
pub struct Recorder {
    writer: BufWriter<File>,
    leases: VecDeque<(PathBuf, crate::capture_sink::CaptureLease)>,
    path: PathBuf,
    last_flush: Instant,
    segment_bytes: u64,
    max_segment_bytes: u64,
    rx_only: bool,
    transcript: Option<crate::transcript::Transcript>,
    part: u64,
    keep_segments: Option<u32>,
    dirty: bool,
    /// Cumulative frame-write count.
    pub frames: u64,
    /// Cumulative output bytes.
    pub bytes: u64,
}

impl Recorder {
    /// Open an append-mode capture file.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, FlattenError> {
        Self::open_mode(path.as_ref(), false)
    }

    /// Create a new raw receive log. Never append unrelated sessions or overwrite an existing file.
    pub fn open_rx(path: impl AsRef<Path>) -> Result<Self, FlattenError> {
        Self::open_mode(path.as_ref(), true)
    }

    /// Create an independent readable capture sink with the same rotation policy.
    pub fn open_readable(path: impl AsRef<Path>) -> Result<Self, FlattenError> {
        let mut recorder = Self::open_mode(path.as_ref(), true)?;
        recorder.rx_only = false;
        recorder.transcript = Some(crate::transcript::Transcript::default());
        let header = format!(
            "\n{}\n\n",
            crate::i18n::text(
                "flattencom capture\nTimes: UTC host receive/write time. RX lines are joined across chunks.\nTX records confirm transmission, not device execution. Binary and control bytes are escaped."
            )
        );
        recorder.write_bytes(header.as_bytes())?;
        Ok(recorder)
    }

    fn open_mode(path: &Path, rx_only: bool) -> Result<Self, FlattenError> {
        if let Some(dir) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).map_err(|e| {
                FlattenError::io(crate::tr!("Failed to create capture directory: {e}", e = e))
            })?;
        }
        let (mut file, lease) = crate::capture_sink::CaptureLease::open(path, rx_only)?;
        let mut segment_bytes = file
            .metadata()
            .map_err(|e| FlattenError::io(e.to_string()))?
            .len();
        let mut boundary_bytes = 0;
        if !rx_only && segment_bytes > 0 {
            file.seek(SeekFrom::End(-1))
                .map_err(|e| FlattenError::io(e.to_string()))?;
            let mut last = [0];
            file.read_exact(&mut last)
                .map_err(|e| FlattenError::io(e.to_string()))?;
            if last[0] != b'\n' {
                // Preserve both complete records without LF and interrupted tails.
                // Append mode writes at EOF despite the read's shared file offset.
                file.write_all(b"\n")
                    .map_err(|e| FlattenError::io(e.to_string()))?;
                boundary_bytes = 1;
                segment_bytes += boundary_bytes;
            }
        }
        Ok(Self {
            writer: BufWriter::new(file),
            leases: VecDeque::from([(path.to_owned(), lease)]),
            path: path.to_owned(),
            last_flush: Instant::now(),
            segment_bytes,
            max_segment_bytes: 64 * 1024 * 1024,
            rx_only,
            transcript: None,
            part: 0,
            keep_segments: None,
            dirty: false,
            frames: 0,
            bytes: boundary_bytes,
        })
    }

    /// Recording file path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
    /// Number of older readable segments to retain in addition to the current segment.
    /// By default readable captures never delete older segments.
    pub fn set_retention(&mut self, keep_segments: Option<u32>) {
        self.keep_segments = keep_segments;
    }

    /// Write a frame and return output bytes; flush every 100 ms and skip TX in RX-only mode.
    pub fn write(&mut self, frame: &Frame) -> Result<u64, FlattenError> {
        if let Some(transcript) = &mut self.transcript {
            let text = transcript.frame(frame);
            self.frames += 1;
            return self.write_bytes(text.as_bytes());
        }
        if self.rx_only && frame.dir != crate::frame::Direction::Rx {
            return Ok(0);
        }
        let line;
        let bytes = if self.rx_only {
            frame.data.as_slice()
        } else {
            line = serde_json::to_string(frame).map_err(|e| {
                FlattenError::internal(crate::tr!("Failed to serialize frame: {e}", e = e))
            })? + "\n";
            line.as_bytes()
        };
        let bytes = bytes.to_vec();
        self.frames += 1;
        self.write_bytes(&bytes)
    }

    /// Write an event into a readable transcript. Does nothing for other sink types.
    pub fn annotation(
        &mut self,
        t_us: i64,
        kind: &str,
        label: &str,
        source: &str,
        seq: Option<u64>,
    ) -> Result<(), FlattenError> {
        if let Some(transcript) = &mut self.transcript {
            let text = transcript.annotation(t_us, kind, label, source, seq);
            self.write_bytes(text.as_bytes())?;
            self.flush()?;
        }
        Ok(())
    }

    /// Finish only RX reassembly at a connection boundary; raw/JSONL keep their bytes.
    pub(crate) fn rx_boundary(&mut self) -> Result<(), FlattenError> {
        if let Some(transcript) = &mut self.transcript {
            let text = transcript.rx_boundary();
            self.write_bytes(text.as_bytes())?;
        }
        Ok(())
    }

    /// Finalize a transcript and flush all pending output.
    pub fn finish(&mut self) -> Result<(), FlattenError> {
        if let Some(transcript) = &mut self.transcript {
            let text = transcript.finish();
            self.write_bytes(text.as_bytes())?;
        }
        self.flush()
    }

    fn write_bytes(&mut self, bytes: &[u8]) -> Result<u64, FlattenError> {
        if bytes.is_empty() {
            return Ok(0);
        }
        if self.segment_bytes > 0
            && self.segment_bytes + bytes.len() as u64 > self.max_segment_bytes
        {
            self.rotate()?;
        }
        self.writer
            .write_all(bytes)
            .map_err(|e| FlattenError::io(crate::tr!("Failed to write capture: {e}", e = e)))?;
        self.bytes += bytes.len() as u64;
        self.segment_bytes += bytes.len() as u64;
        self.dirty = true;
        self.flush_due()?;
        Ok(bytes.len() as u64)
    }

    /// Flush sparse traffic as well as sustained capture, without a write on every idle poll.
    pub fn flush_due(&mut self) -> Result<(), FlattenError> {
        if self.dirty && self.last_flush.elapsed().as_millis() >= 100 {
            self.flush()?;
        }
        Ok(())
    }

    /// Flush pending output.
    pub fn flush(&mut self) -> Result<(), FlattenError> {
        self.writer
            .flush()
            .map_err(|e| FlattenError::io(crate::tr!("Failed to flush capture: {e}", e = e)))?;
        self.dirty = false;
        self.last_flush = Instant::now();
        Ok(())
    }

    fn rotate(&mut self) -> Result<(), FlattenError> {
        self.flush()?;
        if self.transcript.is_some() {
            let extension = self
                .path
                .extension()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            // Stable segment names: never truncate, rename or delete a segment
            // that a reader may be inspecting. Keep all text history by default.
            loop {
                self.part += 1;
                let stem = self.path.file_stem().unwrap_or_default().to_string_lossy();
                let path = self
                    .path
                    .with_file_name(format!("{stem}-part-{:06}.{extension}", self.part));
                if path.exists() {
                    continue;
                }
                match crate::capture_sink::CaptureLease::open(&path, true) {
                    Ok((file, lease)) => {
                        self.leases.back().expect("current segment").1.complete()?;
                        self.writer = BufWriter::new(file);
                        self.leases.push_back((path, lease));
                        break;
                    }
                    Err(error) => return Err(error),
                }
            }
            self.segment_bytes = 0;
            if let Some(keep) = self.keep_segments {
                // Numeric gaps belong to other captures. Retain and expire only
                // paths that this recorder actually created, including the base.
                while self.leases.len() as u64 > u64::from(keep) + 1 {
                    let (path, lease) = self.leases.front().expect("expired segment");
                    // A path may have been replaced since this recorder created
                    // it. Never expire the replacement as though it were ours.
                    if lease.matches(path)? {
                        match std::fs::remove_file(path) {
                            Ok(()) => {}
                            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                            Err(error) => return Err(FlattenError::io(error.to_string())),
                        }
                    }
                    self.leases.pop_front();
                }
            }
            return Ok(());
        }
        // Preserve legacy suffixes, but publish each copy through the same active
        // capture checks and atomic replacement used by exports. A failed copy
        // must never truncate a destination or the current capture.
        for index in (1..=4).rev() {
            let destination = self.segment_path(index);
            if index == 1 {
                // Read the exact file that will be truncated, not its pathname:
                // external renames and retargeted symlinks must not replace the
                // backup source. The shared offset is safe after flush, and
                // subsequent writes use append mode regardless of this seek.
                let mut input = self.writer.get_ref().try_clone().map_err(io_e)?;
                input.rewind().map_err(io_e)?;
                crate::capture_sink::atomic_output(&destination, |output| {
                    std::io::copy(&mut input, output).map_err(io_e)?;
                    Ok(())
                })?;
            } else {
                let previous = self.segment_path(index - 1);
                if previous.exists() {
                    copy_segment(&previous, &destination)?;
                }
            }
        }
        self.truncate_live()?;
        self.segment_bytes = 0;
        Ok(())
    }

    /// Truncate the live segment after its history was copied to the backup.
    fn truncate_live(&self) -> Result<(), FlattenError> {
        #[cfg(not(windows))]
        {
            self.writer.get_ref().set_len(0).map_err(io_e)
        }
        #[cfg(windows)]
        {
            truncate_handle(self.writer.get_ref())
        }
    }

    fn segment_path(&self, index: usize) -> PathBuf {
        let mut path = self.path.as_os_str().to_owned();
        path.push(format!(".{index}"));
        PathBuf::from(path)
    }
}

fn copy_segment(source: &Path, destination: &Path) -> Result<(), FlattenError> {
    crate::capture_sink::atomic_output(destination, |output| {
        let mut input = File::open(source).map_err(|e| FlattenError::io(e.to_string()))?;
        std::io::copy(&mut input, output).map_err(|e| FlattenError::io(e.to_string()))?;
        Ok(())
    })
}

/// Truncate the segment that a live append handle refers to.
///
/// Windows grants an append handle `FILE_APPEND_DATA` but not the `GENERIC_WRITE`
/// that resizing needs, so `set_len` on it is rejected with access denied.
/// `ReOpenFile` reopens the same file object for writing, so an external rename
/// or replacement at the recorder's pathname is never truncated by mistake.
#[cfg(windows)]
#[allow(unsafe_code)]
fn truncate_handle(file: &File) -> Result<(), FlattenError> {
    use std::os::windows::io::{AsRawHandle, FromRawHandle};
    use windows_sys::Win32::Foundation::{GENERIC_WRITE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, ReOpenFile,
    };
    // SAFETY: the source handle is owned by the recorder and outlives this call;
    // ReOpenFile only derives a second handle to the same file object, and the
    // returned handle is immediately taken ownership of by `File`.
    let handle = unsafe {
        ReOpenFile(
            file.as_raw_handle().cast(),
            GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            // File attributes are only accepted at creation time; passing one here
            // fails with ERROR_INVALID_PARAMETER.
            0,
        )
    };
    if handle == INVALID_HANDLE_VALUE || handle.is_null() {
        return Err(FlattenError::io(
            std::io::Error::last_os_error().to_string(),
        ));
    }
    let resized = unsafe { File::from_raw_handle(handle.cast()) };
    resized.set_len(0).map_err(io_e)
}

/// Export a frame snapshot to a file.
#[allow(clippy::missing_panics_doc)] // Internal string serialization is infallible.
pub fn export(
    frames: &[Frame],
    fmt: ExportFormat,
    path: impl AsRef<Path>,
) -> Result<ExportResult, FlattenError> {
    if let Some(dir) = path.as_ref().parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| {
            FlattenError::io(crate::tr!("Failed to create export directory: {e}", e = e))
        })?;
    }
    crate::capture_sink::atomic_output(path.as_ref(), |file| {
        let mut w = BufWriter::new(file);
        let written: u64 = match fmt {
            ExportFormat::Txt => {
                writeln!(w, "{}\n",crate::i18n::text("flattencom capture\nTimes: UTC host receive/write time. TX does not confirm device execution.")).map_err(io_e)?;
                let mut transcript = crate::transcript::Transcript::default();
                let mut n = 0u64;
                for f in frames {
                    w.write_all(transcript.frame(f).as_bytes()).map_err(io_e)?;
                    n += 1;
                }
                w.write_all(transcript.finish().as_bytes()).map_err(io_e)?;
                n
            }
            ExportFormat::Hex => {
                let mut n = 0u64;
                for f in frames {
                    write!(w, "seq={} {} @{} ", f.seq, f.dir.as_char(), f.mono_us).map_err(io_e)?;
                    write_hexdump_line(&mut w, &f.data)?;
                    n += 1;
                }
                n
            }
            ExportFormat::Csv => {
                let mut n = 0u64;
                writeln!(w, "seq,dir,t_us,mono_us,text,hex").map_err(io_e)?;
                for f in frames {
                    let text = csv_escape(&f.text_lossy());
                    let hexs = f.hex_string(true);
                    writeln!(
                        w,
                        "{},{},{},{},{},{}",
                        f.seq,
                        f.dir.as_str(),
                        f.t_us,
                        f.mono_us,
                        text,
                        hexs
                    )
                    .map_err(io_e)?;
                    n += 1;
                }
                n
            }
            ExportFormat::Jsonl => {
                let mut n = 0u64;
                for f in frames {
                    let line = serde_json::to_string(f).map_err(|e| {
                        FlattenError::internal(crate::tr!("Failed to serialize frame: {e}", e = e))
                    })?;
                    writeln!(w, "{line}").map_err(io_e)?;
                    n += 1;
                }
                n
            }
            ExportFormat::Pcap => {
                // Classic little-endian PCAP; user-defined link type 147 (USER0).
                w.write_all(&0xa1b2_c3d4_u32.to_le_bytes()).map_err(io_e)?;
                w.write_all(&2u16.to_le_bytes()).map_err(io_e)?;
                w.write_all(&4u16.to_le_bytes()).map_err(io_e)?;
                for value in [0u32, 0, 16 * 1024 * 1024 + 1, 147] {
                    w.write_all(&value.to_le_bytes()).map_err(io_e)?;
                }
                for frame in frames {
                    let timestamp =
                        u64::try_from(frame.t_us).map_err(|_| FlattenError::InvalidConfig {
                            field: "t_us".into(),
                            reason: "PCAP requires nonnegative epoch timestamps".into(),
                        })?;
                    let seconds = u32::try_from(timestamp / 1_000_000)
                        .map_err(|_| FlattenError::io("PCAP timestamp exceeds u32 seconds"))?;
                    let length = u32::try_from(frame.len() + 1)
                        .map_err(|_| FlattenError::io("PCAP frame too large"))?;
                    for value in [seconds, (timestamp % 1_000_000) as u32, length, length] {
                        w.write_all(&value.to_le_bytes()).map_err(io_e)?;
                    }
                    w.write_all(&[u8::from(frame.dir == crate::frame::Direction::Tx)])
                        .map_err(io_e)?;
                    w.write_all(&frame.data).map_err(io_e)?;
                }
                frames.len() as u64
            }
        };
        w.flush().map_err(io_e)?;
        let bytes = w.get_ref().metadata().map_err(io_e)?.len();
        Ok(ExportResult {
            path: path.as_ref().to_owned(),
            frames: written,
            bytes,
        })
    })
}

fn io_e(e: std::io::Error) -> FlattenError {
    FlattenError::io(crate::tr!("Failed to write export: {e}", e = e))
}

fn write_hexdump_line(w: &mut impl std::io::Write, data: &[u8]) -> Result<(), FlattenError> {
    for (i, chunk) in data.chunks(16).enumerate() {
        let mut hexs = String::new();
        let mut ascii = String::new();
        for &b in chunk {
            write!(hexs, "{b:02X} ").expect("String write");
            ascii.push(if (0x20..0x7F).contains(&b) {
                char::from_u32(u32::from(b)).unwrap_or('.')
            } else {
                '.'
            });
        }
        for _ in chunk.len()..16 {
            hexs.push_str("   ");
        }
        writeln!(w, "{:08X}  {} {}", (i * 16) as u64, hexs, ascii).map_err(io_e)?;
    }
    Ok(())
}

fn csv_escape(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') || s.contains('\r') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_owned()
    }
}

/// Maximum JSONL record size, including LF when present, matching daemon replay.
pub const MAX_LOG_LINE_BYTES: usize = 16 * 1024 * 1024;

/// Lazy JSONL capture reader. Malformed, invalid UTF-8 and oversized records are
/// counted and skipped; IO errors are yielded once and terminate iteration.
/// Memory use is bounded by one record plus the underlying reader's buffer.
pub struct LogReader<R: BufRead> {
    reader: R,
    line: Vec<u8>,
    skipped: u64,
    done: bool,
}

impl<R: BufRead> LogReader<R> {
    /// Wrap a buffered stream without reading any bytes.
    pub fn new(reader: R) -> Self {
        Self {
            reader,
            line: Vec::new(),
            skipped: 0,
            done: false,
        }
    }

    /// Invalid records encountered so far; the final count is available after EOF.
    pub fn skipped(&self) -> u64 {
        self.skipped
    }

    fn read_record(&mut self) -> std::io::Result<Option<bool>> {
        self.line.clear();
        let mut oversized = false;
        loop {
            let buffer = self.reader.fill_buf()?;
            if buffer.is_empty() {
                self.done = true;
                return Ok((oversized || !self.line.is_empty()).then_some(oversized));
            }
            let newline = buffer.iter().position(|&byte| byte == b'\n');
            let consumed = newline.map_or(buffer.len(), |index| index + 1);
            if !oversized {
                if consumed > MAX_LOG_LINE_BYTES - self.line.len() {
                    oversized = true;
                    self.line.clear();
                } else {
                    self.line.extend_from_slice(&buffer[..consumed]);
                }
            }
            self.reader.consume(consumed);
            if newline.is_some() {
                return Ok(Some(oversized));
            }
        }
    }
}

impl<R: BufRead> Iterator for LogReader<R> {
    type Item = Result<Frame, FlattenError>;

    fn next(&mut self) -> Option<Self::Item> {
        while !self.done {
            match self.read_record() {
                Ok(None) => return None,
                Ok(Some(true)) => {
                    self.skipped += 1;
                    continue;
                }
                Ok(Some(false)) => {}
                Err(error) => {
                    self.done = true;
                    return Some(Err(FlattenError::io(crate::tr!(
                        "Failed to read capture file: {e}",
                        e = error
                    ))));
                }
            }
            // Validate the entire record, including fields serde would ignore.
            let line = match std::str::from_utf8(&self.line) {
                Ok(line) => line,
                Err(_) => {
                    self.skipped += 1;
                    continue;
                }
            };
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<Frame>(line) {
                Ok(frame) => return Some(Ok(frame)),
                Err(_) => self.skipped += 1,
            }
        }
        None
    }
}

/// Open a capture for bounded, lazy iteration without loading a full snapshot.
pub fn stream_log(path: impl AsRef<Path>) -> Result<LogReader<BufReader<File>>, FlattenError> {
    let file = File::open(path.as_ref())
        .map_err(|e| FlattenError::io(crate::tr!("Failed to read capture file: {e}", e = e)))?;
    Ok(LogReader::new(BufReader::new(file)))
}

/// Collect a complete capture snapshot, counting and skipping malformed or
/// oversized lines. Use [`stream_log`] when total capture size is unbounded.
pub fn read_log(path: impl AsRef<Path>) -> Result<(Vec<Frame>, u64), FlattenError> {
    let mut reader = stream_log(path)?;
    let frames = reader.by_ref().collect::<Result<Vec<_>, _>>()?;
    Ok((frames, reader.skipped()))
}

/// Replay delay: adjacent monotonic timestamp difference divided by a finite,
/// positive speed. Each wait is capped at 60 seconds, including when division
/// by an extremely small speed overflows. Reversed timestamps produce no wait.
#[must_use]
pub fn replay_delay(prev_mono_us: u64, next_mono_us: u64, speed: f64) -> std::time::Duration {
    let delta_us = next_mono_us.saturating_sub(prev_mono_us) as f64 / speed;
    std::time::Duration::from_secs_f64(delta_us.min(60_000_000.0) / 1_000_000.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::Direction;
    use std::fs::OpenOptions;
    use std::sync::Arc;

    fn fx(seq: u64, dir: Direction, data: &[u8]) -> Frame {
        Frame::new(seq, dir, data.to_vec(), 1_790_000_000_000_000, seq * 1_000)
    }

    #[test]
    fn streaming_log_rejects_invalid_utf8_in_unknown_fields_and_recovers() {
        let rejected = fx(1, Direction::Rx, b"invalid unknown field");
        let valid = fx(2, Direction::Rx, b"valid");
        let mut wire = serde_json::to_vec(&rejected).unwrap();
        assert_eq!(wire.pop(), Some(b'}'));
        wire.extend_from_slice(b",\"extra\":\"\xff\"}\n");
        wire.extend_from_slice(&serde_json::to_vec(&valid).unwrap());
        let mut reader = LogReader::new(wire.as_slice());
        assert_eq!(reader.next().unwrap().unwrap(), valid);
        assert_eq!(reader.skipped(), 1);
        assert!(reader.next().is_none());
    }

    #[test]
    fn streaming_log_bounds_lines_and_recovers_after_oversized_records() {
        let frame = fx(1, Direction::Rx, b"valid");
        let encoded = serde_json::to_vec(&frame).unwrap();
        let mut wire = encoded.clone();
        wire.resize(MAX_LOG_LINE_BYTES - 1, b' ');
        wire.push(b'\n'); // Exactly the limit, including LF, is accepted.
        wire.extend_from_slice(&encoded);
        wire.resize(2 * MAX_LOG_LINE_BYTES, b' ');
        wire.push(b'\n'); // Valid JSON plus whitespace, one byte over the limit.
        wire.extend_from_slice(b"\xff\n\n{bad}\n");
        wire.extend_from_slice(&encoded); // A final record needs no LF.
        let mut reader = LogReader::new(BufReader::with_capacity(17, wire.as_slice()));
        assert_eq!(reader.next().unwrap().unwrap(), frame);
        assert_eq!(reader.skipped(), 0);
        assert_eq!(reader.next().unwrap().unwrap(), frame);
        assert_eq!(reader.skipped(), 3);
        assert!(reader.next().is_none());
        assert!(reader.line.capacity() <= MAX_LOG_LINE_BYTES * 2);

        let oversized = vec![b'x'; MAX_LOG_LINE_BYTES + 1];
        let mut reader = LogReader::new(oversized.as_slice());
        assert!(reader.next().is_none());
        assert_eq!(reader.skipped(), 1);
    }

    #[test]
    fn streaming_log_is_lazy_and_propagates_io_errors_once() {
        struct FailingTail {
            first: std::io::Cursor<Vec<u8>>,
        }
        impl std::io::Read for FailingTail {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                unreachable!("the iterator uses buffered reads")
            }
        }
        impl BufRead for FailingTail {
            fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
                if self.first.position() == self.first.get_ref().len() as u64 {
                    Err(std::io::Error::other("injected tail error"))
                } else {
                    self.first.fill_buf()
                }
            }
            fn consume(&mut self, amount: usize) {
                self.first.consume(amount);
            }
        }
        let frame = fx(1, Direction::Rx, b"first");
        let mut wire = serde_json::to_vec(&frame).unwrap();
        wire.push(b'\n');
        let mut reader = LogReader::new(FailingTail {
            first: std::io::Cursor::new(wire),
        });
        assert_eq!(reader.reader.first.position(), 0);
        assert_eq!(reader.next().unwrap().unwrap(), frame);
        assert!(
            reader
                .next()
                .unwrap()
                .unwrap_err()
                .to_string()
                .contains("injected tail error")
        );
        assert_eq!(reader.skipped(), 0);
        assert!(reader.next().is_none());
    }

    #[test]
    fn streaming_file_does_not_read_or_parse_before_iteration() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        let mut reader = stream_log(file.path()).unwrap();
        let frame = fx(7, Direction::Tx, b"appended after open");
        writeln!(file, "{}", serde_json::to_string(&frame).unwrap()).unwrap();
        assert_eq!(reader.next().unwrap().unwrap(), frame);
        assert!(reader.next().is_none());
    }

    #[test]
    fn 录制回读往返() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut r = Recorder::open(&path).unwrap();
        let frames = vec![
            fx(0, Direction::Tx, b"ATZ\r"),
            fx(1, Direction::Rx, b"OK\r\n"),
        ];
        for f in &frames {
            r.write(f).unwrap();
        }
        r.flush().unwrap();
        assert_eq!(r.frames, 2);
        let (back, skipped) = read_log(&path).unwrap();
        assert_eq!(skipped, 0);
        assert_eq!(back, frames);
    }

    #[test]
    fn jsonl_append_repairs_only_missing_record_boundaries() {
        let directory = tempfile::tempdir().unwrap();
        let first = fx(0, Direction::Rx, b"first");
        let next = fx(1, Direction::Rx, b"next");
        let encoded = serde_json::to_vec(&first).unwrap();
        for (name, original, repair, valid_first, skipped) in [
            ("empty", Vec::new(), false, false, 0),
            (
                "terminated",
                [encoded.as_slice(), b"\n"].concat(),
                false,
                true,
                0,
            ),
            ("complete", encoded.clone(), true, true, 0),
            ("partial", b"{\"seq\":".to_vec(), true, false, 1),
        ] {
            let path = directory.path().join(format!("{name}.jsonl"));
            std::fs::write(&path, &original).unwrap();
            let mut recorder = Recorder::open(&path).unwrap();
            assert_eq!(recorder.frames, 0);
            assert_eq!(recorder.bytes, u64::from(repair));
            assert_eq!(
                recorder.segment_bytes,
                original.len() as u64 + u64::from(repair)
            );
            let written = recorder.write(&next).unwrap();
            recorder.finish().unwrap();
            assert_eq!(recorder.frames, 1);
            assert_eq!(recorder.bytes, written + u64::from(repair));
            let actual = std::fs::read(&path).unwrap();
            assert!(actual.starts_with(&original));
            assert_eq!(recorder.segment_bytes, actual.len() as u64);
            let (frames, bad) = read_log(&path).unwrap();
            assert_eq!(bad, skipped);
            assert_eq!(
                frames,
                if valid_first {
                    vec![first.clone(), next.clone()]
                } else {
                    vec![next.clone()]
                }
            );
        }
    }

    #[test]
    fn repaired_jsonl_boundary_is_counted_in_rotation_threshold() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("capture.jsonl");
        let first = fx(0, Direction::Rx, b"first");
        let next = fx(1, Direction::Rx, b"next");
        let original = serde_json::to_vec(&first).unwrap();
        std::fs::write(&path, &original).unwrap();
        let mut recorder = Recorder::open(&path).unwrap();
        let next_bytes = serde_json::to_vec(&next).unwrap().len() as u64 + 1;
        recorder.max_segment_bytes = original.len() as u64 + next_bytes;
        recorder.write(&next).unwrap();
        recorder.finish().unwrap();
        assert_eq!(recorder.bytes, next_bytes + 1);
        assert_eq!(recorder.segment_bytes, next_bytes);
        assert_eq!(read_log(&path).unwrap(), (vec![next], 0));
        assert_eq!(
            read_log(recorder.segment_path(1)).unwrap(),
            (vec![first], 0)
        );
    }

    #[test]
    fn 坏行容错() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.jsonl");
        let mut r = Recorder::open(&path).unwrap();
        r.write(&fx(0, Direction::Rx, b"ok")).unwrap();
        r.flush().unwrap();
        drop(r);
        // Append an invalid line.
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(f, "{{broken json").unwrap();
        drop(f);
        let (frames, skipped) = read_log(&path).unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(skipped, 1);
    }

    #[test]
    fn 导出四种格式() {
        let dir = tempfile::tempdir().unwrap();
        let frames = vec![
            fx(0, Direction::Tx, b"AT+CSQ\r\n"),
            fx(1, Direction::Rx, &[0x01, 0x03, 0x02, 0x00, 0x1E]),
        ];
        for fmt in [
            ExportFormat::Txt,
            ExportFormat::Hex,
            ExportFormat::Csv,
            ExportFormat::Jsonl,
        ] {
            let p = dir.path().join(format!("out.{}", fmt.ext()));
            let r = export(&frames, fmt, &p).unwrap();
            assert_eq!(r.frames, 2);
            assert!(p.exists());
            let s = std::fs::read_to_string(&p).unwrap();
            if fmt == ExportFormat::Csv {
                assert!(s.starts_with("seq,dir,"));
                // Quoted CSV text may contain newlines, so do not assert the physical line count.
                assert!(s.contains("41 54 2B 43")); // Hexadecimal bytes for "AT+CSQ".
            }
            if fmt == ExportFormat::Hex {
                assert!(s.contains("01 03 02 00 1E"));
            }
        }
    }

    #[test]
    fn 回放延迟按倍速() {
        assert_eq!(
            replay_delay(0, 1_000_000, 1.0),
            std::time::Duration::from_millis(1_000)
        );
        assert_eq!(
            replay_delay(0, 1_000_000, 10.0),
            std::time::Duration::from_millis(100)
        );
    }

    #[test]
    fn replay_delay_honors_slow_speeds_and_bounds_extreme_intervals() {
        use std::time::Duration;

        assert_eq!(replay_delay(0, 1_000, 0.001), Duration::from_secs(1));
        assert_eq!(replay_delay(0, 1, 0.000_001), Duration::from_secs(1));
        assert_eq!(replay_delay(0, 61_000_000, 1.0), Duration::from_secs(60));
        for speed in [f64::from_bits(1), f64::MIN_POSITIVE] {
            assert_eq!(replay_delay(0, u64::MAX, speed), Duration::from_secs(60));
            assert_eq!(replay_delay(1, 1, speed), Duration::ZERO);
            assert_eq!(replay_delay(2, 1, speed), Duration::ZERO);
        }
        assert_eq!(replay_delay(0, 1, f64::MAX), Duration::ZERO);
    }

    #[test]
    fn csv_转义() {
        assert_eq!(csv_escape("plain"), "plain");
        assert_eq!(csv_escape("a,b"), "\"a,b\"");
        assert_eq!(csv_escape("say \"hi\""), "\"say \"\"hi\"\"\"");
    }

    #[test]
    fn 导出格式解析() {
        assert_eq!(ExportFormat::parse("hex").unwrap(), ExportFormat::Hex);
        assert_eq!(ExportFormat::parse("pcap").unwrap(), ExportFormat::Pcap);
    }

    #[test]
    fn pcap_direction_timestamp_and_payload() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("capture.pcap");
        let frames = [fx(0, Direction::Tx, b"AT"), fx(1, Direction::Rx, b"OK")];
        export(&frames, ExportFormat::Pcap, &path).unwrap();
        let bytes = std::fs::read(path).unwrap();
        assert_eq!(&bytes[..4], &[0xd4, 0xc3, 0xb2, 0xa1]);
        assert_eq!(u32::from_le_bytes(bytes[20..24].try_into().unwrap()), 147);
        assert_eq!(u32::from_le_bytes(bytes[32..36].try_into().unwrap()), 3);
        assert_eq!(&bytes[40..43], b"\x01AT");
        assert_eq!(&bytes[59..62], b"\x00OK");
    }

    #[test]
    fn rotation_keeps_complete_records_and_bounded_segments() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("capture.jsonl");
        let mut recorder = Recorder::open(&path).unwrap();
        recorder.max_segment_bytes = 1;
        for seq in 0..7 {
            recorder.write(&fx(seq, Direction::Rx, b"data")).unwrap();
        }
        recorder.flush().unwrap();
        assert_eq!(read_log(&path).unwrap().0[0].seq, 6);
        for (index, seq) in [(1, 5), (2, 4), (3, 3), (4, 2)] {
            let (frames, skipped) =
                read_log(path.with_extension(format!("jsonl.{index}"))).unwrap();
            assert_eq!(skipped, 0);
            assert_eq!(frames[0].seq, seq);
        }
    }

    #[test]
    fn rotation_preserves_active_numbered_capture_and_its_tail() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("capture.jsonl");
        let numbered = directory.path().join("capture.jsonl.1");
        let mut active = Recorder::open(&numbered).unwrap();
        active.write(&fx(10, Direction::Rx, b"active")).unwrap();
        active.flush().unwrap();
        let original = std::fs::read(&numbered).unwrap();
        let mut recorder = Recorder::open(&path).unwrap();
        recorder.max_segment_bytes = 1;
        recorder.write(&fx(0, Direction::Rx, b"first")).unwrap();
        assert!(recorder.write(&fx(1, Direction::Rx, b"rotate")).is_err());
        assert_eq!(std::fs::read(&numbered).unwrap(), original);
        active.write(&fx(11, Direction::Rx, b"tail")).unwrap();
        active.flush().unwrap();
        assert_eq!(read_log(&numbered).unwrap().0.len(), 2);
        assert_eq!(read_log(&path).unwrap().0[0].seq, 0);
    }

    #[cfg(unix)]
    #[test]
    fn rotation_copies_open_writer_after_base_symlink_is_retargeted() {
        let directory = tempfile::tempdir().unwrap();
        let original = directory.path().join("original.jsonl");
        let unrelated = directory.path().join("unrelated.jsonl");
        let alias = directory.path().join("capture.jsonl");
        std::fs::write(&original, b"").unwrap();
        std::fs::write(&unrelated, b"UNRELATED").unwrap();
        std::os::unix::fs::symlink(&original, &alias).unwrap();
        let mut recorder = Recorder::open(&alias).unwrap();
        recorder.max_segment_bytes = 1;
        recorder.write(&fx(0, Direction::Rx, b"original")).unwrap();
        recorder.flush().unwrap();
        let before = std::fs::read(&original).unwrap();
        std::fs::remove_file(&alias).unwrap();
        std::os::unix::fs::symlink(&unrelated, &alias).unwrap();
        recorder.write(&fx(1, Direction::Rx, b"next")).unwrap();
        recorder.flush().unwrap();
        assert_eq!(std::fs::read(recorder.segment_path(1)).unwrap(), before);
        assert_eq!(
            read_log(&original).unwrap().0,
            [fx(1, Direction::Rx, b"next")]
        );
        assert_eq!(std::fs::read(&unrelated).unwrap(), b"UNRELATED");
    }

    #[test]
    fn rotation_copies_open_writer_after_base_is_replaced() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("capture.bin");
        let moved = directory.path().join("moved.bin");
        let mut recorder = Recorder::open_rx(&path).unwrap();
        recorder.max_segment_bytes = 1;
        recorder.write(&fx(0, Direction::Rx, b"original")).unwrap();
        recorder.flush().unwrap();
        std::fs::rename(&path, &moved).unwrap();
        std::fs::write(&path, b"UNRELATED").unwrap();
        recorder.write(&fx(1, Direction::Rx, b"next")).unwrap();
        recorder.flush().unwrap();
        assert_eq!(
            std::fs::read(recorder.segment_path(1)).unwrap(),
            b"original"
        );
        assert_eq!(std::fs::read(&moved).unwrap(), b"next");
        assert_eq!(std::fs::read(&path).unwrap(), b"UNRELATED");
        // A second rotation must rewind the shared reader offset again.
        recorder.write(&fx(2, Direction::Rx, b"last")).unwrap();
        recorder.flush().unwrap();
        assert_eq!(std::fs::read(recorder.segment_path(1)).unwrap(), b"next");
        assert_eq!(
            std::fs::read(recorder.segment_path(2)).unwrap(),
            b"original"
        );
        assert_eq!(std::fs::read(&moved).unwrap(), b"last");
        assert_eq!(std::fs::read(&path).unwrap(), b"UNRELATED");
    }

    #[test]
    fn readable_retention_preserves_replacements_at_owned_paths() {
        for replace_numbered in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let base = directory.path().join("capture.log");
            let moved = directory.path().join("moved.log");
            let mut recorder = Recorder::open_readable(&base).unwrap();
            recorder.max_segment_bytes = 1;
            recorder.set_retention(Some(0));
            let replaced = if replace_numbered {
                recorder.write(&fx(0, Direction::Rx, b"line\n")).unwrap();
                recorder.flush().unwrap();
                directory.path().join("capture-part-000001.log")
            } else {
                recorder.flush().unwrap();
                base
            };
            std::fs::rename(&replaced, &moved).unwrap();
            let original = std::fs::read(&moved).unwrap();
            std::fs::write(&replaced, b"UNRELATED").unwrap();
            recorder.write(&fx(1, Direction::Rx, b"next\n")).unwrap();
            recorder.finish().unwrap();
            assert_eq!(std::fs::read(&replaced).unwrap(), b"UNRELATED");
            assert_eq!(std::fs::read(&moved).unwrap(), original);
        }
    }

    #[test]
    fn readable_retention_counts_owned_segments_across_numbering_gaps() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("capture.log");
        let unrelated = directory.path().join("capture-part-000001.log");
        std::fs::write(&unrelated, b"unrelated").unwrap();
        let mut recorder = Recorder::open_readable(&path).unwrap();
        recorder.max_segment_bytes = 1;
        recorder.set_retention(Some(1));
        for seq in 0..4 {
            recorder.write(&fx(seq, Direction::Rx, b"line\n")).unwrap();
        }
        recorder.finish().unwrap();
        assert_eq!(std::fs::read(&unrelated).unwrap(), b"unrelated");
        assert!(!path.exists());
        assert!(!directory.path().join("capture-part-000003.log").exists());
        assert!(directory.path().join("capture-part-000004.log").exists());
        assert!(directory.path().join("capture-part-000005.log").exists());
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 3);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn readable_rotations_keep_descriptors_bounded_and_archives_protected() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("capture.log");
        let mut recorder = Recorder::open_readable(&path).unwrap();
        recorder.max_segment_bytes = 1;
        let open_segment_handles = || {
            std::fs::read_dir("/proc/self/fd")
                .unwrap()
                .filter_map(Result::ok)
                .filter_map(|entry| std::fs::read_link(entry.path()).ok())
                .filter(|target| target.starts_with(directory.path()))
                .count()
        };
        assert_eq!(open_segment_handles(), 2);
        for seq in 0..128 {
            recorder.write(&fx(seq, Direction::Rx, b"line\n")).unwrap();
            assert_eq!(open_segment_handles(), 2, "rotation {seq}");
        }
        recorder.finish().unwrap();
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 129);
        let archived = directory.path().join("capture-part-000001.log");
        let alias = directory.path().join("archive-alias.log");
        std::fs::hard_link(&archived, &alias).unwrap();
        assert!(export(&[], ExportFormat::Txt, &alias).is_err());
        assert!(Recorder::open(&alias).is_err());
        assert!(
            std::fs::read_to_string(&archived)
                .unwrap()
                .contains("line\n")
        );
        drop(recorder);
        assert_eq!(open_segment_handles(), 0);
        assert!(export(&[], ExportFormat::Txt, &alias).is_ok());
    }

    #[test]
    fn completed_retention_preserves_replacement_and_modified_segments() {
        for replace in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let base = directory.path().join("capture.log");
            let mut recorder = Recorder::open_readable(&base).unwrap();
            recorder.max_segment_bytes = 1;
            recorder.set_retention(Some(2));
            recorder.write(&fx(0, Direction::Rx, b"first\n")).unwrap();
            recorder.write(&fx(1, Direction::Rx, b"second\n")).unwrap();
            let completed = directory.path().join("capture-part-000001.log");
            if replace {
                std::fs::rename(&completed, directory.path().join("moved.log")).unwrap();
            }
            std::fs::write(&completed, b"preserve modified/replaced content").unwrap();
            recorder.write(&fx(2, Direction::Rx, b"third\n")).unwrap();
            recorder.write(&fx(3, Direction::Rx, b"fourth\n")).unwrap();
            recorder.finish().unwrap();
            assert_eq!(
                std::fs::read(&completed).unwrap(),
                b"preserve modified/replaced content"
            );
            // The expired path reservation is released even when deletion is skipped.
            assert!(export(&[], ExportFormat::Txt, &completed).is_ok());
        }
    }

    #[test]
    fn readable_zero_retention_releases_expired_paths() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("capture.log");
        let mut recorder = Recorder::open_readable(&path).unwrap();
        recorder.max_segment_bytes = 1;
        recorder.set_retention(Some(0));
        recorder.write(&fx(0, Direction::Rx, b"line\n")).unwrap();
        recorder.finish().unwrap();
        assert!(!path.exists());
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
        let mut replacement = Recorder::open_rx(&path).unwrap();
        replacement
            .write(&fx(1, Direction::Rx, b"replacement"))
            .unwrap();
        replacement.flush().unwrap();
        recorder.write(&fx(2, Direction::Rx, b"next\n")).unwrap();
        recorder.finish().unwrap();
        assert_eq!(std::fs::read(path).unwrap(), b"replacement");
    }

    #[test]
    fn readable_rotation_preserves_transcript_across_partial_rx_and_events() {
        let directory = tempfile::tempdir().unwrap();
        let mut expected = Vec::new();
        for rotate in [false, true] {
            let path = directory
                .path()
                .join(if rotate { "rotated.log" } else { "whole.log" });
            let mut recorder = Recorder::open_readable(&path).unwrap();
            if rotate {
                recorder.max_segment_bytes = 1;
            }
            recorder.write(&fx(0, Direction::Rx, b"prompt ")).unwrap();
            recorder.write(&fx(1, Direction::Rx, &[0xe4])).unwrap();
            recorder
                .annotation(0, "MARK", "event", "test", Some(1))
                .unwrap();
            recorder
                .write(&fx(2, Direction::Tx, b"command\r\n"))
                .unwrap();
            recorder
                .write(&fx(3, Direction::Rx, &[0xb8, 0xad, b'\r']))
                .unwrap();
            recorder
                .write(&fx(4, Direction::Rx, b"\n\xff\xe4"))
                .unwrap();
            recorder.finish().unwrap();
            let mut actual = Vec::new();
            for (path, _) in &recorder.leases {
                actual.extend_from_slice(&std::fs::read(path).unwrap());
            }
            if rotate {
                assert_eq!(actual, expected);
                assert!(recorder.part > 1);
            } else {
                expected = actual;
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn rotation_preserves_active_capture_through_numbered_aliases() {
        for hard_link in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("capture.bin");
            let active_path = directory.path().join("active.bin");
            let numbered = directory.path().join("capture.bin.1");
            let mut active = Recorder::open_rx(&active_path).unwrap();
            active.write(&fx(0, Direction::Rx, b"active")).unwrap();
            active.flush().unwrap();
            if hard_link {
                std::fs::hard_link(&active_path, &numbered).unwrap();
            } else {
                std::os::unix::fs::symlink(&active_path, &numbered).unwrap();
            }
            let mut recorder = Recorder::open_rx(&path).unwrap();
            recorder.max_segment_bytes = 1;
            recorder.write(&fx(0, Direction::Rx, b"first")).unwrap();
            assert!(recorder.write(&fx(1, Direction::Rx, b"next")).is_err());
            active.write(&fx(1, Direction::Rx, b" tail")).unwrap();
            active.flush().unwrap();
            assert_eq!(std::fs::read(&active_path).unwrap(), b"active tail");
            assert_eq!(std::fs::read(&numbered).unwrap(), b"active tail");
            assert_eq!(std::fs::read(&path).unwrap(), b"first");
        }
    }

    #[test]
    fn replay_skips_invalid_utf8_lines_and_retains_neighboring_frames() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("capture.jsonl");
        let frames = [
            fx(0, Direction::Rx, b"first"),
            fx(1, Direction::Rx, b"last"),
        ];
        let mut bytes = serde_json::to_vec(&frames[0]).unwrap();
        bytes.extend_from_slice(b"\n\xff\n \t\n");
        bytes.extend_from_slice(&serde_json::to_vec(&frames[1]).unwrap());
        std::fs::write(&path, bytes).unwrap();
        let (actual, skipped) = read_log(&path).unwrap();
        assert_eq!(actual, frames);
        assert_eq!(skipped, 1);
    }

    #[test]
    fn frame_arc_数据() {
        let f = fx(3, Direction::Rx, b"data");
        assert_eq!(*f.data, Arc::new(b"data".to_vec()).as_ref().clone());
    }

    #[test]
    fn raw_log_preserves_bytes_across_chunks_and_excludes_tx() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("kernel.log");
        let mut recorder = Recorder::open_rx(&path).unwrap();
        let bytes = "[kernel] 中文\r\nlogin: ".as_bytes();
        for (seq, byte) in bytes.iter().enumerate() {
            recorder
                .write(&fx(seq as u64, Direction::Rx, &[*byte]))
                .unwrap();
            recorder
                .write(&fx(seq as u64, Direction::Tx, b"AT\r\n"))
                .unwrap();
        }
        recorder.flush().unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert!(
            Recorder::open_rx(&path).is_err(),
            "never append a new session to an existing RX file"
        );
    }

    #[test]
    fn raw_rotation_does_not_collide_with_jsonl_segments() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.log");
        let structured = directory.path().join("session.jsonl.1");
        std::fs::write(&structured, "untouched").unwrap();
        let mut recorder = Recorder::open_rx(&path).unwrap();
        recorder.max_segment_bytes = 3;
        recorder.write(&fx(0, Direction::Rx, b"abc")).unwrap();
        recorder.write(&fx(1, Direction::Rx, b"def")).unwrap();
        recorder.flush().unwrap();
        assert_eq!(
            std::fs::read(directory.path().join("session.log.1")).unwrap(),
            b"abc"
        );
        assert_eq!(std::fs::read(path).unwrap(), b"def");
        assert_eq!(std::fs::read(structured).unwrap(), b"untouched");
    }
}
#[test]
fn text_segments_are_stable_and_retained_unless_explicitly_limited() {
    for extension in ["log", "txt", "LOG"] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(format!("capture.{extension}"));
        assert!(is_readable(&path));
        let mut recorder = Recorder::open_readable(&path).unwrap();
        recorder.max_segment_bytes = 1;
        for seq in 0..7 {
            recorder
                .write(&Frame::new(
                    seq,
                    crate::frame::Direction::Rx,
                    b"line\n".to_vec(),
                    0,
                    seq,
                ))
                .unwrap();
        }
        recorder.finish().unwrap();
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 8);
        let first = dir.path().join(format!("capture-part-000001.{extension}"));
        let original = std::fs::read(&first).unwrap();
        recorder
            .write(&Frame::new(
                8,
                crate::frame::Direction::Rx,
                b"later\n".to_vec(),
                0,
                8,
            ))
            .unwrap();
        recorder.finish().unwrap();
        assert_eq!(std::fs::read(&first).unwrap(), original);
        let limited = dir.path().join(format!("limited.{extension}"));
        let mut recorder = Recorder::open_readable(&limited).unwrap();
        recorder.max_segment_bytes = 1;
        recorder.set_retention(Some(2));
        for seq in 0..6 {
            recorder
                .write(&Frame::new(
                    seq,
                    crate::frame::Direction::Rx,
                    b"line\n".to_vec(),
                    0,
                    seq,
                ))
                .unwrap();
        }
        recorder.finish().unwrap();
        assert!(!limited.exists());
        assert!(
            !dir.path()
                .join(format!("limited-part-000003.{extension}"))
                .exists()
        );
        assert!(
            dir.path()
                .join(format!("limited-part-000004.{extension}"))
                .exists()
        );
    }
}
