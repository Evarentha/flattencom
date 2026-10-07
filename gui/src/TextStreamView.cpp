/*
 * flattencom - Continuous Receive Text View
 *
 * Reassembles UTF-8 and line endings across chunks while retaining searchable sequence anchors.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

#include "TextStreamView.h"
#include "Style.h"
#include <QFontDatabase>
#include <QJsonObject>
#include <QScrollBar>
#include <QSignalBlocker>
#include <QTextBlock>
#include <QTextCursor>

namespace {
constexpr int MaxBlocks = 20000;
constexpr int MaxCharacters = 2 * 1024 * 1024;
constexpr int MaxLineCharacters = 16384;
constexpr int MaxEscapeCharacters = 4096;
} // namespace

TextStreamView::TextStreamView(QWidget *parent) : QPlainTextEdit(parent) {
    Q_INIT_RESOURCE(resources);
    rules_ = new LogRules(document());
    setObjectName("receiveTextStream");
    setReadOnly(true);
    setUndoRedoEnabled(false);
    setFont(QFontDatabase::systemFont(QFontDatabase::FixedFont));
    setMaximumBlockCount(MaxBlocks);
    setLineWrapMode(QPlainTextEdit::NoWrap);
    document()->setDocumentMargin(2);
    setPlaceholderText(fc::text("Waiting for data"));
    connect(verticalScrollBar(), &QScrollBar::valueChanged, this, [this](int value) {
        if (!updating_)
            following_ = value >= verticalScrollBar()->maximum();
    });
    searchTimer_.setSingleShot(true);
    searchTimer_.setInterval(150);
    connect(&searchTimer_, &QTimer::timeout, this, [this] { highlightMatches(); });
    connect(this, &QPlainTextEdit::cursorPositionChanged, this, [this] {
        const auto block = textCursor().block();
        if (auto *a = dynamic_cast<LogAnchor *>(block.userData()))
            emit frameInspected(QJsonObject{
                {"seq", a->firstSeq}, {"t_us", a->firstUs}, {"text", block.text()}, {"dir", "rx"}});
    });
}

void TextStreamView::appendFrames(const QJsonArray &frames) {
    for (const auto &value : frames) {
        const auto frame = value.toObject();
        // TX must not split an incoming line or feed bytes into the RX decoder.
        if (frame.value("dir").toString() != "rx")
            continue;
        QString output;
        if (frame.contains("hex")) {
            const auto bytes = QByteArray::fromHex(frame.value("hex").toString().toLatin1());
            output += normalize(decoder_.decode(bytes));
        } else {
            // Compatibility with text-only sources. Normal daemon pages include hex.
            output += normalize(frame.value("text").toString());
        }
        appendText(output, frame);
    }
}

QString TextStreamView::normalize(const QString &text) {
    QString output;
    output.reserve(text.size());
    for (const QChar ch : text) {
        // Suppress ANSI decoration (including sequences split across reads).
        // This is a line-oriented log viewer, not a VT terminal emulator.
        if (escape_ != EscapeState::Text) {
            ++escapeLength_;
            if (escapeLength_ > MaxEscapeCharacters) {
                escape_ = EscapeState::Text;
                escapeLength_ = 0;
                // Resume at this character rather than swallowing an unbounded log.
            } else {
                switch (escape_) {
                case EscapeState::Escape:
                    escape_ = ch == '[' ? EscapeState::Csi : ch == ']' ? EscapeState::Osc : EscapeState::Text;
                    break;
                case EscapeState::Csi:
                    if (ch.unicode() >= 0x40 && ch.unicode() <= 0x7e)
                        escape_ = EscapeState::Text;
                    break;
                case EscapeState::Osc:
                    if (ch == QChar(7))
                        escape_ = EscapeState::Text;
                    else if (ch == QChar(27))
                        escape_ = EscapeState::OscEscape;
                    break;
                case EscapeState::OscEscape:
                    escape_ = ch == '\\' || ch == QChar(7) ? EscapeState::Text : EscapeState::Osc;
                    break;
                case EscapeState::Text:
                    break;
                }
                continue;
            }
        }
        if (ch == QChar(27)) {
            escape_ = EscapeState::Escape;
            escapeLength_ = 0;
            continue;
        }
        if (ch == '\n' && afterCr_) {
            afterCr_ = false;
            continue; // CRLF is one newline even when CR and LF arrive separately.
        }
        afterCr_ = ch == '\r';
        if (ch == '\r' || ch == '\n') {
            output += '\n';
            column_ = 0;
        } else {
            // Bound pathological binary/no-newline input without truncating raw frames.
            if (column_ >= MaxLineCharacters && !ch.isLowSurrogate()) {
                output += '\n';
                column_ = 0;
            }
            if (ch.unicode() < 0x20 && ch != '\t')
                output += QChar(0x2400 + ch.unicode());
            else
                output += ch;
            ++column_;
        }
    }
    return output;
}

void TextStreamView::appendText(const QString &text, const QJsonObject &frame) {
    if (text.isEmpty())
        return;
    const bool batch = updating_;
    const int previousScroll = batch ? 0 : verticalScrollBar()->value();
    updating_ = true;
    QTextCursor cursor(document());
    cursor.movePosition(QTextCursor::End);
    // The empty paragraph after a final newline is only an insertion position.
    // Restore its normal metrics before appending actual text or another newline.
    cursor.setBlockCharFormat(QTextCharFormat());
    cursor.setCharFormat(QTextCharFormat());
    // Attach byte-chunk bounds and host timestamps to each logical line.
    const auto parts = text.split(QRegularExpression("[\\n\\x{2029}]"));
    qsizetype historyOffset = frame.value("history_offset").toInteger();
    qsizetype textOffset = 0;
    for (qsizetype i = 0; i < parts.size(); ++i) {
        if (!frame.isEmpty() && (!parts[i].isEmpty() || i + 1 < parts.size())) {
            auto block = cursor.block();
            auto *anchor = dynamic_cast<LogAnchor *>(block.userData());
            if (!anchor) {
                anchor = new LogAnchor;
                anchor->firstSeq = frame.value("seq").toInteger();
                anchor->firstUs = frame.value("t_us").toInteger();
                anchor->firstMono = frame.value("mono_us").toInteger(anchor->firstUs);
                anchor->historyKey = frame.value("history_key").toString();
                anchor->monotonic = frame.value("history_clock").toString() != "wall";
                block.setUserData(anchor);
            }
            anchor->lastSeq = frame.value("seq").toInteger();
            anchor->lastUs = frame.value("t_us").toInteger();
            anchor->lastMono = frame.value("mono_us").toInteger(anchor->lastUs);
            const auto key = frame.value("history_key").toString();
            if (!key.isEmpty())
                anchor->historySpans.append({key, historyOffset, parts[i].size(), cursor.positionInBlock()});
            if (i + 1 < parts.size())
                anchor->separator = text[textOffset + parts[i].size()];
        }
        cursor.insertText(parts[i]);
        if (i + 1 < parts.size())
            cursor.insertBlock();
        historyOffset += parts[i].size() + 1;
        textOffset += parts[i].size() + 1;
    }
    if (cursor.block().text().isEmpty()) {
        QTextCharFormat insertionPoint;
        insertionPoint.setFontPointSize(1);
        cursor.setBlockCharFormat(insertionPoint);
    }
    if (document()->characterCount() > MaxCharacters) {
        const int excess = document()->characterCount() - MaxCharacters;
        const auto block = document()->findBlock(excess);
        QTextCursor trim(document());
        const auto next = block.next();
        trim.setPosition(next.isValid() ? next.position() : block.position(), QTextCursor::KeepAnchor);
        trim.removeSelectedText();
    }
    if (!batch)
        verticalScrollBar()->setValue(following_ ? verticalScrollBar()->maximum() : previousScroll);
    updating_ = batch;
    if (!search_.pattern().isEmpty() && !searchTimer_.isActive())
        searchTimer_.start();
}

void TextStreamView::resetStream() {
    decoder_.resetState();
    afterCr_ = false;
    escape_ = EscapeState::Text;
    escapeLength_ = 0;
    column_ = 0;
}

void TextStreamView::reloadRules() { rules_->reload(); }
QJsonObject TextStreamView::selectedEvidence() const {
    const auto cursor = textCursor();
    if (!cursor.hasSelection())
        return {};
    auto first = document()->findBlock(cursor.selectionStart());
    auto last = document()->findBlock(cursor.selectionEnd() - 1);
    auto *a = dynamic_cast<LogAnchor *>(first.userData());
    auto *b = dynamic_cast<LogAnchor *>(last.userData());
    auto selected = cursor.selectedText();
    for (auto block = first; block.isValid() && block.position() < cursor.selectionEnd();
         block = block.next()) {
        const auto separator = block.position() + block.length() - 1;
        if (separator >= cursor.selectionStart() && separator < cursor.selectionEnd()) {
            auto *anchor = dynamic_cast<LogAnchor *>(block.userData());
            selected[separator - cursor.selectionStart()] = anchor ? anchor->separator : QChar('\n');
        }
    }
    QJsonObject result{{"text", selected}};
    if (a && b && a->firstSeq >= 0 && b->lastSeq >= 0) {
        result["from_seq"] = a->firstSeq;
        result["to_seq"] = b->lastSeq + 1;
        result["from_us"] = a->firstUs;
        result["to_us"] = b->lastUs;
        result["elapsed_us"] = b->lastMono - a->firstMono;
        result["clock"] = a->monotonic && b->monotonic ? "monotonic" : "wall";
    }
    return result;
}
qint64 TextStreamView::firstSequence() const {
    for (auto block = document()->begin(); block.isValid(); block = block.next())
        if (auto *a = dynamic_cast<LogAnchor *>(block.userData()))
            return a->firstSeq;
    return -1;
}
QString TextStreamView::topHistoryKey() const {
    auto block = firstVisibleBlock();
    if (auto *a = dynamic_cast<LogAnchor *>(block.userData())) {
        if (!a->historySpans.isEmpty())
            return historyPositionKey(a->historySpans.first().key, a->historySpans.first().offset);
        return a->historyKey;
    }
    return {};
}
QString TextStreamView::historyPositionKey(const QString &key, qsizetype offset) {
    return key + ":" + QString::number(offset);
}
bool TextStreamView::jumpToHistoryKey(const QString &key, bool top) {
    if (key.isEmpty())
        return false;
    const auto separator = key.lastIndexOf(':');
    bool positioned = false;
    const auto offset = key.mid(separator + 1).toLongLong(&positioned);
    const auto recordKey = key.left(separator);
    for (auto block = document()->begin(); block.isValid(); block = block.next()) {
        auto *a = dynamic_cast<LogAnchor *>(block.userData());
        int column = -1;
        if (a && a->historyKey == key)
            column = 0;
        else if (a && positioned)
            for (const auto &span : a->historySpans)
                if (span.key == recordKey && offset >= span.offset && offset <= span.offset + span.length) {
                    column = span.column + int(offset - span.offset);
                    break;
                }
        if (column >= 0) {
            following_ = false;
            QTextCursor cursor(block);
            cursor.setPosition(block.position() + column);
            setTextCursor(cursor);
            if (top)
                verticalScrollBar()->setValue(block.blockNumber());
            else
                centerCursor();
            return true;
        }
    }
    return false;
}
void TextStreamView::showHistory(const QJsonArray &records) {
    const QSignalBlocker signalBlocker(this);
    const bool updates = updatesEnabled();
    setUpdatesEnabled(false);
    clearStream();
    following_ = false;
    updating_ = true;
    QTextCursor edit(document());
    edit.beginEditBlock();
    for (const auto &v : records) {
        auto record = v.toObject();
        if (!record.contains("seq"))
            record["seq"] = -1;
        appendText(record.value("text").toString(), record);
    }
    edit.endEditBlock();
    verticalScrollBar()->setValue(0);
    updating_ = false;
    highlightMatches();
    setUpdatesEnabled(updates);
}
QJsonArray TextStreamView::searchResults() const {
    QJsonArray results;
    if (search_.pattern().isEmpty() || !search_.isValid())
        return results;
    for (auto block = document()->begin(); block.isValid() && results.size() < 1000; block = block.next())
        if (search_.match(block.text()).hasMatch()) {
            QJsonObject r{{"block", block.blockNumber()}, {"text", block.text().left(512)}};
            if (auto *a = dynamic_cast<LogAnchor *>(block.userData())) {
                r["seq"] = a->firstSeq;
                r["t_us"] = a->firstUs;
            }
            results.append(r);
        }
    return results;
}
void TextStreamView::jumpToBlock(int number) {
    auto block = document()->findBlockByNumber(number);
    if (!block.isValid())
        return;
    following_ = false;
    setTextCursor(QTextCursor(block));
    centerCursor();
}
bool TextStreamView::jumpToSequence(qint64 seq) {
    for (auto block = document()->begin(); block.isValid(); block = block.next()) {
        auto *a = dynamic_cast<LogAnchor *>(block.userData());
        if (a && a->firstSeq <= seq && a->lastSeq >= seq) {
            jumpToBlock(block.blockNumber());
            return true;
        }
    }
    return false;
}

void TextStreamView::clearStream() {
    updating_ = true;
    clear();
    resetStream();
    setExtraSelections({});
    searchTimer_.stop();
    following_ = true;
    updating_ = false;
}

void TextStreamView::markGap() {
    resetStream();
    appendText("\n[" + fc::text("Some received data was removed from the buffer") + "]\n");
}

void TextStreamView::followTail() {
    following_ = true;
    verticalScrollBar()->setValue(verticalScrollBar()->maximum());
}

void TextStreamView::setSearch(const QRegularExpression &expression) {
    search_ = expression;
    highlightMatches();
}

void TextStreamView::highlightMatches() {
    QList<QTextEdit::ExtraSelection> selections;
    if (search_.isValid() && !search_.pattern().isEmpty()) {
        QTextCursor cursor(document());
        for (int count = 0; count < 1000; ++count) {
            const auto match = document()->find(search_, cursor);
            if (match.isNull())
                break;
            QTextEdit::ExtraSelection selection;
            selection.cursor = match;
            selection.format.setBackground(palette().highlight());
            selection.format.setForeground(palette().highlightedText());
            selections.append(selection);
            cursor = match;
            if (!match.hasSelection() && !cursor.movePosition(QTextCursor::NextCharacter))
                break;
        }
    }
    setExtraSelections(selections);
}
