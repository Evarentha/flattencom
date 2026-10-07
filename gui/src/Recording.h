/*
 * flattencom - Recording Preferences Interface
 *
 * Declares the types and operations for the recording preferences component.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

#pragma once
#include <QJsonObject>
#include <QString>

namespace fc {
bool autoRecordingEnabled();
QString recordingDirectory();
// Pure path/config construction: actual files are opened by the daemon before serial IO starts.
void applyAutoRecording(QJsonObject &config, bool enabled, const QString &directory);
} // namespace fc
