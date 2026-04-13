// Portions of this file are derived from MMDVMHost by Jonathan Naylor
// G4KLX, Copyright (C) 2015-2026, licensed under GPL-2.0-or-later.
// See LICENSE for full attribution.

//! Parsed modem status response.
//!
//! The status command `0x01` returns a varying payload depending on
//! the firmware protocol version. Parsing is split into
//! [`ModemStatus::parse_v1`] and [`ModemStatus::parse_v2`] — the
//! caller is expected to know which version the modem speaks
//! (established via the `GetVersion` handshake).
//!
//! Payload layouts mirror `ref/MMDVMHost/Modem.cpp:818-946`.

use crate::error::MmdvmError;
use crate::mode::ModemMode;

/// Bit 0 of the state byte: TX active.
const STATE_TX: u8 = 0x01;
/// Bit 1: ADC overflow.
const STATE_ADC_OVERFLOW: u8 = 0x02;
/// Bit 2: RX buffer overflow.
const STATE_RX_OVERFLOW: u8 = 0x04;
/// Bit 3: TX buffer overflow.
const STATE_TX_OVERFLOW: u8 = 0x08;
/// Bit 4: lockout.
const STATE_LOCKOUT: u8 = 0x10;
/// Bit 5: DAC overflow.
const STATE_DAC_OVERFLOW: u8 = 0x20;
/// Bit 6: carrier detect.
const STATE_CD: u8 = 0x40;

/// Minimum payload length for a protocol-v1 status response.
///
/// v1 layout: proto(0), mode(1), state(2), dstarSpace(3). Earlier
/// fields (dmr/ysf/etc.) beyond byte 3 are optional and default to 0.
const MIN_V1_LEN: usize = 4;

/// Minimum payload length for a protocol-v2 status response.
///
/// v2 layout: mode(0), state(1), reserved(2), dstarSpace(3),
/// dmrSpace1(4), dmrSpace2(5), ysfSpace(6), p25Space(7),
/// nxdnSpace(8). FM and POCSAG live at 10 and 11 respectively and are
/// optional.
const MIN_V2_LEN: usize = 9;

/// The seven flag bits packed into the status byte.
///
/// Wrapped as a newtype to avoid `clippy::struct_excessive_bools` on
/// [`ModemStatus`]. The underlying byte is the one produced (and
/// consumed) by the MMDVM firmware.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StatusFlags(u8);

impl StatusFlags {
    /// Build a flags value from the raw status byte.
    #[must_use]
    pub const fn from_bits(bits: u8) -> Self {
        // We mask to the seven documented bits to keep equality stable
        // even if the firmware ever leaks stray bits in the reserved
        // top bit.
        Self(
            bits & (STATE_TX
                | STATE_ADC_OVERFLOW
                | STATE_RX_OVERFLOW
                | STATE_TX_OVERFLOW
                | STATE_LOCKOUT
                | STATE_DAC_OVERFLOW
                | STATE_CD),
        )
    }

    /// Raw status byte.
    #[must_use]
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// TX active.
    #[must_use]
    pub const fn tx(self) -> bool {
        (self.0 & STATE_TX) != 0
    }

    /// ADC input level overflowed.
    #[must_use]
    pub const fn adc_overflow(self) -> bool {
        (self.0 & STATE_ADC_OVERFLOW) != 0
    }

    /// RX buffer overflowed.
    #[must_use]
    pub const fn rx_overflow(self) -> bool {
        (self.0 & STATE_RX_OVERFLOW) != 0
    }

    /// TX buffer overflowed.
    #[must_use]
    pub const fn tx_overflow(self) -> bool {
        (self.0 & STATE_TX_OVERFLOW) != 0
    }

    /// Carrier-sense lockout active.
    #[must_use]
    pub const fn lockout(self) -> bool {
        (self.0 & STATE_LOCKOUT) != 0
    }

    /// DAC output level overflowed.
    #[must_use]
    pub const fn dac_overflow(self) -> bool {
        (self.0 & STATE_DAC_OVERFLOW) != 0
    }

    /// Carrier detected.
    #[must_use]
    pub const fn cd(self) -> bool {
        (self.0 & STATE_CD) != 0
    }
}

/// Parsed modem status response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModemStatus {
    /// Current modem mode.
    pub mode: ModemMode,
    /// Status flags (TX, overflow bits, lockout, CD).
    pub flags: StatusFlags,
    /// D-STAR TX FIFO slots free.
    pub dstar_space: u8,
    /// DMR slot-1 TX FIFO slots free.
    pub dmr_space1: u8,
    /// DMR slot-2 TX FIFO slots free.
    pub dmr_space2: u8,
    /// YSF TX FIFO slots free.
    pub ysf_space: u8,
    /// P25 TX FIFO slots free.
    pub p25_space: u8,
    /// NXDN TX FIFO slots free.
    pub nxdn_space: u8,
    /// POCSAG TX FIFO slots free.
    pub pocsag_space: u8,
    /// FM TX FIFO slots free (0 on protocol v1).
    pub fm_space: u8,
}

impl ModemStatus {
    /// TX active (delegates to [`StatusFlags::tx`]).
    #[must_use]
    pub const fn tx(&self) -> bool {
        self.flags.tx()
    }

    /// ADC input level overflowed.
    #[must_use]
    pub const fn adc_overflow(&self) -> bool {
        self.flags.adc_overflow()
    }

    /// RX buffer overflowed.
    #[must_use]
    pub const fn rx_overflow(&self) -> bool {
        self.flags.rx_overflow()
    }

    /// TX buffer overflowed.
    #[must_use]
    pub const fn tx_overflow(&self) -> bool {
        self.flags.tx_overflow()
    }

    /// Carrier-sense lockout active.
    #[must_use]
    pub const fn lockout(&self) -> bool {
        self.flags.lockout()
    }

    /// DAC output level overflowed.
    #[must_use]
    pub const fn dac_overflow(&self) -> bool {
        self.flags.dac_overflow()
    }

    /// Carrier detected.
    #[must_use]
    pub const fn cd(&self) -> bool {
        self.flags.cd()
    }

    /// Parse a protocol-v1 status response payload.
    ///
    /// # Errors
    ///
    /// Returns [`MmdvmError::InvalidStatusLength`] if the payload is
    /// shorter than the minimum v1 layout requires.
    pub fn parse_v1(payload: &[u8]) -> Result<Self, MmdvmError> {
        if payload.len() < MIN_V1_LEN {
            return Err(MmdvmError::InvalidStatusLength {
                len: payload.len(),
                min: MIN_V1_LEN,
            });
        }
        // Lint-safe fetch; `get(..)` never panics.
        let mode = ModemMode::from_byte(payload.get(1).copied().unwrap_or(0));
        let flags = StatusFlags::from_bits(payload.get(2).copied().unwrap_or(0));
        let dstar_space = payload.get(3).copied().unwrap_or(0);
        // Remaining fields are optional on v1.
        let dmr_space1 = payload.get(4).copied().unwrap_or(0);
        let dmr_space2 = payload.get(5).copied().unwrap_or(0);
        let ysf_space = payload.get(6).copied().unwrap_or(0);
        let p25_space = payload.get(7).copied().unwrap_or(0);
        let nxdn_space = payload.get(8).copied().unwrap_or(0);
        let pocsag_space = payload.get(9).copied().unwrap_or(0);

        let status = Self {
            mode,
            flags,
            dstar_space,
            dmr_space1,
            dmr_space2,
            ysf_space,
            p25_space,
            nxdn_space,
            pocsag_space,
            fm_space: 0,
        };
        log_overflow_warnings(status);
        Ok(status)
    }

    /// Parse a protocol-v2 status response payload.
    ///
    /// # Errors
    ///
    /// Returns [`MmdvmError::InvalidStatusLength`] if the payload is
    /// shorter than the minimum v2 layout requires.
    pub fn parse_v2(payload: &[u8]) -> Result<Self, MmdvmError> {
        if payload.len() < MIN_V2_LEN {
            return Err(MmdvmError::InvalidStatusLength {
                len: payload.len(),
                min: MIN_V2_LEN,
            });
        }
        let mode = ModemMode::from_byte(payload.first().copied().unwrap_or(0));
        let flags = StatusFlags::from_bits(payload.get(1).copied().unwrap_or(0));
        // payload[2] is unused / reserved.
        let dstar_space = payload.get(3).copied().unwrap_or(0);
        let dmr_space1 = payload.get(4).copied().unwrap_or(0);
        let dmr_space2 = payload.get(5).copied().unwrap_or(0);
        let ysf_space = payload.get(6).copied().unwrap_or(0);
        let p25_space = payload.get(7).copied().unwrap_or(0);
        let nxdn_space = payload.get(8).copied().unwrap_or(0);
        // payload[9] is reserved / older FM. FM_SPACE is at index 10
        // per the reference implementation.
        let fm_space = payload.get(10).copied().unwrap_or(0);
        let pocsag_space = payload.get(11).copied().unwrap_or(0);

        let status = Self {
            mode,
            flags,
            dstar_space,
            dmr_space1,
            dmr_space2,
            ysf_space,
            p25_space,
            nxdn_space,
            pocsag_space,
            fm_space,
        };
        log_overflow_warnings(status);
        Ok(status)
    }
}

/// Emit a `tracing::warn!` event for each overflow flag that's set,
/// mirroring the `LogError` calls in
/// `ref/MMDVMHost/Modem.cpp:826-837,883-894`. Keeping the logging in
/// the sans-io core is fine because `tracing` is a passive facade —
/// no subscriber means no I/O.
fn log_overflow_warnings(status: ModemStatus) {
    if status.adc_overflow() {
        tracing::warn!("MMDVM ADC levels have overflowed");
    }
    if status.rx_overflow() {
        tracing::warn!("MMDVM RX buffer has overflowed");
    }
    if status.tx_overflow() {
        tracing::warn!("MMDVM TX buffer has overflowed");
    }
    if status.dac_overflow() {
        tracing::warn!("MMDVM DAC levels have overflowed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn v1_minimum_payload() -> TestResult {
        // proto=1, mode=DStar, state=0 (all flags clear), dstar_space=10.
        let payload = [1, 1, 0, 10];
        let s = ModemStatus::parse_v1(&payload)?;
        assert_eq!(s.mode, ModemMode::DStar);
        assert!(!s.tx());
        assert_eq!(s.dstar_space, 10);
        assert_eq!(s.fm_space, 0, "v1 always reports 0 FM space");
        Ok(())
    }

    #[test]
    fn v1_too_short_errors() {
        let err = ModemStatus::parse_v1(&[1, 1, 0]);
        assert!(
            matches!(err, Err(MmdvmError::InvalidStatusLength { len: 3, min: 4 })),
            "got {err:?}"
        );
    }

    #[test]
    fn v2_too_short_errors() {
        let err = ModemStatus::parse_v2(&[0, 0, 0, 0]);
        assert!(
            matches!(err, Err(MmdvmError::InvalidStatusLength { len: 4, min: 9 })),
            "got {err:?}"
        );
    }

    #[test]
    fn v2_includes_fm_space() -> TestResult {
        // Index layout: mode=1(DStar), state=0, reserved=0, dstar=2,
        // dmr1=3, dmr2=4, ysf=5, p25=6, nxdn=7, reserved=0, fm=8, pocsag=9.
        let payload = [1, 0, 0, 2, 3, 4, 5, 6, 7, 0, 8, 9];
        let s = ModemStatus::parse_v2(&payload)?;
        assert_eq!(s.fm_space, 8);
        assert_eq!(s.pocsag_space, 9);
        assert_eq!(s.dstar_space, 2);
        Ok(())
    }

    #[test]
    fn flags_all_bits_detected() {
        let f = StatusFlags::from_bits(0x7F);
        assert!(f.tx());
        assert!(f.adc_overflow());
        assert!(f.rx_overflow());
        assert!(f.tx_overflow());
        assert!(f.lockout());
        assert!(f.dac_overflow());
        assert!(f.cd());
    }

    #[test]
    fn flags_reserved_bit_masked_off() {
        // Bit 7 (0x80) is reserved — from_bits must clear it.
        let f = StatusFlags::from_bits(0xFF);
        assert_eq!(f.bits(), 0x7F);
    }
}
