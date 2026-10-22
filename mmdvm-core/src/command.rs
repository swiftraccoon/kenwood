// Portions of this file are derived from MMDVMHost by Jonathan Naylor
// G4KLX, Copyright (C) 2015-2026, licensed under GPL-2.0-or-later.
// See LICENSE for full attribution.

//! Typed command identifiers, framing bytes, mode bytes and capability masks.
//!
//! Values follow `MMDVMHost/Modem.cpp:42-126` and the ordinary protocol modes
//! in `MMDVM/Globals.h`.
//! See the [frame example](crate#encode-and-decode-a-frame).

use std::fmt;

/// A command or response identifier, including an unrecognized wire byte.
///
/// Every byte is representable so frames from another firmware revision retain
/// their identifier. Associated constants name the commands recognized here;
/// interpreting their payloads is a separate operation.
/// See the [frame example](crate#encode-and-decode-a-frame).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Command(u8);

impl Command {
    /// Request firmware version information.
    pub const GET_VERSION: Self = Self(0x00);
    /// Request current modem status.
    pub const GET_STATUS: Self = Self(0x01);
    /// Set modem configuration.
    pub const SET_CONFIG: Self = Self(0x02);
    /// Set modem operating mode.
    pub const SET_MODE: Self = Self(0x03);
    /// Set modem RX/TX frequency.
    pub const SET_FREQ: Self = Self(0x04);
    /// Send a continuous-wave identifier.
    pub const SEND_CWID: Self = Self(0x0A);
    /// D-STAR radio header.
    pub const DSTAR_HEADER: Self = Self(0x10);
    /// D-STAR voice and slow data.
    pub const DSTAR_DATA: Self = Self(0x11);
    /// D-STAR signal lost indication.
    pub const DSTAR_LOST: Self = Self(0x12);
    /// D-STAR end of transmission.
    pub const DSTAR_EOT: Self = Self(0x13);
    /// DMR slot-1 data.
    pub const DMR_DATA1: Self = Self(0x18);
    /// DMR slot-1 signal lost indication.
    pub const DMR_LOST1: Self = Self(0x19);
    /// DMR slot-2 data.
    pub const DMR_DATA2: Self = Self(0x1A);
    /// DMR slot-2 signal lost indication.
    pub const DMR_LOST2: Self = Self(0x1B);
    /// DMR short link control.
    pub const DMR_SHORTLC: Self = Self(0x1C);
    /// DMR start of transmission.
    pub const DMR_START: Self = Self(0x1D);
    /// DMR transmission abort.
    pub const DMR_ABORT: Self = Self(0x1E);
    /// Yaesu System Fusion data.
    pub const YSF_DATA: Self = Self(0x20);
    /// Yaesu System Fusion signal lost indication.
    pub const YSF_LOST: Self = Self(0x21);
    /// P25 header.
    pub const P25_HDR: Self = Self(0x30);
    /// P25 logical data unit.
    pub const P25_LDU: Self = Self(0x31);
    /// P25 signal lost indication.
    pub const P25_LOST: Self = Self(0x32);
    /// NXDN data.
    pub const NXDN_DATA: Self = Self(0x40);
    /// NXDN signal lost indication.
    pub const NXDN_LOST: Self = Self(0x41);
    /// POCSAG paging data.
    pub const POCSAG_DATA: Self = Self(0x50);
    /// FM parameter set 1.
    pub const FM_PARAMS1: Self = Self(0x60);
    /// FM parameter set 2.
    pub const FM_PARAMS2: Self = Self(0x61);
    /// FM parameter set 3.
    pub const FM_PARAMS3: Self = Self(0x62);
    /// FM parameter set 4.
    pub const FM_PARAMS4: Self = Self(0x63);
    /// FM audio data.
    pub const FM_DATA: Self = Self(0x65);
    /// FM status.
    pub const FM_STATUS: Self = Self(0x66);
    /// FM end of transmission.
    pub const FM_EOT: Self = Self(0x67);
    /// FM received signal strength.
    pub const FM_RSSI: Self = Self(0x68);
    /// Acknowledgement response.
    pub const ACK: Self = Self(0x70);
    /// Negative acknowledgement response.
    pub const NAK: Self = Self(0x7F);
    /// Serial data passthrough.
    pub const SERIAL_DATA: Self = Self(0x80);
    /// Transparent data.
    pub const TRANSPARENT: Self = Self(0x90);
    /// Contact information.
    pub const QSO_INFO: Self = Self(0x91);
    /// Debug text with no appended integer values.
    pub const DEBUG1: Self = Self(0xF1);
    /// Debug text with one appended integer value.
    pub const DEBUG2: Self = Self(0xF2);
    /// Debug text with two appended integer values.
    pub const DEBUG3: Self = Self(0xF3);
    /// Debug text with three appended integer values.
    pub const DEBUG4: Self = Self(0xF4);
    /// Debug text with four appended integer values.
    pub const DEBUG5: Self = Self(0xF5);
    /// Debug memory dump.
    pub const DEBUG_DUMP: Self = Self(0xFA);

    /// Return the identifier's unchanged wire byte.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

impl From<u8> for Command {
    fn from(byte: u8) -> Self {
        Self(byte)
    }
}

impl From<Command> for u8 {
    fn from(command: Command) -> Self {
        command.get()
    }
}

impl fmt::Display for Command {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, formatter)
    }
}

impl fmt::LowerHex for Command {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::LowerHex::fmt(&self.0, formatter)
    }
}

impl fmt::UpperHex for Command {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::UpperHex::fmt(&self.0, formatter)
    }
}

impl fmt::Octal for Command {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Octal::fmt(&self.0, formatter)
    }
}

impl fmt::Binary for Command {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Binary::fmt(&self.0, formatter)
    }
}

/// Start byte of an MMDVM frame.
pub const MMDVM_FRAME_START: u8 = 0xE0;

/// D-STAR capability in the first capability byte.
pub const CAP1_DSTAR: u8 = 0x01;
/// DMR capability in the first capability byte.
pub const CAP1_DMR: u8 = 0x02;
/// Yaesu System Fusion capability in the first capability byte.
pub const CAP1_YSF: u8 = 0x04;
/// P25 capability in the first capability byte.
pub const CAP1_P25: u8 = 0x08;
/// NXDN capability in the first capability byte.
pub const CAP1_NXDN: u8 = 0x10;
/// FM capability in the first capability byte.
pub const CAP1_FM: u8 = 0x40;
/// POCSAG capability in the second capability byte.
pub const CAP2_POCSAG: u8 = 0x01;

/// Idle mode byte.
pub const MODE_IDLE: u8 = 0;
/// D-STAR mode byte.
pub const MODE_DSTAR: u8 = 1;
/// DMR mode byte.
pub const MODE_DMR: u8 = 2;
/// Yaesu System Fusion mode byte.
pub const MODE_YSF: u8 = 3;
/// P25 mode byte.
pub const MODE_P25: u8 = 4;
/// NXDN mode byte.
pub const MODE_NXDN: u8 = 5;
/// POCSAG mode byte.
pub const MODE_POCSAG: u8 = 6;
/// FM mode byte.
pub const MODE_FM: u8 = 10;

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn unknown_identifiers_and_numeric_formats_preserve_the_wire_byte() -> TestResult {
        for byte in 0..=u8::MAX {
            let command = Command::from(byte);
            assert_eq!(u8::from(command), byte);
            assert_eq!(command.to_string().parse::<u8>()?, byte);
            assert_eq!(u8::from_str_radix(&format!("{command:x}"), 16)?, byte);
            assert_eq!(u8::from_str_radix(&format!("{command:X}"), 16)?, byte);
            assert_eq!(u8::from_str_radix(&format!("{command:o}"), 8)?, byte);
            assert_eq!(u8::from_str_radix(&format!("{command:b}"), 2)?, byte);
        }
        assert_eq!(format!("{:#04X}", Command::NAK), "0x7F");
        Ok(())
    }
}
