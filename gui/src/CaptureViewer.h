/*
 * flattencom - Capture File Viewer Interface
 *
 * Declares the types and operations for the capture file viewer component.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

#pragma once
#include <QFile>
#include <QTimer>
#include <QWidget>

class QPlainTextEdit;
class QLineEdit;
class QLabel;
class QPushButton;

// Bounded disk viewer. Display limits never discard data from the current page.
class CaptureViewer final : public QWidget {
    Q_OBJECT
  public:
    static constexpr qint64 PageBytes = 256 * 1024;
    explicit CaptureViewer(QWidget *parent = nullptr);
    bool openFile(const QString &path);
    qint64 startOffset() const { return start_; }
    qint64 endOffset() const { return end_; }
    qint64 snapshotSize() const { return size_; }
    void firstPage();
    void lastPage();
    void nextPage();
    void previousPage();
    void refreshFile();
    void findNext();
    void cancelSearch();
  signals:
    void pageLoaded();
    void searchFinished(bool found);

  private:
    bool load(qint64 offset, qint64 includeThrough = -1);
    qint64 boundary(qint64 offset);
    void searchChunk();
    void updateControls();
    void fail(const QString &message);
    QFile file_;
    qint64 size_ = 0, start_ = 0, end_ = 0;
    QByteArray page_;
    QPlainTextEdit *view_;
    QLineEdit *find_;
    QLabel *position_, *notice_;
    QPushButton *first_, *previous_, *next_, *last_, *search_, *cancel_;
    QTimer searchTimer_;
    QByteArray needle_, overlap_;
    qint64 scan_ = 0, searchNext_ = 0;
    QString lastQuery_;
};
