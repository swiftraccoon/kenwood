// Portions of this file are derived from MMDVMHost by Jonathan Naylor
// G4KLX, Copyright (C) 2015-2026, licensed under GPL-2.0-or-later.
// See LICENSE for full attribution.

//! Modem `SetConfig` payload builder (stub).
//!
//! The `SetConfig` wire format differs between protocol v1 (six-byte
//! legacy payload used by the earliest MMDVM boards) and protocol v2
//! (much larger, multi-mode configuration with per-protocol deviation,
//! colour codes, hang timers, etc.). Mirroring
//! `ref/MMDVMHost/Modem.cpp:2100-2260` requires careful handling of
//! every optional mode.
//!
//! This module exposes the configuration type so downstream crates can
//! construct it, but does **not** yet implement wire encoding.
//
// TODO: full SetConfig encoding — multi-mode config is complex, will
// be done when needed.

use crate::mode::ModemMode;

/// Modem configuration parameters (provisional — encoding not yet
/// implemented, see module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModemConfig {
    /// Inversion flag byte (bit flags for RX/TX/PTT invert).
    pub invert: u8,
    /// Enabled-mode bitmask (`0x01` D-STAR, `0x02` DMR, `0x04` YSF, …).
    pub mode_flags: u8,
    /// TX delay, in 10 ms units.
    pub tx_delay: u8,
    /// Desired modem mode after applying the config.
    pub mode: ModemMode,
    /// RX audio level (0–255).
    pub rx_level: u8,
    /// TX audio level (0–255).
    pub tx_level: u8,
}

impl ModemConfig {
    /// Build a default-zero configuration targeting [`ModemMode::Idle`].
    #[must_use]
    pub const fn idle() -> Self {
        Self {
            invert: 0,
            mode_flags: 0,
            tx_delay: 0,
            mode: ModemMode::Idle,
            rx_level: 0,
            tx_level: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_constructor_defaults() {
        let c = ModemConfig::idle();
        assert_eq!(c.mode, ModemMode::Idle);
        assert_eq!(c.invert, 0);
        assert_eq!(c.mode_flags, 0);
        assert_eq!(c.tx_delay, 0);
        assert_eq!(c.rx_level, 0);
        assert_eq!(c.tx_level, 0);
    }

    #[test]
    fn struct_fields_are_publicly_writable() {
        let mut c = ModemConfig::idle();
        c.mode = ModemMode::DStar;
        c.mode_flags = 0x01;
        c.tx_delay = 10;
        c.rx_level = 128;
        c.tx_level = 128;
        assert_eq!(c.mode, ModemMode::DStar);
        assert_eq!(c.tx_delay, 10);
    }
}
