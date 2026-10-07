#!/usr/bin/env python3
# flattencom - Documentation Verification
#
# Checks English reference docs, bilingual READMEs, links, examples and source contracts, with optional isolated RPC smoke validation.
#
# Authors:
# worryzu <worryzu@gmail.com> @LinearTeam
#
# Copyright (C) 2026 Evarentha
# SPDX-License-Identifier: GPL-3.0-or-later

"""Check public docs; --smoke exercises examples using isolated virtual sessions.

Structural checks do not certify prose accuracy or semantic translation parity.
Run from any directory. Smoke requires freshly built CLI and daemon binaries;
it never connects to the user's service or opens a physical port.
"""
import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import time
from urllib.parse import unquote, urlsplit
import uuid

ROOT = Path(__file__).resolve().parents[1]
FENCE = re.compile(r'^```([^\n]*)\n(.*?)^```\s*$', re.M | re.S)
# The README language switcher may be written as a Markdown or a centered HTML
# link. Its label is navigation, not English prose; the target is checked as a
# normal link below, so the destination is deliberately not matched here.
SWITCHER = re.compile(r'\[简体中文\]\([^)]*\)|<a\s+href="[^"]*"[^>]*>简体中文</a>')


def body(text):
    return re.sub(r'<!--.*?-->', '', text, flags=re.S)


def anchors(text):
    result = set(re.findall(r'<a\s+id="([^"]+)"', text))
    seen = {}
    for heading in re.findall(r'^#{1,6}\s+(.+?)\s*$', body(text), re.M):
        slug = re.sub(r'[^\w\- ]', '', heading.lower()).replace(' ', '-')
        number = seen.get(slug, 0)
        seen[slug] = number + 1
        result.add(slug if number == 0 else f'{slug}-{number}')
    return result


def examples(text):
    output = []
    for language, content in FENCE.findall(text):
        if language.strip() in ('bash', 'bat', 'powershell', 'sh', 'json'):
            if language.strip() == 'json':
                value = json.loads(content)
                # Startup language is deliberately different in translated setup.
                if 'mcpServers' in value:
                    for server in value['mcpServers'].values():
                        args = server.get('args', [])
                        if '--lang' in args:
                            args[args.index('--lang') + 1] = 'LANGUAGE'
                content = json.dumps(value, sort_keys=True)
            output.append((language.strip(), content.strip()))
    return output


def rpc_methods():
    source = (ROOT / 'crates/flattencomd/src/dispatch.rs').read_text(encoding='utf-8')
    block = source.split('    match method {', 1)[1].split('\nfn attributed(', 1)[0]
    return set(re.findall(r'"([a-z_]+)"(?=\s*(?:\||=>))', block))


def static_checks():
    errors = []
    files = [ROOT / 'README.md', ROOT / 'README.zh-CN.md', *sorted((ROOT / 'docs').rglob('*.md')),
             ROOT / 'schemas/README.md', *sorted((ROOT / 'packaging').rglob('*.md'))]
    texts = {p: p.read_text(encoding='utf-8') for p in files}
    for path, text in texts.items():
        plain = body(text)
        label = str(path.relative_to(ROOT))
        english = SWITCHER.sub('', plain) if path == ROOT / 'README.md' else plain
        if path != ROOT / 'README.zh-CN.md' and re.search(r'[\u3400-\u9fff]', english):
            errors.append(f'{label}: reference documentation must be English')
        if len(re.findall(r'^```', plain, re.M)) % 2:
            errors.append(f'{label}: unclosed code fence')
        # A centered HTML title is as valid as a Markdown one; require exactly one.
        titles = len(re.findall(r'^# ', plain, re.M)) + len(re.findall(r'<h1[\s>]', plain, re.I))
        if titles != 1:
            errors.append(f'{label}: expected one top-level heading')
        for language, content in FENCE.findall(plain):
            if language.strip() == 'json':
                try:
                    json.loads(content)
                except json.JSONDecodeError as error:
                    errors.append(f'{label}: invalid JSON example: {error}')
        prose = FENCE.sub('', plain)
        for asset in re.findall(r'\b(?:src|srcset)="([^"]+)"', prose):
            parsed = urlsplit(asset)
            if not parsed.scheme and not parsed.netloc:
                target = (path.parent / unquote(parsed.path)).resolve()
                if not target.is_relative_to(ROOT) or not target.is_file():
                    errors.append(f'{label}: missing or non-public image: {asset}')
        for destination in re.findall(r'\]\(([^\s)]+)\)|\bhref="([^"]+)"', prose):
            link = destination[0] or destination[1]
            parsed = urlsplit(link)
            if parsed.scheme or parsed.netloc:
                continue
            target = (path.parent / unquote(parsed.path)).resolve() if parsed.path else path
            if not target.is_relative_to(ROOT) or target.is_relative_to(ROOT / 'local-docs'):
                errors.append(f'{label}: non-public link: {link}')
            elif not target.exists():
                errors.append(f'{label}: missing link target: {link}')
            elif parsed.fragment and target.suffix == '.md':
                if unquote(parsed.fragment) not in anchors(target.read_text(encoding='utf-8')):
                    errors.append(f'{label}: missing anchor: {link}')
        obsolete = ('GUI 默认生成两条路径', '.markers.jsonl', '.boots.jsonl',
                    '原始 JSON 默认折叠', '默认中文，可从',
                    '录制和解码当前在状态锁内执行',
                    'Independent recording queues to reduce slow-storage impact')
        for claim in obsolete:
            if claim in plain:
                errors.append(f'{label}: obsolete claim: {claim}')

    pairs = [(ROOT / 'README.md', ROOT / 'README.zh-CN.md')]
    if any((ROOT / 'docs/zh-CN').rglob('*.md')):
        errors.append('Chinese documentation belongs only in README.zh-CN.md')
    for en, zh in pairs:
        if not zh.exists():
            errors.append(f'Missing translation: {zh.relative_to(ROOT)}')
            continue
        a, b = texts[en], texts[zh]
        if re.findall(r'^(#{1,6}) ', body(a), re.M) != re.findall(r'^(#{1,6}) ', body(b), re.M):
            errors.append(f'{en.name}: bilingual heading structure differs')
        try:
            if examples(a) != examples(b):
                errors.append(f'{en.name}: bilingual executable/JSON examples differ')
        except json.JSONDecodeError:
            pass  # Reported above with its file name.

    expected_codes = {-32700, -32600, -32601, -32602, -32603, -32001, *range(100, 111)}
    for folder in (ROOT / 'docs',):
        codes = {int(x) for x in re.findall(r'^\| (-?\d+) \|', (folder / 'PROTOCOL.md').read_text(encoding='utf-8'), re.M)}
        if codes != expected_codes:
            errors.append(f'{folder.relative_to(ROOT)}: incomplete protocol error table')

    for folder in (ROOT / 'docs',):
        reference = '\n'.join((folder / name).read_text(encoding='utf-8') for name in ('PROTOCOL.md', 'WORKBENCH-RPC.md'))
        documented = set(re.findall(r'^\| `([a-z_]+)` \|', reference, re.M))
        if rpc_methods() != documented:
            errors.append(f'{folder.relative_to(ROOT)}: RPC table mismatch; missing={sorted(rpc_methods()-documented)}, extra={sorted(documented-rpc_methods())}')

    checks = [
        ('Cargo.toml', 'rust-version = "1.98"', 'DEVELOPMENT.md', '1.98+'),
        ('gui/CMakeLists.txt', 'cmake_minimum_required(VERSION 3.24)', 'DEVELOPMENT.md', '3.24+'),
        ('gui/CMakeLists.txt', 'find_package(Qt6 6.5 REQUIRED', 'DEVELOPMENT.md', '6.5+'),
        ('gui/src/Recording.cpp', 'base + ".log"', 'RECORDING.md', 'capture.log'),
        ('crates/flattencom-core/src/record.rs', '64 * 1024 * 1024', 'RECORDING.md', '64 MiB'),
        ('crates/flattencom-core/src/capture_worker.rs', 'mpsc::sync_channel(64)', 'RECORDING.md', '64'),
        ('crates/flattencomd/src/workbench.rs', 'Duration::from_secs(300)', 'WORKBENCH-RPC.md', 'five-minute'),
        ('gui/src/MainWindow.cpp', 'view->addMenu("Language")', 'GUI.md', 'View > Language'),
    ]
    for source, needle, doc, statement in checks:
        if needle not in (ROOT / source).read_text(encoding='utf-8') or statement not in (ROOT / 'docs' / doc).read_text(encoding='utf-8'):
            errors.append(f'Review documented source contract: {source} / {doc}')

    cli = (ROOT / 'crates/flattencom-cli/src/main.rs').read_text(encoding='utf-8').split('enum Commands {', 1)[1].split('enum DaemonAction', 1)[0]
    commands = {name.lower() for name in re.findall(r'^    ([A-Z]\w*)\s*[,\{]', cli, re.M)}
    for path, text in texts.items():
        for _, block in FENCE.findall(text):
            for command in re.findall(r'(?:\./target/release/)?flattencom(?:\.exe)?\s+([a-z][a-z_-]*)', block):
                if command not in commands:
                    errors.append(f'{path.relative_to(ROOT)}: unknown CLI command {command}')
    if errors:
        raise SystemExit('\n'.join(errors))
    print(f'Documentation checks passed: {len(files)} files, {len(pairs)} bilingual pairs, {len(rpc_methods())} RPC methods.')


def smoke(bin_dir):
    suffix = '.exe' if os.name == 'nt' else ''
    cli = (bin_dir / f'flattencom{suffix}').resolve()
    daemon = (bin_dir / f'flattencomd{suffix}').resolve()
    if not cli.is_file() or not daemon.is_file():
        raise SystemExit('Build release CLI and daemon first, or pass --bin-dir.')
    with tempfile.TemporaryDirectory(prefix='fc-docs-') as tmp:
        directory = Path(tmp)
        endpoint = rf'\\.\pipe\flattencom-docs-{uuid.uuid4()}' if os.name == 'nt' else str(directory / 'daemon.sock')
        env = {**os.environ, 'FLATTENCOM_SOCKET': endpoint, 'FLATTENCOM_STATE_DIR': str(directory / 'state'),
               'FLATTENCOM_NO_AUTOSPAWN': '1', 'FLATTENCOM_LANG': 'en'}
        with (directory / 'daemon.log').open('wb') as log:
            process = subprocess.Popen([str(daemon), '--foreground'], env=env, stdout=log, stderr=log)
            def run(*args):
                result = subprocess.run([str(cli), *args], env=env, capture_output=True, text=True, encoding='utf-8', timeout=15)
                if result.returncode:
                    raise AssertionError(f'{args}: {result.stderr or result.stdout}')
                return result.stdout
            def rpc(method, params):
                return json.loads(run('rpc', method, json.dumps(params)))
            try:
                deadline = time.monotonic() + 10
                while True:
                    try:
                        rpc('daemon_info', {})
                        break
                    except AssertionError:
                        if process.poll() is not None or time.monotonic() >= deadline:
                            raise
                        time.sleep(.05)
                for args in [('send', 'virtual://echo', 'AT', '--expect', 'AT'),
                             ('send', 'virtual://echo', '--hex', '00 FF 0D 0A', '--newline', 'none'),
                             ('decode', '--list'), ('--lang', 'zh-CN', '--help')]:
                    run(*args)
                capture = directory / 'capture.log'
                opened = rpc('open_session', {'path': 'virtual://echo', 'record_to': str(capture)})
                sid = opened['session']['session_id']
                assert rpc('open_session', {'path': 'virtual://echo'})['reused'] is True
                sent = rpc('send', {'session_id': sid, 'data': 'PING', 'newline': 'crlf'})
                assert sent['bytes_sent'] == 6
                deadline = time.monotonic() + 5
                cursor, frames = 0, []
                while not any(f.get('dir') == 'rx' and 'PING' in f.get('text', '') for f in frames):
                    page = rpc('read_frames', {'session_id': sid, 'since_seq': cursor, 'format': 'decoded'})
                    assert page['next_seq'] >= cursor
                    cursor = page['next_seq']; frames.extend(page['frames'])
                    assert time.monotonic() < deadline
                    time.sleep(.01)
                exported = rpc('export_log', {'session_id': sid, 'format': 'txt', 'all': True})
                assert Path(exported['path']).suffix == '.log'
                assert 'PING' in Path(exported['path']).read_text(encoding='utf-8')
                selection = rpc('publish_selection', {'session_id': sid, 'text': 'PING'})['selection']
                assert rpc('read_selection', {'selection_id': selection['id']})['selection']['text'] == 'PING'
                assert rpc('revoke_selection', {'selection_id': selection['id']})['removed'] is True
                rpc('close_session', {'session_id': sid, 'force': True})
                assert 'PING' in capture.read_text(encoding='utf-8')
                rpc('shutdown', {})
                process.wait(timeout=10)
            finally:
                if process.poll() is None:
                    process.terminate()
                    try:
                        process.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        process.kill(); process.wait()
    print('Documentation smoke passed: CLI echo, shared RPC, cursor reads, .log export and selection lifecycle.')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--smoke', action='store_true')
    parser.add_argument('--bin-dir', type=Path, default=ROOT / 'target/release')
    args = parser.parse_args()
    static_checks()
    if args.smoke:
        smoke(args.bin_dir)
