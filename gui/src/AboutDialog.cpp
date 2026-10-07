/*
 * flattencom - About Dialog
 *
 * Displays the final wordmark, application attribution and license links.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

#include "AboutDialog.h"
#include "Style.h"
#include <QCoreApplication>
#include <QDialogButtonBox>
#include <QEvent>
#include <QLabel>
#include <QPixmap>
#include <QVBoxLayout>

AboutDialog::AboutDialog(QWidget *parent)
    : QDialog(parent), logo_(new QLabel(this)), details_(new QLabel(this)) {
    setObjectName("aboutDialog");
    setWindowTitle(fc::text("About") + " flattencom");
    setAttribute(Qt::WA_DeleteOnClose);
    auto *layout = new QVBoxLayout(this);
    layout->setContentsMargins(28, 24, 28, 20);
    layout->setSpacing(14);
    logo_->setObjectName("aboutLogo");
    logo_->setAccessibleName("flattencom");
    logo_->setAlignment(Qt::AlignCenter);
    logo_->setFixedSize(360, 127);
    layout->addWidget(logo_, 0, Qt::AlignHCenter);
    auto *details = details_;
    details->setObjectName("aboutDetails");
    details->setWordWrap(true);
    details->setOpenExternalLinks(true);
    details->setTextInteractionFlags(Qt::TextBrowserInteraction);
    details->setText(QString("<p>%5</p><p>%1 %2</p>"
                             "<p><b>%3</b><br>"
                             "<a href=\"https://www.gnu.org/licenses/gpl-3.0.html\">GPL-3.0-or-later</a></p>"
                             "<p>Copyright (C) 2026 Evarentha</p><p><small>%4</small></p>")
                         .arg(fc::text("Version").toHtmlEscaped(),
                              QCoreApplication::applicationVersion().toHtmlEscaped(),
                              fc::text("License").toHtmlEscaped(),
                              fc::text("This program comes with no warranty. You may redistribute and "
                                       "modify it under GPL version 3 or any later version.")
                                  .toHtmlEscaped(),
                              fc::text("Serial Workbench").toHtmlEscaped()));
    layout->addWidget(details);
    detailsText_ = details->text();
    auto *buttons = new QDialogButtonBox(QDialogButtonBox::Close, this);
    connect(buttons, &QDialogButtonBox::rejected, this, &QDialog::close);
    layout->addWidget(buttons);
    resize(416, 400);
    updateLogo();
}

void AboutDialog::updateLogo() {
    const bool dark = palette().color(QPalette::Window).lightness() < 128;
    // Rich-text link formats otherwise keep the colour used at first layout.
    auto content = detailsText_;
    content.replace("<a href", QString("<a style=\"color:%1\" href").arg(dark ? "#46b3c2" : "#176779"));
    details_->setText(content);
    QPixmap artwork(dark ? ":/flattencom/branding/logo-dark.png" : ":/flattencom/branding/logo-light.png");
    if (!artwork.isNull()) {
        // Retain the full raster resolution for fractional and high-DPI screens.
        artwork.setDevicePixelRatio(qreal(artwork.width()) / logo_->width());
        logo_->setPixmap(artwork);
    }
}

void AboutDialog::changeEvent(QEvent *event) {
    QDialog::changeEvent(event);
    if (event->type() == QEvent::PaletteChange)
        updateLogo();
}
