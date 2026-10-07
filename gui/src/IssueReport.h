/*
 * flattencom - Readable Issue Report Interface
 *
 * Declares the types and operations for the readable issue report component.
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
QString issueReport(const QJsonObject &bundle);
}
