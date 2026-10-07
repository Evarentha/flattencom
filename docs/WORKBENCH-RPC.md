<!--
flattencom - Workbench Rpc Guide

Documents marker, selection, history, transfer and reset RPC parameters and semantics.

Authors:
worryzu <worryzu@gmail.com> @LinearTeam

Copyright (C) 2026 Evarentha
SPDX-License-Identifier: GPL-3.0-or-later
-->

# Workbench RPC

## Method contracts

Use the authenticated connection described in [Protocol](PROTOCOL.md). Required parameters have no suffix; `?` means optional. Results below are the JSON-RPC result object. File paths refer to the service host. CLI can invoke methods through `flattencom rpc METHOD 'JSON'` in a POSIX shell.

| Method | Parameters | Result |
|---|---|---|
| `add_marker` | session_id, label, seq?, kind? | marker: id/session_id/label/kind/source/t_us/seq |
| `list_markers` | session_id | markers, boots, truncated, markers_omitted, boots_omitted |
| `read_sent` | session_id, since_seq?, max_bytes?, format? | frames, next_seq, dropped_rx, up_to_date, first_seq, last_seq |
| `export_readable` | session_id, path | path, frames; unfiltered memory text snapshot |
| `save_report` | path, text | ok; atomic text publication |
| `publish_selection` | session_id, text, from_seq?, to_seq? | selection: id/session_id/text/from_seq/to_seq/published_us/source |
| `read_selection` | selection_id | selection |
| `revoke_selection` | selection_id | removed (boolean) |
| `start_transfer` | session_id, exactly one path/data/hex, chunk_size?, pacing_ms?, replay?, speed? | operation |
| `reset_device` | session_id, steps, label? | operation, with operation.marker |
| `operation_status` | operation_id | operation |
| `cancel_operation` | operation_id | current operation; cancellation is not necessarily complete |

Unknown RPC methods return -32601. Workbench validation errors use -32602; core failures use the [core error codes](PROTOCOL.md#errors-and-request-outcomes). `read_sent` uses at most 128 chunks, defaults to 65536 estimated bytes, caps the requested budget at 1 MiB and permits one oversized chunk. It ignores receive filters; dropped_rx denotes evicted TX entries for wire compatibility. See [Recording](RECORDING.md) for export protection and formats.

## Transfer, replay and cancellation

[Transfer schema](../schemas/transfer.json) covers start_transfer. Default chunk_size is **1024**, clamped to 1–4096; pacing_ms defaults to zero and caps at 5000. Text is verbatim, so include line endings explicitly. Input paths must be regular files. Only one background operation per session runs at a time; the global job table caps at 128 entries, pruning completed entries when needed.

```json
{"session_id":"SESSION_ID","data":"AT\r\n","chunk_size":1024,"pacing_ms":20}
```

```json
{"session_id":"SESSION_ID","path":"/absolute/capture.jsonl","replay":true,"speed":1.0}
```

`replay=true` requires `path`. Replay streams JSONL records, skips invalid lines, rejects lines over 16 MiB including the newline and sends both recorded directions. It sends payloads in chunks of at most 4096 bytes; replay ignores chunk_size and pacing_ms. Speed must be finite and positive; each recorded interval is divided by speed, with each wait capped at 60 seconds. Compatibility send_file/replay_log use the same engine.

Every replay line is validated as UTF-8, including unknown JSON fields. Invalid
UTF-8 lines increment `skipped` and contribute their consumed bytes to `input_bytes`,
but transmit no payload; subsequent valid records still replay.

Operation fields include id, session_id, source, kind (send/reset/replay), state (running/completed/cancelled/failed), bytes_sent, bytes_sent_exact, input_bytes, frames_sent, skipped, total_bytes, started_us, finished_us when terminal, error on failure, and marker (null except for reset).

For send/replay jobs:

- `input_bytes` counts consumed input, including invalid replay lines. `total_bytes` is the initial file size or inline payload length.
- `bytes_sent` advances after each successful chunk. On a send error, `bytes_sent_exact=false` marks it as a lower bound because the failed write may have transmitted some bytes.
- `frames_sent` counts fully transmitted replay records, or completed chunks for ordinary transfers. A partially cancelled replay record contributes its successful chunks to bytes_sent but does not increment frames_sent.
- `skipped` counts invalid JSONL records. Read failures and oversized lines fail the job.

Reset jobs use the same byte accounting for data/hex actions. Their total_bytes is the sum of those payloads, and frames_sent counts completed payload actions. Signal changes and delays do not increment transfer counters.

Jobs have a five-minute budget. User cancellation, session closure, service shutdown or budget expiry stop later work; budget expiry reports cancelled unless an operation fails first. Waits check approximately every 10 ms, but an in-flight transport/file call may finish first. Closing a GUI progress dialog does not cancel. Poll operation_status until terminal; the immediate cancel response can still be running. Completed state confirms the job finished, not device execution.

## Board reset

steps contains 1–32 objects with dtr/rts booleans, data/hex strings and delay_ms. Unknown keys, wrong types, strings over 4096 bytes and delays above 5000 ms are rejected before execution. A step can contain multiple actions; do not assume data/hex exclusivity here. There is no universal reset polarity or timing.

```json
{"session_id":"SESSION_ID","label":"Board reset","steps":[{"dtr":false,"delay_ms":100},{"dtr":true}]}
```

Use only a sequence matching the target board. A reset marker records the request; subsequent banner detection is separate from successful hardware reset.

## Selections and annotations

Selections contain exact published text, limited to 256 KiB and 32 snapshots. from_seq/to_seq are optional unsigned 64-bit reference numbers; absent or null means unspecified. to_seq is conventionally exclusive; reading never refetches underlying frames. Revoke or service restart invalidates snapshots; closing a session alone does not revoke them. `flattencom-mcp --selection ID` enforces one-ID read scope in that MCP process.

Marker labels cap at 4096 bytes and `kind` at 64 UTF-8 bytes. Absent/null kind defaults to `manual`. An explicit unsigned 64-bit `seq` is used when supplied; otherwise the current buffer tail is used. Each session retains up to 2000 manual/operation notes and 1000 boot events; `list_markers.markers` combines both lists. Each returned array is bounded to 4 MiB of serialized JSON, retaining the newest entries in chronological order. `truncated` indicates omitted entries; `markers_omitted` and `boots_omitted` count omissions from the current retained snapshot.

Marker/boot annotations are written to the same readable recording, without sidecars. Closing the session removes its live notes; published selections remain until revoked or the service restarts. Live indexes are not reloaded from disk after restart. No readable sink means there is no persistent annotation copy. TX/source labels are client declarations plus connection numbers or trigger identities, not authenticated OS usernames.
