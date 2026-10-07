/*
 * flattencom - Serial Session Workspace
 *
 * Coordinates live and historical receive views, display pause, sending and session actions.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

#include "SessionPane.h"
#include "DeviceProfiles.h"
#include "RuleEditors.h"
#include "Style.h"
#include <QApplication>
#include <QClipboard>
#include <QCompleter>
#include <QDesktopServices>
#include <QDialog>
#include <QDialogButtonBox>
#include <QFileDialog>
#include <QFileInfo>
#include <QFormLayout>
#include <QHeaderView>
#include <QInputDialog>
#include <QJsonDocument>
#include <QKeyEvent>
#include <QLabel>
#include <QMenu>
#include <QMouseEvent>
#include <QPointer>
#include <QPushButton>
#include <QScrollBar>
#include <QSettings>
#include <QShortcut>
#include <QSignalBlocker>
#include <QSplitter>
#include <QTextEdit>
#include <QUrl>
#include <QVBoxLayout>
#include <QWheelEvent>

SessionPane::SessionPane(RpcClient *rpc, const QJsonObject &session, QWidget *parent)
    : QWidget(parent), rpc_(rpc), session_(session), model_(new FramesModel(this)),
      proxy_(new QSortFilterProxyModel(this)), table_(new QTableView(this)) {
    controller_ = new SessionController(rpc_, sessionId(), this);
    if (!session_.contains("profile_key")) {
        auto metadata = session_.value("config").toObject().value("device_identity").toObject();
        metadata["path"] = session_.value("path");
        session_["profile_key"] = fc::profileKey(metadata);
        if (!session_.contains("send_newline"))
            session_["send_newline"] =
                fc::loadProfile(session_.value("profile_key").toString()).value("newline");
    }
    auto *layout = new QVBoxLayout(this);
    layout->setContentsMargins(4, 4, 4, 4);
    layout->setSpacing(4);
    auto *recordingRow = new QHBoxLayout;
    recording_ = new QLabel;
    recording_->setObjectName("recordingStatus");
    recording_->setTextFormat(Qt::PlainText);
    recording_->setTextInteractionFlags(Qt::TextSelectableByMouse);
    recording_->setWordWrap(false);
    recording_->setSizePolicy(QSizePolicy::Ignored, QSizePolicy::Preferred);
    recordingFolder_ = new QPushButton(fc::text("Open log folder"));
    recordingRow->addWidget(recording_, 1);
    connectionState_ = new QLabel;
    connectionState_->setObjectName("sessionConnectionState");
    connectionState_->setTextFormat(Qt::PlainText);
    connectionState_->setAlignment(Qt::AlignCenter);
    connectionState_->setSizePolicy(QSizePolicy::Minimum, QSizePolicy::Fixed);
    recordingRow->addWidget(connectionState_);
    recordingRow->addWidget(recordingFolder_);
    layout->addLayout(recordingRow);
    connect(recordingFolder_, &QPushButton::clicked, this, [this] {
        const auto config = session_.value("config").toObject();
        auto path = config.value("record_rx_to").toString();
        if (path.isEmpty())
            path = config.value("record_to").toString();
        if (!path.isEmpty() &&
            !QDesktopServices::openUrl(QUrl::fromLocalFile(QFileInfo(path).absolutePath())))
            emit message(fc::text("Could not open log folder"));
    });
    updateRecording(session_.value("stats").toObject().value("recording").toObject());
    auto *viewTools = new QHBoxLayout;
    auto *search = new QLineEdit;
    search->setObjectName("receiveSearch");
    search->setPlaceholderText(fc::text("Search logs"));
    auto *viewMode = new QComboBox;
    viewMode->setObjectName("receiveViewMode");
    viewMode->addItem(fc::text("Text stream (RX)"), "text");
    viewMode->addItem(fc::text("Chunks (RX/TX)"), "chunks");
    viewMode->setToolTip(fc::text("Text view shows received logs; chunk view shows RX/TX records."));
    auto *regex = new QCheckBox(fc::text("Regex"));
    auto *hex = new QCheckBox("HEX");
    hex->setObjectName("receiveHex");
    auto *relative = new QCheckBox(fc::text("Relative"));
    pause_ = new QPushButton(fc::text("Pause display"));
    pause_->setObjectName("pauseReceiveView");
    pause_->setCheckable(true);
    pause_->setToolTip(fc::text("Pause display; capture continues."));
    styleDisplayControl();
    updateViewNotice();
    decoder_ = new QComboBox;
    for (const auto &name :
         {"none", "ascii_lines", "hex_dump", "utf8_lossy", "json_lines", "modbus_rtu", "nmea0183"})
        fc::addValues(decoder_, {name});
    viewTools->addWidget(search, 1);
    viewTools->addWidget(viewMode);
    viewTools->addWidget(regex);
    viewTools->addWidget(hex);
    viewTools->addWidget(relative);
    viewTools->addWidget(pause_);
    viewTools->addWidget(decoder_);
    layout->addLayout(viewTools);
    proxy_->setSourceModel(model_);
    proxy_->setFilterKeyColumn(-1);
    proxy_->setFilterCaseSensitivity(Qt::CaseInsensitive);
    table_->setModel(proxy_);
    table_->setAlternatingRowColors(true);
    table_->setSelectionBehavior(QAbstractItemView::SelectRows);
    table_->setSelectionMode(QAbstractItemView::ExtendedSelection);
    table_->setShowGrid(false);
    table_->setWordWrap(false);
    table_->verticalHeader()->hide();
    table_->verticalHeader()->setDefaultSectionSize(22);
    table_->horizontalHeader()->setSectionResizeMode(3, QHeaderView::Stretch);
    table_->horizontalHeader()->setSectionResizeMode(4, QHeaderView::Interactive);
    table_->setColumnWidth(0, 75);
    table_->setColumnWidth(1, 125);
    table_->setColumnWidth(2, 70);
    table_->setColumnWidth(4, 260);
    textStream_ = new TextStreamView(this);
    markAction_ = new QAction(fc::text("Mark"), this);
    markAction_->setObjectName("markReceiveSelection");
    markAction_->setShortcut(QKeySequence("Ctrl+M"));
    measureAction_ = new QAction(fc::text("Measure"), this);
    measureAction_->setObjectName("measureReceiveSelection");
    measureAction_->setShortcut(QKeySequence("Ctrl+Shift+T"));
    clearAction_ = new QAction(fc::text("Clear view"), this);
    clearAction_->setObjectName("clearReceiveView");
    clearAction_->setShortcut(QKeySequence("Ctrl+L"));
    for (auto *action : {markAction_, measureAction_, clearAction_}) {
        action->setShortcutContext(Qt::WidgetWithChildrenShortcut);
        addAction(action);
    }
    connect(markAction_, &QAction::triggered, this, &SessionPane::addMarker);
    connect(measureAction_, &QAction::triggered, this, &SessionPane::measureSelection);
    connect(clearAction_, &QAction::triggered, this, &SessionPane::clearView);
    textStream_->setContextMenuPolicy(Qt::CustomContextMenu);
    table_->setContextMenuPolicy(Qt::CustomContextMenu);
    connect(textStream_, &QWidget::customContextMenuRequested, this,
            [this](const QPoint &p) { receiveContextMenu(p, true); });
    connect(table_, &QWidget::customContextMenuRequested, this,
            [this](const QPoint &p) { receiveContextMenu(p, false); });
    connect(textStream_, &TextStreamView::frameInspected, this, &SessionPane::frameSelected);
    diskHistory_ = new SessionHistory(this);
    connect(diskHistory_, &SessionHistory::failed, this, [this](const QString &error) {
        historyLoading_ = false;
        emit message(error);
    });
    connect(diskHistory_, &SessionHistory::indexed, this, [this](int) {
        if (historyLoading_)
            diskHistory_->locate(historyTarget_, viewGeneration_);
    });
    connect(diskHistory_, &SessionHistory::windowReady, this,
            [this](int page, int, const QJsonArray &records, quint64 generation) {
                if (generation != viewGeneration_ || !paused_)
                    return;
                historyApplying_ = true;
                diskView_ = true;
                historyPage_ = page;
                textStream_->showHistory(records);
                if (!textStream_->jumpToHistoryKey(historyAnchor_, true) && historyTarget_ >= 0)
                    textStream_->jumpToSequence(historyTarget_);
                historyLoading_ = false;
                historyApplying_ = false;
                historyAnchor_.clear();
            });
    connect(textStream_->verticalScrollBar(), &QScrollBar::valueChanged, this, [this](int value) {
        if (!paused_ || historyLoading_ || historyApplying_)
            return;
        if (value == 0)
            QTimer::singleShot(0, this, [this] { historyEdge(true); });
        else if (diskView_ && value == textStream_->verticalScrollBar()->maximum())
            QTimer::singleShot(0, this, [this] { historyEdge(false); });
    });
    historySearchTimer_.setSingleShot(true);
    historySearchTimer_.setInterval(300);
    connect(&historySearchTimer_, &QTimer::timeout, this, [this] {
        if (!capturePath().isEmpty() && !historySearch_.pattern().isEmpty()) {
            diskHistory_->open(capturePath(),
                               session_.value("config").toObject().value("record_to").toString().isEmpty());
            diskHistory_->search(historySearch_);
        }
    });
    connect(diskHistory_, &SessionHistory::matchesReady, this,
            [this](const QJsonArray &matches, bool complete) {
                searchResults_->clear();
                searchResults_->setVisible(!matches.isEmpty());
                for (const auto &v : matches) {
                    const auto result = v.toObject();
                    auto *item = new QListWidgetItem(result.value("text").toString(), searchResults_);
                    item->setData(Qt::UserRole, result);
                }
                if (complete)
                    emit message(fc::text("Saved log search complete."));
                else if (matches.size() >= 1000)
                    emit message(fc::text("Showing the first 1000 matches."));
                else
                    emit message(fc::text("Saved log search has limited coverage: long lines were "
                                          "searched in overlapping windows."));
            });
    receiveViews_ = new QStackedWidget(this);
    receiveViews_->addWidget(textStream_);
    receiveViews_->addWidget(table_);
    auto *logSplit = new QSplitter(Qt::Vertical);
    logSplit_ = logSplit;
    logSplit->setObjectName("receiveLogSplit");
    logSplit->setChildrenCollapsible(false);
    logSplit->addWidget(receiveViews_);
    sentLog_ = new SentLog;
    logSplit->addWidget(sentLog_);
    logSplit->setStretchFactor(0, 1);
    logSplit->setStretchFactor(1, 0);
    sentDrawerHeight_ = qBound(80, QSettings().value("sentDrawerHeight", 150).toInt(), 400);
    connect(sentLog_, &SentLog::expandedChanged, this, [this] { sizeSentDrawer(); });
    connect(logSplit, &QSplitter::splitterMoved, this, [this] {
        if (sentLog_->isExpanded()) {
            sentDrawerHeight_ = logSplit_->sizes().value(1);
            QSettings().setValue("sentDrawerHeight", sentDrawerHeight_);
        }
    });
    sizeSentDrawer();
    layout->addWidget(logSplit, 1);
    connect(sentLog_, &SentLog::selected, this, &SessionPane::frameSelected);
    connect(sentLog_, &SentLog::locate, this, &SessionPane::locateSent);
    searchResults_ = new QListWidget;
    searchResults_->setMaximumHeight(125);
    searchResults_->hide();
    layout->addWidget(searchResults_);
    connect(searchResults_, &QListWidget::itemClicked, this, [this](QListWidgetItem *item) {
        beginNavigation();
        const auto result = item->data(Qt::UserRole).toJsonObject();
        if (result.contains("history_key")) {
            setPaused(true);
            if (auto *mode = findChild<QComboBox *>("receiveViewMode"))
                mode->setCurrentIndex(0);
            historyAnchor_ = TextStreamView::historyPositionKey(result.value("history_key").toString(),
                                                                result.value("history_offset").toInteger());
            historyTarget_ = result.value("seq").toInteger(-1);
            historyLoading_ = true;
            diskHistory_->window(result.value("page").toInt(), ++viewGeneration_);
            return;
        }
        setPaused(true);
        if (auto *mode = findChild<QComboBox *>("receiveViewMode"))
            mode->setCurrentIndex(0);
        textStream_->jumpToBlock(item->data(Qt::UserRole).toInt());
    });
    auto selectView = [this, viewMode, relative, hex](int index) {
        if (index == 0)
            hex->setChecked(false); // Also updates the raw model through toggled.
        receiveViews_->setCurrentIndex(index);
        updateViewNotice();
        relative->setEnabled(index == 1);
        QSettings().setValue("receiveView", viewMode->currentData());
    };
    connect(viewMode, &QComboBox::currentIndexChanged, this, selectView);
    const int savedView = QSettings().value("receiveView", "text").toString() == "chunks" ? 1 : 0;
    viewMode->setCurrentIndex(savedView);
    selectView(savedView);
    connect(table_->selectionModel(), &QItemSelectionModel::currentRowChanged, this,
            [this](const QModelIndex &current) {
                emit frameSelected(model_->frame(proxy_->mapToSource(current).row()));
            });
    // User interaction signals only: model insertion, search highlights and
    // programmatic followTail must never pause an otherwise live view.
    connect(table_, &QTableView::pressed, this, [this](const QModelIndex &index) {
        if (index.isValid())
            beginNavigation();
    });
    for (auto *scrollbar : {table_->verticalScrollBar(), textStream_->verticalScrollBar()}) {
        connect(scrollbar, &QScrollBar::sliderMoved, this, [this, scrollbar](int position) {
            if (position < scrollbar->maximum())
                beginNavigation();
        });
        connect(scrollbar, &QScrollBar::actionTriggered, this, [this, scrollbar](int) {
            if (scrollbar->sliderPosition() < scrollbar->maximum())
                beginNavigation();
        });
    }
    connect(hex, &QCheckBox::toggled, model_, &FramesModel::setHex);
    connect(hex, &QCheckBox::toggled, this, [viewMode](bool checked) {
        if (checked)
            viewMode->setCurrentIndex(1);
    });
    connect(relative, &QCheckBox::toggled, model_, &FramesModel::setRelative);
    connect(pause_, &QPushButton::toggled, this, &SessionPane::setPaused);
    auto filter = [this, search, regex] {
        const auto pattern = regex->isChecked() ? search->text() : QRegularExpression::escape(search->text());
        QRegularExpression expression(pattern, QRegularExpression::CaseInsensitiveOption);
        if (expression.isValid()) {
            proxy_->setFilterRegularExpression(expression);
            textStream_->setSearch(expression);
            historySearch_ = expression;
            diskHistory_->cancelSearch();
            historySearchTimer_.stop();
            if (!expression.pattern().isEmpty())
                historySearchTimer_.start();
            refreshSearchResults();
            search->setToolTip({});
        } else
            search->setToolTip(expression.errorString());
    };
    connect(search, &QLineEdit::textChanged, this, filter);
    connect(regex, &QCheckBox::toggled, this, filter);
    connect(decoder_, &QComboBox::currentIndexChanged, this, [this] {
        const auto generation = ++decoderGeneration_;
        decoder_->setToolTip({});
        const auto name = decoder_->currentData().toString();
        QJsonValue spec = name == "none"
                              ? QJsonValue::Null
                              : QJsonValue(QJsonObject{{"name", name}, {"options", QJsonObject{}}});
        call("set_decoder", {{"spec", spec}},
             [this, name, generation](const QJsonObject &, const QString &e) {
                 if (generation != decoderGeneration_)
                     return;
                 if (e.isEmpty()) {
                     const auto key = session_.value("profile_key").toString();
                     auto profile = fc::loadProfile(key);
                     profile["decoder"] = name;
                     fc::saveProfile(key, profile);
                 } else {
                     decoder_->setToolTip(e);
                     refreshDecoder(generation);
                 }
             });
    });
    auto *sendRow = new QHBoxLayout;
    input_ = new QLineEdit;
    history_ = QSettings().value("sendHistory").toStringList();
    historyIndex_ = history_.size();
    input_->installEventFilter(this);
    table_->viewport()->installEventFilter(this);
    table_->installEventFilter(this);
    textStream_->viewport()->installEventFilter(this);
    textStream_->installEventFilter(this);
    input_->setPlaceholderText(fc::text("Command or hex bytes…"));
    encoding_ = new QComboBox;
    encoding_->addItems({"UTF-8", "HEX"});
    newline_ = new QComboBox;
    newline_->setObjectName("sendNewline");
    fc::addValues(newline_, {"CRLF", "LF", "CR", "None"});
    fc::selectValue(newline_, session_.value("send_newline").toString("CRLF"));
    connect(newline_, &QComboBox::currentIndexChanged, this, [this] {
        const auto ending = newline_->currentData().toString();
        auto key = session_.value("profile_key").toString();
        if (key.isEmpty())
            return;
        auto profile = fc::loadProfile(key);
        profile["newline"] = ending;
        fc::saveProfile(key, profile);
    });
    newline_->setToolTip(
        fc::text("Appends a line ending to transmitted text only; it does not change receive rendering."));
    auto *sendButton = new QPushButton(fc::text("Send"));
    sendButton->setObjectName("primary");
    sendRow->addWidget(input_, 1);
    sendRow->addWidget(encoding_);
    sendRow->addWidget(newline_);
    sendRow->addWidget(sendButton);
    layout->addLayout(sendRow);
    auto *automation = new QHBoxLayout;
    periodic_ = new QCheckBox(fc::text("Repeat"));
    interval_ = new QSpinBox;
    interval_->setRange(20, 3600000);
    interval_->setValue(1000);
    interval_->setSuffix(" ms");
    automation->addWidget(periodic_);
    automation->addWidget(interval_);
    automation->addStretch();
    layout->addLayout(automation);
    connect(sendButton, &QPushButton::clicked, this, [this] { send(input_->text()); });
    connect(input_, &QLineEdit::returnPressed, this, [this] { send(input_->text()); });
    connect(&sendTimer_, &QTimer::timeout, this, [this] { send(input_->text()); });
    connect(periodic_, &QCheckBox::toggled, this, [this](bool checked) {
        if (checked)
            sendTimer_.start(interval_->value());
        else
            sendTimer_.stop();
    });
    connect(interval_, &QSpinBox::valueChanged, this, [this](int value) {
        if (sendTimer_.isActive())
            sendTimer_.setInterval(value);
    });
    auto *findShortcut = new QShortcut(QKeySequence::Find, this);
    findShortcut->setContext(Qt::WidgetWithChildrenShortcut);
    connect(findShortcut, &QShortcut::activated, this, &SessionPane::focusSearch);
    pollTimer_.setInterval(50);
    connect(&pollTimer_, &QTimer::timeout, this, &SessionPane::poll);
    pollTimer_.start();
    sentTimer_.setInterval(200);
    connect(&sentTimer_, &QTimer::timeout, this, &SessionPane::pollSent);
    sentTimer_.start();
    statsTimer_.setInterval(1000);
    connect(&statsTimer_, &QTimer::timeout, this, &SessionPane::pollBootEvents);
    connect(&statsTimer_, &QTimer::timeout, this, [this] {
        if (!rpc_->ready())
            return;
        QPointer<SessionPane> self(this);
        call("get_stats", {}, [this](const QJsonObject &result, const QString &error) {
            if (!error.isEmpty())
                return;
            QPointer<SessionPane> self(this);
            updateRecording(result.value("stats").toObject().value("recording").toObject());
            if (!self)
                return;
            emit statsChanged(result.value("stats").toObject());
        });
        if (!self)
            return;
        call("get_signals", {}, [this](const QJsonObject &result, const QString &) {
            emit signalsChanged(result.value("pins").toObject());
        });
    });
    statsTimer_.start();
    connect(rpc_, &RpcClient::disconnected, this, [this] {
        sessionStateConfirmed_ = false;
        updateViewNotice();
        pending_ = false;
        sentPending_ = false;
        sentUnsupported_ = false;
        periodic_->setChecked(false);
    });
    connect(rpc_, &RpcClient::connected, this, [this] { updateViewNotice(); });
    connect(rpc_, &RpcClient::notification, this, [this](const QString &method, const QJsonObject &params) {
        if (method != "session_state" || params.value("session_id").toString() != sessionId())
            return;
        session_["state"] = params.value("state");
        sessionStateConfirmed_ = true;
        updateViewNotice();
    });
    refreshDecoder(decoderGeneration_);
}
void SessionPane::refreshDecoder(quint64 generation) {
    call("get_decoder", {}, [this, generation](const QJsonObject &result, const QString &error) {
        if (!error.isEmpty() || generation != decoderGeneration_)
            return;
        QSignalBlocker blocker(decoder_);
        const auto name = result.value("spec").toObject().value("name").toString("none");
        fc::selectValue(decoder_, name);
    });
}
void SessionPane::sizeSentDrawer() {
    const int available = qMax(0, logSplit_->height() - logSplit_->handleWidth());
    const int compact = sentLog_->collapsedHeight();
    const int limit = qMax(compact, available / 3);
    const bool expanded = sentLog_->isExpanded();
    sentLog_->setMaximumHeight(expanded ? limit : compact);
    logSplit_->handle(1)->setEnabled(expanded);
    const int height = expanded ? qBound(compact, sentDrawerHeight_, limit) : compact;
    logSplit_->setSizes({qMax(0, available - height), height});
}
void SessionPane::resizeEvent(QResizeEvent *event) {
    QWidget::resizeEvent(event);
    // Layout geometry is updated after the pane's resize event.
    QTimer::singleShot(0, this, [this] { sizeSentDrawer(); });
}
QString SessionPane::label() const {
    const auto label = session_.value("label").toString();
    return label.isEmpty() ? session_.value("path").toString() : label;
}
void SessionPane::updateSession(const QJsonObject &session) {
    const auto key = session_.value("profile_key");
    const auto ending = session_.value("send_newline");
    session_ = session;
    if (!session_.contains("profile_key") && !key.isUndefined())
        session_["profile_key"] = key;
    if (!session_.contains("send_newline") && !ending.isUndefined())
        session_["send_newline"] = ending;
    QPointer<SessionPane> self(this);
    updateRecording(session.value("stats").toObject().value("recording").toObject());
    if (!self)
        return;
    sessionStateConfirmed_ = session.value("state").toObject().contains("state");
    updateViewNotice();
}
void SessionPane::updateViewNotice() {
    const auto state = session_.value("state").toObject();
    const auto name = state.value("state").toString();
    QString status, caption;
    if (!rpc_->ready()) {
        status = fc::text("Background service disconnected; capture status is unknown.");
        caption = fc::text("Service disconnected");
    } else if (!sessionStateConfirmed_ || name.isEmpty()) {
        status = fc::text("Checking serial connection status.");
        caption = fc::text("Checking connection");
    } else if (name == "reconnecting") {
        status = fc::text("Serial port disconnected; reconnecting.");
        caption = fc::text("Reconnecting...");
    } else if (name == "closed") {
        status = fc::text("Serial session closed.");
        caption = fc::text("Closed");
    } else if (name == "failed") {
        status = fc::text("Serial connection failed: %1").arg(state.value("error").toString());
        caption = fc::text("Connection failed");
    } else if (name == "connected") {
        status = fc::text("Connected");
        caption = status;
    }
    connectionState_->setText(caption);
    connectionState_->setToolTip(status);
    connectionState_->setAccessibleName(status);
    const bool known = rpc_->ready() && sessionStateConfirmed_ && !name.isEmpty();
    const QString level = !rpc_->ready()                         ? "error"
                          : !known                               ? "pending"
                          : name == "connected"                  ? "connected"
                          : name == "reconnecting"               ? "pending"
                          : name == "failed" || name == "closed" ? "error"
                                                                 : "pending";
    connectionState_->setProperty("connectionLevel", level);
    styleConnectionState();
    pause_->setText(paused_ ? fc::text("Resume live") : fc::text("Pause display"));
    pause_->setAccessibleName(pause_->text());
    const bool connected = rpc_->ready() && sessionStateConfirmed_ && name == "connected";
    pause_->setToolTip(
        !paused_ ? (connected ? fc::text("Pause display; capture continues.") : status)
        : connected
            ? fc::text("Display paused; serial capture continues. Click to jump to the latest output.")
            : fc::text("Display paused.") + " " + status);
}
void SessionPane::styleConnectionState() {
    const bool dark = palette().color(QPalette::Window).lightness() < 128;
    const auto level = connectionState_->property("connectionLevel").toString();
    const bool connected = level == "connected", pending = level == "pending";
    const QString foreground = connected ? (dark ? "#89e3b2" : "#155f38")
                               : pending ? (dark ? "#ffe0a0" : "#775009")
                                         : (dark ? "#ffb2bc" : "#99243a");
    const QString background = connected ? (dark ? "#193c2c" : "#e3f4e9")
                               : pending ? (dark ? "#493a1d" : "#fff1d1")
                                         : (dark ? "#482831" : "#fbe7eb");
    const QString border = connected ? (dark ? "#438b63" : "#7fb594")
                           : pending ? (dark ? "#a17c39" : "#c6a35a")
                                     : (dark ? "#a45969" : "#d38d9b");
    connectionState_->setStyleSheet(
        QString("QLabel#sessionConnectionState { color: %1; background-color: %2; border: 1px solid %3; "
                "border-radius: 3px; padding: 3px 9px; font-weight: 600; }")
            .arg(foreground, background, border));
}
void SessionPane::styleDisplayControl() {
    const bool dark = palette().color(QPalette::Window).lightness() < 128;
    pause_->setStyleSheet(
        QString("QPushButton#pauseReceiveView:checked { background: %1; color: %2; border: 1px solid %3; }")
            .arg(dark ? "#59451f" : "#fff0c7", dark ? "#ffe3a3" : "#60450b", dark ? "#b99046" : "#b88c32"));
    pause_->setMinimumWidth(qMax(pause_->fontMetrics().horizontalAdvance(fc::text("Pause display")),
                                 pause_->fontMetrics().horizontalAdvance(fc::text("Resume live"))) +
                            30);
}
void SessionPane::updateRecording(const QJsonObject &recording) {
    recordingStatus_ = recording;
    const auto config = session_.value("config").toObject();
    const auto rx = config.value("record_rx_to").toString();
    const auto jsonl = config.value("record_to").toString();
    const auto readable = recording.value("readable_path").toString();
    recordingFolder_->setEnabled(!rx.isEmpty() || !jsonl.isEmpty());
    QStringList errors;
    for (const auto &error : recording.value("errors").toArray())
        errors.append(error.toString());
    QString text;
    bool reportError = false;
    if (!errors.isEmpty()) {
        text = fc::text("Recording failed: ") + errors.join("; ");
        recording_->setStyleSheet("color: #ed7985");
        if (recordingError_ != text) {
            recordingError_ = text;
            reportError = true;
        }
    } else if (rx.isEmpty() && jsonl.isEmpty()) {
        text = fc::text("Recording off");
        recording_->setStyleSheet({});
    } else {
        text = recording.contains("active")
                   ? (recording.value("active").toBool() ? fc::text("Recording: ") : fc::text("Saved: "))
                   : fc::text("Checking recording status: ");
        text += QFileInfo(!readable.isEmpty() ? readable : rx.isEmpty() ? jsonl : rx).fileName();
        recording_->setStyleSheet({});
    }
    recording_->setText(text);
    recording_->setToolTip("TXT: " + readable + "\nRX: " + rx + "\nJSONL: " + jsonl + "\n" +
                           fc::text("Pausing or clearing the display does not stop recording."));
    // A direct message handler may destroy this pane. Complete widget updates first.
    if (reportError)
        emit message(text);
}
void SessionPane::retranslate() {
    styleDisplayControl();
    updateViewNotice();
    QPointer<SessionPane> self(this);
    updateRecording(recordingStatus_);
    if (!self)
        return;
    for (auto *combo : {decoder_, newline_}) {
        const QSignalBlocker blocker(combo);
        for (int i = 0; i < combo->count(); ++i)
            combo->setItemText(i, fc::valueLabel(combo->itemData(i).toString()));
    }
    model_->headerDataChanged(Qt::Horizontal, 0, model_->columnCount() - 1);
}
void SessionPane::setPaused(bool paused) {
    const bool changed = paused_ != paused;
    if (paused_ != paused)
        ++viewGeneration_;
    if (paused_ && !paused)
        tailPending_ = true;
    paused_ = paused;
    follow_ = !paused;
    const QSignalBlocker blocker(pause_);
    pause_->setChecked(paused);
    updateViewNotice();
    if (!paused) {
        diskView_ = false;
        historyLoading_ = false;
        historyAnchor_.clear();
        diskHistory_->cancelSearch();
        table_->scrollToBottom();
        textStream_->followTail();
    }
    if (changed)
        emit displayPausedChanged(paused_);
}
void SessionPane::focusSearch() {
    if (auto *search = findChild<QLineEdit *>("receiveSearch")) {
        search->setFocus();
        search->selectAll();
    }
}
void SessionPane::copySelection() {
    if (receiveViews_->currentWidget() == textStream_)
        textStream_->copy();
    else if (!evidence().isEmpty())
        QApplication::clipboard()->setText(evidence().value("text").toString());
}
void SessionPane::selectAllOutput() {
    beginNavigation();
    if (receiveViews_->currentWidget() == textStream_)
        textStream_->selectAll();
    else
        table_->selectAll();
}
void SessionPane::longCommand() {
    bool ok = false;
    const auto text =
        QInputDialog::getMultiLineText(this, fc::text("Long / multiline command"),
                                       fc::text("Uses current encoding and line ending"), {}, &ok);
    if (ok)
        send(text);
}
QString SessionPane::capturePath() const {
    const auto config = session_.value("config").toObject();
    const auto path = config.value("record_to").toString();
    return path.isEmpty() ? config.value("record_rx_to").toString() : path;
}
void SessionPane::openHistory(qint64 seq) {
    if (capturePath().isEmpty()) {
        emit message(fc::text("No saved capture is available for this session."));
        return;
    }
    beginNavigation();
    if (auto *mode = findChild<QComboBox *>("receiveViewMode"))
        mode->setCurrentIndex(0);
    historyLoading_ = true;
    historySearchTimer_.stop();
    historyTarget_ = seq;
    historyAnchor_.clear();
    ++viewGeneration_;
    diskHistory_->open(capturePath(),
                       session_.value("config").toObject().value("record_to").toString().isEmpty());
    emit message(fc::text("Loading saved log..."));
}
void SessionPane::historyEdge(bool earlier) {
    if (historyLoading_ || historyApplying_ || !paused_ || receiveViews_->currentWidget() != textStream_)
        return;
    const auto scroll = textStream_->verticalScrollBar();
    if (earlier && scroll->value() != 0)
        return;
    if (!earlier && scroll->value() != scroll->maximum())
        return;
    if (!diskView_) {
        if (earlier)
            openHistory(textStream_->firstSequence());
        return;
    }
    const int next =
        earlier ? qMax(0, historyPage_ - 1) : qMin(historyPage_ + 1, qMax(0, diskHistory_->pages() - 3));
    if (next == historyPage_) {
        if (earlier)
            emit message(fc::text("Beginning of saved log."));
        return;
    }
    historyAnchor_ = textStream_->topHistoryKey();
    historyTarget_ = -1;
    historyLoading_ = true;
    diskHistory_->window(next, ++viewGeneration_);
}
void SessionPane::call(const QString &method, QJsonObject params, RpcClient::Callback callback,
                       int timeoutMs) {
    params["session_id"] = sessionId();
    QPointer<SessionPane> self(this);
    controller_->request(
        method, params,
        [self, callback](const QJsonObject &result, const QString &failure) {
            if (!self)
                return;
            if (result.contains("annotation_error"))
                emit self->message(result.value("annotation_error").toString());
            if (!self)
                return;
            if (!failure.isEmpty()) {
                emit self->message(failure);
                if (!self)
                    return;
                self->periodic_->setChecked(false);
            }
            if (self && callback)
                callback(result, failure);
        },
        timeoutMs);
}
void SessionPane::poll() {
    if (!rpc_->ready() || pending_ || paused_)
        return;
    pending_ = true;
    const auto generation = viewGeneration_;
    const bool tail = tailPending_;
    tailPending_ = false;
    QJsonObject request{{"max_bytes", 65536}, {"format", "decoded"}, {"tail", tail}};
    if (!tail)
        request["since_seq"] = cursor_;
    call("read_frames", request, [this, generation, tail](const QJsonObject &result, const QString &failure) {
        pending_ = false;
        // A request may have been sent just before a click/scroll paused
        // the view. Leave its cursor unconsumed so resume fetches it again.
        if (!failure.isEmpty()) {
            if (tail)
                tailPending_ = true;
            return;
        }
        if (paused_ || generation != viewGeneration_)
            return;
        if (tail) {
            model_->clear();
            textStream_->clearStream();
        }
        const auto first = result.value("first_seq").toInteger();
        if (!tail && first > cursor_) {
            textStream_->markGap();
            QPointer<SessionPane> self(this);
            emit message(fc::text("Some data was removed from the buffer; check the capture file."));
            if (!self)
                return;
        }
        cursor_ = result.value("next_seq").toInteger(cursor_);
        const auto frames = result.value("frames").toArray();
        model_->append(frames);
        textStream_->appendFrames(frames);
        if (!result.value("up_to_date").toBool(true))
            tailPending_ = true;
        refreshSearchResults();
        if (!frames.isEmpty()) {
            if (follow_)
                table_->scrollToBottom();
            emit receivedFrames(model_->rowCount());
        }
    });
}
void SessionPane::send(const QString &text) {
    if (text.isEmpty())
        return;
    if (encoding_->currentText() == "HEX") {
        QString compact = text;
        compact.remove(QRegularExpression("\\s+"));
        if (compact.size() % 2 || !QRegularExpression("^[0-9a-fA-F]+$").match(compact).hasMatch()) {
            emit message(fc::text("HEX must contain complete bytes"));
            return;
        }
    }
    const bool hex = encoding_->currentText() == "HEX";
    if (text.toUtf8().size() > 4096) {
        QString payload = text;
        if (!hex) {
            const auto ending = newline_->currentData().toString();
            payload += ending == "CRLF" ? "\r\n" : ending == "LF" ? "\n" : ending == "CR" ? "\r" : "";
        }
        startOperation("start_transfer", {{hex ? "hex" : "data", payload}, {"chunk_size", 1024}});
        return;
    }
    if (history_.isEmpty() || history_.last() != text) {
        history_.append(text);
        while (history_.size() > 200)
            history_.removeFirst();
        QSettings().setValue("sendHistory", history_);
    }
    historyIndex_ = history_.size();
    call("send",
         {{hex ? "hex" : "data", text},
          {"newline", hex ? "none" : newline_->currentData().toString().toLower()}},
         [this](const QJsonObject &result, const QString &failure) {
             if (failure.isEmpty()) {
                 QPointer<SessionPane> self(this);
                 emit message(fc::text("Sent ") + QString::number(result.value("bytes_sent").toInteger()) +
                              " B");
                 if (!self)
                     return;
                 emit sent();
             }
         });
}
void SessionPane::insertMacro(const QString &text) {
    input_->setText(text);
    input_->setFocus();
}
void SessionPane::saveCurrentMacro() {
    auto values = QSettings().value("macros").toStringList();
    if (input_->text().isEmpty())
        return;
    if (!values.contains(input_->text()))
        values.append(input_->text());
    QSettings().setValue("macros", values);
    emit macrosChanged();
}
void SessionPane::clearView() {
    ++viewGeneration_;
    diskView_ = false;
    historyLoading_ = false;
    model_->clear();
    textStream_->clearStream();
    searchResults_->clear();
}
void SessionPane::receiveContextMenu(const QPoint &position, bool textView) {
    beginNavigation();
    // Opening a menu must preserve the existing text/chunk selection.
    auto *menu = textView ? textStream_->createStandardContextMenu() : new QMenu(this);
    if (!textView) {
        auto *copy = menu->addAction(fc::text("Copy"));
        copy->setEnabled(!table_->selectionModel()->selectedRows().isEmpty());
        connect(copy, &QAction::triggered, this,
                [this] { QApplication::clipboard()->setText(evidence().value("text").toString()); });
    }
    menu->addSeparator();
    menu->addAction(markAction_);
    auto *labeled = menu->addAction(fc::text("Add labeled marker..."));
    connect(labeled, &QAction::triggered, this, [this] {
        bool ok = false;
        auto label = QInputDialog::getText(this, fc::text("Add marker"), fc::text("Action / observation"),
                                           QLineEdit::Normal, {}, &ok);
        if (!ok || label.isEmpty())
            return;
        QJsonObject params{{"label", label}};
        auto selected = evidence();
        if (selected.contains("from_seq"))
            params["seq"] = selected.value("from_seq");
        call("add_marker", params);
    });
    auto *measure = menu->addAction(measureAction_->text());
    measure->setShortcut(measureAction_->shortcut());
    measure->setEnabled(evidence().contains("elapsed_us"));
    connect(measure, &QAction::triggered, this, &SessionPane::measureSelection);
    menu->addSeparator();
    menu->addAction(clearAction_);
    menu->exec((textView ? textStream_->viewport() : table_->viewport())->mapToGlobal(position));
    delete menu;
}
void SessionPane::exportLog() {
    const auto readableFilter = fc::text("Readable capture (*.log)");
    const auto path =
        QFileDialog::getSaveFileName(this, fc::text("Export log"), "capture.log", readableFilter);
    if (path.isEmpty())
        return;
    call(
        "export_readable", {{"path", path}, {"all", true}},
        [this, path](const QJsonObject &, const QString &error) {
            if (error.isEmpty())
                emit message(path);
        },
        60000);
}
void SessionPane::sendFile() {
    const auto path = QFileDialog::getOpenFileName(this, fc::text("Send file"));
    if (path.isEmpty())
        return;
    startOperation("start_transfer", {{"path", path}, {"chunk_size", 1024}, {"pacing_ms", 20}});
}
void SessionPane::replayLog() {
    const auto path = QFileDialog::getOpenFileName(this, fc::text("Replay capture"), {}, "JSONL (*.jsonl)");
    if (path.isEmpty())
        return;
    bool ok = false;
    const auto speed =
        QInputDialog::getDouble(this, fc::text("Replay"), fc::text("Speed multiplier"), 1, 0.01, 100, 2, &ok);
    if (!ok)
        return;
    startOperation("start_transfer", {{"path", path}, {"speed", speed}, {"replay", true}});
}
void SessionPane::editTriggers() {
    call("get_triggers", {}, [this](const QJsonObject &r, const QString &error) {
        if (!error.isEmpty())
            return;
        TriggersDialog dialog(r.value("triggers").toArray(), this);
        if (dialog.exec() != QDialog::Accepted)
            return;
        call("set_triggers", {{"triggers", dialog.triggers()}});
    });
}
void SessionPane::configure() {
    QDialog dialog(this);
    dialog.setWindowTitle(fc::text("Serial settings"));
    auto *form = new QFormLayout(&dialog);
    const auto current = session_.value("config").toObject();
    auto *baud = new QSpinBox;
    baud->setRange(1, 12000000);
    baud->setValue(current.value("baud").toInt(115200));
    form->addRow(fc::text("Baud"), baud);
    QMap<QString, QComboBox *> options;
    const QMap<QString, QStringList> values{{"data_bits", {"eight", "seven", "six", "five"}},
                                            {"parity", {"none", "odd", "even", "mark", "space"}},
                                            {"stop_bits", {"one", "one_point_five", "two"}},
                                            {"flow_control", {"none", "hardware", "software"}}};
    const QMap<QString, QString> captions{{"data_bits", fc::text("Data bits")},
                                          {"parity", fc::text("Parity")},
                                          {"stop_bits", fc::text("Stop bits")},
                                          {"flow_control", fc::text("Flow control")}};
    for (auto it = values.begin(); it != values.end(); ++it) {
        auto *combo = new QComboBox;
        fc::addValues(combo, it.value());
        if (it.key() == "stop_bits")
            combo->setToolTip(
                fc::text("1.5 stop bits require 5 data bits; 2 stop bits require 6–8 data bits."));
        fc::selectValue(combo, current.value(it.key()).toString());
        form->addRow(captions.value(it.key()), combo);
        options.insert(it.key(), combo);
    }
    auto *buttons = new QDialogButtonBox(QDialogButtonBox::Save | QDialogButtonBox::Cancel);
    form->addRow(buttons);
    connect(buttons, &QDialogButtonBox::accepted, &dialog, &QDialog::accept);
    connect(buttons, &QDialogButtonBox::rejected, &dialog, &QDialog::reject);
    if (dialog.exec() == QDialog::Accepted) {
        QJsonObject params{{"baud", baud->value()}};
        for (auto it = options.begin(); it != options.end(); ++it)
            params[it.key()] = it.value()->currentData().toString();
        call("configure_session", params, [this](const QJsonObject &r, const QString &e) {
            if (e.isEmpty()) {
                session_["config"] = r.value("config");
                const auto key = session_.value("profile_key").toString();
                if (!key.isEmpty()) {
                    auto profile = fc::loadProfile(key);
                    const auto config = r.value("config").toObject();
                    for (auto i = config.begin(); i != config.end(); ++i)
                        if (i.key() != "record_to" && i.key() != "record_rx_to")
                            profile[i.key()] = i.value();
                    fc::saveProfile(key, profile);
                }
            }
        });
    }
}

void SessionPane::editDecoder() {
    const auto generation = ++decoderGeneration_;
    call("get_decoder", {}, [this, generation](const QJsonObject &result, const QString &error) {
        if (!error.isEmpty() || generation != decoderGeneration_)
            return;
        auto spec = result.value("spec").toObject();
        // This read superseded initialization, so reconcile the known backend
        // state even if the user subsequently cancels the editor.
        {
            const QSignalBlocker blocker(decoder_);
            fc::selectValue(decoder_, spec.value("name").toString("none"));
        }
        QPointer<SessionPane> self(this);
        const bool accepted = fc::editDecoderOptions(spec, this);
        if (!self || !accepted || generation != decoderGeneration_)
            return;
        call("set_decoder", {{"spec", spec.isEmpty() ? QJsonValue(QJsonValue::Null) : QJsonValue(spec)}},
             [this, spec, generation](const QJsonObject &, const QString &e) {
                 if (generation != decoderGeneration_)
                     return;
                 decoder_->setToolTip(e);
                 if (!e.isEmpty()) {
                     refreshDecoder(generation);
                     return;
                 }
                 QSignalBlocker block(decoder_);
                 auto name = spec.value("name").toString("none");
                 fc::selectValue(decoder_, name);
             });
    });
}

bool SessionPane::eventFilter(QObject *object, QEvent *event) {
    const bool tableViewport = object == table_->viewport();
    const bool textViewport = object == textStream_->viewport();
    if ((tableViewport || textViewport) && event->type() == QEvent::PaletteChange) {
        styleDisplayControl();
        styleConnectionState();
    }
    if (tableViewport || textViewport) {
        if (event->type() == QEvent::Wheel) {
            const auto *wheel = static_cast<QWheelEvent *>(event);
            if (wheel->angleDelta().y() > 0 || wheel->pixelDelta().y() > 0)
                beginNavigation();
            else if (paused_)
                beginNavigation();
            if (textViewport) {
                const bool earlier = wheel->angleDelta().y() > 0 || wheel->pixelDelta().y() > 0;
                QTimer::singleShot(0, this, [this, earlier] { historyEdge(earlier); });
            }
        } else if (event->type() == QEvent::MouseButtonPress ||
                   event->type() == QEvent::MouseButtonDblClick) {
            const auto *mouse = static_cast<QMouseEvent *>(event);
            if (mouse->button() == Qt::LeftButton &&
                (tableViewport ? table_->indexAt(mouse->position().toPoint()).isValid()
                               : !textStream_->document()->isEmpty()))
                beginNavigation();
        }
    }
    if ((object == table_ || object == textStream_) && event->type() == QEvent::KeyPress) {
        const auto *key = static_cast<QKeyEvent *>(event);
        switch (key->key()) {
        case Qt::Key_Up:
        case Qt::Key_Down:
        case Qt::Key_PageUp:
        case Qt::Key_PageDown:
        case Qt::Key_Home:
        case Qt::Key_End:
            beginNavigation();
            if (object == textStream_) {
                const bool earlier =
                    key->key() == Qt::Key_Up || key->key() == Qt::Key_PageUp || key->key() == Qt::Key_Home;
                QTimer::singleShot(0, this, [this, earlier] { historyEdge(earlier); });
            }
            break;
        default:
            if (key->matches(QKeySequence::SelectAll))
                beginNavigation();
            break;
        }
    }
    if (object == input_ && event->type() == QEvent::KeyPress) {
        auto *key = static_cast<QKeyEvent *>(event);
        if (key->key() == Qt::Key_Up || key->key() == Qt::Key_Down) {
            historyIndex_ = qBound(0, historyIndex_ + (key->key() == Qt::Key_Up ? -1 : 1),
                                   static_cast<int>(history_.size()));
            input_->setText(historyIndex_ < history_.size() ? history_.at(historyIndex_) : QString());
            return true;
        }
    }
    return QWidget::eventFilter(object, event);
}
