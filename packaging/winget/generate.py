#!/usr/bin/env python3
# flattencom - Winget Manifest Generator
#
# Hashes an MSI and emits installer metadata using its release URL and version.
#
# Authors:
# worryzu <worryzu@gmail.com> @LinearTeam
#
# Copyright (C) 2026 Evarentha
# SPDX-License-Identifier: GPL-3.0-or-later

"""Generate a winget installer manifest after the MSI has a public release URL."""
import argparse
import hashlib
import json
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


def workspace_version():
    """Read the project version from the single source, the Cargo manifest."""
    manifest = (ROOT / "Cargo.toml").read_text(encoding="utf-8")
    declared = re.findall(r'^version = "([^"]+)"$', manifest, flags=re.M)
    if len(declared) != 1:
        raise SystemExit(f"Cargo.toml must declare exactly one line-initial version, found {len(declared)}")
    return declared[0]


p = argparse.ArgumentParser()
p.add_argument("installer", type=Path)
p.add_argument("--url", required=True)
p.add_argument("--version", help="Release version; defaults to the Cargo.toml workspace version.")
a = p.parse_args()
version = a.version or workspace_version()
# JSON is valid YAML 1.2; emitting it avoids a YAML dependency and unsafe quoting.
print(json.dumps({"PackageIdentifier": "flattencom.flattencom", "PackageVersion": version,
                  "InstallerType": "wix", "Installers": [{"Architecture": "x64", "InstallerUrl": a.url,
                  "InstallerSha256": hashlib.sha256(a.installer.read_bytes()).hexdigest().upper()}],
                  "ManifestType": "installer", "ManifestVersion": "1.6.0"}, indent=2))
