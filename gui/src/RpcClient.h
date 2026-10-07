/*
 * flattencom - Qt Background Service Client Interface
 *
 * Declares the types and operations for the qt background service client component.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

#pragma once
#include <QHash>
#include <QJsonObject>
#include <QLocalSocket>
#include <QObject>
#include <QTimer>
#include <functional>

class RpcClient final : public QObject {
    Q_OBJECT
  public:
    using Callback = std::function<void(const QJsonObject &, const QString &)>;
    explicit RpcClient(QObject *parent = nullptr);
    ~RpcClient() override;
    void start();
    void stop();
    bool ready() const { return ready_; }
    quint64 call(const QString &method, const QJsonObject &params, Callback callback = {},
                 int timeoutMs = 10000);
  signals:
    void connected();
    void disconnected();
    void notification(const QString &method, const QJsonObject &params);
    void error(const QString &message);

  private:
    void connectSocket();
    void read();
    void failPending(const QString &reason);
    QLocalSocket socket_;
    QTimer retry_;
    QByteArray buffer_;
    struct Pending {
        Callback callback;
        qint64 deadline;
    };
    QHash<quint64, Pending> pending_;
    quint64 nextId_ = 1;
    bool ready_ = false;
    bool spawned_ = false;
    bool stopped_ = false;
};
