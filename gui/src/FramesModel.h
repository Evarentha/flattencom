/*
 * flattencom - Serial Frame Table Model Interface
 *
 * Declares the types and operations for the serial frame table model component.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

#pragma once
#include <QAbstractTableModel>
#include <QJsonArray>
#include <QJsonObject>
#include <deque>

class FramesModel final : public QAbstractTableModel {
    Q_OBJECT
  public:
    explicit FramesModel(QObject *parent = nullptr);
    int rowCount(const QModelIndex &parent = {}) const override;
    int columnCount(const QModelIndex &parent = {}) const override;
    QVariant data(const QModelIndex &index, int role = Qt::DisplayRole) const override;
    QVariant headerData(int section, Qt::Orientation orientation, int role) const override;
    void append(const QJsonArray &batch);
    void clear();
    void setHex(bool hex);
    void setRelative(bool relative);
    QJsonObject frame(int row) const;
    quint64 discarded() const { return discarded_; }

  private:
    std::deque<QJsonObject> frames_;
    qsizetype bytes_ = 0;
    bool hex_ = false;
    bool relative_ = false;
    quint64 discarded_ = 0;
};
