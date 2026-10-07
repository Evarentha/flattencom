#!/usr/bin/env python3
# flattencom - Release Package Builder
#
# Builds release binaries, runs Qt tests and generates CPack archives and SHA-256 checksums.
#
# Authors:
# worryzu <worryzu@gmail.com> @LinearTeam
#
# Copyright (C) 2026 Evarentha
# SPDX-License-Identifier: GPL-3.0-or-later

"""Build local release artifacts and an installable CPack archive/MSI."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile

ROOT = Path(__file__).resolve().parents[1]


def run(*args, cwd=ROOT):
    subprocess.run(args, cwd=cwd, check=True)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--generator", default="ZIP" if os.name == "nt" else "TGZ",
                        choices=["TGZ", "ZIP", "DEB", "WIX"])
    parser.add_argument("--deploy-qt", action="store_true")
    args = parser.parse_args()
    run("cargo", "build", "--release", "--workspace", "--locked")
    run("cmake", "-S", "gui", "-B", "gui/build/release", "-G", "Ninja",
        "-DCMAKE_BUILD_TYPE=Release", "-DFLATTENCOM_BUNDLE_RUST=ON",
        f"-DFLATTENCOM_DEPLOY_QT={'ON' if args.deploy_qt else 'OFF'}")
    run("cmake", "--build", "gui/build/release", "--parallel")
    run("ctest", "--test-dir", "gui/build/release", "--output-on-failure")
    output = ROOT / "dist"
    output.mkdir(exist_ok=True)
    # Ship the matching project source with binary artifacts, including LICENSE,
    # build scripts and the lockfile; exclude build caches and runtime captures.
    run("python3" if os.name != "nt" else "python", "scripts/maintain-headers.py")
    source_files = subprocess.check_output(
        ["git", "ls-files", "--cached", "--others", "--exclude-standard", "-z"], cwd=ROOT
    ).decode().split('\0')
    metadata = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--no-deps", "--format-version", "1", "--locked"], cwd=ROOT))
    version = next(p["version"] for p in metadata["packages"] if p["name"] == "flattencom-core")
    # Stage this invocation separately so old dist files cannot enter its manifest.
    with tempfile.TemporaryDirectory(prefix=".package-", dir=output) as temporary:
        stage = Path(temporary)
        run("cpack", "--config", "gui/build/release/CPackConfig.cmake", "-G", args.generator,
            "-B", str(stage))
        with tarfile.open(stage / f"flattencom-{version}-source.tar.gz", "w:gz") as archive:
            for name in sorted(set(filter(None, source_files))):
                archive.add(ROOT / name, arcname=f"flattencom-{version}/" + name, recursive=False)
        artifacts = sorted(p for p in stage.iterdir() if p.is_file())
        checksums = []
        for artifact in artifacts:
            digest = hashlib.sha256()
            with artifact.open("rb") as stream:
                for chunk in iter(lambda: stream.read(1024 * 1024), b""):
                    digest.update(chunk)
            checksums.append(f"{digest.hexdigest()}  {artifact.name}\n")
        manifest = stage / "SHA256SUMS"
        manifest.write_text("".join(checksums), encoding="utf-8")
        for artifact in [*artifacts, manifest]:
            artifact.replace(output / artifact.name)
    print(f"Artifacts and hashes: {output}")


if __name__ == "__main__":
    main()
