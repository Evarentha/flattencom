/*
 * flattencom - Record Inspector
 *
 * Displays complete records as JSON or translated fields with search and exact-value copying.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

#include "InspectorDetails.h"
#include "Style.h"
#include <QApplication>
#include <QClipboard>
#include <QFontDatabase>
#include <QHeaderView>
#include <QJsonArray>
#include <QJsonDocument>
#include <QLabel>
#include <QLineEdit>
#include <QMenu>
#include <QPlainTextEdit>
#include <QPushButton>
#include <QSettings>
#include <QShortcut>
#include <QTabWidget>
#include <QTreeWidget>
#include <QVBoxLayout>

namespace {
QString fieldName(const QString &key) {
    const QMap<QString, QString> labels{{"seq", fc::text("Sequence")},
                                        {"dir", fc::text("Direction")},
                                        {"t_us", fc::text("Timestamp (UTC microseconds)")},
                                        {"mono_us", fc::text("Elapsed time (microseconds)")},
                                        {"len", fc::text("Bytes")},
                                        {"source", fc::text("Source")},
                                        {"text", fc::text("Text")},
                                        {"hex", fc::text("Hexadecimal data")},
                                        {"decoded_text", fc::text("Decoded")},
                                        {"decoded_fields", fc::text("Decoded fields")},
                                        {"decoded_level", fc::text("Severity")},
                                        {"history_key", fc::text("Saved log position")}};
    return labels.value(key, key);
}
QString valueText(const QJsonValue &value) {
    if (value.isString())
        return value.toString();
    if (value.isObject())
        return QString::fromUtf8(QJsonDocument(value.toObject()).toJson(QJsonDocument::Indented));
    if (value.isArray())
        return QString::fromUtf8(QJsonDocument(value.toArray()).toJson(QJsonDocument::Indented));
    // Serialize scalars as JSON rather than locale-dependent QVariant text.
    const auto bytes = QJsonDocument(QJsonArray{value}).toJson(QJsonDocument::Compact);
    return QString::fromUtf8(bytes.mid(1, bytes.size() - 2));
}
} // namespace

InspectorDetails::InspectorDetails(QWidget *parent) : QWidget(parent) {
    setObjectName("inspectorDetails");
    setMinimumWidth(290);
    auto *layout = new QVBoxLayout(this);
    layout->setContentsMargins(0, 0, 0, 0);
    layout->setSpacing(4);
    tabs_ = new QTabWidget;
    tabs_->setObjectName("inspectorMode");
    json_ = new QPlainTextEdit;
    json_->setObjectName("inspectorJson");
    json_->setReadOnly(true);
    json_->setUndoRedoEnabled(false);
    json_->setLineWrapMode(QPlainTextEdit::NoWrap);
    json_->setFont(QFontDatabase::systemFont(QFontDatabase::FixedFont));
    json_->setPlaceholderText(fc::text("Select a record to inspect."));
    auto *fieldPage = new QWidget;
    auto *fieldLayout = new QVBoxLayout(fieldPage);
    fieldLayout->setContentsMargins(0, 0, 0, 0);
    fields_ = new QTreeWidget;
    fields_->setObjectName("inspectorFields");
    fields_->setHeaderLabels({fc::text("Field"), fc::text("Value")});
    fields_->setRootIsDecorated(false);
    fields_->setWordWrap(false);
    fields_->setTextElideMode(Qt::ElideNone);
    fields_->header()->setStretchLastSection(false);
    fields_->header()->setSectionResizeMode(QHeaderView::ResizeToContents);
    fields_->setContextMenuPolicy(Qt::CustomContextMenu);
    fieldLayout->addWidget(fields_);
    auto *copy = new QPushButton(fc::text("Copy full value"));
    copy->setObjectName("copyInspectorValue");
    copy->setEnabled(false);
    fieldLayout->addWidget(copy);
    connect(fields_, &QTreeWidget::currentItemChanged, this,
            [copy](QTreeWidgetItem *item) { copy->setEnabled(item != nullptr); });
    connect(copy, &QPushButton::clicked, this, &InspectorDetails::copyValue);
    auto *copyShortcut = new QShortcut(QKeySequence::Copy, fields_);
    copyShortcut->setContext(Qt::WidgetWithChildrenShortcut);
    connect(copyShortcut, &QShortcut::activated, this, &InspectorDetails::copyValue);
    connect(fields_, &QTreeWidget::customContextMenuRequested, this, [this](const QPoint &point) {
        auto *item = fields_->itemAt(point);
        if (!item)
            return;
        fields_->setCurrentItem(item);
        QMenu menu;
        auto *copy = menu.addAction(fc::text("Copy full value"));
        if (menu.exec(fields_->viewport()->mapToGlobal(point)) == copy)
            copyValue();
    });
    tabs_->addTab(json_, "JSON");
    tabs_->addTab(fieldPage, fc::text("Fields"));
    layout->addWidget(tabs_, 1);
    auto *row = new QHBoxLayout;
    query_ = new QLineEdit;
    query_->setObjectName("inspectorSearch");
    query_->setClearButtonEnabled(true);
    query_->setPlaceholderText(fc::text("Search record"));
    auto *next = new QPushButton(fc::text("Find next"));
    row->addWidget(query_, 1);
    row->addWidget(next);
    layout->addLayout(row);
    result_ = new QLabel;
    result_->setObjectName("inspectorSearchResult");
    result_->hide();
    layout->addWidget(result_);
    connect(next, &QPushButton::clicked, this, &InspectorDetails::findNext);
    connect(query_, &QLineEdit::returnPressed, this, &InspectorDetails::findNext);
    connect(query_, &QLineEdit::textChanged, this, [this] { result_->hide(); });
    auto *find = new QShortcut(QKeySequence::Find, this);
    find->setContext(Qt::WidgetWithChildrenShortcut);
    connect(find, &QShortcut::activated, this, [this] {
        query_->setFocus();
        query_->selectAll();
    });
    tabs_->setCurrentIndex(QSettings().value("inspector/view", "json").toString() == "fields" ? 1 : 0);
    connect(tabs_, &QTabWidget::currentChanged, this, [this](int index) {
        QSettings().setValue("inspector/view", index == 0 ? "json" : "fields");
        result_->hide();
    });
}
void InspectorDetails::setFrame(const QJsonObject &frame) {
    json_->setPlainText(QString::fromUtf8(QJsonDocument(frame).toJson(QJsonDocument::Indented)));
    fields_->clear();
    result_->hide();
    // Include every returned field, including nested decoder fields and unknown extensions.
    for (auto it = frame.begin(); it != frame.end(); ++it) {
        const auto value = valueText(it.value());
        auto *item = new QTreeWidgetItem(fields_, {fieldName(it.key()), value});
        item->setData(0, Qt::UserRole, it.key());
        item->setToolTip(0, it.key());
        item->setData(1, Qt::UserRole, value);
    }
}
void InspectorDetails::clear() {
    json_->clear();
    fields_->clear();
    result_->hide();
}
void InspectorDetails::retranslate() {
    for (int i = 0; i < fields_->topLevelItemCount(); ++i) {
        auto *item = fields_->topLevelItem(i);
        item->setText(0, fieldName(item->data(0, Qt::UserRole).toString()));
    }
}
void InspectorDetails::copyValue() {
    if (auto *item = fields_->currentItem())
        QApplication::clipboard()->setText(item->data(1, Qt::UserRole).toString());
}
void InspectorDetails::findNext() {
    if (query_->text().isEmpty()) {
        result_->hide();
        return;
    }
    // JSON search provides exact character navigation even for multiline table values.
    if (tabs_->currentIndex() != 0)
        tabs_->setCurrentIndex(0);
    bool found = json_->find(query_->text());
    if (!found) {
        auto cursor = json_->textCursor();
        cursor.movePosition(QTextCursor::Start);
        json_->setTextCursor(cursor);
        found = json_->find(query_->text());
    }
    result_->setText(fc::text("No match"));
    result_->setVisible(!found);
}
