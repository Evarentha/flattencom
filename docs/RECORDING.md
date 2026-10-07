<!--
flattencom - Recording Guide

Explains capture formats, retention, persistence, export and historical data access.

Authors:
worryzu <worryzu@gmail.com> @LinearTeam

Copyright (C) 2026 Evarentha
SPDX-License-Identifier: GPL-3.0-or-later
-->

# Recording and export

## Choose the right output

| Output | Selection | Contents |
|---|---|---|
| Readable recording | `record_to: "capture.log"` | RX/TX, UTC host time, sequence references, TX sources, markers and boot events |
| Legacy readable recording | `record_to: "capture.txt"` | Same text format; retained for compatibility |
| Structured recording | `record_to: "capture.jsonl"` | Serialized frames with raw bytes, direction, timestamps and decoding |
| Raw receive recording | `record_rx_to: "receive.log"` | RX bytes only, regardless of extension; no inserted timestamp or TX |
| Ordinary export | GUI Export / RPC `export_log` with `format: "txt"` | Retained-memory text snapshot; automatic extension `.log` |
| Issue report | GUI Issue bundle | One `.txt` report containing selected evidence and context |

Readable `.log` normalizes line endings, joins chunks, hides ANSI decoration and escapes binary/control bytes. Use JSONL to preserve payload bytes for replay. Renaming a file does not convert it; GUI history also recognizes older raw/JSONL `.log` captures.

CLI replay and offline decoding read JSONL incrementally. Each record is limited to 16 MiB including LF. Malformed, invalid-UTF-8 and oversized records are counted as skipped; reading resumes at the next line. File IO failures stop the command. The core `read_log` convenience API still returns a full in-memory snapshot.

## Start recording

New GUI connections enable recording by default; choose its directory or disable it before opening. Each new capture uses UTC time, port name and a random ID. Existing active/reconnecting sessions are reused with their original settings: joining them neither starts a new recording nor recovers past unrecorded bytes.

CLI direct monitoring accepts `--record-to capture.log`. MCP `open_port` and RPC `open_session` record only when given a recording path. A `record_to` extension of `.log` or `.txt` selects readable output (case-insensitive); other extensions select JSONL. Readable/raw recorders require a new filename; JSONL appends to an existing file. If a nonempty JSONL file lacks a final LF, opening the recorder appends one before new records. This preserves a complete final record and isolates an interrupted malformed tail, which readers count as skipped. Capture files are opened for reading and appending so rotation can copy the open file even if its pathname changes. Existing files therefore require read and write permission. Sinks are prepared before the device is opened.

`receive --output receive.log` appends the CLI's formatted output, including HEX formatting when requested. Use `record_rx_to` for raw RX bytes.

Default GUI directory: Linux `$XDG_STATE_HOME/flattencom/captures/` or `~/.local/state/flattencom/captures/`; Windows `%APPDATA%/flattencom/captures/`. `FLATTENCOM_STATE_DIR` changes the state root; a saved GUI recording-directory preference can override that default.

## Segments and retention

At a 64 MiB threshold, readable recording continues in `capture-part-000001.log`, `capture-part-000002.log`, etc. Explicit legacy `.txt` recordings keep `.txt` segment names. A single write can exceed the threshold; this is not a strict per-file byte cap.

All readable segments are retained by default. GUI retention **0** means keep all and omits `record_keep_segments`. In RPC, omit/null means keep all; supplying **0** means retain no older segments. A positive count means older segments in addition to the current segment. Settings apply only to new sessions. Legacy JSONL/raw uses four rolling backups, `.1` being newest.

Completed readable segments retain process-local path and identity protection through metadata snapshots, without keeping a file descriptor open for each segment. Retention checks the saved identity, size and timestamps before deleting an old segment; changed or replaced files are preserved.

## Persistence and failures

Service capture continues while the GUI is paused, filtered, cleared or closed. Closing the shared session returns after serial workers stop, queued RX is processed, capture is finalized and the port is released. Restarting the service closes sessions; reopen them explicitly afterward.

The capture worker uses a 64-record queue. Saturation or a write failure stops recording and reports errors through `stats.recording`; serial IO may continue. Multiple configured sinks share this worker's failure domain. Dirty output flushes roughly every 100 ms, not an fsync guarantee; abrupt termination/power loss can lose unwritten data.

Markers and boot annotations go into the same readable file; no automatic JSON sidecars are created. Their bounded live indexes and published selections are not reloaded after daemon restart. The original files remain on disk unless retention or an external operation removes them.

## Export and historical access

GUI Export and RPC exports read the retained memory snapshot, not all capture segments. Use Capture library > Save copy for a selected disk file, or copy all relevant segments for an entire session. GUI history reads local files; MCP `read_frames`/`get_log` expose memory only.

Readable automatic export names end in `.log`; the protocol format remains `txt`, and explicit paths are respected. HEX/CSV/JSONL/PCAP keep their own formats. Service exports and issue reports write a temporary file before atomic publication. Publication rechecks active capture paths and aliases, and rejects a destination whose resolved path changed during export. This protection is process-local; external programs can still modify files.

JSONL replay sends both RX and TX payloads with scaled intervals. Transmission success means a transport write, not device execution. A `.txt` issue report includes only selected text and available related commands/markers, not a complete or byte-exact archive.
