// Portions of this file are derived from MMDVMHost by Jonathan Naylor
// G4KLX, Copyright (C) 2015-2026, licensed under GPL-2.0-or-later.
// See LICENSE for full attribution.

//! MMDVM command byte constants.
//!
//! Every protocol byte used in the MMDVM wire format — command IDs,
//! response IDs, mode bytes, and capability bitfield masks.
//!
//! Mirrors the `MMDVM_*` constants at the top of
//! `ref/MMDVMHost/Modem.cpp:42-113` and the `MODE_*` constants in
//! `ref/MMDVMHost/Defines.h:31-44`.

// ---------------------------------------------------------------------------
// Framing
// ---------------------------------------------------------------------------

/// Start byte marker for all MMDVM frames (`0xE0`).
pub const MMDVM_FRAME_START: u8 = 0xE0;

// ---------------------------------------------------------------------------
// Core command / request IDs
// ---------------------------------------------------------------------------

/// Request firmware version information.
pub const MMDVM_GET_VERSION: u8 = 0x00;
/// Request current modem status.
pub const MMDVM_GET_STATUS: u8 = 0x01;
/// Set modem configuration.
pub const MMDVM_SET_CONFIG: u8 = 0x02;
/// Set modem operating mode.
pub const MMDVM_SET_MODE: u8 = 0x03;
/// Set modem RX/TX frequency.
pub const MMDVM_SET_FREQ: u8 = 0x04;

/// Send a CW ID.
pub const MMDVM_SEND_CWID: u8 = 0x0A;

// ---------------------------------------------------------------------------
// D-STAR
// ---------------------------------------------------------------------------

/// D-STAR radio header frame.
pub const MMDVM_DSTAR_HEADER: u8 = 0x10;
/// D-STAR voice+slow-data frame.
pub const MMDVM_DSTAR_DATA: u8 = 0x11;
/// D-STAR signal lost indication.
pub const MMDVM_DSTAR_LOST: u8 = 0x12;
/// D-STAR end-of-transmission marker.
pub const MMDVM_DSTAR_EOT: u8 = 0x13;

// ---------------------------------------------------------------------------
// DMR
// ---------------------------------------------------------------------------

/// DMR slot-1 data.
pub const MMDVM_DMR_DATA1: u8 = 0x18;
/// DMR slot-1 signal lost.
pub const MMDVM_DMR_LOST1: u8 = 0x19;
/// DMR slot-2 data.
pub const MMDVM_DMR_DATA2: u8 = 0x1A;
/// DMR slot-2 signal lost.
pub const MMDVM_DMR_LOST2: u8 = 0x1B;
/// DMR short LC.
pub const MMDVM_DMR_SHORTLC: u8 = 0x1C;
/// DMR start of transmission.
pub const MMDVM_DMR_START: u8 = 0x1D;
/// DMR abort current transmission.
pub const MMDVM_DMR_ABORT: u8 = 0x1E;

// ---------------------------------------------------------------------------
// Yaesu System Fusion (YSF)
// ---------------------------------------------------------------------------

/// YSF data frame.
pub const MMDVM_YSF_DATA: u8 = 0x20;
/// YSF signal lost.
pub const MMDVM_YSF_LOST: u8 = 0x21;

// ---------------------------------------------------------------------------
// P25
// ---------------------------------------------------------------------------

/// P25 header.
pub const MMDVM_P25_HDR: u8 = 0x30;
/// P25 LDU (logical data unit).
pub const MMDVM_P25_LDU: u8 = 0x31;
/// P25 signal lost.
pub const MMDVM_P25_LOST: u8 = 0x32;

// ---------------------------------------------------------------------------
// NXDN
// ---------------------------------------------------------------------------

/// NXDN data.
pub const MMDVM_NXDN_DATA: u8 = 0x40;
/// NXDN signal lost.
pub const MMDVM_NXDN_LOST: u8 = 0x41;

// ---------------------------------------------------------------------------
// POCSAG
// ---------------------------------------------------------------------------

/// POCSAG paging data.
pub const MMDVM_POCSAG_DATA: u8 = 0x50;

// ---------------------------------------------------------------------------
// FM
// ---------------------------------------------------------------------------

/// FM parameter set 1.
pub const MMDVM_FM_PARAMS1: u8 = 0x60;
/// FM parameter set 2.
pub const MMDVM_FM_PARAMS2: u8 = 0x61;
/// FM parameter set 3.
pub const MMDVM_FM_PARAMS3: u8 = 0x62;
/// FM parameter set 4.
pub const MMDVM_FM_PARAMS4: u8 = 0x63;
/// FM audio data.
pub const MMDVM_FM_DATA: u8 = 0x65;
/// FM status.
pub const MMDVM_FM_STATUS: u8 = 0x66;
/// FM end-of-transmission.
pub const MMDVM_FM_EOT: u8 = 0x67;
/// FM RSSI reading.
pub const MMDVM_FM_RSSI: u8 = 0x68;

// ---------------------------------------------------------------------------
// Responses
// ---------------------------------------------------------------------------

/// Acknowledgement response.
pub const MMDVM_ACK: u8 = 0x70;
/// Negative acknowledgement response.
pub const MMDVM_NAK: u8 = 0x7F;

// ---------------------------------------------------------------------------
// Serial passthrough / transparent / QSO info
// ---------------------------------------------------------------------------

/// Serial data passthrough.
pub const MMDVM_SERIAL_DATA: u8 = 0x80;

/// Transparent (bypass) frame.
pub const MMDVM_TRANSPARENT: u8 = 0x90;
/// QSO info frame.
pub const MMDVM_QSO_INFO: u8 = 0x91;

// ---------------------------------------------------------------------------
// Debug (emitted by radio firmware)
// ---------------------------------------------------------------------------

/// Firmware debug message, level 1.
pub const MMDVM_DEBUG1: u8 = 0xF1;
/// Firmware debug message, level 2.
pub const MMDVM_DEBUG2: u8 = 0xF2;
/// Firmware debug message, level 3.
pub const MMDVM_DEBUG3: u8 = 0xF3;
/// Firmware debug message, level 4.
pub const MMDVM_DEBUG4: u8 = 0xF4;
/// Firmware debug message, level 5.
pub const MMDVM_DEBUG5: u8 = 0xF5;
/// Firmware debug memory dump.
pub const MMDVM_DEBUG_DUMP: u8 = 0xFA;

// ---------------------------------------------------------------------------
// Capability bitfield bits (GetVersion response, protocol v2)
// ---------------------------------------------------------------------------

/// D-STAR capability bit in CAP1.
pub const CAP1_DSTAR: u8 = 0x01;
/// DMR capability bit in CAP1.
pub const CAP1_DMR: u8 = 0x02;
/// YSF capability bit in CAP1.
pub const CAP1_YSF: u8 = 0x04;
/// P25 capability bit in CAP1.
pub const CAP1_P25: u8 = 0x08;
/// NXDN capability bit in CAP1.
pub const CAP1_NXDN: u8 = 0x10;
/// FM capability bit in CAP1.
pub const CAP1_FM: u8 = 0x40;
/// POCSAG capability bit in CAP2.
pub const CAP2_POCSAG: u8 = 0x01;

// ---------------------------------------------------------------------------
// Mode byte values (for `SetMode` and the status response)
// ---------------------------------------------------------------------------

/// Idle mode (no active protocol).
pub const MODE_IDLE: u8 = 0;
/// D-STAR mode.
pub const MODE_DSTAR: u8 = 1;
/// DMR mode.
pub const MODE_DMR: u8 = 2;
/// YSF mode.
pub const MODE_YSF: u8 = 3;
/// P25 mode.
pub const MODE_P25: u8 = 4;
/// NXDN mode.
pub const MODE_NXDN: u8 = 5;
/// POCSAG paging mode.
pub const MODE_POCSAG: u8 = 6;
/// FM analog mode.
pub const MODE_FM: u8 = 10;
/// Continuous-wave (CW) ID mode.
pub const MODE_CW: u8 = 98;
/// Lockout (carrier sense) mode.
pub const MODE_LOCKOUT: u8 = 99;
/// Error mode.
pub const MODE_ERROR: u8 = 100;
/// Quit / shutdown mode.
pub const MODE_QUIT: u8 = 110;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_start_byte() {
        assert_eq!(MMDVM_FRAME_START, 0xE0);
    }

    #[test]
    fn core_command_ids_match_reference() {
        assert_eq!(MMDVM_GET_VERSION, 0x00);
        assert_eq!(MMDVM_GET_STATUS, 0x01);
        assert_eq!(MMDVM_SET_CONFIG, 0x02);
        assert_eq!(MMDVM_SET_MODE, 0x03);
        assert_eq!(MMDVM_SET_FREQ, 0x04);
        assert_eq!(MMDVM_SEND_CWID, 0x0A);
    }

    #[test]
    fn dstar_command_range() {
        assert_eq!(MMDVM_DSTAR_HEADER, 0x10);
        assert_eq!(MMDVM_DSTAR_DATA, 0x11);
        assert_eq!(MMDVM_DSTAR_LOST, 0x12);
        assert_eq!(MMDVM_DSTAR_EOT, 0x13);
    }

    #[test]
    fn response_codes() {
        assert_eq!(MMDVM_ACK, 0x70);
        assert_eq!(MMDVM_NAK, 0x7F);
    }

    #[test]
    fn capability_bits() {
        // CAP1 is a bitfield — each bit must be distinct.
        let cap1 = CAP1_DSTAR | CAP1_DMR | CAP1_YSF | CAP1_P25 | CAP1_NXDN | CAP1_FM;
        assert_eq!(cap1, 0x5F);
        assert_eq!(CAP2_POCSAG, 0x01);
    }

    #[test]
    fn mode_byte_values() {
        assert_eq!(MODE_IDLE, 0);
        assert_eq!(MODE_DSTAR, 1);
        assert_eq!(MODE_DMR, 2);
        assert_eq!(MODE_YSF, 3);
        assert_eq!(MODE_P25, 4);
        assert_eq!(MODE_NXDN, 5);
        assert_eq!(MODE_POCSAG, 6);
        assert_eq!(MODE_FM, 10);
        assert_eq!(MODE_CW, 98);
        assert_eq!(MODE_LOCKOUT, 99);
        assert_eq!(MODE_ERROR, 100);
        assert_eq!(MODE_QUIT, 110);
    }
}
