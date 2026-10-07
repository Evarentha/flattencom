/*
 * flattencom - Serial Session Workspace Interface
 *
 * Declares the types and operations for the serial session workspace component.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

#pragma once
#include "FramesModel.h"
#include "RpcClient.h"
#include "SentLog.h"
#include "SessionController.h"
#include "SessionHistory.h"
#include "TextStreamView.h"
#include <QCheckBox>
#include <QComboBox>
#include <QJsonObject>
#include <QLabel>
#include <QLineEdit>
#include <QListWidget>
#include <QPushButton>
#include <QSortFilterProxyModel>
#include <QSpinBox>
#include <QStackedWidget>
#include <QTableView>
#include <QTimer>
#include <QWidget>
class QSplitter;

class SessionPane final : public QWidget {
    Q_OBJECT
  public:
    SessionPane(RpcClient *rpc, const QJsonObject &session, QWidget *parent = nullptr);
    QString sessionId() const { return session_.value("session_id").toString(); }
    QString label() const;
    const QJsonObject &session() const { return session_; }
    void exportLog();
    void sendFile();
    void replayLog();
    void editTriggers();
    void editDecoder();
    void configure();
    void addMarker();
    void showMarkers();
    void resetDevice();
    void configureReset();
    void editRules();
    void exportProblem();
    void shareSelection();
    void measureSelection();
    void send(const QString &text);
    void insertMacro(const QString &text);
    void saveCurrentMacro();
    void longCommand();
    void focusSearch();
    void copySelection();
    void selectAllOutput();
    void clearView();
    bool isPaused() const { return paused_; }
    void setPaused(bool paused);
    void updateSession(const QJsonObject &session);
    void retranslate();
    FramesModel *framesModel() { return model_; }
  signals:
    void frameSelected(const QJsonObject &frame);
    void statsChanged(const QJsonObject &stats);
    void signalsChanged(const QJsonObject &pins);
    void message(const QString &message);
    void receivedFrames(int rows);
    void sent();
    void macrosChanged();
    void displayPausedChanged(bool paused);

  private:
    void poll();
    void receiveContextMenu(const QPoint &position, bool textView);
    void updateRecording(const QJsonObject &recording);
    void updateViewNotice();
    void styleDisplayControl();
    void styleConnectionState();
    QJsonObject evidence() const;
    void startOperation(const QString &method, QJsonObject params);
    void refreshSearchResults();
    void beginNavigation();
    void refreshDecoder(quint64 generation);
    void locateSequence(qint64 seq);
    void pollBootEvents();
    void pollSent();
    void locateSent(qint64 seq);
    void sizeSentDrawer();
    void resizeEvent(QResizeEvent *event) override;
    void historyEdge(bool earlier);
    void openHistory(qint64 sequence = -1);
    QString capturePath() const;
    void call(const QString &method, QJsonObject params, RpcClient::Callback callback = {},
              int timeoutMs = 10000);
    RpcClient *rpc_;
    QJsonObject session_;
    FramesModel *model_;
    QSortFilterProxyModel *proxy_;
    QTableView *table_;
    TextStreamView *textStream_;
    QStackedWidget *receiveViews_;
    QLabel *connectionState_;
    bool sessionStateConfirmed_ = true;
    QLineEdit *input_;
    QLabel *recording_;
    QPushButton *recordingFolder_;
    SessionController *controller_;
    QString recordingError_;
    QJsonObject recordingStatus_;
    QComboBox *encoding_, *newline_, *decoder_;
    QAction *markAction_, *measureAction_, *clearAction_;
    QCheckBox *periodic_;
    QPushButton *pause_;
    QSpinBox *interval_;
    QTimer pollTimer_, sendTimer_, statsTimer_;
    QListWidget *searchResults_;
    SentLog *sentLog_;
    QSplitter *logSplit_;
    int sentDrawerHeight_ = 150;
    QTimer sentTimer_;
    qint64 sentCursor_ = 0;
    bool sentPending_ = false, sentUnsupported_ = false;
    bool tailPending_ = true;
    quint64 viewGeneration_ = 0;
    quint64 decoderGeneration_ = 0;
    QJsonArray bootEvents_;
    QString publishedSelection_;
    bool bootPollPending_ = false;
    qint64 lastBootNumber_ = 0, resetAfterUs_ = 0;
    qint64 cursor_ = 0;
    QStringList history_;
    int historyIndex_ = 0;
    bool pending_ = false, paused_ = false, follow_ = true;
    bool eventFilter(QObject *object, QEvent *event) override;
    SessionHistory *diskHistory_;
    bool diskView_ = false, historyLoading_ = false, historyApplying_ = false;
    int historyPage_ = 0;
    qint64 historyTarget_ = -1;
    QString historyAnchor_;
    QTimer historySearchTimer_;
    QRegularExpression historySearch_;
};
