<!--
flattencom - Development Guide

Documents build dependencies, verification commands, protocol changes and release packaging.

Authors:
worryzu <worryzu@gmail.com> @LinearTeam

Copyright (C) 2026 Evarentha
SPDX-License-Identifier: GPL-3.0-or-later
-->

# Development and packaging

## Requirements

Run commands from the repository root. Rust components require Rust **1.98+** and the platform linker. GUI additionally requires CMake **3.24+**, Ninja, a C++20 compiler and Qt **6.5+** Widgets/Network; Qt Test is needed with BUILD_TESTING enabled (default). CI uses Qt 6.8.3. Project scripts require Python **3.9+** and Git. Rustup selects stable via rust-toolchain.toml; check `rustc --version` meets the minimum.

Linux needs libudev development files and pkg-config. On Debian/Ubuntu install `build-essential`, `libudev-dev`, `pkg-config`, `cmake` and `ninja-build`. A separately installed Qt SDK also needs `libgl1-mesa-dev` and `libxkbcommon-dev`, as installed by CI. Install Qt separately if the distribution's version is older than the minimum. Set CMAKE_PREFIX_PATH for a nonstandard Qt installation.

The explicit CMake commands below work with 3.24+. The format-6 presets in `gui/CMakePresets.json` require CMake **3.25+**.

## Linux build

```bash
cargo build --release --workspace --locked
cmake -S gui -B gui/build/release -G Ninja -DCMAKE_BUILD_TYPE=Release
cmake --build gui/build/release --parallel
ctest --test-dir gui/build/release --output-on-failure
./gui/build/release/flattencom-gui
```

Rust executables are in target/release. For CLI/service/MCP only, the Cargo build is sufficient. Linux serial permissions depend on the device's group; inspect `ls -l /dev/ttyUSB0` rather than assuming a group name. See [Troubleshooting](TROUBLESHOOTING.md#port-access).

## Windows build

Install Visual Studio 2022 Build Tools with Desktop development with C++, MSVC v143 and Windows SDK; Rust MSVC, CMake, Ninja, Python 3.9+, Git and Qt's **MSVC 2022 64-bit** kit. Open **x64 Native Tools Command Prompt for VS 2022**. Adjust the Qt path:

```bat
set "CMAKE_PREFIX_PATH=C:\Qt\6.8.3\msvc2022_64"
set "PATH=%CMAKE_PREFIX_PATH%\bin;%PATH%"
rustup default stable-msvc
rustc --version
cargo build --release --workspace --locked
cmake -S gui -B gui/build/release -G Ninja -DCMAKE_BUILD_TYPE=Release -DFLATTENCOM_BUNDLE_RUST=ON -DFLATTENCOM_DEPLOY_QT=ON
cmake --build gui/build/release --parallel
ctest --test-dir gui/build/release --output-on-failure
cmake --install gui/build/release --prefix build/windows
build\windows\bin\flattencom-gui.exe
```

C++ must match the Qt kit. Use a separate, fresh build directory for each platform/toolchain. Installation collects GUI, Rust executables and Qt runtime. An extracted ZIP must retain all DLLs and platforms/qwindows.dll.

## Checks and test layers

```bash
python3 scripts/check.py
python3 scripts/check-docs.py
cargo run -p flattencom-proto --bin gen-schemas --locked
```

On Windows use `python` instead of `python3`. The full checker runs documentation/localization/header checks, rustfmt, strict Clippy, Rust build/tests, Qt Debug build/CTest and isolated GUI/service smoke. Build daemon binaries before process tests so the tests do not pick up stale executables.

| Layer | Evidence |
|---|---|
| core unit tests | Configuration, buffering, decoding, triggers, capture and worker behavior |
| Linux PTY | Kernel pseudo-terminal IO, settings, throughput and reconnect paths |
| proto fixtures | Rust/Qt shared wire examples and framing limits |
| daemon/MCP e2e | Real service/stdio processes, calls, selections and reconnect behavior |
| Qt tests | Models, selections, history, translation, theme and interaction |
| GUI smoke | Actual Qt → daemon → virtual echo with isolated state |

For release smoke use `python3 scripts/gui-smoke.py --bin-dir target/release --gui gui/build/release/flattencom-gui --lang zh-CN`. Windows accepts the `.exe` GUI path. `cargo check --workspace --all-targets --locked --target x86_64-pc-windows-gnu` checks a cross target after installation; it does not link or run executables. `cargo test --no-run --target ...` links test binaries but still does not run them.

## Package and install

From a Git working copy:

```bash
python3 scripts/package.py --generator TGZ
```

Windows, in the configured developer terminal:

```bat
python scripts/package.py --generator ZIP --deploy-qt
```

Artifacts and SHA256SUMS go to dist/. Packaging runs release builds and Qt tests, not the full Rust/check suite. TGZ without --deploy-qt uses system Qt, not a distribution-independent runtime. MSI requires WiX configured for the installed CMake version. AppImage helpers need linuxdeploy and its Qt plugin; publication/signing is separate.

The source-archive/header scripts enumerate Git files and expect a working copy. With an extracted source archive lacking Git metadata, use Cargo/CMake directly, configure FLATTENCOM_BUNDLE_RUST=ON and then:

```bash
cpack --config gui/build/release/CPackConfig.cmake -G ZIP -B dist
```

Choose TGZ on Linux. Enable FLATTENCOM_DEPLOY_QT when the platform's Qt deployment support is available. If Cargo output is in a target-specific directory, set FLATTENCOM_RUST_BIN_DIR accordingly. Before distribution verify the archive and its checksums, licenses, platform plugin and runtime dependencies. `SHA256SUMS` covers artifacts from the current package invocation; older files in `dist/` are left in place but excluded from that manifest. The source archive version comes from Cargo metadata.

## Version and release tags

`Cargo.toml` is the only place the project version is written. Crates inherit that line, CMake reads it for `project()` and the `FLATTENCOM_VERSION` compile definition that supplies the GUI `--version` output, the About dialog and the service handshake, and the Arch and winget packaging recipes read it too. Nothing repeats the number.

Raise a release number by editing that line, then refresh the two derived records:

```bash
cargo update --workspace
cmake -S gui -B gui/build/release -G Ninja -DCMAKE_BUILD_TYPE=Release
```

Cargo does not rewrite member versions in `Cargo.lock` during metadata queries, and `--locked` builds accept the stale entries, so the lockfile needs the explicit refresh above.

`python3 scripts/check-version.py gui/build/release` rejects a repeated literal in any consumer, a lockfile that still names an older version and a build tree configured with a different version.

Release tags are `v` followed by that exact version, created as annotated tags on an already verified commit. Deleting or moving a published tag breaks that correspondence; a changed codebase is a new version. `.github/workflows/release.yml` builds Linux and Windows artifacts when a `v` tag is pushed and publishes nothing by itself.

## Behavior and limits

`virtual://echo` returns transmitted bytes. `virtual://gen?bps=800000&pattern=binary` produces a binary test stream at the URI's payload bit rate. Changing its configured baud changes that rate; label-only changes and reapplying the current baud preserve it. Virtual transports test software paths, not physical UART timing.

CLI file sends use 4096-byte reads. A file-read failure can occur after earlier chunks were transmitted; the command fails without appending its requested line ending. `--expect` starts its timeout after transmission. Autobaud scores all observed RX bytes using constant-size accumulators; its byte/frame totals cover the sampling interval, while `best_text` contains at most the first 32 printable ASCII characters.

The TUI scrolls rendered rows: Up/Down move one row, PageUp/PageDown move 15, and End follows the newest output. Command feedback remains visible for five seconds independently of live statistics. `watch` checks cancellation at least every 50 ms during interval and retry waits; port enumeration itself remains a platform call.

TUI keyboard interrupts and supported OS termination signals exit through terminal restoration. Native path arguments are preserved through CLI parsing; file paths need not be valid UTF-8 on Unix. Live baud changes update the monitor title after successful configuration.

Linux x64 and Windows x64 are supported. macOS is not a supported target.

The Linux adapter applies native termios framing, including CMSPAR for Mark/Space; Windows applies the DCB framing tuple. Both read settings back and reject silent driver downgrades. 1.5 stop bits require 5 data bits; 2 stop bits require 6–8. Linux PTYs cannot emulate all physical framing modes. Baud/boot recognition is heuristic; Modbus framing is not a hard-real-time 3.5-character timer. Queue overflow and recording errors are reported; recording may stop while serial IO continues. Replay sends both recorded directions and is not a device emulator.

## Contribution and documentation workflow

Keep application/default text and comments in English; translate application messages through catalogs. Maintain English and Chinese READMEs; detailed documentation is English only. Author/SPDX declarations are checked by maintain-headers.py. Do not edit generated messages.rs or schemas by hand; see [Internationalization](I18N.md).

Protocol changes update proto DTOs, daemon dispatch, MCP adapters, Qt parsing, schemas and the protocol reference together. Add defaults for compatible fields and increment PROTOCOL_VERSION for incompatible contracts. Prefer a regression scenario demonstrating the user-visible failure over a test that mirrors implementation.

### Documentation ownership

Keep each topic's main reference in one place:

- README: overview and shortest startup path.
- [GUI](GUI.md): interactive workflows.
- [MCP](MCP.md): bridge configuration and recovery.
- [Recording](RECORDING.md): formats, locations, retention and persistence.
- [Protocol](PROTOCOL.md) and [Workbench RPC](WORKBENCH-RPC.md): wire contracts and limits.
- [Architecture](ARCHITECTURE.md): implementation boundaries.
- This page: build, test and package commands and runtime limits.
- [Troubleshooting](TROUBLESHOOTING.md): symptoms and links to the relevant references.

Update the topic's reference page first, then affected summaries and both READMEs. Use relative links, stable anchors and runnable fenced examples. Link only to files shipped with the project. Application localization and README translation are maintained separately.

### Documentation checks

`python3 scripts/check-docs.py` checks public links/anchors, English-only detailed docs, README section/example parity, JSON examples, RPC method-table coverage against dispatch, selected source defaults, known obsolete claims and CLI entry points. It is included in scripts/check.py and CI.

After building release binaries, `python3 scripts/check-docs.py --smoke` verifies CLI echo and shared RPC examples with an isolated virtual session. It never opens physical ports or uses the existing service. Use `--bin-dir` for another build directory; on Windows use python.
