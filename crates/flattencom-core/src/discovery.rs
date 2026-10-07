/*
 * flattencom - Core Discovery
 *
 * Enumerates physical ports, resolves USB identities and polls device connection changes.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

//! Port discovery: enumeration and hotplug polling.
//!
//! Hotplug uses **poll-and-diff**, default 1500 ms, because the serial library lacks
//! a portable event source; Linux udev adds dependencies and Windows WM_DEVICECHANGE needs a GUI loop.
//! Polling provides consistent daemon/GUI behavior without additional privileges.

use std::collections::{BTreeMap, VecDeque};
use std::thread;
use std::time::Duration;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::FlattenError;

/// Port transport type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "lowercase")]
pub enum PortKind {
    /// USB serial port
    Usb,
    /// PCI serial port
    Pci,
    /// Bluetooth serial port
    Bluetooth,
    /// Unknown or legacy port
    #[default]
    Unknown,
}

impl PortKind {
    /// Name.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Usb => "usb",
            Self::Pci => "pci",
            Self::Bluetooth => "bluetooth",
            Self::Unknown => "unknown",
        }
    }
}

/// Port metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PortInfo {
    /// Port path, for example /dev/ttyUSB0 or COM3.
    pub path: String,
    /// Display name based on product or manufacturer and USB IDs.
    pub friendly_name: String,
    /// Transport type.
    pub kind: PortKind,
    /// USB vendor ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vid: Option<u16>,
    /// USB product ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u16>,
    /// Device serial number.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serial: Option<String>,
    /// Manufacturer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manufacturer: Option<String>,
    /// Product name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub product: Option<String>,
}

/// Stable USB identity. VID/PID alone cannot distinguish two identical adapters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DeviceIdentity {
    /// USB vendor ID.
    pub vid: u16,
    /// USB product ID.
    pub pid: u16,
    /// Nonempty USB serial number.
    pub serial: String,
}

impl DeviceIdentity {
    /// Capture only identities that have an actual serial number.
    #[must_use]
    pub fn from_port(port: &PortInfo) -> Option<Self> {
        Some(Self {
            vid: port.vid?,
            pid: port.pid?,
            serial: port
                .serial
                .as_ref()
                .filter(|s| !s.trim().is_empty())?
                .clone(),
        })
    }

    /// Resolve uniquely; ambiguity never falls back to an arbitrary matching device.
    pub fn resolve(&self, ports: &[PortInfo]) -> Result<String, FlattenError> {
        let matches: Vec<_> = ports
            .iter()
            .filter(|p| Self::from_port(p).as_ref() == Some(self))
            .collect();
        match matches.as_slice() {
            [port] => Ok(port.path.clone()),
            [] => Err(FlattenError::PortNotFound {
                path: format!("USB {:04X}:{:04X} {}", self.vid, self.pid, self.serial),
            }),
            _ => Err(FlattenError::InvalidConfig {
                field: "device_identity".into(),
                reason: crate::i18n::text(
                    "Multiple ports match this device identity; select a port manually",
                )
                .into(),
            }),
        }
    }
}

#[cfg(test)]
mod identity_tests {
    use super::*;
    #[test]
    fn renamed_missing_and_ambiguous_devices() {
        let mut port = PortInfo {
            path: "COM3".into(),
            friendly_name: "adapter".into(),
            kind: PortKind::Usb,
            vid: Some(1),
            pid: Some(2),
            serial: Some("A".into()),
            manufacturer: None,
            product: None,
        };
        let id = DeviceIdentity::from_port(&port).unwrap();
        port.path = "COM9".into();
        assert_eq!(id.resolve(&[port.clone()]).unwrap(), "COM9");
        assert!(id.resolve(&[]).is_err());
        assert!(id.resolve(&[port.clone(), port.clone()]).is_err());
        port.serial = Some("B".into());
        assert!(id.resolve(&[port.clone()]).is_err());
        port.serial = Some(" ".into());
        assert!(DeviceIdentity::from_port(&port).is_none());
    }
}

/// Enumerate system serial ports without opening them or requiring port access permission.
///
/// Sort by path; the daemon adds `busy` from its session table (RPC `list_ports`).
pub fn list_ports() -> Result<Vec<PortInfo>, FlattenError> {
    let ports = serialport::available_ports()
        .map_err(|e| FlattenError::io(format!("Failed to enumerate serial ports: {e}")))?;
    let mut out: Vec<PortInfo> = ports
        .into_iter()
        .map(|p| {
            let (kind, vid, pid, serial, manufacturer, product) = match &p.port_type {
                serialport::SerialPortType::UsbPort(u) => (
                    PortKind::Usb,
                    Some(u.vid),
                    Some(u.pid),
                    u.serial_number.clone(),
                    u.manufacturer.clone(),
                    u.product.clone(),
                ),
                serialport::SerialPortType::PciPort => {
                    (PortKind::Pci, None, None, None, None, None)
                }
                serialport::SerialPortType::BluetoothPort => {
                    (PortKind::Bluetooth, None, None, None, None, None)
                }
                serialport::SerialPortType::Unknown => {
                    (PortKind::Unknown, None, None, None, None, None)
                }
            };
            let friendly_name = product
                .clone()
                .or_else(|| manufacturer.clone())
                .unwrap_or_else(|| {
                    if let (Some(v), Some(p)) = (vid, pid) {
                        crate::tr!("USB serial port ({v:04X}:{p:04X})", p = p, v = v)
                    } else {
                        match kind {
                            PortKind::Bluetooth => {
                                crate::i18n::text("Bluetooth serial port").into()
                            }
                            PortKind::Pci => crate::i18n::text("PCI serial port").into(),
                            _ => p.port_name.clone(),
                        }
                    }
                });
            PortInfo {
                path: p.port_name,
                friendly_name,
                kind,
                vid,
                pid,
                serial,
                manufacturer,
                product,
            }
        })
        .collect();
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

/// Hotplug event from a port-list difference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "change", rename_all = "snake_case")]
pub enum HotplugEvent {
    /// Added ports with metadata.
    Added {
        /// List of added ports.
        ports: Vec<PortInfo>,
    },
    /// Removed ports identified by path.
    Removed {
        /// Removed port paths.
        paths: Vec<String>,
    },
}

/// Blocking hotplug watcher comparing port lists at configured intervals.
///
/// Usage: daemon and CLI `watch` repeatedly poll for changes.
pub struct HotplugWatcher {
    interval: Duration,
    prev: BTreeMap<String, PortInfo>,
    pending: VecDeque<HotplugEvent>,
}

impl HotplugWatcher {
    /// Start monitoring by taking an immediate baseline.
    pub fn start(interval: Duration) -> Result<Self, FlattenError> {
        let prev = list_ports()?
            .into_iter()
            .map(|p| (p.path.clone(), p))
            .collect::<BTreeMap<_, _>>();
        Ok(Self {
            interval: interval.max(Duration::from_millis(50)),
            prev,
            pending: VecDeque::new(),
        })
    }

    /// Wait for the polling interval and compare; return `None` when nothing changed.
    ///
    /// Call repeatedly until an event arrives or the caller stops monitoring.
    pub fn poll_once(&mut self) -> Result<Option<HotplugEvent>, FlattenError> {
        self.poll_once_until(|| false)
    }

    /// Poll with cancellation checked at most 50 ms apart while waiting.
    /// Cancellation returns no event and leaves the previous baseline intact.
    pub fn poll_once_until(
        &mut self,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<HotplugEvent>, FlattenError> {
        if cancelled() {
            return Ok(None);
        }
        if let Some(event) = self.pending.pop_front() {
            return Ok(Some(event));
        }
        let started = std::time::Instant::now();
        while started.elapsed() < self.interval {
            thread::sleep(
                self.interval
                    .saturating_sub(started.elapsed())
                    .min(Duration::from_millis(50)),
            );
            if cancelled() {
                return Ok(None);
            }
        }
        let cur: BTreeMap<String, PortInfo> = list_ports()?
            .into_iter()
            .map(|p| (p.path.clone(), p))
            .collect();
        let added: Vec<PortInfo> = cur
            .iter()
            .filter(|(k, _)| !self.prev.contains_key(*k))
            .map(|(_, v)| v.clone())
            .collect();
        let removed: Vec<String> = self
            .prev
            .keys()
            .filter(|k| !cur.contains_key(*k))
            .cloned()
            .collect();
        self.prev = cur;
        if !added.is_empty() {
            self.pending.push_back(HotplugEvent::Added { ports: added });
        }
        if !removed.is_empty() {
            self.pending
                .push_back(HotplugEvent::Removed { paths: removed });
        }
        Ok(self.pending.pop_front())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 枚举端口自检() {
        // Enumeration must succeed; an empty list is valid without hardware.
        let ports = list_ports().expect("枚举失败");
        assert!(ports.iter().all(|p| !p.path.is_empty()));
        // Verify sorting invariants.
        let mut sorted = ports.clone();
        sorted.sort_by(|a, b| a.path.cmp(&b.path));
        assert_eq!(ports, sorted);
    }

    #[test]
    fn 热插拔基线无事件() {
        let mut w = HotplugWatcher::start(Duration::from_millis(1)).unwrap();
        // Without device changes, the first poll must return None.
        assert!(w.poll_once().unwrap().is_none());
    }

    #[test]
    fn cancellation_interrupts_long_poll_without_enumerating_or_losing_baseline() {
        let mut watcher = HotplugWatcher {
            interval: Duration::from_secs(60),
            prev: BTreeMap::new(),
            pending: VecDeque::new(),
        };
        let start = std::time::Instant::now();
        assert!(
            watcher
                .poll_once_until(|| start.elapsed() >= Duration::from_millis(75))
                .unwrap()
                .is_none()
        );
        assert!(start.elapsed() < Duration::from_secs(1));
        watcher.pending.push_back(HotplugEvent::Removed {
            paths: vec!["test".into()],
        });
        assert!(watcher.poll_once_until(|| true).unwrap().is_none());
        assert!(watcher.poll_once_until(|| false).unwrap().is_some());
    }
}
