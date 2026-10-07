<!--
flattencom - Protocol Guide

Specifies local JSON-RPC transport, methods, errors, frame cursors and recording behavior.

Authors:
worryzu <worryzu@gmail.com> @LinearTeam

Copyright (C) 2026 Evarentha
SPDX-License-Identifier: GPL-3.0-or-later
-->

# Local RPC protocol v1

## Transport and handshake

UTF-8 NDJSON carries one JSON-RPC 2.0 object per line, with a 16 MiB message limit. Requests use unsigned integer id, method and object params; responses correlate by id, notifications omit id. This implementation does not provide JSON-RPC batch requests. Paths refer to the service host.

The limit includes the terminating newline in both directions. A reply that cannot
fit returns a bounded -32603 error with the original request ID; successful results
are never silently truncated. This also applies to aggregate `list_sessions`
responses: the daemon has no global session-count cap. Oversized notifications are
dropped under the best-effort notification contract. A reply-size error does not
roll back an operation that already completed.

Linux endpoint: `$XDG_RUNTIME_DIR/flattencom.sock`, fallback `/tmp/flattencom-<uid>.sock`. Windows: `\\.\pipe\flattencom.sock`. FLATTENCOM_SOCKET overrides it. The token is in the state directory as daemon.token; FLATTENCOM_STATE_DIR overrides that directory. Defaults are Linux XDG state or ~/.local/state/flattencom, and Windows application data/flattencom. Clients must agree on endpoint and token directory.

```json
{"jsonrpc":"2.0","id":1,"method":"hello","params":{"client":"example","version":"1.0.0","proto":1,"token":"TOKEN"}}
```

`hello` must be first and arrive within five seconds. Its `client` label is limited to 256 UTF-8 bytes. Success returns daemon, version, proto and capabilities. Each request's params can include `_language: "en"` or `"zh-CN"`; omission uses English. This selects application messages; field names and device data stay unchanged. Rust clients can set endpoints explicitly through ConnectConfig.

## Errors and request outcomes

| Code | Meaning |
|---:|---|
| -32700 | Invalid JSON |
| -32600 | Invalid request/version envelope |
| -32601 | Unknown method |
| -32602 | Invalid parameters / workbench validation |
| -32603 | Internal RPC serialization error |
| -32001 | Authentication failed |
| 100 | Port busy |
| 101 | Permission denied |
| 102 | Port not found |
| 103 | Session not found |
| 104 | Session closed |
| 105 | Invalid configuration |
| 106 | Timeout |
| 107 | IO error |
| 108 | Unsupported operation |
| 109 | Decoder error |
| 110 | Internal core error |

Errors contain `code` and `message`. Core errors also include `data.kind` and category-specific details: `path`/`hint` for access failures, `field`/`reason` for invalid configuration, or `message` for string-detail errors such as timeout and IO. Validation errors can omit `data`. Branch on codes and structured fields rather than translated messages.

```json
{"jsonrpc":"2.0","id":2,"error":{"code":-32602,"message":"Invalid parameters"}}
```

A transport timeout/disconnect after dispatch is not proof that a write failed. Do not automatically replay writes. Normal requests wait serially per connection; compatibility send_file/replay_log permit two concurrent job waits. Client filters/subscriptions disappear on disconnect; sessions do not. A new connection does not inherit owner_conn, so closing a session it did not open requires force=true.

## Method reference

`S` below means required session_id; `?` means optional. Results name fields inside result. Canonical DTOs: [methods.rs](../crates/flattencom-proto/src/methods.rs), [workbench.rs](../crates/flattencom-proto/src/workbench.rs); [schemas](../schemas/README.md) cover selected contracts, not all endpoints.

| Method | Parameters | Result |
|---|---|---|
| `hello` | client, version?, proto, token | daemon, version, proto, capabilities |
| `list_ports` | empty object | ports with busy and session usage |
| `get_port_info` | path | port |
| `open_session` | path, baud?, data_bits?, parity?, stop_bits?, flow_control?, read_timeout_ms?, exclusive?, label?, record_to?, record_rx_to?, record_keep_segments?, buffer_max_frames?, buffer_max_bytes? | session, reused |
| `close_session` | S, force?=false | stats after worker shutdown and capture finalization |
| `list_sessions` | empty object | sessions |
| `configure_session` | S, baud?, data_bits?, parity?, stop_bits?, flow_control?, read_timeout_ms?, label? | config |
| `send` | S, exactly one data/hex, newline? | bytes_sent, seq |
| `send_file` | S, path, chunk_size?, pacing_ms? | bytes_sent, frames; waits for job |
| `read_frames` | S, since_seq?, last_ms?, max_bytes?, tail?=false, format?=decoded | frame page |
| `get_log` | S, from_seq?, to_seq?, format?=decoded | frame page |
| `clear_buffer` | S | cleared; shared memory, not GUI-only |
| `flush_rx` | S | cleared; driver input and retained RX |
| `flush` | S | ok; driver TX drain check |
| `set_decoder` | S, spec object or null | ok |
| `get_decoder` | S | spec |
| `list_decoders` | empty object | decoders ID-to-description map |
| `set_filter` | S, filter object or null | active |
| `set_triggers` | S, triggers array | count |
| `get_triggers` | S | triggers |
| `get_trigger_fires` | S | fires; non-destructive read |
| `set_signals` | S, dtr?, rts? | pins |
| `get_signals` | S | pins |
| `send_break` | S, duration_ms?=100 | ok; duration clamped to 1–5000 |
| `get_stats` | S | stats |
| `export_log` | S, format?=txt, path?, all?=false | path, frames, bytes |
| `replay_log` | S, path, speed?=1.0 | frames_sent, bytes_sent, skipped; waits for job |
| `subscribe` | S, kinds? | ok |
| `unsubscribe` | S | ok |
| `daemon_info` | empty object | daemon, version, proto, uptime_ms, sessions, socket, capabilities |
| `shutdown` | empty object | ok; requests service stop and closes sessions |

Additional methods are specified in [Workbench RPC](WORKBENCH-RPC.md): read_sent, export_readable, save_report, add_marker, list_markers, publish_selection, read_selection, revoke_selection, start_transfer, reset_device, operation_status, cancel_operation.

## Session and data semantics

New sessions default to 115200 baud, eight data bits, no parity, one stop bit, no flow control, 10 ms read timeout and exclusive access. Enum spellings are five/six/seven/eight, none/odd/even/mark/space, one/one_point_five/two and none/software/hardware. Linux and Windows support Mark/Space using native driver settings. one_point_five requires five data bits; two requires six/seven/eight. Change data_bits and stop_bits together when switching between these combinations. Invalid combinations return code 105 before changing the port. Driver rejection or readback mismatch returns an error. Valid read timeout is 1–1000 ms. configure_session changes supplied fields; omitted label is unchanged and null clears it.

For example, open a 5-bit Mark-parity session with 1.5 stop bits:

```json
{"path":"/dev/ttyUSB0","baud":9600,"data_bits":"five","parity":"mark","stop_bits":"one_point_five"}
```

The equivalent direct CLI monitor command is:

```bash
flattencom monitor /dev/ttyUSB0 --baud 9600 --data-bits 5 --parity mark --stop-bits 1.5
```

Session labels are limited to 4096 UTF-8 bytes. Open and configure requests validate
this limit before changing the session; a rejected configure request also preserves
the previous serial settings. This limit is separate from the 256-byte hello client label.

An active/reconnecting matching device returns reused=true without changing its settings, owner or captures. Failed/closed entries are not reused. The opening transaction is serialized separately from the session table, which is not held across device IO. Reconnect preserves the same session and sequences; service restart does not.

send requires exactly one data or hex. Text defaults to CRLF; HEX gets no suffix unless newline is explicitly supplied. newline accepts crlf/lf/cr/none. Maximum core payload, including the suffix, is 1 MiB; use start_transfer for larger work. TX records track successful driver writes. Partial writes may leave TX evidence even when the call returns an error.

`clear_buffer` removes retained main-buffer frames. `flush_rx` also clears driver input and excludes pre-flush queued/decoding RX from subsequent memory reads; newly received data can appear immediately. Neither operation deletes recording files. RX already read into the processing queue still reaches the recorder. The returned `cleared` count covers retained frames removed, not driver bytes or pending chunks.

```json
{"seq":12,"dir":"tx","t_us":1790438728748745,"mono_us":2184,"len":6,"hex":"50 49 4E 47 0D 0A","text":"PING\r\n","source":"flattencom-gui#1"}
```

Frames are chunks, not packet/line boundaries. t_us/mono_us are host timestamps; seq reflects insertion order after processing, so mixed RX/TX timestamps need not strictly follow sequence. format raw returns base fields, hex adds bytes, text adds lossy text, decoded also adds decoding. DTR/RTS known flags distinguish cached explicit writes from unreadable initial output states; hardware error counts depend on driver reporting.

## Pagination and filters

since_seq is an inclusive next-unread cursor; omit/zero starts at the retained head. Always reuse returned next_seq. tail selects a bounded newest window; last_ms intersects the sequence window before limits. read_frames defaults to 256 KiB estimated output, clamps requested bytes to 1..1 MiB and returns at most 512 chunks, permitting one oversize chunk. get_log uses a 256 KiB budget and exclusive to_seq; stop at that endpoint even if up_to_date describes the broader buffer.

```json
{"frames":[],"next_seq":42,"dropped_rx":0,"up_to_date":true,"first_seq":0,"last_seq":41}
```

Filtered-out frames advance the cursor. first_seq/last_seq may be null; dropped_rx counts evicted main-buffer chunks, not a hardware RX-loss counter. read_sent has its own limits/eviction semantics in Workbench RPC. A cursor behind the retained head cannot recover evicted bytes unless they were recorded to disk.

set_filter is connection-local; direction, contains/regex, time bounds, decoded fields, all_of and any_of are supported. regex takes precedence over contains. `case_insensitive` applies to text and decoded-field names and values. Decoders/triggers/serial settings are session-wide. Filters never alter recordings. Triggers match visible raw text and decoded text separately for each chunk; a match in either representation fires the trigger once. `responded` records response enqueue acceptance, not an eventual device acknowledgment.

Each trigger requires a nonempty ID of at most 256 UTF-8 bytes and permits at most 32 actions. Each text or decoded HEX response is limited to 1 MiB. The ID, regex, response strings (including HEX whitespace), executable paths and arguments together are limited to 4 MiB per rule. Fire history retains 100 entries with match excerpts of at most 96 Unicode characters; responses and executable arguments are excluded from fire records.

## Notifications and persistence

subscribe kinds: frames, stats, state, trigger_fires; omit/null/empty subscribes to all. Notification methods are ports_changed, session_state, frames, stats, buffer_overflow and trigger_fired. Port/state broadcasts can arrive without an explicit subscription. Bounded queues can drop notifications; cursor reads recover only retained history.

There is no service-side disk-history query in read_frames/get_log. Recording settings, file formats, retention and failure domains are defined in [Recording](RECORDING.md). export_log format values remain txt/hex/csv/jsonl/pcap; automatic readable filenames use .log. Explicit paths are honored. all=true bypasses the calling connection's view filter. Workbench jobs/selection persistence is defined separately.

## Reproducible local example

Run in a POSIX shell and replace SESSION_ID with the session.session_id returned by the first command. Each CLI invocation opens a separate connection; connection-local filters do not carry over between commands.

```bash
./target/release/flattencom rpc open_session '{"path":"virtual://echo"}'
./target/release/flattencom rpc send '{"session_id":"SESSION_ID","data":"PING","newline":"crlf"}'
./target/release/flattencom rpc read_frames '{"session_id":"SESSION_ID","since_seq":0,"format":"decoded"}'
./target/release/flattencom rpc close_session '{"session_id":"SESSION_ID","force":true}'
```

RX can arrive after TX; repeat read_frames with next_seq to retrieve delayed echo. On Windows use your shell's JSON quoting rules; the Rust client handles hello automatically. Physical devices are not required.

Virtual echo returns writes after approximately 1 ms (`?delay_ms=20` changes it); virtual gen accepts `?bps=115200&pattern=ascii` or binary. PCAP v2.4 is little-endian, LINKTYPE_USER0, with direction byte 0=RX/1=TX before each serial payload.
