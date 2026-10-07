/*
 * flattencom - Serial Transfer Rate Chart
 *
 * Plots bounded RX and TX rate samples using the current application palette.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

#include "RateChart.h"
#include <QPainter>
#include <QPainterPath>
#include <algorithm>

void RateChart::add(double rx, double tx) {
    samples_.emplace_back(rx, tx);
    if (samples_.size() > 120)
        samples_.pop_front();
    update();
}
void RateChart::paintEvent(QPaintEvent *) {
    QPainter painter(this);
    painter.setRenderHint(QPainter::Antialiasing);
    const QRectF area = rect().adjusted(12, 24, -12, -22);
    painter.setPen(QPen(palette().mid().color(), 0.5));
    for (int y = 0; y < 4; ++y)
        painter.drawLine(QPointF(area.left(), area.top() + area.height() * y / 3),
                         QPointF(area.right(), area.top() + area.height() * y / 3));
    double maximum = 1;
    for (auto [rx, tx] : samples_)
        maximum = std::max({maximum, rx, tx});
    for (int channel = 0; channel < 2; ++channel) {
        QPainterPath path;
        for (size_t i = 0; i < samples_.size(); ++i) {
            const auto value = channel == 0 ? samples_[i].first : samples_[i].second;
            QPointF point(area.left() + area.width() * static_cast<double>(i) / 119,
                          area.bottom() - area.height() * value / maximum);
            if (!i)
                path.moveTo(point);
            else
                path.lineTo(point);
        }
        painter.setPen(QPen(channel == 0 ? QColor("#42b5d5") : QColor("#53c999"), 2));
        painter.drawPath(path);
    }
    painter.setPen(palette().text().color());
    painter.drawText(12, 16, QString("RX / TX    %1 B/s").arg(maximum, 0, 'f', 0));
    painter.setPen(palette().placeholderText().color());
    painter.drawText(12, height() - 5, "B/s");
}
