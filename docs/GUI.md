<!--
flattencom - Gui Guide

Documents desktop controls, session workflows, history navigation, recording and evidence export.

Authors:
worryzu <worryzu@gmail.com> @LinearTeam

Copyright (C) 2026 Evarentha
SPDX-License-Identifier: GPL-3.0-or-later
-->

# Graphical interface

## Open and share a session

Launch `flattencom-gui`, choose **File > Open port**, then select the port and its serial settings. GUI defaults to 115200 8N1. Check the recording directory before connecting. Connected/reconnecting ports join the existing session and preserve its configuration and recording; another application's port ownership is still an error.

The connection badge and recording status are separate. **Pause display** freezes the receive view, not serial capture. **Resume live** jumps to recent output. **File > Detach view** closes only the tab; **Session > Close shared session** releases the port for every client. Closing the GUI leaves service-owned sessions running.

## Workspace and menus

| Location | Actions |
|---|---|
| File | Open port, Capture library, Export, Issue bundle, Detach view, Exit |
| Edit / receive context menu | Copy, select, find, mark, measure, clear view |
| View | Docks/toolbars, Theme, Language, Reset dock layout |
| Session | Serial settings, pause/resume, reset, markers/boots, long command, shared selection, send file, replay, close |
| Tools | Rules, reset settings, recording defaults/directory/retention, triggers, decoder options |
| Window / Help | Open tabs; MCP setup and About |

The device and inspector panels are dockable; the event log is initially hidden. Layout is saved in QSettings. Reset dock layout restores the default arrangement. Session actions apply to the active tab and are disabled when none is selected.

The **Inspector** shows statistics, rates, control lines and complete selected-record details. JSON is the default detail mode; Fields is an alternative in the same area. Search switches to JSON for precise navigation. Field names may be translated, while raw JSON, unknown keys and copied values remain unchanged. DTR/RTS are writable; an unknown initial output state is distinct from an asserted or deasserted line.

## Receive views and navigation

- **Text stream (RX):** joins UTF-8 and CR/LF/CRLF across transport chunks, shows unfinished prompts immediately, and hides common ANSI decoration. TX does not interrupt live RX text. This is a log viewer, not a VT terminal emulator.
- **Chunks (RX/TX):** exposes timestamps, direction, data/HEX, decoded results and TX source. Selecting HEX switches to this view; selecting continuous text clears HEX mode.
- **Sent commands:** a separate, initially collapsed drawer. Expand it to filter, copy text/HEX or locate subsequent output. It continues updating while RX display is paused and does not automatically expand on a send.

Selecting output or navigating upward pauses display. Scrolling downward does not implicitly resume. Clear view removes GUI content; it neither deletes recorded files nor clears the service or driver buffers.

Scroll to the top to load older saved output into the same text area. The history worker indexes capture segments and displays a three-page window. Each page is limited to 65,536 UTF-16 units and 4000 line/paragraph breaks; one extra unit is allowed to keep a surrogate pair intact. Large source records can span pages, with within-record offsets preserved for navigation. The first access to a large capture may take time to index. Historical text includes formatted TX entries where available. **Resume live** returns to recent RX output.

With recording available, search scans saved session files in the background and displays at most 1000 results. Otherwise it searches retained display content. Changing the query cancels the prior search. If search reaches 1000 results, additional matches may be omitted. Raw legacy logs without timestamps cannot provide timing measurements; readable captures use host wall-clock time rather than a separate saved monotonic clock.

Saved-log search evaluates lines longer than 16,384 UTF-16 code units in bounded windows with 1,024 units of overlap and reports limited coverage. Adjacent-character context prevents line/subject anchors from matching artificial window edges. Expressions spanning larger boundaries may be missed; lookarounds requiring more context than the window provides may also differ from whole-line evaluation. Earlier matching text is retained as a result; selecting a result navigates to its position within the saved record, including later lines of a multi-line frame.

## Sending and configuration

The input supports text or HEX, selectable CRLF/LF/CR/None endings, history and periodic sends. The ending control affects outgoing text only. Macro insertion fills the input and does not send automatically. **Long command** accepts multiline text. Large inputs and files use background operations; closing their progress window does not cancel them. Cancel stops later work, not bytes already sent.

Progress polling resumes after transient connection, timeout or queue errors. While disconnected, the operation's status is unknown. If the service reports that the operation no longer exists, the dialog stops polling and notes that the service may have restarted.

Device profiles store settings, labels, default decoder, send ending, recording preferences and reset steps. Profiles use USB VID/PID/serial where supplied, otherwise a path. Settings edited while identity lookup is pending are preserved; superseded lookup results are ignored. Backend identity-based reconnect separately checks uniqueness; ambiguous matches are not chosen arbitrarily.

Parity offers None, Odd, Even, Mark (always 1) and Space (always 0). Stop bits offer 1, 1.5 and 2. Select 5 data bits for 1.5 stop bits, or 6–8 data bits for 2 stop bits; unsupported driver settings produce an error rather than silently changing the format.

**Tools > Rules** edits local display highlighting, not device behavior. **Tools > Triggers** edits backend frame matches and response/event/program actions. **Decoder options / plugins** configures decoding, Modbus role, process arguments and WASM fuel. These backend settings affect all clients of the session.

## Markers, reset and selected evidence

Mark creates an annotation at the selected sequence or current tail. Hold Shift while activating **Mark** to enter a label. Markers/boots can locate retained or saved history. Boot counts are based on recognized banners such as U-Boot, Linux, ESP-ROM and Zephyr.

Use **Tools > Reset settings** to edit and save board-specific DTR/RTS, data/HEX and delay steps. **Session > Reset** runs the profile; without one, it first opens settings. The view locates recognized boot output after reset. If no banner appears within two minutes, check the output at the reset marker to assess the result.

**File > Issue bundle** writes a single `.txt` report with the selected text, configuration, available related sent commands and annotations. It does not contain the full recording. **Session > Share selection** creates a fixed snapshot and copies MCP instructions. Revoke it in the result dialog. For enforced access scope, use [selection-only MCP](MCP.md#selected-evidence).

## Recording and previous files

See [Recording and export](RECORDING.md) for file formats, locations, retention and failure behavior. New GUI sessions record by default; joining an existing session never changes its paths.

**File > Capture library** lists saved files and supports copying their paths, opening them, saving a copy and paged viewing. The independent viewer reads about 256 KiB at a time, supports beginning/end/percentage navigation and cancellable case-sensitive search. Refresh reopens a changed file or reads appended data. This is local disk access, not a daemon history RPC.

## Language, theme and shortcuts

**View > Language** switches English/Chinese in place, preserving sessions and inputs. The submenu title remains `Language`. **View > Theme** offers Follow system, Light and Dark. New installations follow the system; saved explicit preferences remain. An unknown system color scheme falls back to Light.

The application uses the orange-red C icon. **Help > About** displays the stacked flatten/COM wordmark: ink and orange-red in Light, warm-white and orange-red in Dark. An open About window updates its artwork when the theme changes.

| Shortcut | Action |
|---|---|
| Ctrl+O | Open port |
| Ctrl+S | Export current retained capture |
| Ctrl+F | Search |
| Ctrl+M | Mark |
| Ctrl+Shift+T | Measure selection |
| Ctrl+L | Clear view |
| Ctrl+W | Detach view |
| Ctrl+Tab / Ctrl+Shift+Tab | Next / previous tab |
| Ctrl+Q | Exit GUI |

For connection, rendering or recording problems, see [Troubleshooting](TROUBLESHOOTING.md).
