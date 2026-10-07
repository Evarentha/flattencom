/*
 * flattencom - Desktop Workspace Interface
 *
 * Declares the types and operations for the desktop workspace component.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

#pragma once
#include "InspectorDetails.h"
#include "RateChart.h"
#include "RpcClient.h"
#include "SessionPane.h"
#include <QCheckBox>
#include <QLabel>
#include <QMainWindow>
#include <QTabWidget>
#include <QTextEdit>
#include <QTreeWidget>
class QComboBox;
class QToolBar;

class MainWindow final : public QMainWindow {
    Q_OBJECT
  public:
    explicit MainWindow(QWidget *parent = nullptr);
    void openVirtual();
    RpcClient *rpc() { return &rpc_; }
    SessionPane *currentPane() const;
    void refresh();
  signals:
    void ready();
    void paneAdded(SessionPane *pane);

  protected:
    void closeEvent(QCloseEvent *event) override;

  private:
    void openPort(const QString &path = {});
    void addSession(const QJsonObject &session);
    void openSessionResult(const QJsonObject &result);
    void showFrame(const QJsonObject &frame);
    void updateStats(const QJsonObject &stats);
    void closeSession(int index);
    void changeLanguage(const QString &language);
    void refreshMacros();
    void detachCurrentView();
    RpcClient rpc_;
    QTreeWidget *ports_;
    QTabWidget *tabs_;
    QTextEdit *events_;
    InspectorDetails *details_ = nullptr;
    QLabel *connection_, *stats_;
    RateChart *chart_;
    QMap<QString, QLabel *> signalLights_;
    bool refreshing_ = false;
    QByteArray defaultLayout_;
    QCheckBox *dtr_, *rts_;
    QToolBar *macroBar_;
    QComboBox *macroList_;
};
