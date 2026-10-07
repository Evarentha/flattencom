/*
 * flattencom - Qt Presentation and Localization
 *
 * Provides themes, translated UI labels, protocol-value labels and application path discovery.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

#include "Style.h"
#include <QAbstractButton>
#include <QAction>
#include <QApplication>
#include <QComboBox>
#include <QDir>
#include <QFile>
#include <QFont>
#include <QGroupBox>
#include <QHash>
#include <QJsonDocument>
#include <QJsonObject>
#include <QLabel>
#include <QLibraryInfo>
#include <QLineEdit>
#include <QLocale>
#include <QPainter>
#include <QPalette>
#include <QPlainTextEdit>
#include <QProxyStyle>
#include <QSettings>
#include <QSignalBlocker>
#include <QStandardPaths>
#include <QStyleFactory>
#include <QStyleHints>
#include <QStyleOptionComboBox>
#include <QStyleOptionSpinBox>
#include <QTabWidget>
#include <QTextEdit>
#include <QTranslator>
#include <QTreeWidget>

static void initializeResources() { Q_INIT_RESOURCE(resources); }
namespace {
// Own the complete control drawing so platform bevels do not leak through a
// partially styled combo/spin box. Keep Qt's input handling and hit testing.
class WorkbenchStyle final : public QProxyStyle {
  public:
    WorkbenchStyle() : QProxyStyle(QStyleFactory::create("Fusion")) {}

    QRect subControlRect(ComplexControl control, const QStyleOptionComplex *option, SubControl part,
                         const QWidget *widget = nullptr) const override {
        const auto r = option->rect;
        QRect logical;
        if (control == CC_ComboBox) {
            if (part == SC_ComboBoxArrow)
                logical = QRect(r.right() - 23, r.top() + 1, 23, r.height() - 2);
            else if (part == SC_ComboBoxEditField)
                logical = r.adjusted(5, 2, -27, -2);
        } else if (control == CC_SpinBox) {
            const auto *spin = qstyleoption_cast<const QStyleOptionSpinBox *>(option);
            const bool buttons = spin && spin->buttonSymbols != QAbstractSpinBox::NoButtons;
            const int height = (r.height() - 2) / 2;
            if (part == SC_SpinBoxUp)
                logical = buttons ? QRect(r.right() - 23, r.top() + 1, 23, height) : QRect();
            else if (part == SC_SpinBoxDown)
                logical = buttons ? QRect(r.right() - 23, r.top() + 1 + height, 23, r.height() - 2 - height)
                                  : QRect();
            else if (part == SC_SpinBoxEditField)
                logical = r.adjusted(5, 2, buttons ? -27 : -5, -2);
            if (part == SC_SpinBoxUp || part == SC_SpinBoxDown)
                return visualRect(option->direction, r, logical);
        }
        return logical.isValid() ? visualRect(option->direction, r, logical)
                                 : QProxyStyle::subControlRect(control, option, part, widget);
    }

    QSize sizeFromContents(ContentsType type, const QStyleOption *option, const QSize &contents,
                           const QWidget *widget = nullptr) const override {
        auto size = QProxyStyle::sizeFromContents(type, option, contents, widget);
        if (type == CT_ComboBox || type == CT_SpinBox) {
            size.setHeight(qMax(size.height(), option->fontMetrics.height() + 10));
            size.rwidth() += 10;
        }
        return size;
    }

    void drawComplexControl(ComplexControl control, const QStyleOptionComplex *option, QPainter *painter,
                            const QWidget *widget = nullptr) const override {
        if (control != CC_ComboBox && control != CC_SpinBox) {
            QProxyStyle::drawComplexControl(control, option, painter, widget);
            return;
        }
        const auto &palette = option->palette;
        const bool enabled = option->state & State_Enabled;
        const bool dark = palette.color(QPalette::Window).lightness() < 128;
        const QColor border(dark ? "#2b3b50" : "#cfdae6");
        const QColor accent("#46b3c2");
        const auto group = enabled ? QPalette::Active : QPalette::Disabled;
        painter->save();
        painter->setRenderHint(QPainter::Antialiasing);
        painter->setBrush(palette.color(group, QPalette::Base));
        painter->setPen(enabled && (option->state & (State_HasFocus | State_MouseOver)) ? accent : border);
        painter->drawRoundedRect(QRectF(option->rect).adjusted(0.5, 0.5, -0.5, -0.5), 2, 2);
        auto button = [&](SubControl part, bool up, bool available, bool plusMinus) {
            const auto rect = subControlRect(control, option, part, widget);
            if (rect.isEmpty() || !(option->subControls & part))
                return;
            const bool active = enabled && available;
            if (active && (option->activeSubControls & part) && (option->state & State_MouseOver))
                painter->fillRect(rect,
                                  palette.color(option->state & State_Sunken ? QPalette::Highlight
                                                                             : QPalette::AlternateBase));
            painter->setPen(border);
            const int edge = option->direction == Qt::RightToLeft ? rect.right() : rect.left();
            painter->drawLine(edge, rect.top(), edge, rect.bottom());
            painter->setPen(
                QPen(palette.color(active ? QPalette::Active : QPalette::Disabled, QPalette::ButtonText), 1.4,
                     Qt::SolidLine, Qt::RoundCap, Qt::RoundJoin));
            const QPointF center = QRectF(rect).center();
            if (plusMinus) {
                painter->drawLine(center + QPointF(-3, 0), center + QPointF(3, 0));
                if (up)
                    painter->drawLine(center + QPointF(0, -3), center + QPointF(0, 3));
            } else {
                const qreal sign = up ? -1 : 1;
                const QPolygonF arrow{center + QPointF(-3, -sign * 1.5), center + QPointF(0, sign * 1.5),
                                      center + QPointF(3, -sign * 1.5)};
                painter->setBrush(Qt::NoBrush);
                painter->drawPolyline(arrow);
            }
        };
        if (control == CC_ComboBox)
            button(SC_ComboBoxArrow, false, true, false);
        else if (const auto *spin = qstyleoption_cast<const QStyleOptionSpinBox *>(option)) {
            const bool symbols = spin->buttonSymbols == QAbstractSpinBox::PlusMinus;
            button(SC_SpinBoxUp, true, spin->stepEnabled & QAbstractSpinBox::StepUpEnabled, symbols);
            button(SC_SpinBoxDown, false, spin->stepEnabled & QAbstractSpinBox::StepDownEnabled, symbols);
        }
        painter->restore();
    }
};
} // namespace
// Standard dialog controls must remain localized even when system Qt catalogs
// are not installed (for example a minimal Linux deployment).
class StandardTranslations final : public QTranslator {
  public:
    QString translate(const char *context, const char *source, const char *, int) const override {
        if (fc::language() != "zh" || !QString::fromLatin1(context).startsWith("Q"))
            return {};
        const QStringList keys{
            "OK",    "Save",  "Cancel",           "Close",           "Open",           "Yes", "No",
            "Apply", "Reset", "Restore Defaults", "Show Details...", "Hide Details..."};
        auto key = QString::fromUtf8(source);
        key.remove('&');
        return keys.contains(key) ? fc::text(key.toUtf8().constData()) : QString{};
    }
    bool isEmpty() const override { return false; }
};
QString fc::language() {
    const auto configured =
        QSettings().value("language", qEnvironmentVariable("FLATTENCOM_LANG", "en")).toString();
    return configured.startsWith("zh", Qt::CaseInsensitive) ? "zh" : "en";
}
namespace {
// History workers also translate diagnostics. Each thread owns its rendering
// cache; the UI's source-pointer bookkeeping is never mutated by a worker.
thread_local QHash<const QChar *, QString> translationSources;
thread_local QHash<QString, QString> renderedTranslations;
} // namespace
QString fc::text(const char *english) {
    static const auto catalog = [] {
        initializeResources();
        QFile file(":/flattencom/zh_CN.json");
        if (!file.open(QIODevice::ReadOnly))
            return QHash<QString, QString>{};
        const auto object = QJsonDocument::fromJson(file.readAll()).object();
        QHash<QString, QString> translations;
        for (auto it = object.begin(); it != object.end(); ++it)
            translations.insert(it.key(), it.value().toString());
        return translations;
    }();
    const auto source = QString::fromUtf8(english);
    const auto id = language() + ":" + source;
    auto found = renderedTranslations.find(id);
    if (found == renderedTranslations.end()) {
        auto rendered = language() == "zh" ? catalog.value(source, source) : source;
        rendered.detach();
        found = renderedTranslations.insert(id, rendered);
        translationSources.insert(found->constData(), source);
    }
    return *found;
}
void fc::retranslateUi(QWidget *root, const QString &previousLanguage) {
    QFile file(":/flattencom/zh_CN.json");
    if (!file.open(QIODevice::ReadOnly))
        return;
    const auto catalog = QJsonDocument::fromJson(file.readAll()).object();
    QHash<QString, QString> sources;
    for (auto it = catalog.begin(); it != catalog.end(); ++it) {
        const auto displayed = previousLanguage == "zh" ? it.value().toString() : it.key();
        // Never guess between two source keys with the same translation.
        sources[displayed] = sources.contains(displayed) ? QString() : it.key();
    }
    // Only UI chrome is visited. Editor contents, tree data rows, editable
    // inputs, session names and user-saved macros are deliberately excluded.
    auto objects = root->findChildren<QObject *>();
    objects.prepend(root);
    for (auto *object : objects) {
        const QSignalBlocker blocker(object);
        auto translate = [object, &sources, &catalog](const QString &value, const QByteArray &slot = "text") {
            const auto property = "translationSource/" + slot;
            auto source = translationSources.value(value.constData());
            if (source.isEmpty()) {
                const auto saved = object->property(property.constData()).toString();
                if (value == saved || value == catalog.value(saved).toString())
                    source = saved;
            }
            if (source.isEmpty())
                source = sources.value(value);
            if (source.isEmpty())
                return value;
            object->setProperty(property.constData(), source);
            return fc::text(source.toUtf8().constData());
        };
        if (auto *action = qobject_cast<QAction *>(object)) {
            if (action->property("documentEntry").toBool())
                continue;
            action->setText(translate(action->text()));
            action->setStatusTip(translate(action->statusTip(), "statusTip"));
            if (!action->shortcut().isEmpty())
                action->setToolTip(action->text() + " (" +
                                   action->shortcut().toString(QKeySequence::NativeText) + ")");
            else
                action->setToolTip(translate(action->toolTip(), "toolTip"));
        }
        if (auto *widget = qobject_cast<QWidget *>(object)) {
            widget->setWindowTitle(translate(widget->windowTitle(), "windowTitle"));
            widget->setToolTip(translate(widget->toolTip(), "toolTip"));
            widget->setAccessibleName(translate(widget->accessibleName(), "accessibleName"));
        }
        if (auto *button = qobject_cast<QAbstractButton *>(object))
            button->setText(translate(button->text()));
        if (auto *group = qobject_cast<QGroupBox *>(object))
            group->setTitle(translate(group->title()));
        if (auto *label = qobject_cast<QLabel *>(object)) {
            const auto name = label->objectName();
            if (name != "lastSentCommand" && name != "recordingStatus" && name != "sessionConnectionState")
                label->setText(translate(label->text()));
        }
        if (auto *input = qobject_cast<QLineEdit *>(object))
            input->setPlaceholderText(translate(input->placeholderText()));
        if (auto *editor = qobject_cast<QPlainTextEdit *>(object))
            editor->setPlaceholderText(translate(editor->placeholderText()));
        if (auto *editor = qobject_cast<QTextEdit *>(object))
            editor->setPlaceholderText(translate(editor->placeholderText()));
        if (auto *combo = qobject_cast<QComboBox *>(object)) {
            if (combo->objectName() == "macroList" || combo->isEditable())
                continue;
            for (int i = 0; i < combo->count(); ++i)
                combo->setItemText(i, translate(combo->itemText(i), "item/" + QByteArray::number(i)));
        }
        if (auto *tabs = qobject_cast<QTabWidget *>(object)) {
            for (int i = 0; i < tabs->count(); ++i)
                if (!tabs->widget(i)->inherits("SessionPane"))
                    tabs->setTabText(i, translate(tabs->tabText(i), "tab/" + QByteArray::number(i)));
        }
        if (auto *tree = qobject_cast<QTreeWidget *>(object)) {
            for (int i = 0; i < tree->columnCount(); ++i)
                tree->headerItem()->setText(
                    i, translate(tree->headerItem()->text(i), "header/" + QByteArray::number(i)));
        }
    }
}
void fc::setLanguage(const QString &value) {
    QSettings().setValue("language", value);
    loadTranslations();
}
QString fc::valueLabel(const QString &value) {
    if (value == "none" || value == "None")
        return text("None");
    if (value == "even")
        return text("Even");
    if (value == "odd")
        return text("Odd");
    if (value == "mark")
        return text("Mark (always 1)");
    if (value == "space")
        return text("Space (always 0)");
    if (value == "hardware")
        return text("Hardware (RTS/CTS)");
    if (value == "software")
        return text("Software (XON/XOFF)");
    if (value == "five")
        return "5";
    if (value == "six")
        return "6";
    if (value == "seven")
        return "7";
    if (value == "eight")
        return "8";
    if (value == "one")
        return "1";
    if (value == "one_point_five")
        return "1.5";
    if (value == "two")
        return "2";
    if (value == "ascii_lines")
        return text("Text lines");
    if (value == "utf8_lossy")
        return text("UTF-8 text");
    if (value == "hex_dump")
        return text("Hex dump");
    if (value == "json_lines")
        return text("JSON lines");
    if (value == "modbus_rtu")
        return "Modbus RTU";
    if (value == "nmea0183")
        return "NMEA 0183";
    if (value == "cmd")
        return text("Process plugin");
    if (value == "wasm")
        return text("WASM plugin");
    if (value == "connected")
        return text("Connected");
    if (value == "reconnecting")
        return text("Reconnecting...");
    if (value == "closed")
        return text("Closed");
    if (value == "failed")
        return text("Failed");
    if (value == "manual")
        return text("Marker");
    if (value == "boot")
        return text("Boot");
    if (value == "reset")
        return text("Reset");
    if (value == "operation")
        return text("Operation");
    return value;
}
void fc::addValues(QComboBox *combo, const QStringList &values) {
    for (const auto &value : values)
        combo->addItem(valueLabel(value), value);
}
void fc::selectValue(QComboBox *combo, const QString &value) {
    int index = combo->findData(value);
    if (index < 0) {
        addValues(combo, {value});
        index = combo->count() - 1;
    }
    combo->setCurrentIndex(index);
}
void fc::loadTranslations() {
    static QTranslator qt;
    static StandardTranslations standard;
    qApp->removeTranslator(&qt);
    qApp->removeTranslator(&standard);
    QLocale::setDefault(language() == "zh" ? QLocale(QLocale::Chinese, QLocale::China)
                                           : QLocale(QLocale::English));
    if (language() == "zh" && qt.load("qtbase_zh_CN", QLibraryInfo::path(QLibraryInfo::TranslationsPath)))
        qApp->installTranslator(&qt);
    if (language() == "zh")
        qApp->installTranslator(&standard);
}
QString fc::stateDirectory() {
    const auto override = qEnvironmentVariable("FLATTENCOM_STATE_DIR");
    if (!override.isEmpty())
        return override;
#ifdef Q_OS_WIN
    return QDir(qEnvironmentVariable("APPDATA")).filePath("flattencom");
#else
    return QDir(qEnvironmentVariable("XDG_STATE_HOME", QDir::homePath() + "/.local/state"))
        .filePath("flattencom");
#endif
}
QString fc::socketName() {
    const auto override = qEnvironmentVariable("FLATTENCOM_SOCKET");
    if (!override.isEmpty())
        return override;
#ifdef Q_OS_WIN
    return "\\\\.\\pipe\\flattencom.sock";
#else
    const auto runtime = qEnvironmentVariable("XDG_RUNTIME_DIR");
    if (!runtime.isEmpty())
        return QDir(runtime).filePath("flattencom.sock");
    QFile status("/proc/self/status");
    if (status.open(QIODevice::ReadOnly)) {
        for (const auto &line : status.readAll().split('\n')) {
            if (line.startsWith("Uid:"))
                return "/tmp/flattencom-" + QString::fromUtf8(line.mid(4).simplified().split(' ').first()) +
                       ".sock";
        }
    }
    return QStandardPaths::writableLocation(QStandardPaths::RuntimeLocation) + "/flattencom.sock";
#endif
}
QString fc::executable(const QString &name) {
#ifdef Q_OS_WIN
    const QString filename = name + ".exe";
#else
    const QString filename = name;
#endif
    if (name == "flattencomd" && !qEnvironmentVariable("FLATTENCOMD_BIN").isEmpty())
        return qEnvironmentVariable("FLATTENCOMD_BIN");
    QDir directory(QCoreApplication::applicationDirPath());
    for (int i = 0; i < 5; ++i) {
        for (const auto &relative : {filename, "target/release/" + filename, "target/debug/" + filename}) {
            const auto candidate = directory.filePath(relative);
            if (QFileInfo(candidate).isExecutable())
                return candidate;
        }
        directory.cdUp();
    }
    return QStandardPaths::findExecutable(filename);
}
QString fc::themeMode() {
    QSettings settings;
    const auto mode = settings.value("theme").toString();
    if (mode == "system" || mode == "light" || mode == "dark")
        return mode;
    // Migrate an explicit legacy choice; new installations follow the system.
    return settings.contains("dark") ? (settings.value("dark").toBool() ? "dark" : "light") : "system";
}
void fc::setThemeMode(QApplication &app, const QString &mode) {
    if (mode != "system" && mode != "light" && mode != "dark")
        return;
    QSettings settings;
    settings.setValue("theme", mode);
    settings.remove("dark");
    applyStyle(app, mode == "dark" ||
                        (mode == "system" && app.styleHints()->colorScheme() == Qt::ColorScheme::Dark));
}
void fc::initializeTheme(QApplication &app) {
    if (!app.property("themeListenerInstalled").toBool()) {
        app.setProperty("themeListenerInstalled", true);
        // Apply after Qt finishes processing the platform palette change.
        QObject::connect(
            app.styleHints(), &QStyleHints::colorSchemeChanged, &app,
            [&app](Qt::ColorScheme scheme) {
                const auto mode = themeMode();
                applyStyle(app, mode == "dark" || (mode == "system" && scheme == Qt::ColorScheme::Dark));
            },
            Qt::QueuedConnection);
    }
    setThemeMode(app, themeMode());
}
void fc::applyStyle(QApplication &app, bool dark) {
    app.setStyle(new WorkbenchStyle);
    QPalette palette;
    const QColor bg(dark ? "#111823" : "#eef2f7"), panel(dark ? "#17212f" : "#ffffff");
    const QColor fg(dark ? "#dce5f2" : "#263247"), dim(dark ? "#8798ae" : "#63738b");
    palette.setColor(QPalette::Window, bg);
    palette.setColor(QPalette::WindowText, fg);
    palette.setColor(QPalette::Base, panel);
    palette.setColor(QPalette::AlternateBase, dark ? QColor("#1c2838") : QColor("#f4f7fb"));
    palette.setColor(QPalette::Text, fg);
    palette.setColor(QPalette::Button, panel);
    palette.setColor(QPalette::ButtonText, fg);
    palette.setColor(QPalette::Highlight, QColor("#286c8d"));
    palette.setColor(QPalette::HighlightedText, QColor("#ffffff"));
    palette.setColor(QPalette::Link, QColor(dark ? "#46b3c2" : "#176779"));
    palette.setColor(QPalette::LinkVisited, palette.color(QPalette::Link));
    palette.setColor(QPalette::PlaceholderText, dim);
    palette.setColor(QPalette::ToolTipBase, panel);
    palette.setColor(QPalette::ToolTipText, fg);
    palette.setColor(QPalette::Disabled, QPalette::Text, dim);
    palette.setColor(QPalette::Disabled, QPalette::ButtonText, dim);
    app.setPalette(palette);
    app.setStyleSheet(QString(R"(
      QMainWindow { background: %1; }
      QToolBar { spacing: 3px; border: 0; border-bottom: 1px solid %2; padding: 3px; }
      QToolBar#serialTools, QToolBar#macroTools { spacing: 3px; padding: 3px; }
      QToolBar#serialTools QToolButton, QToolBar#macroTools QToolButton { padding: 4px; }
      QToolButton, QPushButton { padding: 3px 8px; border: 1px solid %2; border-radius: 2px; }
      QToolButton:hover, QPushButton:hover { border-color: #46b3c2; }
      QPushButton#primary { background: #138d9e; color: white; border: 0; font-weight: bold; }
      QLineEdit { padding: 3px; border: 1px solid %2; border-radius: 2px; }
      QComboBox QLineEdit, QAbstractSpinBox QLineEdit { padding: 0; border: 0; background: transparent; }
      QDockWidget::title { padding: 4px; background: %3; font-weight: bold; }
      QTabWidget::pane { border: 1px solid %2; }
      QTabBar::tab { padding: 4px 10px; border-bottom: 2px solid transparent; }
      QTabBar::tab:selected { border-bottom-color: #46b3c2; }
      QHeaderView { background: %3; }
      QHeaderView::section { background: %3; border: 0; padding: 4px; color: %4; }
      QTableCornerButton::section { background: %3; border: 0; }
      QTreeWidget, QTableView, QTextEdit { border: 0; }
      QStatusBar { padding: 2px; }
      QGroupBox { border: 1px solid %2; border-radius: 2px; margin-top: 8px; padding-top: 6px; }
      QGroupBox::title { subcontrol-origin: margin; left: 10px; padding: 0 5px; }
      QSplitter::handle { background: %2; }
    )")
                          .arg(bg.name(), dark ? "#2b3b50" : "#cfdae6", panel.name(), dim.name()));
}
