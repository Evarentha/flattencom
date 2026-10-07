/*
 * flattencom - CLI Tui Mod
 *
 * Renders the terminal monitor and handles navigation, filtering and interactive send commands.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

//! TUI: interactive direct (`monitor`) or shared (`attach`) monitoring.
//!
//! Data-source abstraction ([`FrameSource`]):
//! - `EmbeddedSource`: direct core session (`flattencom monitor`);
//! - `AttachedSource`: daemon session shared with GUI/MCP (`flattencom attach`).
//!
//! Layout:
//! ```text
//! + flattencom -- Port/session -- State --------------------+
//! | Time, direction, text/hex data, decoding | Scroll/tail   |
//! │ …                                                    │
//! ├──────────────────────────────────────────────────────┤
//! | Send input | Filter prompt | Help overlay               |
//! + RX/TX statistics, rates, errors, evictions, buffer ------+
//! ```
//!
//! Keys: `q`/`Ctrl-C` quit, `Space` pause/follow, `e` switch text/hex;
//! `/` filter, `s` send mode (Enter sends, Esc exits), `g` clear buffer;
//! arrows and PgUp/PgDn scroll, `h` shows help.

pub mod source;

use std::borrow::Cow;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use source::FrameSource;
use unicode_segmentation::UnicodeSegmentation;

use crate::cmd::CmdError;

/// Source-independent TUI frame view model.
pub type FrameView = flattencom_proto::methods::FrameOut;

/// Display mode.
#[derive(Clone, Copy, PartialEq, Eq)]
enum TextMode {
    /// Lossy text.
    Text,
    /// Hexadecimal.
    Hex,
}

/// Application state.
struct App {
    source: Box<dyn FrameSource>,
    frames: Vec<FrameView>,
    /// Maximum retained view records to bound memory.
    max_view: usize,
    scroll: usize,
    text_mode: TextMode,
    paused: bool,
    /// Input mode: None for commands, Some for line-oriented input.
    input: Option<InputMode>,
    /// Send history (Up recalls previous entries).
    history: Vec<String>,
    history_idx: Option<usize>,
    /// View-side substring filter.
    filter: Option<String>,
    /// Filter input buffer.
    filter_buf: String,
    status: String,
    notification: Option<(String, Instant)>,
    decoder_index: usize,
    layout: ViewLayout,
}

/// Input mode.
enum InputMode {
    /// Send input line.
    Send(String),
    /// Filter input line.
    Filter,
    /// Live baud-rate configuration.
    Baud(String),
}

/// Interactive main loop.
fn run_tui(source: Box<dyn FrameSource>) -> Result<i32, CmdError> {
    // Install before entering raw mode so registration failures leave the terminal intact.
    // Signal callbacks only request shutdown; the event loop performs cleanup.
    let stopped = Arc::new(AtomicBool::new(false));
    let signal = Arc::clone(&stopped);
    ctrlc::set_handler(move || signal.store(true, Ordering::Relaxed)).map_err(|e| {
        CmdError::new(flattencom_core::tr!(
            "Failed to install Ctrl-C handler: {e}",
            e = e
        ))
    })?;
    // ratatui 0.29: initialize raw mode and alternate screen; restore the terminal on exit.
    let mut terminal = ratatui::init();
    terminal.hide_cursor().ok();

    let mut app = App {
        source,
        frames: Vec::new(),
        max_view: 20_000,
        scroll: 0,
        text_mode: TextMode::Text,
        paused: false,
        input: None,
        history: Vec::new(),
        history_idx: None,
        filter: None,
        filter_buf: String::new(),
        status: String::new(),
        notification: None,
        decoder_index: 0,
        layout: ViewLayout::default(),
    };

    // Raw-mode Ctrl-C also remains a keyboard shortcut handled below.
    let result = event_loop(&mut terminal, &mut app, &stopped);

    // Restore the terminal on both success and failure.
    terminal.show_cursor().ok();
    ratatui::restore();
    result
}

/// Event loop: poll frames, render and process keys on a 50 ms cadence.
fn event_loop(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    stopped: &AtomicBool,
) -> Result<i32, CmdError> {
    loop {
        if stopped.load(Ordering::Relaxed) {
            return Ok(crate::exit_code::OK);
        }
        // 1) Fetch new frames unless paused.
        if !app.paused
            && let Err(e) = poll_source(app)
        {
            app.status = flattencom_core::tr!("Read failed: {e}", e = e);
        }
        // 2) Render.
        terminal
            .draw(|f| render(f, app))
            .map_err(|e| CmdError::new(flattencom_core::tr!("Render failed: {e}", e = e)))?;
        // 3) Handle events on a 50 ms cadence.
        if event::poll(Duration::from_millis(50)).map_err(|e| CmdError::new(e.to_string()))?
            && let Event::Key(key) = event::read().map_err(|e| CmdError::new(e.to_string()))?
            && handle_key(app, key)
        {
            return Ok(crate::exit_code::OK);
        }
    }
}

/// Fetch source frames and append them to the view.
fn poll_source(app: &mut App) -> Result<(), String> {
    let (new_frames, status) = app.source.poll(256 * 1024)?;
    // Preserve the local capture; changing a filter must reveal older matches.
    if !new_frames.is_empty() {
        app.layout.rendered = None;
    }
    app.frames.extend(new_frames);
    if app.frames.len() > app.max_view {
        let removed = app.frames.len() - app.max_view;
        app.frames.drain(..removed);
        app.layout
            .frames
            .drain(..removed.min(app.layout.frames.len()));
    }
    if !status.is_empty() {
        app.status = status;
    }
    Ok(())
}

/// Process a key; true requests exit.
fn handle_key(app: &mut App, key: KeyEvent) -> bool {
    if key.kind == KeyEventKind::Release {
        return false;
    }
    // Ctrl-C exits from any input mode.
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        return true;
    }
    match &mut app.input {
        Some(InputMode::Send(line)) => match key.code {
            KeyCode::Enter => {
                let text = std::mem::take(line);
                if !text.is_empty() {
                    match app.source.send(text.clone()) {
                        Ok(()) => {
                            app.history.push(text);
                            app.history_idx = None;
                        }
                        Err(e) => app.notify(flattencom_core::tr!("Send failed: {e}", e = e)),
                    }
                }
                app.input = None;
            }
            KeyCode::Esc => app.input = None,
            KeyCode::Backspace => {
                line.pop();
            }
            KeyCode::Up => {
                if !app.history.is_empty() {
                    let idx = app
                        .history_idx
                        .unwrap_or(app.history.len())
                        .saturating_sub(1);
                    if let Some(h) = app.history.get(idx) {
                        *line = h.clone();
                        app.history_idx = Some(idx);
                    }
                }
            }
            KeyCode::Down => {
                if let Some(idx) = app.history_idx {
                    if idx + 1 < app.history.len() {
                        *line = app.history[idx + 1].clone();
                        app.history_idx = Some(idx + 1);
                    } else {
                        app.history_idx = None;
                        line.clear();
                    }
                }
            }
            KeyCode::Char(c) => line.push(c),
            _ => {}
        },
        Some(InputMode::Baud(line)) => match key.code {
            KeyCode::Enter => {
                let message = match line.parse::<u32>() {
                    Ok(baud) if baud > 0 => match app.source.configure(baud) {
                        Ok(()) => flattencom_core::tr!("Baud rate set to {baud}", baud = baud),
                        Err(e) => e,
                    },
                    _ => flattencom_core::i18n::text("Baud rate must be a positive integer").into(),
                };
                app.notify(message);
                app.input = None;
            }
            KeyCode::Esc => app.input = None,
            KeyCode::Backspace => {
                line.pop();
            }
            KeyCode::Char(c) if c.is_ascii_digit() => line.push(c),
            _ => {}
        },
        Some(InputMode::Filter) => match key.code {
            KeyCode::Enter => {
                app.filter = if app.filter_buf.is_empty() {
                    None
                } else {
                    Some(app.filter_buf.clone())
                };
                app.input = None;
                app.scroll = 0;
            }
            KeyCode::Esc => {
                app.input = None;
                app.filter_buf.clear();
            }
            KeyCode::Backspace => {
                app.filter_buf.pop();
            }
            KeyCode::Char(c) => app.filter_buf.push(c),
            _ => {}
        },
        None => match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return true,
            KeyCode::Char(' ') => {
                app.paused = !app.paused;
                app.scroll = 0;
            }
            KeyCode::Up | KeyCode::PageUp => {
                app.paused = true;
                app.scroll =
                    app.scroll
                        .saturating_add(if key.code == KeyCode::Up { 1 } else { 15 });
            }
            KeyCode::Down | KeyCode::PageDown => {
                app.scroll =
                    app.scroll
                        .saturating_sub(if key.code == KeyCode::Down { 1 } else { 15 });
            }
            KeyCode::End => {
                app.scroll = 0;
                app.paused = false;
            }
            KeyCode::Char('e') => {
                app.text_mode = if app.text_mode == TextMode::Text {
                    TextMode::Hex
                } else {
                    TextMode::Text
                };
            }
            KeyCode::Char('s') => app.input = Some(InputMode::Send(String::new())),
            KeyCode::Char('c') => app.input = Some(InputMode::Baud(String::new())),
            KeyCode::Char('d') => {
                const DECODERS: &[&str] = &[
                    "none",
                    "ascii_lines",
                    "hex_dump",
                    "utf8_lossy",
                    "json_lines",
                    "modbus_rtu",
                    "nmea0183",
                ];
                app.decoder_index = (app.decoder_index + 1) % DECODERS.len();
                let decoder = DECODERS[app.decoder_index];
                let message = match app.source.decoder(decoder) {
                    Ok(()) => flattencom_core::tr!("Decoder: {decoder}", decoder = decoder),
                    Err(e) => e,
                };
                app.notify(message);
            }
            KeyCode::Char('/') => {
                app.filter_buf.clear();
                app.input = Some(InputMode::Filter);
            }
            KeyCode::Char('g') => {
                let n = app.source.clear_buffer();
                app.frames.clear();
                app.layout = ViewLayout::default();
                app.scroll = 0;
                app.notify(flattencom_core::tr!("Cleared {n} frames", n = n));
            }
            KeyCode::Char('h') => app.notify(flattencom_core::i18n::text(HELP).to_owned()),
            _ => {}
        },
    }
    false
}

/// Help text.
const HELP: &str = "[q]Quit [Space]Pause [e]HEX [/]Filter [c]Baud [d]Decoder [s]Send [g]Clear (send supports hex: and \\r\\n)";

/// Render the view.
fn render(f: &mut ratatui::Frame<'_>, app: &mut App) {
    let area = f.area();
    // Layout: main view above input/status rows.
    let main = ratatui::layout::Rect {
        height: area.height.saturating_sub(2),
        ..area
    };
    let foot = ratatui::layout::Rect {
        y: main.bottom(),
        height: 2,
        ..area
    };

    let lines = viewport(
        app,
        main.width.saturating_sub(2),
        main.height.saturating_sub(2) as usize,
    );

    let title = format!(
        " flattencom | {} | {} [{}]{} ",
        app.source.label(),
        app.paused_text(),
        if app.text_mode == TextMode::Text {
            "text"
        } else {
            "hex"
        },
        app.filter
            .as_ref()
            .map(|f| flattencom_core::tr!(" Filter: {f}", f = f))
            .unwrap_or_default(),
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::default().fg(Color::DarkGray));
    f.render_widget(Paragraph::new(lines).block(block), main);

    // Input/status row
    let input_line = match &app.input {
        Some(InputMode::Send(line)) => Line::from(vec![
            Span::styled(
                flattencom_core::i18n::text("Send: "),
                Style::default().fg(Color::Green),
            ),
            Span::raw(line.clone()),
            Span::styled("_", Style::default().fg(Color::DarkGray)),
        ]),
        Some(InputMode::Filter) => Line::from(vec![
            Span::styled(
                flattencom_core::i18n::text("Filter: "),
                Style::default().fg(Color::Yellow),
            ),
            Span::raw(app.filter_buf.clone()),
            Span::styled("_", Style::default().fg(Color::DarkGray)),
        ]),
        Some(InputMode::Baud(value)) => {
            Line::from(flattencom_core::tr!("Baud: {value}_", value = value))
        }
        None => Line::from(Span::styled(
            flattencom_core::i18n::text(
                "[s]Send [e]View [/]Filter [c]Baud [d]Decoder [Space]Pause [q]Quit",
            ),
            Style::default().fg(Color::DarkGray),
        )),
    };
    f.render_widget(
        Paragraph::new(vec![
            input_line,
            Line::from(Span::raw(app.display_status().to_owned())),
        ]),
        foot,
    );
}

/// Sparse checkpoints bound index storage to two machine words per 64 rows.
/// Frames are immutable after insertion; entries follow their vector slots through eviction.
const ROW_STRIDE: usize = 64;

#[derive(Clone, Copy, Default)]
struct RowPosition {
    span: usize,
    byte: usize,
}

struct FrameLayout {
    count: usize,
    checkpoints: Vec<RowPosition>,
    display: Line<'static>,
    ascii: Vec<bool>,
}

#[derive(Default)]
struct ViewLayout {
    key: Option<(u16, TextMode, Option<String>)>,
    frames: Vec<Option<FrameLayout>>,
    rendered: Option<(usize, usize, Vec<Line<'static>>)>,
    #[cfg(test)]
    indexed_frames: usize,
    #[cfg(test)]
    materialized_rows: usize,
    #[cfg(test)]
    materialized_bytes: usize,
}

/// Visit row boundaries without allocating strings or spans per grapheme. Printable
/// ASCII runs use byte arithmetic; other text uses ratatui's grapheme/width semantics.
/// Returning false stops before scanning any more content.
fn row_boundaries(
    line: &Line<'_>,
    ascii: &[bool],
    width: u16,
    start: RowPosition,
    visited: &mut usize,
    mut emit: impl FnMut(RowPosition) -> bool,
) {
    let mut used = 0usize;
    let width = usize::from(width);
    for (span_index, span) in line.spans.iter().enumerate().skip(start.span) {
        let mut offset = if span_index == start.span {
            start.byte
        } else {
            0
        };
        let remaining = &span.content[offset..];
        if ascii[span_index] {
            let mut left = remaining.len();
            while left > 0 {
                if used >= width {
                    if !emit(RowPosition {
                        span: span_index,
                        byte: offset,
                    }) {
                        return;
                    }
                    used = 0;
                }
                let take = left.min(width - used);
                *visited += take;
                left -= take;
                used += take;
                offset += take;
            }
            continue;
        }
        // Do not use styled_graphemes: its LF filter scans an entire blank-line
        // suffix before yielding. Raw graphemes expose every separator immediately.
        for grapheme in remaining.graphemes(true) {
            *visited += grapheme.len();
            if matches!(grapheme, "\n" | "\r\n") {
                offset += grapheme.len();
                if !emit(RowPosition {
                    span: span_index,
                    byte: offset,
                }) {
                    return;
                }
                used = 0;
            } else {
                let size = Span::raw(grapheme).width();
                if used > 0 && used + size > width {
                    if !emit(RowPosition {
                        span: span_index,
                        byte: offset,
                    }) {
                        return;
                    }
                    used = 0;
                }
                used += size;
                offset += grapheme.len();
            }
        }
    }
}

fn index_frame(line: &Line<'_>, width: u16) -> FrameLayout {
    let mut layout = FrameLayout {
        count: 1,
        checkpoints: vec![RowPosition::default()],
        display: Line::from(
            line.spans
                .iter()
                .map(|span| Span::styled(span.content.to_string(), span.style))
                .collect::<Vec<_>>(),
        ),
        ascii: line
            .spans
            .iter()
            .map(|span| span.content.bytes().all(|b| (b' '..=b'~').contains(&b)))
            .collect(),
    };
    row_boundaries(
        line,
        &layout.ascii,
        width,
        RowPosition::default(),
        &mut 0,
        |position| {
            if layout.count.is_multiple_of(ROW_STRIDE) {
                layout.checkpoints.push(position);
            }
            layout.count += 1;
            true
        },
    );
    layout
}

/// Copy only spans intersecting a visible row, preserving styles and all bytes
/// except the explicit newline that separates this row from the next.
fn row_slice(line: &Line<'_>, start: RowPosition, end: RowPosition) -> Line<'static> {
    let mut spans = Vec::new();
    for index in start.span..=end.span.min(line.spans.len().saturating_sub(1)) {
        let span = &line.spans[index];
        let from = if index == start.span { start.byte } else { 0 };
        let to = if index == end.span {
            end.byte
        } else {
            span.content.len()
        };
        let text = span.content[from..to].trim_end_matches('\n');
        if !text.is_empty() {
            spans.push(Span::styled(text.to_owned(), span.style));
        }
    }
    Line::from(spans)
}

fn visible_rows(
    width: u16,
    layout: &FrameLayout,
    start: usize,
    end: usize,
    visited: &mut usize,
) -> Vec<Line<'static>> {
    if start == end {
        return Vec::new();
    }
    let checkpoint = start / ROW_STRIDE;
    let mut row = checkpoint * ROW_STRIDE;
    let mut previous = layout.checkpoints[checkpoint];
    let mut rows = Vec::with_capacity(end - start);
    let line = &layout.display;
    row_boundaries(line, &layout.ascii, width, previous, visited, |position| {
        if row >= start {
            rows.push(row_slice(line, previous, position));
        }
        row += 1;
        previous = position;
        row < end
    });
    if row < end {
        rows.push(row_slice(
            line,
            previous,
            RowPosition {
                span: line.spans.len(),
                byte: 0,
            },
        ));
    }
    rows
}

/// Index each visited frame once per width/mode/filter. Repeated redraws clone
/// only the last viewport; scrolling materializes rows from sparse checkpoints.
fn viewport(app: &mut App, width: u16, height: usize) -> Vec<Line<'static>> {
    if width == 0 || height == 0 {
        return Vec::new();
    }
    let key = (width, app.text_mode, app.filter.clone());
    if app.layout.key.as_ref() != Some(&key) {
        app.layout = ViewLayout {
            key: Some(key),
            ..ViewLayout::default()
        };
    }
    if app.layout.frames.len() != app.frames.len() {
        app.layout.frames.resize_with(app.frames.len(), || None);
        app.layout.rendered = None;
    }
    if let Some((scroll, previous_height, rows)) = &app.layout.rendered
        && *scroll == app.scroll
        && *previous_height == height
    {
        return rows.clone();
    }
    let filter = app.filter.as_ref().map(|s| s.to_lowercase());
    let mut skipped = 0usize;
    let mut rows = VecDeque::new();
    for (index, frame) in app.frames.iter().enumerate().rev() {
        if app.layout.frames[index].is_none() {
            let matches = !filter.as_ref().is_some_and(|filter| {
                !format!(
                    "{} {} {}",
                    frame.text.as_deref().unwrap_or_default(),
                    frame.hex.as_deref().unwrap_or_default(),
                    frame.decoded_text.as_deref().unwrap_or_default()
                )
                .to_lowercase()
                .contains(filter)
            });
            app.layout.frames[index] = Some(if matches {
                index_frame(&frame_line(frame, app.text_mode), width)
            } else {
                FrameLayout {
                    count: 0,
                    checkpoints: Vec::new(),
                    display: Line::default(),
                    ascii: Vec::new(),
                }
            });
            #[cfg(test)]
            {
                app.layout.indexed_frames += 1;
            }
        }
        let layout = app.layout.frames[index].as_ref().unwrap();
        let count = layout.count;
        let end = count.saturating_sub(app.scroll.saturating_sub(skipped));
        let start = end.saturating_sub(height - rows.len());
        if start < end {
            let mut visited = 0;
            let selected = visible_rows(width, layout, start, end, &mut visited);
            #[cfg(test)]
            {
                app.layout.materialized_rows += selected.len();
                app.layout.materialized_bytes += visited;
            }
            for row in selected.into_iter().rev() {
                rows.push_front(row);
            }
        }
        skipped += count;
        if rows.len() == height {
            break;
        }
    }
    let maximum = skipped.saturating_sub(height);
    if rows.len() < height && app.scroll > maximum {
        app.scroll = maximum;
        return viewport(app, width, height);
    }
    let rows: Vec<_> = rows.into();
    app.layout.rendered = Some((app.scroll, height, rows.clone()));
    rows
}

/// Render a frame row: time, direction and content/decoding.
fn frame_line(fr: &FrameView, mode: TextMode) -> Line<'_> {
    let t = flattencom_core::timefmt::fmt_clock_us(fr.t_us);
    let (dir_color, content) = match fr.dir.as_str() {
        "rx" => (
            Color::Cyan,
            match mode {
                TextMode::Text => fr.text.as_deref().unwrap_or_default(),
                TextMode::Hex => fr.hex.as_deref().unwrap_or_default(),
            },
        ),
        _ => (
            Color::Green,
            match mode {
                TextMode::Text => fr.text.as_deref().unwrap_or_default(),
                TextMode::Hex => fr.hex.as_deref().unwrap_or_default(),
            },
        ),
    };
    let mut spans = vec![
        Span::styled(format!("{t} "), Style::default().fg(Color::DarkGray)),
        Span::styled(
            fr.dir.as_str().to_uppercase(),
            Style::default().fg(dir_color),
        ),
        Span::raw(" "),
        Span::raw(if content.contains(['\r', '\n', '\0']) {
            Cow::Owned(
                content
                    .replace('\r', "␍")
                    .replace('\n', "␊")
                    .replace('\0', "␀"),
            )
        } else {
            Cow::Borrowed(content)
        }),
    ];
    if let Some(decoded) = &fr.decoded_text {
        let style = match fr.decoded_level.as_deref() {
            Some("error") => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            Some("warn") => Style::default().fg(Color::Yellow),
            _ => Style::default().fg(Color::Magenta),
        };
        spans.push(Span::styled("  ", style));
        spans.push(Span::styled(decoded.as_str(), style));
    }
    Line::from(spans)
}

impl App {
    fn notify(&mut self, message: String) {
        self.notification = Some((message, Instant::now()));
    }

    fn display_status(&self) -> &str {
        self.notification
            .as_ref()
            .filter(|(_, created)| created.elapsed() < Duration::from_secs(5))
            .map_or(&self.status, |(message, _)| message)
    }

    /// Paused-state caption.
    fn paused_text(&self) -> &'static str {
        if self.paused {
            flattencom_core::i18n::text("Paused (Space to resume)")
        } else {
            flattencom_core::i18n::text("Following tail")
        }
    }
}

/// Public entry point for direct monitoring.
pub fn run_monitor(
    port: Option<String>,
    baud: u32,
    data_bits: Option<u8>,
    parity: Option<String>,
    stop_bits: Option<flattencom_core::config::StopBits>,
    flow: Option<String>,
    record_to: Option<std::path::PathBuf>,
) -> Result<i32, CmdError> {
    // Select a port; list choices when none is supplied.
    let port = match port {
        Some(p) => p,
        None => {
            let ports = flattencom_core::discovery::list_ports().map_err(CmdError::from_core)?;
            if ports.is_empty() {
                println!(
                    "{}",
                    flattencom_core::tr!(
                        "No serial ports found. Check the device connection and driver."
                    )
                );
                return Ok(crate::exit_code::OK);
            }
            println!("{}", flattencom_core::tr!("Specify a serial port:"));
            for p in ports {
                println!("  {}  {}", p.path, p.friendly_name);
            }
            return Ok(crate::exit_code::OK);
        }
    };
    let source = source::EmbeddedSource::open(
        &port,
        baud,
        data_bits,
        parity.as_deref(),
        stop_bits,
        flow.as_deref(),
        record_to,
    )?;
    run_tui(Box::new(source))
}

/// Public entry point for shared attachment.
pub fn run_attached(
    client: flattencom_proto::client::DaemonClient,
    session_id: String,
    port: String,
    rt: tokio::runtime::Runtime,
) -> Result<i32, CmdError> {
    let source = source::AttachedSource::new(client, session_id, port, rt);
    run_tui(Box::new(source))
}

#[cfg(test)]
mod tests {
    use super::*;
    use flattencom_core::frame::{Direction, Frame};
    use flattencom_proto::methods::{FrameFormat, frame_out};
    use ratatui::{Terminal, backend::TestBackend};

    #[derive(Default)]
    struct Source {
        incoming: Vec<FrameView>,
    }
    impl FrameSource for Source {
        fn poll(&mut self, _: u64) -> Result<(Vec<FrameView>, String), String> {
            Ok((std::mem::take(&mut self.incoming), "live statistics".into()))
        }
        fn send(&mut self, _: String) -> Result<(), String> {
            Err("invalid payload".into())
        }
        fn clear_buffer(&mut self) -> u64 {
            0
        }
        fn label(&self) -> String {
            "test".into()
        }
        fn configure(&mut self, _: u32) -> Result<(), String> {
            Ok(())
        }
        fn decoder(&mut self, _: &str) -> Result<(), String> {
            Ok(())
        }
    }

    fn app() -> App {
        App {
            source: Box::new(Source::default()),
            frames: Vec::new(),
            max_view: 20_000,
            scroll: 0,
            text_mode: TextMode::Text,
            paused: false,
            input: None,
            history: Vec::new(),
            history_idx: None,
            filter: None,
            filter_buf: String::new(),
            status: String::new(),
            notification: None,
            decoder_index: 0,
            layout: ViewLayout::default(),
        }
    }

    fn frame(text: &str) -> FrameView {
        frame_out(
            &Frame::new(0, Direction::Rx, text.as_bytes().to_vec(), 0, 0),
            FrameFormat::Decoded,
        )
    }

    fn key(app: &mut App, code: KeyCode) {
        assert!(!handle_key(app, KeyEvent::new(code, KeyModifiers::NONE)));
    }

    fn screen(app: &mut App, terminal: &mut Terminal<TestBackend>) -> String {
        terminal.draw(|f| render(f, app)).unwrap();
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect()
    }

    #[test]
    fn notifications_survive_polling_and_expire_back_to_current_stats() {
        let mut app = app();
        let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
        app.input = Some(InputMode::Send("hex:GG".into()));
        key(&mut app, KeyCode::Enter);
        poll_source(&mut app).unwrap();
        assert!(screen(&mut app, &mut terminal).contains("invalid payload"));
        key(&mut app, KeyCode::Char('h'));
        poll_source(&mut app).unwrap();
        assert!(screen(&mut app, &mut terminal).contains("send supports"));
        app.notification.as_mut().unwrap().1 =
            Instant::now().checked_sub(Duration::from_secs(6)).unwrap();
        assert!(screen(&mut app, &mut terminal).contains("live statistics"));
    }

    #[test]
    fn wrapped_tail_and_row_navigation_preserve_long_frame_contents() {
        let mut app = app();
        app.frames = vec![frame(&"A".repeat(1800)), frame("LATEST_MARKER")];
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        assert!(screen(&mut app, &mut terminal).contains("LATEST_MARKER"));
        key(&mut app, KeyCode::PageUp);
        assert!(!screen(&mut app, &mut terminal).contains("LATEST_MARKER"));
        key(&mut app, KeyCode::End);
        assert!(screen(&mut app, &mut terminal).contains("LATEST_MARKER"));

        // Retrieve every row of a single long Unicode record in one-row pages.
        app.frames = vec![frame(&"设备abc".repeat(100))];
        let expected = frame_line(&app.frames[0], TextMode::Text).to_string();
        let mut pieces = Vec::new();
        loop {
            let requested = app.scroll;
            let rows = viewport(&mut app, 17, 1);
            if app.scroll != requested {
                break;
            }
            assert_eq!(rows.len(), 1);
            assert!(rows[0].width() <= 17);
            pieces.push(rows[0].to_string());
            app.scroll += 1;
        }
        pieces.reverse();
        assert_eq!(pieces.concat(), expected);
    }

    #[test]
    fn filtering_resize_and_clear_clamp_row_scroll_without_blank_views() {
        let mut app = app();
        app.frames = vec![frame(&"A".repeat(1000)), frame("needle")];
        app.scroll = usize::MAX;
        assert!(
            !viewport(&mut app, 20, 5).is_empty(),
            "{:?}",
            viewport(&mut app, 20, 5).is_empty()
        );
        app.input = Some(InputMode::Filter);
        app.filter_buf = "needle".into();
        key(&mut app, KeyCode::Enter);
        assert_eq!(app.scroll, 0);
        assert!(viewport(&mut app, 80, 20)[0].to_string().contains("needle"));
        app.filter = None;
        app.scroll = 30;
        assert!(
            !viewport(&mut app, 200, 20).is_empty(),
            "{:?}",
            viewport(&mut app, 200, 20).is_empty()
        );
        assert_eq!(app.scroll, 0);
        key(&mut app, KeyCode::Char('g'));
        assert!(
            viewport(&mut app, 80, 20).is_empty(),
            "{:?}",
            viewport(&mut app, 80, 20).is_empty()
        );
    }

    #[test]
    fn layout_work_is_cached_and_only_visible_rows_are_materialized() {
        let mut app = app();
        app.frames = (0..200).map(|_| frame(&"A".repeat(4096))).collect();
        app.scroll = 8000;
        let first = viewport(&mut app, 78, 20);
        assert_eq!(first.len(), 20);
        let indexed = app.layout.indexed_frames;
        assert!(indexed > 100);
        assert_eq!(app.layout.materialized_rows, 20);
        app.paused = true;
        for _ in 0..100 {
            assert_eq!(viewport(&mut app, 78, 20), first);
        }
        assert_eq!(app.layout.indexed_frames, indexed);
        assert_eq!(app.layout.materialized_rows, 20);
        app.scroll -= 1;
        assert_eq!(viewport(&mut app, 78, 20).len(), 20);
        assert_eq!(app.layout.indexed_frames, indexed);
        assert_eq!(app.layout.materialized_rows, 40);
        let checkpoints: usize = app
            .layout
            .frames
            .iter()
            .flatten()
            .map(|l| l.checkpoints.len())
            .sum();
        assert!(checkpoints <= indexed * 2);
        app.frames.push(frame("latest"));
        app.scroll = 0;
        assert!(
            viewport(&mut app, 78, 20)
                .iter()
                .any(|line| line.to_string().contains("latest"))
        );
        assert_eq!(app.layout.indexed_frames, indexed + 1);
    }

    #[test]
    fn sparse_checkpoints_preserve_newlines_styles_and_graphemes() {
        let line = Line::from(vec![
            Span::styled(
                "e\u{301}设备🙂\n\n".repeat(200),
                Style::default().fg(Color::Red),
            ),
            Span::styled("trailing\n", Style::default().fg(Color::Green)),
        ]);
        let layout = index_frame(&line, 7);
        assert!(layout.count > ROW_STRIDE);
        let rows = visible_rows(7, &layout, 0, layout.count, &mut 0);
        assert_eq!(rows.len(), layout.count);
        for (i, expected) in rows.iter().enumerate() {
            assert_eq!(
                visible_rows(7, &layout, i, i + 1, &mut 0),
                vec![expected.clone()]
            );
        }
        assert_eq!(
            rows.iter().map(Line::to_string).collect::<String>(),
            line.to_string().replace('\n', "")
        );
        assert!(
            rows.last().unwrap().spans.is_empty(),
            "{:?}",
            rows.last().unwrap().spans.is_empty()
        );
    }

    #[test]
    fn cached_layout_tracks_eviction_and_width_mode_filter_changes() {
        let mut app = app();
        app.max_view = 2;
        app.frames = vec![frame("old"), frame("kept")];
        viewport(&mut app, 78, 20);
        app.source = Box::new(Source {
            incoming: vec![frame("new")],
        });
        poll_source(&mut app).unwrap();
        let text = viewport(&mut app, 78, 20)
            .iter()
            .map(Line::to_string)
            .collect::<String>();
        assert!(!text.contains("old"));
        assert!(text.contains("kept") && text.contains("new"));
        assert_eq!(app.layout.indexed_frames, 3);
        app.text_mode = TextMode::Hex;
        assert!(
            viewport(&mut app, 78, 20)
                .iter()
                .any(|line| line.to_string().contains("6E 65 77"))
        );
        assert_eq!(app.layout.indexed_frames, 2);
        app.filter = Some("new".into());
        assert_eq!(viewport(&mut app, 78, 20).len(), 1);
        assert!(viewport(&mut app, 10, 20).len() > 1);
        assert_eq!(app.layout.frames.len(), 2);
    }

    #[test]
    fn shifted_scroll_visits_only_checkpoint_to_visible_rows() {
        for text in [
            "A".repeat(1024 * 1024),
            "设备e\u{301}🙂".repeat(80_000),
            "line\r\n".repeat(170_000),
        ] {
            let mut app = app();
            app.frames = vec![frame(&text)];
            viewport(&mut app, 78, 20);
            let indexed = app.layout.indexed_frames;
            for scroll in 1..=100 {
                app.scroll = scroll;
                let before = app.layout.materialized_bytes;
                assert_eq!(viewport(&mut app, 78, 20).len(), 20);
                assert_eq!(app.layout.indexed_frames, indexed);
                // Sparse checkpoint + viewport + at most one lookahead grapheme.
                // This bound is independent of the megabyte suffix after these rows.
                assert!(app.layout.materialized_bytes - before <= (ROW_STRIDE + 20) * 78 * 4);
            }
        }
    }

    #[test]
    fn decoded_blank_line_materialization_does_not_scan_unvisited_suffix() {
        let mut measurements = Vec::new();
        for bytes in [4096, 1024 * 1024] {
            let mut frame = frame("payload");
            frame.decoded_text = Some("\n".repeat(bytes));
            let layout = index_frame(&frame_line(&frame, TextMode::Text), 78);
            assert_eq!(layout.count, bytes + 1);
            let mut visited = 0;
            for first in 1..=100 {
                let rows = visible_rows(78, &layout, first, first + 20, &mut visited);
                assert_eq!(rows.len(), 20);
                assert!(rows.iter().all(|line| line.spans.is_empty()));
            }
            measurements.push(visited);
        }
        assert_eq!(measurements[0], measurements[1]);
        assert!(measurements[1] < 100 * (ROW_STRIDE + 20 + 32));
    }
}
