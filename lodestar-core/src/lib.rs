// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

#![doc = include_str!("../README.md")]

//! Lodestar core: Rust library powering the Lodestar native macOS and
//! iPadOS D-STAR gateway app for the Kenwood TH-D75.
//!
//! Surfaces to Swift via `UniFFI`:
//!
//! - `version()`: crate semver.
//! - [`audio`]: `RxAudioPipeline`, which AMBE-decodes reflector voice frames
//!   into live-enhanced 8 kHz PCM with loss concealment, reorder
//!   rejection, and click-free stream edges for on-device monitoring.
//! - [`cat`]: minimal CAT codec covering the `ID` identify and `FV`
//!   firmware-version commands.
//! - [`mcp`]: programming-protocol primitives plus the qualified Menu 985
//!   and Menu 650 schema for entering Reflector Terminal Mode.
//! - [`mmdvm`]: MMDVM frame codec and the `GetVersion` probe used for
//!   radio-mode detection.
//! - [`reflector`]: `DPlus` / `DExtra` / `DCS` reflector list loaded
//!   from bundled `ircDDBGateway` host files, plus Pi-Star hosts-text
//!   parsing and a provenance-aware merge over bundled and `DPlus`
//!   auth-server sources.
//! - [`session`]: async [`ReflectorSession::connect`]
//!   driving the full radio-to-reflector voice loop, plus the
//!   [`session::ReflectorObserver`] callback trait Swift implements to
//!   receive voice events, slow-data text updates, and parsed GPS
//!   positions.
//! - Logging bridge: [`init_tracing`] installs a `tracing` subscriber that
//!   forwards each event to a Swift-implemented [`LogSink`] (tagged with
//!   [`LogLevel`]) for the in-app Log Viewer and Apple's Unified Log.

pub use audio::{RxAudioError, RxAudioPipeline, RxStreamEnd, RxStreamStats};
pub use cat::{CatCommand, CatResponse, encode_cat, parse_cat_line};
pub use mcp::{
    GATEWAY_INTERFACE_OFFSET, GATEWAY_MODE_ACCESS_POINT, GATEWAY_MODE_OFF, GATEWAY_MODE_OFFSET,
    GATEWAY_MODE_REFLECTOR_TERMINAL, MCP_D75_SCHEMA_FIRMWARE_IDENTITIES, MCP_D75_SCHEMA_MODEL,
    McpD75SchemaTarget, McpError, McpPage, PcOutputInterface, ReflectorTerminalSettings,
    build_enter_cmd, build_exit_cmd, build_read_page_cmd, byte_of, mcp_d75_schema_target, page_of,
    reflector_terminal_settings,
};
pub use mmdvm::{
    MMDVM_CMD_GET_VERSION, MMDVM_START_BYTE, MmdvmDecodeResult, MmdvmFrame, MmdvmFrameError,
    MmdvmVersionResponse, build_mmdvm_frame, decode_mmdvm_bytes, mmdvm_get_version_probe,
    parse_mmdvm_version_payload,
};
pub use reflector::{
    DirectoryEntry, DirectorySource, Reflector, ReflectorProtocol, default_reflectors,
    merge_directories, parse_hosts_text,
};
pub use session::{
    DisconnectCause, ReflectorConnectionCandidate, ReflectorConnectionConfig, ReflectorError,
    ReflectorSession, VoiceEndCause, fetch_dplus_directory,
};
pub use tracing_layer::init_tracing;
pub use uniffi_scaffolding::*;
pub use wire::{VoiceSequence, VoiceStreamId, WireValueError};

#[forbid(unsafe_code)]
pub mod audio;
#[forbid(unsafe_code)]
pub mod cat;
#[forbid(unsafe_code)]
pub mod mcp;
#[forbid(unsafe_code)]
pub mod mmdvm;
#[forbid(unsafe_code)]
pub mod reflector;
#[forbid(unsafe_code)]
pub mod session;
#[forbid(unsafe_code)]
mod wire;

/// Generated `UniFFI` exports for the UDL interface.
#[expect(
    clippy::large_const_arrays,
    reason = "UniFFI's non-Copy 32 KiB metadata buffer must remain const for consuming into_array"
)]
mod uniffi_scaffolding {
    use crate::{
        CatCommand, CatResponse, Reflector, ReflectorProtocol, default_reflectors, encode_cat,
        parse_cat_line, version,
    };

    uniffi::include_scaffolding!("lodestar");
}

/// Returns the semantic version of this crate as configured in `Cargo.toml`.
#[must_use]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_owned()
}

/// Level label passed to a [`LogSink`]; mirrors `tracing::Level`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum LogLevel {
    /// `tracing::Level::TRACE`: finest-grained diagnostic.
    Trace,
    /// `tracing::Level::DEBUG`: development diagnostic.
    Debug,
    /// `tracing::Level::INFO`: normal operational event.
    Info,
    /// `tracing::Level::WARN`: unexpected but recoverable event.
    Warn,
    /// `tracing::Level::ERROR`: unrecoverable error.
    Error,
}

/// Foreign-implemented sink that receives one call per Rust `tracing`
/// event. Swift writes a concrete implementation that forwards each
/// event to `os_log` with subsystem `org.swiftraccoon.lodestar.rust`
/// so the crate's `tracing::debug!` / `tracing::trace!` calls end up in
/// Apple's Unified Log and the app's Log Viewer.
#[uniffi::export(with_foreign)]
pub trait LogSink: Send + Sync + std::fmt::Debug {
    /// Called once per tracing event. `target` is the
    /// `tracing::Event::metadata().target()` (usually the module path
    /// or an override like `"lodestar_core::session::slow_data"`).
    fn log(&self, level: LogLevel, target: String, message: String);
}

/// Tracing event projection used by [`init_tracing`].
mod tracing_layer {
    use super::{LogLevel, LogSink};
    use std::fmt::Write as _;
    use std::sync::Arc;
    use tracing::Event;
    use tracing::field::{Field, Visit};
    use tracing_subscriber::Layer;
    use tracing_subscriber::layer::Context;
    use tracing_subscriber::registry::LookupSpan;

    /// Tracing layer that forwards admitted events to the foreign sink.
    struct SinkLayer {
        sink: Arc<dyn LogSink>,
    }

    impl SinkLayer {
        /// Wrap a foreign sink.
        #[must_use]
        fn new(sink: Arc<dyn LogSink>) -> Self {
            Self { sink }
        }
    }

    /// Attempt to install the process-wide tracing subscriber using `RUST_LOG`.
    ///
    /// Missing or invalid filters use
    /// `info,lodestar_core=debug,dstar_gateway_core=debug,dstar_gateway=debug`.
    /// Only admitted events reach `sink`. An existing subscriber is retained;
    /// installation failure is ignored. The sink runs synchronously on each
    /// emitting thread and must return promptly without reentering tracing.
    #[uniffi::export]
    pub fn init_tracing(sink: Arc<dyn LogSink>) {
        use tracing_subscriber::EnvFilter;
        use tracing_subscriber::layer::SubscriberExt as _;
        use tracing_subscriber::util::SubscriberInitExt as _;

        let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            EnvFilter::new("info,lodestar_core=debug,dstar_gateway_core=debug,dstar_gateway=debug")
        });
        drop(
            tracing_subscriber::registry()
                .with(filter)
                .with(SinkLayer::new(sink))
                .try_init(),
        );
    }

    impl std::fmt::Debug for SinkLayer {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("SinkLayer").finish_non_exhaustive()
        }
    }

    impl<S> Layer<S> for SinkLayer
    where
        S: tracing::Subscriber + for<'a> LookupSpan<'a>,
    {
        fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
            let meta = event.metadata();
            let level = match *meta.level() {
                tracing::Level::TRACE => LogLevel::Trace,
                tracing::Level::DEBUG => LogLevel::Debug,
                tracing::Level::INFO => LogLevel::Info,
                tracing::Level::WARN => LogLevel::Warn,
                tracing::Level::ERROR => LogLevel::Error,
            };
            let mut collector = MessageCollector(String::new());
            event.record(&mut collector);
            self.sink.log(level, meta.target().to_owned(), collector.0);
        }
    }

    struct MessageCollector(String);
    impl Visit for MessageCollector {
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            if field.name() == "message" {
                let _ = write!(self.0, "{value:?}");
            } else if !self.0.is_empty() {
                let _ = write!(self.0, " {}={value:?}", field.name());
            } else {
                let _ = write!(self.0, "{}={value:?}", field.name());
            }
        }

        fn record_str(&mut self, field: &Field, value: &str) {
            if field.name() == "message" {
                self.0.push_str(value);
            } else if !self.0.is_empty() {
                let _ = write!(self.0, " {}={value}", field.name());
            } else {
                let _ = write!(self.0, "{}={value}", field.name());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::version;

    #[test]
    fn version_matches_cargo_pkg_version() {
        assert_eq!(version(), env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn version_is_semver_shape() {
        let v = version();
        let parts: Vec<&str> = v.split('.').collect();
        assert!(
            parts.len() == 3 && parts.iter().all(|p| p.chars().all(|c| c.is_ascii_digit())),
            "version {v:?} is not a three-part numeric semver"
        );
    }
}
