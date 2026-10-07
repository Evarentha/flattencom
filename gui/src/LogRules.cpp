/*
 * flattencom - Log Syntax Highlighting
 *
 * Loads, validates and applies prioritized keyword and regular-expression highlighting rules.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

#include "LogRules.h"
#include "Style.h"
#include <QFile>
#include <QJsonDocument>
#include <QJsonObject>
#include <QSettings>

QJsonArray LogRules::defaults() {
    QFile file(":/flattencom/log-rules.json");
    if (!file.open(QIODevice::ReadOnly))
        return {};
    return QJsonDocument::fromJson(file.readAll()).array();
}
QJsonArray LogRules::configured() {
    const auto bytes = QSettings().value("logRules").toByteArray();
    if (bytes.isEmpty())
        return defaults();
    const auto doc = QJsonDocument::fromJson(bytes);
    return doc.isArray() && validate(doc.array()).isEmpty() ? doc.array() : defaults();
}
QString LogRules::validate(const QJsonArray &rules) {
    if (rules.size() > 128)
        return fc::text("Maximum 128 rules.");
    for (const auto &v : rules) {
        const auto r = v.toObject();
        const auto pattern = r.value("pattern").toString();
        if (pattern.isEmpty() || pattern.size() > 2048)
            return r.value("name").toString(r.value("id").toString()) + ": " +
                   fc::text("Pattern must contain 1 to 2048 characters.");
        const auto regex = expression(r);
        if (!regex.isValid())
            return r.value("name").toString(r.value("id").toString()) + ": " + regex.errorString();
        if (!QColor(r.value("color").toString()).isValid())
            return fc::text("Invalid color.");
    }
    return {};
}
QRegularExpression LogRules::expression(const QJsonObject &rule) {
    auto pattern = rule.value("pattern").toString();
    if (rule.value("match").toString() == "literal")
        pattern = QRegularExpression::escape(pattern);
    return QRegularExpression(pattern, rule.value("case_sensitive").toBool()
                                           ? QRegularExpression::NoPatternOption
                                           : QRegularExpression::CaseInsensitiveOption);
}
LogRules::LogRules(QTextDocument *document) : QSyntaxHighlighter(document) { reload(); }
void LogRules::reload() {
    rules_.clear();
    for (const auto &v : configured()) {
        const auto r = v.toObject();
        if (!r.value("enabled").toBool(true))
            continue;
        rules_.append({r.value("id").toString(), r.value("category").toString(),
                       r.value("severity").toString(), expression(r), QColor(r.value("color").toString())});
    }
    rehighlight();
}
QString LogRules::classify(const QString &line) const {
    for (const auto &r : rules_)
        if (r.pattern.match(line).hasMatch())
            return r.severity + " / " + r.category;
    return {};
}
void LogRules::highlightBlock(const QString &text) {
    // First match wins: fatal > error > warning > informational. Rules use bounded
    // line input, not full capture buffers, and are editable in the workbench.
    for (const auto &rule : rules_)
        if (rule.pattern.match(text).hasMatch()) {
            QTextCharFormat format;
            format.setForeground(rule.color);
            if (rule.severity == "fatal")
                format.setFontWeight(QFont::Bold);
            setFormat(0, text.size(), format);
            return;
        }
}
