/*
 * flattencom - Core Transport Serial Impl
 *
 * Adapts serialport I/O, settings, control lines and driver errors to the core transport interface.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

//! Physical serial adapter backed by the serialport crate.
//!
//! [`FlattenError::port_access`] classifies platform diagnostics heuristically;
//! busy, permission-denied and missing-device errors include recovery guidance.
//!
//! Adapter behavior:
//! - BREAK uses set_break, a timed wait and clear_break;
//! - DTR/RTS are write-only in the backend, so read_signals returns cached values;
//! - Native framing supports Mark/Space and five-data-bit 1.5-stop-bit modes;
//! - Exclusive opening uses builder exclusive/TIOCEXCL to prevent competing opens.

use std::io::{Read, Write};
use std::time::Duration;

use serialport::{
    ClearBuffer, DataBits as SpDataBits, FlowControl as SpFlow, Parity as SpParity, SerialPort,
    SerialPortBuilder, StopBits as SpStop,
};

use crate::FlattenError;
use crate::config::SerialConfig;
use crate::transport::{PinStates, SerialTransport, TransportFactory};

#[cfg(unix)]
type NativePort = serialport::TTYPort;
#[cfg(windows)]
type NativePort = serialport::COMPort;

/// Physical serial-port factory.
#[derive(Debug, Default)]
pub struct SerialPortFactory;

impl TransportFactory for SerialPortFactory {
    fn open(&self, cfg: &SerialConfig) -> Result<Box<dyn SerialTransport>, FlattenError> {
        cfg.validate()?;
        let builder: SerialPortBuilder = serialport::new(&cfg.path, cfg.baud)
            // Open a valid neutral format, then set the full native framing tuple
            // before exposing the transport to session workers.
            .data_bits(SpDataBits::Eight)
            .parity(SpParity::None)
            .stop_bits(SpStop::One)
            .flow_control(map_flow(cfg.flow_control));
        // Windows COM handles are opened with exclusive sharing by the backend.
        #[cfg(unix)]
        let builder = builder.exclusive(cfg.exclusive);
        let builder = if cfg.read_timeout_ms == 0 {
            // Zero means an indefinitely blocking read; configuration validation normally rejects it.
            builder.timeout(Duration::from_secs(1))
        } else {
            builder.timeout(Duration::from_millis(cfg.read_timeout_ms))
        };
        let mut port = builder.open_native().map_err(|e| {
            tracing::warn!(port = %cfg.path, error = %e, "Failed to open serial port");
            FlattenError::port_access(&cfg.path, &e.to_string())
        })?;
        apply_framing(&mut port, cfg)?;
        Ok(Box::new(SerialPortTransport {
            port,
            dtr: true,
            rts: true,
            dtr_known: false,
            rts_known: false,
        }))
    }

    fn kind(&self) -> &'static str {
        "serial"
    }
}

/// Physical serial transport instance.
#[derive(Debug)]
pub struct SerialPortTransport {
    port: NativePort,
    /// Cached DTR state; backend is write-only and dtr_on_open defaults to asserted.
    dtr: bool,
    /// Cached RTS state, with the same readback limitation.
    rts: bool,
    dtr_known: bool,
    rts_known: bool,
}

impl SerialTransport for SerialPortTransport {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, FlattenError> {
        match self.port.read(buf) {
            Ok(n) => Ok(n),
            // serialport reports idle reads as TimedOut; expose them as zero bytes available.
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => Ok(0),
            Err(e) => Err(FlattenError::Io(crate::tr!(
                "Failed to read {}: {e}",
                self.port.name().unwrap_or_default(),
                e = e
            ))),
        }
    }

    fn write(&mut self, data: &[u8]) -> Result<usize, FlattenError> {
        self.port
            .write(data)
            .map_err(|e| FlattenError::io(crate::tr!("Write failed: {e}", e = e)))
    }

    fn set_params(&mut self, cfg: &SerialConfig) -> Result<(), FlattenError> {
        cfg.validate()?;
        let r = (|| -> Result<(), FlattenError> {
            self.port.set_baud_rate(cfg.baud).map_err(io_err)?;
            self.port
                .set_flow_control(map_flow(cfg.flow_control))
                .map_err(io_err)?;
            // Data bits and stop bits must change together (5/1.5 <-> 8/2).
            apply_framing(&mut self.port, cfg)?;
            if cfg.read_timeout_ms > 0 {
                self.port
                    .set_timeout(Duration::from_millis(cfg.read_timeout_ms))
                    .map_err(io_err)?;
            }
            Ok(())
        })();
        r.map_err(|e| FlattenError::io(crate::tr!("Failed to configure port: {e}", e = e)))
    }

    fn set_signals(&mut self, dtr: Option<bool>, rts: Option<bool>) -> Result<(), FlattenError> {
        if let Some(v) = dtr {
            self.port.write_data_terminal_ready(v).map_err(io_err)?;
            self.dtr = v;
            self.dtr_known = true;
        }
        if let Some(v) = rts {
            self.port.write_request_to_send(v).map_err(io_err)?;
            self.rts = v;
            self.rts_known = true;
        }
        Ok(())
    }

    fn read_signals(&mut self) -> Result<PinStates, FlattenError> {
        let cts = self.port.read_clear_to_send().map_err(io_err)?;
        let dsr = self.port.read_data_set_ready().map_err(io_err)?;
        let ri = self.port.read_ring_indicator().map_err(io_err)?;
        let dcd = self.port.read_carrier_detect().map_err(io_err)?;
        // Return cached DTR/RTS values because the backend cannot read them back.
        Ok(PinStates {
            dtr: self.dtr,
            rts: self.rts,
            cts,
            dsr,
            dcd,
            ri,
            dtr_known: self.dtr_known,
            rts_known: self.rts_known,
        })
    }

    fn set_break(&mut self, duration: Duration) -> Result<(), FlattenError> {
        self.port.set_break().map_err(io_err)?;
        std::thread::sleep(duration);
        let clear_result = self.port.clear_break();
        // Attempt to clear BREAK and propagate any failure.
        clear_result.map_err(io_err)
    }

    fn flush_rx(&mut self) -> Result<(), FlattenError> {
        self.port
            .clear(ClearBuffer::Input)
            .map_err(|e| FlattenError::io(crate::tr!("Failed to clear receive buffer: {e}", e = e)))
    }

    fn flush_tx(&mut self) -> Result<(), FlattenError> {
        // Never enter the unbounded OS drain operation while holding the shared
        // transport lock. The session actor polls this bounded query instead.
        if self.port.bytes_to_write().map_err(io_err)? == 0 {
            Ok(())
        } else {
            Err(FlattenError::Timeout("Transmit buffer is not empty".into()))
        }
    }
}

fn io_err(e: serialport::Error) -> FlattenError {
    FlattenError::io(crate::tr!("Control line operation failed: {e}", e = e))
}

#[cfg(not(any(target_os = "linux", windows)))]
fn map_data_bits(d: crate::config::DataBits) -> SpDataBits {
    match d {
        crate::config::DataBits::Five => SpDataBits::Five,
        crate::config::DataBits::Six => SpDataBits::Six,
        crate::config::DataBits::Seven => SpDataBits::Seven,
        crate::config::DataBits::Eight => SpDataBits::Eight,
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
fn map_parity(p: crate::config::Parity) -> Result<SpParity, FlattenError> {
    Ok(match p {
        crate::config::Parity::None => SpParity::None,
        crate::config::Parity::Odd => SpParity::Odd,
        crate::config::Parity::Even => SpParity::Even,
        crate::config::Parity::Mark | crate::config::Parity::Space => {
            return Err(FlattenError::Unsupported(
                crate::i18n::text("Mark/Space parity is not supported; use None, Odd or Even")
                    .into(),
            ));
        }
    })
}

#[cfg(not(any(target_os = "linux", windows)))]
fn map_stop(s: crate::config::StopBits) -> Result<SpStop, FlattenError> {
    match s {
        crate::config::StopBits::One => Ok(SpStop::One),
        crate::config::StopBits::Two => Ok(SpStop::Two),
        crate::config::StopBits::OnePointFive => Err(FlattenError::Unsupported(
            "1.5 stop bits are not supported on this platform".into(),
        )),
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
fn apply_framing(port: &mut NativePort, cfg: &SerialConfig) -> Result<(), FlattenError> {
    let parity = map_parity(cfg.parity)?;
    let stop = map_stop(cfg.stop_bits)?;
    port.set_data_bits(map_data_bits(cfg.data_bits))
        .map_err(io_err)?;
    port.set_parity(parity).map_err(io_err)?;
    port.set_stop_bits(stop).map_err(io_err)
}

#[cfg(target_os = "linux")]
fn linux_framing(termios: &mut rustix::termios::Termios, cfg: &SerialConfig) {
    use crate::config::{DataBits, Parity, StopBits};
    use rustix::termios::{ControlModes as C, InputModes as I};
    termios
        .control_modes
        .remove(C::CSIZE | C::PARENB | C::PARODD | C::CMSPAR | C::CSTOPB);
    termios.control_modes.insert(match cfg.data_bits {
        DataBits::Five => C::CS5,
        DataBits::Six => C::CS6,
        DataBits::Seven => C::CS7,
        DataBits::Eight => C::CS8,
    });
    termios.control_modes.insert(match cfg.parity {
        Parity::None => C::empty(),
        Parity::Even => C::PARENB,
        Parity::Odd => C::PARENB | C::PARODD,
        Parity::Mark => C::PARENB | C::CMSPAR | C::PARODD,
        Parity::Space => C::PARENB | C::CMSPAR,
    });
    termios
        .control_modes
        .set(C::CSTOPB, cfg.stop_bits != StopBits::One);
    // Preserve all eight payload bits even with parity enabled. Never request
    // PARMRK byte expansion or silently discard bytes with parity errors.
    termios
        .input_modes
        .remove(I::ISTRIP | I::IGNPAR | I::PARMRK);
    termios
        .input_modes
        .set(I::INPCK, cfg.parity != Parity::None);
}

#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
fn apply_framing(port: &mut NativePort, cfg: &SerialConfig) -> Result<(), FlattenError> {
    use rustix::termios::{ControlModes as C, OptionalActions, tcgetattr, tcsetattr};
    use std::os::fd::{AsRawFd, BorrowedFd};
    // SAFETY: the borrowed descriptor is owned by port, which outlives this
    // function. It is never closed or transferred by the termios calls.
    let fd = unsafe { BorrowedFd::borrow_raw(port.as_raw_fd()) };
    let mut settings = tcgetattr(fd).map_err(|e| FlattenError::io(e.to_string()))?;
    linux_framing(&mut settings, cfg);
    tcsetattr(fd, OptionalActions::Now, &settings).map_err(|e| FlattenError::io(e.to_string()))?;
    let actual = tcgetattr(fd).map_err(|e| FlattenError::io(e.to_string()))?;
    let mask = C::CSIZE | C::PARENB | C::PARODD | C::CMSPAR | C::CSTOPB;
    if actual.control_modes & mask != settings.control_modes & mask {
        return Err(FlattenError::Unsupported(
            crate::i18n::text(
                "Serial driver did not retain the requested data bits, parity and stop bits",
            )
            .into(),
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn windows_framing(
    settings: &mut windows_sys::Win32::Devices::Communication::DCB,
    cfg: &SerialConfig,
) {
    use crate::config::{DataBits, Parity, StopBits};
    use windows_sys::Win32::Devices::Communication::{
        EVENPARITY, MARKPARITY, NOPARITY, ODDPARITY, ONE5STOPBITS, ONESTOPBIT, SPACEPARITY,
        TWOSTOPBITS,
    };
    settings.ByteSize = match cfg.data_bits {
        DataBits::Five => 5,
        DataBits::Six => 6,
        DataBits::Seven => 7,
        DataBits::Eight => 8,
    };
    settings.Parity = match cfg.parity {
        Parity::None => NOPARITY,
        Parity::Odd => ODDPARITY,
        Parity::Even => EVENPARITY,
        Parity::Mark => MARKPARITY,
        Parity::Space => SPACEPARITY,
    };
    settings.StopBits = match cfg.stop_bits {
        StopBits::One => ONESTOPBIT,
        StopBits::OnePointFive => ONE5STOPBITS,
        StopBits::Two => TWOSTOPBITS,
    };
    // DCB fParity is bit 1. Preserve flow-control and other unrelated flags.
    settings._bitfield = (settings._bitfield & !2) | (u32::from(cfg.parity != Parity::None) << 1);
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn apply_framing(port: &mut NativePort, cfg: &SerialConfig) -> Result<(), FlattenError> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Devices::Communication::{DCB, GetCommState, SetCommState};
    let handle = port.as_raw_handle();
    let mut settings = DCB {
        DCBlength: std::mem::size_of::<DCB>() as u32,
        ..Default::default()
    };
    // SAFETY: COMPort owns a live handle and each pointer addresses an initialized
    // DCB of DCBlength bytes for the duration of the synchronous Windows call.
    if unsafe { GetCommState(handle, &raw mut settings) } == 0 {
        return Err(FlattenError::io(
            std::io::Error::last_os_error().to_string(),
        ));
    }
    windows_framing(&mut settings, cfg);
    if unsafe { SetCommState(handle, &raw const settings) } == 0 {
        return Err(FlattenError::io(
            std::io::Error::last_os_error().to_string(),
        ));
    }
    let mut actual = DCB {
        DCBlength: std::mem::size_of::<DCB>() as u32,
        ..Default::default()
    };
    if unsafe { GetCommState(handle, &raw mut actual) } == 0 {
        return Err(FlattenError::io(
            std::io::Error::last_os_error().to_string(),
        ));
    }
    if (
        actual.ByteSize,
        actual.Parity,
        actual.StopBits,
        actual._bitfield & 2,
    ) != (
        settings.ByteSize,
        settings.Parity,
        settings.StopBits,
        settings._bitfield & 2,
    ) {
        return Err(FlattenError::Unsupported(
            crate::i18n::text(
                "Serial driver did not retain the requested data bits, parity and stop bits",
            )
            .into(),
        ));
    }
    Ok(())
}

fn map_flow(f: crate::config::FlowControl) -> SpFlow {
    match f {
        crate::config::FlowControl::None => SpFlow::None,
        crate::config::FlowControl::Software => SpFlow::Software,
        crate::config::FlowControl::Hardware => SpFlow::Hardware,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{DataBits, Parity, StopBits};

    #[test]
    #[cfg(target_os = "linux")]
    fn linux_native_framing_preserves_unrelated_settings_and_clears_stick_parity() {
        use rustix::termios::{ControlModes as C, InputModes as I, tcgetattr};
        let pair = nix::pty::openpty(None, None).unwrap();
        let mut settings = tcgetattr(&pair.slave).unwrap();
        settings.set_speed(123_457).unwrap();
        settings.control_modes.insert(C::CLOCAL | C::CRTSCTS);
        settings
            .input_modes
            .insert(I::IXON | I::ISTRIP | I::IGNPAR | I::PARMRK);
        let mut cfg = SerialConfig::new("test");
        for (parity, expected) in [
            (Parity::Mark, C::PARENB | C::CMSPAR | C::PARODD),
            (Parity::Space, C::PARENB | C::CMSPAR),
            (Parity::Odd, C::PARENB | C::PARODD),
            (Parity::Even, C::PARENB),
            (Parity::None, C::empty()),
        ] {
            cfg.parity = parity;
            for (bits, stop, expected_bits) in [
                (DataBits::Five, StopBits::OnePointFive, C::CS5 | C::CSTOPB),
                (DataBits::Eight, StopBits::Two, C::CS8 | C::CSTOPB),
                (DataBits::Seven, StopBits::One, C::CS7),
            ] {
                cfg.data_bits = bits;
                cfg.stop_bits = stop;
                linux_framing(&mut settings, &cfg);
                assert_eq!(
                    settings.control_modes & (C::PARENB | C::CMSPAR | C::PARODD),
                    expected
                );
                assert_eq!(
                    settings.control_modes & (C::CSIZE | C::CSTOPB),
                    expected_bits
                );
                assert!(settings.control_modes.contains(C::CLOCAL | C::CRTSCTS));
                assert!(settings.input_modes.contains(I::IXON));
                assert!(
                    !settings
                        .input_modes
                        .intersects(I::ISTRIP | I::IGNPAR | I::PARMRK)
                );
                assert_eq!(settings.input_speed(), 123_457);
                assert_eq!(settings.output_speed(), 123_457);
            }
        }
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn kernel_framing_verification_rejects_pty_silent_downgrade() {
        let pair = nix::pty::openpty(None, None).unwrap();
        use std::os::fd::AsRawFd;
        let path = std::fs::read_link(format!("/proc/self/fd/{}", pair.slave.as_raw_fd())).unwrap();
        let mut cfg = SerialConfig::new(path.to_string_lossy());
        cfg.exclusive = false;
        cfg.baud = 123_457;
        let mut port = serialport::new(&cfg.path, cfg.baud)
            .exclusive(false)
            .open_native()
            .unwrap();
        apply_framing(&mut port, &cfg).unwrap();
        assert_eq!(port.baud_rate().unwrap(), cfg.baud);
        cfg.parity = Parity::Mark;
        // Linux PTYs do not retain PARENB: reject instead of reporting a working
        // physical mode based solely on tcsetattr's success return.
        assert!(matches!(
            apply_framing(&mut port, &cfg),
            Err(FlattenError::Unsupported(_))
        ));
        cfg.parity = Parity::None;
        apply_framing(&mut port, &cfg).unwrap();
        cfg.data_bits = DataBits::Five;
        cfg.stop_bits = StopBits::OnePointFive;
        assert!(apply_framing(&mut port, &cfg).is_err());
    }

    #[test]
    #[cfg(windows)]
    fn windows_native_framing_sets_dcb_tuple_and_preserves_flow_flags() {
        use windows_sys::Win32::Devices::Communication::{
            DCB, MARKPARITY, NOPARITY, ONE5STOPBITS, ONESTOPBIT, SPACEPARITY, TWOSTOPBITS,
        };
        let mut settings = DCB {
            _bitfield: 0xa555,
            BaudRate: 123_457,
            ..Default::default()
        };
        let mut cfg = SerialConfig::new("COM1");
        cfg.data_bits = DataBits::Five;
        cfg.stop_bits = StopBits::OnePointFive;
        cfg.parity = Parity::Mark;
        windows_framing(&mut settings, &cfg);
        assert_eq!(
            (settings.ByteSize, settings.Parity, settings.StopBits),
            (5, MARKPARITY, ONE5STOPBITS)
        );
        assert_eq!(settings._bitfield & !2, 0xa555 & !2);
        assert_eq!(settings._bitfield & 2, 2);
        cfg.data_bits = DataBits::Eight;
        cfg.stop_bits = StopBits::Two;
        cfg.parity = Parity::Space;
        windows_framing(&mut settings, &cfg);
        assert_eq!(
            (settings.ByteSize, settings.Parity, settings.StopBits),
            (8, SPACEPARITY, TWOSTOPBITS)
        );
        cfg.parity = Parity::None;
        cfg.stop_bits = StopBits::One;
        windows_framing(&mut settings, &cfg);
        assert_eq!(
            (settings.Parity, settings.StopBits, settings._bitfield & 2),
            (NOPARITY, ONESTOPBIT, 0)
        );
        assert_eq!(settings.BaudRate, 123_457);
    }
}
