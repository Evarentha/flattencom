#!/usr/bin/env bash
# flattencom - AppImage Packaging Helper
#
# Installs a built application into AppDir and invokes linuxdeploy with its Qt plugin.
#
# Authors:
# worryzu <worryzu@gmail.com> @LinearTeam
#
# Copyright (C) 2026 Evarentha
# SPDX-License-Identifier: GPL-3.0-or-later

set -euo pipefail
# Build Rust/Qt first with scripts/package.py. Point these variables at trusted
# linuxdeploy and linuxdeploy-plugin-qt executables from their official releases.
: "${LINUXDEPLOY:?Set LINUXDEPLOY to the linuxdeploy AppImage executable}"
: "${LINUXDEPLOY_PLUGIN_QT:?Set LINUXDEPLOY_PLUGIN_QT to its Qt plugin executable}"
root="$(cd "$(dirname "$0")/.." && pwd)"
export PATH="$(dirname "$LINUXDEPLOY_PLUGIN_QT"):$PATH"
export QMAKE="${QMAKE:-qmake6}"
export APPIMAGE_EXTRACT_AND_RUN=1
cmake --install "$root/gui/build/release" --prefix "$root/dist/AppDir/usr"
"$LINUXDEPLOY" --appdir "$root/dist/AppDir" --plugin qt \
  --desktop-file "$root/packaging/linux/flattencom.desktop" \
  --icon-file "$root/packaging/linux/flattencom.svg" --output appimage
