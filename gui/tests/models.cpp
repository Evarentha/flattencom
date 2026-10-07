/*
 * flattencom - Qt Regression Tests
 *
 * Verifies models, translation, selection, serial interactions, disk history and inspector behavior.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

#include "AboutDialog.h"
#include "CaptureViewer.h"
#include "DeviceProfiles.h"
#include "FramesModel.h"
#include "Icons.h"
#include "InspectorDetails.h"
#include "IssueReport.h"
#include "MainWindow.h"
#include "Recording.h"
#include "RuleEditors.h"
#include "SentLog.h"
#include "SessionHistory.h"
#include "SessionPane.h"
#include "Style.h"
#include "TextStreamView.h"
#include <QClipboard>
#include <QDialogButtonBox>
#include <QFile>
#include <QJsonDocument>
#include <QLocalServer>
#include <QPointer>
#include <QPushButton>
#include <QScopeGuard>
#include <QScrollBar>
#include <QSettings>
#include <QSplitter>
#include <QStyleHints>
#include <QTemporaryDir>
#include <QTextBlock>
#include <QToolButton>
#include <QTreeWidget>
#include <QWheelEvent>
#include <QtTest>

static QJsonObject receiveChunk(const QByteArray &bytes, const QString &direction = "rx") {
    return {{"dir", direction},
            {"hex", QString::fromLatin1(bytes.toHex(' '))},
            // Deliberately decode each chunk independently as the RPC text field does.
            // The text view must use original hex bytes instead to handle split UTF-8.
            {"text", QString::fromUtf8(bytes)}};
}

// Isolated service with explicitly controlled asynchronous replies.
class PaneRpcFixture final {
  public:
    QTemporaryDir directory;
    QLocalServer server;
    QLocalSocket *peer = nullptr;
    QList<QJsonObject> requests;
    QByteArray oldSocket = qgetenv("FLATTENCOM_SOCKET"), oldState = qgetenv("FLATTENCOM_STATE_DIR");
    PaneRpcFixture() {
#ifdef Q_OS_WIN
        const auto endpoint = "flattencom-pane-" + QString::number(QCoreApplication::applicationPid());
#else
        const auto endpoint = directory.filePath("rpc.sock");
#endif
        qputenv("FLATTENCOM_SOCKET", endpoint.toUtf8());
        qputenv("FLATTENCOM_STATE_DIR", directory.path().toUtf8());
        QFile token(directory.filePath("daemon.token"));
        if (token.open(QIODevice::WriteOnly))
            token.write("test-token");
        server.listen(endpoint);
        QObject::connect(&server, &QLocalServer::newConnection, &server, [this] {
            peer = server.nextPendingConnection();
            auto *socket = peer;
            QObject::connect(socket, &QLocalSocket::readyRead, &server,
                             [this, socket, buffer = QByteArray{}]() mutable {
                                 buffer += socket->readAll();
                                 while (buffer.contains('\n')) {
                                     const auto end = buffer.indexOf('\n');
                                     const auto request = QJsonDocument::fromJson(buffer.left(end)).object();
                                     buffer.remove(0, end + 1);
                                     if (request.value("method") == "hello")
                                         reply(request, {{"proto", 1}});
                                     else
                                         requests.append(request);
                                 }
                             });
        });
    }
    ~PaneRpcFixture() {
        if (oldSocket.isNull())
            qunsetenv("FLATTENCOM_SOCKET");
        else
            qputenv("FLATTENCOM_SOCKET", oldSocket);
        if (oldState.isNull())
            qunsetenv("FLATTENCOM_STATE_DIR");
        else
            qputenv("FLATTENCOM_STATE_DIR", oldState);
    }
    QJsonObject take(const QString &method) {
        for (qsizetype i = 0; i < requests.size(); ++i)
            if (requests[i].value("method") == method)
                return requests.takeAt(i);
        return {};
    }
    bool has(const QString &method) const {
        return std::any_of(requests.begin(), requests.end(),
                           [&](const auto &r) { return r.value("method") == method; });
    }
    void reply(const QJsonObject &request, const QJsonObject &result, const QString &error = {}) {
        QJsonObject response{{"jsonrpc", "2.0"}, {"id", request.value("id")}};
        if (error.isEmpty())
            response["result"] = result;
        else
            response["error"] = QJsonObject{{"code", -32602}, {"message", error}};
        peer->write(QJsonDocument(response).toJson(QJsonDocument::Compact) + '\n');
        peer->flush();
    }
};

class ModelsTest final : public QObject {
    Q_OBJECT
    QTemporaryDir settings_;
  private slots:
    void localSearchSupersedesPendingMarkerNavigation() {
        PaneRpcFixture service;
        QVERIFY(service.server.isListening());
        RpcClient rpc;
        rpc.start();
        QTRY_VERIFY(rpc.ready());
        SessionPane pane(&rpc, {{"session_id", "navigation-test"}});
        pane.setPaused(true);
        auto *stream = pane.findChild<TextStreamView *>();
        auto visible = receiveChunk("visible marker\n");
        visible["seq"] = 100;
        stream->appendFrames({visible});
        pane.findChild<QLineEdit *>("receiveSearch")->setText("visible marker");
        auto *results = pane.findChild<QListWidget *>();
        QCOMPARE(results->count(), 1);
        pane.showMarkers();
        QTRY_VERIFY(service.has("list_markers"));
        service.reply(service.take("list_markers"),
                      {{"markers", QJsonArray{QJsonObject{{"seq", 10}, {"label", "old"}}}}});
        QTRY_VERIFY(!pane.findChildren<QDialog *>().isEmpty());
        auto *markers = pane.findChild<QDialog *>()->findChild<QListWidget *>();
        QVERIFY(markers);
        QVERIFY(QMetaObject::invokeMethod(markers, "itemDoubleClicked", Qt::DirectConnection,
                                          Q_ARG(QListWidgetItem *, markers->item(0))));
        QTRY_VERIFY(service.has("read_frames"));
        const auto pending = service.take("read_frames");
        QCOMPARE(pending.value("params").toObject().value("since_seq").toInt(), 10);
        QVERIFY(QMetaObject::invokeMethod(results, "itemClicked", Qt::DirectConnection,
                                          Q_ARG(QListWidgetItem *, results->item(0))));
        auto cursor = stream->textCursor();
        cursor.select(QTextCursor::LineUnderCursor);
        stream->setTextCursor(cursor);
        auto old = receiveChunk("obsolete response\n");
        old["seq"] = 10;
        service.reply(pending, {{"frames", QJsonArray{old}}, {"first_seq", 0}, {"next_seq", 11}});
        QTest::qWait(100);
        QCOMPARE(stream->toPlainText(), QString("visible marker\n"));
        QCOMPARE(stream->selectedEvidence().value("from_seq").toInt(), 100);
    }
    void decoderInitializationAndFailedChangeRespectNewestIntent() {
        PaneRpcFixture service;
        RpcClient rpc;
        rpc.start();
        QTRY_VERIFY(rpc.ready());
        SessionPane pane(&rpc, {{"session_id", "decoder-test"}});
        pane.setPaused(true);
        QSignalSpy messages(&pane, &SessionPane::message);
        QTRY_VERIFY(service.has("get_decoder"));
        const auto initial = service.take("get_decoder");
        QComboBox *decoder = nullptr;
        for (auto *combo : pane.findChildren<QComboBox *>())
            if (combo->findData("json_lines") >= 0)
                decoder = combo;
        QVERIFY(decoder);
        fc::selectValue(decoder, "json_lines");
        service.reply(initial, {{"spec", QJsonValue::Null}});
        QTRY_VERIFY(service.has("set_decoder"));
        service.reply(service.take("set_decoder"), {});
        QTest::qWait(50);
        QCOMPARE(decoder->currentData().toString(), QString("json_lines"));
        fc::selectValue(decoder, "modbus_rtu");
        QTRY_VERIFY(service.has("set_decoder"));
        service.reply(service.take("set_decoder"), {}, "decoder rejected");
        QTRY_VERIFY(service.has("get_decoder"));
        QVERIFY(!messages.isEmpty());
        QCOMPARE(decoder->toolTip(), QString("decoder rejected"));
        service.reply(service.take("get_decoder"), {{"spec", QJsonObject{{"name", "json_lines"}}}});
        QTRY_COMPARE(decoder->currentData().toString(), QString("json_lines"));
        // A failed-change readback must also yield to subsequent user input.
        fc::selectValue(decoder, "modbus_rtu");
        QTRY_VERIFY(service.has("set_decoder"));
        service.reply(service.take("set_decoder"), {}, "decoder rejected");
        QTRY_VERIFY(service.has("get_decoder"));
        const auto readback = service.take("get_decoder");
        fc::selectValue(decoder, "ascii_lines");
        service.reply(readback, {{"spec", QJsonObject{{"name", "json_lines"}}}});
        QTRY_VERIFY(service.has("set_decoder"));
        service.reply(service.take("set_decoder"), {});
        QTest::qWait(50);
        QCOMPARE(decoder->currentData().toString(), QString("ascii_lines"));
    }
    void cancelledDecoderEditorStillReconcilesInitialState_data() {
        QTest::addColumn<bool>("newerChoice");
        QTest::newRow("cancel") << false;
        QTest::newRow("newer-choice-during-modal") << true;
    }
    void cancelledDecoderEditorStillReconcilesInitialState() {
        QFETCH(bool, newerChoice);
        PaneRpcFixture service;
        RpcClient rpc;
        rpc.start();
        QTRY_VERIFY(rpc.ready());
        SessionPane pane(&rpc, {{"session_id", "decoder-cancel-test"}});
        pane.setPaused(true);
        QTRY_VERIFY(service.has("get_decoder"));
        const auto initial = service.take("get_decoder");
        QComboBox *decoder = nullptr;
        for (auto *combo : pane.findChildren<QComboBox *>())
            if (combo->findData("json_lines") >= 0)
                decoder = combo;
        QVERIFY(decoder);
        pane.editDecoder();
        QTRY_VERIFY(service.has("get_decoder"));
        const auto editorRead = service.take("get_decoder");
        const QJsonObject actual{{"spec", QJsonObject{{"name", "json_lines"}, {"options", QJsonObject{}}}}};
        // Both replies arrive in daemon order after the editor supersedes initialization.
        service.reply(initial, actual);
        QTest::qWait(25);
        bool visited = false;
        QString toolbarWhileEditing;
        QTimer closeEditor;
        connect(&closeEditor, &QTimer::timeout, &pane, [&] {
            auto *dialog = qobject_cast<QDialog *>(QApplication::activeModalWidget());
            if (!dialog)
                return;
            closeEditor.stop();
            visited = true;
            toolbarWhileEditing = decoder->currentData().toString();
            if (newerChoice) {
                fc::selectValue(decoder, "ascii_lines");
                dialog->accept();
            } else
                dialog->reject();
        });
        closeEditor.start(5);
        service.reply(editorRead, actual);
        QTRY_VERIFY(visited);
        QCOMPARE(toolbarWhileEditing, QString("json_lines"));
        if (newerChoice) {
            QTRY_VERIFY(service.has("set_decoder"));
            const auto change = service.take("set_decoder");
            QCOMPARE(change.value("params").toObject().value("spec").toObject().value("name").toString(),
                     QString("ascii_lines"));
            service.reply(change, {});
        }
        QTest::qWait(100);
        QVERIFY(!service.has("set_decoder"));
        QCOMPARE(decoder->currentData().toString(),
                 newerChoice ? QString("ascii_lines") : QString("json_lines"));
    }
    void operationMonitoringRecoversAndStopsOnlyAtTerminal_data() {
        QTest::addColumn<QString>("terminal");
        QTest::newRow("completed") << QString("completed");
        QTest::newRow("service-restarted-en") << QString("operation not found");
        QTest::newRow("service-restarted-zh") << QString::fromUtf8("操作不存在");
    }
    void operationMonitoringRecoversAndStopsOnlyAtTerminal() {
        QFETCH(QString, terminal);
        PaneRpcFixture service;
        RpcClient rpc;
        rpc.start();
        QTRY_VERIFY(rpc.ready());
        SessionPane pane(&rpc, {{"session_id", "operation-test"}});
        pane.setPaused(true);
        QSignalSpy messages(&pane, &SessionPane::message);
        pane.send(QString(5000, 'x'));
        QTRY_VERIFY(service.has("start_transfer"));
        service.reply(service.take("start_transfer"), {{"operation", QJsonObject{{"id", "op"}}}});
        QTRY_VERIFY(service.has("operation_status"));
        auto status = service.take("operation_status");
        // Disconnect with a status request pending: it must not disable monitoring.
        service.peer->disconnectFromServer();
        QTRY_VERIFY(!rpc.ready());
        QTRY_VERIFY_WITH_TIMEOUT(rpc.ready(), 3000);
        QTRY_VERIFY_WITH_TIMEOUT(service.has("operation_status"), 2500);
        service.reply(service.take("operation_status"), {}, "Request timed out");
        QTest::qWait(400);
        QVERIFY(!service.has("operation_status"));
        QTRY_VERIFY_WITH_TIMEOUT(service.has("operation_status"), 2000);
        service.reply(
            service.take("operation_status"),
            {{"operation", QJsonObject{{"state", "running"}, {"bytes_sent", 123}, {"total_bytes", 5000}}}});
        QTRY_VERIFY_WITH_TIMEOUT(service.has("operation_status"), 1000);
        status = service.take("operation_status");
        if (terminal == "completed")
            service.reply(status,
                          {{"operation",
                            QJsonObject{{"state", terminal}, {"bytes_sent", 5000}, {"total_bytes", 5000}}}});
        else
            service.reply(status, {}, terminal);
        auto *dialog = pane.findChild<QDialog *>();
        QVERIFY(dialog);
        auto *cancel = dialog->findChild<QPushButton *>();
        QTRY_VERIFY(!cancel->isEnabled());
        QString labels;
        for (auto *label : dialog->findChildren<QLabel *>())
            labels += label->text();
        QVERIFY(labels.contains(terminal == "completed"
                                    ? fc::text("Completed")
                                    : fc::text("Operation unavailable; the service may have restarted.")));
        QTest::qWait(1100);
        QVERIFY(!service.has("operation_status"));
        for (const auto &message : messages)
            QVERIFY(!message.first().toString().contains("Request timed out"));
    }
    void historyDenseAndOversizedRecordsKeepSearchTargetsAndOverlap() {
        QTemporaryDir directory;
        QFile file(directory.filePath("dense.jsonl"));
        QVERIFY(file.open(QIODevice::WriteOnly));
        for (int i = 0; i < 32; ++i) {
            QByteArray data;
            for (int j = 0; j < 2048; ++j)
                data += "x\n";
            if (i == 0)
                data.prepend("FIRST TARGET\n");
            if (i == 31)
                data += "LAST TARGET\n";
            file.write(
                QJsonDocument(
                    QJsonObject{{"seq", i}, {"dir", "rx"}, {"data", QString::fromLatin1(data.toHex())}})
                    .toJson(QJsonDocument::Compact) +
                '\n');
        }
        // A single legitimate record can exceed either live display cap.
        const auto large = QByteArray(1100000, 'a') + "\nLARGE TARGET\n";
        file.write(QJsonDocument(
                       QJsonObject{{"seq", 32}, {"dir", "rx"}, {"data", QString::fromLatin1(large.toHex())}})
                       .toJson(QJsonDocument::Compact) +
                   '\n');
        file.close();
        SessionHistory history;
        QSignalSpy indexed(&history, &SessionHistory::indexed);
        QSignalSpy windows(&history, &SessionHistory::windowReady);
        QSignalSpy matches(&history, &SessionHistory::matchesReady);
        history.open(file.fileName());
        // Indexing and search run on worker threads; a shared CI runner can need
        // well over the five-second default, so every wait below is bounded but
        // generous.
        QTRY_COMPARE_WITH_TIMEOUT(indexed.count(), 1, 60000);
        history.search(QRegularExpression("TARGET"));
        QTRY_COMPARE_WITH_TIMEOUT(matches.count(), 1, 60000);
        QCOMPARE(matches.last()[0].toJsonArray().size(), 3);
        TextStreamView view;
        view.resize(600, 220);
        view.show();
        for (const auto &value : matches.last()[0].toJsonArray()) {
            const auto result = value.toObject();
            windows.clear();
            history.window(result.value("page").toInt(), 1);
            QTRY_COMPARE_WITH_TIMEOUT(windows.count(), 1, 60000);
            view.showHistory(windows.last()[2].toJsonArray());
            QVERIFY(view.document()->blockCount() <= 12001);
            QVERIFY(view.document()->characterCount() <= 3 * 65537 + 1);
            QVERIFY(view.jumpToHistoryKey(TextStreamView::historyPositionKey(
                result.value("history_key").toString(), result.value("history_offset").toInteger())));
            auto cursor = view.textCursor();
            cursor.movePosition(QTextCursor::NextCharacter, QTextCursor::KeepAnchor, 6);
            QCOMPARE(cursor.selectedText(), QString("TARGET"));
        }
        QString entire;
        QString overlap;
        for (int page = 0; page < history.pages(); ++page) {
            windows.clear();
            history.window(page, 2);
            QTRY_COMPARE_WITH_TIMEOUT(windows.count(), 1, 60000);
            const auto records = windows.last()[2].toJsonArray();
            view.showHistory(records);
            if (page && page < history.pages() - 2)
                QVERIFY(view.jumpToHistoryKey(overlap, true));
            // Keep an anchor from the overlapping last page for the next move.
            const auto last = records.last().toObject();
            overlap = TextStreamView::historyPositionKey(last.value("history_key").toString(),
                                                         last.value("history_offset").toInteger());
            QVERIFY(view.jumpToHistoryKey(overlap, true));
            entire += view.toPlainText();
        }
        QVERIFY(entire.contains("FIRST TARGET"));
        QVERIFY(entire.contains("LAST TARGET"));
        QVERIFY(entire.contains("LARGE TARGET"));
        windows.clear();
        history.window(0, 3);
        QTRY_COMPARE_WITH_TIMEOUT(windows.count(), 1, 60000);
        view.showHistory(windows.last()[2].toJsonArray());
        QCOMPARE(view.firstSequence(), qint64(0));
        QVERIFY(view.toPlainText().startsWith("FIRST TARGET"));
        RpcClient rpc;
        SessionPane pane(&rpc,
                         {{"session_id", "dense"}, {"config", QJsonObject{{"record_to", file.fileName()}}}});
        pane.resize(900, 600);
        pane.show();
        auto *search = pane.findChild<QLineEdit *>("receiveSearch");
        auto *results = pane.findChild<QListWidget *>();
        auto *stream = pane.findChild<TextStreamView *>();
        search->setText("FIRST TARGET");
        QTRY_COMPARE_WITH_TIMEOUT(results->count(), 1, 60000);
        QVERIFY(QMetaObject::invokeMethod(results, "itemClicked", Qt::DirectConnection,
                                          Q_ARG(QListWidgetItem *, results->item(0))));
        QTRY_VERIFY_WITH_TIMEOUT(stream->toPlainText().startsWith("FIRST TARGET"), 60000);
        stream->verticalScrollBar()->setValue(stream->verticalScrollBar()->maximum());
        QTRY_VERIFY_WITH_TIMEOUT(stream->firstSequence() > 0, 60000);
        stream->verticalScrollBar()->setValue(0);
        QTRY_COMPARE_WITH_TIMEOUT(stream->firstSequence(), qint64(0), 60000);
        QVERIFY(stream->toPlainText().startsWith("FIRST TARGET"));
    }
    void unicodeSeparatorsKeepEvidenceAndHistoryOffsets() {
        const auto text = QString::fromUtf8("alpha\u2029beta\u2028gamma\nend");
        for (bool saved : {false, true}) {
            TextStreamView view;
            QJsonObject frame{{"seq", 10},   {"t_us", 100},  {"mono_us", 90},
                              {"dir", "rx"}, {"text", text}, {"history_key", "file:0"}};
            if (saved)
                view.showHistory({frame});
            else
                view.appendFrames({frame});
            view.selectAll();
            QCOMPARE(view.selectedEvidence().value("text").toString(), text);
            for (const auto &needle : {QString("beta"), QString("gamma"), QString("end")}) {
                const auto offset = text.indexOf(needle);
                QVERIFY(view.jumpToHistoryKey(TextStreamView::historyPositionKey("file:0", offset)));
                auto cursor = view.textCursor();
                cursor.movePosition(QTextCursor::NextCharacter, QTextCursor::KeepAnchor, needle.size());
                view.setTextCursor(cursor);
                const auto evidence = view.selectedEvidence();
                QCOMPARE(evidence.value("text").toString(), needle);
                QCOMPARE(evidence.value("from_seq").toInteger(), qint64(10));
                QCOMPARE(evidence.value("to_seq").toInteger(), qint64(11));
                QCOMPARE(evidence.value("from_us").toInteger(), qint64(100));
                QCOMPARE(evidence.value("elapsed_us").toInteger(), qint64(0));
            }
        }
    }
    void readableIndexFastPathPreservesDecoderStateAndRenderedBounds() {
        QTemporaryDir directory;
        QFile file(directory.filePath("mixed.txt"));
        QVERIFY(file.open(QIODevice::WriteOnly));
        QString expected;
        for (int i = 0; i < 6000; ++i) {
            QByteArray payload;
            QString rendered;
            if (i == 1000) {
                rendered = QString(4500, QChar::ParagraphSeparator) + "dense\n";
                payload = rendered.toUtf8();
            } else if (i == 2000) {
                payload = "\x1b]hidden title\ncontinued\x07visible\n";
                rendered = "visible\n";
            } else if (i == 3000) {
                payload = QByteArray(65535, 'x') + QString::fromUtf8("设备\n").toUtf8();
                rendered = QString::fromUtf8(payload);
            } else {
                rendered = QString::fromUtf8("line %1 设备\u2028detail\n").arg(i);
                payload = rendered.toUtf8();
            }
            file.write(QString("[2026-10-04T00:00:00.123Z RX #%1] ").arg(i).toUtf8());
            file.write(payload);
            expected += rendered;
        }
        file.close();
        SessionHistory history;
        QSignalSpy indexed(&history, &SessionHistory::indexed),
            windows(&history, &SessionHistory::windowReady);
        history.open(file.fileName());
        QTRY_COMPARE(indexed.count(), 1);
        QString actual;
        QSet<QString> seen;
        for (int page = 0; page < history.pages(); page += 3) {
            windows.clear();
            history.window(page, 1);
            QTRY_COMPARE(windows.count(), 1);
            qsizetype characters = 0, breaks = 0;
            for (const auto &v : windows.last()[2].toJsonArray()) {
                const auto row = v.toObject();
                const auto text = row.value("text").toString();
                characters += text.size();
                breaks += text.count('\n') + text.count(QChar::ParagraphSeparator) +
                          text.count(QChar::LineSeparator);
                const auto key = TextStreamView::historyPositionKey(row.value("history_key").toString(),
                                                                    row.value("history_offset").toInteger());
                if (!seen.contains(key))
                    actual += text;
                seen.insert(key);
            }
            QVERIFY(characters <= 3 * 65537);
            QVERIFY(breaks <= 12000);
        }
        QCOMPARE(actual, expected);
    }
    void delayedProfileLookupPreservesEditsAndRejectsOldPortGeneration() {
        QTemporaryDir directory;
        const auto oldSocket = qgetenv("FLATTENCOM_SOCKET"), oldState = qgetenv("FLATTENCOM_STATE_DIR");
        const auto oldPort = QSettings().value("lastPort");
        const auto restore = qScopeGuard([&] {
            qputenv("FLATTENCOM_SOCKET", oldSocket);
            qputenv("FLATTENCOM_STATE_DIR", oldState);
            QSettings().setValue("lastPort", oldPort);
        });
#ifdef Q_OS_WIN
        const auto endpoint =
            "flattencom-profile-test-" + QString::number(QCoreApplication::applicationPid());
#else
        const auto endpoint = directory.filePath("profile.sock");
#endif
        qputenv("FLATTENCOM_SOCKET", endpoint.toUtf8());
        qputenv("FLATTENCOM_STATE_DIR", directory.path().toUtf8());
        QFile token(directory.filePath("daemon.token"));
        QVERIFY(token.open(QIODevice::WriteOnly));
        token.write("test-token");
        token.close();
        QLocalServer server;
        QVERIFY(server.listen(endpoint));
        QLocalSocket *peer = nullptr;
        QList<QJsonObject> lookups;
        QJsonObject opened;
        auto respond = [&](const QJsonObject &request, const QJsonObject &result) {
            peer->write(QJsonDocument(QJsonObject{{"id", request.value("id")}, {"result", result}})
                            .toJson(QJsonDocument::Compact) +
                        '\n');
            peer->flush();
        };
        connect(&server, &QLocalServer::newConnection, &server, [&] {
            peer = server.nextPendingConnection();
            connect(peer, &QLocalSocket::readyRead, &server, [&, buffer = QByteArray{}]() mutable {
                buffer += peer->readAll();
                while (buffer.contains('\n')) {
                    const auto end = buffer.indexOf('\n');
                    const auto request = QJsonDocument::fromJson(buffer.left(end)).object();
                    buffer.remove(0, end + 1);
                    const auto method = request.value("method").toString();
                    if (method == "get_port_info")
                        lookups.append(request);
                    else if (method == "open_session")
                        opened = request.value("params").toObject();
                    else
                        respond(request, method == "hello" ? QJsonObject{{"proto", 1}} : QJsonObject{});
                }
            });
        });
        const QJsonObject oldIdentity{{"path", "port-A"}, {"vid", 1}, {"pid", 2}, {"serial", "old"}};
        const QJsonObject newIdentity{{"path", "port-A"}, {"vid", 1}, {"pid", 2}, {"serial", "new"}};
        fc::saveProfile(fc::profileKey(oldIdentity), {{"baud", 9600}, {"parity", "odd"}});
        fc::saveProfile(fc::profileKey(newIdentity), {{"baud", 19200}, {"parity", "even"}});
        QSettings().setValue("lastPort", "port-A");
        MainWindow window;
        QTRY_VERIFY(window.rpc()->ready());
        QAction *open = nullptr;
        for (auto *action : window.findChildren<QAction *>())
            if (action->text() == fc::text("Open port")) {
                open = action;
                break;
            }
        QVERIFY(open);
        bool exercised = false;
        QTimer::singleShot(0, &window, [&] {
            auto *dialog = qobject_cast<QDialog *>(QApplication::activeModalWidget());
            QVERIFY(dialog);
            const auto close = qScopeGuard([&] {
                if (!exercised)
                    dialog->reject();
            });
            auto *port = dialog->findChildren<QLineEdit *>().first();
            QComboBox *baud = nullptr, *parity = nullptr;
            for (auto *combo : dialog->findChildren<QComboBox *>()) {
                if (combo->isEditable())
                    baud = combo;
                if (combo->findData("even") >= 0)
                    parity = combo;
            }
            QVERIFY(baud && parity);
            QTRY_COMPARE(lookups.size(), 1);
            port->setText("port-B");
            QVERIFY(QMetaObject::invokeMethod(port, "editingFinished"));
            QTRY_COMPARE(lookups.size(), 2);
            port->setText("port-A");
            QVERIFY(QMetaObject::invokeMethod(port, "editingFinished"));
            QTRY_COMPARE(lookups.size(), 3);
            baud->setCurrentText("57600");
            respond(lookups[2], {{"port", newIdentity}});
            QTRY_COMPARE(parity->currentData().toString(), QString("even"));
            QCOMPARE(baud->currentText(), QString("57600"));
            respond(lookups[0], {{"port", oldIdentity}});
            respond(lookups[1], {{"port", QJsonObject{{"path", "port-B"}}}});
            QTest::qWait(30);
            QCOMPARE(parity->currentData().toString(), QString("even"));
            QCOMPARE(baud->currentText(), QString("57600"));
            exercised = true;
            dialog->accept();
        });
        open->trigger();
        QVERIFY(exercised);
        QTRY_VERIFY(!opened.isEmpty());
        QCOMPARE(opened.value("baud").toInt(), 57600);
        QCOMPARE(opened.value("parity").toString(), QString("even"));
        QCOMPARE(opened.value("path").toString(), QString("port-A"));
        window.close();
    }
    void initTestCase() {
        QVERIFY(settings_.isValid());
        QCoreApplication::setOrganizationName("flattencom-tests");
        QCoreApplication::setApplicationName("receive-view-tests");
        QSettings::setDefaultFormat(QSettings::IniFormat);
        QSettings::setPath(QSettings::IniFormat, QSettings::UserScope, settings_.path());
    }
    void languageDefaultsToEnglishAndChineseCatalogWorks() {
        QSettings().remove("language");
        QCOMPARE(fc::text("Send"), QString("Send"));
        fc::setLanguage("zh-CN");
        QCOMPARE(fc::text("Send"), QString::fromUtf8("发送"));
        QFile catalog(":/flattencom/zh_CN.json");
        QVERIFY(catalog.open(QIODevice::ReadOnly));
        const auto translations = QJsonDocument::fromJson(catalog.readAll()).object();
        for (auto it = translations.begin(); it != translations.end(); ++it)
            QCOMPARE(fc::text(it.key().toUtf8().constData()), it.value().toString());
        QCOMPARE(fc::text("untranslated identifier"), QString("untranslated identifier"));
        fc::setLanguage("en");
        QCOMPARE(fc::text("Send"), QString("Send"));
    }
    void themeFollowsSystemAndPreservesManualPreference() {
        const auto oldTheme = QSettings().value("theme");
        const auto oldDark = QSettings().value("dark");
        const auto restore = qScopeGuard([&] {
            QSettings settings;
            settings.remove("theme");
            settings.remove("dark");
            if (oldTheme.isValid())
                settings.setValue("theme", oldTheme);
            if (oldDark.isValid())
                settings.setValue("dark", oldDark);
            fc::applyStyle(*qApp, false);
        });
        QSettings().remove("theme");
        QSettings().remove("dark");
        QCOMPARE(fc::themeMode(), QString("system"));
        QSettings().setValue("dark", true);
        fc::initializeTheme(*qApp);
        QCOMPARE(fc::themeMode(), QString("dark"));
        QVERIFY(!QSettings().contains("dark"));
        auto changed = [](Qt::ColorScheme scheme) {
            return QMetaObject::invokeMethod(qApp->styleHints(), "colorSchemeChanged", Qt::DirectConnection,
                                             Q_ARG(Qt::ColorScheme, scheme));
        };
        fc::setThemeMode(*qApp, "system");
        QVERIFY(changed(Qt::ColorScheme::Dark));
        QTRY_VERIFY(qApp->palette().color(QPalette::Window).lightness() < 128);
        QVERIFY(changed(Qt::ColorScheme::Light));
        QTRY_VERIFY(qApp->palette().color(QPalette::Window).lightness() >= 128);
        fc::setThemeMode(*qApp, "dark");
        QVERIFY(changed(Qt::ColorScheme::Light));
        QCoreApplication::processEvents();
        QVERIFY(qApp->palette().color(QPalette::Window).lightness() < 128);
        fc::setThemeMode(*qApp, "light");
        QVERIFY(changed(Qt::ColorScheme::Dark));
        QCoreApplication::processEvents();
        QVERIFY(qApp->palette().color(QPalette::Window).lightness() >= 128);
        fc::initializeTheme(*qApp);
        QCOMPARE(fc::themeMode(), QString("light"));
        fc::setThemeMode(*qApp, "system");
        QVERIFY(changed(Qt::ColorScheme::Unknown));
        QTRY_VERIFY(qApp->palette().color(QPalette::Window).lightness() >= 128);
    }
    void aboutArtworkTracksThemeAndPreservesAttribution_data() {
        QTest::addColumn<QString>("language");
        QTest::newRow("english") << QString("en");
        QTest::newRow("chinese") << QString("zh-CN");
    }
    void aboutArtworkTracksThemeAndPreservesAttribution() {
        QFETCH(QString, language);
        const auto restore = qScopeGuard([] {
            fc::setLanguage("en");
            fc::applyStyle(*qApp, false);
        });
        fc::setLanguage(language);
        fc::applyStyle(*qApp, false);
        AboutDialog dialog;
        dialog.setAttribute(Qt::WA_DeleteOnClose, false);
        dialog.show();
        auto *logo = dialog.findChild<QLabel *>("aboutLogo");
        auto *details = dialog.findChild<QLabel *>("aboutDetails");
        QVERIFY(logo && details);
        const QImage light(":/flattencom/branding/logo-light.png");
        const QImage dark(":/flattencom/branding/logo-dark.png");
        QVERIFY(!light.isNull() && !dark.isNull());
        QVERIFY(light != dark);
        auto displayedInk = [&] {
            const auto image = logo->pixmap().toImage();
            // Sample the solid stem of the top-row l, away from antialiased edges.
            return image.pixelColor(qRound(image.width() * .24), qRound(image.height() * .23));
        };
        QCOMPARE(displayedInk(), QColor("#252723"));
        QVERIFY(details->text().contains(fc::text("Serial Workbench")));
        QVERIFY(!details->text().contains("worryzu", Qt::CaseInsensitive));
        QVERIFY(!details->text().contains("LinearTeam", Qt::CaseInsensitive));
        QVERIFY(!details->text().contains("mailto:", Qt::CaseInsensitive));
        QVERIFY(!details->text().contains(fc::text("Author")));
        QVERIFY(details->text().contains("GPL-3.0-or-later"));
        QVERIFY(details->text().contains("Copyright (C) 2026 Evarentha"));
        QVERIFY(details->openExternalLinks());
        fc::applyStyle(*qApp, true);
        QTRY_COMPARE(displayedInk(), QColor("#EAE6DC"));
        QVERIFY(details->palette().color(QPalette::Link).lightness() > 100);
        QVERIFY(details->text().contains("color:#46b3c2"));
        fc::applyStyle(*qApp, false);
        QTRY_COMPARE(displayedInk(), QColor("#252723"));
        for (const int size : {16, 24, 32, 48, 64, 128, 256, 512, 1024}) {
            const QImage icon(QString(":/flattencom/branding/icon-%1.png").arg(size));
            QVERIFY(!icon.isNull());
            QCOMPARE(icon.size(), QSize(size, size));
            QCOMPARE(icon.pixelColor(size / 2, size / 8), QColor("#D94B27"));
        }
    }
    void retranslationPreservesUserDataAndComboSelection() {
        const auto restore = qScopeGuard([] { fc::setLanguage("en"); });
        fc::setLanguage("en");
        QWidget root;
        auto *button = new QPushButton(fc::text("Reset dock layout"), &root);
        auto *input = new QLineEdit("Connected", &root);
        input->setPlaceholderText(fc::text("Search logs"));
        auto *output = new QPlainTextEdit(&root);
        output->setPlainText("Connected\n设备原文\n");
        output->selectAll();
        auto *combo = new QComboBox(&root);
        fc::addValues(combo, {"none", "even", "odd"});
        fc::selectValue(combo, "even");
        QSignalSpy changes(combo, &QComboBox::currentIndexChanged);
        fc::setLanguage("zh");
        fc::retranslateUi(&root, "en");
        QCOMPARE(button->text(), QString::fromUtf8("重置停靠布局"));
        QCOMPARE(input->text(), QString("Connected"));
        QCOMPARE(combo->currentData().toString(), QString("even"));
        QCOMPARE(combo->currentText(), QString::fromUtf8("偶校验"));
        QCOMPARE(changes.count(), 0);
        QVERIFY(output->textCursor().hasSelection());
        QCOMPARE(output->toPlainText(), QString::fromUtf8("Connected\n设备原文\n"));
        fc::setLanguage("en");
        fc::retranslateUi(&root, "zh");
        QCOMPARE(button->text(), QString("Reset dock layout"));
    }
    void translationIdentitySurvivesCollisionsFromEitherLanguage() {
        for (const auto &initial : QStringList{"en", "zh"}) {
            fc::setLanguage(initial);
            QWidget root;
            auto *tree = new QTreeWidget(&root);
            tree->setHeaderLabels({fc::text("Field"), fc::text("Fields")});
            for (const auto &target : QStringList{initial == "en" ? "zh" : "en", initial, "en"}) {
                const auto previous = fc::language();
                fc::setLanguage(target);
                fc::retranslateUi(&root, previous);
            }
            QCOMPARE(tree->headerItem()->text(0), QString("Field"));
            QCOMPARE(tree->headerItem()->text(1), QString("Fields"));
        }
    }
    void historyLongTxRetainsUtf8AcrossPages() {
        QTemporaryDir directory;
        QFile file(directory.filePath("long.txt"));
        QVERIFY(file.open(QIODevice::WriteOnly));
        file.write("[2026-10-04T00:00:00.000Z TX #0 source=cli bytes=65542]\n");
        file.write(QByteArray(65535, 'a'));
        file.write(QString::fromUtf8("设备\n").toUtf8());
        file.close();
        SessionHistory history;
        QSignalSpy indexed(&history, &SessionHistory::indexed);
        QSignalSpy windows(&history, &SessionHistory::windowReady);
        history.open(file.fileName());
        QTRY_COMPARE(indexed.count(), 1);
        history.window(0, 1);
        QTRY_COMPARE(windows.count(), 1);
        QString text;
        for (const auto &v : windows.last()[2].toJsonArray())
            text += v.toObject().value("text").toString();
        QVERIFY(text.endsWith(QString::fromUtf8("设备\n")));
        QVERIFY(!text.contains(QChar::ReplacementCharacter));
    }
    void toolbarIconsIgnoreDesktopTheme() {
        const auto previous = QIcon::themeName();
        const auto restore = qScopeGuard([previous] { QIcon::setThemeName(previous); });
        const auto icon = fc::icon(fc::Icon::Configure);
        QIcon::setThemeName("breeze");
        const auto first = icon.pixmap(QSize(36, 36)).toImage();
        QIcon::setThemeName("missing-test-theme");
        const auto second = icon.pixmap(QSize(36, 36)).toImage();
        QVERIFY(!first.isNull());
        QCOMPARE(first, second);
        bool visible = false;
        for (int y = 0; y < first.height(); ++y)
            for (int x = 0; x < first.width(); ++x)
                visible |= qAlpha(first.pixel(x, y)) > 0;
        QVERIFY(visible);
    }
    void inspectorModesRetainFullValuesAndRememberSelection() {
        QSettings().remove("inspector/view");
        const auto restore = qScopeGuard([] { QSettings().remove("inspector/view"); });
        InspectorDetails details;
        auto *tabs = details.findChild<QTabWidget *>("inspectorMode");
        auto *json = details.findChild<QPlainTextEdit *>("inspectorJson");
        auto *fields = details.findChild<QTreeWidget *>("inspectorFields");
        QCOMPARE(tabs->currentIndex(), 0);
        const QString payload = QString(20000, 'x') + "END_OF_LONG_VALUE\nnext line";
        const QJsonObject frame{
            {"seq", 42}, {"text", payload}, {"decoded_fields", QJsonObject{{"custom", "nested value"}}}};
        details.setFrame(frame);
        QCOMPARE(QJsonDocument::fromJson(json->toPlainText().toUtf8()).object(), frame);
        QVERIFY(json->maximumHeight() > 160);
        tabs->setCurrentIndex(1);
        auto items = fields->findItems(fc::text("Text"), Qt::MatchExactly, 0);
        QCOMPARE(items.size(), 1);
        QCOMPARE(items.first()->text(1), payload);
        QCOMPARE(fields->textElideMode(), Qt::ElideNone);
        fields->setCurrentItem(items.first());
        details.findChild<QPushButton *>("copyInspectorValue")->click();
        QCOMPARE(QApplication::clipboard()->text(), payload);
        InspectorDetails reopened;
        QCOMPARE(reopened.findChild<QTabWidget *>("inspectorMode")->currentIndex(), 1);
        auto *query = details.findChild<QLineEdit *>("inspectorSearch");
        query->setText("END_OF_LONG_VALUE");
        QTest::keyClick(query, Qt::Key_Return);
        QCOMPARE(tabs->currentIndex(), 0);
        QCOMPARE(json->textCursor().selectedText(), QString("END_OF_LONG_VALUE"));
        details.clear();
        QVERIFY(json->toPlainText().isEmpty());
        QCOMPARE(fields->topLevelItemCount(), 0);
    }
    void translatedOptionsKeepProtocolValues() {
        const auto restore = qScopeGuard([] { fc::setLanguage("en"); });
        fc::setLanguage("zh-CN");
        InspectorDetails details;
        const QJsonObject record{{"seq", 123}, {"text", "original payload"}, {"vendor_key", "keep me"}};
        details.setFrame(record);
        auto *fields = details.findChild<QTreeWidget *>("inspectorFields");
        const auto sequence = fields->findItems(fc::text("Sequence"), Qt::MatchExactly, 0);
        QCOMPARE(sequence.size(), 1);
        QCOMPARE(sequence.first()->data(0, Qt::UserRole).toString(), QString("seq"));
        QCOMPARE(fields->findItems("vendor_key", Qt::MatchExactly, 0).size(), 1);
        QCOMPARE(
            QJsonDocument::fromJson(details.findChild<QPlainTextEdit *>()->toPlainText().toUtf8()).object(),
            record);
        QComboBox parity;
        fc::addValues(&parity, {"none", "even", "odd", "mark", "space"});
        QCOMPARE(parity.currentText(), QString::fromUtf8("无"));
        fc::selectValue(&parity, "even");
        QCOMPARE(parity.currentText(), QString::fromUtf8("偶校验"));
        for (const auto &value : {"mark", "space"}) {
            fc::selectValue(&parity, value);
            QCOMPARE(parity.currentData().toString(), QString(value));
            QVERIFY(parity.currentText().contains(value == QString("mark") ? '1' : '0'));
        }
        fc::selectValue(&parity, "even");
        QComboBox stop;
        fc::addValues(&stop, {"one", "one_point_five", "two"});
        fc::selectValue(&stop, "one_point_five");
        QCOMPARE(stop.currentText(), QString("1.5"));
        QCOMPARE(stop.currentData().toString(), QString("one_point_five"));
        QCOMPARE(parity.currentData().toString(), QString("even"));
        QCOMPARE(fc::valueLabel("closed"), QString::fromUtf8("已关闭"));
        QCOMPARE(fc::valueLabel("vendor_decoder"), QString("vendor_decoder"));
        RpcClient rpc;
        SessionPane pane(&rpc, {{"session_id", "translation-test"}, {"send_newline", "None"}});
        auto *ending = pane.findChild<QComboBox *>("sendNewline");
        QVERIFY(ending);
        QCOMPARE(ending->currentText(), QString::fromUtf8("无"));
        QCOMPARE(ending->currentData().toString(), QString("None"));
    }
    void automaticRecordingUsesDistinctPortableFilenamesAndCanBeDisabled() {
        QSettings().remove("recording/enabled");
        QVERIFY(fc::autoRecordingEnabled());
        QTemporaryDir directory;
        QJsonObject first{{"path", "/dev/ttyUSB0"}}, second{{"path", "/dev/ttyUSB0"}};
        fc::applyAutoRecording(first, true, directory.path());
        fc::applyAutoRecording(second, true, directory.path());
        QVERIFY(first.value("record_to") != second.value("record_to"));
        const QFileInfo rx(first.value("record_to").toString());
        QCOMPARE(rx.absolutePath(), directory.path());
        QVERIFY(rx.fileName().endsWith(".log"));
        QVERIFY(!rx.fileName().contains(':'));
        QVERIFY(!first.contains("record_rx_to"));
        fc::applyAutoRecording(first, false, directory.path());
        QVERIFY(!first.contains("record_to"));
        QVERIFY(!first.contains("record_rx_to"));
    }
    void capturePagesReachBeginningAndEndWithoutDroppingShortLines() {
        QTemporaryDir directory;
        QFile file(directory.filePath("capture.txt"));
        QVERIFY(file.open(QIODevice::WriteOnly));
        QByteArray data("START\n");
        data += QByteArray("x\n").repeated(180000);
        data += QString::fromUtf8("末尾\nEND\n").toUtf8();
        QCOMPARE(file.write(data), data.size());
        file.close();
        CaptureViewer viewer;
        QVERIFY(viewer.openFile(file.fileName()));
        auto *page = viewer.findChild<QPlainTextEdit *>("capturePage");
        QVERIFY(page->toPlainText().startsWith("START\n"));
        QVERIFY(page->document()->blockCount() > 20000);
        QString combined;
        qint64 expected = 0;
        do {
            QCOMPARE(viewer.startOffset(), expected);
            combined += page->toPlainText();
            expected = viewer.endOffset();
            if (expected == viewer.snapshotSize())
                break;
            viewer.nextPage();
        } while (true);
        QCOMPARE(combined, QString::fromUtf8(data));
        viewer.previousPage();
        QVERIFY(viewer.startOffset() < expected);
        viewer.firstPage();
        QCOMPARE(viewer.startOffset(), qint64(0));
        viewer.lastPage();
        QVERIFY(page->toPlainText().endsWith("END\n"));
    }
    void sessionHistoryIndexesSegmentsAndLocatesOlderSequences_data() {
        QTest::addColumn<QString>("extension");
        QTest::newRow("log") << "log";
        QTest::newRow("legacy-txt") << "txt";
        QTest::newRow("uppercase-log") << "LOG";
    }
    void sessionHistoryIndexesSegmentsAndLocatesOlderSequences() {
        QFETCH(QString, extension);
        QTemporaryDir directory;
        const auto path = directory.filePath("session." + extension);
        auto write = [&](const QString &name, int start, int end) {
            QFile file(name);
            QVERIFY(file.open(QIODevice::WriteOnly));
            for (int i = start; i < end; ++i)
                file.write(QString("[2026-10-04T00:00:00.000Z RX #%1] line %1\n").arg(i).toUtf8());
        };
        write(path, 0, 2500);
        write(directory.filePath("session-part-000001." + extension), 2500, 5000);
        SessionHistory history;
        QSignalSpy indexed(&history, &SessionHistory::indexed);
        QSignalSpy windows(&history, &SessionHistory::windowReady);
        history.open(path);
        QTRY_COMPARE(indexed.count(), 1);
        QVERIFY(history.pages() > 3);
        history.locate(20, 7);
        QTRY_COMPARE(windows.count(), 1);
        QCOMPARE(windows.last()[3].toULongLong(), quint64(7));
        const auto rows = windows.last()[2].toJsonArray();
        QVERIFY(rows.first().toObject().value("text").toString().startsWith("line 0"));
        TextStreamView view;
        view.showHistory(rows);
        QVERIFY(view.jumpToSequence(20));
        history.locate(4900, 8);
        QTRY_COMPARE(windows.count(), 2);
        view.showHistory(windows.last()[2].toJsonArray());
        QVERIFY(view.jumpToSequence(4900));
        QSignalSpy matches(&history, &SessionHistory::matchesReady);
        history.search(QRegularExpression("^line 10$"));
        QTRY_COMPARE(matches.count(), 1);
        QCOMPARE(matches.last()[0].toJsonArray().size(), 1);
        QCOMPARE(matches.last()[0].toJsonArray().first().toObject().value("seq").toInt(), 10);
        QFile append(directory.filePath("session-part-000001." + extension));
        QVERIFY(append.open(QIODevice::Append));
        append.write("[2026-10-04T00:00:01.000Z RX #5000] appended\n");
        append.close();
        history.open(path);
        QTRY_COMPARE(indexed.count(), 2);
        history.locate(5000, 9);
        QTRY_COMPARE(windows.count(), 3);
        view.showHistory(windows.last()[2].toJsonArray());
        QVERIFY(view.toPlainText().contains("appended"));
    }
    void historyDistinguishesReadableLogFromLegacyRawAndJson() {
        QTemporaryDir directory;
        const auto path = directory.filePath("capture.log");
        auto write = [&](const QByteArray &bytes) {
            QFile file(path);
            QVERIFY(file.open(QIODevice::WriteOnly));
            QCOMPARE(file.write(bytes), qint64(bytes.size()));
        };
        write(QString::fromUtf8(
                  "flattencom 收发记录\n时间为主机接收时间\n\n[2026-10-04T00:00:00.000Z RX #12] readable\n")
                  .toUtf8());
        {
            SessionHistory history;
            QSignalSpy ready(&history, &SessionHistory::indexed),
                windows(&history, &SessionHistory::windowReady);
            history.open(path);
            QTRY_COMPARE(ready.count(), 1);
            history.window(0, 1);
            QTRY_COMPARE(windows.count(), 1);
            const auto row = windows.last()[2].toJsonArray().last().toObject();
            QCOMPARE(row.value("seq").toInteger(), qint64(12));
            QCOMPARE(row.value("text").toString(), QString("readable\n"));
        }
        const QByteArray raw("[2026-10-04T00:00:00.000Z RX #12] device text\n");
        write(raw);
        {
            SessionHistory history;
            QSignalSpy ready(&history, &SessionHistory::indexed),
                windows(&history, &SessionHistory::windowReady);
            history.open(path, true);
            QTRY_COMPARE(ready.count(), 1);
            history.window(0, 1);
            QTRY_COMPARE(windows.count(), 1);
            const auto row = windows.last()[2].toJsonArray().first().toObject();
            QVERIFY(!row.contains("seq"));
            QCOMPARE(row.value("text").toString(), QString::fromUtf8(raw));
        }
        write("{\"seq\":7,\"dir\":\"rx\",\"data\":\"6F6C640A\",\"t_us\":1,\"mono_us\":1}\n");
        {
            SessionHistory history;
            QSignalSpy ready(&history, &SessionHistory::indexed),
                windows(&history, &SessionHistory::windowReady);
            history.open(path);
            QTRY_COMPARE(ready.count(), 1);
            history.window(0, 1);
            QTRY_COMPARE(windows.count(), 1);
            const auto row = windows.last()[2].toJsonArray().first().toObject();
            QCOMPARE(row.value("seq").toInteger(), qint64(7));
            QCOMPARE(row.value("text").toString(), QString("old\n"));
        }
    }
    void historyDiscoversGrowingLog_data() {
        QTest::addColumn<QByteArray>("contents");
        QTest::addColumn<bool>("raw");
        QTest::addColumn<QString>("expected");
        const QByteArray row("[2026-10-04T00:00:00.000Z RX #42] received\n");
        QTest::newRow("english-header")
            << QByteArray("flattencom capture\n\n") + row << false << QString("received\n");
        QTest::newRow("chinese-header")
            << QString::fromUtf8("flattencom 收发记录\n\n").toUtf8() + row << false << QString("received\n");
        QTest::newRow("frame-header") << row << false << QString("received\n");
        const QByteArray json("{\"seq\":42,\"dir\":\"rx\",\"data\":\"72656365697665640a\",\"t_us\":1}\n");
        QTest::newRow("legacy-jsonl") << json << false << QString("received\n");
        QTest::newRow("explicit-raw-readable") << row << true << QString::fromUtf8(row);
        QTest::newRow("explicit-raw-jsonl") << json << true << QString::fromUtf8(json);
        QTest::newRow("legacy-raw") << QByteArray("device output\n") << false << QString("device output\n");
    }
    void historyDiscoversGrowingLog() {
        QFETCH(QByteArray, contents);
        QFETCH(bool, raw);
        QFETCH(QString, expected);
        QTemporaryDir directory;
        QFile file(directory.filePath("growing.log"));
        QVERIFY(file.open(QIODevice::WriteOnly));
        SessionHistory history;
        QSignalSpy indexed(&history, &SessionHistory::indexed);
        QSignalSpy windows(&history, &SessionHistory::windowReady);
        QSignalSpy failures(&history, &SessionHistory::failed);
        history.open(file.fileName(), raw);
        QTRY_COMPARE(indexed.count(), 1);
        QCOMPARE(history.pages(), 0);
        // Exercise every byte split, including partial Chinese UTF-8, JSON keys,
        // the sequence number and the closing bracket of the first RX header.
        for (qsizetype i = 0; i < contents.size(); ++i) {
            QCOMPARE(file.write(contents.mid(i, 1)), qint64(1));
            QVERIFY(file.flush());
            history.open(file.fileName(), raw);
            QTRY_COMPARE(indexed.count(), i + 2);
        }
        history.locate(42, 1);
        QTRY_COMPARE(windows.count(), 1);
        const auto rows = windows.last()[2].toJsonArray();
        QVERIFY(!rows.isEmpty());
        QString text;
        for (const auto &value : rows)
            text += value.toObject().value("text").toString();
        QCOMPARE(text, expected);
        if (!raw && expected == "received\n") {
            QCOMPARE(rows.last().toObject().value("seq").toInteger(-1), qint64(42));
            TextStreamView view;
            view.showHistory(rows);
            QVERIFY(view.jumpToSequence(42));
        } else {
            QVERIFY(!rows.last().toObject().contains("seq"));
        }
        QVERIFY(failures.isEmpty());
    }
    void historyProbesLargeFirstJsonRecord_data() {
        QTest::addColumn<int>("payloadSize");
        QTest::addColumn<bool>("growing");
        QTest::newRow("normal-frame") << 4096 << false;
        QTest::newRow("growing-normal-frame") << 4096 << true;
        QTest::newRow("large-frame") << 65536 << false;
        QTest::newRow("growing-large-frame") << 65536 << true;
        QTest::newRow("near-record-limit") << 2 * 1024 * 1024 - 64 << false;
    }
    void historyProbesLargeFirstJsonRecord() {
        QFETCH(int, payloadSize);
        QFETCH(bool, growing);
        QTemporaryDir directory;
        QFile file(directory.filePath("legacy.log"));
        QVERIFY(file.open(QIODevice::WriteOnly));
        const QByteArray payload = QByteArray(payloadSize - 1, 'x') + '\n';
        // Keep the data before the metadata so a truncated probe cannot discover
        // seq/dir before the complete object has arrived.
        const auto record =
            QByteArray("{\"data\":\"") + payload.toHex() + "\",\"seq\":42,\"dir\":\"rx\",\"t_us\":1}\n";
        QVERIFY(record.size() > 8192);
        SessionHistory history;
        QSignalSpy indexed(&history, &SessionHistory::indexed);
        QSignalSpy windows(&history, &SessionHistory::windowReady);
        QSignalSpy failures(&history, &SessionHistory::failed);
        int refreshes = 0;
        if (growing) {
            history.open(file.fileName());
            ++refreshes;
            QTRY_COMPARE(indexed.count(), refreshes);
            QCOMPARE(file.write(record.left(8200)), qint64(8200));
            QVERIFY(file.flush());
            history.open(file.fileName());
            ++refreshes;
            QTRY_COMPARE(indexed.count(), refreshes);
            QCOMPARE(history.pages(), 0);
            QCOMPARE(file.write(record.mid(8200, record.size() - 8202)), qint64(record.size() - 8202));
            QVERIFY(file.flush());
            history.open(file.fileName());
            ++refreshes;
            QTRY_COMPARE(indexed.count(), refreshes);
            QCOMPARE(history.pages(), 0);
            QCOMPARE(file.write(record.right(2)), qint64(2));
        } else {
            QCOMPARE(file.write(record), qint64(record.size()));
        }
        file.close();
        history.open(file.fileName());
        ++refreshes;
        QTRY_COMPARE(indexed.count(), refreshes);
        history.locate(42, 1);
        QTRY_COMPARE(windows.count(), 1);
        const auto rows = windows.last()[2].toJsonArray();
        QVERIFY(!rows.isEmpty());
        QCOMPARE(rows.first().toObject().value("seq").toInteger(-1), qint64(42));
        QString reconstructed;
        for (int page = 0; page < history.pages(); page += 3) {
            windows.clear();
            history.window(page, 2);
            QTRY_COMPARE(windows.count(), 1);
            for (const auto &value : windows.last()[2].toJsonArray()) {
                const auto row = value.toObject();
                const auto offset = row.value("history_offset").toInteger();
                const auto text = row.value("text").toString();
                // The final window is clamped to three pages and can overlap.
                QCOMPARE(text, QString::fromUtf8(payload).mid(offset, text.size()));
                if (offset + text.size() > reconstructed.size())
                    reconstructed += text.mid(qMax(qint64(0), reconstructed.size() - offset));
            }
        }
        QCOMPARE(reconstructed, QString::fromUtf8(payload));
        QVERIFY(failures.isEmpty());
    }
    void historyDetectsSameSizeRewriteBeyondFingerprint() {
        QTemporaryDir directory;
        const auto path = directory.filePath("capture.log");
        const QByteArray prefix = QByteArray("flattencom capture\n") + QByteArray(300, '-') + '\n';
        auto contents = [&](int first) {
            QByteArray bytes = prefix;
            for (int i = 0; i < 5000; ++i)
                bytes += "[2026-10-04T00:00:00.000Z RX #" + QByteArray::number(first + i) + "] line\n";
            return bytes;
        };
        const auto original = contents(10000), replacement = contents(20000);
        QCOMPARE(original.size(), replacement.size());
        QCOMPARE(original.left(256), replacement.left(256));
        QFile file(path);
        QVERIFY(file.open(QIODevice::WriteOnly));
        QCOMPARE(file.write(original), qint64(original.size()));
        QVERIFY(file.flush());
        const auto oldTime = QDateTime::currentDateTimeUtc().addSecs(-60);
        QVERIFY(file.setFileTime(oldTime, QFileDevice::FileModificationTime));
        file.close();
        SessionHistory history;
        QSignalSpy indexed(&history, &SessionHistory::indexed);
        QSignalSpy windows(&history, &SessionHistory::windowReady);
        QSignalSpy failures(&history, &SessionHistory::failed);
        history.open(path);
        QTRY_COMPARE(indexed.count(), 1);
        QVERIFY(history.pages() > 3);
        QVERIFY(file.open(QIODevice::WriteOnly));
        QCOMPARE(file.write(replacement), qint64(replacement.size()));
        QVERIFY(file.flush());
        QVERIFY(file.setFileTime(oldTime.addSecs(30), QFileDevice::FileModificationTime));
        file.close();
        // A window requested before refresh must not mix old page context with
        // rewritten bytes. Refresh must rebuild the sequence navigation index.
        history.window(1, 1);
        QTRY_COMPARE(failures.count(), 1);
        QVERIFY(windows.isEmpty());
        history.open(path);
        QTRY_COMPARE(indexed.count(), 2);
        history.locate(20010, 2);
        QTRY_COMPARE(windows.count(), 1);
        TextStreamView view;
        view.showHistory(windows.last()[2].toJsonArray());
        QVERIFY(view.jumpToSequence(20010));
        QVERIFY(!view.jumpToSequence(10010));
        QCOMPARE(failures.count(), 1);
    }
    void historyReprobesReplacedLogAndLegacyRotation() {
        QTemporaryDir directory;
        const auto path = directory.filePath("capture.log");
        QFile file(path);
        QVERIFY(file.open(QIODevice::WriteOnly));
        file.write("raw device output\n");
        file.close();
        SessionHistory history;
        QSignalSpy indexed(&history, &SessionHistory::indexed);
        QSignalSpy windows(&history, &SessionHistory::windowReady);
        history.open(path);
        QTRY_COMPARE(indexed.count(), 1);
        QVERIFY(file.open(QIODevice::WriteOnly));
        file.write("{\"seq\":42,\"dir\":\"rx\",\"data\":\"6e65770a\"}\n");
        file.close();
        QFile backup(path + ".1");
        QVERIFY(backup.open(QIODevice::WriteOnly));
        backup.write("{\"seq\":41,\"dir\":\"rx\",\"data\":\"6f6c640a\"}\n");
        backup.close();
        history.open(path);
        QTRY_COMPARE(indexed.count(), 2);
        history.locate(41, 1);
        QTRY_COMPARE(windows.count(), 1);
        const auto rows = windows.last()[2].toJsonArray();
        QCOMPARE(rows.size(), 2);
        QCOMPARE(rows.first().toObject().value("seq").toInteger(), qint64(41));
        QCOMPARE(rows.last().toObject().value("text").toString(), QString("new\n"));
    }
    void historySearchDoesNotCancelPendingIndex() {
        QTemporaryDir directory;
        QFile file(directory.filePath("capture.log"));
        QVERIFY(file.open(QIODevice::WriteOnly));
        file.write("[2026-10-04T00:00:00.000Z RX #42] received\n");
        file.close();
        SessionHistory history;
        QSignalSpy indexed(&history, &SessionHistory::indexed);
        QSignalSpy matches(&history, &SessionHistory::matchesReady);
        QSignalSpy windows(&history, &SessionHistory::windowReady);
        connect(&history, &SessionHistory::indexed, &history, [&] { history.locate(42, 7); });
        history.open(file.fileName());
        history.search(QRegularExpression("received"));
        QTRY_COMPARE(indexed.count(), 1);
        QTRY_COMPARE(matches.count(), 1);
        QTRY_COMPARE(windows.count(), 1);
        QCOMPARE(windows.last()[2].toJsonArray().last().toObject().value("seq").toInteger(), qint64(42));
    }
    void historyDiscardsErrorsFromSupersededSearches() {
        QTemporaryDir directory;
        QFile file(directory.filePath("valid.log"));
        QVERIFY(file.open(QIODevice::WriteOnly));
        file.write("valid\n");
        file.close();
        SessionHistory history;
        QSignalSpy failures(&history, &SessionHistory::failed);
        QSignalSpy matches(&history, &SessionHistory::matchesReady);
        QSignalSpy indexed(&history, &SessionHistory::indexed);
        history.open(directory.filePath("missing.log"));
        QTRY_COMPARE(failures.count(), 1);
        failures.clear();
        history.search(QRegularExpression("old"));
        history.open(file.fileName());
        history.search(QRegularExpression("valid"));
        QTRY_COMPARE(matches.count(), 1);
        QCOMPARE(matches.last()[0].toJsonArray().size(), 1);
        QVERIFY(failures.isEmpty());
    }
    void largeSavedHistoryLoadsAndRefreshes() {
        QTemporaryDir directory;
        const auto path = directory.filePath("large.txt");
        QFile file(path);
        QVERIFY(file.open(QIODevice::WriteOnly));
        QByteArray batch;
        constexpr int lines = 600000;
        for (int i = 0; i < lines; ++i) {
            batch += "[2026-10-04T00:00:00.123Z RX #" + QByteArray::number(i) +
                     "] kernel: received packet from serial device; payload remains intact\n";
            if (batch.size() >= 1024 * 1024) {
                QCOMPARE(file.write(batch), qint64(batch.size()));
                batch.clear();
            }
        }
        QCOMPARE(file.write(batch), qint64(batch.size()));
        file.close();
        SessionHistory history;
        QSignalSpy indexed(&history, &SessionHistory::indexed);
        QSignalSpy windows(&history, &SessionHistory::windowReady);
        QElapsedTimer timer;
        timer.start();
        history.open(path);
        QTRY_COMPARE_WITH_TIMEOUT(indexed.count(), 1, 30000);
        const auto indexingMs = timer.restart();
        history.locate(lines - 10, 1);
        QTRY_COMPARE(windows.count(), 1);
        const auto readingMs = timer.restart();
        TextStreamView view;
        view.resize(900, 600);
        view.show();
        view.showHistory(windows.last()[2].toJsonArray());
        QCoreApplication::processEvents();
        const auto renderingMs = timer.restart();
        QVERIFY(view.jumpToSequence(lines - 10));
        auto cursor = view.textCursor();
        cursor.select(QTextCursor::LineUnderCursor);
        view.setTextCursor(cursor);
        QCOMPARE(view.selectedEvidence().value("from_us").toInteger(), qint64(1791072000123000));
        history.open(path);
        QTRY_COMPARE(indexed.count(), 2);
        qInfo("Saved history %lld bytes: index=%lld ms read=%lld ms render=%lld ms refresh=%lld ms",
              qint64(QFileInfo(path).size()), indexingMs, readingMs, renderingMs, timer.elapsed());
    }
    void sessionHistoryReadsLegacyJsonWithoutExposingJsonAndHandlesCancellation() {
        QTemporaryDir directory;
        const auto path = directory.filePath("legacy.jsonl");
        QFile file(path);
        QVERIFY(file.open(QIODevice::WriteOnly));
        for (int i = 0; i < 4000; ++i) {
            const auto bytes = QString("legacy %1\n").arg(i).toUtf8();
            file.write(QJsonDocument(QJsonObject{{"seq", i},
                                                 {"dir", "rx"},
                                                 {"t_us", 1000 + i},
                                                 {"data", QString::fromLatin1(bytes.toHex())}})
                           .toJson(QJsonDocument::Compact) +
                       '\n');
        }
        file.close();
        SessionHistory history;
        QSignalSpy indexed(&history, &SessionHistory::indexed);
        QSignalSpy windows(&history, &SessionHistory::windowReady);
        history.open(path);
        QTRY_COMPARE(indexed.count(), 1);
        history.locate(5, 1);
        QTRY_COMPARE(windows.count(), 1);
        TextStreamView view;
        view.showHistory(windows.last()[2].toJsonArray());
        QVERIFY(view.toPlainText().startsWith("legacy 0\n"));
        QVERIFY(!view.toPlainText().contains("t_us"));
        QSignalSpy matches(&history, &SessionHistory::matchesReady);
        history.search(QRegularExpression("legacy"));
        history.cancelSearch();
        history.search(QRegularExpression("^legacy 3999$"));
        QTRY_COMPARE(matches.count(), 1);
        QCOMPARE(matches.last()[0].toJsonArray().size(), 1);
    }
    void historySearchPreservesLongLineMatchesAcrossChunkings_data() {
        QTest::addColumn<int>("chunkSize");
        QTest::newRow("single-record") << 100000;
        QTest::newRow("normal-serial-chunks") << 4096;
        QTest::newRow("tiny-chunks") << 17;
    }
    void historySearchPreservesLongLineMatchesAcrossChunkings() {
        QFETCH(int, chunkSize);
        QTemporaryDir directory;
        QFile file(directory.filePath("long.jsonl"));
        QVERIFY(file.open(QIODevice::WriteOnly));
        QByteArray input("TARGET");
        input += QByteArray(6 * 4096, 'x');
        input += "\n";
        input += QByteArray(16380, 'y') + "BOUNDARY" + QByteArray(12000, 'z') + "\n";
        for (int offset = 0, seq = 0; offset < input.size(); offset += chunkSize, ++seq)
            file.write(QJsonDocument(
                           QJsonObject{{"seq", seq},
                                       {"dir", "rx"},
                                       {"data", QString::fromLatin1(input.mid(offset, chunkSize).toHex())}})
                           .toJson(QJsonDocument::Compact) +
                       '\n');
        file.close();
        SessionHistory history;
        QSignalSpy indexed(&history, &SessionHistory::indexed);
        QSignalSpy matches(&history, &SessionHistory::matchesReady);
        history.open(file.fileName());
        QTRY_COMPARE(indexed.count(), 1);
        history.search(QRegularExpression("TARGET|BOUNDARY"));
        QTRY_COMPARE(matches.count(), 1);
        const auto results = matches.last()[0].toJsonArray();
        QCOMPARE(results.size(), 2);
        QVERIFY(results[0].toObject().value("text").toString().contains("TARGET"));
        QVERIFY(results[1].toObject().value("text").toString().contains("BOUNDARY"));
        QVERIFY(!matches.last()[1].toBool());
        // A spanning regex cannot be guaranteed by bounded windows. Even with
        // no results the completion flag must not claim exhaustive coverage.
        history.search(QRegularExpression("TARGETx{24576}"));
        QTRY_COMPARE(matches.count(), 2);
        QVERIFY(matches.last()[0].toJsonArray().isEmpty());
        QVERIFY(!matches.last()[1].toBool());
    }
    void historySearchAnchorsIgnoreArtificialWindowEdges_data() {
        QTest::addColumn<QByteArray>("line");
        QTest::addColumn<QString>("pattern");
        QTest::addColumn<bool>("expected");
        const QByteArray longLine(20000, 'x');
        QTest::newRow("false-complete-first-window") << longLine << "^x{16384}$" << false;
        QTest::newRow("false-subject-first-window") << longLine << "\\Ax{16384}\\z" << false;
        QTest::newRow("false-end-first-window") << QByteArray(16384, 'x') + "Y" << "x{16384}$" << false;
        QTest::newRow("false-start-final-window") << "Y" + QByteArray(20000, 'x') << "^x+$" << false;
        QTest::newRow("false-middle-window") << "Y" + QByteArray(50000, 'x') + "Z" << "(?m)^x+$" << false;
        QTest::newRow("real-start") << "START" + longLine << "\\ASTART" << true;
        QTest::newRow("real-end") << longLine + "END" << "END\\z" << true;
        QTest::newRow("short-normal-line") << QByteArray("TARGET") << "^TARGET$" << true;
        QTest::newRow("short-subject-line") << QByteArray("TARGET") << "\\ATARGET\\z" << true;
    }
    void historySearchAnchorsIgnoreArtificialWindowEdges() {
        QFETCH(QByteArray, line);
        QFETCH(QString, pattern);
        QFETCH(bool, expected);
        QTemporaryDir directory;
        QFile file(directory.filePath("anchors.jsonl"));
        QVERIFY(file.open(QIODevice::WriteOnly));
        line += '\n';
        for (int offset = 0, seq = 0; offset < line.size(); offset += 4096, ++seq)
            file.write(
                QJsonDocument(QJsonObject{{"seq", seq},
                                          {"dir", "rx"},
                                          {"data", QString::fromLatin1(line.mid(offset, 4096).toHex())}})
                    .toJson(QJsonDocument::Compact) +
                '\n');
        file.close();
        SessionHistory history;
        QSignalSpy indexed(&history, &SessionHistory::indexed);
        QSignalSpy matches(&history, &SessionHistory::matchesReady);
        history.open(file.fileName());
        QTRY_COMPARE(indexed.count(), 1);
        history.search(QRegularExpression(pattern));
        QTRY_COMPARE(matches.count(), 1);
        QCOMPARE(matches.last()[0].toJsonArray().size(), expected ? 1 : 0);
        QCOMPARE(matches.last()[1].toBool(), line.size() <= 16385);
    }
    void historySearchNavigatesWithinMultilineAndSplitRecords() {
        QTemporaryDir directory;
        QFile file(directory.filePath("multiline.jsonl"));
        QVERIFY(file.open(QIODevice::WriteOnly));
        const QList<QByteArray> chunks{
            "first line\n", "prefix ",
            QString::fromUtf8("设备 TARGET second line\nthird TARGET line\n").toUtf8()};
        for (int i = 0; i < chunks.size(); ++i)
            file.write(
                QJsonDocument(
                    QJsonObject{{"seq", i}, {"dir", "rx"}, {"data", QString::fromLatin1(chunks[i].toHex())}})
                    .toJson(QJsonDocument::Compact) +
                '\n');
        file.close();
        RpcClient rpc;
        SessionPane pane(
            &rpc, {{"session_id", "search-offset"}, {"config", QJsonObject{{"record_to", file.fileName()}}}});
        pane.resize(1000, 700);
        pane.show();
        auto *search = pane.findChild<QLineEdit *>("receiveSearch");
        auto *results = pane.findChild<QListWidget *>();
        auto *stream = pane.findChild<TextStreamView *>();
        search->setText("TARGET");
        QTRY_COMPARE(results->count(), 2);
        for (int i = 0; i < results->count(); ++i) {
            const auto result = results->item(i)->data(Qt::UserRole).toJsonObject();
            QVERIFY(result.contains("history_offset"));
            QVERIFY(QMetaObject::invokeMethod(results, "itemClicked", Qt::DirectConnection,
                                              Q_ARG(QListWidgetItem *, results->item(i))));
            const auto expected =
                i == 0 ? QString::fromUtf8("prefix 设备 TARGET second line") : QString("third TARGET line");
            QTRY_COMPARE(stream->textCursor().block().text(), expected);
            auto cursor = stream->textCursor();
            cursor.movePosition(QTextCursor::NextCharacter, QTextCursor::KeepAnchor, 6);
            QCOMPARE(cursor.selectedText(), QString("TARGET"));
        }
    }
    void historyMetadataSurvivesPageBoundariesAndPartialAppend() {
        QTemporaryDir directory;
        const auto path = directory.filePath("boundary.txt");
        QFile file(path);
        QVERIFY(file.open(QIODevice::WriteOnly));
        file.write("[2026-10-04T00:00:00.123Z RX #12] first\n");
        for (int i = 0; i < 999; ++i)
            file.write("continuation\n");
        file.write("boundary continuation\n[2026-10-04T00:00:01.456Z TX #13 source=cli bytes=2]\n"
                   "    AT\n");
        for (int i = 0; i < 2995; ++i)
            file.write("tx continuation\n");
        file.write("[2026-10-04T00:00:02.000Z MARK anchor=13] annotation\n"
                   "[2026-10-04T00:00:03.789Z RX #14] partial");
        file.close();
        SessionHistory history;
        QSignalSpy indexed(&history, &SessionHistory::indexed);
        QSignalSpy windows(&history, &SessionHistory::windowReady);
        history.open(path);
        QTRY_COMPARE(indexed.count(), 1);
        history.window(1, 1);
        QTRY_COMPARE(windows.count(), 1);
        const auto rows = windows.last()[2].toJsonArray();
        QCOMPARE(rows[0].toObject().value("text").toString(), QString("boundary continuation\n"));
        QCOMPARE(rows[0].toObject().value("t_us").toInteger(), qint64(1791072000123000));
        QCOMPARE(rows[2].toObject().value("seq").toInteger(), qint64(13));
        QCOMPARE(rows[2].toObject().value("t_us").toInteger(), qint64(1791072001456000));
        QCOMPARE(rows.last().toObject().value("text").toString(), QString("partial"));
        QVERIFY(file.open(QIODevice::Append));
        file.write(" completed\n");
        file.close();
        history.open(path);
        QTRY_COMPARE(indexed.count(), 2);
        history.locate(14, 2);
        QTRY_COMPARE(windows.count(), 2);
        const auto last = windows.last()[2].toJsonArray().last().toObject();
        QCOMPARE(last.value("text").toString(), QString("partial completed\n"));
        QCOMPARE(last.value("t_us").toInteger(), qint64(1791072003789000));
    }
    void historyWindowKeepsOverlappingAnchorAndSelectionEvidence() {
        TextStreamView view;
        view.resize(600, 220);
        view.show();
        QJsonArray rows;
        for (int i = 0; i < 300; ++i)
            rows.append(QJsonObject{{"seq", i},
                                    {"text", QString("line %1\n").arg(i)},
                                    {"history_key", QString("file:%1").arg(i)},
                                    {"t_us", i * 1000}});
        view.showHistory(rows);
        QVERIFY(view.jumpToHistoryKey("file:100", true));
        QCoreApplication::processEvents();
        const auto key = view.topHistoryKey();
        QJsonArray older;
        for (int i = 0; i < 200; ++i)
            older.append(rows[i]);
        view.showHistory(older);
        QVERIFY(view.jumpToHistoryKey(key, true));
        QCOMPARE(view.topHistoryKey(), key);
        QVERIFY(view.jumpToSequence(100));
        auto cursor = view.textCursor();
        cursor.select(QTextCursor::LineUnderCursor);
        view.setTextCursor(cursor);
        QCOMPARE(view.selectedEvidence().value("from_seq").toInt(), 100);
    }
    void scrollingLiveViewLoadsDiskInPlaceAndSearchLocatesOldLog() {
        QTemporaryDir directory;
        const auto path = directory.filePath("session.txt");
        QFile file(path);
        QVERIFY(file.open(QIODevice::WriteOnly));
        for (int i = 0; i < 6000; ++i)
            file.write(QString("[2026-10-04T00:00:00.000Z RX #%1] saved line %1\n").arg(i).toUtf8());
        file.close();
        RpcClient rpc;
        SessionPane pane(&rpc, {{"session_id", "disk-test"}, {"config", QJsonObject{{"record_to", path}}}});
        pane.resize(1400, 800);
        pane.show();
        auto *stream = pane.findChild<TextStreamView *>();
        auto *mode = pane.findChild<QComboBox *>("receiveViewMode");
        mode->setCurrentIndex(0);
        QJsonArray live;
        for (int i = 5900; i < 6000; ++i) {
            auto r = receiveChunk(QString("live line %1\n").arg(i).toUtf8());
            r["seq"] = i;
            live.append(r);
        }
        stream->appendFrames(live);
        pane.setPaused(true);
        stream->verticalScrollBar()->setValue(0);
        const QPointF point(20, 20);
        QWheelEvent wheel(point, stream->viewport()->mapToGlobal(point.toPoint()), {}, QPoint(0, 120),
                          Qt::NoButton, Qt::NoModifier, Qt::NoScrollPhase, false);
        QCoreApplication::sendEvent(stream->viewport(), &wheel);
        QTRY_VERIFY(stream->toPlainText().contains("saved line 5900"));
        QVERIFY(stream->toPlainText().size() < 512 * 1024);
        auto *search = pane.findChild<QLineEdit *>("receiveSearch");
        search->setText("saved line 10");
        auto *results = pane.findChild<QListWidget *>();
        QTRY_VERIFY(results->count() > 0);
        auto *item = results->item(0);
        QVERIFY(item->data(Qt::UserRole).toJsonObject().contains("history_key"));
        QVERIFY(QMetaObject::invokeMethod(results, "itemClicked", Qt::DirectConnection,
                                          Q_ARG(QListWidgetItem *, item)));
        QTRY_VERIFY(stream->toPlainText().contains("saved line 10\n"));
        QCOMPARE(mode->currentIndex(), 0);
        // Returning live invalidates every queued disk response.
        stream->verticalScrollBar()->setValue(0);
        QCoreApplication::sendEvent(stream->viewport(), &wheel);
        pane.setPaused(false);
        stream->clearStream();
        stream->appendFrames({receiveChunk("latest only\n")});
        QTest::qWait(100);
        QCOMPARE(stream->toPlainText(), QString("latest only\n"));
    }
    void captureSearchCrossesPageBoundaryAndRefreshesGrowth() {
        QTemporaryDir directory;
        QFile file(directory.filePath("capture.txt"));
        QVERIFY(file.open(QIODevice::WriteOnly));
        QByteArray data(CaptureViewer::PageBytes - 2, 'a');
        data += QString::fromUtf8("中文目标\r\n").toUtf8();
        data += QByteArray(500000, 'b');
        QCOMPARE(file.write(data), data.size());
        file.close();
        CaptureViewer viewer;
        QVERIFY(viewer.openFile(file.fileName()));
        auto *page = viewer.findChild<QPlainTextEdit *>("capturePage");
        QVERIFY(!page->toPlainText().contains(QChar::ReplacementCharacter));
        auto *search = viewer.findChild<QLineEdit *>("captureSearch");
        search->setText(QString::fromUtf8("中文目标"));
        QSignalSpy matches(&viewer, &CaptureViewer::searchFinished);
        viewer.findNext();
        QTRY_COMPARE(matches.count(), 1);
        QVERIFY(matches.first().first().toBool());
        QCOMPARE(page->textCursor().selectedText(), QString::fromUtf8("中文目标"));
        viewer.lastPage();
        const auto size = viewer.snapshotSize();
        QVERIFY(file.open(QIODevice::Append));
        file.write("NEW TAIL\n");
        file.close();
        QCOMPARE(viewer.snapshotSize(), size);
        viewer.refreshFile();
        QVERIFY(page->toPlainText().endsWith("NEW TAIL\n"));
        QVERIFY(file.open(QIODevice::WriteOnly | QIODevice::Truncate));
        file.write("REPLACED\n");
        file.close();
        viewer.refreshFile();
        QCOMPARE(page->toPlainText(), QString("REPLACED\n"));
        search->setText("missing");
        viewer.findNext();
        viewer.cancelSearch();
        QCOMPARE(matches.count(), 1);
    }
    void issueReportIsReadableAndPreservesSelectedLines() {
        const QJsonObject bundle{
            {"created_at", "2026-10-04"},
            {"session", QJsonObject{{"session_id", "test"},
                                    {"config", QJsonObject{{"path", "COM3"}, {"baud", 115200}}}}},
            {"selected_evidence",
             QJsonObject{{"text", "first line\n中文日志\n"}, {"from_seq", 3}, {"to_seq", 5}}},
            {"preceding_send",
             QJsonObject{{"seq", 2}, {"t_us", 1000}, {"text", "reboot\r\n"}, {"source", "GUI#1"}}},
            {"markers", QJsonArray{QJsonObject{{"kind", "manual"}, {"label", "pressed reset"}}}}};
        const auto report = fc::issueReport(bundle);
        QVERIFY(report.contains("first line\n中文日志\n"));
        QVERIFY(report.contains("reboot\\r\\n"));
        QVERIFY(report.contains("COM3"));
        QVERIFY(report.contains("pressed reset"));
        QVERIFY(!report.contains("selected_evidence"));
    }
    void deviceProfilesFollowSerialNotPortNumber() {
        QJsonObject a{{"path", "/dev/ttyUSB0"}, {"vid", 4292}, {"pid", 60000}, {"serial", "board-A"}};
        auto b = a;
        b["path"] = "/dev/ttyUSB3";
        QCOMPARE(fc::profileKey(a), fc::profileKey(b));
        b["serial"] = "board-B";
        QVERIFY(fc::profileKey(a) != fc::profileKey(b));
        a.remove("serial");
        b.remove("serial");
        QVERIFY(fc::profileKey(a) != fc::profileKey(b));
    }
    void sentLogKeepsExactBytesAndDoesNotDuplicatePages() {
        SentLog log;
        auto a = receiveChunk("ls\r\n", "tx");
        a["seq"] = 4;
        a["t_us"] = 1000000;
        a["source"] = "GUI#1";
        a["len"] = 4;
        auto b = receiveChunk(QByteArray::fromHex("00ff"), "tx");
        b["seq"] = 9;
        b["source"] = "MCP#2";
        b["len"] = 2;
        log.append({a, receiveChunk("rx data"), b});
        log.append({a, b});
        QCOMPARE(log.frames().size(), 2);
        QCOMPARE(log.frames().first().toObject().value("text").toString(), QString("ls\r\n"));
        auto *tree = log.findChild<QTreeWidget *>("sentLogEntries");
        QCOMPARE(tree->topLevelItem(0)->text(1), QString("ls\\r\\n"));
        QCOMPARE(tree->topLevelItem(1)->text(3), QString("MCP#2"));
        log.findChild<QLineEdit *>()->setText("MCP");
        QVERIFY(tree->topLevelItem(0)->isHidden());
        QVERIFY(!tree->topLevelItem(1)->isHidden());
    }
    void receiveActionsAndMacrosPreserveSelectionAndInput() {
        const auto old = QSettings().value("macros");
        const auto restore = qScopeGuard([old] { QSettings().setValue("macros", old); });
        QSettings().setValue("macros", QStringList{});
        RpcClient rpc;
        SessionPane pane(&rpc, {{"session_id", "actions"}});
        QVERIFY(!pane.findChild<QPushButton *>("clearReceiveView"));
        auto *clear = pane.findChild<QAction *>("clearReceiveView");
        auto *mark = pane.findChild<QAction *>("markReceiveSelection");
        auto *measure = pane.findChild<QAction *>("measureReceiveSelection");
        QVERIFY(clear && mark && measure);
        QCOMPARE(clear->shortcut(), QKeySequence("Ctrl+L"));
        auto *stream = pane.findChild<TextStreamView *>();
        QCOMPARE(stream->contextMenuPolicy(), Qt::CustomContextMenu);
        QCOMPARE(pane.findChild<QTableView *>()->contextMenuPolicy(), Qt::CustomContextMenu);
        pane.insertMacro("AT+TEST\r\n");
        QSignalSpy changed(&pane, &SessionPane::macrosChanged);
        pane.saveCurrentMacro();
        QCOMPARE(changed.count(), 1);
        QCOMPARE(QSettings().value("macros").toStringList(), QStringList{"AT+TEST\r\n"});
        pane.framesModel()->append({receiveChunk("keep\n")});
        stream->appendFrames({receiveChunk("keep\n")});
        clear->trigger();
        QVERIFY(stream->toPlainText().isEmpty());
        QCOMPARE(pane.framesModel()->rowCount(), 0);
    }
    void singleDisplayButtonKeepsPauseWhileScrolling() {
        RpcClient rpc;
        SessionPane pane(&rpc, {{"session_id", "overlay"}});
        pane.resize(1300, 800);
        pane.show();
        auto *stream = pane.findChild<TextStreamView *>();
        auto *mode = pane.findChild<QComboBox *>("receiveViewMode");
        auto *button = pane.findChild<QPushButton *>("pauseReceiveView");
        QVERIFY(button);
        QVERIFY(!pane.findChild<QWidget *>("receiveViewNotice"));
        QVERIFY(!pane.findChild<QWidget *>("followReceiveTail"));
        QCOMPARE(button->text(), fc::text("Pause display"));
        mode->setCurrentIndex(0);
        stream->appendFrames({receiveChunk(QByteArray("device output\n").repeated(100))});
        QCoreApplication::processEvents();
        const auto area = stream->viewport()->size();
        button->click();
        QCoreApplication::processEvents();
        QCOMPARE(stream->viewport()->size(), area);
        QVERIFY(button->isChecked());
        QCOMPARE(button->text(), fc::text("Resume live"));
        stream->verticalScrollBar()->setValue(stream->verticalScrollBar()->maximum() / 2);
        QCoreApplication::processEvents();
        QVERIFY(button->isChecked());
        stream->verticalScrollBar()->setValue(stream->verticalScrollBar()->maximum());
        QCoreApplication::processEvents();
        QVERIFY(button->isChecked());
        QTest::mouseClick(stream->viewport(), Qt::LeftButton, Qt::NoModifier, QPoint(20, 20));
        stream->selectAll();
        QVERIFY(stream->textCursor().selectedText().contains("device output"));
        mode->setCurrentIndex(1);
        QCoreApplication::processEvents();
        auto *table = pane.findChild<QTableView *>();
        QJsonArray rows;
        for (int i = 0; i < 200; ++i)
            rows.append(receiveChunk(QByteArray(500, 'x')));
        pane.framesModel()->append(rows);
        QCoreApplication::processEvents();
        table->verticalScrollBar()->setValue(table->verticalScrollBar()->maximum() / 2);
        table->horizontalScrollBar()->setValue(table->horizontalScrollBar()->maximum());
        QCoreApplication::processEvents();
        QVERIFY(button->isChecked());
        pane.resize(1100, 650);
        QCoreApplication::processEvents();
        button->click();
        QVERIFY(!button->isChecked());
        QCOMPARE(button->text(), fc::text("Pause display"));
    }
    void sentDrawerPreservesReceiveSpaceAndCollectsWhileCollapsed() {
        QSettings().setValue("sentDrawerHeight", 120);
        RpcClient rpc;
        SessionPane pane(&rpc, {{"session_id", "drawer-test"}});
        pane.resize(1300, 850);
        pane.show();
        auto *log = pane.findChild<SentLog *>();
        auto *split = pane.findChild<QSplitter *>("receiveLogSplit");
        auto *body = pane.findChild<QWidget *>("sentLogBody");
        auto *toggle = pane.findChild<QToolButton *>("toggleSentLog");
        QVERIFY(log && split && body && toggle);
        QTRY_VERIFY(log->height() <= log->collapsedHeight());
        QVERIFY(!log->isExpanded());
        QVERIFY(body->isHidden());
        const int receiveHeight = split->sizes().first();
        auto frame = receiveChunk("status\r\n", "tx");
        frame["seq"] = 7;
        frame["source"] = "MCP#2";
        log->append({frame});
        QCOMPARE(log->frames().size(), 1);
        QVERIFY(!log->isExpanded());
        QCOMPARE(split->sizes().first(), receiveHeight);
        QVERIFY(pane.findChild<QLabel *>("lastSentCommand")->text().contains("status"));
        toggle->click();
        QTRY_VERIFY(body->isVisible());
        QVERIFY(split->sizes().first() >= 2 * split->sizes().last());
        QCOMPARE(log->frames().size(), 1);
        pane.resize(1300, 650);
        QTRY_VERIFY(split->sizes().first() >= 2 * split->sizes().last());
        toggle->click();
        QTRY_VERIFY(log->height() <= log->collapsedHeight());
        QCOMPARE(log->frames().size(), 1);
        QVERIFY(body->isHidden());
        QSettings().remove("sentDrawerHeight");
    }
    void highlightEditorUsesLiteralKeywordsAndPreservesOldRules() {
        TextStreamView stream;
        const auto initial = LogRules::defaults();
        HighlightRulesDialog dialog(initial);
        QCOMPARE(dialog.rules().size(), initial.size());
        QCOMPARE(dialog.rules().first().toObject().value("pattern"),
                 initial.first().toObject().value("pattern"));
        auto *add = dialog.findChild<QPushButton *>("addHighlightRule");
        QVERIFY(add);
        add->click();
        auto *pattern = dialog.findChild<QLineEdit *>("highlightPattern");
        pattern->setText("error [7].");
        const auto rule = dialog.rules().last().toObject();
        QCOMPARE(rule.value("match").toString(), QString("literal"));
        const auto regex = LogRules::expression(rule);
        QVERIFY(regex.match("ERROR [7].").hasMatch());
        QVERIFY(!regex.match("error 7x").hasMatch());
        QVERIFY(LogRules::validate(dialog.rules()).isEmpty());
        dialog.reject();
        QCOMPARE(LogRules::configured(), initial);
    }
    void resetEditorPreservesBytesAndBuildsStepsWithoutJson() {
        const QJsonArray initial{
            QJsonObject{{"dtr", false}, {"rts", true}, {"data", "one\r\ntwo\r\n"}, {"delay_ms", 25}}};
        ResetSequenceDialog dialog(initial);
        QCOMPARE(dialog.steps(), initial);
        dialog.findChild<QSpinBox *>("resetDelay")->setValue(50);
        QCOMPARE(dialog.steps().first().toObject().value("data").toString(), QString("one\r\ntwo\r\n"));
        dialog.findChild<QPushButton *>("addResetStep")->click();
        dialog.findChild<QComboBox *>("resetEncoding")->setCurrentIndex(2);
        dialog.findChild<QPlainTextEdit *>("resetPayload")->setPlainText("0G");
        auto *buttons = dialog.findChild<QDialogButtonBox *>();
        buttons->button(QDialogButtonBox::Save)->click();
        QCOMPARE(dialog.result(), int(QDialog::Rejected));
        dialog.findChild<QPlainTextEdit *>("resetPayload")->setPlainText("00 ff\n0a");
        buttons->button(QDialogButtonBox::Save)->click();
        QCOMPARE(dialog.result(), int(QDialog::Accepted));
        QCOMPARE(dialog.steps().last().toObject().value("hex").toString(), QString("00ff0a"));
        const QJsonArray combined{
            QJsonObject{{"dtr", true}, {"data", "x\r\n"}, {"hex", "ff"}, {"delay_ms", 10}}};
        ResetSequenceDialog legacy(combined);
        QCOMPARE(legacy.steps().size(), 2);
        QCOMPARE(legacy.steps().first().toObject().value("data").toString(), QString("x\r\n"));
        QVERIFY(!legacy.steps().first().toObject().contains("delay_ms"));
        QCOMPARE(legacy.steps().last().toObject().value("delay_ms").toInt(), 10);
    }
    void triggerAndDecoderDialogsPreserveExistingConfiguration() {
        const QJsonArray actions{QJsonObject{{"action", "respond"}, {"data", "AT\r\n"}},
                                 QJsonObject{{"action", "execute"},
                                             {"program", "/test/program"},
                                             {"args", QJsonArray{"a b", "--flag"}}}};
        const QJsonArray rules{QJsonObject{{"id", "test"},
                                           {"direction", "rx"},
                                           {"regex", "READY"},
                                           {"min_interval_ms", 250},
                                           {"actions", actions}}};
        TriggersDialog triggers(rules);
        QTimer::singleShot(0, [] {
            auto *dialog = qobject_cast<QDialog *>(QApplication::activeModalWidget());
            QVERIFY(dialog);
            dialog->findChild<QLineEdit *>("triggerName")->setText("renamed");
            dialog->findChild<QDialogButtonBox *>()->button(QDialogButtonBox::Save)->click();
        });
        triggers.findChild<QPushButton *>("editTrigger")->click();
        QCOMPARE(triggers.triggers().first().toObject().value("actions").toArray(), actions);
        QCOMPARE(triggers.triggers().first().toObject().value("id").toString(), QString("renamed"));
        QJsonObject spec{{"name", "cmd"},
                         {"options", QJsonObject{{"program", "/test/program"},
                                                 {"args", QJsonArray{"a b", "--flag"}},
                                                 {"timeout_ms", 1500},
                                                 {"custom", true}}}};
        const auto original = spec;
        QTimer::singleShot(0, [] {
            auto *dialog = qobject_cast<QDialog *>(QApplication::activeModalWidget());
            QVERIFY(dialog);
            dialog->findChild<QDialogButtonBox *>()->button(QDialogButtonBox::Save)->click();
        });
        QVERIFY(fc::editDecoderOptions(spec, nullptr));
        QCOMPARE(spec, original);
    }
    void ruleLibraryDistinguishesFailuresFromCounters() {
        TextStreamView view;
        LogRules rules(view.document());
        QVERIFY(LogRules::defaults().size() >= 25);
        QCOMPARE(LogRules::validate(LogRules::defaults()), QString());
        QVERIFY(rules.classify("Kernel panic - not syncing: VFS").startsWith("fatal"));
        QVERIFY(rules.classify("mmc0: error -110 whilst initialising SD card").startsWith("error"));
        QVERIFY(rules.classify("EXT4-fs error (device mmcblk0p2)").startsWith("error"));
        QVERIFY(rules.classify("0 errors, error=0 timeout=0").isEmpty());
        QVERIFY(rules.classify("[ 0.0] Linux version 6.12").startsWith("info"));
    }
    void selectionRetainsExactTextAndChunkTimeBounds() {
        TextStreamView view;
        auto first = receiveChunk("prefix ERR");
        first["seq"] = 10;
        first["t_us"] = 100000;
        auto second = receiveChunk("OR suffix\nnext\n");
        second["seq"] = 11;
        second["t_us"] = 104500;
        view.appendFrames({first, second});
        auto cursor = view.textCursor();
        cursor.setPosition(7);
        cursor.setPosition(12, QTextCursor::KeepAnchor);
        view.setTextCursor(cursor);
        const auto evidence = view.selectedEvidence();
        QCOMPARE(evidence.value("text").toString(), QString("ERROR"));
        QCOMPARE(evidence.value("from_seq").toInteger(), qint64(10));
        QCOMPARE(evidence.value("to_seq").toInteger(), qint64(12));
        QCOMPARE(evidence.value("elapsed_us").toInteger(), qint64(4500));
        view.setSearch(QRegularExpression("ERROR"));
        QCOMPARE(view.searchResults().size(), 1);
        QVERIFY(view.jumpToSequence(10));
    }
    void hexModeTracksReceiveViewSelection() {
        RpcClient rpc;
        SessionPane pane(&rpc, {{"session_id", "test"}});
        auto *mode = pane.findChild<QComboBox *>("receiveViewMode");
        auto *hex = pane.findChild<QCheckBox *>("receiveHex");
        QVERIFY(mode && hex);
        mode->setCurrentIndex(0);
        pane.framesModel()->append({receiveChunk("AT\r\n")});
        hex->setChecked(true);
        QCOMPARE(mode->currentIndex(), 1);
        QCOMPARE(pane.framesModel()->data(pane.framesModel()->index(0, 3)).toString(),
                 QString("41 54 0d 0a"));
        mode->setCurrentIndex(0);
        QVERIFY(!hex->isChecked());
        mode->setCurrentIndex(1);
        QVERIFY(!hex->isChecked());
        QCOMPARE(pane.framesModel()->data(pane.framesModel()->index(0, 3)).toString(),
                 QString::fromUtf8("AT␍␊"));
    }
    void inspectionPausesAndFollowResumesBothViews() {
        RpcClient rpc;
        SessionPane pane(&rpc, {{"session_id", "test"}});
        pane.resize(1200, 600);
        pane.show();
        auto *mode = pane.findChild<QComboBox *>("receiveViewMode");
        auto *pause = pane.findChild<QPushButton *>("pauseReceiveView");
        auto *follow = pause;
        auto *stream = pane.findChild<TextStreamView *>("receiveTextStream");
        auto *table = pane.findChild<QTableView *>();
        QVERIFY(mode && pause && follow && stream && table);
        mode->setCurrentIndex(0);
        stream->appendFrames({receiveChunk(QByteArray("kernel output\n").repeated(200))});
        pane.framesModel()->append({receiveChunk("chunk\r\n")});
        QCoreApplication::processEvents();
        // Arrival, automatic scrolling, and changing view do not imply inspection.
        QVERIFY(!pause->isChecked());
        QTest::mouseClick(stream->viewport(), Qt::LeftButton, Qt::NoModifier, QPoint(30, 10));
        QVERIFY(pause->isChecked());
        QTest::mouseClick(follow, Qt::LeftButton);
        QVERIFY(!pause->isChecked());
        QCOMPARE(stream->verticalScrollBar()->value(), stream->verticalScrollBar()->maximum());
        // Wheel-up must pause before any pending response can render.
        const QPointF point(40, 40);
        QWheelEvent wheel(point, stream->viewport()->mapToGlobal(point.toPoint()), {}, QPoint(0, 120),
                          Qt::NoButton, Qt::NoModifier, Qt::NoScrollPhase, false);
        QCoreApplication::sendEvent(stream->viewport(), &wheel);
        QVERIFY(pause->isChecked());
        pause->setChecked(false);
        QCOMPARE(stream->verticalScrollBar()->value(), stream->verticalScrollBar()->maximum());
        stream->verticalScrollBar()->triggerAction(QAbstractSlider::SliderPageStepSub);
        QVERIFY(pause->isChecked());
        QTest::mouseClick(follow, Qt::LeftButton);
        QTest::keyClick(stream, Qt::Key_PageUp);
        QVERIFY(pause->isChecked());
        mode->setCurrentIndex(1);
        QTest::mouseClick(follow, Qt::LeftButton);
        QVERIFY(!pause->isChecked());
        QCoreApplication::processEvents();
        const auto index = table->model()->index(0, 3);
        QTest::mouseClick(table->viewport(), Qt::LeftButton, Qt::NoModifier,
                          table->visualRect(index).center());
        QVERIFY(pause->isChecked());
        // Programmatic calls (including smoke mode) keep the checkbox in sync too.
        pane.setPaused(false);
        QVERIFY(!pause->isChecked());
        pane.setPaused(true);
        QVERIFY(pause->isChecked());
    }
    void pauseDuringInFlightReadThenResumeRequestsBoundedTail() {
        QTemporaryDir directory;
        QVERIFY(directory.isValid());
        const auto oldSocket = qgetenv("FLATTENCOM_SOCKET");
        const auto oldState = qgetenv("FLATTENCOM_STATE_DIR");
        const auto restoreEnvironment = qScopeGuard([&] {
            if (oldSocket.isNull())
                qunsetenv("FLATTENCOM_SOCKET");
            else
                qputenv("FLATTENCOM_SOCKET", oldSocket);
            if (oldState.isNull())
                qunsetenv("FLATTENCOM_STATE_DIR");
            else
                qputenv("FLATTENCOM_STATE_DIR", oldState);
        });
#ifdef Q_OS_WIN
        const QString endpoint =
            "flattencom-view-test-" + QString::number(QCoreApplication::applicationPid());
#else
        const QString endpoint = directory.filePath("rpc.sock");
#endif
        qputenv("FLATTENCOM_SOCKET", endpoint.toUtf8());
        qputenv("FLATTENCOM_STATE_DIR", directory.path().toUtf8());
        QFile token(directory.filePath("daemon.token"));
        QVERIFY(token.open(QIODevice::WriteOnly));
        token.write("test-token");
        token.close();
        QLocalServer server;
        QVERIFY(server.listen(endpoint));
        QLocalSocket *peer = nullptr;
        QList<QJsonObject> requests;
        auto respond = [&](const QJsonObject &request, const QJsonObject &result) {
            peer->write(QJsonDocument(
                            QJsonObject{{"jsonrpc", "2.0"}, {"id", request.value("id")}, {"result", result}})
                            .toJson(QJsonDocument::Compact) +
                        '\n');
            peer->flush();
        };
        connect(&server, &QLocalServer::newConnection, &server, [&] {
            peer = server.nextPendingConnection();
            auto *connection = peer;
            connect(
                connection, &QLocalSocket::readyRead, &server,
                [&, connection, buffer = QByteArray{}]() mutable {
                    auto respond = [connection](const QJsonObject &request, const QJsonObject &result) {
                        connection->write(QJsonDocument(QJsonObject{{"jsonrpc", "2.0"},
                                                                    {"id", request.value("id")},
                                                                    {"result", result}})
                                              .toJson(QJsonDocument::Compact) +
                                          '\n');
                        connection->flush();
                    };
                    buffer += connection->readAll();
                    while (buffer.contains('\n')) {
                        const auto end = buffer.indexOf('\n');
                        const auto request = QJsonDocument::fromJson(buffer.left(end)).object();
                        buffer.remove(0, end + 1);
                        if (request.value("method") == "read_frames")
                            requests.append(request);
                        else if (request.value("method") == "read_sent") {
                            auto sent = receiveChunk("status\r\n", "tx");
                            sent["seq"] = 12;
                            sent["source"] = "MCP#2";
                            respond(request,
                                    {{"frames", QJsonArray{sent}}, {"next_seq", 13}, {"up_to_date", true}});
                        } else
                            respond(request, request.value("method") == "hello" ? QJsonObject{{"proto", 1}}
                                                                                : QJsonObject{});
                    }
                });
        });
        RpcClient rpc;
        rpc.start();
        QTRY_VERIFY(rpc.ready());
        SessionPane pane(&rpc, {{"session_id", "test"}, {"state", QJsonObject{{"state", "connected"}}}});
        QTRY_COMPARE(requests.size(), 1);
        auto *pause = pane.findChild<QPushButton *>("pauseReceiveView");
        auto *stream = pane.findChild<TextStreamView *>("receiveTextStream");
        QSignalSpy displayed(&pane, &SessionPane::receivedFrames);
        const QJsonObject page{
            {"frames", QJsonArray{receiveChunk("kernel line\r\n")}}, {"next_seq", 1}, {"first_seq", 0}};
        pause->setChecked(true);
        auto *notice = pane.findChild<QLabel *>("sessionConnectionState");
        auto *resume = pause;
        QVERIFY(notice && resume);
        QCOMPARE(notice->text(), fc::text("Connected"));
        QCOMPARE(notice->property("connectionLevel").toString(), QString("connected"));
        QCOMPARE(pause->text(), fc::text("Resume live"));
        QCOMPARE(pause->toolTip(),
                 fc::text("Display paused; serial capture continues. Click to jump to the latest output."));
        QVERIFY(resume->isEnabled());
        respond(requests.first(), page);
        // Let the already-dispatched response and several polling ticks complete.
        QTest::qWait(200);
        QCOMPARE(pane.framesModel()->rowCount(), 0);
        QVERIFY(stream->toPlainText().isEmpty());
        QCOMPARE(displayed.count(), 0);
        QCOMPARE(requests.size(), 1);
        auto *sentLog = pane.findChild<SentLog *>();
        QVERIFY(sentLog);
        QTRY_COMPARE(sentLog->frames().size(), 1);
        QCOMPARE(sentLog->frames().first().toObject().value("source").toString(), QString("MCP#2"));
        resume->click();
        QVERIFY(!pause->isChecked());
        QCOMPARE(pause->text(), fc::text("Pause display"));
        QTRY_COMPARE(requests.size(), 2);
        QVERIFY(requests.last().value("params").toObject().value("tail").toBool());
        QVERIFY(!requests.last().value("params").toObject().contains("since_seq"));
        respond(requests.last(), page);
        QTRY_COMPARE(displayed.count(), 1);
        QCOMPARE(stream->toPlainText(), QString("kernel line\n"));
        QCOMPARE(pane.framesModel()->rowCount(), 1);
        QVERIFY(!pause->isChecked());
        // A response from before pause→resume must not overwrite the newest window.
        QTRY_COMPARE(requests.size(), 3);
        pane.setPaused(true);
        pane.setPaused(false);
        respond(requests.last(), {{"frames", QJsonArray{receiveChunk("stale\n")}}, {"next_seq", 2}});
        QTRY_COMPARE(requests.size(), 4);
        QCOMPARE(stream->toPlainText(), QString("kernel line\n"));
        QVERIFY(requests.last().value("params").toObject().value("tail").toBool());
        respond(requests.last(), {{"frames", QJsonArray{receiveChunk("latest\n")}},
                                  {"next_seq", 10000},
                                  {"first_seq", 0},
                                  {"up_to_date", true}});
        QTRY_COMPARE(stream->toPlainText(), QString("latest\n"));
        pane.setPaused(true);
        // Connection notifications must supersede the healthy-paused message immediately.
        peer->write(
            QJsonDocument(QJsonObject{{"jsonrpc", "2.0"},
                                      {"method", "session_state"},
                                      {"params", QJsonObject{{"session_id", "test"},
                                                             {"state", QJsonObject{{"state", "reconnecting"},
                                                                                   {"attempt", 1}}}}}})
                .toJson(QJsonDocument::Compact) +
            '\n');
        peer->flush();
        QTRY_VERIFY(notice->toolTip().contains(fc::text("Serial port disconnected; reconnecting.")));
        QCOMPARE(notice->property("connectionLevel").toString(), QString("pending"));
        QCOMPARE(pause->text(), fc::text("Resume live"));
        pane.updateSession({{"session_id", "test"}, {"state", QJsonObject{{"state", "closed"}}}});
        QVERIFY(notice->toolTip().contains(fc::text("Serial session closed.")));
        QCOMPARE(notice->property("connectionLevel").toString(), QString("error"));
        pane.updateSession({{"session_id", "test"}, {"state", QJsonObject{{"state", "connected"}}}});
        QVERIFY(resume->isEnabled());
        peer->disconnectFromServer();
        QTRY_VERIFY(!rpc.ready());
        QTRY_VERIFY(notice->toolTip().contains(
            fc::text("Background service disconnected; capture status is unknown.")));
        QVERIFY(!pause->toolTip().contains(
            fc::text("Display paused; serial capture continues. Click to jump to the latest output.")));
        rpc.stop();
        // Callbacks can synchronously destroy their client. Neither the buffered
        // response loop nor the expiry loop may touch it afterward.
        for (bool timeout : {false, true}) {
            QPointer<RpcClient> transient = new RpcClient;
            transient->start();
            QTRY_VERIFY(transient->ready());
            int callbacks = 0;
            transient->call(
                timeout ? "read_frames" : "list_ports", {},
                [&](const QJsonObject &, const QString &error) {
                    QCOMPARE(error.isEmpty(), !timeout);
                    ++callbacks;
                    delete transient.data();
                },
                timeout ? 1 : 10000);
            transient->call(
                timeout ? "read_frames" : "list_sessions", {},
                [&](const QJsonObject &, const QString &) {
                    // Timeout iteration is unordered; whichever expires
                    // first owns teardown and the other is discarded.
                    ++callbacks;
                    delete transient.data();
                },
                timeout ? 1 : 10000);
            QTRY_VERIFY(transient.isNull());
            QCOMPARE(callbacks, 1);
        }
    }
    void kernelLogHasSameLinesAtEveryChunkBoundary() {
        const auto bytes = QString::fromUtf8("[ 1097.736722] audio_msg_recv_cache is empty!\r\n"
                                             "[ 1097.780346] [T4695C4] [Audio:SIPC] channel=0 cmd=9\n"
                                             "[ 1097.817436] 内核启动完成\r\nlogin: ")
                               .toUtf8();
        const auto expected = QString::fromUtf8(bytes).replace("\r\n", "\n");
        TextStreamView view;
        for (qsizetype split = 0; split <= bytes.size(); ++split) {
            view.clearStream();
            view.appendFrames({receiveChunk(bytes.left(split))});
            view.appendFrames({receiveChunk(bytes.mid(split))});
            QCOMPARE(view.toPlainText(), expected);
        }
        // Extreme fragmentation: UTF-8 characters and CRLF arrive one byte at a time.
        view.clearStream();
        for (char byte : bytes)
            view.appendFrames({receiveChunk(QByteArray(1, byte))});
        QCOMPARE(view.toPlainText(), expected);
    }
    void crLfAndCrLfPairsHaveExplicitLogSemantics() {
        TextStreamView view;
        view.appendFrames({receiveChunk("one\r")});
        QCOMPARE(view.toPlainText(), QString("one\n"));
        view.appendFrames({receiveChunk("\ntwo\n\nthree\rfour\r\n")});
        QCOMPARE(view.toPlainText(), QString("one\ntwo\n\nthree\nfour\n"));
        view.appendFrames({receiveChunk("prompt without newline")});
        QVERIFY(view.toPlainText().endsWith("prompt without newline"));
    }
    void trailingNewlineUsesCompactInsertionRow() {
        TextStreamView view;
        view.resize(600, 220);
        view.show();
        auto append = [&](const QString &text) {
            view.appendFrames(QJsonArray{QJsonObject{{"dir", "rx"}, {"seq", 1}, {"text", text}}});
            QCoreApplication::processEvents();
        };
        append("first\n");
        auto first = view.document()->firstBlock();
        auto last = view.document()->lastBlock();
        QVERIFY(last.layout()->boundingRect().height() < first.layout()->boundingRect().height() / 2);
        QCOMPARE(view.toPlainText(), QString("first\n"));
        append("\nsecond");
        QCOMPARE(view.toPlainText(), QString("first\n\nsecond"));
        first = view.document()->firstBlock();
        last = view.document()->lastBlock();
        QCOMPARE(last.layout()->boundingRect().height(), first.layout()->boundingRect().height());
        QCOMPARE(first.next().layout()->boundingRect().height(), first.layout()->boundingRect().height());
        append("\r");
        append("\nthird\n");
        QCOMPARE(view.toPlainText(), QString("first\n\nsecond\nthird\n"));
        view.selectAll();
        QCOMPARE(view.selectedEvidence().value("text").toString(), view.toPlainText());
    }
    void transmitDoesNotCorruptReceiveStream() {
        TextStreamView view;
        view.appendFrames({receiveChunk("ker"), receiveChunk("AT\r\n", "tx"), receiveChunk("nel\r")});
        view.appendFrames({receiveChunk("\n", "tx"), receiveChunk("\nnext")});
        QCOMPARE(view.toPlainText(), QString("kernel\nnext"));
    }
    void ansiSequencesSplitAcrossReadsStayOutOfLogs() {
        TextStreamView view;
        const QByteArray bytes("\x1b[31mERROR\x1b[0m\r\n\x1b]0;title\x07ready\x1b]0;title\x1b\\!\n");
        for (char byte : bytes)
            view.appendFrames({receiveChunk(QByteArray(1, byte))});
        QCOMPARE(view.toPlainText(), QString("ERROR\nready!\n"));
    }
    void clearAndGapResetPartialUtf8AndCrState() {
        TextStreamView view;
        view.appendFrames({receiveChunk(QByteArray::fromHex("e4b8"))});
        view.clearStream();
        view.appendFrames({receiveChunk("clean\r")});
        QCOMPARE(view.toPlainText(), QString("clean\n"));
        view.markGap();
        view.appendFrames({receiveChunk("\nnext")});
        QVERIFY(view.toPlainText().endsWith("]\n\nnext"));
        QVERIFY(!view.toPlainText().contains(QChar::ReplacementCharacter));
    }
    void boundedTextAndSearchDoNotChangeCapturedContent() {
        TextStreamView view;
        view.appendFrames({receiveChunk("error\r\nokay\r\nERROR\n")});
        view.setSearch(QRegularExpression("error", QRegularExpression::CaseInsensitiveOption));
        QCOMPARE(view.extraSelections().size(), 2);
        QCOMPARE(view.toPlainText(), QString("error\nokay\nERROR\n"));
        view.clearStream();
        view.appendFrames({receiveChunk(QByteArray(3 * 1024 * 1024, 'A'))});
        QVERIFY(view.document()->characterCount() <= 2 * 1024 * 1024);
        QVERIFY(view.document()->lastBlock().length() <= 16385);
        view.appendFrames({receiveChunk(QByteArray("line\n").repeated(25000))});
        QVERIFY(view.blockCount() <= 20000);
    }
    void scrollingBackDoesNotJumpToTailOnNewData() {
        TextStreamView view;
        view.resize(500, 200);
        view.show();
        view.appendFrames({receiveChunk(QByteArray("kernel log line\n").repeated(200))});
        QCoreApplication::processEvents();
        view.verticalScrollBar()->setValue(0);
        view.appendFrames({receiveChunk("next line\n")});
        QCOMPARE(view.verticalScrollBar()->value(), 0);
        view.followTail();
        QCOMPARE(view.verticalScrollBar()->value(), view.verticalScrollBar()->maximum());
    }
    void sharedWireFixture() {
        QFile file(FIXTURE_PATH);
        QVERIFY(file.open(QIODevice::ReadOnly));
        auto response = QJsonDocument::fromJson(file.readAll()).object();
        FramesModel model;
        model.append(response.value("result").toObject().value("frames").toArray());
        QCOMPARE(model.rowCount(), 2);
        QCOMPARE(model.frame(0).value("seq").toInteger(), qint64(0));
        QCOMPARE(model.frame(1).value("text").toString(), QString("OK\r\n"));
        QCOMPARE(model.frame(0).value("decoded_text").toString(), QString::fromUtf8("AT␍␊"));
    }
    void wireFramesRenderWithoutLosingSequence() {
        FramesModel model;
        model.append({QJsonObject{
            {"seq", 0}, {"dir", "rx"}, {"t_us", 0}, {"len", 4}, {"text", "AT\r\n"}, {"hex", "41 54 0D 0A"}}});
        QCOMPARE(model.rowCount(), 1);
        QCOMPARE(model.data(model.index(0, 0)).toLongLong(), 0);
        QCOMPARE(model.data(model.index(0, 3)).toString(), QString::fromUtf8("AT␍␊"));
        model.setHex(true);
        QCOMPARE(model.data(model.index(0, 3)).toString(), QString("41 54 0D 0A"));
        model.clear();
        QCOMPARE(model.rowCount(), 0);
    }
    void viewMemoryIsBounded() {
        FramesModel model;
        QJsonArray batch;
        for (int i = 0; i < 1001; ++i)
            batch.append(QJsonObject{{"seq", i}, {"len", 65536}, {"text", "sample"}});
        model.append(batch);
        QVERIFY(model.rowCount() <= 512);
        QVERIFY(model.discarded() > 0);
    }
    void paneDeletionDuringRpcContinuation_data() {
        QTest::addColumn<QString>("boundary");
        for (const auto &boundary : {"send", "gap", "recording", "sync-stats", "update-session", "boot"})
            QTest::newRow(boundary) << QString::fromLatin1(boundary);
    }
    void paneDeletionDuringRpcContinuation() {
        QFETCH(QString, boundary);
        QTemporaryDir directory;
        QVERIFY(directory.isValid());
        const auto oldSocket = qgetenv("FLATTENCOM_SOCKET");
        const auto oldState = qgetenv("FLATTENCOM_STATE_DIR");
        const auto restore = qScopeGuard([&] {
            if (oldSocket.isNull())
                qunsetenv("FLATTENCOM_SOCKET");
            else
                qputenv("FLATTENCOM_SOCKET", oldSocket);
            if (oldState.isNull())
                qunsetenv("FLATTENCOM_STATE_DIR");
            else
                qputenv("FLATTENCOM_STATE_DIR", oldState);
        });
#ifdef Q_OS_WIN
        const auto endpoint =
            "flattencom-lifetime-" + QString::number(QCoreApplication::applicationPid()) + boundary;
#else
        const auto endpoint = directory.filePath("lifetime.sock");
#endif
        qputenv("FLATTENCOM_SOCKET", endpoint.toUtf8());
        qputenv("FLATTENCOM_STATE_DIR", directory.path().toUtf8());
        QFile token(directory.filePath("daemon.token"));
        QVERIFY(token.open(QIODevice::WriteOnly));
        token.write("test-token");
        token.close();
        QLocalServer server;
        QVERIFY(server.listen(endpoint));
        QLocalSocket *peer = nullptr;
        QList<QJsonObject> requests;
        auto reply = [&](const QJsonObject &request, const QJsonObject &result) {
            peer->write(QJsonDocument(
                            QJsonObject{{"jsonrpc", "2.0"}, {"id", request.value("id")}, {"result", result}})
                            .toJson(QJsonDocument::Compact) +
                        '\n');
            peer->flush();
        };
        connect(&server, &QLocalServer::newConnection, &server, [&] {
            peer = server.nextPendingConnection();
            connect(peer, &QLocalSocket::readyRead, &server, [&, buffer = QByteArray{}]() mutable {
                buffer += peer->readAll();
                while (buffer.contains('\n')) {
                    const auto end = buffer.indexOf('\n');
                    const auto request = QJsonDocument::fromJson(buffer.left(end)).object();
                    buffer.remove(0, end + 1);
                    if (request.value("method") == "hello")
                        reply(request, {{"proto", 1}});
                    else
                        requests.append(request);
                }
            });
        });
        auto findRequest = [&](const QString &method) {
            for (qsizetype i = 0; i < requests.size(); ++i)
                if (requests[i].value("method") == method)
                    return requests[i];
            return QJsonObject{};
        };
        RpcClient rpc;
        rpc.start();
        QTRY_VERIFY(rpc.ready());
        QPointer<SessionPane> pane = new SessionPane(&rpc, {{"session_id", "lifetime"}});
        const auto cleanup = qScopeGuard([&] {
            delete pane.data();
            rpc.stop();
        });
        int messages = 0;
        int continuations = 0;
        connect(pane, &SessionPane::sent, this, [&] { ++continuations; });
        connect(pane, &SessionPane::statsChanged, this, [&](const QJsonObject &) { ++continuations; });
        connect(
            pane, &SessionPane::message, this,
            [&](const QString &) {
                ++messages;
                delete pane.data();
            },
            Qt::DirectConnection);
        QJsonObject request;
        const QJsonObject failedRecording{{"errors", QJsonArray{"disk failed"}}};
        const QJsonObject failedStats{{"recording", failedRecording}};
        if (boundary == "send") {
            pane->setPaused(true);
            pane->send("status");
            QTRY_VERIFY(!(request = findRequest("send")).isEmpty());
            reply(request, {{"bytes_sent", 6}});
        } else if (boundary == "gap") {
            QTRY_VERIFY(!(request = findRequest("read_frames")).isEmpty());
            requests.removeAll(request);
            reply(request,
                  {{"frames", QJsonArray{}}, {"first_seq", 0}, {"next_seq", 1}, {"up_to_date", true}});
            QTRY_VERIFY(!(request = findRequest("read_frames")).isEmpty());
            QVERIFY(!request.value("params").toObject().value("tail").toBool());
            reply(request,
                  {{"frames", QJsonArray{receiveChunk("tail\n")}}, {"first_seq", 10}, {"next_seq", 11}});
        } else if (boundary == "update-session") {
            pane->updateSession({{"session_id", "lifetime"}, {"stats", failedStats}});
        } else if (boundary == "boot") {
            pane->setPaused(true);
            QTRY_VERIFY(!(request = findRequest("list_markers")).isEmpty());
            // Deletion on the first boot must prevent both member access and
            // iteration over the second element of the destroyed boot array.
            reply(request,
                  {{"boots",
                    QJsonArray{QJsonObject{{"number", 1}, {"seq", 1}, {"t_us", 1}, {"interval_ms", 0}},
                               QJsonObject{{"number", 2}, {"seq", 2}, {"t_us", 2}, {"interval_ms", 1}}}}});
        } else {
            pane->setPaused(true);
            QTRY_VERIFY(!(request = findRequest("get_stats")).isEmpty());
            if (boundary == "recording") {
                reply(request, {{"stats", failedStats}});
            } else {
                // Keep boot/signals polls pending, release the first stats poll,
                // then fill the RPC queue so the next get_stats fails synchronously.
                reply(request, {{"stats", QJsonObject{}}});
                QTRY_COMPARE(continuations, 1);
                continuations = 0;
                for (int i = 0; i < 64; ++i)
                    rpc.call("held", {});
            }
        }
        QTRY_VERIFY(pane.isNull());
        QCOMPARE(messages, 1);
        QCOMPARE(continuations, 0);
        // Drain any queued signals after the callback destroyed the pane.
        QCoreApplication::processEvents();
        QVERIFY(rpc.ready());
    }
};
QTEST_MAIN(ModelsTest)
#include "models.moc"
