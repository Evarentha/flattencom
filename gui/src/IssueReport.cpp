/*
 * flattencom - Readable Issue Report
 *
 * Formats selected evidence, configuration, transmitted commands and markers as a text report.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

#include "IssueReport.h"
#include "Style.h"
#include <QDateTime>
#include <QJsonArray>
#include <QTextStream>

namespace {
QString clock(const QJsonValue &value) {
    return value.isDouble()
               ? QDateTime::fromMSecsSinceEpoch(value.toInteger() / 1000).toUTC().toString(Qt::ISODateWithMs)
               : QString("-");
}
QString singleLine(QString value) { return value.replace('\r', "\\r").replace('\n', "\\n"); }
void sent(QTextStream &out, const QJsonObject &frame) {
    if (frame.isEmpty())
        return;
    out << clock(frame.value("t_us")) << "  TX #" << frame.value("seq").toInteger() << "  "
        << singleLine(frame.value("source").toString()) << '\n';
    out << singleLine(frame.value("text").toString()) << '\n';
    if (frame.contains("hex"))
        out << "HEX: " << frame.value("hex").toString() << '\n';
    out << '\n';
}
} // namespace
QString fc::issueReport(const QJsonObject &bundle) {
    QString result;
    QTextStream out(&result);
    auto heading = [&out](const QString &label) { out << '\n' << label << '\n' << QString(48, '-') << '\n'; };
    out << text("Serial issue report") << '\n' << bundle.value("created_at").toString() << '\n';
    const auto session = bundle.value("session").toObject();
    const auto config = session.value("config").toObject();
    heading(text("Session"));
    out << text("Port") << ": " << config.value("path").toString(session.value("path").toString()) << '\n';
    out << text("Session ID") << ": " << session.value("session_id").toString() << '\n';
    out << text("Baud") << ": " << config.value("baud").toInt() << '\n';
    out << text("Data bits") << ": " << valueLabel(config.value("data_bits").toString()) << "  "
        << text("Parity") << ": " << valueLabel(config.value("parity").toString()) << "  "
        << text("Stop bits") << ": " << valueLabel(config.value("stop_bits").toString()) << '\n';
    const auto evidence = bundle.value("selected_evidence").toObject();
    heading(text("Selected log"));
    if (evidence.contains("from_seq"))
        out << text("Sequence") << ": " << evidence.value("from_seq").toInteger() << " - "
            << evidence.value("to_seq").toInteger() - 1 << '\n';
    if (evidence.contains("from_us"))
        out << clock(evidence.value("from_us")) << " - " << clock(evidence.value("to_us")) << '\n';
    out << '\n' << evidence.value("text").toString() << '\n';
    heading(text("Last transmission before selection"));
    sent(out, bundle.value("preceding_send").toObject());
    heading(text("Transmissions in selection"));
    for (const auto &frame : bundle.value("sent_in_range").toArray())
        sent(out, frame.toObject());
    heading(text("Markers and boot events"));
    for (const auto &v : bundle.value("markers").toArray()) {
        const auto marker = v.toObject();
        out << clock(marker.value("t_us")) << "  " << valueLabel(marker.value("kind").toString()) << "  #"
            << marker.value("seq").toInteger() << "  " << singleLine(marker.value("source").toString())
            << '\n'
            << marker.value("label").toString() << "\n\n";
    }
    out << text("Times are host timestamps. Transmissions do not confirm device execution.") << '\n';
    out << text("This report contains selected evidence, not the full capture.") << '\n';
    return result;
}
