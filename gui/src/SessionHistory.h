/*
 * flattencom - Session Disk History Index Interface
 *
 * Declares the types and operations for the session disk history index component.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

#pragma once
#include <QJsonArray>
#include <QObject>
#include <QRegularExpression>
#include <QThread>
#include <atomic>
#include <memory>

// Owns a background disk index. No capture contents are retained beyond a
// bounded three-page window. Requests/results are generation tagged.
class SessionHistory final : public QObject {
    Q_OBJECT
  public:
    explicit SessionHistory(QObject *parent = nullptr);
    ~SessionHistory() override;
    void open(const QString &path, bool raw = false);
    void window(int firstPage, quint64 generation);
    void locate(qint64 sequence, quint64 generation);
    void search(const QRegularExpression &expression);
    void cancelSearch();
    int pages() const { return pages_; }
    QString path() const { return path_; }
  signals:
    void indexed(int pages);
    void windowReady(int firstPage, int pageCount, const QJsonArray &records, quint64 generation);
    // complete is false for capped results or segmented long-line regex coverage.
    void matchesReady(const QJsonArray &matches, bool complete);
    void failed(const QString &message);

  private:
    struct Index;
    std::shared_ptr<Index> index_;
    QThread thread_;
    QObject *worker_;
    std::shared_ptr<std::atomic<quint64>> indexGeneration_;
    std::shared_ptr<std::atomic<quint64>> searchGeneration_;
    QString path_;
    bool raw_ = false;
    int pages_ = 0;
    quint64 fileGeneration_ = 0;
};
