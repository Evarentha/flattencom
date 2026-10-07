/*
 * flattencom - Transmission History Drawer Interface
 *
 * Declares the types and operations for the transmission history drawer component.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

#pragma once
#include <QJsonArray>
#include <QWidget>

class QLineEdit;
class QLabel;
class QTreeWidget;
class QToolButton;

// A separate view of successful TX chunks. It never feeds data into the RX decoder.
class SentLog final : public QWidget {
    Q_OBJECT
  public:
    explicit SentLog(QWidget *parent = nullptr);
    void append(const QJsonArray &frames);
    void setEvicted(qint64 count);
    QJsonArray frames() const;
    bool isExpanded() const;
    int collapsedHeight() const;
    void setExpanded(bool expanded);
  signals:
    void locate(qint64 seq);
    void selected(const QJsonObject &frame);
    void expandedChanged(bool expanded);

  private:
    void filter();
    void copy(bool hex);
    void updateSummary();
    void resizeEvent(QResizeEvent *event) override;
    QWidget *header_, *body_;
    QToolButton *toggle_;
    QLabel *summary_;
    QString summaryText_;
    QTreeWidget *list_;
    QLineEdit *search_;
    QLabel *status_;
    qsizetype bytes_ = 0;
    qint64 lastSeq_ = -1;
};
