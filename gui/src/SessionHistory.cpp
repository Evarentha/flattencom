/*
 * flattencom - Session Disk History Index
 *
 * Indexes capture segments on a worker thread for bounded history windows and cancellable searches.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

#include "SessionHistory.h"
#include "Style.h"
#include <QDateTime>
#include <QDir>
#include <QFile>
#include <QFileInfo>
#include <QJsonDocument>
#include <QJsonObject>
#include <QPointer>
#include <algorithm>
#include <charconv>

namespace {
constexpr qint64 PageBytes = 64 * 1024;
constexpr int PageRecords = 1000;
constexpr qsizetype PageCharacters = 64 * 1024;
constexpr int PageBreaks = 4000;
constexpr qint64 MaxRecord = 4 * 1024 * 1024;
struct Context {
    qint64 seq = -1, us = 0, mono = 0;
    QString dir = "rx";
    QByteArray pending;
    bool cr = false;
    int escape = 0, escapeSize = 0;
    QByteArray timestamp;
};
// Index readable captures using byte metadata only. Timestamps and display
// objects are materialized for the requested window, not for every saved line.
qsizetype textHeader(QByteArrayView bytes, Context &state) {
    if (!bytes.startsWith('['))
        return -1;
    const auto space = bytes.indexOf(' ');
    if (space <= 1 || space + 5 >= bytes.size()) {
        state.dir = "event";
        return -1;
    }
    const auto direction = bytes.sliced(space + 1, 4);
    if (direction != "RX #" && direction != "TX #") {
        state.dir = "event";
        return -1;
    }
    const auto begin = space + 5;
    const auto end = bytes.indexOf(']', begin);
    qint64 seq = -1;
    if (end < 0 || bytes[begin] < '0' || bytes[begin] > '9' ||
        std::from_chars(bytes.data() + begin, bytes.data() + end, seq).ec != std::errc{}) {
        state.dir = "event";
        return -1;
    }
    state.seq = seq;
    state.dir = direction == "RX #" ? "rx" : "tx";
    const auto stamp = bytes.sliced(1, space - 1);
    if (stamp != QByteArrayView(state.timestamp)) {
        state.timestamp = QByteArray(stamp.data(), stamp.size());
        state.us = state.mono = 0;
    }
    return end + 1 < bytes.size() && bytes[end + 1] == ' ' ? end + 2 : end + 1;
}
struct Page {
    QString path;
    qint64 begin = 0, end = 0, firstSeq = -1, lastSeq = -1;
    Context context;
    // Large records can span pages. Offsets address normalized UTF-16 text;
    // context always precedes the first source record, so it can be decoded again.
    qsizetype firstCharacter = 0, lastCharacter = -1;
};
QString normalize(QString text, Context &state) {
    QString result;
    for (QChar c : text) {
        if (state.escape) {
            ++state.escapeSize;
            if (state.escapeSize > 4096)
                state.escape = 0;
            else {
                if (state.escape == 1)
                    state.escape = c == '[' ? 2 : c == ']' ? 3 : 0;
                else if (state.escape == 2 && c.unicode() >= 0x40 && c.unicode() <= 0x7e)
                    state.escape = 0;
                else if (state.escape == 3 && c == QChar(7))
                    state.escape = 0;
                else if (state.escape == 3 && c == QChar(27))
                    state.escape = 4;
                else if (state.escape == 4)
                    state.escape = c == '\\' ? 0 : 3;
                continue;
            }
        }
        if (c == QChar(27)) {
            state.escape = 1;
            state.escapeSize = 0;
            continue;
        }
        if (c == '\n' && state.cr) {
            state.cr = false;
            continue;
        }
        state.cr = c == '\r';
        if (c == '\r' || c == '\n')
            result += '\n';
        else if (c.unicode() < 32 && c != '\t')
            result += QChar(0x2400 + c.unicode());
        else
            result += c;
    }
    return result;
}
QString decode(const QByteArray &bytes, Context &state) {
    auto data = state.pending + bytes;
    state.pending.clear();
    qsizetype end = data.size();
    // Retain only a genuinely incomplete final UTF-8 sequence.
    for (qsizetype pos = qMax(qsizetype(0), end - 3); pos < end; ++pos) {
        const auto byte = static_cast<unsigned char>(data[pos]);
        const int size = byte >= 0xc2 && byte <= 0xdf   ? 2
                         : byte >= 0xe0 && byte <= 0xef ? 3
                         : byte >= 0xf0 && byte <= 0xf4 ? 4
                                                        : 0;
        if (size && end - pos < size) {
            bool valid = true;
            for (auto j = pos + 1; j < end; ++j)
                valid &= (static_cast<unsigned char>(data[j]) & 0xc0) == 0x80;
            if (valid) {
                state.pending = data.mid(pos);
                end = pos;
                break;
            }
        }
    }
    return normalize(QString::fromUtf8(data.constData(), end), state);
}
QJsonObject record(const QByteArray &bytes, const QString &path, qint64 offset, bool json, bool readable,
                   Context &state) {
    QString text;
    if (json) {
        auto frame = QJsonDocument::fromJson(bytes).object();
        if (frame.isEmpty())
            return {};
        const auto dir = frame.value("dir").toString();
        // Historical text view is RX-only, matching the live view.
        if (dir == "tx") {
            return QJsonObject{{"dir", "rx"},
                               {"text", QString("[TX %1] %2\n")
                                            .arg(frame.value("source").toString(),
                                                 frame.value("text")
                                                     .toString(QString::fromUtf8(QByteArray::fromHex(
                                                         frame.value("data").toString().toLatin1())))
                                                     .replace('\r', "\\r")
                                                     .replace('\n', "\\n"))},
                               {"seq", frame.value("seq")},
                               {"t_us", frame.value("t_us")},
                               {"mono_us", frame.value("mono_us")},
                               {"history_key", path + ":" + QString::number(offset)}};
        }
        if (dir != "rx")
            return {};
        state.seq = frame.value("seq").toInteger(-1);
        state.us = frame.value("t_us").toInteger();
        state.mono = frame.value("mono_us").toInteger();
        state.dir = dir;
        text = decode(
            QByteArray::fromHex(frame.value("data").toString(frame.value("hex").toString()).toLatin1()),
            state);
    } else if (readable) {
        const auto start = textHeader(bytes, state);
        if (start >= 0)
            text = decode(bytes.mid(state.dir == "rx" ? start : 0), state);
        else if ((state.dir == "rx" || state.dir == "tx") && state.seq >= 0)
            text = decode(bytes, state);
        if (!text.isEmpty() && !state.timestamp.isEmpty() && state.us == 0) {
            state.us = QDateTime::fromString(QString::fromUtf8(state.timestamp), Qt::ISODateWithMs)
                           .toMSecsSinceEpoch() *
                       1000;
            state.mono = state.us;
        }
    } else
        text = decode(bytes, state);
    if (text.isEmpty())
        return {};
    QJsonObject result{{"text", text}, {"dir", "rx"}, {"history_key", path + ":" + QString::number(offset)}};
    if (state.seq >= 0) {
        result["seq"] = state.seq;
        result["t_us"] = state.us;
        result["mono_us"] = state.mono;
        result["history_clock"] = json ? "monotonic" : "wall";
    }
    return result;
}
QStringList segments(const QString &base, bool readable) {
    const QFileInfo info(base);
    QDir dir = info.absoluteDir();
    QStringList paths;
    if (readable) {
        const auto extension = info.suffix();
        // Previous releases numbered rolling backups newest-first.
        for (int i = 4; i >= 1; --i) {
            auto p = dir.filePath(info.completeBaseName() + "-" + QString::number(i) + "." + extension);
            if (QFile::exists(p))
                paths << p;
        }
        if (QFile::exists(base))
            paths << base;
        const QRegularExpression part("^" + QRegularExpression::escape(info.completeBaseName()) +
                                      "-part-([0-9]+)\\." + QRegularExpression::escape(extension) + "$");
        QList<QPair<qulonglong, QString>> numbered;
        for (const auto &name :
             dir.entryList({info.completeBaseName() + "-part-*." + extension}, QDir::Files)) {
            auto m = part.match(name);
            if (m.hasMatch())
                numbered.append({m.captured(1).toULongLong(), dir.filePath(name)});
        }
        std::sort(numbered.begin(), numbered.end());
        for (const auto &p : numbered)
            paths << p.second;
    } else {
        for (int i = 4; i >= 1; --i)
            if (QFile::exists(base + "." + QString::number(i)))
                paths << base + "." + QString::number(i);
        if (QFile::exists(base))
            paths << base;
    }
    return paths;
}
} // namespace

struct SessionHistory::Index {
    QString base;
    QList<Page> pages;
    QHash<QString, qint64> scanned;
    QHash<QString, Context> contexts;
    QHash<QString, QByteArray> fingerprints;
    QHash<QString, QDateTime> modified;
    // Empty files and incomplete format signatures are not raw captures yet.
    bool formatKnown = false;
    bool json = false;
    bool readable = false, raw = false;
    bool refresh(const std::shared_ptr<std::atomic<quint64>> &cancel, quint64 token, QString &error) {
        if (!raw && !formatKnown) {
            readable = base.endsWith(".txt", Qt::CaseInsensitive);
            json = base.endsWith(".jsonl", Qt::CaseInsensitive);
            formatKnown = true;
            if (base.endsWith(".log", Qt::CaseInsensitive)) {
                // Probe legacy raw/JSONL logs as well as new readable captures.
                auto candidates = segments(base, true);
                const auto legacy = segments(base, false);
                if (candidates.isEmpty() || legacy.size() > 1)
                    candidates = legacy;
                formatKnown = false;
                bool pendingFormat = false;
                for (const auto &candidate : candidates) {
                    QFile probe(candidate);
                    if (probe.open(QIODevice::ReadOnly)) {
                        auto bytes = probe.read(8192);
                        // A normal 4096-byte frame already exceeds 8 KiB when
                        // hex-encoded. Probe its complete first JSONL record,
                        // bounded by the same limit used by the index reader.
                        const bool jsonPrefix = bytes.trimmed().startsWith('{');
                        const auto probeLimit = jsonPrefix ? MaxRecord : qint64(8192);
                        if (jsonPrefix && !bytes.contains('\n'))
                            bytes += probe.readLine(probeLimit - bytes.size() + 1);
                        const auto prefix = bytes.trimmed();
                        pendingFormat = true;
                        if (prefix.isEmpty())
                            continue;
                        const auto first =
                            prefix.left(prefix.indexOf('\n') < 0 ? prefix.size() : prefix.indexOf('\n'));
                        Context context;
                        const QByteArray english("flattencom capture");
                        const auto chinese = QString::fromUtf8("flattencom 收发记录").toUtf8();
                        readable = textHeader(first, context) >= 0 || prefix.startsWith(english) ||
                                   prefix.startsWith(chinese);
                        const auto object = QJsonDocument::fromJson(first).object();
                        json = object.contains("seq") && object.contains("dir");
                        const bool partial = !bytes.contains('\n') && bytes.size() < probeLimit &&
                                             (english.startsWith(prefix) || chinese.startsWith(prefix) ||
                                              prefix.startsWith('[') || prefix.startsWith('{'));
                        formatKnown = readable || json || !partial;
                        break;
                    }
                }
                if (!formatKnown) {
                    // No provisional raw pages: refresh will probe again after growth.
                    // Still report a missing file through the normal path below.
                    if (pendingFormat)
                        return true;
                }
            }
        }
        auto paths = segments(base, readable);
        if (paths.isEmpty()) {
            error = fc::text("No saved capture is available for this session.");
            return false;
        }
        // Retention or legacy rotation can remove/rewrite files without changing
        // the active pathname. Never use an index into a different generation.
        bool rebuild = false;
        for (auto it = scanned.begin(); it != scanned.end(); ++it) {
            QFile check(it.key());
            if (!paths.contains(it.key()) || !check.open(QIODevice::ReadOnly) || check.size() < it.value() ||
                (check.size() == it.value() &&
                 check.fileTime(QFileDevice::FileModificationTime) != modified.value(it.key())) ||
                check.read(fingerprints.value(it.key()).size()) != fingerprints.value(it.key())) {
                rebuild = true;
                break;
            }
        }
        if (rebuild) {
            pages.clear();
            scanned.clear();
            contexts.clear();
            fingerprints.clear();
            modified.clear();
            formatKnown = false;
            return refresh(cancel, token, error);
        }
        Context carry;
        for (const auto &path : paths) {
            if (cancel->load() != token)
                return false;
            QFile file(path);
            if (!file.open(QIODevice::ReadOnly)) {
                error = file.errorString();
                return false;
            }
            // Legacy rolling files may have been replaced. Re-index rather than
            // trusting stale offsets into a different capture segment.
            if (scanned.value(path) > file.size()) {
                pages.clear();
                scanned.clear();
                contexts.clear();
                return refresh(cancel, token, error);
            }
            auto offset = scanned.value(path);
            Context state = contexts.value(path, carry);
            qsizetype firstCharacter = 0;
            if (scanned.contains(path) && offset == file.size()) {
                carry = state;
                continue;
            }
            // Revisit the last partial page, including an unterminated final line.
            if (!pages.isEmpty() && pages.last().path == path) {
                const auto tail = pages.takeLast();
                offset = tail.begin;
                state = tail.context;
                firstCharacter = tail.firstCharacter;
            }
            file.seek(offset);
            Page page{path, offset, offset, -1, -1, state};
            page.firstCharacter = firstCharacter;
            int count = 0;
            qsizetype characters = 0;
            int breaks = 0;
            const auto limit = file.size();
            while (file.pos() < limit) {
                if (cancel->load() != token)
                    return false;
                const auto at = file.pos();
                const auto bytes = file.readLine(qMin(json ? MaxRecord : PageBytes, limit - at) + 1);
                if (bytes.isEmpty()) {
                    error = file.errorString();
                    return false;
                }
                // Readable transcripts normally already contain normalized text.
                // Bound their rendered size conservatively from bytes instead of
                // allocating a QString/QJsonObject and parsing a timestamp per
                // line. Unusual decoder state or oversized lines use the exact
                // materializing path below, including within-record pagination.
                if (readable && !firstCharacter && state.pending.isEmpty() && !state.escape &&
                    !bytes.contains('\x1b')) {
                    auto next = state;
                    const auto header = textHeader(bytes, next);
                    const auto payload =
                        QByteArrayView(bytes).sliced(header >= 0 && next.dir == "rx" ? header : 0);
                    // Every UTF-16 unit needs at least one source byte. E2 is
                    // also a conservative count of U+2028/U+2029 separators.
                    const auto size = payload.size();
                    const auto lineBreaks =
                        payload.count('\n') + payload.count('\r') + payload.count(char(0xe2));
                    if ((next.dir == "rx" || next.dir == "tx") && next.seq >= 0 && size <= PageCharacters &&
                        lineBreaks <= PageBreaks) {
                        if (page.end > page.begin &&
                            (characters + size > PageCharacters || breaks + lineBreaks > PageBreaks)) {
                            pages.append(page);
                            page = Page{path, at, at, -1, -1, state};
                            count = 0;
                            characters = 0;
                            breaks = 0;
                        }
                        state = next;
                        if (!payload.isEmpty() && static_cast<unsigned char>(payload.back()) >= 0x80)
                            decode(bytes.right(4), state);
                        else if (!payload.isEmpty())
                            state.cr = payload.back() == '\r';
                        if (page.firstSeq < 0)
                            page.firstSeq = state.seq;
                        page.lastSeq = state.seq;
                        page.end = file.pos();
                        page.lastCharacter = -1;
                        characters += size;
                        breaks += lineBreaks;
                        ++count;
                        if (characters >= PageCharacters || breaks >= PageBreaks ||
                            page.end - page.begin >= PageBytes || count >= PageRecords) {
                            pages.append(page);
                            page = Page{path, file.pos(), file.pos(), -1, -1, state};
                            count = 0;
                            characters = 0;
                            breaks = 0;
                        }
                        continue;
                    }
                }
                const auto before = state;
                const auto row = record(bytes, path, at, json, readable, state);
                const auto text = row.value("text").toString();
                const auto seq = row.value("seq").toInteger(-1);
                auto position = firstCharacter;
                firstCharacter = 0;
                do {
                    const auto start = position;
                    while (position < text.size() && characters < PageCharacters && breaks < PageBreaks) {
                        const auto ch = text[position++];
                        ++characters;
                        if (ch == '\n' || ch == QChar::ParagraphSeparator || ch == QChar::LineSeparator)
                            ++breaks;
                    }
                    if (position < text.size() && text[position].isLowSurrogate()) {
                        ++position;
                        ++characters;
                    }
                    if (seq >= 0) {
                        if (page.firstSeq < 0)
                            page.firstSeq = seq;
                        page.lastSeq = seq;
                    }
                    page.end = file.pos();
                    page.lastCharacter = position;
                    ++count;
                    if (characters >= PageCharacters || breaks >= PageBreaks ||
                        page.end - page.begin >= PageBytes || count >= PageRecords) {
                        pages.append(page);
                        if (position < text.size()) {
                            page = Page{path, at, at, -1, -1, before};
                            page.firstCharacter = position;
                        } else
                            page = Page{path, file.pos(), file.pos(), -1, -1, state};
                        count = 0;
                        characters = 0;
                        breaks = 0;
                    }
                    Q_ASSERT(position > start || position == text.size());
                } while (position < text.size());
            }
            if (page.end > page.begin)
                pages.append(page);
            scanned[path] = limit;
            contexts[path] = state;
            file.seek(0);
            fingerprints[path] = file.read(qMin(qint64(256), limit));
            modified[path] = file.fileTime(QFileDevice::FileModificationTime);
            carry = state;
        }
        return true;
    }
    QJsonArray read(int first, QString &error) const {
        QJsonArray rows;
        for (int i = first; i < qMin(first + 3, pages.size()); ++i) {
            const auto &page = pages[i];
            QFile file(page.path);
            if (!file.open(QIODevice::ReadOnly) || file.size() < page.end || !file.seek(page.begin)) {
                error = fc::text("Saved log changed or is unavailable.");
                return {};
            }
            file.seek(0);
            if ((file.size() == scanned.value(page.path) &&
                 file.fileTime(QFileDevice::FileModificationTime) != modified.value(page.path)) ||
                file.read(fingerprints.value(page.path).size()) != fingerprints.value(page.path)) {
                error = fc::text("Saved log changed or is unavailable.");
                return {};
            }
            file.seek(page.begin);
            auto state = page.context;
            while (file.pos() < page.end) {
                auto at = file.pos();
                auto bytes = file.readLine(qMin(json ? MaxRecord : PageBytes, page.end - at) + 1);
                if (bytes.isEmpty()) {
                    error = file.errorString();
                    return {};
                }
                auto row = record(bytes, page.path, at, json, readable, state);
                if (!row.isEmpty()) {
                    const auto text = row.value("text").toString();
                    const auto start = at == page.begin ? page.firstCharacter : 0;
                    const auto end =
                        file.pos() == page.end && page.lastCharacter >= 0 ? page.lastCharacter : text.size();
                    row["text"] = text.mid(start, end - start);
                    row["history_offset"] = qint64(start);
                    rows.append(row);
                }
            }
        }
        return rows;
    }
};

SessionHistory::SessionHistory(QObject *parent)
    : QObject(parent), index_(std::make_shared<Index>()), worker_(new QObject),
      indexGeneration_(std::make_shared<std::atomic<quint64>>(0)),
      searchGeneration_(std::make_shared<std::atomic<quint64>>(0)) {
    worker_->moveToThread(&thread_);
    connect(&thread_, &QThread::finished, worker_, &QObject::deleteLater);
    thread_.start();
}
SessionHistory::~SessionHistory() {
    ++*indexGeneration_;
    ++*searchGeneration_;
    thread_.quit();
    thread_.wait();
}
void SessionHistory::open(const QString &path, bool raw) {
    if (path.isEmpty())
        return;
    if (path_ != path || raw_ != raw) {
        path_ = path;
        raw_ = raw;
        pages_ = 0;
        ++fileGeneration_;
        index_ = std::make_shared<Index>();
        index_->base = path;
        index_->raw = raw;
    }
    ++*searchGeneration_;
    const auto token = ++*indexGeneration_;
    const auto fileGeneration = fileGeneration_;
    auto index = index_;
    auto cancel = indexGeneration_;
    QPointer<SessionHistory> self(this);
    QMetaObject::invokeMethod(worker_, [self, index, cancel, token, fileGeneration] {
        QString error;
        auto next = *index;
        if (!next.refresh(cancel, token, error) && error.isEmpty())
            return;
        if (error.isEmpty())
            *index = std::move(next);
        const auto count = index->pages.size();
        if (self)
            QMetaObject::invokeMethod(
                self,
                [self, count, error, fileGeneration, cancel, token] {
                    if (!self || self->fileGeneration_ != fileGeneration || cancel->load() != token)
                        return;
                    self->pages_ = count;
                    if (!error.isEmpty())
                        emit self->failed(error);
                    else
                        emit self->indexed(count);
                },
                Qt::QueuedConnection);
    });
}
void SessionHistory::window(int firstPage, quint64 generation) {
    auto index = index_;
    auto fileGeneration = fileGeneration_;
    QPointer<SessionHistory> self(this);
    QMetaObject::invokeMethod(worker_, [self, index, firstPage, generation, fileGeneration] {
        QString error;
        const int first = qBound(0, firstPage, qMax(0, int(index->pages.size()) - 3));
        auto rows = index->read(first, error);
        const int count = index->pages.size();
        if (self)
            QMetaObject::invokeMethod(
                self,
                [self, first, count, rows, error, generation, fileGeneration] {
                    if (!self || self->fileGeneration_ != fileGeneration)
                        return;
                    self->pages_ = count;
                    if (!error.isEmpty())
                        emit self->failed(error);
                    else
                        emit self->windowReady(first, count, rows, generation);
                },
                Qt::QueuedConnection);
    });
}
void SessionHistory::locate(qint64 seq, quint64 generation) {
    auto index = index_;
    auto fileGeneration = fileGeneration_;
    QPointer<SessionHistory> self(this);
    QMetaObject::invokeMethod(worker_, [self, index, seq, generation, fileGeneration] {
        int chosen = qMax(0, int(index->pages.size()) - 3);
        bool found = seq < 0;
        if (seq >= 0) {
            for (int i = 0; i < index->pages.size(); ++i)
                if (index->pages[i].lastSeq >= seq) {
                    chosen = qMax(0, i - 1);
                    found = true;
                    break;
                }
        }
        QString error;
        if (!found && !index->pages.isEmpty() && index->pages.last().lastSeq >= 0)
            error = fc::text("This position has not been saved yet. Try again after capture flushes.");
        auto rows = error.isEmpty() ? index->read(chosen, error) : QJsonArray{};
        const int count = index->pages.size();
        if (self)
            QMetaObject::invokeMethod(
                self,
                [self, chosen, count, rows, error, generation, fileGeneration] {
                    if (!self || self->fileGeneration_ != fileGeneration)
                        return;
                    self->pages_ = count;
                    if (!error.isEmpty())
                        emit self->failed(error);
                    else
                        emit self->windowReady(chosen, count, rows, generation);
                },
                Qt::QueuedConnection);
    });
}
void SessionHistory::cancelSearch() { ++*searchGeneration_; }
void SessionHistory::search(const QRegularExpression &expression) {
    const auto token = ++*searchGeneration_;
    auto cancel = searchGeneration_;
    auto index = index_;
    QPointer<SessionHistory> self(this);
    QMetaObject::invokeMethod(worker_, [self, index, cancel, token, expression] {
        QJsonArray matches;
        QString error;
        auto next = *index;
        if (!next.refresh(cancel, token, error)) {
            if (self && !error.isEmpty())
                QMetaObject::invokeMethod(
                    self,
                    [self, cancel, token, error] {
                        if (self && cancel->load() == token)
                            emit self->failed(error);
                    },
                    Qt::QueuedConnection);
            return;
        }
        *index = std::move(next);
        constexpr qsizetype SearchWindow = 16384;
        constexpr qsizetype SearchOverlap = 1024;
        struct Origin {
            QString key;
            qint64 seq;
            int page;
            qsizetype offset, length;
        };
        QString carry;
        QString preceding;
        QList<Origin> origins;
        bool limited = false, lineMatched = false;
        auto matchWindow = [&](const QString &following = QString()) {
            if (lineMatched || origins.isEmpty())
                return;
            // Adjacent real characters guard artificial subject edges. Start
            // after the prefix and reject matches consuming the suffix, so
            // subject/line anchors can only match real line boundaries.
            const auto subject = preceding + carry + following;
            auto candidates = expression.globalMatch(subject, preceding.size());
            QRegularExpressionMatch match;
            while (candidates.hasNext()) {
                const auto candidate = candidates.next();
                if (candidate.capturedEnd() <= preceding.size() + carry.size()) {
                    match = candidate;
                    break;
                }
            }
            if (!match.hasMatch())
                return;
            const auto start = match.capturedStart() - preceding.size();
            auto position = start;
            for (qsizetype i = 0; i < origins.size(); ++i) {
                const auto &origin = origins[i];
                if (position < origin.length || i + 1 == origins.size()) {
                    matches.append(QJsonObject{{"text", carry.mid(qMax(qsizetype(0), start - 80)).left(512)},
                                               {"history_key", origin.key},
                                               {"history_offset", qint64(origin.offset + position)},
                                               {"seq", origin.seq},
                                               {"page", origin.page}});
                    lineMatched = true;
                    break;
                }
                position -= origin.length;
            }
        };
        auto retainOverlap = [&] {
            auto remove = carry.size() - SearchOverlap;
            // Never split a UTF-16 surrogate pair at a window boundary.
            if (carry[remove].isLowSurrogate())
                --remove;
            const auto contextSize = carry[remove - 1].isLowSurrogate() ? 2 : 1;
            preceding = carry.mid(remove - contextSize, contextSize);
            carry.remove(0, remove);
            while (!origins.isEmpty() && remove >= origins.first().length) {
                remove -= origins.first().length;
                origins.removeFirst();
            }
            if (!origins.isEmpty()) {
                origins.first().offset += remove;
                origins.first().length -= remove;
            }
        };
        for (int i = 0; i < index->pages.size(); i += 3) {
            if (cancel->load() != token)
                return;
            auto rows = index->read(i, error);
            if (!error.isEmpty())
                break;
            for (const auto &v : rows) {
                const auto r = v.toObject();
                const auto text = r.value("text").toString();
                for (qsizetype offset = 0; offset < text.size() && matches.size() < 1000;) {
                    if (cancel->load() != token)
                        return;
                    if (text[offset] == '\n') {
                        if (origins.isEmpty())
                            origins.append({r.value("history_key").toString(), r.value("seq").toInteger(-1),
                                            i, r.value("history_offset").toInteger() + offset, 0});
                        matchWindow();
                        carry.clear();
                        preceding.clear();
                        origins.clear();
                        lineMatched = false;
                        ++offset;
                        continue;
                    }
                    if (carry.size() >= SearchWindow) {
                        limited = true;
                        matchWindow(text.mid(offset, text[offset].isHighSurrogate() ? 2 : 1));
                        retainOverlap();
                    }
                    const auto newline = text.indexOf('\n', offset);
                    auto count =
                        qMin(SearchWindow - carry.size(), (newline < 0 ? text.size() : newline) - offset);
                    if (offset + count < text.size() && text[offset + count].isLowSurrogate())
                        ++count;
                    carry += QStringView(text).mid(offset, count);
                    origins.append({r.value("history_key").toString(), r.value("seq").toInteger(-1), i,
                                    r.value("history_offset").toInteger() + offset, count});
                    offset += count;
                }
                if (matches.size() >= 1000)
                    break;
            }
            if (matches.size() >= 1000)
                break;
        }
        if (matches.size() < 1000 && !carry.isEmpty())
            matchWindow();
        if (!error.isEmpty()) {
            if (self)
                QMetaObject::invokeMethod(
                    self,
                    [self, cancel, token, error] {
                        if (self && cancel->load() == token)
                            emit self->failed(error);
                    },
                    Qt::QueuedConnection);
            return;
        }
        if (self && cancel->load() == token)
            QMetaObject::invokeMethod(
                self,
                [self, cancel, token, matches, limited] {
                    if (self && cancel->load() == token)
                        emit self->matchesReady(matches, !limited && matches.size() < 1000);
                },
                Qt::QueuedConnection);
    });
}
