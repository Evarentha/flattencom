/*
 * flattencom - Device Profile Storage Interface
 *
 * Declares the types and operations for the device profile storage component.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

#pragma once
#include <QJsonObject>
namespace fc {
QString profileKey(const QJsonObject &port);
QJsonObject loadProfile(const QString &key);
void saveProfile(const QString &key, const QJsonObject &settings);
} // namespace fc
