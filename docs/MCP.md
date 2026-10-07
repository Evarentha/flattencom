<!--
flattencom - Mcp Guide

Documents MCP configuration, serial debugging tools, resources and selection-scoped analysis.

Authors:
worryzu <worryzu@gmail.com> @LinearTeam

Copyright (C) 2026 Evarentha
SPDX-License-Identifier: GPL-3.0-or-later
-->

# MCP integration

## Configure a client

The client launches flattencom-mcp over stdio; the bridge connects to the same local service as the GUI. GUI is not required to stay open. For clients using mcpServers:

```json
{"mcpServers":{"flattencom":{"command":"/absolute/path/to/flattencom-mcp","args":["--lang","en"]}}}
```

On Windows use an absolute `.exe` path, escaping backslashes in JSON or using forward slashes. Replace the example path with an existing executable; clients use different configuration schemas. **Help > MCP setup** provides a generic snippet, not every client's native configuration. Keep the sibling daemon available or set FLATTENCOMD_BIN. FLATTENCOM_NO_AUTOSPAWN=1 disables automatic startup.

Use `--lang zh-CN` for Chinese descriptions/prompts. Tool names and JSON keys stay stable. Restart the bridge to change its startup language. Missing translations and external diagnostics may remain English.

## First session

1. Call list_ports and list_sessions. Reuse a connected session rather than opening the device independently.
2. Call open_port with path/baud_rate when necessary. New recording needs explicit record_to; existing sessions retain their settings. See [Recording](RECORDING.md).
3. Compare send results, read_sent and read_frames. Pass returned next_seq as the next since_seq.
4. Set a decoder or connection-specific filter as needed. Serial settings/decoders/triggers are shared, filters are connection-local.
5. Inspect get_stats and recording errors; use close_port only when the shared session should end.

Example tool arguments (replace session IDs; these are not complete MCP envelopes):

```json
{"path":"virtual://echo","baud_rate":115200,"record_to":"/absolute/new/capture.log"}
```

```json
{"session_id":"SESSION_ID","data":"AT","newline":"crlf"}
```

```json
{"session_id":"SESSION_ID","since_seq":0,"max_bytes":65536,"format":"decoded"}
```

The examples call open_port, send and read_frames in that order. Echo returns the transmitted bytes. send requires exactly one of data/hex; verify the device's response separately from the TX result.

## Tools, resources and prompts

tools/list is the runtime schema authority. Current groups cover discovery/session management, IO, decoders/filters/triggers, control lines, statistics/export/replay, markers/selections, background operations and ESP32 flashing. Some names differ from RPC: open_port → open_session, configure_port → configure_session, close_port → close_session. Not every daemon method is exposed as an MCP tool.

| URI | Contents |
|---|---|
| `flattencom://ports` | Physical port metadata and session usage |
| `flattencom://sessions` | Session summaries |
| `flattencom://decoders` | Decoder catalog |
| `flattencom://sessions/{id}/info` | One session summary |
| `flattencom://sessions/{id}/log` | Bounded recent memory frames |
| `flattencom://sessions/{id}/stream` | Alias of log snapshot, not an unbounded byte stream |
| `flattencom://sessions/{id}/stats` | Statistics snapshot |

Legacy resources/subscribe and rmcp listen are supported by periodic snapshot comparison. An initialized legacy peer also receives resource-list change notifications when sessions open, close or change labels, without needing a URI subscription. Notifications indicate change, not delivery of all frames. Use cursor reads for retained history; disk history requires local file access outside these resources.

Prompts are instructions, not automatically executed workflows: analyze-selection, serial-debug-wizard, baud-detect, modbus-analyzer and firmware-flash-esp32. The flashing prompt directs use of flash_esp32 with installed esptool and the firmware's offset map. Release the port first; ordinary send_file is not a flashing protocol.

## Backend recovery

MCP stdio, backend RPC, serial state and recording state are separate. A running bridge reconnects on the next call/resource poll after backend failure; concurrent callers share a connection attempt, with a 10-second budget and one-second failure cooldown. Initial startup still requires a successful backend connection.

Only explicitly allowlisted read queries may retry once after an interrupted RPC. Writes/reset/configuration/transfers/replay are not replayed when the outcome is unknown. A not-yet-dispatched operation may reconnect before its first send. Subscriptions retain their polling registrations; lost sessions/selections are not recreated, and connection filters need reconfiguration.

daemon_info adds backend_connection with connected, generation, connected_at_ms, last_disconnect (at_ms/reason) and last_reconnect_error. During an outage the call itself can fail; stderr retains transport/reconnect observations. They identify the observed EOF/IO/framing event, not necessarily its original external cause. See [Troubleshooting](TROUBLESHOOTING.md#backend-connection).

<a id="selected-evidence"></a>
## Selected evidence

GUI Share selection publishes an immutable text snapshot and copies its ID/instructions. Reading it does not fetch surrounding frames. Limits, revocation and persistence are defined in [Workbench RPC](WORKBENCH-RPC.md).

```bash
flattencom-mcp --selection SELECTION_ID
```

This separate bridge enforces access only to read_selection for that ID. Other daemon reads, resources and device operations are rejected; normal MCP connections retain their full tools. A prompt asking for limited reading alone is not an access restriction. Service restart invalidates snapshots.

## Plugins and operations

Example set_decoder arguments for WASM:

```json
{"session_id":"SESSION_ID","spec":{"name":"wasm","options":{"path":"/absolute/path/decoder.wat","fuel":1000000}}}
```

`json_lines` accepts lines up to 64 KiB; NMEA accepts lines up to 4096 bytes. These limits exclude LF but include CR. An oversized line is discarded through its next LF, after which decoding resumes. Live sessions and offline CLI decoding keep RX and TX decoder state separate.

`cmd` options use program, args and timeout_ms. Each JSONL input needs one JSON/null output line, at most 1 MiB including the newline. Flush stdout after each reply. The default timeout is 200 ms, clamped to 1–2000 ms. Timeout, EOF or an oversized response disables the instance. Process plugins run as ordinary local programs under the service account. See [cmd.rs](../crates/flattencom-core/src/decode/cmd.rs) for input/output fields.

Plugin cleanup terminates its Unix process group or Windows Job Object and joins its IO worker. Unix pipe operations are cancellable even if a descendant leaves the group; that escaped process is outside group cleanup. Process plugins are not sandboxed.

WASM accepts `.wasm` or `.wat` files up to 16 MiB, has no imports/WASI and limits linear memory to 16 MiB. At most one table is allowed, with at most 65,536 elements at instantiation or after growth. Fuel defaults to 1,000,000 and is clamped to 1000–10,000,000 per call. Output is limited to 1 MiB; a trap or invalid output disables that decoder instance. RX and TX keep separate guest state. ABI: [wasm.rs](../crates/flattencom-core/src/decode/wasm.rs); example: [decoder.wat](../examples/decoder.wat).

start_transfer, operation_status, cancel_operation and reset_device provide progress/cancellation. Use replay=true and a JSONL path for a cancellable replay. Cancellation stops later work; a current write may complete. See [Workbench RPC](WORKBENCH-RPC.md) for the precise contract.

### ESP32 flashing

Install esptool for the bridge's Python interpreter (`python3` on Linux, `python` on Windows), or set `FLATTENCOM_ESPTOOL_BIN` to an esptool executable. Release the shared session before calling `flash_esp32`. Supply port, firmware_path and the offset from the firmware build's image map; baud_rate defaults to 460800. Each call writes one image.

The helper has a 300-second timeout and checks the process exit status. It retains the first 32 KiB from each of stdout and stderr while draining both streams; truncated streams are marked. This output may omit later verification messages.
