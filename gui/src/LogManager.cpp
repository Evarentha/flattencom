/*
 * flattencom - Capture Library Dialog
 *
 * Lists recorded files and exposes paged reading, path copying and file-copy actions.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

#include "LogManager.h"
#include "CaptureViewer.h"
#include "Recording.h"
#include "Style.h"
#include <QApplication>
#include <QClipboard>
#include <QDateTime>
#include <QDesktopServices>
#include <QDir>
#include <QFile>
#include <QFileDialog>
#include <QFileInfo>
#include <QJsonDocument>
#include <QJsonObject>
#include <QLabel>
#include <QLineEdit>
#include <QMessageBox>
#include <QPlainTextEdit>
#include <QPushButton>
#include <QTreeWidget>
#include <QUrl>
#include <QVBoxLayout>

LogManager::LogManager(QWidget *parent, const QString &initialFile) : QDialog(parent) {
    setWindowTitle(fc::text("Capture library"));
    resize(1000, 700);
    auto *layout = new QVBoxLayout(this);
    auto *row = new QHBoxLayout;
    auto *directory = new QLineEdit(initialFile.isEmpty() ? fc::recordingDirectory()
                                                          : QFileInfo(initialFile).absolutePath());
    auto *browse = new QPushButton(fc::text("Directory…"));
    auto *refresh = new QPushButton(fc::text("Refresh"));
    row->addWidget(directory, 1);
    row->addWidget(browse);
    row->addWidget(refresh);
    layout->addLayout(row);
    auto *search = new QLineEdit;
    search->setPlaceholderText(fc::text("Filter by filename"));
    layout->addWidget(search);
    auto *files = new QTreeWidget;
    files->setHeaderLabels({fc::text("File"), fc::text("Size"), fc::text("Modified")});
    layout->addWidget(files, 1);
    auto *actions = new QHBoxLayout;
    auto *open = new QPushButton(fc::text("Open file"));
    auto *copy = new QPushButton(fc::text("Copy path"));
    auto *exportFile = new QPushButton(fc::text("Save copy…"));
    actions->addWidget(open);
    actions->addWidget(copy);
    actions->addWidget(exportFile);
    layout->addLayout(actions);
    auto *preview = new CaptureViewer;
    layout->addWidget(preview, 2);
    auto reload = [=] {
        files->clear();
        QDir dir(directory->text());
        for (const auto &info : dir.entryInfoList(QDir::Files, QDir::Time)) {
            if (!info.fileName().contains(".log") && !info.fileName().contains(".jsonl") &&
                !info.fileName().contains(".json") && !info.fileName().contains(".txt"))
                continue;
            if (!info.fileName().contains(search->text(), Qt::CaseInsensitive))
                continue;
            auto *item = new QTreeWidgetItem(files, {info.fileName(), QString::number(info.size()),
                                                     info.lastModified().toString(Qt::ISODate)});
            item->setData(0, Qt::UserRole, info.absoluteFilePath());
        }
        files->resizeColumnToContents(0);
    };
    connect(refresh, &QPushButton::clicked, this, reload);
    connect(search, &QLineEdit::textChanged, this, reload);
    connect(browse, &QPushButton::clicked, this, [=, this] {
        auto path = QFileDialog::getExistingDirectory(this, fc::text("Log directory"), directory->text());
        if (!path.isEmpty()) {
            directory->setText(path);
            reload();
        }
    });
    connect(files, &QTreeWidget::currentItemChanged, this, [=](QTreeWidgetItem *item) {
        if (!item)
            return;
        preview->openFile(item->data(0, Qt::UserRole).toString());
    });
    connect(open, &QPushButton::clicked, this, [=] {
        if (auto *item = files->currentItem())
            QDesktopServices::openUrl(QUrl::fromLocalFile(item->data(0, Qt::UserRole).toString()));
    });
    connect(copy, &QPushButton::clicked, this, [=] {
        if (auto *item = files->currentItem())
            QApplication::clipboard()->setText(item->data(0, Qt::UserRole).toString());
    });
    connect(exportFile, &QPushButton::clicked, this, [=, this] {
        auto *item = files->currentItem();
        if (!item)
            return;
        auto path = QFileDialog::getSaveFileName(this, fc::text("Save copy"), item->text(0));
        if (path.isEmpty())
            return;
        if (!QFile::copy(item->data(0, Qt::UserRole).toString(), path))
            QMessageBox::warning(this, "flattencom", fc::text("Copy failed (destination may already exist)"));
    });
    reload();
    for (int i = 0; i < files->topLevelItemCount(); ++i) {
        auto *item = files->topLevelItem(i);
        if (item->data(0, Qt::UserRole).toString() == QFileInfo(initialFile).absoluteFilePath()) {
            files->setCurrentItem(item);
            break;
        }
    }
}
