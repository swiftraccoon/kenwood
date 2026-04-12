//! Top-level `Reflector` type that owns all enabled endpoints.
//!
//! Supports all three reflector protocols (`DExtra`, `DPlus`, `DCS`);
//! each enabled endpoint is spawned as its own tokio task sharing a
//! common shutdown watch channel.
//!
//! When `cross_protocol_forwarding` is enabled in the config, the
//! reflector creates a `tokio::sync::broadcast` channel that every
//! endpoint publishes voice events onto. Each endpoint also
//! subscribes to the same bus in its run loop, re-encodes inbound
//! events from other protocols via [`transcode_voice`], and fans
//! the transcoded payload out to its own module members.
//!
//! [`transcode_voice`]: crate::tokio_shell::transcode::transcode_voice

use std::sync::Arc;

use tokio::net::UdpSocket;
use tokio::sync::{broadcast, watch};
use tokio::task::JoinSet;

use dstar_gateway_core::session::client::{DExtra, DPlus, Dcs};
use dstar_gateway_core::types::ProtocolKind;

use crate::reflector::authorizer::ClientAuthorizer;
use crate::reflector::config::ReflectorConfig;
use crate::tokio_shell::endpoint::{ProtocolEndpoint, ShellError};
use crate::tokio_shell::transcode::CrossProtocolEvent;

/// Capacity of the cross-protocol voice broadcast channel.
///
/// A voice stream runs at 20 fps; a 256-slot buffer holds roughly
/// 12 seconds of traffic per protocol source. If a receiver lags
/// further than that, `broadcast::Receiver::recv` returns
/// `Err(RecvError::Lagged)` and the subscriber catches up by
/// skipping to the newest frame — which for voice is the right
/// behavior (dropping old frames beats stale-audio artifacts).
const CROSS_PROTOCOL_BUS_CAPACITY: usize = 256;

/// Multi-protocol multi-client D-STAR reflector.
///
/// Owns a shared [`ClientAuthorizer`] plus one [`ProtocolEndpoint`]
/// per enabled protocol. Each enabled endpoint is spawned as its
/// own tokio task when [`Self::run`] is called, sharing a common
/// shutdown watch channel and the optional cross-protocol voice bus.
pub struct Reflector {
    config: ReflectorConfig,
    dextra: Option<Arc<ProtocolEndpoint<DExtra>>>,
    dplus: Option<Arc<ProtocolEndpoint<DPlus>>>,
    dcs: Option<Arc<ProtocolEndpoint<Dcs>>>,
    /// Pre-bound `DExtra` socket, set when the reflector was
    /// constructed via [`Self::new_with_socket`].
    dextra_socket: Option<Arc<UdpSocket>>,
    /// Pre-bound `DPlus` socket.
    dplus_socket: Option<Arc<UdpSocket>>,
    /// Pre-bound `DCS` socket.
    dcs_socket: Option<Arc<UdpSocket>>,
    /// Cross-protocol voice bus — `Some` iff
    /// `config.cross_protocol_forwarding` is `true`. Populated in
    /// the constructor so every endpoint subscribes at spawn time.
    voice_bus: Option<broadcast::Sender<CrossProtocolEvent>>,
    shutdown_tx: watch::Sender<bool>,
    shutdown_rx: watch::Receiver<bool>,
}

impl std::fmt::Debug for Reflector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Reflector")
            .field("config", &self.config)
            .field("dextra", &self.dextra.is_some())
            .field("dplus", &self.dplus.is_some())
            .field("dcs", &self.dcs.is_some())
            .field("dextra_socket", &self.dextra_socket.is_some())
            .field("dplus_socket", &self.dplus_socket.is_some())
            .field("dcs_socket", &self.dcs_socket.is_some())
            .field("voice_bus", &self.voice_bus.is_some())
            .finish_non_exhaustive()
    }
}

impl Reflector {
    /// Construct a reflector that will bind its own UDP sockets on
    /// [`Self::run`].
    ///
    /// This is the default constructor for production use — pass the
    /// parsed [`ReflectorConfig`] and an authorizer, then call
    /// [`Self::run`] to start serving.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use std::collections::HashSet;
    /// use std::sync::Arc;
    /// use dstar_gateway_core::types::{Callsign, Module, ProtocolKind};
    /// use dstar_gateway_server::{AllowAllAuthorizer, Reflector, ReflectorConfig};
    ///
    /// # async fn run() -> Result<(), Box<dyn std::error::Error>> {
    /// let mut modules = HashSet::new();
    /// let _ = modules.insert(Module::try_from_char('C')?);
    /// let config = ReflectorConfig::builder()
    ///     .callsign(Callsign::try_from_str("REF999")?)
    ///     .module_set(modules)
    ///     .bind("0.0.0.0:30001".parse()?)
    ///     .disable(ProtocolKind::DPlus)
    ///     .disable(ProtocolKind::Dcs)
    ///     .build()?;
    /// let reflector = Arc::new(Reflector::new(config, AllowAllAuthorizer));
    /// reflector.run().await?;
    /// # Ok(()) }
    /// ```
    pub fn new<A: ClientAuthorizer + 'static>(config: ReflectorConfig, authorizer: A) -> Self {
        Self::new_with_sockets(config, authorizer, None, None, None)
    }

    /// Construct a reflector with a pre-bound `DExtra` socket.
    ///
    /// Used by integration tests that need to know the bound port
    /// before the reflector starts serving. The caller is responsible
    /// for binding the socket to the same address as
    /// [`ReflectorConfig::bind`].
    pub fn new_with_socket<A: ClientAuthorizer + 'static>(
        config: ReflectorConfig,
        authorizer: A,
        socket: Arc<UdpSocket>,
    ) -> Self {
        Self::new_with_sockets(config, authorizer, Some(socket), None, None)
    }

    /// Construct a reflector with pre-bound sockets for each enabled
    /// protocol.
    ///
    /// Any `None` socket is bound lazily inside [`Self::run`] against
    /// the address in [`ReflectorConfig::bind`]. Pass `Some` only for
    /// the protocols whose port must be known before `run` is called
    /// (typically multi-protocol integration tests).
    pub fn new_with_sockets<A: ClientAuthorizer + 'static>(
        config: ReflectorConfig,
        authorizer: A,
        dextra_socket: Option<Arc<UdpSocket>>,
        dplus_socket: Option<Arc<UdpSocket>>,
        dcs_socket: Option<Arc<UdpSocket>>,
    ) -> Self {
        // `ReflectorConfigBuilder::build` rejects empty module sets,
        // so `modules` always contains at least one entry in
        // well-formed configs. The const fallback below is only
        // reachable if a `ReflectorConfig` is constructed outside
        // the builder path (which currently isn't possible).
        const FALLBACK: dstar_gateway_core::types::Module = {
            match dstar_gateway_core::types::Module::try_from_char('A') {
                Ok(m) => m,
                Err(_) => unreachable!(),
            }
        };
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let authorizer_arc: Arc<dyn ClientAuthorizer> = Arc::new(authorizer);
        // Pick a deterministic default module from the configured
        // set — the endpoint uses this as the seed `reflector_module`
        // for every new `ServerSessionCore`. DExtra/DCS sessions
        // overwrite it from the LINK packet on the wire; DPlus
        // sessions keep the default because the LINK2 packet doesn't
        // carry a module. Sorted `min` gives stable test behavior.
        let default_module = config
            .modules
            .iter()
            .min_by_key(|m| m.as_byte())
            .copied()
            .unwrap_or(FALLBACK);
        // Allocate the cross-protocol voice bus iff forwarding is
        // enabled. The receiver side is created by each endpoint
        // via `subscribe()` at spawn time; we only keep the sender.
        let voice_bus = if config.cross_protocol_forwarding {
            let (tx, _rx) = broadcast::channel(CROSS_PROTOCOL_BUS_CAPACITY);
            Some(tx)
        } else {
            None
        };
        let dextra = if config.is_enabled(ProtocolKind::DExtra) {
            Some(Arc::new(ProtocolEndpoint::<DExtra>::new_with_voice_bus(
                ProtocolKind::DExtra,
                default_module,
                Arc::clone(&authorizer_arc),
                voice_bus.clone(),
            )))
        } else {
            None
        };
        let dplus = if config.is_enabled(ProtocolKind::DPlus) {
            Some(Arc::new(ProtocolEndpoint::<DPlus>::new_with_voice_bus(
                ProtocolKind::DPlus,
                default_module,
                Arc::clone(&authorizer_arc),
                voice_bus.clone(),
            )))
        } else {
            None
        };
        let dcs = if config.is_enabled(ProtocolKind::Dcs) {
            Some(Arc::new(ProtocolEndpoint::<Dcs>::new_with_voice_bus(
                ProtocolKind::Dcs,
                default_module,
                Arc::clone(&authorizer_arc),
                voice_bus.clone(),
            )))
        } else {
            None
        };
        Self {
            config,
            dextra,
            dplus,
            dcs,
            dextra_socket,
            dplus_socket,
            dcs_socket,
            voice_bus,
            shutdown_tx,
            shutdown_rx,
        }
    }

    /// Access the cross-protocol voice bus sender, if forwarding is
    /// enabled in the config.
    ///
    /// Returns `None` when `config.cross_protocol_forwarding` is
    /// `false`; otherwise returns a clone of the broadcast sender
    /// suitable for subscribing a receiver.
    #[must_use]
    pub fn voice_bus(&self) -> Option<broadcast::Sender<CrossProtocolEvent>> {
        self.voice_bus.clone()
    }

    /// Read-only access to the configuration.
    #[must_use]
    pub const fn config(&self) -> &ReflectorConfig {
        &self.config
    }

    /// Signal all endpoint tasks to shut down.
    ///
    /// The `run` future resolves cleanly once every spawned endpoint
    /// task observes the shutdown flag and returns.
    pub fn shutdown(&self) {
        // Ignore send errors — the only failure mode is "no
        // receivers", which already means we're shutting down.
        let _ = self.shutdown_tx.send(true);
    }

    /// Run until shutdown.
    ///
    /// Spawns one tokio task per enabled protocol endpoint
    /// (`DExtra`, `DPlus`, `DCS`) and waits until every task returns.
    /// Returns `Ok(())` on clean shutdown.
    ///
    /// # Errors
    ///
    /// Returns [`ShellError::Io`] if a socket bind fails or if an
    /// endpoint task panics.
    ///
    /// # Cancellation safety
    ///
    /// This method is cancel-safe in the sense that dropping the
    /// future aborts the internal [`tokio::task::JoinSet`] cleanly —
    /// every spawned endpoint task is aborted and the sockets are
    /// released. For a graceful shutdown call [`Self::shutdown`] first
    /// and then `await` the `run()` future to completion; for an
    /// abrupt shutdown just drop the future. Never race `run()`
    /// against a second long-lived future with `tokio::select!`
    /// unless the intent is to kill the reflector.
    pub async fn run(&self) -> Result<(), ShellError> {
        let mut tasks: JoinSet<Result<(), ShellError>> = JoinSet::new();

        if let Some(endpoint) = self.dextra.as_ref() {
            let socket = match self.dextra_socket.clone() {
                Some(s) => s,
                None => Arc::new(UdpSocket::bind(self.config.bind).await?),
            };
            let shutdown = self.shutdown_rx.clone();
            let endpoint = Arc::clone(endpoint);
            let _handle = tasks.spawn(async move { endpoint.run(socket, shutdown).await });
        }

        if let Some(endpoint) = self.dplus.as_ref() {
            let socket = match self.dplus_socket.clone() {
                Some(s) => s,
                None => Arc::new(UdpSocket::bind(self.config.bind).await?),
            };
            let shutdown = self.shutdown_rx.clone();
            let endpoint = Arc::clone(endpoint);
            let _handle = tasks.spawn(async move { endpoint.run(socket, shutdown).await });
        }

        if let Some(endpoint) = self.dcs.as_ref() {
            let socket = match self.dcs_socket.clone() {
                Some(s) => s,
                None => Arc::new(UdpSocket::bind(self.config.bind).await?),
            };
            let shutdown = self.shutdown_rx.clone();
            let endpoint = Arc::clone(endpoint);
            let _handle = tasks.spawn(async move { endpoint.run(socket, shutdown).await });
        }

        while let Some(join_result) = tasks.join_next().await {
            match join_result {
                Ok(Ok(())) => {}
                Ok(Err(e)) => return Err(e),
                Err(join_err) => {
                    return Err(ShellError::Protocol(format!(
                        "endpoint task aborted: {join_err}"
                    )));
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::Reflector;
    use crate::reflector::authorizer::AllowAllAuthorizer;
    use crate::reflector::config::ReflectorConfig;
    use dstar_gateway_core::types::ProtocolKind;
    use dstar_gateway_core::types::{Callsign, Module};
    use std::collections::HashSet;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::sync::Arc;
    use tokio::net::UdpSocket;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    const BIND: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);

    fn module_set(modules: &[Module]) -> HashSet<Module> {
        let mut set = HashSet::new();
        for &m in modules {
            let _ = set.insert(m);
        }
        set
    }

    fn config() -> Result<ReflectorConfig, Box<dyn std::error::Error>> {
        let cfg = ReflectorConfig::builder()
            .callsign(Callsign::from_wire_bytes(*b"REF030  "))
            .module_set(module_set(&[Module::C]))
            .bind(BIND)
            .disable(ProtocolKind::DPlus)
            .disable(ProtocolKind::Dcs)
            .build()?;
        Ok(cfg)
    }

    #[tokio::test]
    async fn new_with_dextra_only_is_constructible() -> TestResult {
        let reflector = Reflector::new(config()?, AllowAllAuthorizer);
        assert!(reflector.config().is_enabled(ProtocolKind::DExtra));
        assert!(!reflector.config().is_enabled(ProtocolKind::DPlus));
        Ok(())
    }

    #[tokio::test]
    async fn voice_bus_is_none_when_cross_protocol_forwarding_disabled() -> TestResult {
        let reflector = Reflector::new(config()?, AllowAllAuthorizer);
        assert!(
            reflector.voice_bus().is_none(),
            "cross_protocol_forwarding = false => no voice bus"
        );
        Ok(())
    }

    #[tokio::test]
    async fn voice_bus_is_some_when_cross_protocol_forwarding_enabled() -> TestResult {
        let cfg = ReflectorConfig::builder()
            .callsign(Callsign::from_wire_bytes(*b"REF030  "))
            .module_set(module_set(&[Module::C]))
            .bind(BIND)
            .disable(ProtocolKind::DPlus)
            .disable(ProtocolKind::Dcs)
            .cross_protocol_forwarding(true)
            .build()?;
        let reflector = Reflector::new(cfg, AllowAllAuthorizer);
        let bus = reflector.voice_bus();
        assert!(
            bus.is_some(),
            "cross_protocol_forwarding = true => voice bus present"
        );
        Ok(())
    }

    #[tokio::test]
    async fn shutdown_before_run_returns_cleanly() -> TestResult {
        let socket = UdpSocket::bind("127.0.0.1:0").await?;
        let bound = socket.local_addr()?;
        let cfg = ReflectorConfig::builder()
            .callsign(Callsign::from_wire_bytes(*b"REF030  "))
            .module_set(module_set(&[Module::C]))
            .bind(bound)
            .disable(ProtocolKind::DPlus)
            .disable(ProtocolKind::Dcs)
            .build()?;
        let reflector = Reflector::new_with_socket(cfg, AllowAllAuthorizer, Arc::new(socket));
        reflector.shutdown();
        // `run` should return immediately because shutdown is already `true`.
        let result =
            tokio::time::timeout(std::time::Duration::from_secs(2), reflector.run()).await?;
        assert!(result.is_ok());
        Ok(())
    }
}
