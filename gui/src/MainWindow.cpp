/*
 * flattencom - Desktop Workspace
 *
 * Coordinates menus, toolbars, device discovery, shared session tabs and dockable inspectors.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

#include "MainWindow.h"
#include "AboutDialog.h"
#include "DeviceProfiles.h"
#include "Icons.h"
#include "LogManager.h"
#include "Recording.h"
#include "Style.h"
#include <QActionGroup>
#include <QApplication>
#include <QCloseEvent>
#include <QDateTime>
#include <QDialog>
#include <QDialogButtonBox>
#include <QDir>
#include <QDockWidget>
#include <QFileDialog>
#include <QFontDatabase>
#include <QFormLayout>
#include <QGroupBox>
#include <QHeaderView>
#include <QInputDialog>
#include <QJsonDocument>
#include <QMenuBar>
#include <QMessageBox>
#include <QPointer>
#include <QProcess>
#include <QPushButton>
#include <QSettings>
#include <QSignalBlocker>
#include <QStatusBar>
#include <QStyle>
#include <QToolBar>
#include <QToolButton>
#include <QVBoxLayout>

MainWindow::MainWindow(QWidget *parent) : QMainWindow(parent), rpc_(this) {
    setWindowTitle("flattencom");
    resize(1380, 860);
    setDockOptions(AllowNestedDocks | AllowTabbedDocks | AnimatedDocks);
    auto *bar = addToolBar(fc::text("Serial tools"));
    bar->setObjectName("serialTools");
    bar->setToolButtonStyle(Qt::ToolButtonIconOnly);
    bar->setIconSize(QSize(18, 18));
    bar->setAllowedAreas(Qt::TopToolBarArea | Qt::BottomToolBarArea);
    auto *open = bar->addAction(fc::icon(fc::Icon::Open), fc::text("Open port"));
    open->setShortcut(QKeySequence("Ctrl+O"));
    auto *config = bar->addAction(fc::icon(fc::Icon::Configure), fc::text("Configure"));
    auto *exportAction = bar->addAction(fc::icon(fc::Icon::Save), fc::text("Export"));
    exportAction->setShortcut(QKeySequence("Ctrl+S"));
    auto *refreshAction = bar->addAction(fc::icon(fc::Icon::Refresh), fc::text("Refresh"));
    refreshAction->setShortcut(QKeySequence("F5"));
    connect(open, &QAction::triggered, this, [this] { openPort(); });
    connect(config, &QAction::triggered, this, [this] {
        if (auto *p = currentPane())
            p->configure();
    });
    connect(exportAction, &QAction::triggered, this, [this] {
        if (auto *p = currentPane())
            p->exportLog();
    });
    connect(refreshAction, &QAction::triggered, this, &MainWindow::refresh);
    for (auto *action : bar->actions()) {
        const auto hint =
            action->text() + (action->shortcut().isEmpty()
                                  ? QString{}
                                  : " (" + action->shortcut().toString(QKeySequence::NativeText) + ")");
        action->setToolTip(hint);
        action->setStatusTip(action->text());
        if (auto *button = bar->widgetForAction(action))
            button->setAccessibleName(action->text());
    }
    macroBar_ = addToolBar(fc::text("Macros"));
    macroBar_->setObjectName("macroTools");
    macroBar_->setToolButtonStyle(Qt::ToolButtonIconOnly);
    macroBar_->setIconSize(QSize(18, 18));
    macroBar_->setAllowedAreas(Qt::TopToolBarArea | Qt::BottomToolBarArea);
    macroBar_->addWidget(new QLabel(fc::text("Macros")));
    macroList_ = new QComboBox;
    macroList_->setObjectName("macroList");
    macroList_->setMinimumWidth(160);
    macroList_->setMaximumWidth(280);
    macroList_->setSizeAdjustPolicy(QComboBox::AdjustToMinimumContentsLengthWithIcon);
    macroList_->setAccessibleName(fc::text("Macros"));
    macroBar_->addWidget(macroList_);
    auto *insertMacro = macroBar_->addAction(fc::icon(fc::Icon::Insert), fc::text("Insert macro"));
    auto *saveMacro = macroBar_->addAction(fc::icon(fc::Icon::Save), fc::text("Save macro"));
    auto *deleteMacro = macroBar_->addAction(fc::icon(fc::Icon::Delete), fc::text("Delete macro"));
    auto insert = [this] {
        if (auto *pane = currentPane())
            if (macroList_->currentIndex() >= 0)
                pane->insertMacro(macroList_->currentData().toString());
    };
    connect(macroList_, &QComboBox::activated, this, insert);
    connect(insertMacro, &QAction::triggered, this, insert);
    connect(saveMacro, &QAction::triggered, this, [this] {
        if (auto *pane = currentPane())
            pane->saveCurrentMacro();
    });
    connect(deleteMacro, &QAction::triggered, this, [this] {
        if (macroList_->currentIndex() < 0)
            return;
        auto values = QSettings().value("macros").toStringList();
        values.removeAll(macroList_->currentData().toString());
        QSettings().setValue("macros", values);
        refreshMacros();
    });
    refreshMacros();
    auto *fileMenu = menuBar()->addMenu(fc::text("File"));
    fileMenu->setObjectName("fileMenu");
    auto *editMenu = menuBar()->addMenu(fc::text("Edit"));
    auto *view = menuBar()->addMenu(fc::text("View"));
    auto *sessionMenu = menuBar()->addMenu(fc::text("Session"));
    auto *tools = menuBar()->addMenu(fc::text("Tools"));
    auto *windowMenu = menuBar()->addMenu(fc::text("Window"));
    windowMenu->setObjectName("windowMenu");
    QList<QAction *> sessionActions;
    auto command = [this, &sessionActions](QMenu *menu, const QString &label, auto method) {
        auto *action = menu->addAction(label);
        sessionActions.append(action);
        connect(action, &QAction::triggered, this, [this, method] {
            if (auto *pane = currentPane())
                (pane->*method)();
        });
        return action;
    };
    fileMenu->addAction(open);
    fileMenu->addSeparator();
    fileMenu->addAction(exportAction);
    command(fileMenu, fc::text("Issue bundle"), &SessionPane::exportProblem);
    auto *detach = fileMenu->addAction(fc::text("Detach view"));
    detach->setObjectName("detachCurrentView");
    detach->setShortcut(QKeySequence("Ctrl+W"));
    connect(detach, &QAction::triggered, this, &MainWindow::detachCurrentView);
    fileMenu->addSeparator();
    auto *exit = fileMenu->addAction(fc::text("Exit"));
    exit->setShortcut(QKeySequence("Ctrl+Q"));
    connect(exit, &QAction::triggered, this, &QWidget::close);
    command(editMenu, fc::text("Copy"), &SessionPane::copySelection);
    command(editMenu, fc::text("Select all"), &SessionPane::selectAllOutput);
    command(editMenu, fc::text("Find..."), &SessionPane::focusSearch);
    editMenu->addSeparator();
    command(editMenu, fc::text("Mark"), &SessionPane::addMarker);
    command(editMenu, fc::text("Measure"), &SessionPane::measureSelection);
    command(editMenu, fc::text("Clear view"), &SessionPane::clearView);
    sessionMenu->addAction(config);
    auto *pauseAction = sessionMenu->addAction(fc::text("Pause display"));
    pauseAction->setObjectName("sessionPauseAction");
    pauseAction->setCheckable(true);
    sessionActions.append(pauseAction);
    connect(pauseAction, &QAction::triggered, this, [this] {
        if (auto *pane = currentPane())
            pane->setPaused(!pane->isPaused());
    });
    command(sessionMenu, fc::text("Reset"), &SessionPane::resetDevice);
    command(sessionMenu, fc::text("Markers / boots"), &SessionPane::showMarkers);
    command(sessionMenu, fc::text("Long command…"), &SessionPane::longCommand);
    command(sessionMenu, fc::text("Share selection"), &SessionPane::shareSelection);
    auto *closeShared = sessionMenu->addAction(fc::text("Close shared session"));
    sessionActions.append(closeShared);
    connect(closeShared, &QAction::triggered, this, [this] {
        if (auto *pane = currentPane()) {
            const auto id = pane->sessionId();
            rpc_.call("close_session", {{"session_id", id}, {"force", true}},
                      [this, id](const QJsonObject &, const QString &error) {
                          if (!error.isEmpty()) {
                              statusBar()->showMessage(error, 10000);
                              return;
                          }
                          for (int i = 0; i < tabs_->count(); ++i)
                              if (auto *p = qobject_cast<SessionPane *>(tabs_->widget(i));
                                  p && p->sessionId() == id) {
                                  tabs_->removeTab(i);
                                  p->deleteLater();
                                  break;
                              }
                          refresh();
                      });
        }
    });
    command(tools, fc::text("Rules"), &SessionPane::editRules);
    auto *resetSettings = tools->addAction(fc::text("Reset settings..."));
    sessionActions.append(resetSettings);
    connect(resetSettings, &QAction::triggered, this, [this] {
        if (auto *pane = currentPane())
            pane->configureReset();
    });
    auto *library = new QAction(fc::text("Capture library…"), this);
    fileMenu->insertAction(exportAction, library);
    connect(library, &QAction::triggered, this, [this] {
        LogManager dialog(this);
        dialog.exec();
    });
    auto *autoRecord = tools->addAction(fc::text("Auto-save logs on new connections"));
    connect(tools->addAction(fc::text("Recording retention...")), &QAction::triggered, this, [this] {
        bool ok = false;
        const int keep =
            QInputDialog::getInt(this, fc::text("Recording retention"),
                                 fc::text("Older 64 MiB segments to keep for new sessions (0 = keep all):"),
                                 QSettings().value("recording/keepSegments", 0).toInt(), 0, 10000, 1, &ok);
        if (ok)
            QSettings().setValue("recording/keepSegments", keep);
    });
    autoRecord->setCheckable(true);
    autoRecord->setChecked(fc::autoRecordingEnabled());
    connect(autoRecord, &QAction::toggled, this,
            [](bool enabled) { QSettings().setValue("recording/enabled", enabled); });
    connect(tools->addAction(fc::text("Log directory…")), &QAction::triggered, this, [this] {
        const auto directory =
            QFileDialog::getExistingDirectory(this, fc::text("Log directory"), fc::recordingDirectory());
        if (!directory.isEmpty())
            QSettings().setValue("recording/directory", directory);
    });
    // The open dialog persists the same defaults; refresh the menu after it changes them.
    connect(tools, &QMenu::aboutToShow, this, [autoRecord] {
        QSignalBlocker blocker(autoRecord);
        autoRecord->setChecked(fc::autoRecordingEnabled());
    });
    command(sessionMenu, fc::text("Send file…"), &SessionPane::sendFile);
    command(sessionMenu, fc::text("Replay capture…"), &SessionPane::replayLog);
    command(tools, fc::text("Triggers…"), &SessionPane::editTriggers);
    command(tools, fc::text("Decoder options / plugins…"), &SessionPane::editDecoder);
    auto *breakAction = sessionMenu->addAction("BREAK (100 ms)");
    sessionActions.append(breakAction);
    connect(breakAction, &QAction::triggered, this, [this] {
        if (auto *p = currentPane())
            rpc_.call("send_break", {{"session_id", p->sessionId()}, {"duration_ms", 100}});
    });
    view->addAction(refreshAction);
    view->addAction(bar->toggleViewAction());
    view->addAction(macroBar_->toggleViewAction());
    auto *themeMenu = view->addMenu(fc::text("Theme"));
    themeMenu->setObjectName("themeMenu");
    auto *themes = new QActionGroup(themeMenu);
    const QList<QPair<QString, QString>> themeChoices{
        {"system", fc::text("Follow system")}, {"light", fc::text("Light")}, {"dark", fc::text("Dark")}};
    for (const auto &[mode, caption] : themeChoices) {
        auto *action = themeMenu->addAction(caption);
        action->setObjectName("theme_" + mode);
        action->setData(mode);
        action->setCheckable(true);
        action->setChecked(fc::themeMode() == mode);
        themes->addAction(action);
    }
    connect(themes, &QActionGroup::triggered, this,
            [](QAction *action) { fc::setThemeMode(*qApp, action->data().toString()); });
    auto *languageMenu = view->addMenu("Language");
    languageMenu->setObjectName("languageMenu");
    connect(languageMenu->addAction(fc::text("Chinese")), &QAction::triggered, this,
            [this] { changeLanguage("zh"); });
    connect(languageMenu->addAction("English"), &QAction::triggered, this, [this] { changeLanguage("en"); });
    auto *resetLayout = view->addAction(fc::text("Reset dock layout"));
    resetLayout->setObjectName("resetDockLayout");
    connect(resetLayout, &QAction::triggered, this, [this] { restoreState(defaultLayout_); });
    auto *help = menuBar()->addMenu(fc::text("Help"));
    connect(help->addAction(fc::text("MCP setup")), &QAction::triggered, this, [this] {
        auto path = fc::executable("flattencom-mcp");
        QJsonObject config{{"mcpServers", QJsonObject{{"flattencom", QJsonObject{{"command", path}}}}}};
        QMessageBox::information(this, "MCP",
                                 QString::fromUtf8(QJsonDocument(config).toJson()) + "\n" +
                                     fc::text("MCP can access the current serial sessions."));
    });
    auto *about = help->addAction(fc::text("About"));
    about->setObjectName("aboutAction");
    connect(about, &QAction::triggered, this, [this] {
        if (auto *dialog = findChild<QDialog *>("aboutDialog")) {
            dialog->show();
            dialog->raise();
            dialog->activateWindow();
        } else {
            (new AboutDialog(this))->show();
        }
    });
    tabs_ = new QTabWidget;
    auto updateTools = [this, config, exportAction, insertMacro, saveMacro, deleteMacro, sessionActions,
                        detach, pauseAction] {
        const bool active = currentPane() != nullptr;
        for (auto *action : sessionActions)
            action->setEnabled(active);
        detach->setEnabled(active);
        const QSignalBlocker blocker(pauseAction);
        pauseAction->setChecked(active && currentPane()->isPaused());
        pauseAction->setText(active && currentPane()->isPaused() ? fc::text("Resume live")
                                                                 : fc::text("Pause display"));
        config->setEnabled(active);
        exportAction->setEnabled(active);
        saveMacro->setEnabled(active);
        macroList_->setEnabled(active);
        insertMacro->setEnabled(active && macroList_->count() > 0);
        deleteMacro->setEnabled(macroList_->count() > 0);
    };
    connect(tabs_, &QTabWidget::currentChanged, this, updateTools);
    connect(macroList_, &QComboBox::currentIndexChanged, this, updateTools);
    updateTools();
    connect(sessionMenu, &QMenu::aboutToShow, this, updateTools);
    connect(this, &MainWindow::paneAdded, this, [this, updateTools](SessionPane *pane) {
        connect(pane, &SessionPane::displayPausedChanged, this, updateTools);
        updateTools();
    });
    connect(tabs_, &QTabWidget::currentChanged, this, [this] {
        if (!details_)
            return;
        details_->clear();
        dtr_->setTristate(true);
        dtr_->setCheckState(Qt::PartiallyChecked);
        rts_->setTristate(true);
        rts_->setCheckState(Qt::PartiallyChecked);
        for (auto *lamp : signalLights_) {
            lamp->setStyleSheet("background:#8798ae;border-radius:5px;");
            lamp->setToolTip(fc::text("Unknown"));
        }
        if (auto *pane = currentPane())
            updateStats(pane->session().value("stats").toObject());
        else
            stats_->setText(fc::text("Select a session"));
    });
    tabs_->setTabsClosable(true);
    tabs_->setMovable(true);
    tabs_->setDocumentMode(true);
    tabs_->setObjectName("sessionDocuments");
    tabs_->setUsesScrollButtons(true);
    auto *next = windowMenu->addAction(fc::text("Next session"));
    next->setShortcut(QKeySequence("Ctrl+Tab"));
    auto *previous = windowMenu->addAction(fc::text("Previous session"));
    previous->setShortcut(QKeySequence("Ctrl+Shift+Tab"));
    connect(next, &QAction::triggered, this, [this] {
        if (tabs_->count() > 0)
            tabs_->setCurrentIndex((tabs_->currentIndex() + 1) % tabs_->count());
    });
    connect(previous, &QAction::triggered, this, [this] {
        if (tabs_->count() > 0)
            tabs_->setCurrentIndex((tabs_->currentIndex() + tabs_->count() - 1) % tabs_->count());
    });
    windowMenu->addSeparator();
    connect(windowMenu, &QMenu::aboutToShow, this, [this, windowMenu, next, previous] {
        for (auto *action : windowMenu->actions())
            if (action->property("documentEntry").toBool()) {
                windowMenu->removeAction(action);
                delete action;
            }
        next->setEnabled(tabs_->count() > 1);
        previous->setEnabled(tabs_->count() > 1);
        for (int i = 0; i < tabs_->count(); ++i) {
            auto *action = windowMenu->addAction(tabs_->tabText(i));
            action->setProperty("documentEntry", true);
            action->setCheckable(true);
            action->setChecked(i == tabs_->currentIndex());
            QPointer<QWidget> target = tabs_->widget(i);
            connect(action, &QAction::triggered, this, [this, target] {
                if (target)
                    tabs_->setCurrentWidget(target);
            });
        }
    });
    setCentralWidget(tabs_);
    connect(tabs_, &QTabWidget::tabCloseRequested, this, &MainWindow::closeSession);
    auto *welcome = new QWidget;
    auto *welcomeLayout = new QVBoxLayout(welcome);
    welcomeLayout->addStretch();
    auto *title = new QLabel(fc::text("Serial monitor"));
    title->setAlignment(Qt::AlignCenter);
    title->setStyleSheet("font-size:27px; font-weight:600; padding:20px;");
    auto *description = new QLabel(fc::text("Open a serial port to start."));
    description->setAlignment(Qt::AlignCenter);
    auto *start = new QPushButton(fc::text("Open port"));
    start->setObjectName("primary");
    start->setMaximumWidth(350);
    connect(start, &QPushButton::clicked, this, [this] { openPort({}); });
    welcomeLayout->addWidget(title);
    welcomeLayout->addWidget(description);
    welcomeLayout->addSpacing(25);
    welcomeLayout->addWidget(start, 0, Qt::AlignHCenter);
    welcomeLayout->addStretch();
    tabs_->addTab(welcome, fc::text("Welcome"));
    auto *portDock = new QDockWidget(fc::text("PORTS & SESSIONS"), this);
    portDock->setObjectName("ports");
    ports_ = new QTreeWidget;
    ports_->setObjectName("portTree");
    ports_->setHeaderLabels({fc::text("Port / session"), fc::text("State")});
    ports_->setMinimumWidth(260);
    ports_->setTextElideMode(Qt::ElideNone);
    ports_->setHorizontalScrollMode(QAbstractItemView::ScrollPerPixel);
    ports_->header()->setStretchLastSection(false);
    ports_->header()->setSectionResizeMode(QHeaderView::ResizeToContents);
    ports_->header()->setMinimumSectionSize(100);
    ports_->setIndentation(14);
    portDock->setWidget(ports_);
    addDockWidget(Qt::LeftDockWidgetArea, portDock);
    view->addAction(portDock->toggleViewAction());
    connect(ports_, &QTreeWidget::itemDoubleClicked, this, [this](QTreeWidgetItem *item, int) {
        if (item->data(0, Qt::UserRole + 1).isValid())
            addSession(item->data(0, Qt::UserRole + 1).toJsonObject());
        else if (!item->data(0, Qt::UserRole).toString().isEmpty())
            openPort(item->data(0, Qt::UserRole).toString());
    });
    auto *inspectDock = new QDockWidget(fc::text("INSPECTOR"), this);
    inspectDock->setObjectName("inspector");
    auto *inspector = new QWidget;
    auto *inspection = new QVBoxLayout(inspector);
    inspection->setContentsMargins(6, 4, 6, 4);
    inspection->setSpacing(4);
    stats_ = new QLabel(fc::text("Select a session"));
    stats_->setWordWrap(true);
    auto *statistics = new QGroupBox(fc::text("Traffic"));
    auto *statsLayout = new QVBoxLayout(statistics);
    statsLayout->addWidget(stats_);
    inspection->addWidget(statistics);
    chart_ = new RateChart;
    auto *rates = new QGroupBox(fc::text("Transfer rate"));
    auto *rateLayout = new QVBoxLayout(rates);
    rateLayout->addWidget(chart_);
    inspection->addWidget(rates);
    auto *signalsBox = new QGroupBox(fc::text("Control lines"));
    auto *signalsLayout = new QVBoxLayout(signalsBox);
    auto *signalRow = new QHBoxLayout;
    dtr_ = new QCheckBox("DTR");
    rts_ = new QCheckBox("RTS");
    signalRow->addWidget(dtr_);
    signalRow->addWidget(rts_);
    signalsLayout->addLayout(signalRow);
    auto *inputs = new QHBoxLayout;
    for (const auto &name : {"cts", "dsr", "dcd", "ri"}) {
        auto *lamp = new QLabel;
        lamp->setFixedSize(10, 10);
        lamp->setObjectName(QString("signal_%1").arg(name));
        lamp->setStyleSheet("background:#8798ae;border-radius:5px;");
        lamp->setToolTip(fc::text("Unknown"));
        signalLights_.insert(name, lamp);
        inputs->addWidget(lamp);
        inputs->addWidget(new QLabel(QString(name).toUpper()));
    }
    signalsLayout->addLayout(inputs);
    inspection->addWidget(signalsBox);
    auto applySignals = [this](const QString &name, bool checked) {
        if (auto *p = currentPane())
            rpc_.call("set_signals", {{"session_id", p->sessionId()}, {name, checked}},
                      [this](const QJsonObject &, const QString &e) {
                          if (!e.isEmpty())
                              statusBar()->showMessage(e, 10000);
                      });
    };
    connect(dtr_, &QCheckBox::clicked, this, [applySignals](bool checked) { applySignals("dtr", checked); });
    connect(rts_, &QCheckBox::clicked, this, [applySignals](bool checked) { applySignals("rts", checked); });
    details_ = new InspectorDetails;
    inspection->addWidget(details_, 1);
    inspectDock->setWidget(inspector);
    addDockWidget(Qt::RightDockWidgetArea, inspectDock);
    view->addAction(inspectDock->toggleViewAction());
    auto *eventDock = new QDockWidget(fc::text("EVENT LOG"), this);
    eventDock->setObjectName("events");
    events_ = new QTextEdit;
    events_->setReadOnly(true);
    events_->document()->setMaximumBlockCount(1000);
    events_->setMaximumHeight(145);
    eventDock->setWidget(events_);
    addDockWidget(Qt::BottomDockWidgetArea, eventDock);
    eventDock->hide();
    view->addAction(eventDock->toggleViewAction());
    connection_ = new QLabel(fc::text("Connecting to background service..."));
    statusBar()->addPermanentWidget(connection_);
    connect(&rpc_, &RpcClient::connected, this, [this] {
        connection_->setText(fc::text("Background service connected"));
        refresh();
        emit ready();
    });
    connect(&rpc_, &RpcClient::disconnected, this,
            [this] { connection_->setText(fc::text("Reconnecting...")); });
    connect(&rpc_, &RpcClient::error, this, [this](const QString &error) {
        events_->append(error.toHtmlEscaped());
        statusBar()->showMessage(error, 15000);
    });
    connect(&rpc_, &RpcClient::notification, this, [this](const QString &method, const QJsonObject &params) {
        if (method == "ports_changed" || method == "session_state")
            refresh();
        if (method != "frames" && method != "stats")
            events_->append(
                QDateTime::currentDateTime().toString("HH:mm:ss ") + method + " " +
                QString::fromUtf8(QJsonDocument(params).toJson(QJsonDocument::Compact)).toHtmlEscaped());
    });
    resizeDocks({portDock, inspectDock}, {290, 290}, Qt::Horizontal);
    defaultLayout_ = saveState();
    restoreGeometry(QSettings().value("geometry").toByteArray());
    restoreState(QSettings().value("layout").toByteArray());
    rpc_.start();
}
SessionPane *MainWindow::currentPane() const { return qobject_cast<SessionPane *>(tabs_->currentWidget()); }
void MainWindow::detachCurrentView() {
    if (auto *pane = currentPane()) {
        tabs_->removeTab(tabs_->indexOf(pane));
        pane->deleteLater();
    }
}
void MainWindow::refreshMacros() {
    const auto current = macroList_->currentData().toString();
    macroList_->clear();
    for (const auto &command : QSettings().value("macros").toStringList()) {
        const auto label = command.simplified();
        macroList_->addItem(label, command);
        macroList_->setItemData(macroList_->count() - 1, command, Qt::ToolTipRole);
    }
    const int selected = macroList_->findData(current);
    if (selected >= 0)
        macroList_->setCurrentIndex(selected);
}
void MainWindow::refresh() {
    if (!rpc_.ready() || refreshing_)
        return;
    refreshing_ = true;
    rpc_.call("list_ports", {}, [this](const QJsonObject &result, const QString &error) {
        if (!error.isEmpty()) {
            refreshing_ = false;
            return;
        }
        ports_->clear();
        auto *physical = new QTreeWidgetItem(ports_, {fc::text("Devices")});
        physical->setFirstColumnSpanned(true);
        physical->setExpanded(true);
        for (const auto &value : result.value("ports").toArray()) {
            const auto p = value.toObject();
            auto *item = new QTreeWidgetItem(
                physical, {p.value("path").toString(),
                           p.value("busy").toBool() ? fc::text("Connected") : fc::text("Not verified")});
            item->setData(0, Qt::UserRole, p.value("path"));
            item->setData(0, Qt::UserRole + 2, p.value("sessions").toArray());
            item->setToolTip(0, p.value("path").toString() + "\n" + p.value("friendly_name").toString());
        }
        rpc_.call("list_sessions", {}, [this](const QJsonObject &result, const QString &error) {
            refreshing_ = false;
            if (!error.isEmpty())
                return;
            auto *group = new QTreeWidgetItem(ports_, {fc::text("Shared sessions")});
            group->setFirstColumnSpanned(true);
            group->setExpanded(true);
            for (const auto &value : result.value("sessions").toArray()) {
                const auto s = value.toObject();
                auto *item = new QTreeWidgetItem(
                    group, {s.value("label").toString(s.value("path").toString()),
                            fc::valueLabel(s.value("state").toObject().value("state").toString())});
                item->setData(0, Qt::UserRole + 1, s);
                item->setToolTip(0, s.value("label").toString() + "\n" + s.value("path").toString() + "\n" +
                                        s.value("session_id").toString());
                const auto state = s.value("state").toObject().value("state").toString();
                if (state == "connected" || state == "reconnecting") {
                    // Associate physical port entries with their current session so
                    // double-click joins immediately instead of reopening settings.
                    for (int root = 0; root < ports_->topLevelItemCount(); ++root) {
                        auto *parent = ports_->topLevelItem(root);
                        for (int child = 0; child < parent->childCount(); ++child) {
                            auto *portItem = parent->child(child);
                            if (portItem->data(0, Qt::UserRole).toString() == s.value("path").toString() ||
                                portItem->data(0, Qt::UserRole + 2)
                                    .toJsonArray()
                                    .contains(s.value("session_id"))) {
                                portItem->setData(0, Qt::UserRole + 1, s);
                                portItem->setText(1, fc::text("Connected"));
                            }
                        }
                    }
                }
                for (int i = 0; i < tabs_->count(); ++i)
                    if (auto *pane = qobject_cast<SessionPane *>(tabs_->widget(i));
                        pane && pane->sessionId() == s.value("session_id").toString())
                        pane->updateSession(s);
            }
        });
    });
}
void MainWindow::openVirtual() {
    if (rpc_.ready()) {
        QJsonObject config{{"path", "virtual://echo"}, {"label", fc::text("Virtual echo")}};
        fc::applyAutoRecording(config, fc::autoRecordingEnabled(), fc::recordingDirectory());
        rpc_.call("open_session", config, [this](const QJsonObject &r, const QString &e) {
            if (e.isEmpty()) {
                openSessionResult(r);
            } else
                statusBar()->showMessage(e, 10000);
        });
    }
}
void MainWindow::openPort(const QString &path) {
    if (!path.isEmpty()) {
        for (int i = 0; i < tabs_->count(); ++i) {
            if (auto *pane = qobject_cast<SessionPane *>(tabs_->widget(i));
                pane && pane->session().value("path").toString() == path) {
                const auto state = pane->session().value("state").toObject().value("state").toString();
                if (state == "connected" || state == "reconnecting") {
                    tabs_->setCurrentIndex(i);
                    return;
                }
            }
        }
    }
    QDialog dialog(this);
    dialog.setWindowTitle(fc::text("Open serial port"));
    dialog.setMinimumWidth(430);
    auto *form = new QFormLayout(&dialog);
    auto *port = new QLineEdit(path.isEmpty() ? QSettings().value("lastPort").toString() : path);
    if (path.isEmpty() && port->text().startsWith("virtual://"))
        port->clear();
    QString key = fc::profileKey({{"path", port->text()}});
    QJsonObject profile = fc::loadProfile(key);
    form->addRow(fc::text("Port"), port);
    auto *baud = new QComboBox;
    baud->setEditable(true);
    baud->addItems(
        {"9600", "19200", "38400", "57600", "115200", "230400", "460800", "921600", "1000000", "3000000"});
    baud->setCurrentText(QSettings().value("baud", 115200).toString());
    form->addRow(fc::text("Baud"), baud);
    auto *bits = new QComboBox;
    fc::addValues(bits, {"eight", "seven", "six", "five"});
    form->addRow(fc::text("Data bits"), bits);
    auto *parity = new QComboBox;
    fc::addValues(parity, {"none", "even", "odd", "mark", "space"});
    form->addRow(fc::text("Parity"), parity);
    auto *stop = new QComboBox;
    fc::addValues(stop, {"one", "one_point_five", "two"});
    stop->setToolTip(fc::text("1.5 stop bits require 5 data bits; 2 stop bits require 6–8 data bits."));
    form->addRow(fc::text("Stop bits"), stop);
    auto *flow = new QComboBox;
    fc::addValues(flow, {"none", "hardware", "software"});
    form->addRow(fc::text("Flow control"), flow);
    auto *label = new QLineEdit;
    form->addRow(fc::text("Label"), label);
    auto *record = new QCheckBox(fc::text("Automatically save logs"));
    record->setToolTip(fc::text("Save traffic, commands and markers in one text file."));
    record->setChecked(fc::autoRecordingEnabled());
    form->addRow(record);
    auto *directory = new QLineEdit(fc::recordingDirectory());
    auto *browse = new QPushButton(fc::text("Browse…"));
    auto *directoryRow = new QHBoxLayout;
    directoryRow->addWidget(directory);
    directoryRow->addWidget(browse);
    form->addRow(fc::text("Save directory"), directoryRow);
    auto *identityHint = new QLabel;
    identityHint->setWordWrap(true);
    form->addRow(identityHint);
    auto *decoder = new QComboBox;
    fc::addValues(decoder,
                  {"none", "ascii_lines", "utf8_lossy", "json_lines", "modbus_rtu", "nmea0183", "hex_dump"});
    form->addRow(fc::text("Default decoder"), decoder);
    auto *ending = new QComboBox;
    fc::addValues(ending, {"CRLF", "LF", "CR", "None"});
    form->addRow(fc::text("Send line ending"), ending);
    QHash<QObject *, quint64> edits;
    bool applyingProfile = false;
    auto edited = [&](QObject *control) {
        if (!applyingProfile)
            ++edits[control];
    };
    for (auto *combo : {baud, bits, parity, stop, flow, decoder, ending}) {
        connect(combo, &QComboBox::currentIndexChanged, &dialog, [&, combo] { edited(combo); });
        if (combo->isEditable())
            connect(combo, &QComboBox::editTextChanged, &dialog, [&, combo] { edited(combo); });
    }
    for (auto *input : {label, directory})
        connect(input, &QLineEdit::textChanged, &dialog, [&, input] { edited(input); });
    connect(record, &QCheckBox::toggled, &dialog, [&] { edited(record); });
    auto applyProfile = [&](const QJsonObject &metadata,
                            const QHash<QObject *, quint64> *snapshot = nullptr) {
        key = fc::profileKey(metadata);
        profile = fc::loadProfile(key);
        identityHint->setText(metadata.value("serial").toString().isEmpty()
                                  ? fc::text("Settings saved by port.")
                                  : fc::text("Device serial: ") + metadata.value("serial").toString());
        auto untouched = [&](QObject *control) {
            return !snapshot || edits.value(control) == snapshot->value(control);
        };
        applyingProfile = true;
        if (untouched(baud))
            baud->setCurrentText(QString::number(profile.value("baud").toInt(115200)));
        if (untouched(bits))
            fc::selectValue(bits, profile.value("data_bits").toString("eight"));
        if (untouched(parity))
            fc::selectValue(parity, profile.value("parity").toString("none"));
        if (untouched(stop))
            fc::selectValue(stop, profile.value("stop_bits").toString("one"));
        if (untouched(flow))
            fc::selectValue(flow, profile.value("flow_control").toString("none"));
        if (untouched(label))
            label->setText(profile.value("label").toString());
        if (untouched(directory))
            directory->setText(profile.value("directory").toString(fc::recordingDirectory()));
        if (untouched(record))
            record->setChecked(profile.value("record").toBool(fc::autoRecordingEnabled()));
        if (untouched(decoder))
            fc::selectValue(decoder, profile.value("decoder").toString("none"));
        if (untouched(ending))
            fc::selectValue(ending, profile.value("newline").toString("CRLF"));
        applyingProfile = false;
    };
    QString resolvedPath;
    quint64 profileGeneration = 0;
    connect(port, &QLineEdit::textChanged, &dialog, [&] {
        ++profileGeneration;
        resolvedPath.clear();
    });
    auto resolveProfile = [&] {
        const auto path = port->text();
        if (resolvedPath == path)
            return;
        resolvedPath = path;
        applyProfile({{"path", path}});
        const auto generation = ++profileGeneration;
        const auto snapshot = edits;
        QPointer<QDialog> alive(&dialog);
        rpc_.call("get_port_info", {{"path", path}},
                  [&, alive, path, generation, snapshot](const QJsonObject &r, const QString &e) {
                      if (!alive || generation != profileGeneration || port->text() != path)
                          return;
                      if (e.isEmpty())
                          applyProfile(r.value("port").toObject(), &snapshot);
                  });
    };
    connect(port, &QLineEdit::editingFinished, &dialog, resolveProfile);
    resolveProfile();
    auto *reuseHint = new QLabel(fc::text("Connected ports use the existing session."));
    reuseHint->setWordWrap(true);
    form->addRow(reuseHint);
    directory->setEnabled(record->isChecked());
    browse->setEnabled(record->isChecked());
    connect(record, &QCheckBox::toggled, directory, &QLineEdit::setEnabled);
    connect(record, &QCheckBox::toggled, browse, &QPushButton::setEnabled);
    connect(browse, &QPushButton::clicked, &dialog, [&dialog, directory] {
        const auto selected =
            QFileDialog::getExistingDirectory(&dialog, fc::text("Log directory"), directory->text());
        if (!selected.isEmpty())
            directory->setText(selected);
    });
    auto *buttons = new QDialogButtonBox(QDialogButtonBox::Open | QDialogButtonBox::Cancel);
    form->addRow(buttons);
    connect(buttons, &QDialogButtonBox::accepted, &dialog, &QDialog::accept);
    connect(buttons, &QDialogButtonBox::rejected, &dialog, &QDialog::reject);
    if (dialog.exec() != QDialog::Accepted)
        return;
    bool ok = false;
    const auto rate = baud->currentText().toUInt(&ok);
    if (!ok || rate == 0) {
        statusBar()->showMessage(fc::text("Baud must be a positive integer"));
        return;
    }
    QJsonObject config{{"path", port->text()},
                       {"baud", static_cast<qint64>(rate)},
                       {"data_bits", bits->currentData().toString()},
                       {"parity", parity->currentData().toString()},
                       {"stop_bits", stop->currentData().toString()},
                       {"flow_control", flow->currentData().toString()},
                       {"exclusive", true}};
    if (!label->text().isEmpty())
        config["label"] = label->text();
    if (record->isChecked() && directory->text().trimmed().isEmpty()) {
        QMessageBox::warning(this, "flattencom", fc::text("Choose a log directory"));
        return;
    }
    QSettings().setValue("recording/enabled", record->isChecked());
    if (!directory->text().trimmed().isEmpty())
        QSettings().setValue("recording/directory", QDir(directory->text()).absolutePath());
    fc::applyAutoRecording(config, record->isChecked(), directory->text());
    QSettings().setValue("lastPort", port->text());
    QSettings().setValue("baud", rate);
    QJsonObject saved = profile;
    for (auto i = config.begin(); i != config.end(); ++i)
        saved[i.key()] = i.value();
    saved.remove("record_to");
    saved.remove("record_rx_to");
    saved["directory"] = directory->text();
    saved["record"] = record->isChecked();
    saved["decoder"] = decoder->currentData().toString();
    saved["newline"] = ending->currentData().toString();
    rpc_.call("open_session", config, [this, key, saved](const QJsonObject &r, const QString &e) {
        if (e.isEmpty()) {
            auto result = r;
            auto session = result.value("session").toObject();
            session["profile_key"] = key;
            session["send_newline"] = saved.value("newline");
            result["session"] = session;
            if (!r.value("reused").toBool()) {
                fc::saveProfile(key, saved);
                const auto decoder = saved.value("decoder").toString();
                if (decoder != "none")
                    rpc_.call("set_decoder",
                              {{"session_id", session.value("session_id")},
                               {"spec", QJsonObject{{"name", decoder}, {"options", QJsonObject{}}}}});
            }
            openSessionResult(result);
        } else
            QMessageBox::warning(this, "flattencom", e);
    });
}
void MainWindow::openSessionResult(const QJsonObject &result) {
    const auto session = result.value("session").toObject();
    addSession(session);
    if (result.value("reused").toBool()) {
        const auto config = session.value("config").toObject();
        statusBar()->showMessage(fc::text("Connected: ") + config.value("path").toString() + " @ " +
                                     QString::number(config.value("baud").toInt()),
                                 10000);
    }
    refresh();
}
void MainWindow::addSession(const QJsonObject &session) {
    for (int i = 0; i < tabs_->count(); ++i)
        if (auto *p = qobject_cast<SessionPane *>(tabs_->widget(i));
            p && p->sessionId() == session.value("session_id").toString()) {
            p->updateSession(session);
            tabs_->setCurrentIndex(i);
            return;
        }
    auto *pane = new SessionPane(&rpc_, session);
    connect(pane, &SessionPane::macrosChanged, this, &MainWindow::refreshMacros);
    const auto index = tabs_->addTab(pane, pane->label());
    tabs_->setCurrentIndex(index);
    connect(pane, &SessionPane::message, this,
            [this](const QString &m) { statusBar()->showMessage(m, 10000); });
    connect(pane, &SessionPane::frameSelected, this, &MainWindow::showFrame);
    connect(pane, &SessionPane::statsChanged, this, [this, pane](const QJsonObject &s) {
        if (currentPane() == pane)
            updateStats(s);
    });
    connect(pane, &SessionPane::signalsChanged, this, [this, pane](const QJsonObject &p) {
        if (currentPane() != pane || p.isEmpty())
            return;
        dtr_->setTristate(!p.value("dtr_known").toBool());
        rts_->setTristate(!p.value("rts_known").toBool());
        dtr_->setCheckState(!p.value("dtr_known").toBool() ? Qt::PartiallyChecked
                            : p.value("dtr").toBool()      ? Qt::Checked
                                                           : Qt::Unchecked);
        rts_->setCheckState(!p.value("rts_known").toBool() ? Qt::PartiallyChecked
                            : p.value("rts").toBool()      ? Qt::Checked
                                                           : Qt::Unchecked);
        for (auto it = signalLights_.begin(); it != signalLights_.end(); ++it) {
            const bool known = p.contains(it.key()) && !p.value(it.key()).isNull();
            const bool active = p.value(it.key()).toBool();
            it.value()->setStyleSheet(QString("background:%1;border-radius:5px;")
                                          .arg(!known   ? "#8798ae"
                                               : active ? "#53c999"
                                                        : "#445263"));
            it.value()->setToolTip(!known   ? fc::text("Unknown")
                                   : active ? fc::text("Asserted")
                                            : fc::text("Deasserted"));
        }
    });
    emit paneAdded(pane);
}
void MainWindow::showFrame(const QJsonObject &frame) { details_->setFrame(frame); }
void MainWindow::updateStats(const QJsonObject &stats) {
    stats_->setText(QString("RX  %1 B    %2 B/s\nTX  %3 B    %4 B/s\n%5: %6    %7: %8")
                        .arg(stats.value("rx_bytes").toInteger())
                        .arg(stats.value("rx_rate_bps").toDouble(), 0, 'f', 0)
                        .arg(stats.value("tx_bytes").toInteger())
                        .arg(stats.value("tx_rate_bps").toDouble(), 0, 'f', 0)
                        .arg(fc::text("Reconnects"))
                        .arg(stats.value("reconnects").toInteger())
                        .arg(fc::text("Evicted"))
                        .arg(stats.value("dropped_rx").toInteger()));
    chart_->add(stats.value("rx_rate_bps").toDouble(), stats.value("tx_rate_bps").toDouble());
}
void MainWindow::closeSession(int index) {
    auto *pane = qobject_cast<SessionPane *>(tabs_->widget(index));
    if (!pane) {
        auto *w = tabs_->widget(index);
        tabs_->removeTab(index);
        w->deleteLater();
        return;
    }
    // Closing a view detaches; the explicit menu below controls shared session lifetime.
    QMenu menu;
    auto *detach = menu.addAction(fc::text("Detach view"));
    auto *close = menu.addAction(fc::text("Close shared session"));
    const auto selected = menu.exec(QCursor::pos());
    if (!selected)
        return;
    if (selected == close)
        rpc_.call("close_session", {{"session_id", pane->sessionId()}, {"force", true}});
    if (selected == detach || selected == close) {
        tabs_->removeTab(index);
        pane->deleteLater();
        refresh();
    }
}
void MainWindow::changeLanguage(const QString &language) {
    const auto previous = fc::language();
    if (previous == (language.startsWith("zh") ? "zh" : "en"))
        return;
    setUpdatesEnabled(false);
    fc::setLanguage(language);
    fc::retranslateUi(this, previous);
    setWindowTitle("flattencom");
    for (int i = 0; i < tabs_->count(); ++i)
        if (auto *pane = qobject_cast<SessionPane *>(tabs_->widget(i)))
            pane->retranslate();
    details_->retranslate();
    for (int i = 0; i < ports_->topLevelItemCount(); ++i) {
        auto *group = ports_->topLevelItem(i);
        group->setText(0, i == 0 ? fc::text("Devices") : fc::text("Shared sessions"));
        for (int j = 0; j < group->childCount(); ++j) {
            auto *item = group->child(j);
            const auto session = item->data(0, Qt::UserRole + 1).toJsonObject();
            item->setText(
                1, session.isEmpty()
                       ? (item->data(0, Qt::UserRole + 2).toJsonArray().isEmpty() ? fc::text("Not verified")
                                                                                  : fc::text("Connected"))
                       : fc::valueLabel(session.value("state").toObject().value("state").toString()));
        }
    }
    connection_->setText(rpc_.ready() ? fc::text("Background service connected")
                                      : fc::text("Reconnecting..."));
    if (auto *pane = currentPane())
        updateStats(pane->session().value("stats").toObject());
    setUpdatesEnabled(true);
    update();
}
void MainWindow::closeEvent(QCloseEvent *event) {
    QSettings().setValue("geometry", saveGeometry());
    QSettings().setValue("layout", saveState());
    while (tabs_->count()) {
        auto *pane = tabs_->widget(0);
        tabs_->removeTab(0);
        delete pane;
    }
    rpc_.stop();
    event->accept();
}
