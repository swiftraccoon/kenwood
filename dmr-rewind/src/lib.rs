// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later

//! Tokio UDP client for `BrandMeister`'s receive-only Open DMR Terminal
//! service.
//!
//! Wire encoding and authentication primitives live in
//! [`dmr_rewind_core`]. This crate owns the socket and timers, performs
//! the self-service DMR-ID/hotspot-password handshake, installs
//! subscriptions, and yields typed packets. Reconnection and hostname
//! resolution belong to the calling application.

mod client;

pub use client::{
    AudioEvent, Client, ClientConfig, DEFAULT_KEEPALIVE_INTERVAL, DEFAULT_TIMEOUT,
    EmbeddedDataEvent, Error, Event, EventMetadata, MAX_PENDING_BYTES, MAX_PENDING_EVENTS,
    NoticeEvent, OtherEvent, Subscription, SuperHeaderEvent, TerminatorEvent, VoiceHeaderEvent,
};
pub use dmr_rewind_core::{
    Callsign, DEFAULT_OPEN_TERMINAL_PORT, FullLinkControl, PacketFlags, PacketType, Payload,
    SessionType, SuperHeader,
};

/// Compatibility name for [`Event`].
pub type ClientEvent = Event;

/// Compatibility name for [`Error`].
pub type ConnectError = Error;
