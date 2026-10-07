/*
 * flattencom - Graphical Configuration Editors Interface
 *
 * Declares the types and operations for the graphical configuration editors component.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

#pragma once

#include <QDialog>
#include <QJsonArray>
#include <QJsonObject>

namespace fc {
bool editDecoderOptions(QJsonObject &spec, QWidget *parent);
}

class QCheckBox;
class QComboBox;
class QLabel;
class QLineEdit;
class QListWidget;
class QPlainTextEdit;
class QPushButton;
class QSpinBox;

class HighlightRulesDialog final : public QDialog {
    Q_OBJECT
  public:
    explicit HighlightRulesDialog(const QJsonArray &rules, QWidget *parent = nullptr);
    QJsonArray rules() const;

  private:
    void loadCurrent();
    void updateCurrent();
    void preview();
    void populate(const QJsonArray &rules);
    QListWidget *list_;
    QWidget *editor_;
    QLineEdit *name_, *category_, *pattern_, *sample_;
    QComboBox *mode_, *severity_;
    QCheckBox *enabled_, *caseSensitive_;
    QPushButton *color_;
    QLabel *feedback_;
    QString colorValue_;
    bool loading_ = false;
};

class ResetSequenceDialog final : public QDialog {
    Q_OBJECT
  public:
    explicit ResetSequenceDialog(const QJsonArray &steps, QWidget *parent = nullptr);
    QJsonArray steps() const;

  private:
    void loadCurrent();
    void updateCurrent();
    QListWidget *list_;
    QWidget *editor_;
    QComboBox *dtr_, *rts_, *encoding_, *newline_;
    QPlainTextEdit *payload_;
    QSpinBox *delay_;
    QLabel *feedback_;
    bool loading_ = false;
};

class TriggersDialog final : public QDialog {
    Q_OBJECT
  public:
    explicit TriggersDialog(const QJsonArray &triggers, QWidget *parent = nullptr);
    QJsonArray triggers() const;

  private:
    QListWidget *list_;
};
