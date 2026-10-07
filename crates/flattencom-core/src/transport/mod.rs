/*
 * flattencom - Core Transport Mod
 *
 * Defines serial transport operations and routes physical or virtual connection factories.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

//! Serial transport abstraction.
//!
//! ## Design
//!
//! [`SerialTransport`] handles reads, writes, settings and control lines. Sessions
//! obtain it from [`TransportFactory`] and share a mutex-protected instance between workers,
//! so live settings apply to both directions.
//!
//! ## Transport implementations
//!
//! | Implementation | Purpose |
//! |------|------|
//! | [`serial_impl::SerialPortFactory`] | Physical `/dev/ttyUSB*` and `COM*` ports via serialport |
//! | [`virtual_port::VirtualFactory`] | Hardware-free echo/generator channels for tests and benchmarks |
//! | [`mock::MockFactory`] | Scripted IO and fault injection for unit/reconnect tests |
//!
//! [`TransportRegistry`] routes physical and virtual paths through one entry point.

pub mod mock;
pub mod serial_impl;
pub mod virtual_port;

use std::time::Duration;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::FlattenError;
use crate::config::SerialConfig;

/// Control lines: DTR/RTS are writable; CTS/DSR/DCD/RI are read-only.
///
/// Reset and bootloader entry depend on the board's control-line wiring.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
pub struct PinStates {
    /// Data terminal ready (writable).
    pub dtr: bool,
    /// Request to send (writable).
    pub rts: bool,
    /// Clear to send.
    pub cts: bool,
    /// Data set ready.
    pub dsr: bool,
    /// Data carrier detect.
    pub dcd: bool,
    /// Ring indicator.
    pub ri: bool,
    /// Whether DTR has been set by this process; initial state cannot be read back.
    #[serde(default)]
    pub dtr_known: bool,
    /// Whether RTS has been set by this process.
    #[serde(default)]
    pub rts_known: bool,
}

/// Shared serial transport; callers serialize access unless the implementation provides synchronization.
///
/// Reads return `Ok(0)` for idle timeouts and `Err` for fatal failures.
pub trait SerialTransport: Send {
    /// Blocking read of at most `buf.len()` bytes.
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, FlattenError>;

    /// Write bytes, allowing partial progress; return the number actually written.
    fn write(&mut self, data: &[u8]) -> Result<usize, FlattenError>;

    /// Apply baud, data bits, parity, stop bits, flow control and timeout settings.
    fn set_params(&mut self, cfg: &SerialConfig) -> Result<(), FlattenError>;

    /// Set DTR/RTS, leaving omitted values unchanged.
    fn set_signals(&mut self, dtr: Option<bool>, rts: Option<bool>) -> Result<(), FlattenError>;

    /// Read control-line states.
    ///
    /// Read methods take `&mut self` because driver pin-state APIs require mutable access;
    /// DTR/RTS are cached locally.
    fn read_signals(&mut self) -> Result<PinStates, FlattenError>;

    /// Assert BREAK for `duration`.
    fn set_break(&mut self, duration: Duration) -> Result<(), FlattenError>;

    /// Discard unread driver RX bytes.
    fn flush_rx(&mut self) -> Result<(), FlattenError>;

    /// Check or drain pending driver TX bytes according to the transport implementation.
    fn flush_tx(&mut self) -> Result<(), FlattenError>;
}

/// Shareable transport factory, also used for reopening after failure.
pub trait TransportFactory: Send + Sync {
    /// Open a transport instance.
    fn open(&self, cfg: &SerialConfig) -> Result<Box<dyn SerialTransport>, FlattenError>;

    /// Factory identifier: `serial`, `virtual` or `mock`.
    fn kind(&self) -> &'static str;

    /// Resolve a reconnect target. Test/custom factories retain their configured path.
    fn reconnect_config(&self, cfg: &SerialConfig) -> Result<SerialConfig, FlattenError> {
        Ok(cfg.clone())
    }

    /// Capture metadata before opening (custom transports need not enumerate USB).
    fn prepare_config(&self, cfg: &SerialConfig) -> SerialConfig {
        cfg.clone()
    }
}

/// Default registry for physical and virtual serial ports.
#[derive(Debug, Default)]
pub struct TransportRegistry {
    serial: serial_impl::SerialPortFactory,
    virtual_factory: virtual_port::VirtualFactory,
}

/// Virtual-port URI prefix.
pub const VIRTUAL_PREFIX: &str = "virtual://";

impl TransportRegistry {
    /// Create a registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether a path denotes a virtual port.
    #[must_use]
    pub fn is_virtual(path: &str) -> bool {
        path.starts_with(VIRTUAL_PREFIX)
    }
}

impl TransportFactory for TransportRegistry {
    fn prepare_config(&self, cfg: &SerialConfig) -> SerialConfig {
        let mut config = cfg.clone();
        if !Self::is_virtual(&cfg.path)
            && config.device_identity.is_none()
            && let Ok(ports) = crate::discovery::list_ports()
        {
            let path = std::fs::canonicalize(&cfg.path).unwrap_or_else(|_| cfg.path.clone().into());
            config.device_identity = ports
                .iter()
                .find(|p| {
                    std::fs::canonicalize(&p.path).unwrap_or_else(|_| p.path.clone().into()) == path
                })
                .and_then(crate::discovery::DeviceIdentity::from_port)
                .filter(|id| id.resolve(&ports).is_ok());
        }
        config
    }
    fn open(&self, cfg: &SerialConfig) -> Result<Box<dyn SerialTransport>, FlattenError> {
        if Self::is_virtual(&cfg.path) {
            self.virtual_factory.open(cfg)
        } else {
            self.serial.open(cfg)
        }
    }

    fn kind(&self) -> &'static str {
        "registry"
    }

    fn reconnect_config(&self, cfg: &SerialConfig) -> Result<SerialConfig, FlattenError> {
        let mut next = cfg.clone();
        if let Some(identity) = &cfg.device_identity {
            next.path = identity.resolve(&crate::discovery::list_ports()?)?;
        }
        Ok(next)
    }
}
