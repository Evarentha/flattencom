#!/usr/bin/env python3
# flattencom - Project Verification Runner
#
# Runs formatting, Rust checks, tests and Qt virtual-port verification with failure propagation.
#
# Authors:
# worryzu <worryzu@gmail.com> @LinearTeam
#
# Copyright (C) 2026 Evarentha
# SPDX-License-Identifier: GPL-3.0-or-later

"""One reproducible local verification command; subprocess failures propagate."""
from pathlib import Path
import subprocess
import sys

root = Path(__file__).resolve().parents[1]
commands = [
    [sys.executable, "scripts/maintain-headers.py"],
    [sys.executable, "scripts/check-i18n.py"],
    [sys.executable, "scripts/check-docs.py"],
    ["cargo", "fmt", "--all", "--", "--check"],
    ["cargo", "clippy", "--workspace", "--all-targets", "--locked", "--", "-D", "warnings"],
    ["cargo", "build", "--workspace", "--locked"],
    ["cargo", "test", "--workspace", "--locked"],
    ["cmake", "-S", "gui", "-B", "gui/build/debug", "-G", "Ninja", "-DCMAKE_BUILD_TYPE=Debug"],
    [sys.executable, "scripts/check-version.py", "gui/build/debug"],
    ["cmake", "--build", "gui/build/debug", "--parallel"],
    ["ctest", "--test-dir", "gui/build/debug", "--output-on-failure"],
    [sys.executable, "scripts/gui-smoke.py"],
]
for command in commands:
    print("+", " ".join(command), flush=True)
    subprocess.run(command, cwd=root, check=True)
