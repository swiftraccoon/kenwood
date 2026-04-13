// Portions of this file are derived from MMDVMHost by Jonathan Naylor
// G4KLX, Copyright (C) 2015-2026, licensed under GPL-2.0-or-later.
// See LICENSE for full attribution.

//! Sans-io codec for the MMDVM ham-radio digital-voice modem protocol.
//!
//! `mmdvm-core` is a **runtime-agnostic, I/O-free** library. It
//! contains the wire-format codec for MMDVM frames and the supporting
//! type, capability, mode and error declarations used by every shell
//! that speaks to an MMDVM-compatible modem (`MMDVMHost`, `ZUMspot`,
//! `MMDVM_HS_Hat`, etc.).
//!
//! The crate mirrors the reference C++ implementation at
//! `ref/MMDVMHost/Modem.cpp` / `ref/MMDVMHost/Modem.h` (GPL-2.0) which
//! is treated as the protocol's ground truth. D-STAR is the primary
//! target because of the TH-D75's internal modem, but the command
//! byte constants and the frame codec itself are multi-mode (DMR,
//! YSF, P25, NXDN, POCSAG, FM).
//!
//! ## What's in scope
//!
//! - [`frame::MmdvmFrame`] — the `[0xE0, len, cmd, payload]` wire
//!   frame and its codec.
//! - [`command`] — every MMDVM protocol byte constant.
//! - [`ModemMode`] — the modem's operating-mode enum.
//! - [`ModemStatus`] — parsing of the status response for protocol v1
//!   and v2.
//! - [`VersionResponse`] / [`Capabilities`] / [`HwType`] — parsing of
//!   the `GetVersion` handshake.
//! - [`NakReason`] — typed NAK reason codes.
//! - [`MmdvmError`] — the unified codec error type.
//!
//! ## What's out of scope
//!
//! - Serial / network I/O (see the `thd75` and `mmdvm-gateway` shell
//!   crates).
//! - Per-protocol voice/data framing above the MMDVM wire layer
//!   (e.g. AMBE, IMBE, NXDN, DMR BPTC). Those live in dedicated
//!   sans-io crates.
//! - The full `SetConfig` encoder (stub only — see [`ModemConfig`]).

pub mod capabilities;
pub mod command;
pub mod config;
pub mod error;
pub mod frame;
pub mod hw_type;
pub mod mode;
pub mod nak;
pub mod status;
pub mod version;

pub use capabilities::Capabilities;
pub use command::{
    CAP1_DMR, CAP1_DSTAR, CAP1_FM, CAP1_NXDN, CAP1_P25, CAP1_YSF, CAP2_POCSAG, MMDVM_ACK,
    MMDVM_DEBUG_DUMP, MMDVM_DEBUG1, MMDVM_DEBUG2, MMDVM_DEBUG3, MMDVM_DEBUG4, MMDVM_DEBUG5,
    MMDVM_DMR_ABORT, MMDVM_DMR_DATA1, MMDVM_DMR_DATA2, MMDVM_DMR_LOST1, MMDVM_DMR_LOST2,
    MMDVM_DMR_SHORTLC, MMDVM_DMR_START, MMDVM_DSTAR_DATA, MMDVM_DSTAR_EOT, MMDVM_DSTAR_HEADER,
    MMDVM_DSTAR_LOST, MMDVM_FM_DATA, MMDVM_FM_EOT, MMDVM_FM_PARAMS1, MMDVM_FM_PARAMS2,
    MMDVM_FM_PARAMS3, MMDVM_FM_PARAMS4, MMDVM_FM_RSSI, MMDVM_FM_STATUS, MMDVM_FRAME_START,
    MMDVM_GET_STATUS, MMDVM_GET_VERSION, MMDVM_NAK, MMDVM_NXDN_DATA, MMDVM_NXDN_LOST,
    MMDVM_P25_HDR, MMDVM_P25_LDU, MMDVM_P25_LOST, MMDVM_POCSAG_DATA, MMDVM_QSO_INFO,
    MMDVM_SEND_CWID, MMDVM_SERIAL_DATA, MMDVM_SET_CONFIG, MMDVM_SET_FREQ, MMDVM_SET_MODE,
    MMDVM_TRANSPARENT, MMDVM_YSF_DATA, MMDVM_YSF_LOST, MODE_CW, MODE_DMR, MODE_DSTAR, MODE_ERROR,
    MODE_FM, MODE_IDLE, MODE_LOCKOUT, MODE_NXDN, MODE_P25, MODE_POCSAG, MODE_QUIT, MODE_YSF,
};
pub use config::ModemConfig;
pub use error::MmdvmError;
pub use frame::{MAX_PAYLOAD_LEN, MIN_FRAME_LEN, MmdvmFrame, decode_frame, encode_frame};
pub use hw_type::HwType;
pub use mode::ModemMode;
pub use nak::NakReason;
pub use status::{ModemStatus, StatusFlags};
pub use version::VersionResponse;

// `proptest` is a dev-dependency used only from the integration test
// suites under `tests/`. The library test crate sees dev-deps too,
// so we acknowledge it here to keep `-D unused-crate-dependencies`
// happy.
#[cfg(test)]
use proptest as _;
