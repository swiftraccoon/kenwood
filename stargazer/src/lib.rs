// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later

//! D-STAR reflector voice recorder.
//!
//! A TOML config lists reflector targets; one supervisor task per
//! `(reflector, module)` pair connects listen-only and writes each
//! received transmission to disk as three files: a raw AMBE frame
//! container (ground truth), a decoded 8 kHz WAV, and a metadata
//! JSON (written last, so a recording exists iff its JSON exists).

// Binary-only dependencies (used by `main.rs`); acknowledged here so
// `unused_crate_dependencies` stays quiet on the lib target.
use clap as _;
use tracing_subscriber as _;

pub mod audio;
pub mod capture;
pub mod config;
pub mod control;
pub mod dvrec;
pub mod features;
pub mod harvest;
pub mod session;
pub mod survey;
pub mod wav;
pub mod writer;
