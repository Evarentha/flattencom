/*
 * flattencom - Serial Frame Table Model
 *
 * Stores bounded frame history and projects timestamps, bytes, decoding and source columns.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

#include "FramesModel.h"
#include "Style.h"
#include <QColor>
#include <QDateTime>
#include <QFontDatabase>
#include <QJsonDocument>

static qsizetype storageBytes(const QJsonObject &record) {
    return qMax(static_cast<qsizetype>(record.value("len").toInteger()),
                QJsonDocument(record).toJson(QJsonDocument::Compact).size());
}

FramesModel::FramesModel(QObject *parent) : QAbstractTableModel(parent) {}
int FramesModel::rowCount(const QModelIndex &parent) const {
    return parent.isValid() ? 0 : static_cast<int>(frames_.size());
}
int FramesModel::columnCount(const QModelIndex &parent) const { return parent.isValid() ? 0 : 6; }
QJsonObject FramesModel::frame(int row) const {
    return row >= 0 && row < rowCount() ? frames_[row] : QJsonObject{};
}
QVariant FramesModel::data(const QModelIndex &index, int role) const {
    if (!index.isValid() || index.row() >= rowCount())
        return {};
    const auto &record = frames_[index.row()];
    if (role == Qt::FontRole)
        return QFontDatabase::systemFont(QFontDatabase::FixedFont);
    if (role == Qt::ForegroundRole && index.column() == 2)
        return record.value("dir") == "rx" ? QColor("#42b5d5") : QColor("#53c999");
    if (role == Qt::ForegroundRole && index.column() == 4 && record.value("decoded_level") == "error")
        return QColor("#ed7985");
    if (role == Qt::ToolTipRole)
        return record.value("hex").toString();
    if (role != Qt::DisplayRole)
        return {};
    switch (index.column()) {
    case 0:
        return record.value("seq").toVariant();
    case 1:
        return relative_ ? QString::number(record.value("mono_us").toDouble() / 1e6, 'f', 6) + " s"
                         : QDateTime::fromMSecsSinceEpoch(record.value("t_us").toInteger() / 1000)
                               .toString("HH:mm:ss.zzz");
    case 2:
        return record.value("dir").toString().toUpper();
    case 3: {
        QString text = record.value(hex_ ? "hex" : "text").toString();
        if (!hex_) {
            text.replace('\r', QChar(0x240d)).replace('\n', QChar(0x240a)).replace('\0', QChar(0x2400));
        }
        return text.left(8192);
    }
    case 4:
        return record.value("decoded_text").toString().replace('\n', " ");
    case 5:
        return record.value("source").toString();
    default:
        return {};
    }
}
QVariant FramesModel::headerData(int section, Qt::Orientation orientation, int role) const {
    if (orientation != Qt::Horizontal || role != Qt::DisplayRole)
        return {};
    const QStringList headers{"#",
                              fc::text("Time"),
                              fc::text("Direction"),
                              fc::text("Data"),
                              fc::text("Decoded"),
                              fc::text("Source")};
    return headers.value(section);
}
void FramesModel::append(const QJsonArray &batch) {
    if (batch.isEmpty())
        return;
    beginInsertRows({}, rowCount(), rowCount() + static_cast<int>(batch.size()) - 1);
    for (const auto &item : batch) {
        auto record = item.toObject();
        bytes_ += storageBytes(record);
        frames_.push_back(std::move(record));
    }
    endInsertRows();
    int remove = 0;
    auto keepBytes = bytes_;
    while (remove < rowCount() && (rowCount() - remove > 100000 || keepBytes > 32 * 1024 * 1024)) {
        keepBytes -= storageBytes(frames_[remove]);
        ++remove;
    }
    if (remove) {
        beginRemoveRows({}, 0, remove - 1);
        for (int i = 0; i < remove; ++i)
            frames_.pop_front();
        bytes_ = keepBytes;
        discarded_ += remove;
        endRemoveRows();
    }
}
void FramesModel::clear() {
    beginResetModel();
    frames_.clear();
    bytes_ = 0;
    discarded_ = 0;
    endResetModel();
}
void FramesModel::setHex(bool hex) {
    hex_ = hex;
    if (rowCount())
        emit dataChanged(index(0, 3), index(rowCount() - 1, 3));
}
void FramesModel::setRelative(bool relative) {
    relative_ = relative;
    if (rowCount())
        emit dataChanged(index(0, 1), index(rowCount() - 1, 1));
}
