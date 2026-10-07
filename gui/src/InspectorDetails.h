/*
 * flattencom - Record Inspector Interface
 *
 * Declares the types and operations for the record inspector component.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

#pragma once
#include <QJsonObject>
#include <QWidget>

class QPlainTextEdit;
class QTreeWidget;
class QLineEdit;
class QTabWidget;
class QLabel;

class InspectorDetails final : public QWidget {
    Q_OBJECT
  public:
    explicit InspectorDetails(QWidget *parent = nullptr);
    void setFrame(const QJsonObject &frame);
    void clear();
    void retranslate();

  private:
    void findNext();
    void copyValue();
    QPlainTextEdit *json_;
    QTreeWidget *fields_;
    QLineEdit *query_;
    QTabWidget *tabs_;
    QLabel *result_;
};
