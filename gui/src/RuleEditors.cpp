/*
 * flattencom - Graphical Configuration Editors
 *
 * Edits highlighting, reset steps, triggers and decoder plugin settings through forms.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

#include "RuleEditors.h"
#include "LogRules.h"
#include "Style.h"
#include <QCheckBox>
#include <QColorDialog>
#include <QComboBox>
#include <QDialogButtonBox>
#include <QFileDialog>
#include <QFormLayout>
#include <QJsonObject>
#include <QLabel>
#include <QLineEdit>
#include <QListWidget>
#include <QMessageBox>
#include <QPlainTextEdit>
#include <QPushButton>
#include <QSet>
#include <QSpinBox>
#include <QSplitter>
#include <QUuid>
#include <QVBoxLayout>

namespace {
// Free-function dialog builders use the same QObject connection API as widgets.
template <class... Args> auto connect(Args &&...args) {
    return QObject::connect(std::forward<Args>(args)...);
}
QJsonArray contents(QListWidget *list) {
    QJsonArray result;
    for (int i = 0; i < list->count(); ++i)
        result.append(list->item(i)->data(Qt::UserRole).toJsonObject());
    return result;
}

// Reorder actual items so selection and the editor always refer to the same entry.
void moveItem(QListWidget *list, int delta) {
    const int row = list->currentRow(), target = row + delta;
    if (row < 0 || target < 0 || target >= list->count())
        return;
    auto *item = list->takeItem(row);
    list->insertItem(target, item);
    list->setCurrentItem(item);
}

QVBoxLayout *listPanel(QSplitter *split, QListWidget *&list) {
    auto *panel = new QWidget(split);
    auto *layout = new QVBoxLayout(panel);
    layout->setContentsMargins(0, 0, 0, 0);
    list = new QListWidget;
    list->setMinimumWidth(220);
    layout->addWidget(list, 1);
    auto *buttons = new QHBoxLayout;
    for (const auto &entry : {qMakePair(fc::text("Move up"), -1), qMakePair(fc::text("Move down"), 1)}) {
        auto *button = new QPushButton(entry.first);
        buttons->addWidget(button);
        QObject::connect(button, &QPushButton::clicked, list,
                         [list, delta = entry.second] { moveItem(list, delta); });
        QObject::connect(list, &QListWidget::currentRowChanged, button,
                         [list, button, delta = entry.second](int row) {
                             button->setEnabled(row >= 0 && row + delta >= 0 && row + delta < list->count());
                         });
        button->setEnabled(false);
    }
    layout->addLayout(buttons);
    return layout;
}

void appendItem(QListWidget *list, const QJsonObject &object, const QString &label) {
    auto *item = new QListWidgetItem(label, list);
    item->setData(Qt::UserRole, object);
    list->setCurrentItem(item);
}

QString ruleName(const QJsonObject &rule) {
    if (!rule.value("name").toString().isEmpty())
        return rule.value("name").toString();
    const QMap<QString, QString> names{{"kernel-panic", fc::text("Kernel panic")},
                                       {"oops-bug", fc::text("Kernel exception")},
                                       {"watchdog-lockup", fc::text("Watchdog and lockup")},
                                       {"oom", fc::text("Out of memory")},
                                       {"boot-rootfs", fc::text("Root filesystem failure")},
                                       {"stacktrace", fc::text("Stack trace")},
                                       {"segfault", fc::text("Process crash")},
                                       {"storage-io", fc::text("Storage I/O")},
                                       {"filesystem", fc::text("Filesystem error")},
                                       {"mmc", fc::text("MMC and SD card")},
                                       {"usb", "USB"},
                                       {"firmware", fc::text("Firmware loading")},
                                       {"driver-probe", fc::text("Driver initialization")},
                                       {"bus", fc::text("I2C and SPI")},
                                       {"network", fc::text("Network error")},
                                       {"service-failed", fc::text("Service failure")},
                                       {"security", fc::text("Permissions and authentication")},
                                       {"thermal", fc::text("Temperature and power")},
                                       {"protocol", fc::text("Protocol validation")},
                                       {"explicit-error", fc::text("Error messages")},
                                       {"warning", fc::text("Warnings")},
                                       {"timeout", fc::text("Timeouts and retries")},
                                       {"resources", fc::text("Resource limits")},
                                       {"boot-linux", fc::text("Linux boot")},
                                       {"boot-loader", fc::text("Bootloader")},
                                       {"ready", fc::text("System ready")}};
    return names.value(rule.value("id").toString(), rule.value("id").toString());
}

QString stepName(const QJsonObject &step) {
    QStringList parts;
    for (const auto &key : {QString("dtr"), QString("rts")})
        if (step.contains(key))
            parts.append(key.toUpper() + " " +
                         (step.value(key).toBool() ? fc::text("Assert") : fc::text("Clear")));
    if (step.contains("data"))
        parts.append(fc::text("Send text"));
    if (step.contains("hex"))
        parts.append(fc::text("Send HEX"));
    if (step.value("delay_ms").toInt() > 0)
        parts.append(fc::text("Wait ") + QString::number(step.value("delay_ms").toInt()) + " ms");
    return parts.isEmpty() ? fc::text("New step") : parts.join(", ");
}
} // namespace

HighlightRulesDialog::HighlightRulesDialog(const QJsonArray &rules, QWidget *parent) : QDialog(parent) {
    setWindowTitle(fc::text("Highlight rules"));
    resize(900, 570);
    auto *layout = new QVBoxLayout(this);
    layout->addWidget(new QLabel(fc::text("Rules are checked from top to bottom. The first match is used.")));
    auto *split = new QSplitter;
    layout->addWidget(split, 1);
    auto *left = listPanel(split, list_);
    list_->setObjectName("highlightRuleList");
    auto *row = new QHBoxLayout;
    auto *add = new QPushButton(fc::text("Add"));
    add->setObjectName("addHighlightRule");
    auto *remove = new QPushButton(fc::text("Delete"));
    row->addWidget(add);
    row->addWidget(remove);
    left->addLayout(row);
    editor_ = new QWidget(split);
    auto *form = new QFormLayout(editor_);
    enabled_ = new QCheckBox(fc::text("Enabled"));
    name_ = new QLineEdit;
    category_ = new QLineEdit;
    mode_ = new QComboBox;
    mode_->addItem(fc::text("Keyword"), "literal");
    mode_->addItem(fc::text("Regular expression"), "regex");
    pattern_ = new QLineEdit;
    pattern_->setObjectName("highlightPattern");
    pattern_->setMaxLength(2048);
    caseSensitive_ = new QCheckBox(fc::text("Case sensitive"));
    severity_ = new QComboBox;
    for (const auto &entry : {qMakePair(fc::text("Critical"), "fatal"), qMakePair(fc::text("Error"), "error"),
                              qMakePair(fc::text("Warning"), "warning"), qMakePair(fc::text("Info"), "info")})
        severity_->addItem(entry.first, entry.second);
    color_ = new QPushButton;
    sample_ = new QLineEdit;
    sample_->setObjectName("highlightSample");
    feedback_ = new QLabel;
    feedback_->setObjectName("highlightPreview");
    feedback_->setWordWrap(true);
    form->addRow(enabled_);
    form->addRow(fc::text("Name"), name_);
    form->addRow(fc::text("Category"), category_);
    form->addRow(fc::text("Match type"), mode_);
    form->addRow(fc::text("Pattern"), pattern_);
    form->addRow(caseSensitive_);
    form->addRow(fc::text("Severity"), severity_);
    form->addRow(fc::text("Color"), color_);
    form->addRow(fc::text("Test text"), sample_);
    form->addRow(feedback_);
    auto *buttons = new QDialogButtonBox(QDialogButtonBox::Save | QDialogButtonBox::Cancel);
    auto *defaults = buttons->addButton(fc::text("Restore defaults"), QDialogButtonBox::ResetRole);
    layout->addWidget(buttons);
    connect(list_, &QListWidget::currentRowChanged, this, [this] { loadCurrent(); });
    for (auto *field : {name_, category_, pattern_})
        connect(field, &QLineEdit::textChanged, this, [this] { updateCurrent(); });
    for (auto *field : {mode_, severity_})
        connect(field, &QComboBox::currentIndexChanged, this, [this] { updateCurrent(); });
    for (auto *field : {enabled_, caseSensitive_})
        connect(field, &QCheckBox::toggled, this, [this] { updateCurrent(); });
    connect(sample_, &QLineEdit::textChanged, this, [this] { preview(); });
    connect(color_, &QPushButton::clicked, this, [this] {
        const auto color = QColorDialog::getColor(QColor(colorValue_), this, fc::text("Highlight color"));
        if (color.isValid()) {
            colorValue_ = color.name();
            updateCurrent();
        }
    });
    connect(add, &QPushButton::clicked, this, [this] {
        if (list_->count() >= 128)
            return;
        appendItem(list_,
                   {{"id", QUuid::createUuid().toString(QUuid::WithoutBraces)},
                    {"name", fc::text("New rule")},
                    {"pattern", ""},
                    {"match", "literal"},
                    {"severity", "warning"},
                    {"color", "#e5b45a"},
                    {"enabled", true}},
                   fc::text("New rule"));
        pattern_->setFocus();
    });
    connect(remove, &QPushButton::clicked, this, [this] {
        delete list_->takeItem(list_->currentRow());
        loadCurrent();
    });
    connect(defaults, &QPushButton::clicked, this, [this] {
        if (QMessageBox::question(this, windowTitle(),
                                  fc::text("Restore default rules and replace current changes?")) ==
            QMessageBox::Yes)
            populate(LogRules::defaults());
    });
    connect(buttons, &QDialogButtonBox::rejected, this, &QDialog::reject);
    connect(buttons, &QDialogButtonBox::accepted, this, [this] {
        const auto error = LogRules::validate(this->rules());
        if (!error.isEmpty()) {
            QMessageBox::warning(this, windowTitle(), error);
            return;
        }
        accept();
    });
    populate(rules);
    split->setSizes({270, 600});
}
QJsonArray HighlightRulesDialog::rules() const { return contents(list_); }
void HighlightRulesDialog::populate(const QJsonArray &rules) {
    list_->clear();
    for (const auto &v : rules) {
        auto rule = v.toObject();
        rule["name"] = ruleName(rule);
        appendItem(list_, rule, rule.value("name").toString());
    }
    list_->setCurrentRow(list_->count() ? 0 : -1);
    loadCurrent();
}
void HighlightRulesDialog::loadCurrent() {
    loading_ = true;
    auto *item = list_->currentItem();
    editor_->setEnabled(item != nullptr);
    const auto rule = item ? item->data(Qt::UserRole).toJsonObject() : QJsonObject{};
    name_->setText(rule.value("name").toString());
    category_->setText(rule.value("category").toString());
    pattern_->setText(rule.value("pattern").toString());
    mode_->setCurrentIndex(rule.value("match").toString("regex") == "literal" ? 0 : 1);
    severity_->setCurrentIndex(qMax(0, severity_->findData(rule.value("severity").toString("warning"))));
    enabled_->setChecked(rule.value("enabled").toBool(true));
    caseSensitive_->setChecked(rule.value("case_sensitive").toBool());
    colorValue_ = rule.value("color").toString("#e5b45a");
    loading_ = false;
    preview();
}
void HighlightRulesDialog::updateCurrent() {
    if (loading_ || !list_->currentItem())
        return;
    auto *item = list_->currentItem();
    auto rule = item->data(Qt::UserRole).toJsonObject();
    rule["name"] = name_->text();
    rule["category"] = category_->text();
    rule["pattern"] = pattern_->text();
    rule["match"] = mode_->currentData().toString();
    rule["severity"] = severity_->currentData().toString();
    rule["enabled"] = enabled_->isChecked();
    rule["case_sensitive"] = caseSensitive_->isChecked();
    rule["color"] = colorValue_;
    item->setData(Qt::UserRole, rule);
    item->setText(name_->text().isEmpty() ? fc::text("Unnamed rule") : name_->text());
    preview();
}
void HighlightRulesDialog::preview() {
    color_->setText(colorValue_);
    color_->setStyleSheet("border: 2px solid " + colorValue_ + ";");
    if (!list_->currentItem()) {
        feedback_->clear();
        return;
    }
    const auto rule = list_->currentItem()->data(Qt::UserRole).toJsonObject();
    const auto expression = LogRules::expression(rule);
    feedback_->setStyleSheet({});
    if (pattern_->text().isEmpty())
        feedback_->setText(fc::text("Enter a pattern."));
    else if (!expression.isValid())
        feedback_->setText(fc::text("Invalid expression: ") + expression.errorString());
    else if (sample_->text().isEmpty())
        feedback_->setText(fc::text("Enter test text to preview a match."));
    else if (expression.match(sample_->text()).hasMatch()) {
        feedback_->setText(fc::text("Match"));
        feedback_->setStyleSheet("color: " + colorValue_ + ";");
    } else
        feedback_->setText(fc::text("No match"));
}

ResetSequenceDialog::ResetSequenceDialog(const QJsonArray &steps, QWidget *parent) : QDialog(parent) {
    setWindowTitle(fc::text("Reset settings"));
    resize(820, 540);
    auto *layout = new QVBoxLayout(this);
    auto *split = new QSplitter;
    layout->addWidget(split, 1);
    auto *left = listPanel(split, list_);
    list_->setObjectName("resetStepList");
    auto *row = new QHBoxLayout;
    auto *add = new QPushButton(fc::text("Add step"));
    add->setObjectName("addResetStep");
    auto *remove = new QPushButton(fc::text("Delete"));
    row->addWidget(add);
    row->addWidget(remove);
    left->addLayout(row);
    editor_ = new QWidget(split);
    auto *form = new QFormLayout(editor_);
    dtr_ = new QComboBox;
    rts_ = new QComboBox;
    dtr_->setObjectName("resetDtr");
    rts_->setObjectName("resetRts");
    for (auto *combo : {dtr_, rts_})
        combo->addItems({fc::text("Unchanged"), fc::text("Assert"), fc::text("Clear")});
    encoding_ = new QComboBox;
    encoding_->setObjectName("resetEncoding");
    encoding_->addItems({fc::text("None"), fc::text("Text"), "HEX"});
    payload_ = new QPlainTextEdit;
    payload_->setObjectName("resetPayload");
    payload_->setMaximumHeight(150);
    newline_ = new QComboBox;
    newline_->addItems({fc::text("None"), "CRLF", "LF", "CR"});
    newline_->setObjectName("resetNewline");
    delay_ = new QSpinBox;
    delay_->setObjectName("resetDelay");
    delay_->setRange(0, 5000);
    delay_->setSuffix(" ms");
    feedback_ = new QLabel;
    feedback_->setWordWrap(true);
    form->addRow("DTR", dtr_);
    form->addRow("RTS", rts_);
    form->addRow(fc::text("Send format"), encoding_);
    form->addRow(fc::text("Payload"), payload_);
    form->addRow(fc::text("Line ending"), newline_);
    form->addRow(fc::text("Delay after step"), delay_);
    form->addRow(new QLabel(fc::text("Each step sets control lines, sends data, then waits.")));
    form->addRow(feedback_);
    auto *buttons = new QDialogButtonBox(QDialogButtonBox::Save | QDialogButtonBox::Cancel);
    layout->addWidget(buttons);
    connect(list_, &QListWidget::currentRowChanged, this, [this] { loadCurrent(); });
    for (auto *combo : {dtr_, rts_, encoding_, newline_})
        connect(combo, &QComboBox::currentIndexChanged, this, [this] { updateCurrent(); });
    connect(payload_, &QPlainTextEdit::textChanged, this, [this] { updateCurrent(); });
    connect(delay_, &QSpinBox::valueChanged, this, [this] { updateCurrent(); });
    connect(add, &QPushButton::clicked, this, [this] {
        if (list_->count() < 32)
            appendItem(list_, {{"delay_ms", 100}}, fc::text("Wait 100 ms"));
    });
    connect(remove, &QPushButton::clicked, this, [this] {
        delete list_->takeItem(list_->currentRow());
        loadCurrent();
    });
    connect(buttons, &QDialogButtonBox::rejected, this, &QDialog::reject);
    connect(buttons, &QDialogButtonBox::accepted, this, [this] {
        const auto value = this->steps();
        QString error;
        if (value.isEmpty() || value.size() > 32)
            error = fc::text("Add 1 to 32 steps.");
        for (qsizetype i = 0; i < value.size() && error.isEmpty(); ++i) {
            const auto step = value[i].toObject();
            if (step.isEmpty() || (step.size() == 1 && step.value("delay_ms").toInt() == 0))
                error = fc::text("Step has no action.");
            if (step.contains("hex")) {
                auto hex = step.value("hex").toString();
                hex.remove(QRegularExpression("\\s+"));
                if (hex.isEmpty() || hex.size() % 2 ||
                    !QRegularExpression("^[0-9a-fA-F]+$").match(hex).hasMatch())
                    error = fc::text("HEX must contain complete hexadecimal bytes.");
            }
            for (const auto &key : {"data", "hex"})
                if (step.value(key).toString().toUtf8().size() > 4096)
                    error = fc::text("Payload exceeds 4096 bytes.");
            if (!error.isEmpty())
                error = fc::text("Step ") + QString::number(i + 1) + ": " + error;
        }
        if (!error.isEmpty()) {
            feedback_->setText(error);
            return;
        }
        accept();
    });
    // Older profiles may send text and HEX in the same step. Preserve execution order.
    for (const auto &v : steps) {
        auto step = v.toObject();
        if (step.contains("data") && step.contains("hex")) {
            auto next = QJsonObject{{"hex", step.take("hex")}};
            if (step.contains("delay_ms"))
                next["delay_ms"] = step.take("delay_ms");
            appendItem(list_, step, stepName(step));
            appendItem(list_, next, stepName(next));
        } else
            appendItem(list_, step, stepName(step));
    }
    list_->setCurrentRow(list_->count() ? 0 : -1);
    loadCurrent();
    split->setSizes({290, 490});
}
QJsonArray ResetSequenceDialog::steps() const { return contents(list_); }
void ResetSequenceDialog::loadCurrent() {
    loading_ = true;
    auto *item = list_->currentItem();
    editor_->setEnabled(item != nullptr);
    const auto step = item ? item->data(Qt::UserRole).toJsonObject() : QJsonObject{};
    dtr_->setCurrentIndex(step.contains("dtr") ? (step.value("dtr").toBool() ? 1 : 2) : 0);
    rts_->setCurrentIndex(step.contains("rts") ? (step.value("rts").toBool() ? 1 : 2) : 0);
    encoding_->setCurrentIndex(step.contains("data") ? 1 : step.contains("hex") ? 2 : 0);
    auto payload = step.value(step.contains("data") ? "data" : "hex").toString();
    int ending = 0;
    if (step.contains("data")) {
        if (payload.endsWith("\r\n")) {
            payload.chop(2);
            ending = 1;
        } else if (payload.endsWith('\n')) {
            payload.chop(1);
            ending = 2;
        } else if (payload.endsWith('\r')) {
            payload.chop(1);
            ending = 3;
        }
    }
    payload_->setPlainText(payload);
    payload_->setProperty("originalPayload", payload);
    payload_->setProperty("displayPayload", payload_->toPlainText());
    newline_->setCurrentIndex(ending);
    delay_->setValue(step.value("delay_ms").toInt());
    payload_->setEnabled(encoding_->currentIndex() != 0);
    newline_->setEnabled(encoding_->currentIndex() == 1);
    feedback_->clear();
    loading_ = false;
}
void ResetSequenceDialog::updateCurrent() {
    if (loading_ || !list_->currentItem())
        return;
    QJsonObject step;
    for (const auto &entry : {qMakePair("dtr", dtr_), qMakePair("rts", rts_)})
        if (entry.second->currentIndex())
            step[entry.first] = entry.second->currentIndex() == 1;
    const auto mode = encoding_->currentIndex();
    payload_->setEnabled(mode != 0);
    newline_->setEnabled(mode == 1);
    if (mode == 1) {
        const QStringList endings{"", "\r\n", "\n", "\r"};
        const auto text = payload_->toPlainText() == payload_->property("displayPayload").toString()
                              ? payload_->property("originalPayload").toString()
                              : payload_->toPlainText();
        step["data"] = text + endings[newline_->currentIndex()];
    }
    if (mode == 2) {
        auto hex = payload_->toPlainText();
        hex.remove(QRegularExpression("\\s+"));
        step["hex"] = hex;
    }
    if (delay_->value())
        step["delay_ms"] = delay_->value();
    auto *item = list_->currentItem();
    item->setData(Qt::UserRole, step);
    item->setText(stepName(step));
    feedback_->clear();
}

namespace {
bool editTrigger(QJsonObject &trigger, QWidget *parent) {
    QDialog dialog(parent);
    dialog.setObjectName("triggerEditor");
    dialog.setWindowTitle(fc::text("Edit trigger"));
    dialog.resize(840, 590);
    auto *layout = new QVBoxLayout(&dialog);
    auto *form = new QFormLayout;
    layout->addLayout(form);
    auto *name = new QLineEdit(trigger.value("id").toString());
    name->setObjectName("triggerName");
    auto *direction = new QComboBox;
    direction->addItem(fc::text("Receive (RX)"), "rx");
    direction->addItem(fc::text("Transmit (TX)"), "tx");
    direction->setCurrentIndex(trigger.value("direction").toString() == "tx" ? 1 : 0);
    auto *mode = new QComboBox;
    mode->addItems({fc::text("Keyword"), fc::text("Regular expression")});
    mode->setCurrentIndex(trigger.contains("regex") ? 1 : 0);
    auto *pattern = new QLineEdit(trigger.value("regex").toString());
    pattern->setObjectName("triggerPattern");
    auto *interval = new QSpinBox;
    interval->setRange(0, 2147483647);
    interval->setSuffix(" ms");
    interval->setValue(trigger.value("min_interval_ms").toInt(100));
    form->addRow(fc::text("Name"), name);
    form->addRow(fc::text("Direction"), direction);
    form->addRow(fc::text("Match type"), mode);
    form->addRow(fc::text("Pattern"), pattern);
    form->addRow(fc::text("Minimum interval"), interval);
    auto *actions = new QListWidget;
    layout->addWidget(new QLabel(fc::text("Actions")));
    layout->addWidget(actions, 1);
    auto append = [actions](const QJsonObject &action) {
        const auto type = action.value("action").toString();
        const auto label = type == "respond"
                               ? (action.contains("hex") ? fc::text("Send HEX") : fc::text("Send text"))
                           : type == "execute" ? fc::text("Run program")
                                               : fc::text("Record event");
        appendItem(actions, action, label);
    };
    for (const auto &v : trigger.value("actions").toArray())
        append(v.toObject());
    auto editAction = [&](bool adding) {
        if (!adding && !actions->currentItem())
            return;
        const auto original =
            adding ? QJsonObject{} : actions->currentItem()->data(Qt::UserRole).toJsonObject();
        QDialog actionDialog(&dialog);
        actionDialog.setWindowTitle(fc::text("Trigger action"));
        actionDialog.resize(580, 390);
        auto *actionForm = new QFormLayout(&actionDialog);
        auto *type = new QComboBox;
        type->addItems(
            {fc::text("Send text"), fc::text("Send HEX"), fc::text("Record event"), fc::text("Run program")});
        type->setCurrentIndex(original.value("action") == "execute"     ? 3
                              : original.value("action") == "highlight" ? 2
                              : original.contains("hex")                ? 1
                                                                        : 0);
        auto *payload = new QPlainTextEdit;
        payload->setPlainText(original.value(original.contains("hex") ? "hex" : "data").toString());
        const auto displayPayload = payload->toPlainText();
        auto *ending = new QComboBox;
        ending->addItems({fc::text("None"), "CRLF", "LF", "CR"});
        auto *program = new QLineEdit(original.value("program").toString());
        auto *browse = new QPushButton(fc::text("Browse..."));
        auto *programRow = new QHBoxLayout;
        programRow->addWidget(program, 1);
        programRow->addWidget(browse);
        auto *args = new QPlainTextEdit;
        QStringList arguments;
        for (const auto &v : original.value("args").toArray())
            arguments.append(v.toString());
        args->setPlainText(arguments.join('\n'));
        actionForm->addRow(fc::text("Action"), type);
        actionForm->addRow(fc::text("Payload"), payload);
        actionForm->addRow(fc::text("Append line ending"), ending);
        actionForm->addRow(fc::text("Program"), programRow);
        actionForm->addRow(fc::text("Arguments (one per line)"), args);
        auto update = [=] {
            payload->setEnabled(type->currentIndex() < 2);
            ending->setEnabled(type->currentIndex() == 0);
            program->setEnabled(type->currentIndex() == 3);
            browse->setEnabled(type->currentIndex() == 3);
            args->setEnabled(type->currentIndex() == 3);
        };
        connect(type, &QComboBox::currentIndexChanged, &actionDialog, update);
        update();
        connect(browse, &QPushButton::clicked, &actionDialog, [&] {
            const auto path = QFileDialog::getOpenFileName(&actionDialog, fc::text("Select program"));
            if (!path.isEmpty())
                program->setText(path);
        });
        auto *error = new QLabel;
        error->setWordWrap(true);
        actionForm->addRow(error);
        auto *buttons = new QDialogButtonBox(QDialogButtonBox::Save | QDialogButtonBox::Cancel);
        actionForm->addRow(buttons);
        QJsonObject result;
        connect(buttons, &QDialogButtonBox::rejected, &actionDialog, &QDialog::reject);
        connect(buttons, &QDialogButtonBox::accepted, &actionDialog, [&] {
            result = {};
            if (type->currentIndex() < 2) {
                result["action"] = "respond";
                auto text = payload->toPlainText();
                if (type->currentIndex() == 0 && original.contains("data") && text == displayPayload)
                    text = original.value("data").toString();
                if (type->currentIndex() == 1) {
                    text.remove(QRegularExpression("\\s+"));
                    if (text.isEmpty() || text.size() % 2 ||
                        !QRegularExpression("^[0-9a-fA-F]+$").match(text).hasMatch()) {
                        error->setText(fc::text("Invalid HEX."));
                        return;
                    }
                    result["hex"] = text;
                } else {
                    const QStringList endings{"", "\r\n", "\n", "\r"};
                    result["data"] = text + endings[ending->currentIndex()];
                }
            } else if (type->currentIndex() == 2)
                result["action"] = "highlight";
            else {
                if (program->text().trimmed().isEmpty()) {
                    error->setText(fc::text("Select a program."));
                    return;
                }
                result["action"] = "execute";
                result["program"] = program->text();
                QJsonArray values;
                if (!args->toPlainText().isEmpty())
                    for (const auto &s : args->toPlainText().split('\n'))
                        values.append(s);
                result["args"] = values;
            }
            actionDialog.accept();
        });
        if (actionDialog.exec() != QDialog::Accepted)
            return;
        if (!adding) {
            const int row = actions->currentRow();
            delete actions->takeItem(row);
            append(result);
            auto *item = actions->takeItem(actions->count() - 1);
            actions->insertItem(row, item);
            actions->setCurrentItem(item);
        } else
            append(result);
    };
    auto *row = new QHBoxLayout;
    layout->addLayout(row);
    auto *add = new QPushButton(fc::text("Add action"));
    auto *edit = new QPushButton(fc::text("Edit"));
    auto *remove = new QPushButton(fc::text("Delete"));
    row->addWidget(add);
    row->addWidget(edit);
    row->addWidget(remove);
    connect(add, &QPushButton::clicked, &dialog, [&] { editAction(true); });
    connect(edit, &QPushButton::clicked, &dialog, [&] { editAction(false); });
    connect(remove, &QPushButton::clicked, &dialog, [&] { delete actions->takeItem(actions->currentRow()); });
    for (const auto &entry : {qMakePair(fc::text("Move up"), -1), qMakePair(fc::text("Move down"), 1)}) {
        auto *button = new QPushButton(entry.first);
        row->addWidget(button);
        connect(button, &QPushButton::clicked, &dialog, [=] { moveItem(actions, entry.second); });
    }
    auto *error = new QLabel;
    error->setWordWrap(true);
    layout->addWidget(error);
    auto *buttons = new QDialogButtonBox(QDialogButtonBox::Save | QDialogButtonBox::Cancel);
    layout->addWidget(buttons);
    connect(buttons, &QDialogButtonBox::rejected, &dialog, &QDialog::reject);
    connect(buttons, &QDialogButtonBox::accepted, &dialog, [&] {
        const auto regex =
            mode->currentIndex() ? pattern->text() : QRegularExpression::escape(pattern->text());
        const QRegularExpression expression(regex);
        if (name->text().trimmed().isEmpty() || pattern->text().isEmpty() || !actions->count()) {
            error->setText(fc::text("Enter a name, pattern and at least one action."));
            return;
        }
        if (!expression.isValid()) {
            error->setText(expression.errorString());
            return;
        }
        trigger = {{"id", name->text()},
                   {"direction", direction->currentData().toString()},
                   {"regex", regex},
                   {"min_interval_ms", interval->value()},
                   {"actions", contents(actions)}};
        dialog.accept();
    });
    return dialog.exec() == QDialog::Accepted;
}
} // namespace

TriggersDialog::TriggersDialog(const QJsonArray &triggers, QWidget *parent) : QDialog(parent) {
    setWindowTitle(fc::text("Triggers"));
    resize(640, 430);
    auto *layout = new QVBoxLayout(this);
    list_ = new QListWidget;
    layout->addWidget(list_, 1);
    for (const auto &v : triggers)
        appendItem(list_, v.toObject(), v.toObject().value("id").toString());
    auto edit = [this](bool adding) {
        if (!adding && !list_->currentItem())
            return;
        auto value = adding ? QJsonObject{} : list_->currentItem()->data(Qt::UserRole).toJsonObject();
        const int row = list_->currentRow();
        if (!editTrigger(value, this))
            return;
        if (adding)
            appendItem(list_, value, value.value("id").toString());
        else {
            auto *item = list_->item(row);
            item->setData(Qt::UserRole, value);
            item->setText(value.value("id").toString());
        }
    };
    auto *row = new QHBoxLayout;
    layout->addLayout(row);
    auto *add = new QPushButton(fc::text("Add"));
    auto *modify = new QPushButton(fc::text("Edit"));
    modify->setObjectName("editTrigger");
    auto *remove = new QPushButton(fc::text("Delete"));
    row->addWidget(add);
    row->addWidget(modify);
    row->addWidget(remove);
    connect(add, &QPushButton::clicked, this, [edit] { edit(true); });
    connect(modify, &QPushButton::clicked, this, [edit] { edit(false); });
    connect(list_, &QListWidget::itemDoubleClicked, this, [edit] { edit(false); });
    connect(remove, &QPushButton::clicked, this, [this] { delete list_->takeItem(list_->currentRow()); });
    auto *buttons = new QDialogButtonBox(QDialogButtonBox::Save | QDialogButtonBox::Cancel);
    layout->addWidget(buttons);
    connect(buttons, &QDialogButtonBox::rejected, this, &QDialog::reject);
    connect(buttons, &QDialogButtonBox::accepted, this, [this] {
        QSet<QString> ids;
        for (const auto &v : this->triggers()) {
            const auto id = v.toObject().value("id").toString();
            if (ids.contains(id)) {
                QMessageBox::warning(this, windowTitle(), fc::text("Trigger names must be unique."));
                return;
            }
            ids.insert(id);
        }
        accept();
    });
}
QJsonArray TriggersDialog::triggers() const { return contents(list_); }

bool fc::editDecoderOptions(QJsonObject &spec, QWidget *parent) {
    QDialog dialog(parent);
    dialog.setWindowTitle(fc::text("Decoder settings"));
    dialog.resize(620, 460);
    auto *form = new QFormLayout(&dialog);
    auto *name = new QComboBox;
    fc::addValues(name, {"none", "ascii_lines", "utf8_lossy", "hex_dump", "json_lines", "modbus_rtu",
                         "nmea0183", "cmd", "wasm"});
    const auto oldName = spec.value("name").toString("none");
    fc::selectValue(name, oldName);
    const auto options = spec.value("options").toObject();
    auto *role = new QComboBox;
    role->addItem(fc::text("Auto"), "auto");
    role->addItem(fc::text("Master"), "master");
    role->addItem(fc::text("Slave"), "slave");
    role->setCurrentIndex(qMax(0, role->findData(options.value("role").toString("auto"))));
    auto *path = new QLineEdit(options.value(oldName == "wasm" ? "path" : "program").toString());
    auto *browse = new QPushButton(fc::text("Browse..."));
    auto *fileRow = new QHBoxLayout;
    fileRow->addWidget(path, 1);
    fileRow->addWidget(browse);
    auto *args = new QPlainTextEdit;
    QStringList values;
    for (const auto &v : options.value("args").toArray())
        values.append(v.toString());
    args->setPlainText(values.join('\n'));
    args->setMaximumHeight(120);
    auto *timeout = new QSpinBox;
    timeout->setRange(1, 60000);
    timeout->setValue(options.value("timeout_ms").toInt(1000));
    timeout->setSuffix(" ms");
    auto *fuel = new QLineEdit(QString::number(options.value("fuel").toInteger(1000000)));
    form->addRow(fc::text("Decoder"), name);
    form->addRow(fc::text("Modbus role"), role);
    form->addRow(fc::text("Plugin file"), fileRow);
    form->addRow(fc::text("Arguments (one per line)"), args);
    form->addRow(fc::text("Timeout"), timeout);
    form->addRow(fc::text("WASM instruction budget"), fuel);
    auto update = [=] {
        const auto current = name->currentData().toString();
        role->setEnabled(current == "modbus_rtu");
        path->setEnabled(current == "wasm" || current == "cmd");
        browse->setEnabled(path->isEnabled());
        args->setEnabled(current == "cmd");
        timeout->setEnabled(current == "cmd");
        fuel->setEnabled(current == "wasm");
    };
    connect(name, &QComboBox::currentIndexChanged, &dialog, update);
    update();
    connect(browse, &QPushButton::clicked, &dialog, [&] {
        const auto selected = QFileDialog::getOpenFileName(&dialog, fc::text("Select plugin file"));
        if (!selected.isEmpty())
            path->setText(selected);
    });
    auto *error = new QLabel;
    error->setWordWrap(true);
    form->addRow(error);
    auto *buttons = new QDialogButtonBox(QDialogButtonBox::Save | QDialogButtonBox::Cancel);
    form->addRow(buttons);
    connect(buttons, &QDialogButtonBox::rejected, &dialog, &QDialog::reject);
    connect(buttons, &QDialogButtonBox::accepted, &dialog, [&] {
        const auto current = name->currentData().toString();
        auto next = current == oldName ? options : QJsonObject{};
        if ((current == "cmd" || current == "wasm") && path->text().isEmpty()) {
            error->setText(fc::text("Select a plugin file."));
            return;
        }
        if (current == "modbus_rtu")
            next["role"] = role->currentData().toString();
        if (current == "cmd") {
            next["program"] = path->text();
            next["timeout_ms"] = timeout->value();
            QJsonArray list;
            if (!args->toPlainText().isEmpty())
                for (const auto &arg : args->toPlainText().split('\n'))
                    list.append(arg);
            next["args"] = list;
        }
        if (current == "wasm") {
            bool ok = false;
            const auto budget = fuel->text().toLongLong(&ok);
            if (!ok || budget < 1) {
                error->setText(fc::text("Instruction budget must be a positive integer."));
                return;
            }
            next["path"] = path->text();
            next["fuel"] = budget;
        }
        spec = current == "none" ? QJsonObject{} : QJsonObject{{"name", current}, {"options", next}};
        dialog.accept();
    });
    return dialog.exec() == QDialog::Accepted;
}
