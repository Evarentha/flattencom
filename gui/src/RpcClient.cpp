/*
 * flattencom - Qt Background Service Client
 *
 * Handles local JSON-RPC authentication, request callbacks, notifications and reconnection.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

#include "RpcClient.h"
#include "Style.h"
#include <QDateTime>
#include <QDir>
#include <QFile>
#include <QJsonDocument>
#include <QPointer>
#include <QProcess>
#include <utility>

RpcClient::RpcClient(QObject *parent) : QObject(parent) {
    retry_.setInterval(1000);
    connect(&retry_, &QTimer::timeout, this, &RpcClient::connectSocket);
    connect(&socket_, &QLocalSocket::readyRead, this, &RpcClient::read);
    connect(&socket_, &QLocalSocket::connected, this, [this] {
        retry_.stop();
        QFile token(QDir(fc::stateDirectory()).filePath("daemon.token"));
        if (!token.open(QIODevice::ReadOnly)) {
            QPointer<RpcClient> self(this);
            emit error(token.errorString());
            if (self)
                socket_.abort();
            return;
        }
        call("hello",
             {{"client", "flattencom-gui"},
              {"version", QString::fromLatin1(FLATTENCOM_VERSION)},
              {"proto", 1},
              {"token", QString::fromUtf8(token.readAll().trimmed())}},
             [this](const QJsonObject &result, const QString &failure) {
                 if (!failure.isEmpty() || result.value("proto").toInt() != 1) {
                     QPointer<RpcClient> self(this);
                     emit error(failure.isEmpty() ? fc::text("Incompatible service protocol version")
                                                  : failure);
                     if (self)
                         socket_.abort();
                     return;
                 }
                 ready_ = true;
                 emit connected();
             });
    });
    connect(&socket_, &QLocalSocket::disconnected, this, [this] {
        if (stopped_)
            return;
        ready_ = false;
        buffer_.clear();
        QPointer<RpcClient> self(this);
        failPending(fc::text("Background service disconnected"));
        if (!self || stopped_)
            return;
        emit disconnected();
        if (!self || stopped_)
            return;
        retry_.start();
    });
    connect(&socket_, &QLocalSocket::errorOccurred, this, [this](QLocalSocket::LocalSocketError) {
        QPointer<RpcClient> self(this);
        if (stopped_)
            return;
        if (!spawned_ && qEnvironmentVariable("FLATTENCOM_NO_AUTOSPAWN") != "1") {
            spawned_ = true;
            const auto daemon = fc::executable("flattencomd");
            if (!daemon.isEmpty()) {
                QProcess process;
                process.setProgram(daemon);
                process.setStandardOutputFile(QProcess::nullDevice());
                process.setStandardErrorFile(QProcess::nullDevice());
                if (!process.startDetached())
                    emit error(fc::text("Unable to start background service"));
            } else
                emit error(fc::text("Set FLATTENCOMD_BIN to locate the daemon"));
        }
        if (self && !stopped_)
            retry_.start();
    });
    auto *expiry = new QTimer(this);
    expiry->setInterval(500);
    connect(expiry, &QTimer::timeout, this, [this] {
        QPointer<RpcClient> self(this);
        const auto now = QDateTime::currentMSecsSinceEpoch();
        for (const auto id : pending_.keys()) {
            if (pending_.value(id).deadline <= now) {
                const auto pending = pending_.take(id);
                if (pending.callback)
                    pending.callback({}, fc::text("Request timed out"));
                if (!self)
                    return;
            }
        }
    });
    expiry->start();
}
RpcClient::~RpcClient() {
    // QLocalSocket emits disconnected while it is destroyed. Block signals before
    // pending_ is destroyed, otherwise the callback lambda can re-enter a partly
    // destructed RpcClient during MainWindow shutdown.
    socket_.blockSignals(true);
    socket_.abort();
}
void RpcClient::start() {
    stopped_ = false;
    connectSocket();
}
void RpcClient::stop() {
    stopped_ = true;
    retry_.stop();
    ready_ = false;
    pending_.clear();
    socket_.abort();
    buffer_.clear();
}
void RpcClient::connectSocket() {
    if (!stopped_ && socket_.state() == QLocalSocket::UnconnectedState)
        socket_.connectToServer(fc::socketName());
}
quint64 RpcClient::call(const QString &method, const QJsonObject &params, Callback callback, int timeoutMs) {
    if (socket_.state() != QLocalSocket::ConnectedState) {
        if (callback)
            callback({}, fc::text("Background service not connected"));
        return 0;
    }
    if (pending_.size() >= 64 || socket_.bytesToWrite() > 1024 * 1024) {
        if (callback)
            callback({}, fc::text("Request queue is full; wait for pending operations."));
        return 0;
    }
    const auto id = nextId_++;
    pending_.insert(id, {std::move(callback), QDateTime::currentMSecsSinceEpoch() + timeoutMs});
    auto localized = params;
    localized["_language"] = fc::language() == "zh" ? "zh-CN" : "en";
    const QJsonObject message{
        {"jsonrpc", "2.0"}, {"id", static_cast<qint64>(id)}, {"method", method}, {"params", localized}};
    socket_.write(QJsonDocument(message).toJson(QJsonDocument::Compact) + '\n');
    return id;
}
void RpcClient::read() {
    QPointer<RpcClient> self(this);
    buffer_ += socket_.readAll();
    constexpr qsizetype maxMessage = 16 * 1024 * 1024;
    while (true) {
        const auto end = buffer_.indexOf('\n');
        if (end < 0) {
            if (buffer_.size() > maxMessage) {
                emit error(fc::text("RPC message exceeds size limit"));
                if (self)
                    socket_.abort();
            }
            return;
        }
        if (end > maxMessage) {
            socket_.abort();
            return;
        }
        QJsonParseError parse;
        const auto doc = QJsonDocument::fromJson(buffer_.left(end), &parse);
        buffer_.remove(0, end + 1);
        if (parse.error != QJsonParseError::NoError || !doc.isObject()) {
            emit error(fc::text("Invalid RPC message format"));
            if (!self)
                return;
            continue;
        }
        const auto object = doc.object();
        if (object.contains("id")) {
            const auto pending = pending_.take(object.value("id").toInteger());
            if (pending.callback)
                pending.callback(object.value("result").toObject(),
                                 object.value("error").toObject().value("message").toString());
        } else
            emit notification(object.value("method").toString(), object.value("params").toObject());
        // A response or notification may synchronously close the owning window.
        if (!self)
            return;
    }
}
void RpcClient::failPending(const QString &reason) {
    QPointer<RpcClient> self(this);
    auto pending = std::exchange(pending_, {});
    for (auto it = pending.begin(); it != pending.end(); ++it) {
        if (it.value().callback)
            it.value().callback({}, reason);
        if (!self)
            return;
    }
}
