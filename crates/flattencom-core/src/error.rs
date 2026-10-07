/*
 * flattencom - Core Error
 *
 * Defines stable error categories, localized messages and port-access recovery hints.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

//! Shared error classification.
//!
//! Design:
//! - Actionable errors carry recovery guidance (`hint`) for users and RPC/MCP clients;
//! - Stable `code` values preserve semantics across Qt C++, Rust and JSON; see `docs/PROTOCOL.md`;
//! - Self-describing, extensible serialization as `{ "kind": "...", ...fields }`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Shared flattencom error categories.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FlattenError {
    /// Port is busy in another process.
    PortBusy {
        /// Port path.
        path: String,
        /// Recovery guidance.
        hint: String,
    },
    /// Permission denied when opening the port.
    PermissionDenied {
        /// Port path.
        path: String,
        /// Recovery guidance.
        hint: String,
    },
    /// Port does not exist or was unplugged.
    PortNotFound {
        /// Port path.
        path: String,
    },
    /// Session not found, possibly already closed.
    SessionNotFound(
        #[serde(with = "message_detail")]
        #[schemars(with = "MessageDetail")]
        String,
    ),
    /// Session is closed.
    SessionClosed(
        #[serde(with = "message_detail")]
        #[schemars(with = "MessageDetail")]
        String,
    ),
    /// Invalid configuration.
    InvalidConfig {
        /// Field name.
        field: String,
        /// Reason.
        reason: String,
    },
    /// Operation timed out.
    Timeout(
        #[serde(with = "message_detail")]
        #[schemars(with = "MessageDetail")]
        String,
    ),
    /// Input/output error.
    Io(
        #[serde(with = "message_detail")]
        #[schemars(with = "MessageDetail")]
        String,
    ),
    /// Operation unsupported by this platform or backend.
    Unsupported(
        #[serde(with = "message_detail")]
        #[schemars(with = "MessageDetail")]
        String,
    ),
    /// Decoder error, including unknown decoder or plugin failure.
    Decode(
        #[serde(with = "message_detail")]
        #[schemars(with = "MessageDetail")]
        String,
    ),
    /// Internal consistency error.
    Internal(
        #[serde(with = "message_detail")]
        #[schemars(with = "MessageDetail")]
        String,
    ),
}

// String variants retain their Rust API but carry an object on the wire so the
// internal `kind` tag can be combined with a named, round-trippable detail.
#[derive(Deserialize, JsonSchema)]
#[schemars(inline)]
struct MessageDetail {
    message: String,
}

mod message_detail {
    use serde::ser::SerializeStruct;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(message: &str, serializer: S) -> Result<S::Ok, S::Error> {
        let mut detail = serializer.serialize_struct("MessageDetail", 1)?;
        detail.serialize_field("message", message)?;
        detail.end()
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
        super::MessageDetail::deserialize(deserializer).map(|detail| detail.message)
    }
}

impl std::error::Error for FlattenError {}
impl std::fmt::Display for FlattenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::PortBusy { path, .. } => crate::tr!("Port is busy: {path}", path = path),
            Self::PermissionDenied { path, .. } => {
                crate::tr!("Permission denied: {path}", path = path)
            }
            Self::PortNotFound { path } => crate::tr!("Port not found: {path}", path = path),
            Self::SessionNotFound(id) => crate::tr!("Session not found: {0}", id),
            Self::SessionClosed(id) => crate::tr!("Session closed: {0}", id),
            Self::InvalidConfig { field, reason } => crate::tr!(
                "Invalid configuration: {field}: {reason}",
                field = field,
                reason = crate::i18n::text(reason)
            ),
            Self::Timeout(message) => {
                crate::tr!("Operation timed out: {0}", crate::i18n::text(message))
            }
            Self::Io(message) => crate::tr!("I/O error: {0}", crate::i18n::text(message)),
            Self::Unsupported(message) => {
                crate::tr!("Unsupported operation: {0}", crate::i18n::text(message))
            }
            Self::Decode(message) => crate::tr!("Decoder error: {0}", crate::i18n::text(message)),
            Self::Internal(message) => {
                crate::tr!("Internal error: {0}", crate::i18n::text(message))
            }
        };
        f.write_str(&message)
    }
}

impl FlattenError {
    /// Stable RPC `error.code`; see the error-code table in `docs/PROTOCOL.md`.
    #[must_use]
    pub fn code(&self) -> i64 {
        match self {
            Self::PortBusy { .. } => 100,
            Self::PermissionDenied { .. } => 101,
            Self::PortNotFound { .. } => 102,
            Self::SessionNotFound(_) => 103,
            Self::SessionClosed(_) => 104,
            Self::InvalidConfig { .. } => 105,
            Self::Timeout(_) => 106,
            Self::Io(_) => 107,
            Self::Unsupported(_) => 108,
            Self::Decode(_) => 109,
            Self::Internal(_) => 110,
        }
    }

    /// Readable recovery guidance and commands; empty when unavailable.
    #[must_use]
    pub fn hint(&self) -> &str {
        crate::i18n::text(match self {
            Self::PortBusy { hint, .. } | Self::PermissionDenied { hint, .. } => hint,
            Self::PortNotFound { .. } => "Run flattencom list to see available ports.",
            Self::SessionNotFound(_) => "Run flattencom sessions to see existing sessions.",
            _ => "",
        })
    }

    /// Classify port-access errors, including platforms that report busy ports as permission denied.
    #[must_use]
    pub fn port_access(path: &str, raw: &str) -> Self {
        let l = raw.to_ascii_lowercase();
        let is_perm =
            l.contains("permission") || l.contains("access is denied") || l.contains("拒绝");
        let is_missing = l.contains("no such file")
            || l.contains("not found")
            || l.contains("enoent")
            || l.contains("cannot find")
            || l.contains("找不到");
        let is_busy = l.contains("busy") || l.contains("already open") || l.contains("in use");
        if is_perm {
            if cfg!(windows) {
                Self::PermissionDenied {
                    path: path.to_owned(),
                    hint: crate::i18n::text(
                        "Check port permissions and close applications using the port.",
                    )
                    .into(),
                }
            } else {
                Self::PermissionDenied {
                    path: path.to_owned(),
                    hint: crate::i18n::text("Check the device group with ls -l <port>. Usually dialout on Debian/Ubuntu or uucp on Arch/CachyOS. Join that group and sign in again.").into(),
                }
            }
        } else if is_busy {
            Self::PortBusy {
                path: path.to_owned(),
                hint:
                    "Close the application using the port and retry. Use flattencom attach to join an existing session."
                        .into(),
            }
        } else if is_missing {
            Self::PortNotFound {
                path: path.to_owned(),
            }
        } else {
            Self::Io(format!("{path}:{raw}"))
        }
    }

    /// General IO error.
    #[must_use]
    pub fn io(raw: impl Into<String>) -> Self {
        Self::Io(raw.into())
    }

    /// Internal assertion or consistency error.
    #[must_use]
    pub fn internal(raw: impl Into<String>) -> Self {
        Self::Internal(raw.into())
    }

    /// Serialize to a JSON value for RPC/MCP error details.
    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_else(
            |_| serde_json::json!({ "kind": "internal", "message": "Failed to serialize error" }),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_error_preserves_wire_details_codes_and_schema() {
        let cases = [
            (
                FlattenError::PortBusy {
                    path: "port".into(),
                    hint: "retry".into(),
                },
                serde_json::json!({"kind":"port_busy", "path":"port", "hint":"retry"}),
            ),
            (
                FlattenError::PermissionDenied {
                    path: "port".into(),
                    hint: "permissions".into(),
                },
                serde_json::json!({"kind":"permission_denied", "path":"port", "hint":"permissions"}),
            ),
            (
                FlattenError::PortNotFound {
                    path: "port".into(),
                },
                serde_json::json!({"kind":"port_not_found", "path":"port"}),
            ),
            (
                FlattenError::SessionNotFound("session".into()),
                serde_json::json!({"kind":"session_not_found", "message":"session"}),
            ),
            (
                FlattenError::SessionClosed("session".into()),
                serde_json::json!({"kind":"session_closed", "message":"session"}),
            ),
            (
                FlattenError::InvalidConfig {
                    field: "baud".into(),
                    reason: "invalid".into(),
                },
                serde_json::json!({"kind":"invalid_config", "field":"baud", "reason":"invalid"}),
            ),
            (
                FlattenError::Timeout("deadline".into()),
                serde_json::json!({"kind":"timeout", "message":"deadline"}),
            ),
            (
                FlattenError::Io("设备\nfailed".into()),
                serde_json::json!({"kind":"io", "message":"设备\nfailed"}),
            ),
            (
                FlattenError::Unsupported("operation".into()),
                serde_json::json!({"kind":"unsupported", "message":"operation"}),
            ),
            (
                FlattenError::Decode("plugin".into()),
                serde_json::json!({"kind":"decode", "message":"plugin"}),
            ),
            (
                FlattenError::Internal(String::new()),
                serde_json::json!({"kind":"internal", "message":""}),
            ),
        ];
        let schema = serde_json::to_value(schemars::schema_for!(FlattenError)).unwrap();
        let variants = schema["oneOf"].as_array().unwrap();
        assert_eq!(variants.len(), cases.len());
        for (index, (error, expected)) in cases.into_iter().enumerate() {
            assert_eq!(error.code(), 100 + i64::try_from(index).unwrap());
            assert_eq!(error.to_json(), expected);
            let wire = serde_json::to_string(&error).unwrap();
            assert_eq!(serde_json::from_str::<FlattenError>(&wire).unwrap(), error);
            let variant = variants
                .iter()
                .find(|variant| variant["properties"]["kind"]["const"] == expected["kind"])
                .unwrap();
            assert_eq!(variant["type"], "object");
            let required = variant["required"].as_array().unwrap();
            for field in expected.as_object().unwrap().keys() {
                assert_eq!(variant["properties"][field]["type"], "string", "{variant}");
                assert!(required.contains(&serde_json::json!(field)), "{variant}");
            }
        }
        assert!(serde_json::from_value::<FlattenError>(serde_json::json!({"kind":"io"})).is_err());
    }

    #[test]
    fn 分类端口访问错误() {
        let e = FlattenError::port_access("/dev/ttyUSB0", "Permission denied (os error 13)");
        assert!(matches!(e, FlattenError::PermissionDenied { .. }));
        assert!(!e.hint().is_empty(), "{:?}", e.hint().is_empty());
        let e = FlattenError::port_access("COM3", "The system cannot find the file specified.");
        assert!(matches!(e, FlattenError::PortNotFound { .. }));
    }

    #[test]
    fn 错误码稳定() {
        assert_eq!(
            FlattenError::PortBusy {
                path: "x".into(),
                hint: String::new()
            }
            .code(),
            100
        );
        assert_eq!(FlattenError::SessionNotFound("s".into()).code(), 103);
    }

    #[test]
    fn 序列化自描述() {
        let e = FlattenError::PortNotFound {
            path: "COM9".into(),
        };
        let v = e.to_json();
        assert_eq!(v["kind"], "port_not_found");
        assert_eq!(v["path"], "COM9");
    }
}
