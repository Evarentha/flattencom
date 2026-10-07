/*
 * flattencom - Qt Application Entry Point
 *
 * Initializes application settings, localization and the main window with isolated smoke-test support.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

#include "MainWindow.h"
#include "Recording.h"
#include "Style.h"
#include <QAction>
#include <QApplication>
#include <QCommandLineParser>
#include <QDir>
#include <QFile>
#include <QHeaderView>
#include <QIcon>
#include <QJsonDocument>
#include <QLoggingCategory>
#include <QMenu>
#include <QPlainTextEdit>
#include <QSettings>
#include <QTemporaryDir>
#include <QTimer>
#include <QToolBar>

int main(int argc, char **argv) {
    // Qt reports every rejected complex-script fallback font at info level.
    // Keep warnings/errors visible; QT_LOGGING_RULES can re-enable diagnostics.
    QLoggingCategory::setFilterRules("qt.text.font.db.info=false");
    QApplication app(argc, argv);
    QCoreApplication::setOrganizationName("flattencom");
    QCoreApplication::setApplicationName("flattencom-gui");
    QCoreApplication::setApplicationVersion(QLatin1String(FLATTENCOM_VERSION));
    QCommandLineParser parser;
    parser.addHelpOption();
    parser.addVersionOption();
    parser.addOption({"smoke", "Exercise daemon connection, open, send, receive, and close."});
    parser.addOption({"screenshot", "Save the smoke-test window as PNG.", "path"});
    parser.addOption({"lang", "Language: en or zh-CN (default: en).", "language"});
    parser.process(app);
    QTemporaryDir smokeSettings;
    if (parser.isSet("smoke")) {
        QSettings::setDefaultFormat(QSettings::IniFormat);
        QSettings::setPath(QSettings::IniFormat, QSettings::UserScope, smokeSettings.path());
        QSettings().setValue("recording/enabled", true);
        QSettings().setValue("recording/directory", fc::stateDirectory() + "/captures");
    }
    if (parser.isSet("lang")) {
        const auto value = parser.value("lang");
        if (value != "en" && value != "zh" && value != "zh-CN") {
            qCritical("Invalid language. Use en or zh-CN.");
            return 1;
        }
        fc::setLanguage(value);
    }
    fc::loadTranslations();
    QIcon applicationIcon;
    for (int size : {16, 24, 32, 48, 64, 128, 256, 512, 1024})
        applicationIcon.addFile(QString(":/flattencom/branding/icon-%1.png").arg(size), QSize(size, size));
    app.setWindowIcon(applicationIcon);
    app.setDesktopFileName("flattencom");
    fc::initializeTheme(app);
    MainWindow window;
    window.show();
    if (parser.isSet("smoke")) {
        auto *tools = window.findChild<QToolBar *>("serialTools");
        if (window.windowTitle() != "flattencom")
            return 19;
        if (window.windowIcon().isNull() || window.windowIcon().pixmap(32, 32).isNull())
            return 20;
        for (const auto &mode : QStringList{"system", "light", "dark"}) {
            auto *action = window.findChild<QAction *>("theme_" + mode);
            if (!action || !action->isCheckable() || action->isChecked() != (fc::themeMode() == mode))
                return 18;
        }
        auto *resetLayout = window.findChild<QAction *>("resetDockLayout");
        const auto expectedReset = fc::language() == "zh"
                                       ? QString::fromUtf8("\u91cd\u7f6e\u505c\u9760\u5e03\u5c40")
                                       : QString("Reset dock layout");
        if (!resetLayout || resetLayout->text() != expectedReset) {
            fprintf(stderr, "Reset layout translation: %s; expected: %s\n",
                    qPrintable(resetLayout ? resetLayout->text() : QString("missing")),
                    qPrintable(expectedReset));
            return 14;
        }
        auto *ports = window.findChild<QTreeWidget *>("portTree");
        if (!tools || tools->actions().size() != 4 || !ports || ports->textElideMode() != Qt::ElideNone ||
            ports->header()->sectionResizeMode(0) != QHeaderView::ResizeToContents)
            return 12;
        for (auto *action : tools->actions())
            if (action->icon().isNull())
                return 13;
        QTimer::singleShot(12000, &app, [&app] { app.exit(2); });
        QObject::connect(&window, &MainWindow::ready, &window, &MainWindow::openVirtual);
        QObject::connect(&window, &MainWindow::paneAdded, &window, [&, resetLayout](SessionPane *pane) {
            pane->send("flattencom GUI smoke");
            QObject::connect(pane, &SessionPane::receivedFrames, &window, [&, pane, resetLayout](int) {
                bool found = false;
                for (int i = 0; i < pane->framesModel()->rowCount(); ++i) {
                    const auto f = pane->framesModel()->frame(i);
                    if (f.value("dir") == "rx" && f.value("text").toString().contains("GUI smoke"))
                        found = true;
                }
                if (!found)
                    return;
                // Verify the receive log renders actual line endings, not chunk symbols.
                auto *stream = pane->findChild<QPlainTextEdit *>("receiveTextStream");
                if (!stream || stream->toPlainText() != "flattencom GUI smoke\n") {
                    app.exit(4);
                    return;
                }
                pane->setPaused(true);
                // Menu and in-pane display controls must target the same active session.
                auto *sessionPause = window.findChild<QAction *>("sessionPauseAction");
                if (!sessionPause || !sessionPause->isEnabled() || !sessionPause->isChecked()) {
                    app.exit(9);
                    return;
                }
                sessionPause->trigger();
                if (pane->isPaused()) {
                    app.exit(10);
                    return;
                }
                sessionPause->trigger();
                if (!pane->isPaused()) {
                    app.exit(11);
                    return;
                }
                const auto language = fc::language();
                const auto windowId = window.winId();
                const auto label = pane->label();
                stream->selectAll();
                const auto selection = stream->textCursor().selectedText();
                for (const auto &target : QStringList{language == "zh" ? "en" : "zh", language}) {
                    const auto menuLabel = target == "en" ? QString("English") : fc::text("Chinese");
                    QAction *switchAction = nullptr;
                    for (auto *action : window.findChildren<QAction *>())
                        if (action->text() == menuLabel) {
                            switchAction = action;
                            break;
                        }
                    if (!switchAction) {
                        app.exit(15);
                        return;
                    }
                    switchAction->trigger();
                    if (window.winId() != windowId || window.currentPane() != pane ||
                        !window.rpc()->ready() || !pane->isPaused() || pane->label() != label ||
                        stream->textCursor().selectedText() != selection) {
                        app.exit(16);
                        return;
                    }
                    if (resetLayout->text() != fc::text("Reset dock layout") ||
                        window.windowTitle() != "flattencom") {
                        app.exit(17);
                        return;
                    }
                }
                QTimer::singleShot(1100, &window, [&, pane] {
                    // Saving is performed by the daemon, even after display has paused.
                    const auto config = pane->session().value("config").toObject();
                    QFile capture(config.value("record_to").toString());
                    if (!capture.open(QIODevice::ReadOnly)) {
                        app.exit(5);
                        return;
                    }
                    const auto text = capture.readAll();
                    if (!text.contains(" RX #") || !text.contains(" TX #") ||
                        !text.contains("flattencom GUI smoke")) {
                        app.exit(6);
                        return;
                    }
                    if (QDir(fc::stateDirectory() + "/captures").entryList(QDir::Files).size() != 1) {
                        app.exit(8);
                        return;
                    }
                    if (parser.isSet("screenshot"))
                        window.grab().save(parser.value("screenshot"));
                    // A second connection request must focus the existing tab,
                    // preserve recording files, and never emit paneAdded again.
                    const auto id = pane->sessionId();
                    window.openVirtual();
                    QTimer::singleShot(400, &window, [&, id, config] {
                        auto *current = window.currentPane();
                        if (!current || current->sessionId() != id ||
                            current->session().value("config").toObject() != config) {
                            app.exit(7);
                            return;
                        }
                        window.rpc()->call("close_session", {{"session_id", id}},
                                           [&](const QJsonObject &, const QString &error) {
                                               app.exit(error.isEmpty() ? 0 : 3);
                                           });
                    });
                });
            });
        });
    }
    return app.exec();
}
