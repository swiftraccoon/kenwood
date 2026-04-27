// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

//! Lodestar core — Rust library powering the Lodestar native macOS and
//! iOS/iPadOS D-STAR gateway app for the Kenwood TH-D75.
//!
//! Surfaces to Swift via `UniFFI`:
//!
//! - `version()` — crate semver.
//! - [`cat`] — minimal CAT codec covering the `ID` identify command.
//! - [`mcp`] — programming-protocol primitives for flipping menu 650
//!   (DV Gateway) into Reflector Terminal Mode.
//! - [`mmdvm`] — MMDVM frame codec and the `GetVersion` probe used for
//!   radio-mode detection.
//! - [`reflector`] — `DPlus` / `DExtra` / `DCS` reflector list loaded
//!   from bundled `ircDDBGateway` host files.
//! - [`session`] — async `connect_reflector` + [`session::ReflectorSession`]
//!   driving the full radio-to-reflector voice loop, plus the
//!   [`session::ReflectorObserver`] callback trait Swift implements to
//!   receive voice events, slow-data text updates, and parsed GPS
//!   positions.

pub mod cat;
pub mod mcp;
pub mod mmdvm;
pub mod reflector;
pub mod session;

pub use cat::{CatCommand, CatResponse, encode_cat, parse_cat_line};
pub use mcp::{
    GATEWAY_MODE_ACCESS_POINT, GATEWAY_MODE_OFF, GATEWAY_MODE_OFFSET,
    GATEWAY_MODE_REFLECTOR_TERMINAL, McpError, McpPage, build_enter_cmd, build_exit_cmd,
    build_read_page_cmd, build_write_page_cmd, byte_of, page_of, parse_w_frame, patch_page_byte,
};
pub use mmdvm::{
    MMDVM_CMD_GET_VERSION, MMDVM_START_BYTE, MmdvmDecodeResult, MmdvmFrame, MmdvmFrameError,
    build_mmdvm_frame, decode_mmdvm_bytes, looks_like_mmdvm_response, mmdvm_get_version_probe,
};
pub use reflector::{Reflector, ReflectorProtocol, default_reflectors};
pub use session::{ReflectorError, ReflectorSession, connect_reflector};

uniffi::include_scaffolding!("lodestar");

/// Returns the semantic version of this crate as configured in `Cargo.toml`.
#[must_use]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_owned()
}

/// Level label passed to a [`LogSink`] — mirrors `tracing::Level`.
#[derive(Debug, Clone, Copy, uniffi::Enum)]
pub enum LogLevel {
    /// `tracing::Level::TRACE` — finest-grained diagnostic.
    Trace,
    /// `tracing::Level::DEBUG` — development diagnostic.
    Debug,
    /// `tracing::Level::INFO` — normal operational event.
    Info,
    /// `tracing::Level::WARN` — unexpected but recoverable event.
    Warn,
    /// `tracing::Level::ERROR` — unrecoverable error.
    Error,
}

/// Foreign-implemented sink that receives one call per Rust `tracing`
/// event. Swift writes a concrete implementation that forwards each
/// event to `os_log` with subsystem `org.swiftraccoon.lodestar.rust`
/// so our `tracing::debug!` / `tracing::trace!` calls end up in
/// Apple's Unified Log and our in-app Log Viewer.
#[uniffi::export(with_foreign)]
pub trait LogSink: Send + Sync + std::fmt::Debug {
    /// Called once per tracing event. `target` is the
    /// `tracing::Event::metadata().target()` (usually the module path
    /// or an override like `"lodestar_core::session::slow_data"`).
    fn log(&self, level: LogLevel, target: String, message: String);
}

/// Install the `tracing` subscriber that forwards every event to the
/// given [`LogSink`]. Idempotent across calls; subsequent sinks are
/// ignored because `tracing`'s global dispatcher can only be set once.
#[uniffi::export]
pub fn init_tracing(sink: std::sync::Arc<dyn LogSink>) {
    use tracing_subscriber::EnvFilter;
    use tracing_subscriber::layer::SubscriberExt as _;
    use tracing_subscriber::util::SubscriberInitExt as _;

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new("info,lodestar_core=debug,dstar_gateway_core=debug,dstar_gateway=debug")
    });
    let layer = tracing_layer::SinkLayer::new(sink);
    drop(
        tracing_subscriber::registry()
            .with(filter)
            .with(layer)
            .try_init(),
    );
}

/// Internal `tracing_subscriber::Layer` impl used by [`init_tracing`].
///
/// Exposed as `pub` only because `tracing_subscriber`'s `.with(...)`
/// builder chain requires the layer type to be nameable — callers
/// don't construct this directly.
pub mod tracing_layer {
    use super::{LogLevel, LogSink};
    use std::fmt::Write as _;
    use std::sync::Arc;
    use tracing::Event;
    use tracing::field::{Field, Visit};
    use tracing::span::Span;
    use tracing_subscriber::Layer;
    use tracing_subscriber::layer::Context;
    use tracing_subscriber::registry::LookupSpan;

    /// Tracing `Layer` that forwards each event to the foreign
    /// [`LogSink`]. `pub` because `tracing_subscriber`'s
    /// `.with(...)` chain requires the type to be nameable outside
    /// its module.
    pub struct SinkLayer {
        sink: Arc<dyn LogSink>,
    }

    impl SinkLayer {
        /// Wrap a foreign sink.
        #[must_use]
        pub fn new(sink: Arc<dyn LogSink>) -> Self {
            Self { sink }
        }
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

        fn on_new_span(
            &self,
            _attrs: &tracing::span::Attributes<'_>,
            _id: &tracing::span::Id,
            _ctx: Context<'_, S>,
        ) {
            // Intentionally ignore span lifecycle — only flat events
            // make it to the sink, which is what our UI needs.
            drop(Span::none());
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
