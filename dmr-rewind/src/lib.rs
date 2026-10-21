// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later

#![doc = include_str!("../README.md")]

#[cfg(test)]
use socket2 as _;

pub use client::{
    AudioEvent, Client, ClientConfig, ControlOperation, DEFAULT_KEEPALIVE_INTERVAL,
    DEFAULT_SEND_TIMEOUT, DEFAULT_TIMEOUT, EmbeddedDataEvent, Error, Event, EventKind,
    EventMetadata, MAX_PENDING_BYTES, MAX_PENDING_EVENTS, NoticeEvent, OtherEvent, Subscription,
    SuperHeaderEvent, TerminatorEvent, VoiceHeaderEvent,
};
pub use config::{ConfigError, DestinationId, SubscriberId, SubscriptionKind};
pub use dmr_rewind_core::{
    Callsign, DEFAULT_OPEN_TERMINAL_PORT, DmrId, FullLinkControl, PacketFlags, PacketType, Payload,
    SessionType, SuperHeader,
};

mod client;
mod config;
