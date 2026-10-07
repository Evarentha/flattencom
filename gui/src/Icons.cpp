/*
 * flattencom - Application Vector Icons
 *
 * Draws palette-aware toolbar artwork independently of desktop icon themes.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

#include "Icons.h"
#include <QApplication>
#include <QIconEngine>
#include <QPainter>
#include <QPainterPath>
#include <QPalette>

namespace {
// Application-owned vector artwork. Colors follow the application palette;
// geometry is independent of the desktop's icon theme and scales with DPI.
class Engine final : public QIconEngine {
  public:
    explicit Engine(fc::Icon kind) : kind_(kind) {}
    QIconEngine *clone() const override { return new Engine(kind_); }
    void paint(QPainter *p, const QRect &rect, QIcon::Mode mode, QIcon::State) override {
        p->save();
        p->setRenderHint(QPainter::Antialiasing);
        const auto color = QApplication::palette().color(
            mode == QIcon::Disabled ? QPalette::Disabled : QPalette::Active, QPalette::ButtonText);
        const qreal side = qMin(rect.width(), rect.height());
        p->translate(rect.x() + (rect.width() - side) / 2, rect.y() + (rect.height() - side) / 2);
        p->scale(side / 24, side / 24);
        p->setPen(QPen(color, 1.7, Qt::SolidLine, Qt::RoundCap, Qt::RoundJoin));
        p->setBrush(Qt::NoBrush);
        switch (kind_) {
        case fc::Icon::Open: {
            QPainterPath path;
            path.moveTo(3, 19);
            path.lineTo(3, 5);
            path.lineTo(9, 5);
            path.lineTo(12, 8);
            path.lineTo(20, 8);
            path.lineTo(20, 11);
            path.moveTo(3, 19);
            path.lineTo(6, 11);
            path.lineTo(22, 11);
            path.lineTo(19, 19);
            path.closeSubpath();
            p->drawPath(path);
            break;
        }
        case fc::Icon::Configure:
            p->drawLine(5, 3, 5, 7);
            p->drawLine(5, 11, 5, 21);
            p->drawEllipse(QPointF(5, 9), 2, 2);
            p->drawLine(12, 3, 12, 13);
            p->drawLine(12, 17, 12, 21);
            p->drawEllipse(QPointF(12, 15), 2, 2);
            p->drawLine(19, 3, 19, 6);
            p->drawLine(19, 10, 19, 21);
            p->drawEllipse(QPointF(19, 8), 2, 2);
            break;
        case fc::Icon::Save: {
            QPainterPath path;
            path.moveTo(4, 3);
            path.lineTo(17, 3);
            path.lineTo(21, 7);
            path.lineTo(21, 21);
            path.lineTo(3, 21);
            path.lineTo(3, 3);
            path.closeSubpath();
            p->drawPath(path);
            p->drawRect(QRectF(7, 3, 9, 6));
            p->drawRect(QRectF(7, 14, 10, 7));
            break;
        }
        case fc::Icon::Refresh:
            p->drawArc(QRectF(4, 4, 16, 16), 35 * 16, 280 * 16);
            p->drawLine(20, 3, 20, 9);
            p->drawLine(20, 9, 14, 9);
            break;
        case fc::Icon::Insert:
            p->drawLine(12, 3, 12, 16);
            p->drawLine(7, 11, 12, 16);
            p->drawLine(17, 11, 12, 16);
            p->drawLine(4, 17, 4, 21);
            p->drawLine(4, 21, 20, 21);
            p->drawLine(20, 21, 20, 17);
            break;
        case fc::Icon::Delete:
            p->drawLine(3, 6, 21, 6);
            p->drawRect(QRectF(9, 3, 6, 3));
            p->drawLine(6, 6, 7, 21);
            p->drawLine(7, 21, 17, 21);
            p->drawLine(17, 21, 18, 6);
            p->drawLine(10, 10, 10, 17);
            p->drawLine(14, 10, 14, 17);
            break;
        }
        p->restore();
    }
    QPixmap pixmap(const QSize &size, QIcon::Mode mode, QIcon::State state) override {
        QPixmap result(size);
        result.fill(Qt::transparent);
        QPainter painter(&result);
        paint(&painter, QRect(QPoint(), size), mode, state);
        return result;
    }

  private:
    fc::Icon kind_;
};
} // namespace
QIcon fc::icon(Icon kind) { return QIcon(new Engine(kind)); }
