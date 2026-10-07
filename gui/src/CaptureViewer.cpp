/*
 * flattencom - Capture File Viewer
 *
 * Pages through disk captures and searches file contents with bounded display memory.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

#include "CaptureViewer.h"
#include "Style.h"
#include <QFontDatabase>
#include <QHBoxLayout>
#include <QLabel>
#include <QLineEdit>
#include <QPlainTextEdit>
#include <QPushButton>
#include <QSpinBox>
#include <QTextCursor>
#include <QVBoxLayout>

namespace {
// QTextDocument normalizes CR/CRLF. Use the same normalization for search anchors.
QString displayText(const QByteArray &bytes) {
    return QString::fromUtf8(bytes).replace("\r\n", "\n").replace('\r', '\n');
}
} // namespace

CaptureViewer::CaptureViewer(QWidget *parent) : QWidget(parent) {
    setObjectName("captureViewer");
    auto *layout = new QVBoxLayout(this);
    layout->setContentsMargins(0, 0, 0, 0);
    auto *navigation = new QHBoxLayout;
    first_ = new QPushButton(fc::text("Beginning"));
    previous_ = new QPushButton(fc::text("Previous page"));
    next_ = new QPushButton(fc::text("Next page"));
    last_ = new QPushButton(fc::text("End"));
    auto *refresh = new QPushButton(fc::text("Refresh file"));
    for (auto *button : {first_, previous_, next_, last_, refresh})
        navigation->addWidget(button);
    auto *percent = new QSpinBox;
    percent->setRange(0, 100);
    percent->setSuffix(" %");
    percent->setAccessibleName(fc::text("File position"));
    auto *jump = new QPushButton(fc::text("Go to"));
    navigation->addWidget(percent);
    navigation->addWidget(jump);
    navigation->addStretch();
    layout->addLayout(navigation);
    position_ = new QLabel;
    position_->setTextFormat(Qt::PlainText);
    layout->addWidget(position_);
    auto *searchRow = new QHBoxLayout;
    find_ = new QLineEdit;
    find_->setObjectName("captureSearch");
    find_->setMaxLength(1024);
    find_->setPlaceholderText(fc::text("Search entire file (case-sensitive text)"));
    search_ = new QPushButton(fc::text("Find next"));
    cancel_ = new QPushButton(fc::text("Cancel"));
    cancel_->setEnabled(false);
    searchRow->addWidget(find_, 1);
    searchRow->addWidget(search_);
    searchRow->addWidget(cancel_);
    layout->addLayout(searchRow);
    notice_ = new QLabel(
        fc::text("Pages are read from disk. Older rotated files can be selected in the file list."));
    notice_->setTextFormat(Qt::PlainText);
    notice_->setWordWrap(true);
    layout->addWidget(notice_);
    view_ = new QPlainTextEdit;
    view_->setObjectName("capturePage");
    view_->setReadOnly(true);
    view_->setUndoRedoEnabled(false);
    view_->setLineWrapMode(QPlainTextEdit::NoWrap);
    view_->setFont(QFontDatabase::systemFont(QFontDatabase::FixedFont));
    // The page's byte bound is the memory limit; a block-count cap would hide
    // early lines in pages containing many short lines.
    layout->addWidget(view_, 1);
    connect(first_, &QPushButton::clicked, this, &CaptureViewer::firstPage);
    connect(previous_, &QPushButton::clicked, this, &CaptureViewer::previousPage);
    connect(next_, &QPushButton::clicked, this, &CaptureViewer::nextPage);
    connect(last_, &QPushButton::clicked, this, &CaptureViewer::lastPage);
    connect(refresh, &QPushButton::clicked, this, &CaptureViewer::refreshFile);
    connect(jump, &QPushButton::clicked, this, [this, percent] {
        cancelSearch();
        if (percent->value() == 100)
            lastPage();
        else
            load(size_ / 100 * percent->value());
    });
    connect(search_, &QPushButton::clicked, this, &CaptureViewer::findNext);
    connect(find_, &QLineEdit::returnPressed, this, &CaptureViewer::findNext);
    connect(find_, &QLineEdit::textChanged, this, [this] {
        cancelSearch();
        lastQuery_.clear();
        searchNext_ = 0;
    });
    connect(cancel_, &QPushButton::clicked, this, &CaptureViewer::cancelSearch);
    searchTimer_.setInterval(1);
    connect(&searchTimer_, &QTimer::timeout, this, &CaptureViewer::searchChunk);
    updateControls();
}

bool CaptureViewer::openFile(const QString &path) {
    cancelSearch();
    file_.close();
    file_.setFileName(path);
    size_ = start_ = end_ = searchNext_ = 0;
    page_.clear();
    lastQuery_.clear();
    view_->clear();
    if (!file_.open(QIODevice::ReadOnly)) {
        fail(file_.errorString());
        return false;
    }
    size_ = file_.size();
    notice_->setText(
        fc::text("Pages are read from disk. Older rotated files can be selected in the file list."));
    return load(0);
}

qint64 CaptureViewer::boundary(qint64 offset) {
    offset = qBound(qint64(0), offset, size_);
    if (offset == 0 || offset == size_)
        return offset;
    const auto base = qMax(qint64(0), offset - 3);
    if (!file_.seek(base))
        return offset;
    const auto bytes = file_.read(offset - base + 1);
    qint64 result = offset;
    // Start/end on a UTF-8 lead byte rather than splitting a character.
    while (result > base && result - base < bytes.size() &&
           (static_cast<unsigned char>(bytes[result - base]) & 0xc0) == 0x80)
        --result;
    if (result == offset && offset > base && offset - base < bytes.size() && bytes[offset - base] == '\n' &&
        bytes[offset - base - 1] == '\r')
        --result;
    return result;
}

bool CaptureViewer::load(qint64 offset, qint64 includeThrough) {
    if (!file_.isOpen())
        return false;
    if (file_.size() < size_) {
        fail(fc::text("File was truncated or rotated. Refresh to reopen it, or select an older segment."));
        return false;
    }
    const auto start = boundary(offset);
    const auto end = boundary(qMin(size_, qMax(start + PageBytes, includeThrough)));
    if (!file_.seek(start)) {
        fail(file_.errorString());
        return false;
    }
    auto bytes = file_.read(end - start);
    if (bytes.size() != end - start) {
        fail(fc::text("File changed while reading. Refresh and try again."));
        return false;
    }
    start_ = start;
    end_ = end;
    page_ = bytes;
    view_->setPlainText(displayText(page_));
    position_->setText(fc::text("Bytes %1-%2 of %3 (snapshot)").arg(start_).arg(end_).arg(size_));
    updateControls();
    emit pageLoaded();
    return true;
}

void CaptureViewer::firstPage() {
    cancelSearch();
    load(0);
}
void CaptureViewer::lastPage() {
    cancelSearch();
    load(qMax(qint64(0), size_ - PageBytes));
}
void CaptureViewer::nextPage() {
    cancelSearch();
    if (end_ < size_)
        load(end_);
}
void CaptureViewer::previousPage() {
    cancelSearch();
    if (start_ > 0)
        load(qMax(qint64(0), start_ - PageBytes));
}
void CaptureViewer::refreshFile() {
    const auto path = file_.fileName();
    const auto offset = start_;
    const bool tail = end_ == size_;
    if (!path.isEmpty() && openFile(path)) {
        if (tail)
            lastPage();
        else
            load(qMin(offset, qMax(qint64(0), size_ - PageBytes)));
    }
}
void CaptureViewer::updateControls() {
    const bool opened = file_.isOpen();
    first_->setEnabled(opened && start_ > 0);
    previous_->setEnabled(opened && start_ > 0);
    next_->setEnabled(opened && end_ < size_);
    last_->setEnabled(opened && end_ < size_);
    search_->setEnabled(opened && !searchTimer_.isActive());
    cancel_->setEnabled(searchTimer_.isActive());
}
void CaptureViewer::fail(const QString &message) {
    cancelSearch();
    notice_->setText(message);
    updateControls();
}
void CaptureViewer::cancelSearch() {
    if (searchTimer_.isActive()) {
        searchTimer_.stop();
        notice_->setText(fc::text("Search cancelled."));
    }
    updateControls();
}
void CaptureViewer::findNext() {
    if (!file_.isOpen() || find_->text().isEmpty() || searchTimer_.isActive())
        return;
    if (lastQuery_ != find_->text()) {
        lastQuery_ = find_->text();
        searchNext_ = 0;
    }
    needle_ = find_->text().toUtf8();
    overlap_.clear();
    scan_ = searchNext_;
    notice_->setText(fc::text("Searching file..."));
    searchTimer_.start();
    updateControls();
}
void CaptureViewer::searchChunk() {
    if (file_.size() < size_) {
        fail(fc::text("File was truncated or rotated. Refresh to reopen it, or select an older segment."));
        emit searchFinished(false);
        return;
    }
    if (scan_ >= size_) {
        searchTimer_.stop();
        searchNext_ = 0;
        notice_->setText(fc::text("No further match. Find next will search from the beginning."));
        updateControls();
        emit searchFinished(false);
        return;
    }
    if (!file_.seek(scan_)) {
        fail(file_.errorString());
        emit searchFinished(false);
        return;
    }
    const auto bytes = file_.read(qMin(PageBytes, size_ - scan_));
    if (bytes.isEmpty()) {
        fail(fc::text("File changed while reading. Refresh and try again."));
        emit searchFinished(false);
        return;
    }
    const auto base = scan_ - overlap_.size();
    const auto window = overlap_ + bytes;
    const auto index = window.indexOf(needle_);
    scan_ += bytes.size();
    if (index >= 0) {
        const auto match = base + index;
        searchNext_ = match + needle_.size();
        searchTimer_.stop();
        if (load(qMax(qint64(0), match - 4096), searchNext_)) {
            QTextCursor cursor(view_->document());
            cursor.setPosition(displayText(page_.left(match - start_)).size());
            cursor.setPosition(displayText(page_.left(searchNext_ - start_)).size(), QTextCursor::KeepAnchor);
            view_->setTextCursor(cursor);
            view_->centerCursor();
            notice_->setText(fc::text("Match at byte %1").arg(match));
            updateControls();
            emit searchFinished(true);
        } else {
            emit searchFinished(false);
        }
        return;
    }
    overlap_ = window.right(needle_.size() - 1);
}
