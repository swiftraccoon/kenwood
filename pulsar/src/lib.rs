// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later

#![doc = include_str!("../README.md")]

// These dependencies are used by the binary target. Acknowledging them here
// keeps the workspace's per-target dependency lint useful.
use clap as _;
use tracing_subscriber as _;

pub mod capture;
pub mod config;
pub mod session;
pub mod writer;
