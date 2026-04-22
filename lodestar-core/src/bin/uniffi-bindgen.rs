// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

//! Crate-local `uniffi-bindgen` binary.
//!
//! Used by `scripts/build-xcframework.sh` to generate the Swift bindings
//! from the UDL without depending on a globally-installed `uniffi-bindgen`
//! that could drift away from the crate's `uniffi` version.

// The bin must reference each of the crate's top-level deps, or the
// workspace `unused_crate_dependencies` lint fires on this separate
// compilation unit. None of them are used in the bin itself.
use dstar_gateway as _;
use dstar_gateway_core as _;
use lodestar_core as _;
use mmdvm_core as _;
use thiserror as _;
use tokio as _;
use tracing as _;

fn main() {
    uniffi::uniffi_bindgen_main();
}
