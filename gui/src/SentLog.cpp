/*
 * flattencom - Transmission History Drawer
 *
 * Displays bounded sent-command history with filtering, copying and sequence navigation.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

#include "SentLog.h"
#include "Style.h"
#include <QApplication>
#include <QClipboard>
#include <QDateTime>
#include <QHeaderView>
#include <QJsonDocument>
#include <QJsonObject>
#include <QLabel>
#include <QLineEdit>
#include <QMenu>
#include <QPushButton>
#include <QResizeEvent>
#include <QScrollBar>
#include <QToolButton>
#include <QTreeWidget>
#include <QVBoxLayout>

namespace {
QString preview(const QJsonObject &frame) {
    QString text = frame.value("text").toString();
    text.replace('\\', "\\\\")
        .replace('\r', "\\r")
        .replace('\n', "\\n")
        .replace('\t', "\\t")
        .replace(QChar(0), "\\0");
    return text.left(1024);
}
qsizetype sizeOf(const QJsonObject &frame) {
    return QJsonDocument(frame).toJson(QJsonDocument::Compact).size();
}
} // namespace

SentLog::SentLog(QWidget *parent) : QWidget(parent) {
    setObjectName("sentLog");
    auto *layout = new QVBoxLayout(this);
    layout->setSizeConstraint(QLayout::SetNoConstraint);
    layout->setContentsMargins(0, 0, 0, 0);
    layout->setSpacing(0);
    header_ = new QWidget;
    auto *headerLayout = new QHBoxLayout(header_);
    headerLayout->setContentsMargins(0, 0, 0, 0);
    toggle_ = new QToolButton;
    toggle_->setObjectName("toggleSentLog");
    toggle_->setText(fc::text("Sent commands"));
    toggle_->setCheckable(true);
    toggle_->setArrowType(Qt::RightArrow);
    toggle_->setToolButtonStyle(Qt::ToolButtonTextBesideIcon);
    toggle_->setStyleSheet("QToolButton { padding: 2px 4px; border: 0; }");
    toggle_->setToolTip(fc::text("Show or hide sent commands"));
    summary_ = new QLabel;
    summary_->setObjectName("lastSentCommand");
    summary_->setTextFormat(Qt::PlainText);
    summary_->setSizePolicy(QSizePolicy::Ignored, QSizePolicy::Preferred);
    auto palette = summary_->palette();
    palette.setColor(QPalette::WindowText, palette.color(QPalette::PlaceholderText));
    summary_->setPalette(palette);
    headerLayout->addWidget(toggle_);
    headerLayout->addWidget(summary_, 1);
    layout->addWidget(header_);
    body_ = new QWidget;
    body_->setObjectName("sentLogBody");
    auto *bodyLayout = new QVBoxLayout(body_);
    bodyLayout->setSizeConstraint(QLayout::SetNoConstraint);
    bodyLayout->setContentsMargins(0, 4, 0, 0);
    bodyLayout->setSpacing(4);
    layout->addWidget(body_, 1);
    auto *row = new QHBoxLayout;
    search_ = new QLineEdit;
    search_->setPlaceholderText(fc::text("Search content or source"));
    row->addWidget(search_, 1);
    auto *copyText = new QPushButton(fc::text("Copy"));
    auto *locate = new QPushButton(fc::text("Show in log"));
    row->addWidget(copyText);
    row->addWidget(locate);
    bodyLayout->addLayout(row);
    list_ = new QTreeWidget;
    list_->setObjectName("sentLogEntries");
    list_->setRootIsDecorated(false);
    list_->setHeaderLabels({fc::text("Time"), fc::text("Content"), fc::text("Bytes"), fc::text("Source")});
    list_->setSelectionMode(QAbstractItemView::ExtendedSelection);
    list_->setUniformRowHeights(true);
    list_->header()->setSectionResizeMode(1, QHeaderView::Stretch);
    list_->setColumnWidth(0, 110);
    list_->setColumnWidth(2, 65);
    list_->setColumnWidth(3, 190);
    list_->setContextMenuPolicy(Qt::CustomContextMenu);
    list_->setMinimumHeight(0);
    bodyLayout->addWidget(list_, 1);
    status_ = new QLabel;
    status_->hide();
    bodyLayout->addWidget(status_);
    list_->setToolTip(fc::text("Double-click to locate. Sent data does not confirm device execution."));
    connect(search_, &QLineEdit::textChanged, this, [this] { filter(); });
    connect(copyText, &QPushButton::clicked, this, [this] { copy(false); });
    connect(locate, &QPushButton::clicked, this, [this] {
        if (auto *item = list_->currentItem())
            emit this->locate(item->data(0, Qt::UserRole).toJsonObject().value("seq").toInteger());
    });
    connect(list_, &QTreeWidget::itemDoubleClicked, this, [this](QTreeWidgetItem *item) {
        emit this->locate(item->data(0, Qt::UserRole).toJsonObject().value("seq").toInteger());
    });
    connect(list_, &QTreeWidget::currentItemChanged, this, [this](QTreeWidgetItem *item) {
        if (item)
            emit selected(item->data(0, Qt::UserRole).toJsonObject());
    });
    connect(list_, &QTreeWidget::customContextMenuRequested, this, [this](const QPoint &pos) {
        QMenu menu;
        auto *text = menu.addAction(fc::text("Copy text"));
        auto *hex = menu.addAction(fc::text("Copy HEX"));
        const auto action = menu.exec(list_->viewport()->mapToGlobal(pos));
        if (action == text)
            copy(false);
        else if (action == hex)
            copy(true);
    });
    copyText->setEnabled(false);
    locate->setEnabled(false);
    connect(list_, &QTreeWidget::itemSelectionChanged, this, [this, copyText, locate] {
        const bool any = !list_->selectedItems().isEmpty();
        copyText->setEnabled(any);
        locate->setEnabled(any);
    });
    connect(toggle_, &QToolButton::toggled, this, [this](bool expanded) {
        body_->setVisible(expanded);
        toggle_->setArrowType(expanded ? Qt::DownArrow : Qt::RightArrow);
        setMaximumHeight(expanded ? QWIDGETSIZE_MAX : collapsedHeight());
        updateGeometry();
        emit expandedChanged(expanded);
    });
    body_->hide();
    setMaximumHeight(collapsedHeight());
}
bool SentLog::isExpanded() const { return toggle_->isChecked(); }
int SentLog::collapsedHeight() const { return header_->sizeHint().height(); }
void SentLog::setExpanded(bool expanded) { toggle_->setChecked(expanded); }
void SentLog::updateSummary() {
    summary_->setText(summary_->fontMetrics().elidedText(summaryText_, Qt::ElideRight, summary_->width()));
}
void SentLog::resizeEvent(QResizeEvent *event) {
    QWidget::resizeEvent(event);
    updateSummary();
}
void SentLog::append(const QJsonArray &frames) {
    if (frames.isEmpty())
        return;
    const bool follow = list_->verticalScrollBar()->value() >= list_->verticalScrollBar()->maximum();
    for (const auto &value : frames) {
        const auto frame = value.toObject();
        const auto seq = frame.value("seq").toInteger();
        if (frame.value("dir") != "tx" || seq <= lastSeq_)
            continue;
        lastSeq_ = seq;
        bytes_ += sizeOf(frame);
        auto *item = new QTreeWidgetItem(
            list_,
            {QDateTime::fromMSecsSinceEpoch(frame.value("t_us").toInteger() / 1000).toString("HH:mm:ss.zzz"),
             preview(frame), QString::number(frame.value("len").toInteger()),
             frame.value("source").toString()});
        item->setData(0, Qt::UserRole, frame);
        item->setToolTip(1, preview(frame));
        item->setToolTip(0, QDateTime::fromMSecsSinceEpoch(frame.value("t_us").toInteger() / 1000)
                                .toString(Qt::ISODateWithMs));
        summaryText_ = item->text(0) + "  " + item->text(1);
        summary_->setToolTip(item->text(0) + "  " + frame.value("source").toString() + "\n" + preview(frame));
    }
    while (list_->topLevelItemCount() > 1 &&
           (list_->topLevelItemCount() > 2000 || bytes_ > 8 * 1024 * 1024)) {
        auto *item = list_->takeTopLevelItem(0);
        bytes_ -= sizeOf(item->data(0, Qt::UserRole).toJsonObject());
        delete item;
    }
    filter();
    if (follow)
        list_->scrollToBottom();
    updateSummary();
}
void SentLog::filter() {
    for (int i = 0; i < list_->topLevelItemCount(); ++i) {
        auto *item = list_->topLevelItem(i);
        const auto frame = item->data(0, Qt::UserRole).toJsonObject();
        item->setHidden(!(frame.value("text").toString() + frame.value("hex").toString() +
                          frame.value("source").toString())
                             .contains(search_->text(), Qt::CaseInsensitive));
    }
}
void SentLog::copy(bool hex) {
    QStringList values;
    for (int i = 0; i < list_->topLevelItemCount(); ++i) {
        auto *item = list_->topLevelItem(i);
        if (item->isSelected() && !item->isHidden())
            values.append(item->data(0, Qt::UserRole).toJsonObject().value(hex ? "hex" : "text").toString());
    }
    QApplication::clipboard()->setText(values.join(hex ? "\n" : ""));
}
void SentLog::setEvicted(qint64 count) {
    status_->setVisible(count > 0);
    status_->setText(fc::text("Older sent records were evicted; see the JSONL capture."));
}
QJsonArray SentLog::frames() const {
    QJsonArray result;
    for (int i = 0; i < list_->topLevelItemCount(); ++i)
        result.append(list_->topLevelItem(i)->data(0, Qt::UserRole).toJsonObject());
    return result;
}
