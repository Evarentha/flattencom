#!/usr/bin/env python3
# flattencom - Version Source Verifier
#
# Confirms Cargo.toml is the only declared project version and that every consumer derives it.
#
# Authors:
# worryzu <worryzu@gmail.com> @LinearTeam
#
# Copyright (C) 2026 Evarentha
# SPDX-License-Identifier: GPL-3.0-or-later

"""Verify that Cargo.toml is the only place the project version is written.

Rust crates inherit the workspace version, so Cargo, the GUI, CPack and the
packaging recipes must all read it instead of repeating it. This check rejects
a reintroduced literal, and cross-checks the version CMake resolved when a
configured build tree is available.
"""
from pathlib import Path
import re
import sys

ROOT = Path(__file__).resolve().parents[1]
DECLARATION = re.compile(r'^version = "([^"]+)"$', flags=re.M)
SEMVER = re.compile(r'^\d+\.\d+\.\d+([-+][0-9A-Za-z.-]+)?$')

# Each consumer must derive the version; these patterns reject a repeated literal.
HARDCODE = {
    'gui/CMakeLists.txt': (re.compile(r'project\(\s*flattencom-gui\s+VERSION\s+([0-9][^\s)]*)'), 'literal project() version'),
    'gui/src/main.cpp': (re.compile(r'setApplicationVersion\(\s*"([^"]+)"'), 'literal application version'),
    'gui/src/RpcClient.cpp': (re.compile(r'\{"version",\s*"([^"]+)"\}'), 'literal handshake version'),
    'packaging/arch/PKGBUILD': (re.compile(r'^pkgver=["\']?([0-9][^\s"\']*)', flags=re.M), 'literal pkgver'),
    'packaging/winget/generate.py': (re.compile(r'--version[^\n]*default\s*=\s*"?([0-9][^\s"\']*)'), 'literal --version default'),
}


def workspace_version():
    manifest = (ROOT / 'Cargo.toml').read_text(encoding='utf-8')
    declared = DECLARATION.findall(manifest)
    if len(declared) != 1:
        raise SystemExit(f'Cargo.toml must declare exactly one line-initial version, found {len(declared)}')
    return declared[0]


def workspace_packages():
    """Return the package names of every workspace member, read from their manifests."""
    manifest = (ROOT / 'Cargo.toml').read_text(encoding='utf-8')
    members = re.search(r'^members = \[\n(.*?)^\]$', manifest, flags=re.M | re.S)
    if not members:
        raise SystemExit('Cargo.toml has no readable [workspace] members list')
    names = []
    for entry in re.findall(r'"([^"]+)"', members.group(1)):
        member = (ROOT / entry / 'Cargo.toml').read_text(encoding='utf-8')
        found = re.search(r'^name = "([^"]+)"$', member, flags=re.M)
        if not found:
            raise SystemExit(f'{entry}/Cargo.toml has no line-initial package name')
        names.append(found.group(1))
    return sorted(names)


def configured_version(build_dir):
    cache = Path(build_dir) / 'CMakeCache.txt'
    if not cache.is_file():
        return None
    found = re.search(r'^FLATTENCOM_VERSION:INTERNAL=(.*)$', cache.read_text(encoding='utf-8'), flags=re.M)
    return found.group(1).strip() if found else None


def main():
    version = workspace_version()
    problems = []
    if not SEMVER.match(version):
        problems.append(f'Workspace version is not a release number: {version}')
    for name, (pattern, label) in sorted(HARDCODE.items()):
        match = pattern.search((ROOT / name).read_text(encoding='utf-8'))
        if match:
            problems.append(f'{name} repeats {label} {match.group(1)} instead of reading Cargo.toml')
    # Cargo.lock is generated; confirm every resolved workspace package matches.
    lock = (ROOT / 'Cargo.lock').read_text(encoding='utf-8')
    for name in workspace_packages():
        entry = re.search(rf'name = "{name}"\nversion = "([^"]+)"', lock)
        if not entry:
            problems.append(f'Cargo.lock has no {name} package')
        elif entry.group(1) != version:
            problems.append(f'Cargo.lock resolves {name} {entry.group(1)}, expected {version}')
    for build_dir in sys.argv[1:]:
        resolved = configured_version(build_dir)
        if resolved is None:
            problems.append(f'{build_dir} has no configured FLATTENCOM_VERSION; run cmake configure first')
        elif resolved != version:
            problems.append(f'{build_dir} was configured with version {resolved}, expected {version}')
    if problems:
        raise SystemExit('\n'.join(problems))
    print(f'Version source check passed: Cargo.toml declares {version}; '
          f'Rust, Qt, CPack and packaging recipes derive it.')


if __name__ == '__main__':
    main()