//! Library side of the accessible TH-D75 IF demodulator.
//!
//! Pure command parsing, output formatting, and radio session state
//! handling. The binary owns all I/O (cpal audio, serial CAT, the
//! terminal).

// Bin-only deps, acknowledged so `unused_crate_dependencies` stays
// silent for the lib compilation unit.
use cpal as _;
use rustyline as _;
use tokio as _;
/// Pure format functions for every user-facing string.
pub mod output;

/// Command grammar for the listener prompt.
pub mod parser;
