// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

//! Build script — generates [`UniFFI`](https://mozilla.github.io/uniffi-rs/) Rust scaffolding from the UDL.

use std::process;

fn main() {
    if let Err(e) = uniffi::generate_scaffolding("src/lodestar.udl") {
        eprintln!("uniffi scaffolding generation failed: {e}");
        process::exit(1);
    }
}
