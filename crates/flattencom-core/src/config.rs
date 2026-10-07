/*
 * flattencom - Core Config
 *
 * Defines serial settings, buffer limits, reconnect policy and validated configuration patches.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

//! Serial session configuration and patches for live updates.

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::FlattenError;

/// Maximum session label length in UTF-8 bytes.
pub const MAX_SESSION_LABEL_BYTES: usize = 4096;

/// Data bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "lowercase")]
pub enum DataBits {
    /// 5 bits
    Five,
    /// 6 bits
    Six,
    /// 7 bits
    Seven,
    /// 8 bits (default)
    #[default]
    Eight,
}

impl DataBits {
    /// Name shared by serialization, deserialization and display.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Five => "five",
            Self::Six => "six",
            Self::Seven => "seven",
            Self::Eight => "eight",
        }
    }

    /// All values for configuration controls.
    pub fn all() -> [Self; 4] {
        [Self::Eight, Self::Seven, Self::Six, Self::Five]
    }
}

/// Parity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "lowercase")]
pub enum Parity {
    /// No parity (default)
    #[default]
    None,
    /// Odd parity
    Odd,
    /// Even parity
    Even,
    /// Mark parity (always 1)
    Mark,
    /// Space parity (always 0)
    Space,
}

impl Parity {
    /// Name.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Odd => "odd",
            Self::Even => "even",
            Self::Mark => "mark",
            Self::Space => "space",
        }
    }

    /// All supported values.
    pub fn all() -> [Self; 5] {
        [Self::None, Self::Even, Self::Odd, Self::Mark, Self::Space]
    }
}

/// Stop bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "lowercase")]
pub enum StopBits {
    /// 1 bit (default)
    #[default]
    One,
    /// 1.5 bits; requires five data bits.
    #[serde(rename = "one_point_five")]
    OnePointFive,
    /// 2 bits
    Two,
}

impl StopBits {
    /// Name.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::One => "one",
            Self::OnePointFive => "one_point_five",
            Self::Two => "two",
        }
    }

    /// All supported values.
    pub fn all() -> [Self; 3] {
        [Self::One, Self::OnePointFive, Self::Two]
    }
}

/// Flow control.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "lowercase")]
pub enum FlowControl {
    /// No flow control (default)
    #[default]
    None,
    /// Software (XON/XOFF)
    Software,
    /// Hardware (RTS/CTS)
    Hardware,
}

impl FlowControl {
    /// Name.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Software => "software",
            Self::Hardware => "hardware",
        }
    }

    /// All supported values.
    pub fn all() -> [Self; 3] {
        [Self::None, Self::Software, Self::Hardware]
    }
}

/// Reconnect policy after a transport read failure.
///
/// Enabled by default: 10 attempts, 200 ms initial delay, 5 s maximum.
/// Session history is retained across reconnects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum ReconnectPolicy {
    /// Do not reconnect automatically.
    Disabled,
    /// Reconnect automatically.
    Enabled {
        /// Maximum retry count.
        #[serde(default = "d_max_attempts")]
        max_attempts: u32,
        /// Initial retry delay in milliseconds.
        #[serde(default = "d_initial_ms")]
        initial_ms: u64,
        /// Maximum retry delay in milliseconds.
        #[serde(default = "d_max_ms")]
        max_ms: u64,
    },
}

impl Default for ReconnectPolicy {
    fn default() -> Self {
        Self::default_enabled()
    }
}

impl ReconnectPolicy {
    /// Default policy: 10 attempts, 200 ms initial delay, 5000 ms maximum delay.
    #[must_use]
    pub fn default_enabled() -> Self {
        Self::Enabled {
            max_attempts: 10,
            initial_ms: 200,
            max_ms: 5_000,
        }
    }

    /// Backoff before the specified `attempt`.
    #[must_use]
    pub fn backoff_ms(&self, attempt: u32) -> u64 {
        match self {
            Self::Disabled => u64::MAX,
            Self::Enabled {
                initial_ms, max_ms, ..
            } => {
                let shift = attempt.min(16);
                (*initial_ms).saturating_mul(1u64 << shift).min(*max_ms)
            }
        }
    }
}

fn d_max_attempts() -> u32 {
    10
}
fn d_initial_ms() -> u64 {
    200
}
fn d_max_ms() -> u64 {
    5_000
}

/// Ring buffer limits; oldest frames are evicted when a limit is reached.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BufferPolicy {
    /// Maximum retained frame count.
    #[serde(default = "d_max_frames")]
    pub max_frames: usize,
    /// Maximum retained bytes.
    #[serde(default = "d_max_bytes")]
    pub max_bytes: u64,
}

fn d_max_frames() -> usize {
    100_000
}
fn d_max_bytes() -> u64 {
    64 * 1024 * 1024
}

impl Default for BufferPolicy {
    fn default() -> Self {
        Self {
            max_frames: d_max_frames(),
            max_bytes: d_max_bytes(),
        }
    }
}

/// Serial session configuration.
///
/// Path accepts physical ports (/dev/ttyUSB0, COM3) or virtual ports (virtual://echo,
/// virtual://gen?bps=115200). See the transport module.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SerialConfig {
    /// Port path or virtual port URI.
    pub path: String,
    /// Captured hardware identity used on reconnect; absent for devices without a unique serial.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_identity: Option<crate::discovery::DeviceIdentity>,
    /// Baud rate; nonstandard values depend on driver support.
    #[serde(default = "d_baud")]
    pub baud: u32,
    /// Data bits.
    #[serde(default)]
    pub data_bits: DataBits,
    /// Parity.
    #[serde(default)]
    pub parity: Parity,
    /// Stop bits.
    #[serde(default)]
    pub stop_bits: StopBits,
    /// Flow control.
    #[serde(default)]
    pub flow_control: FlowControl,
    /// Read timeout in milliseconds (1 to 1000).
    ///
    /// Reader and writer share the transport handle; reads may delay writes.
    /// Default: 10 ms.
    #[serde(default = "d_read_timeout_ms")]
    pub read_timeout_ms: u64,
    /// Open exclusively; uses TIOCEXCL on Linux.
    #[serde(default = "default_exclusive")]
    pub exclusive: bool,
    /// Reconnect automatically.
    #[serde(default = "ReconnectPolicy::default_enabled")]
    pub auto_reconnect: ReconnectPolicy,
    /// Session label displayed in the GUI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Receive ring buffer limits.
    #[serde(default)]
    pub buffer: BufferPolicy,
    /// Record to one .log capture. Legacy .txt and explicit JSONL paths are also accepted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record_to: Option<PathBuf>,
    /// New raw RX byte-stream file. Unlike JSONL this contains no added timestamps or TX data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record_rx_to: Option<PathBuf>,
    /// Optional number of older text segments to retain. None keeps the full session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record_keep_segments: Option<u32>,
}

fn d_baud() -> u32 {
    115_200
}
fn d_read_timeout_ms() -> u64 {
    10
}

fn default_exclusive() -> bool {
    true
}

impl SerialConfig {
    /// Create 115200 8N1 settings with default reconnect and buffer policies.
    #[must_use]
    pub fn new(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            device_identity: None,
            baud: d_baud(),
            data_bits: DataBits::Eight,
            parity: Parity::None,
            stop_bits: StopBits::One,
            flow_control: FlowControl::None,
            read_timeout_ms: d_read_timeout_ms(),
            exclusive: true,
            auto_reconnect: ReconnectPolicy::default_enabled(),
            label: None,
            buffer: BufferPolicy::default(),
            record_to: None,
            record_rx_to: None,
            record_keep_segments: None,
        }
    }

    /// Validate path, label, baud rate, timeout and buffer bounds.
    pub fn validate(&self) -> Result<(), FlattenError> {
        if self.stop_bits == StopBits::OnePointFive && self.data_bits != DataBits::Five {
            return Err(FlattenError::InvalidConfig {
                field: "stop_bits".into(),
                reason: "1.5 stop bits require 5 data bits".into(),
            });
        }
        if self.stop_bits == StopBits::Two && self.data_bits == DataBits::Five {
            return Err(FlattenError::InvalidConfig {
                field: "stop_bits".into(),
                reason: "5 data bits require 1 or 1.5 stop bits".into(),
            });
        }
        if self
            .label
            .as_ref()
            .is_some_and(|label| label.len() > MAX_SESSION_LABEL_BYTES)
        {
            return Err(FlattenError::InvalidConfig {
                field: "label".into(),
                reason: "Session label exceeds 4096 UTF-8 bytes".into(),
            });
        }
        if self.record_to.is_some() && self.record_to == self.record_rx_to {
            return Err(FlattenError::InvalidConfig {
                field: "record_rx_to".into(),
                reason: "RX log and JSONL capture must use different files".into(),
            });
        }
        if self.path.trim().is_empty() {
            return Err(FlattenError::InvalidConfig {
                field: "path".into(),
                reason: crate::i18n::text("Must not be empty").into(),
            });
        }
        if self.baud == 0 {
            return Err(FlattenError::InvalidConfig {
                field: "baud".into(),
                reason: crate::i18n::text("Must be greater than zero").into(),
            });
        }
        if self.read_timeout_ms == 0 || self.read_timeout_ms > 1_000 {
            return Err(FlattenError::InvalidConfig {
                field: "read_timeout_ms".into(),
                reason: crate::i18n::text("Must be between 1 and 1000 ms").into(),
            });
        }
        if self.buffer.max_frames == 0 || self.buffer.max_bytes == 0 {
            return Err(FlattenError::InvalidConfig {
                field: "buffer".into(),
                reason: crate::i18n::text("max_frames and max_bytes must be greater than zero")
                    .into(),
            });
        }
        if self.buffer.max_frames > 1_000_000 || self.buffer.max_bytes > 1024 * 1024 * 1024 {
            return Err(FlattenError::InvalidConfig {
                field: "buffer".into(),
                reason: "maximum 1,000,000 chunks and 1 GiB per session".into(),
            });
        }
        Ok(())
    }
}

/// Configuration patch; only supplied fields are changed.
///
/// A supplied label replaces the current label; null clears it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
pub struct ConfigPatch {
    /// Baud rate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baud: Option<u32>,
    /// Data bits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_bits: Option<DataBits>,
    /// Parity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parity: Option<Parity>,
    /// Stop bits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_bits: Option<StopBits>,
    /// Flow control.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flow_control: Option<FlowControl>,
    /// Read timeout in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_timeout_ms: Option<u64>,
    /// Label, replaced when supplied in the patch.
    #[serde(
        default,
        deserialize_with = "nullable_label",
        skip_serializing_if = "Option::is_none"
    )]
    pub label: Option<Option<String>>,
}

impl ConfigPatch {
    /// Whether the patch changes nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.baud.is_none()
            && self.data_bits.is_none()
            && self.parity.is_none()
            && self.stop_bits.is_none()
            && self.flow_control.is_none()
            && self.read_timeout_ms.is_none()
            && self.label.is_none()
    }

    /// Apply the patch in place.
    pub fn apply_to(&self, cfg: &mut SerialConfig) {
        if let Some(v) = self.baud {
            cfg.baud = v;
        }
        if let Some(v) = self.data_bits {
            cfg.data_bits = v;
        }
        if let Some(v) = self.parity {
            cfg.parity = v;
        }
        if let Some(v) = self.stop_bits {
            cfg.stop_bits = v;
        }
        if let Some(v) = self.flow_control {
            cfg.flow_control = v;
        }
        if let Some(v) = self.read_timeout_ms {
            cfg.read_timeout_ms = v;
        }
        if let Some(v) = self.label.as_ref() {
            cfg.label.clone_from(v);
        }
    }
}

/// Preserve explicit JSON null in patches; serde's nested Option otherwise
/// treats both an absent property and null as None.
pub fn nullable_label<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Option<String>>, D::Error> {
    Option::<String>::deserialize(deserializer).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stop_bits_wire_values_and_data_bit_combinations() {
        for stop in StopBits::all() {
            let wire = serde_json::to_value(stop).unwrap();
            assert_eq!(wire, stop.as_str());
            assert_eq!(serde_json::from_value::<StopBits>(wire).unwrap(), stop);
            for bits in DataBits::all() {
                let mut cfg = SerialConfig::new("virtual://echo");
                cfg.data_bits = bits;
                cfg.stop_bits = stop;
                let valid = match stop {
                    StopBits::One => true,
                    StopBits::OnePointFive => bits == DataBits::Five,
                    StopBits::Two => bits != DataBits::Five,
                };
                assert_eq!(cfg.validate().is_ok(), valid, "{bits:?}/{stop:?}");
            }
        }
        let schema = serde_json::to_value(schemars::schema_for!(StopBits)).unwrap();
        assert!(schema.to_string().contains("one_point_five"));
        let mut cfg = SerialConfig::new("virtual://echo");
        let patch: ConfigPatch = serde_json::from_value(serde_json::json!({
            "data_bits": "five", "parity": "mark", "stop_bits": "one_point_five"
        }))
        .unwrap();
        patch.apply_to(&mut cfg);
        assert!(cfg.validate().is_ok());
        assert_eq!(cfg.parity, Parity::Mark);
    }

    #[test]
    fn 默认配置_115200_8n1() {
        let c = SerialConfig::new("/dev/ttyUSB0");
        assert_eq!(c.baud, 115_200);
        assert_eq!(c.data_bits, DataBits::Eight);
        assert_eq!(c.parity, Parity::None);
        assert_eq!(c.stop_bits, StopBits::One);
        assert_eq!(c.flow_control, FlowControl::None);
        assert!(c.validate().is_ok());
    }

    #[test]
    fn 校验拒绝非法值() {
        let mut c = SerialConfig::new("COM3");
        c.baud = 0;
        assert!(c.validate().is_err());
        c.baud = 9_600;
        c.read_timeout_ms = 5_000;
        assert!(c.validate().is_err());
    }

    #[test]
    fn 补丁语义() {
        let mut c = SerialConfig::new("x");
        let p = ConfigPatch {
            baud: Some(9_600),
            label: Some(Some("demo".into())),
            ..Default::default()
        };
        p.apply_to(&mut c);
        assert_eq!(c.baud, 9_600);
        assert_eq!(c.label.as_deref(), Some("demo"));
        let clear = ConfigPatch {
            label: Some(None),
            ..Default::default()
        };
        clear.apply_to(&mut c);
        assert_eq!(c.label, None);
    }

    #[test]
    fn 重连退避指数封顶() {
        let p = ReconnectPolicy::default_enabled();
        assert_eq!(p.backoff_ms(0), 200);
        assert_eq!(p.backoff_ms(1), 400);
        assert_eq!(p.backoff_ms(4), 3_200);
        // attempt=5 gives 200*32=6400, capped at max_ms=5000.
        assert_eq!(p.backoff_ms(5), 5_000);
        assert_eq!(p.backoff_ms(20), 5_000);
    }

    #[test]
    fn 序列化小写命名() {
        let c = SerialConfig::new("/dev/ttyUSB0");
        let v = serde_json::to_value(&c).unwrap();
        assert_eq!(v["data_bits"], "eight");
        assert_eq!(v["parity"], "none");
        assert_eq!(v["stop_bits"], "one");
        assert_eq!(v["flow_control"], "none");
    }
}
