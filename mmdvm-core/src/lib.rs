// Portions of this file are derived from MMDVMHost by Jonathan Naylor
// G4KLX, Copyright (C) 2015-2026, licensed under GPL-2.0-or-later.
// See LICENSE for full attribution.

#![doc = include_str!("../README.md")]

#[cfg(test)]
use proptest as _;

pub use capabilities::Capabilities;
pub use command::{
    CAP1_DMR, CAP1_DSTAR, CAP1_FM, CAP1_NXDN, CAP1_P25, CAP1_YSF, CAP2_POCSAG, Command,
    MMDVM_FRAME_START, MODE_DMR, MODE_DSTAR, MODE_FM, MODE_IDLE, MODE_NXDN, MODE_P25, MODE_POCSAG,
    MODE_YSF,
};
pub use error::MmdvmError;
pub use frame::{MAX_PAYLOAD_LEN, MAX_SHORT_PAYLOAD_LEN, MIN_FRAME_LEN, MmdvmFrame};
pub use hw_type::HwType;
pub use mode::ModemMode;
pub use nak::NakReason;
pub use status::{ModemStatus, StatusFlags};
pub use version::{ProtocolVersion, VersionResponse};

mod capabilities;
mod command;
mod error;
mod frame;
mod hw_type;
mod mode;
mod nak;
mod status;
mod version;
