/*
 * flattencom - Device Profile Storage
 *
 * Keys persistent device settings by USB identity or fallback port path.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

#include "DeviceProfiles.h"
#include <QCryptographicHash>
#include <QJsonDocument>
#include <QSettings>
QString fc::profileKey(const QJsonObject &port) {
    QString identity;
    if (!port.value("serial").toString().trimmed().isEmpty() && port.contains("vid") && port.contains("pid"))
        identity = QString("usb:%1:%2:%3")
                       .arg(port.value("vid").toInt())
                       .arg(port.value("pid").toInt())
                       .arg(port.value("serial").toString());
    else
        identity = "path:" + port.value("path").toString();
    return QString::fromLatin1(
        QCryptographicHash::hash(identity.toUtf8(), QCryptographicHash::Sha256).toHex());
}
QJsonObject fc::loadProfile(const QString &key) {
    return QJsonDocument::fromJson(QSettings().value("devices/" + key).toByteArray()).object();
}
void fc::saveProfile(const QString &key, const QJsonObject &settings) {
    QSettings().setValue("devices/" + key, QJsonDocument(settings).toJson(QJsonDocument::Compact));
}
