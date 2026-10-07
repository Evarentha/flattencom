<!--
flattencom - Troubleshooting Guide

Diagnoses backend, port, recording, deployment and display problems with reproducible checks.

Authors:
worryzu <worryzu@gmail.com> @LinearTeam

Copyright (C) 2026 Evarentha
SPDX-License-Identifier: GPL-3.0-or-later
-->

# Troubleshooting

## Identify the failing layer

Check four states separately: MCP stdio, client-to-daemon RPC, serial connection and recording. A connected MCP client does not prove the backend/port/disk is healthy. Start with read-only diagnostics; stopping the daemon closes every shared session.

With binaries on PATH:

```bash
flattencom --version
flattencom daemon status
flattencom sessions
flattencom daemon logs --lines 80
flattencom list
```

daemon status does not start a missing service; sessions may auto-start unless disabled. In a checkout use `./target/release/flattencom`; Windows uses flattencom.exe. Preserve exact errors and timestamps when comparing layers. Avoid repeatedly sending a command whose first result is unknown.

<a id="backend-connection"></a>
## Backend connection fails

1. Check daemon status and logs, and whether flattencomd is beside the client or on PATH; FLATTENCOMD_BIN can provide its absolute path.
2. Compare FLATTENCOM_SOCKET and FLATTENCOM_STATE_DIR across GUI/CLI/MCP. Using another token directory can cause authentication failure even if the socket exists.
3. If `FLATTENCOM_NO_AUTOSPAWN` is set, run `flattencomd --foreground` in another terminal with the same endpoint/state environment. Alternatively, clear the variable before running `flattencom daemon start`; that command uses the autospawn path too.
4. A running MCP bridge retries its backend connection on subsequent calls/resource polls. Inspect daemon_info.backend_connection after recovery and stderr for disconnect/reconnect details. Restart the bridge when replacing its executable so it loads the new binary.
5. After service restart, use list_sessions and reopen deliberately: old session IDs and selection IDs are not restored. Reapply connection filters.

Do not delete daemon.token while the service is running or share its contents in an issue. A deliberately stopped service may be restarted by clients with autospawn enabled. See [MCP recovery](MCP.md#backend-recovery).

<a id="port-access"></a>
## Port busy, denied or missing

Use `flattencom list` and check OS device enumeration. If the service already owns the port, use GUI shared sessions or `flattencom attach SESSION_ID`, not direct monitor/send/receive. Another terminal/flasher can hold the physical port exclusively.

On Linux inspect `ls -l /dev/ttyUSB0`, then grant access through the actual group (often dialout or uucp) and sign in again. Do not change every serial device to world-writable as a default fix. On Windows verify the USB-serial driver and current COM number in Device Manager. Unique USB identities can survive renumbering; devices with absent/duplicate serial IDs may need manual path selection.

## No output or garbled text

Confirm the device is transmitting and compare baud, data bits, parity, stop bits and flow control with its documentation. Inspect HEX/chunk view to separate byte reception from text decoding. TX success is not proof of a recognized command; check the selected outgoing line ending. Changing CRLF in the send controls does not repair RX decoding.

Use `flattencom send virtual://echo AT --expect AT` as a hardware-free transport check; it cannot validate wiring, voltage levels or the board protocol. Baud detection scores text and cannot identify arbitrary binary or silent streams. For 1.5 stop bits select 5 data bits; for 2 stop bits select 6–8. A framing readback error means the driver did not retain the requested combination; check the adapter's supported modes. Reconnect failures and recording errors should be investigated separately.

## Missing history or recording failure

Check GUI recording status or get_stats.stats.recording for active/errors and the actual paths. Joining an existing session retains its original recording settings. Pause display does not stop recording, but no recording can recover bytes from before it started.

MCP frame reads and ordinary exports use bounded memory. Older data requires saved files; search/load through the GUI or read files directly. Verify free space and write permission. A full capture queue or sink error stops recording visibly; a running serial session does not imply active recording. Do not overwrite an active capture when exporting. See [Recording](RECORDING.md).

## Windows startup and Qt libraries

Extract the entire Windows ZIP; keep Qt6Core/Gui/Widgets/Network DLLs, the compiler runtime and platforms/qwindows.dll together. A missing DLL or platform-plugin error is a deployment failure before serial communication. Do not mix Qt versions, architectures or MSVC/MinGW DLLs. For native builds follow [Windows build](DEVELOPMENT.md#windows-build) and deploy from the matching Qt kit. Set QT_DEBUG_PLUGINS=1 temporarily to inspect plugin lookup.

Cross-compilation and dependency checks do not establish Windows runtime support for a particular machine. Report the Windows version, x64 architecture, complete error and package identity. Linux TGZ without bundled Qt instead requires compatible system Qt libraries.

## Font diagnostics and layout

`qt.text.font.db: OpenType support missing ...` at info level is Qt font-fallback diagnostics, not a port-disconnect error. Current GUI suppresses this info category but retains warnings/errors. To investigate:

```bash
QT_LOGGING_RULES='qt.text.font.db.info=true' ./gui/build/release/flattencom-gui
```

Check installed fonts for the actual script if glyphs are missing; do not infer that every listed font is broken. Record the triggering text and whether it came from a live or saved log. **View > Reset dock layout** repairs a hidden/misplaced panel. Theme and Language menus change appearance without restarting sessions.

## Report a reproducible problem

Include application/package version, OS/architecture, Qt version if self-built, direct/shared mode, serial settings, expected/actual behavior, error text and timestamps. A minimal virtual/PTY reproduction helps isolate software; otherwise describe the hardware/driver. Export an issue report from a selected range if appropriate and review it for credentials or private device data before sharing. Do not include daemon.token. Distinguish observed results from guessed causes.
