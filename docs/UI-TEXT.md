<!--
flattencom - Ui Text Guide

Defines application wording, terminology and translation conventions.

Authors:
worryzu <worryzu@gmail.com> @LinearTeam

Copyright (C) 2026 Evarentha
SPDX-License-Identifier: GPL-3.0-or-later
-->

# Application wording

## Writing rules

- Use English source messages and comments; put Chinese translations in catalogs.
- State observed status, not assumed success: request accepted, bytes written and device execution are different events.
- Errors identify the failing operation and one useful recovery step; retain paths, codes and diagnostic details.
- Avoid promotional, anthropomorphic and self-congratulatory language, decorative emoji or pseudo-icons.
- Prefer consistent terms: background service, session, send, receive, response, buffer, recording and export.
- Keep primary UI messages short; use details/help for longer explanations. Include units and label estimates as estimates.
- Never translate device bytes, commands, JSON identifiers or user names. Do not rename recording formats through display labels.
- Use the actual menu labels and paths from MainWindow.cpp; describe current behavior instead of a history of removed controls.

## Review and maintenance

Check both language catalogs and [Internationalization](I18N.md). Update the English reference documentation and both READMEs where terminology appears. Separate transmission confirmation from device acknowledgment, paused display from disconnected capture, and local file history from bounded RPC memory.
