/*
 * flattencom - Capture Library Dialog Interface
 *
 * Declares the types and operations for the capture library dialog component.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

#pragma once
#include <QDialog>
class LogManager final : public QDialog {
  public:
    explicit LogManager(QWidget *parent = nullptr, const QString &initialFile = {});
};
