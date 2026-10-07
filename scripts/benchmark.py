#!/usr/bin/env python3
# flattencom - Capture Benchmark Runner
#
# Builds the synthetic capture benchmark and records throughput and child-process CPU usage.
#
# Authors:
# worryzu <worryzu@gmail.com> @LinearTeam
#
# Copyright (C) 2026 Evarentha
# SPDX-License-Identifier: GPL-3.0-or-later

"""Measure local capture CPU cost; reports metrics rather than promising a universal threshold."""
import json
from pathlib import Path
import resource
import subprocess

root = Path(__file__).resolve().parents[1]
subprocess.run(["cargo", "build", "--release", "-p", "flattencom-core", "--example", "capture_benchmark"], cwd=root, check=True)
before = resource.getrusage(resource.RUSAGE_CHILDREN)
result = subprocess.run([str(root / "target/release/examples/capture_benchmark"), "5"], check=True, capture_output=True, text=True, encoding="utf-8")
after = resource.getrusage(resource.RUSAGE_CHILDREN)
metrics = json.loads(result.stdout)
metrics["cpu_single_core_percent"] = 100 * ((after.ru_utime + after.ru_stime) - (before.ru_utime + before.ru_stime)) / metrics["elapsed_s"]
output = root / "dist/benchmark.json"
output.parent.mkdir(exist_ok=True)
output.write_text(json.dumps(metrics, indent=2), encoding="utf-8")
print(json.dumps(metrics, indent=2))
