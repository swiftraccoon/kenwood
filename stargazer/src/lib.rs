// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later

#![doc = include_str!("../README.md")]

// Binary-only dependencies (used by `main.rs`); acknowledged here so
// `unused_crate_dependencies` stays quiet on the lib target.
use clap as _;
use tracing_subscriber as _;

pub use http::FetchError;

pub mod audio;
pub mod capture;
pub mod config;
pub mod control;
pub mod dvrec;
pub mod features;
pub mod harvest;
mod http;
pub mod session;
pub mod survey;
pub mod wav;
pub mod writer;
