<!--
flattencom - Architecture Guide

Explains component boundaries, capture data flow, concurrency and local access control.

Authors:
worryzu <worryzu@gmail.com> @LinearTeam

Copyright (C) 2026 Evarentha
SPDX-License-Identifier: GPL-3.0-or-later
-->

# Architecture

## Processes and dependencies

GUI, CLI attach/rpc and MCP access service-owned sessions through local JSON-RPC 2.0/NDJSON. CLI monitor/send/receive/autobaud/replay use core directly. MCP clients connect to the bridge over stdio; GUI history reads local files; ESP32 support launches external esptool.

```text
GUI / CLI attach / MCP --> daemon --> core --> transport --> device
CLI direct ------------------------> core --> transport --> device
GUI history -------------------------------------------> local files
MCP flash_esp32 ---------------------------------------> esptool
```

Core is synchronous. Proto depends on core and includes DTOs and the Rust IPC client. Qt uses QLocalSocket and handwritten JSON; it has no Rust FFI dependency. Five Rust crates and one Qt build project produce four user-facing executables, plus the schema-generation utility.

## Frames and processing order

A Frame is a transport chunk, not necessarily a complete packet or line. Raw data and decoded results are Arc-shared; seq is allocated under the session lock, shared across RX/TX and retained on reconnect. t_us is wall time; mono_us is session-monotonic time.

RX samples time after driver reads and allocates sequence after queued processing/decoding; TX samples after writing, then decodes and allocates. Across directions, sequence reflects insertion order, not necessarily strict sampling-time order. Views, transcript recording and protocol decoders maintain separate reassembly states.

## Workers and synchronization

Each session has a reader, RX processor, writer and optional capture worker; the daemon adds a pump. The transport mutex protects one shared handle; inner protects buffers/counters/triggers; both direction-specific decoders share a separate decoder mutex.

RX chunks and TX writes carry a connection generation. Reconnect starts new decoder state in both directions without clearing retained history. If a write partially succeeds before the connection changes, its successful prefix is recorded and the remaining bytes are not sent on the replacement connection.

RX connection boundaries also finalize readable transcript fragments and reset ANSI/line-ending and boot-parser state in receive-processing order. Boot event history and counts remain available. Late TX completion from the old connection does not reset the new RX parser. TX flush binds all retries to its starting connection and fails if that connection changes.

The reader submits at most 4096-byte chunks through a 256-entry queue; overload fails the session. The capture worker uses 64 entries and performs disk IO outside inner; saturation or any sink failure stops recording with visible errors while serial IO may continue. Concurrent close callers wait for the same shutdown completion, including RX drain and capture finalization. Worker joins run outside inner; stalled storage can delay shutdown.

Terminal failure also drains workers, finalizes capture and releases decoder processes. The failed session remains inspectable; its failure state can become visible before cleanup finishes. Explicit close waits for completion and retains the decoder specification, but rejects installing new decoder instances.

Commands enter via try_send with deadline/cancellation state. The writer rechecks expiry and session closure after acquiring the transport mutex, before side effects. An OS call already running can still finish. TX flush queries pending driver bytes within one shared deadline. RX flush advances a generation under the transport lock: earlier queued chunks are recorded but excluded from the receive buffer. The RX decoder is rebuilt when the first new-generation chunk arrives, preventing old fragments from entering post-flush decoded output; TX decoder state is preserved. Trigger responses enqueue nonblockingly; trigger-generated TX does not recursively evaluate TX triggers. Device echoes are new RX and can match triggers again.

Ordinary requests await spawn_blocking serially per connection. Compatibility send_file/replay_log permit at most two concurrent waits and use the cancellable five-minute job engine. A separate opening lock serializes opens without holding the session table over slow IO. Closing a session requests cancellation of its jobs.

## Buffers, persistence and views

The main FrameStore defaults to 100,000 chunks/64 MiB; TX history has 2000 chunks/8 MiB. Byte accounting includes payload, TX source, decoded strings and field storage; it is a buffer budget rather than a measurement of process memory. Each store retains one oversized frame even when it exceeds its byte budget. Cursors are inclusive, advance over filtered frames and expose eviction counts. See [Protocol](PROTOCOL.md) for contracts and [Recording](RECORDING.md) for file semantics.

The daemon pump batches every 32 ms and updates statistics every second, filtering per connection. Full notification queues may drop events; cursors recover only retained data. GUI primarily polls. SessionController owns request lifetimes and some query coalescing; SessionPane still owns most display state.

SessionHistory uses QThread indexing and three-page windows with generation guards. CaptureViewer is a separate byte-paged reader. There is no unified service-side disk-history API. MCP subscriptions compare resource snapshots periodically rather than binding directly to the daemon pump.

## Identity, connections and lifetimes

Matching Connected/Reconnecting sessions are reused atomically with their owner, settings and paths. Linux symlinks and Windows COM aliases are normalized. Hardware identity requires USB VID/PID and a unique serial; ambiguous matches are not selected arbitrarily.

The daemon owns sessions, so client disconnect does not stop capture. Filters/subscriptions belong to conn_id; filters need reconfiguration after reconnect. MCP Backend provides serialized reconnect and controlled read retry; DaemonClient itself does not reconnect transparently, and TUI attach does not inherit MCP recovery. See [MCP](MCP.md).

Marker/boot indexes, selections and job state are in memory; recording does not reload them on restart. Revoke or service restart invalidates selections; closing a session alone does not revoke its snapshots. Qt QSettings independently stores layout, language, theme and profiles.

## Access and language boundaries

Linux socket/token mode is 0600. Windows Named Pipes reject remote clients and require a local token. Authenticated clients share IO/settings; closing another owner's session needs force. TX source combines a declared client label (at most 256 UTF-8 bytes) with a connection number; it does not identify an OS user. Process decoders/external triggers run with service-user privileges; WASM has no host imports.

capture_sink tracks reserved paths and open-file identities within the process, protecting active captures across aliases and renames. Its registry does not coordinate with external processes. Selection scope is enforced at MCP RPC entry. _language is scoped to the blocking request thread and does not automatically propagate to new workers. Protocol fields, payload bytes and user labels are not translated.

## Reading and change entry points

Read core/frame, config and store first; then transport and session open/reader/process_received/writer/close; then capture_worker/record/transcript/capture_sink. Continue through proto → daemon state/dispatch/workbench → GUI RpcClient/SessionPane or MCP backend/server/tools.

Worker ordering, cancellation, recording and reconnection changes need failure scenarios and cross-client regression tests, not only successful function returns. See [Development](DEVELOPMENT.md#contribution-and-documentation-workflow) for protocol and documentation updates.
