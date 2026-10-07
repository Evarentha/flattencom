/*
 * flattencom - Log Syntax Highlighting Interface
 *
 * Declares the types and operations for the log syntax highlighting component.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

#pragma once
#include <QColor>
#include <QJsonArray>
#include <QRegularExpression>
#include <QSyntaxHighlighter>

struct LogRule {
    QString id, category, severity;
    QRegularExpression pattern;
    QColor color;
};
class LogRules final : public QSyntaxHighlighter {
  public:
    explicit LogRules(QTextDocument *document);
    static QJsonArray defaults();
    static QJsonArray configured();
    static QString validate(const QJsonArray &rules);
    static QRegularExpression expression(const QJsonObject &rule);
    void reload();
    QString classify(const QString &line) const;

  protected:
    void highlightBlock(const QString &text) override;

  private:
    QList<LogRule> rules_;
};
