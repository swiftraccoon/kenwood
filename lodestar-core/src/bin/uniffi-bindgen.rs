// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

//! Crate-local `uniffi-bindgen` binary.
//!
//! Used by `scripts/build-xcframework.sh` to generate the Swift bindings
//! from the UDL without depending on a globally-installed `uniffi-bindgen`
//! that could drift away from the crate's `uniffi` version.

// The bin must reference the parent crate or `unused_crate_dependencies`
// fires. The bin itself doesn't use any API from the lib, so the import
// is intentionally unused.
use lodestar_core as _;

fn main() {
    uniffi::uniffi_bindgen_main();
}
