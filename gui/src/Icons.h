/*
 * flattencom - Application Vector Icons Interface
 *
 * Declares the types and operations for the application vector icons component.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

#pragma once
#include <QIcon>
namespace fc {
enum class Icon { Open, Configure, Save, Refresh, Insert, Delete };
QIcon icon(Icon kind);
} // namespace fc
