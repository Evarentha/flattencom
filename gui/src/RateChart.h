/*
 * flattencom - Serial Transfer Rate Chart Interface
 *
 * Declares the types and operations for the serial transfer rate chart component.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

#pragma once
#include <QWidget>
#include <deque>
class RateChart final : public QWidget {
  public:
    explicit RateChart(QWidget *parent = nullptr) : QWidget(parent) { setMinimumHeight(120); }
    void add(double rx, double tx);

  protected:
    void paintEvent(QPaintEvent *) override;

  private:
    std::deque<std::pair<double, double>> samples_;
};
