//! D-STAR gateway client for the TH-D75.
//!
//! The TH-D75 in Reflector Terminal Mode speaks the MMDVM binary
//! framing protocol on its serial link. This module owns the radio-facing
//! side of that interface: a [`DStarGateway`] wraps an
//! [`mmdvm::AsyncModem`] and translates its [`mmdvm::Event`] stream into
//! the higher-level [`DStarEvent`]s that TH-D75 consumers care about
//! (slow-data text messages, URCALL commands, last-heard tracking,
//! echo record/playback).
//!
//! The raw MMDVM framing codec lives in the [`mmdvm_core`] crate; the
//! async event loop, TX-queue buffer gating, and 250 ms status polling
//! live in the [`mmdvm`] crate.
//!
//! D-STAR protocol types (headers, voice frames, slow-data codecs,
//! reflector protocols, host file parser) live in the
//! [`dstar-gateway-core`](dstar_gateway_core) crate.

pub mod gateway;

// Re-export the most commonly used types from mmdvm-core so thd75
// consumers don't need to depend on it directly.
pub use mmdvm_core::{MmdvmError, ModemMode, ModemStatus, NakReason};

pub use gateway::{DStarEvent, DStarGateway, DStarGatewayConfig, LastHeardEntry, ReconnectPolicy};
