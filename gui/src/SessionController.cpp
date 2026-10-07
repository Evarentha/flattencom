/*
 * flattencom - Session Request Controller
 *
 * Scopes request callbacks to session lifetimes and coalesces periodic status requests.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

#include "SessionController.h"
#include <QPointer>

SessionController::SessionController(RpcClient *rpc, QString id, QObject *parent)
    : QObject(parent), rpc_(rpc), id_(std::move(id)) {}
void SessionController::request(const QString &method, QJsonObject params, RpcClient::Callback callback,
                                int timeoutMs) {
    const bool poll = method == "get_stats" || method == "get_signals";
    if (poll && polling_.contains(method))
        return;
    if (poll)
        polling_.insert(method);
    params["session_id"] = id_;
    QPointer<SessionController> self(this);
    rpc_->call(
        method, params,
        [self, method, poll, callback](const QJsonObject &result, const QString &error) {
            if (!self)
                return;
            if (poll)
                self->polling_.remove(method);
            if (callback)
                callback(result, error);
        },
        timeoutMs);
}
