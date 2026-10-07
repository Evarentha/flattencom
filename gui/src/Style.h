/*
 * flattencom - Qt Presentation and Localization Interface
 *
 * Declares the types and operations for the qt presentation and localization component.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

#pragma once
#include <QString>
#include <QStringList>
class QApplication;
class QComboBox;
class QWidget;
namespace fc {
QString text(const char *english);
QString language();
void setLanguage(const QString &language);
void loadTranslations();
void applyStyle(QApplication &app, bool dark);
QString themeMode();
void setThemeMode(QApplication &app, const QString &mode);
void initializeTheme(QApplication &app);
QString stateDirectory();
QString socketName();
QString executable(const QString &name);
QString valueLabel(const QString &value);
void addValues(QComboBox *combo, const QStringList &values);
void selectValue(QComboBox *combo, const QString &value);
void retranslateUi(QWidget *root, const QString &previousLanguage);
} // namespace fc
