<!--
flattencom - Protocol Schema Guide

Explains the generated JSON Schemas and the command used to regenerate them.

Authors:
worryzu <worryzu@gmail.com> @LinearTeam

Copyright (C) 2026 Evarentha
SPDX-License-Identifier: GPL-3.0-or-later
-->

# flattencom JSON Schemas

These files are generated from the Rust protocol types:

```bash
cargo run -p flattencom-proto --bin gen-schemas --locked
```

The generator emits selected request, response, event, frame, session and transfer contracts. It does not cover every RPC method. The method/result reference and runtime constraints are in [Protocol](../docs/PROTOCOL.md) and [Workbench RPC](../docs/WORKBENCH-RPC.md); Rust definitions and runtime MCP tools/list provide further type information. Regenerate after changing Rust DTOs or their descriptions, and review schema changes alongside translations.
