/*
 * flattencom - Continuous Receive Text View Interface
 *
 * Declares the types and operations for the continuous receive text view component.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

#pragma once

#include "LogRules.h"
#include <QJsonArray>
#include <QJsonObject>
#include <QPlainTextEdit>
#include <QRegularExpression>
#include <QStringDecoder>
#include <QTextBlockUserData>
#include <QTimer>

struct LogAnchor final : QTextBlockUserData {
    struct HistorySpan {
        QString key;
        qsizetype offset, length;
        int column;
    };
    qint64 firstSeq = 0, lastSeq = 0, firstUs = 0, lastUs = 0;
    qint64 firstMono = 0, lastMono = 0;
    QString historyKey;
    QList<HistorySpan> historySpans;
    bool monotonic = true;
    QChar separator = '\n';
};

// Receive-only log view. Transport reads are arbitrary byte chunks, not lines.
// Decoding and line-ending state persist across batches and intervening TX frames.
class TextStreamView final : public QPlainTextEdit {
    Q_OBJECT
  public:
    explicit TextStreamView(QWidget *parent = nullptr);
    void appendFrames(const QJsonArray &frames);
    void clearStream();
    void markGap();
    void followTail();
    void setSearch(const QRegularExpression &expression);
    void reloadRules();
    QJsonObject selectedEvidence() const;
    QJsonArray searchResults() const;
    bool jumpToSequence(qint64 seq);
    void jumpToBlock(int block);
    qint64 firstSequence() const;
    QString topHistoryKey() const;
    static QString historyPositionKey(const QString &key, qsizetype offset);
    bool jumpToHistoryKey(const QString &key, bool top = false);
    void showHistory(const QJsonArray &records);
  signals:
    void frameInspected(const QJsonObject &frame);

  private:
    enum class EscapeState { Text, Escape, Csi, Osc, OscEscape };
    QString normalize(const QString &text);
    void appendText(const QString &text, const QJsonObject &frame = {});
    void highlightMatches();
    void resetStream();
    QStringDecoder decoder_{QStringDecoder::Utf8};
    bool afterCr_ = false;
    bool following_ = true;
    bool updating_ = false;
    EscapeState escape_ = EscapeState::Text;
    int escapeLength_ = 0;
    int column_ = 0;
    QRegularExpression search_;
    QTimer searchTimer_;
    LogRules *rules_;
};
