#!/usr/bin/env python3
# flattencom - Qt Serial Smoke Test Runner
#
# Starts isolated service and GUI processes to verify virtual-port communication and recording.
#
# Authors:
# worryzu <worryzu@gmail.com> @LinearTeam
#
# Copyright (C) 2026 Evarentha
# SPDX-License-Identifier: GPL-3.0-or-later

"""Run a real Qt → daemon → serial echo smoke test with isolated state."""
import argparse
import os
from pathlib import Path
import subprocess
import tempfile
import time
import uuid


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--bin-dir", type=Path, default=Path("target/debug"))
    parser.add_argument("--gui", type=Path, default=Path("gui/build/debug/flattencom-gui"))
    parser.add_argument("--screenshot", type=Path)
    parser.add_argument("--lang", choices=["en", "zh-CN"], default="en")
    args = parser.parse_args()
    suffix = ".exe" if os.name == "nt" else ""
    gui = args.gui.resolve()
    if suffix and not gui.suffix:
        gui = gui.with_suffix(suffix)
    daemon_path = (args.bin_dir / f"flattencomd{suffix}").resolve()
    with tempfile.TemporaryDirectory(prefix="flattencom-") as tmp:
        root = Path(tmp)
        endpoint = (rf"\\.\pipe\flattencom-smoke-{uuid.uuid4()}" if os.name == "nt"
                    else str(root / "daemon.sock"))
        env = {**os.environ, "FLATTENCOM_SOCKET": endpoint,
               "FLATTENCOM_STATE_DIR": str(root / "state"), "FLATTENCOMD_BIN": str(daemon_path),
               "QT_QPA_PLATFORM": "offscreen", "XDG_CONFIG_HOME": str(root / "config")}
        with (root / "daemon.log").open("wb") as log:
            daemon = subprocess.Popen([str(daemon_path), "--foreground"], env=env, stdout=log, stderr=log)
            try:
                deadline = time.monotonic() + 10
                while not (root / "state/daemon.token").exists():
                    if daemon.poll() is not None or time.monotonic() > deadline:
                        raise RuntimeError("daemon startup failed")
                    time.sleep(.05)
                command = [str(gui), "--smoke", "--lang", args.lang]
                if args.screenshot:
                    args.screenshot.parent.mkdir(parents=True, exist_ok=True)
                    command += ["--screenshot", str(args.screenshot.resolve())]
                subprocess.run(command, env=env, check=True, timeout=20)
                print("Qt/daemon serial roundtrip: PASS")
            finally:
                daemon.terminate()
                try:
                    daemon.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    daemon.kill()
                    daemon.wait()


if __name__ == "__main__":
    main()
