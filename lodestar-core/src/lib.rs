// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

//! Lodestar core — Rust library powering the Lodestar iOS/iPadOS/Mac Catalyst app.
//!
//! Phase 1 exposed `version()`. Phase 2 adds a minimal CAT codec
//! (`encode_cat`, `parse_cat_line`) for the `ID` identify command.
//! Later phases wrap `thd75`, `dstar-gateway`, and `dstar-gateway-core`
//! and drive the radio-to-reflector session loop.

pub mod cat;

pub use cat::{CatCommand, CatResponse, encode_cat, parse_cat_line};

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
