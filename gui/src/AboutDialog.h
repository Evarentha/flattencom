/*
 * flattencom - About Dialog Interface
 *
 * Declares the branded application information dialog with theme-aware artwork.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

#pragma once
#include <QDialog>
class QLabel;

class AboutDialog final : public QDialog {
  public:
    explicit AboutDialog(QWidget *parent = nullptr);

  protected:
    void changeEvent(QEvent *event) override;

  private:
    void updateLogo();
    QLabel *logo_;
    QLabel *details_;
    QString detailsText_;
};
