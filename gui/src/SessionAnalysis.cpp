/*
 * flattencom - Session Analysis Actions
 *
 * Implements evidence selection, markers, reset operations, timing and issue-report export.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

#include "DeviceProfiles.h"
#include "IssueReport.h"
#include "RuleEditors.h"
#include "SessionPane.h"
#include "Style.h"
#include <QApplication>
#include <QClipboard>
#include <QDateTime>
#include <QDialogButtonBox>
#include <QDir>
#include <QFileDialog>
#include <QFileInfo>
#include <QInputDialog>
#include <QJsonDocument>
#include <QMessageBox>
#include <QPointer>
#include <QProgressBar>
#include <QSaveFile>
#include <QSettings>
#include <QTextEdit>
#include <QVBoxLayout>

QJsonObject SessionPane::evidence() const {
    if (receiveViews_->currentWidget() == textStream_)
        return textStream_->selectedEvidence();
    auto rows = table_->selectionModel()->selectedRows();
    if (rows.isEmpty())
        return {};
    std::sort(rows.begin(), rows.end(),
              [](const QModelIndex &a, const QModelIndex &b) { return a.row() < b.row(); });
    QJsonArray frames;
    QString text;
    qint64 bytes = 0;
    for (const auto &row : rows) {
        auto frame = model_->frame(proxy_->mapToSource(row).row());
        frames.append(frame);
        text += frame.value("text").toString();
        bytes += frame.value("len").toInteger();
    }
    const auto first = frames.first().toObject(), last = frames.last().toObject();
    return {{"text", text},
            {"frames", frames},
            {"from_seq", first.value("seq")},
            {"to_seq", last.value("seq").toInteger() + 1},
            {"from_us", first.value("t_us")},
            {"to_us", last.value("t_us")},
            {"elapsed_us", last.value("mono_us").toInteger() - first.value("mono_us").toInteger()},
            {"bytes", bytes}};
}
void SessionPane::refreshSearchResults() {
    if (!capturePath().isEmpty() && !historySearch_.pattern().isEmpty())
        return;
    const auto results = textStream_->searchResults();
    searchResults_->clear();
    searchResults_->setVisible(!results.isEmpty());
    for (const auto &r : results) {
        const auto value = r.toObject();
        auto *item = new QListWidgetItem(
            QString("%1  %2").arg(QDateTime::fromMSecsSinceEpoch(value.value("t_us").toInteger() / 1000)
                                      .toString("HH:mm:ss.zzz"),
                                  value.value("text").toString()),
            searchResults_);
        item->setData(Qt::UserRole, value.value("block").toInt());
    }
}
void SessionPane::measureSelection() {
    const auto selection = evidence();
    if (selection.isEmpty()) {
        emit message(fc::text("Select a text range or multiple chunks first"));
        return;
    }
    if (!selection.contains("elapsed_us")) {
        emit message(fc::text("This saved range has no timing information."));
        return;
    }
    QMessageBox::information(this, fc::text("Time interval"),
                             QString("%1 ms\n%2 %3 - %4\n%5")
                                 .arg(selection.value("elapsed_us").toDouble() / 1000, 0, 'f', 3)
                                 .arg(fc::text("Sequence"))
                                 .arg(selection.value("from_seq").toInteger())
                                 .arg(selection.value("to_seq").toInteger() - 1)
                                 .arg(fc::text("Based on host receive timestamps.")));
}
void SessionPane::beginNavigation() {
    // A new local intent supersedes remote navigation even when already paused.
    ++viewGeneration_;
    historyLoading_ = false;
    historyAnchor_.clear();
    setPaused(true);
}
void SessionPane::locateSequence(qint64 seq) {
    beginNavigation();
    if (auto *mode = findChild<QComboBox *>("receiveViewMode"))
        mode->setCurrentIndex(0);
    if (textStream_->jumpToSequence(seq))
        return;
    const auto generation = ++viewGeneration_;
    call("read_frames", {{"since_seq", seq}, {"max_bytes", 65536}, {"format", "decoded"}},
         [this, seq, generation](const QJsonObject &r, const QString &e) {
             if (!e.isEmpty() || generation != viewGeneration_)
                 return;
             if (r.value("first_seq").toInteger() > seq) {
                 openHistory(seq);
                 return;
             }
             model_->clear();
             diskView_ = false;
             textStream_->clearStream();
             model_->append(r.value("frames").toArray());
             textStream_->appendFrames(r.value("frames").toArray());
             textStream_->jumpToSequence(seq);
             refreshSearchResults();
         });
}
void SessionPane::pollSent() {
    if (!rpc_->ready() || sentPending_ || sentUnsupported_)
        return;
    sentPending_ = true;
    // This cursor is independent of the receive view, its filters and its pause state.
    QPointer<SessionPane> self(this);
    rpc_->call(
        "read_sent",
        {{"session_id", sessionId()}, {"since_seq", sentCursor_}, {"max_bytes", 65536}, {"format", "text"}},
        [self](const QJsonObject &result, const QString &error) {
            if (!self)
                return;
            self->sentPending_ = false;
            if (!error.isEmpty()) {
                // Older daemons do not implement the dedicated TX history endpoint.
                self->sentUnsupported_ = error.contains("-32601") || error.contains("read_sent");
                emit self->message(fc::text("Cannot read sent history: ") + error);
                return;
            }
            self->sentLog_->append(result.value("frames").toArray());
            self->sentLog_->setEvicted(result.value("dropped_rx").toInteger());
            self->sentCursor_ = result.value("next_seq").toInteger(self->sentCursor_);
        });
}
void SessionPane::locateSent(qint64 seq) {
    if (!capturePath().isEmpty()) {
        openHistory(seq);
        return;
    }
    beginNavigation();
    const auto generation = ++viewGeneration_;
    call("read_frames", {{"since_seq", seq}, {"max_bytes", 65536}, {"format", "decoded"}},
         [this, seq, generation](const QJsonObject &r, const QString &e) {
             if (!e.isEmpty() || generation != viewGeneration_)
                 return;
             if (r.value("first_seq").toInteger() > seq) {
                 emit message(fc::text("Position evicted from receive history; open the JSONL capture."));
                 return;
             }
             if (auto *search = findChild<QLineEdit *>("receiveSearch"))
                 search->clear();
             if (auto *mode = findChild<QComboBox *>("receiveViewMode"))
                 mode->setCurrentIndex(1);
             model_->clear();
             textStream_->clearStream();
             const auto frames = r.value("frames").toArray();
             model_->append(frames);
             textStream_->appendFrames(frames);
             if (model_->rowCount()) {
                 table_->selectRow(0);
                 table_->scrollToTop();
             }
             refreshSearchResults();
         });
}
void SessionPane::pollBootEvents() {
    if (!rpc_->ready() || bootPollPending_)
        return;
    bootPollPending_ = true;
    call("list_markers", {}, [this](const QJsonObject &r, const QString &e) {
        bootPollPending_ = false;
        if (!e.isEmpty())
            return;
        QPointer<SessionPane> self(this);
        bootEvents_ = r.value("boots").toArray();
        for (const auto &v : bootEvents_) {
            const auto event = v.toObject();
            const auto number = event.value("number").toInteger();
            if (number <= lastBootNumber_)
                continue;
            lastBootNumber_ = number;
            emit message(QString(fc::text("Boot count: %1, interval: %2 ms"))
                             .arg(number)
                             .arg(event.value("interval_ms").toInteger()));
            if (!self)
                return;
            if (resetAfterUs_ && event.value("t_us").toInteger() >= resetAfterUs_) {
                resetAfterUs_ = 0;
                locateSequence(event.value("seq").toInteger());
                if (!self)
                    return;
            }
        }
        if (resetAfterUs_ && QDateTime::currentMSecsSinceEpoch() * 1000 - resetAfterUs_ > 120000000) {
            resetAfterUs_ = 0;
            emit message(
                fc::text("No boot message detected within two minutes; check the log at the reset marker."));
        }
    });
}
void SessionPane::addMarker() {
    QString label = fc::text("Marker ") + QDateTime::currentDateTime().toString("HH:mm:ss.zzz");
    if (QApplication::keyboardModifiers() & Qt::ShiftModifier) {
        bool ok = false;
        label = QInputDialog::getText(this, fc::text("Add marker"), fc::text("Action / observation"),
                                      QLineEdit::Normal, label, &ok);
        if (!ok || label.isEmpty())
            return;
    }
    QJsonObject params{{"label", label}};
    const auto selected = evidence();
    if (selected.contains("from_seq"))
        params["seq"] = selected.value("from_seq");
    call("add_marker", params, [this](const QJsonObject &r, const QString &e) {
        if (e.isEmpty())
            emit message(fc::text("Marked: ") + r.value("marker").toObject().value("label").toString());
    });
}
void SessionPane::showMarkers() {
    call("list_markers", {}, [this](const QJsonObject &r, const QString &e) {
        if (!e.isEmpty())
            return;
        auto *dialog = new QDialog(this);
        dialog->setAttribute(Qt::WA_DeleteOnClose);
        dialog->setWindowTitle(fc::text("Markers and boot events"));
        dialog->resize(720, 420);
        auto *layout = new QVBoxLayout(dialog);
        auto *list = new QListWidget;
        layout->addWidget(list);
        for (const auto &v : r.value("markers").toArray()) {
            const auto marker = v.toObject();
            auto *item = new QListWidgetItem(
                QString("%1  %2  %3")
                    .arg(QDateTime::fromMSecsSinceEpoch(marker.value("t_us").toInteger() / 1000)
                             .toString("HH:mm:ss.zzz"),
                         fc::valueLabel(marker.value("kind").toString()), marker.value("label").toString()),
                list);
            item->setData(Qt::UserRole, marker);
        }
        connect(list, &QListWidget::itemDoubleClicked, this, [this, dialog](QListWidgetItem *item) {
            const auto marker = item->data(Qt::UserRole).toJsonObject();
            if (!marker.value("seq").isNull())
                locateSequence(marker.value("seq").toInteger());
            dialog->close();
        });
        dialog->show();
    });
}
void SessionPane::editRules() {
    HighlightRulesDialog dialog(LogRules::configured(), this);
    if (dialog.exec() != QDialog::Accepted)
        return;
    QSettings().setValue("logRules", QJsonDocument(dialog.rules()).toJson(QJsonDocument::Compact));
    for (auto *window : QApplication::topLevelWidgets())
        for (auto *stream : window->findChildren<TextStreamView *>())
            stream->reloadRules();
}
void SessionPane::shareSelection() {
    auto selection = evidence();
    if (selection.isEmpty()) {
        emit message(fc::text("Select log text first."));
        return;
    }
    if (selection.value("text").toString().toUtf8().size() > 256 * 1024) {
        emit message(fc::text("Selection exceeds 256 KiB; choose a smaller range"));
        return;
    }
    selection.remove("frames");
    call("publish_selection", selection, [this](const QJsonObject &r, const QString &e) {
        if (!e.isEmpty())
            return;
        publishedSelection_ = r.value("selection").toObject().value("id").toString();
        const auto instruction =
            fc::text("Analyze only this explicit flattencom selection. Use analyze-selection prompt or call "
                     "read_selection with selection_id=%1. Do not read other session logs.\nRestricted MCP "
                     "process: flattencom-mcp --selection %1")
                .arg(publishedSelection_);
        QApplication::clipboard()->setText(instruction);
        auto *dialog = new QMessageBox(QMessageBox::Information, fc::text("Selection shared"),
                                       fc::text("Instructions copied. Paste them into your MCP client."),
                                       QMessageBox::Close, this);
        dialog->setDetailedText(instruction);
        dialog->setAttribute(Qt::WA_DeleteOnClose);
        auto *revoke = dialog->addButton(fc::text("Revoke"), QMessageBox::DestructiveRole);
        const auto id = publishedSelection_;
        connect(revoke, &QPushButton::clicked, this,
                [this, id] { call("revoke_selection", {{"selection_id", id}}); });
        dialog->show();
    });
}
void SessionPane::exportProblem() {
    const auto selection = evidence();
    if (selection.isEmpty()) {
        emit message(fc::text("Select the issue and its context before exporting"));
        return;
    }
    const auto path = QFileDialog::getSaveFileName(
        this, fc::text("Export issue bundle"), "flattencom-issue.txt", fc::text("Readable report (*.txt)"));
    if (path.isEmpty())
        return;
    call("list_markers", {}, [this, path, selection](const QJsonObject &r, const QString &e) {
        if (!e.isEmpty())
            return;
        QJsonArray commands;
        QJsonObject preceding;
        const auto from = selection.value("from_seq").toInteger(-1),
                   to = selection.value("to_seq").toInteger(-1);
        for (const auto &v : sentLog_->frames()) {
            const auto frame = v.toObject();
            const auto seq = frame.value("seq").toInteger();
            if (from >= 0 && seq < from)
                preceding = frame;
            else if (from >= 0 && seq < to)
                commands.append(frame);
        }
        QJsonObject bundle{{"schema", "flattencom.issue.v1"},
                           {"created_at", QDateTime::currentDateTimeUtc().toString(Qt::ISODateWithMs)},
                           {"session", session_},
                           {"selected_evidence", selection},
                           {"sent_in_range", commands},
                           {"preceding_send", preceding},
                           {"markers", r.value("markers")},
                           {"boot_events", r.value("boots")},
                           {"rules", LogRules::configured()}};
        call("save_report", {{"path", path}, {"text", fc::issueReport(bundle)}},
             [this, path](const QJsonObject &, const QString &error) {
                 if (error.isEmpty())
                     emit message(path);
             });
    });
}
void SessionPane::configureReset() {
    const auto key = session_.value("profile_key").toString(session_.value("path").toString());
    auto profile = fc::loadProfile(key);
    ResetSequenceDialog dialog(profile.value("reset_steps").toArray(), this);
    if (dialog.exec() != QDialog::Accepted)
        return;
    profile["reset_steps"] = dialog.steps();
    fc::saveProfile(key, profile);
    emit message(fc::text("Reset settings saved."));
}
void SessionPane::resetDevice() {
    const auto key = session_.value("profile_key").toString(session_.value("path").toString());
    const auto steps = fc::loadProfile(key).value("reset_steps").toArray();
    if (steps.isEmpty() || (QApplication::keyboardModifiers() & Qt::ShiftModifier)) {
        configureReset();
        return;
    }
    resetAfterUs_ = QDateTime::currentMSecsSinceEpoch() * 1000;
    setPaused(false);
    startOperation("reset_device", {{"steps", steps}, {"label", fc::text("Device reset requested")}});
}
void SessionPane::startOperation(const QString &method, QJsonObject params) {
    call(method, params, [this, method, params](const QJsonObject &r, const QString &e) {
        if (!e.isEmpty()) {
            if (method == "reset_device")
                resetAfterUs_ = 0;
            return;
        }
        if (method == "reset_device") {
            const auto key = session_.value("profile_key").toString();
            auto profile = fc::loadProfile(key);
            profile["reset_steps"] = params.value("steps");
            fc::saveProfile(key, profile);
        }
        const auto id = r.value("operation").toObject().value("id").toString();
        auto *dialog = new QDialog(this);
        dialog->setAttribute(Qt::WA_DeleteOnClose);
        dialog->setWindowTitle(fc::text("Device operation"));
        dialog->resize(480, 160);
        auto *layout = new QVBoxLayout(dialog);
        auto *label = new QLabel;
        label->setWordWrap(true);
        auto *progress = new QProgressBar;
        auto *cancel = new QPushButton(fc::text("Cancel operation"));
        layout->addWidget(label);
        layout->addWidget(progress);
        auto *hint = new QLabel(fc::text("The operation continues after this window is closed."));
        progress->setToolTip(fc::text("Progress counts bytes submitted to the driver."));
        hint->setWordWrap(true);
        layout->addWidget(hint);
        layout->addWidget(cancel);
        auto *timer = new QTimer(dialog);
        timer->setInterval(200);
        auto pending = std::make_shared<bool>(false);
        QPointer<QDialog> alive(dialog);
        connect(cancel, &QPushButton::clicked, this, [this, id, cancel] {
            cancel->setEnabled(false);
            call("cancel_operation", {{"operation_id", id}});
        });
        connect(timer, &QTimer::timeout, this, [this, id, alive, label, progress, cancel, timer, pending] {
            if (!alive || *pending)
                return;
            if (!rpc_->ready()) {
                label->setText(fc::text("Waiting for background service; operation status is unknown."));
                timer->setInterval(1000);
                return;
            }
            *pending = true;
            // Poll errors belong in this dialog, not in the pane's message/error
            // wrapper (which also stops periodic sends). Keep retrying transient
            // transport, timeout and queue errors with bounded polling frequency.
            rpc_->call(
                "operation_status", {{"operation_id", id}},
                [alive, label, progress, cancel, timer, pending](const QJsonObject &r, const QString &e) {
                    *pending = false;
                    if (!alive)
                        return;
                    if (!e.isEmpty()) {
                        // The current RPC callback exposes only localized text.
                        // Match the exact backend diagnostic in both wire languages.
                        if (e == QStringLiteral("operation not found") || e == QStringLiteral("操作不存在")) {
                            label->setText(
                                fc::text("Operation unavailable; the service may have restarted."));
                            timer->stop();
                            cancel->setEnabled(false);
                        } else {
                            label->setText(e);
                            timer->setInterval(1000);
                        }
                        return;
                    }
                    timer->setInterval(200);
                    const auto s = r.value("operation").toObject();
                    const auto done = s.value(s.value("kind") == "replay" ? "input_bytes" : "bytes_sent")
                                          .toDouble(),
                               total = s.value("total_bytes").toDouble();
                    progress->setRange(0, total ? 1000 : 0);
                    if (total)
                        progress->setValue(qRound(done / total * 1000));
                    const auto state = s.value("state").toString();
                    const QMap<QString, QString> states{{"running", fc::text("Running")},
                                                        {"completed", fc::text("Completed")},
                                                        {"cancelled", fc::text("Cancelled")},
                                                        {"failed", fc::text("Failed")}};
                    label->setText(QString("%1  %2 / %3 B\n%4")
                                       .arg(states.value(state, state))
                                       .arg(done, 0, 'f', 0)
                                       .arg(total, 0, 'f', 0)
                                       .arg(s.value("error").toString()));
                    if (state == "completed" || state == "cancelled" || state == "failed") {
                        timer->stop();
                        cancel->setEnabled(false);
                        if (!total) {
                            progress->setRange(0, 1);
                            progress->setValue(1);
                        }
                    }
                });
        });
        timer->start();
        dialog->show();
    });
}
