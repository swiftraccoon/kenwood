// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

//! Lodestar core — Rust library powering the Lodestar native macOS and
//! iOS/iPadOS D-STAR gateway app for the Kenwood TH-D75.
//!
//! Surfaces to Swift via `UniFFI`:
//!
//! - `version()` — crate semver.
//! - [`cat`] — minimal CAT codec covering the `ID` identify command.
//! - [`mcp`] — programming-protocol primitives for flipping menu 650
//!   (DV Gateway) into Reflector Terminal Mode.
//! - [`mmdvm`] — MMDVM frame codec and the `GetVersion` probe used for
//!   radio-mode detection.
//! - [`reflector`] — `DPlus` / `DExtra` / `DCS` reflector list loaded
//!   from bundled `ircDDBGateway` host files.
//! - [`session`] — async `connect_reflector` + [`session::ReflectorSession`]
//!   driving the full radio-to-reflector voice loop, plus the
//!   [`session::ReflectorObserver`] callback trait Swift implements to
//!   receive voice events, slow-data text updates, and parsed GPS
//!   positions.

pub mod cat;
pub mod mcp;
pub mod mmdvm;
pub mod reflector;
pub mod session;

pub use cat::{CatCommand, CatResponse, encode_cat, parse_cat_line};
pub use mcp::{
    GATEWAY_MODE_ACCESS_POINT, GATEWAY_MODE_OFF, GATEWAY_MODE_OFFSET,
    GATEWAY_MODE_REFLECTOR_TERMINAL, McpError, McpPage, build_enter_cmd, build_exit_cmd,
    build_read_page_cmd, build_write_page_cmd, byte_of, page_of, parse_w_frame, patch_page_byte,
};
pub use mmdvm::{
    MMDVM_CMD_GET_VERSION, MMDVM_START_BYTE, MmdvmDecodeResult, MmdvmFrame, MmdvmFrameError,
    build_mmdvm_frame, decode_mmdvm_bytes, looks_like_mmdvm_response, mmdvm_get_version_probe,
};
pub use reflector::{Reflector, ReflectorProtocol, default_reflectors};
pub use session::{ReflectorError, ReflectorSession, connect_reflector};

uniffi::include_scaffolding!("lodestar");

/// Returns the semantic version of this crate as configured in `Cargo.toml`.
#[must_use]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_owned()
}

#[cfg(test)]
mod tests {
    use super::version;

    #[test]
    fn version_matches_cargo_pkg_version() {
        assert_eq!(version(), env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn version_is_semver_shape() {
        let v = version();
        let parts: Vec<&str> = v.split('.').collect();
        assert!(
            parts.len() == 3 && parts.iter().all(|p| p.chars().all(|c| c.is_ascii_digit())),
            "version {v:?} is not a three-part numeric semver"
        );
    }
}
