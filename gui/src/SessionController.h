/*
 * flattencom - Session Request Controller Interface
 *
 * Declares session-scoped requests and periodic query coordination.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

#pragma once
#include "RpcClient.h"
#include <QSet>

// Session-scoped request lifetime and periodic request coalescing.
class SessionController final : public QObject {
  public:
    SessionController(RpcClient *rpc, QString id, QObject *parent = nullptr);
    void request(const QString &method, QJsonObject params, RpcClient::Callback callback,
                 int timeoutMs = 10000);

  private:
    RpcClient *rpc_;
    QString id_;
    QSet<QString> polling_;
};
