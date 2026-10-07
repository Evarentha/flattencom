/*
 * flattencom - Core I18N
 *
 * Selects process or request-scoped language and retrieves literal catalog translations.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

//! Application language selection. Protocol identifiers and device data are never translated.
use std::cell::Cell;
use std::sync::{
    OnceLock,
    atomic::{AtomicU8, Ordering},
};

/// Supported application languages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Language {
    /// English, the default.
    English,
    /// Simplified Chinese.
    Chinese,
}
impl Language {
    /// Parse an explicit language identifier.
    pub fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "en" | "en-us" | "en_us" => Some(Self::English),
            "zh" | "zh-cn" | "zh_cn" | "zh-hans" => Some(Self::Chinese),
            _ => None,
        }
    }
    /// Stable identifier used in RPC requests.
    pub fn code(self) -> &'static str {
        match self {
            Self::English => "en",
            Self::Chinese => "zh-CN",
        }
    }
}
static DEFAULT: AtomicU8 = AtomicU8::new(0);
thread_local! { static OVERRIDE: Cell<Option<Language>> = const { Cell::new(None) }; }

/// Initialize a standalone process using FLATTENCOM_LANG, defaulting to English.
pub fn init() {
    set_default(
        std::env::var("FLATTENCOM_LANG")
            .ok()
            .as_deref()
            .and_then(Language::parse)
            .unwrap_or(Language::English),
    );
}
/// Set the language used by a CLI/MCP process.
pub fn set_default(language: Language) {
    DEFAULT.store(u8::from(language == Language::Chinese), Ordering::Relaxed);
}
/// Current request language, or process default.
pub fn language() -> Language {
    OVERRIDE.with(Cell::get).unwrap_or_else(|| {
        if DEFAULT.load(Ordering::Relaxed) == 1 {
            Language::Chinese
        } else {
            Language::English
        }
    })
}
/// Execute a blocking request in its client's language. Restores state even on panic.
pub fn with_language<T>(language: Language, f: impl FnOnce() -> T) -> T {
    struct Restore(Option<Language>);
    impl Drop for Restore {
        fn drop(&mut self) {
            OVERRIDE.with(|v| v.set(self.0));
        }
    }
    let _restore = Restore(OVERRIDE.with(|v| v.replace(Some(language))));
    f()
}
/// Translate a complete application message template. Unknown messages stay in English.
pub fn text(english: &str) -> &str {
    static CATALOG: OnceLock<std::collections::HashMap<String, String>> = OnceLock::new();
    if language() == Language::English {
        return english;
    }
    CATALOG
        .get_or_init(|| {
            serde_json::from_str(include_str!("../locales/zh-CN.json"))
                .expect("valid Chinese catalog")
        })
        .get(english)
        .map_or(english, String::as_str)
}

// The generated macro keeps format strings literal so Rust checks arguments and
// preserves their bytes. Translation never performs replacements in rendered data.

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn languages_are_scoped_and_payloads_preserved() {
        assert_eq!(language(), Language::English);
        let payload = "中文日志 {path}";
        with_language(Language::Chinese, || {
            assert_eq!(
                text("Background service stop requested"),
                "已请求停止后台服务"
            );
            let error = crate::FlattenError::PortNotFound {
                path: payload.into(),
            }
            .to_string();
            assert!(error.starts_with("端口不存在"));
            assert!(error.ends_with(payload));
            with_language(Language::English, || {
                assert_eq!(
                    text("Background service stop requested"),
                    "Background service stop requested"
                );
            });
            assert_eq!(language(), Language::Chinese);
        });
        assert_eq!(language(), Language::English);
    }
}
