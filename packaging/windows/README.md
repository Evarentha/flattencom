<!--
flattencom - Windows Packaging Guide

Documents Qt runtime deployment, installer generation and Windows release requirements.

Authors:
worryzu <worryzu@gmail.com> @LinearTeam

Copyright (C) 2026 Evarentha
SPDX-License-Identifier: GPL-3.0-or-later
-->

# Windows packaging

Use the MSVC and Qt versions described in the [development guide](../../docs/DEVELOPMENT.md). The package script's `--deploy-qt` option deploys Qt dependencies. For a manually staged build, run:

```powershell
windeployqt --release path\to\flattencom-gui.exe
```

Ship `flattencom-gui.exe`, `flattencom.exe`, `flattencomd.exe`, `flattencom-mcp.exe` and the deployed Qt libraries and plugins together in an MSI or ZIP.

The GUI and daemon communicate through the local Named Pipe `\\.\pipe\flattencom.sock`, authenticated with the daemon token. The default pipe name is not user-specific. Separate daemon instances need distinct `FLATTENCOM_SOCKET` values, with matching settings in their clients.
