#!/usr/bin/env python3
# flattencom - Authorship Header Maintenance
#
# Adds or validates file-specific author notices while preserving original file bodies.
#
# Authors:
# worryzu <worryzu@gmail.com> @LinearTeam
#
# Copyright (C) 2026 Evarentha
# SPDX-License-Identifier: GPL-3.0-or-later

"""Maintain file-specific authorship notices without rewriting file bodies.

Run with --write to add missing headers. Without arguments, validate coverage.
Only non-ignored project files are included; build outputs, runtime captures and
third-party caches are excluded. Titles and summaries below describe the reviewed
responsibilities of each file. Existing comments, documentation, shebangs and data
bytes are preserved.

Files that cannot carry a comment, such as JSON data, raster artwork, the Cargo
lockfile and the verbatim license text, carry no separate declaration. The
repository LICENSE file is their declaration.
"""
import argparse
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[1]
AUTHOR = 'worryzu <worryzu@gmail.com> @LinearTeam'
SPDX = 'GPL-3.0-or-later'

DESCRIPTIONS = {
    'gui/src/AboutDialog.h': ('About Dialog Interface', 'Declares the branded application information dialog with theme-aware artwork.'),
    'gui/src/AboutDialog.cpp': ('About Dialog', 'Displays the final wordmark, application attribution and license links.'),
    'gui/resources/application.rc': ('Windows Application Icon Resource', 'Embeds the final multi-resolution application icon in the Windows executable.'),
    'crates/flattencom-cli/tests/tui_pty.rs': ('CLI Terminal Process Tests', 'Verifies signal cleanup and live configuration display using isolated PTYs.'),
    'crates/flattencomd/src/wire.rs': ('Bounded Daemon Wire Output', 'Serializes bounded replies and best-effort notifications for the local service.'),
    'scripts/check-docs.py': ('Documentation Verification', 'Checks English reference docs, bilingual READMEs, links, examples and source contracts, with optional isolated RPC smoke validation.'),
    'README.zh-CN.md': ('Chinese Readme Guide', 'Introduces the project components, startup commands and documentation links.'),
    'crates/flattencom-mcp/src/backend.rs': ('MCP Backend Connection', 'Serializes backend reconnection and retries only explicit read-only operations while retaining disconnect diagnostics.'),
    'crates/flattencom-proto/src/workbench.rs': ('Workbench Transfer Contracts', 'Defines shared cancellable transfer and replay parameters for service and MCP clients.'),
    'crates/flattencom-core/src/capture_sink.rs': ('Capture Output Protection', 'Protects active capture identities and publishes complete exports atomically.'),
    'crates/flattencom-core/src/capture_worker.rs': ('Capture Writer Worker', 'Writes capture records on a bounded worker queue and reports disk failures.'),
    'gui/src/SessionController.cpp': ('Session Request Controller', 'Scopes request callbacks to session lifetimes and coalesces periodic status requests.'),
    'gui/src/SessionController.h': ('Session Request Controller Interface', 'Declares session-scoped requests and periodic query coordination.'),
    '.clang-format': ('C++ Formatting Rules', 'Configures LLVM-based indentation, line width and control-statement formatting.'),
    '.clippy.toml': ('Rust Lint Thresholds', 'Sets Clippy argument-count thresholds alongside the workspace lint policy.'),
    '.rustfmt.toml': ('Rust Formatting Rules', 'Configures Rust line width and small-item formatting heuristics.'),
    '.gitignore': ('Repository Ignore Rules', 'Excludes build outputs, editor files and runtime serial data from source control.'),
    '.github/workflows/ci.yml': ('Continuous Integration', 'Checks Rust, Qt, protocol schemas and virtual serial behavior on supported runners.'),
    '.github/workflows/release.yml': ('Release Artifact Workflow', 'Builds and uploads Linux and Windows release packages after verification.'),
    'Cargo.toml': ('Rust Workspace Manifest', 'Defines shared packages, dependencies, lint settings and release build configuration.'),
    'rust-toolchain.toml': ('Rust Toolchain Selection', 'Selects the compiler channel, formatting tools and lint components.'),
    'README.md': ('Project Overview', 'Introduces components, build commands, language selection and MCP integration.'),
    'gui/CMakeLists.txt': ('Qt Build and Packaging Configuration', 'Builds the Qt client and tests, installs resources and configures CPack artifacts.'),
    'gui/resources/resources.qrc': ('Qt Resource Manifest', 'Embeds the keyword rules, Chinese translations and final branding artwork.'),
    'examples/decoder.wat': ('WebAssembly Decoder Example', 'Implements the decoder ABI exports and returns a sample decoded JSON result.'),
    'gui/tests/models.cpp': ('Qt Regression Tests', 'Verifies models, translation, selection, serial interactions, disk history and inspector behavior.'),
    'gui/src/main.cpp': ('Qt Application Entry Point', 'Initializes application settings, localization and the main window with isolated smoke-test support.'),
    'gui/src/SessionAnalysis.cpp': ('Session Analysis Actions', 'Implements evidence selection, markers, reset operations, timing and issue-report export.'),
    'packaging/README.md': ('Packaging Overview', 'Describes platform packaging inputs and the release artifact generation workflow.'),
    'packaging/windows/README.md': ('Windows Packaging Guide', 'Documents Qt runtime deployment, installer generation and Windows release requirements.'),
    'packaging/arch/PKGBUILD': ('Arch Linux Package Recipe', 'Builds Rust and Qt components and installs them through makepkg and CMake.'),
    'packaging/linux/flattencom.desktop': ('Desktop Application Entry', 'Registers the GUI executable, icon and development category with desktop launchers.'),
    'packaging/linux/flattencom.svg': ('Application Icon', 'Defines the scalable desktop icon used by Linux launchers and application bundles.'),
    'packaging/systemd/flattencomd.service': ('User Service Unit', 'Runs the serial background service under the user systemd service manager.'),
    'packaging/winget/generate.py': ('Winget Manifest Generator', 'Hashes an MSI and emits installer metadata using its release URL and version.'),
    'schemas/README.md': ('Protocol Schema Guide', 'Explains the generated JSON Schemas and the command used to regenerate them.'),
    'scripts/maintain-headers.py': ('Authorship Header Maintenance', 'Adds or validates file-specific author notices while preserving original file bodies.'),
    'scripts/appimage.sh': ('AppImage Packaging Helper', 'Installs a built application into AppDir and invokes linuxdeploy with its Qt plugin.'),
    'scripts/benchmark.py': ('Capture Benchmark Runner', 'Builds the synthetic capture benchmark and records throughput and child-process CPU usage.'),
    'scripts/check-i18n.py': ('Translation Coverage Checker', 'Checks catalog duplicates, source message coverage and formatting placeholders.'),
    'scripts/check-version.py': ('Version Source Verifier', 'Confirms Cargo.toml is the only declared project version and that every consumer derives it.'),
    'scripts/check.py': ('Project Verification Runner', 'Runs formatting, Rust checks, tests and Qt virtual-port verification with failure propagation.'),
    'scripts/gen-messages.py': ('Rust Translation Macro Generator', 'Generates literal checked format branches from the Chinese message catalog.'),
    'scripts/gui-smoke.py': ('Qt Serial Smoke Test Runner', 'Starts isolated service and GUI processes to verify virtual-port communication and recording.'),
    'scripts/package.py': ('Release Package Builder', 'Builds release binaries, runs Qt tests and generates CPack archives and SHA-256 checksums.'),
}

GUI = {
 'CaptureViewer': ('Capture File Viewer', 'Pages through disk captures and searches file contents with bounded display memory.'),
 'DeviceProfiles': ('Device Profile Storage', 'Keys persistent device settings by USB identity or fallback port path.'),
 'FramesModel': ('Serial Frame Table Model', 'Stores bounded frame history and projects timestamps, bytes, decoding and source columns.'),
 'Icons': ('Application Vector Icons', 'Draws palette-aware toolbar artwork independently of desktop icon themes.'),
 'InspectorDetails': ('Record Inspector', 'Displays complete records as JSON or translated fields with search and exact-value copying.'),
 'IssueReport': ('Readable Issue Report', 'Formats selected evidence, configuration, transmitted commands and markers as a text report.'),
 'LogManager': ('Capture Library Dialog', 'Lists recorded files and exposes paged reading, path copying and file-copy actions.'),
 'LogRules': ('Log Syntax Highlighting', 'Loads, validates and applies prioritized keyword and regular-expression highlighting rules.'),
 'MainWindow': ('Desktop Workspace', 'Coordinates menus, toolbars, device discovery, shared session tabs and dockable inspectors.'),
 'RateChart': ('Serial Transfer Rate Chart', 'Plots bounded RX and TX rate samples using the current application palette.'),
 'Recording': ('Recording Preferences', 'Builds unique text capture paths and applies persisted recording and retention preferences.'),
 'RpcClient': ('Qt Background Service Client', 'Handles local JSON-RPC authentication, request callbacks, notifications and reconnection.'),
 'RuleEditors': ('Graphical Configuration Editors', 'Edits highlighting, reset steps, triggers and decoder plugin settings through forms.'),
 'SentLog': ('Transmission History Drawer', 'Displays bounded sent-command history with filtering, copying and sequence navigation.'),
 'SessionHistory': ('Session Disk History Index', 'Indexes capture segments on a worker thread for bounded history windows and cancellable searches.'),
 'SessionPane': ('Serial Session Workspace', 'Coordinates live and historical receive views, display pause, sending and session actions.'),
 'Style': ('Qt Presentation and Localization', 'Provides themes, translated UI labels, protocol-value labels and application path discovery.'),
 'TextStreamView': ('Continuous Receive Text View', 'Reassembles UTF-8 and line endings across chunks while retaining searchable sequence anchors.'),
}
for stem, (title, summary) in GUI.items():
    DESCRIPTIONS[f'gui/src/{stem}.cpp'] = (title, summary)
    DESCRIPTIONS[f'gui/src/{stem}.h'] = (title + ' Interface', 'Declares the types and operations for the ' + title.lower() + ' component.')

CORE = {
 'autobaud':'Scores received text and ranks candidate baud rates for caller-managed detection.',
 'boot':'Recognizes fragmented boot banners and groups normal bootloader-to-kernel transitions.',
 'config':'Defines serial settings, buffer limits, reconnect policy and validated configuration patches.',
 'discovery':'Enumerates physical ports, resolves USB identities and polls device connection changes.',
 'error':'Defines stable error categories, localized messages and port-access recovery hints.',
 'filter':'Compiles directional, text, time and decoded-field filters without altering captured data.',
 'frame':'Defines shared immutable RX/TX frames, timestamps, decoding results and byte serialization.',
 'i18n':'Selects process or request-scoped language and retrieves literal catalog translations.',
 'ids':'Creates and parses UUIDv7 identifiers for serial sessions.',
 'lib':'Exports the synchronous serial engine modules and their shared public abstractions.',
 'messages':'Provides generated literal formatting branches for localized application messages.',
 'record':'Writes readable, raw and structured captures and supports retention, export and replay input.',
 'session':'Owns serial reader and writer threads, reconnects, frame history, recording and trigger execution.',
 'stats':'Tracks rolling transfer rates, error categories and consistent session statistics.',
 'store':'Maintains byte- and count-bounded frame buffers with inclusive read cursors and eviction accounting.',
 'timefmt':'Formats wall-clock and monotonic microsecond timestamps for display and export.',
 'transcript':'Renders readable RX lines, escaped TX commands and timestamped annotations across chunks.',
 'trigger':'Evaluates rate-limited frame matches and prepares responses or external program actions.',
 'decode/builtin':'Decodes hexadecimal dumps, visible text, streaming UTF-8 and JSON lines.',
 'decode/cmd':'Runs JSONL decoder subprocesses with bounded queues, timeouts and failure isolation.',
 'decode/mod':'Defines streaming decoder interfaces, specifications and the built-in decoder registry.',
 'decode/modbus_rtu':'Validates Modbus RTU CRCs and interprets requests, responses and exception fields.',
 'decode/nmea0183':'Reassembles NMEA sentences, parses fields and verifies XOR checksums.',
 'decode/wasm':'Hosts decoder modules with a bounded WebAssembly memory and instruction budget.',
 'transport/mock':'Provides scripted transport reads, write capture and deterministic fault injection.',
 'transport/mod':'Defines serial transport operations and routes physical or virtual connection factories.',
 'transport/serial_impl':'Adapts serialport I/O, settings, control lines and driver errors to the core transport interface.',
 'transport/virtual_port':'Implements virtual echo and rate-controlled text or binary generator transports.',
}
for stem, summary in CORE.items():
    DESCRIPTIONS[f'crates/flattencom-core/src/{stem}.rs'] = ('Core ' + stem.replace('/', ' ').replace('_', ' ').title(), summary)

CLI = {
 'cmd':'Dispatches CLI commands and maps core or service errors to process exit codes.',
 'main':'Declares CLI arguments, selects language and initializes command dispatch and logging.',
 'tui/mod':'Renders the terminal monitor and handles navigation, filtering and interactive send commands.',
 'tui/source':'Adapts direct engine sessions and service-backed sessions to one terminal data-source interface.',
 'cmd/attach':'Selects a shared service session and launches its interactive terminal monitor.',
 'cmd/autobaud':'Samples candidate baud rates and reports received-text quality estimates.',
 'cmd/daemon':'Starts, stops and inspects the background service and reads its recent log entries.',
 'cmd/decode':'Decodes saved JSONL captures offline or lists available decoder implementations.',
 'cmd/list':'Prints physical port metadata as a table or machine-readable JSON.',
 'cmd/receive':'Streams received serial bytes to stdout or a file until timeout or interruption.',
 'cmd/replay':'Sends recorded frames to a direct serial connection with scaled timing.',
 'cmd/send':'Builds text, hexadecimal or file payloads and optionally waits for a matching response.',
 'cmd/sessions':'Retrieves shared session summaries and displays their state and ownership.',
 'cmd/watch':'Polls serial device changes and emits connection or removal events.',
}
for stem, summary in CLI.items():
    DESCRIPTIONS[f'crates/flattencom-cli/src/{stem}.rs'] = ('CLI ' + stem.replace('/', ' ').replace('_', ' ').title(), summary)

MODULES = {
 'flattencom-proto/src/lib':'Exports local RPC types, framing, endpoint discovery and service client APIs.',
 'flattencom-proto/src/client':'Connects and authenticates clients, correlates RPC replies and starts the service when needed.',
 'flattencom-proto/src/framing':'Reads bounded UTF-8 NDJSON messages and rejects oversized or incomplete frames.',
 'flattencom-proto/src/methods':'Defines JSON-RPC methods, parameter types, responses, notifications and frame projections.',
 'flattencom-proto/src/socket':'Resolves local socket, named-pipe, token and state-directory paths on supported platforms.',
 'flattencom-proto/src/bin/gen-schemas':'Generates public JSON Schemas from the Rust RPC request, response and session types.',
 'flattencomd/src/main':'Starts the authenticated local service and manages connections, discovery and shutdown.',
 'flattencomd/src/dispatch':'Routes RPC methods to shared session operations and serializes results and errors.',
 'flattencomd/src/state':'Coordinates session ownership, device reuse, per-client filters and bounded event delivery.',
 'flattencomd/src/workbench':'Manages annotations, immutable selections and cancellable transfer or reset operations.',
 'flattencom-mcp/src/main':'Starts the localized stdio MCP server and connects it to the serial background service.',
 'flattencom-mcp/src/server':'Exposes MCP tools, prompts and resources with subscription support and selection-only scope checks.',
 'flattencom-mcp/src/tools':'Defines MCP tool schemas and adapts tool calls to serial service RPC operations.',
 'flattencom-mcp/src/prompts':'Renders localized debugging prompt templates without interpreting substituted user values.',
 'flattencom-mcp/src/flash':'Invokes esptool with explicit firmware offsets and checks timeout, exit status and output.',
 'flattencom-cli/tests/commands':'Checks CLI payload fidelity, delayed replies, language selection and help output.',
 'flattencom-core/tests/pty':'Tests serial I/O, throughput, configuration and reconnect failures through Linux pseudo-terminals.',
 'flattencom-core/examples/capture_benchmark':'Measures synthetic capture throughput and verifies every generated binary byte.',
 'flattencom-proto/tests/fixtures':'Checks shared golden JSON-RPC examples against Rust frame and response types.',
 'flattencomd/tests/e2e':'Exercises a real service process through authentication, sessions, capture, export and shutdown.',
 'flattencom-mcp/tests/e2e':'Exercises real MCP stdio processes, tool schemas, resources, translations and selection restrictions.',
}
for stem, summary in MODULES.items():
    DESCRIPTIONS[f'crates/{stem}.rs'] = ('flattencom ' + stem.replace('/', ' ').replace('_', ' ').title(), summary)
for crate in ('core','cli','proto','mcp'):
    DESCRIPTIONS[f'crates/flattencom-{crate}/Cargo.toml'] = (f'{crate.upper()} Package Manifest', f'Declares the flattencom-{crate} package targets and dependencies using workspace metadata.')
DESCRIPTIONS['crates/flattencomd/Cargo.toml'] = ('Service Package Manifest', 'Declares the serial background service binary and its workspace dependencies.')

DOCS = {
 'RECORDING':'Explains capture formats, retention, persistence, export and historical data access.',
 'TROUBLESHOOTING':'Diagnoses backend, port, recording, deployment and display problems with reproducible checks.',
 'ARCHITECTURE':'Explains component boundaries, capture data flow, concurrency and local access control.',
 'DEVELOPMENT':'Documents build dependencies, verification commands, protocol changes and release packaging.',
 'GUI':'Documents desktop controls, session workflows, history navigation, recording and evidence export.',
 'I18N':'Explains language selection, translation catalogs, placeholder handling and localization checks.',
 'MCP':'Documents MCP configuration, serial debugging tools, resources and selection-scoped analysis.',
 'PROTOCOL':'Specifies local JSON-RPC transport, methods, errors, frame cursors and recording behavior.',
 'ROADMAP':'Lists planned decoder tooling and interoperability improvements.',
 'UI-TEXT':'Defines application wording, terminology and translation conventions.',
 'WORKBENCH-RPC':'Documents marker, selection, history, transfer and reset RPC parameters and semantics.',
 'README':'Introduces the project components, startup commands and documentation links.',
 'PLAN':'Describes the original implementation milestones, architecture and delivery targets.',
}
for name, summary in DOCS.items():
    if (ROOT/f'docs/{name}.md').exists():DESCRIPTIONS[f'docs/{name}.md']=(name.replace('-',' ').title()+' Guide',summary)

def plain_header(name):
    title, summary = DESCRIPTIONS[name]
    return (f'flattencom - {title}\n\n{summary}\n\nAuthors:\n'
            f'{AUTHOR}\n\nCopyright (C) 2026 Evarentha\n'
            f'SPDX-License-Identifier: {SPDX}\n')

def uncommentable(name):
    """Return True for files whose format has no comment or declaration syntax."""
    return Path(name).suffix in {'.json', '.png', '.ico'} or name == 'Cargo.lock'

def header_for(name):
    text = plain_header(name)
    suffix = Path(name).suffix
    if suffix in {'.rs', '.cpp', '.h'}:
        return '/*\n' + ''.join(' *'+(' '+line if line else '')+'\n' for line in text.splitlines()) + ' */\n\n'
    if suffix in {'.md', '.svg', '.qrc'}:
        return '<!--\n' + text + '-->\n\n'
    prefix = ';;' if suffix == '.wat' else '//' if suffix == '.rc' else '#'
    return ''.join(prefix+(' '+line if line else '')+'\n' for line in text.splitlines())+'\n'

def files():
    output = subprocess.check_output(['git','ls-files','--cached','--others','--exclude-standard','-z'],cwd=ROOT)
    return sorted(set(p for p in output.decode().split('\0') if p))

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--write',action='store_true')
    parser.add_argument('--license-text',type=Path,help='Verbatim GPL-3.0 license document to install as LICENSE')
    args=parser.parse_args()
    if args.license_text:
        if not args.write:parser.error('--license-text requires --write')
        data=args.license_text.read_bytes()
        if b'GNU GENERAL PUBLIC LICENSE' not in data or b'END OF TERMS AND CONDITIONS' not in data:parser.error('Incomplete GPL text')
        (ROOT/'LICENSE').write_bytes(data)
    problems=[];count=0
    for name in files():
        if name == 'LICENSE':
            # The verbatim GPL text is the declaration for this repository. Never
            # add a project header and never require a companion declaration.
            data=(ROOT/name).read_bytes()
            if not all(marker in data for marker in (
                b'GNU GENERAL PUBLIC LICENSE', b'Version 3, 29 June 2007',
                b'Copyright (C) 2007 Free Software Foundation, Inc.',
                b'END OF TERMS AND CONDITIONS', b'How to Apply These Terms to Your New Programs')):
                problems.append('Incomplete GPL version 3 license document')
            continue
        if uncommentable(name):
            continue
        if name not in DESCRIPTIONS:problems.append(f'Missing reviewed description: {name}');continue
        count+=1
        path=ROOT/name
        data=path.read_bytes();prefix=b'';body=data
        if data.startswith(b'#!') and not data.startswith(b'#!['):
            prefix,_,body=data.partition(b'\n');prefix+=b'\n'
        expected=header_for(name).encode('utf-8')
        matches=body.replace(b'\r\n',b'\n').startswith(expected)
        if args.write and not matches:
            if b'SPDX-License-Identifier:' in body[:1200]:problems.append(f'Existing unexpected header: {name}');continue
            if b'\r\n' in data:expected=expected.replace(b'\n',b'\r\n')
            path.write_bytes(prefix+expected+body)
        elif not matches:problems.append(f'Missing or outdated header: {name}')
    if problems:raise SystemExit('\n'.join(problems))
    print(f'Authorship/license coverage: {count} project files; no missing entries.')

if __name__=='__main__':main()
