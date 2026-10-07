/*
 * flattencom - Recording Preferences
 *
 * Builds unique text capture paths and applies persisted recording and retention preferences.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

#include "Recording.h"
#include "Style.h"
#include <QDateTime>
#include <QDir>
#include <QRegularExpression>
#include <QSettings>
#include <QUuid>

bool fc::autoRecordingEnabled() { return QSettings().value("recording/enabled", true).toBool(); }
QString fc::recordingDirectory() {
    return QSettings().value("recording/directory", QDir(stateDirectory()).filePath("captures")).toString();
}
void fc::applyAutoRecording(QJsonObject &config, bool enabled, const QString &directory) {
    config.remove("record_to");
    config.remove("record_rx_to");
    config.remove("record_keep_segments");
    if (!enabled)
        return;
    auto port = config.value("path").toString();
    port.replace(QRegularExpression("[^a-zA-Z0-9_-]"), "_");
    port = port.right(48);
    const auto name = QDateTime::currentDateTimeUtc().toString("yyyyMMdd'T'HHmmss_zzz'Z'") + "_" + port +
                      "_" + QUuid::createUuid().toString(QUuid::WithoutBraces);
    const auto base = QDir(directory).absoluteFilePath(name);
    config["record_to"] = base + ".log";
    const int keep = QSettings().value("recording/keepSegments", 0).toInt();
    if (keep > 0)
        config["record_keep_segments"] = keep;
}
