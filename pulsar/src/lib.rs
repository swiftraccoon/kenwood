// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later

//! DMR call recorder for `BrandMeister`'s REWIND application interface.
//!
//! This library contains the I/O-free call capture model, validated
//! configuration, and atomic raw AMBE+2/metadata writer. Network session
//! supervision lives in the `pulsar` binary.

// These dependencies are used by the binary target. Acknowledging them here
// keeps the workspace's per-target dependency lint useful.
use clap as _;
use tracing_subscriber as _;

pub mod capture;
pub mod config;
pub mod session;
pub mod writer;
