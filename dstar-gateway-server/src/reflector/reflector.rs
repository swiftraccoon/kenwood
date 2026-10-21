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
use crate::tokio_shell::endpoint::{EndpointSettings, ProtocolEndpoint, ShellError};
use crate::tokio_shell::transcode::CrossProtocolEvent;

/// Capacity of the cross-protocol voice broadcast channel.
///
/// A voice stream runs at 50 frames/s; 256 slots hold about five seconds
/// for one source. If a receiver lags
/// further than that, `broadcast::Receiver::recv` returns
/// `Err(RecvError::Lagged)` and the subscriber catches up by
/// skipping to the newest frame, which for voice is the right
/// behavior (dropping old frames beats stale-audio artifacts).
const CROSS_PROTOCOL_BUS_CAPACITY: usize = 256;

/// Optional pre-bound sockets, named by the protocol that exclusively drives them.
#[derive(Debug, Default)]
pub struct ProtocolSockets {
    /// `DExtra` socket; `None` binds the configured address.
    pub dextra: Option<Arc<UdpSocket>>,
    /// `DPlus` socket; `None` binds the configured address.
    pub dplus: Option<Arc<UdpSocket>>,
    /// `DCS` socket; `None` binds the configured address.
    pub dcs: Option<Arc<UdpSocket>>,
}

/// Cloneable stop signal for one consuming [`Reflector::run`] invocation.
///
/// Calling [`Self::shutdown`] requests shutdown. Awaiting `run` confirms that
/// every endpoint task has finished and released its task-owned resources.
#[derive(Debug, Clone)]
pub struct ShutdownHandle {
    sender: watch::Sender<bool>,
}

impl ShutdownHandle {
    /// Request shutdown, including when called before `run` is first polled.
    pub fn shutdown(&self) {
        let _previous = self.sender.send_replace(true);
    }
}

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
    /// Cross-protocol voice bus: `Some` iff
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
    /// This is the default constructor for production use: pass the
    /// parsed [`ReflectorConfig`] and an authorizer, then call
    /// [`Self::run`] to start serving.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use std::collections::HashSet;
    /// use std::sync::Arc;
    /// use dstar_gateway_core::types::{RoutingCallsign, Module, ProtocolKind};
    /// use dstar_gateway_server::{AllowAllAuthorizer, Reflector, ReflectorConfig};
    ///
    /// # async fn run() -> Result<(), Box<dyn std::error::Error>> {
    /// let mut modules = HashSet::new();
    /// let _ = modules.insert(Module::try_from_char('C')?);
    /// let config = ReflectorConfig::builder()
    ///     .callsign(RoutingCallsign::new("REF999")?)
    ///     .module_set(modules)
    ///     .bind("0.0.0.0:30001".parse()?)
    ///     .disable(ProtocolKind::DPlus)
    ///     .disable(ProtocolKind::Dcs)
    ///     .build()?;
    /// let reflector = Reflector::new(config, AllowAllAuthorizer);
    /// reflector.run().await?;
    /// # Ok(()) }
    /// ```
    pub fn new<A: ClientAuthorizer + 'static>(config: ReflectorConfig, authorizer: A) -> Self {
        Self::new_with_sockets(config, authorizer, ProtocolSockets::default())
    }

    /// Construct a reflector with a pre-bound `DExtra` socket.
    ///
    /// Used by integration tests that need to know the bound port
    /// before the reflector starts serving. A pre-bound socket is
    /// used as-is: it bypasses the per-protocol address resolution
    /// in [`ReflectorConfig::bind_addr_for`] entirely.
    pub fn new_with_socket<A: ClientAuthorizer + 'static>(
        config: ReflectorConfig,
        authorizer: A,
        socket: Arc<UdpSocket>,
    ) -> Self {
        Self::new_with_sockets(
            config,
            authorizer,
            ProtocolSockets {
                dextra: Some(socket),
                ..ProtocolSockets::default()
            },
        )
    }

    /// Construct a reflector with pre-bound sockets for each enabled
    /// protocol.
    ///
    /// Any `None` socket is bound lazily inside [`Self::run`] against
    /// the protocol's resolved address
    /// ([`ReflectorConfig::bind_addr_for`]). Pass `Some` only for
    /// the protocols whose port must be known before `run` is called
    /// (typically multi-protocol integration tests).
    pub fn new_with_sockets<A: ClientAuthorizer + 'static>(
        config: ReflectorConfig,
        authorizer: A,
        sockets: ProtocolSockets,
    ) -> Self {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let authorizer_arc: Arc<dyn ClientAuthorizer> = Arc::new(authorizer);
        // Pick a deterministic default module from the configured
        // set; the endpoint uses this as the seed `reflector_module`
        // for DPlus, whose LINK2 carries no module. DExtra/DCS
        // sessions select their requested configured module; DPlus
        // sessions keep the default because the LINK2 packet doesn't
        // carry a module. Sorted `min` gives stable test behavior.
        let default_module = config
            .modules
            .iter()
            .min_by_key(|m| m.as_byte())
            .copied()
            .unwrap_or_else(|| unreachable!("validated configuration contains a module"));
        // Allocate the cross-protocol voice bus iff forwarding is
        // enabled. The receiver side is created by each endpoint
        // via `subscribe()` at spawn time; we only keep the sender.
        let voice_bus = if config.cross_protocol_forwarding {
            let (tx, _rx) = broadcast::channel(CROSS_PROTOCOL_BUS_CAPACITY);
            Some(tx)
        } else {
            None
        };
        // Every endpoint inherits the reflector's configured
        // shell-level knobs (rate limit, client caps, keepalive and
        // inactivity windows).
        let settings = EndpointSettings::from(&config);
        let configured_modules = config.modules.clone();
        let dextra = if config.is_enabled(ProtocolKind::DExtra) {
            Some(Arc::new(ProtocolEndpoint::<DExtra>::new_with_settings(
                ProtocolKind::DExtra,
                default_module,
                Arc::clone(&authorizer_arc),
                voice_bus.clone(),
                settings.clone(),
                configured_modules.clone(),
            )))
        } else {
            None
        };
        let dplus = if config.is_enabled(ProtocolKind::DPlus) {
            Some(Arc::new(ProtocolEndpoint::<DPlus>::new_with_settings(
                ProtocolKind::DPlus,
                default_module,
                Arc::clone(&authorizer_arc),
                voice_bus.clone(),
                settings.clone(),
                configured_modules.clone(),
            )))
        } else {
            None
        };
        let dcs = if config.is_enabled(ProtocolKind::Dcs) {
            Some(Arc::new(ProtocolEndpoint::<Dcs>::new_with_settings(
                ProtocolKind::Dcs,
                default_module,
                Arc::clone(&authorizer_arc),
                voice_bus.clone(),
                settings,
                configured_modules,
            )))
        } else {
            None
        };
        Self {
            config,
            dextra,
            dplus,
            dcs,
            dextra_socket: sockets.dextra,
            dplus_socket: sockets.dplus,
            dcs_socket: sockets.dcs,
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
    #[cfg(test)]
    fn voice_bus(&self) -> Option<broadcast::Sender<CrossProtocolEvent>> {
        self.voice_bus.clone()
    }

    /// Read-only access to the configuration.
    #[must_use]
    pub const fn config(&self) -> &ReflectorConfig {
        &self.config
    }

    /// Obtain the stop signal before transferring this reflector into `run`.
    #[must_use]
    pub fn shutdown_handle(&self) -> ShutdownHandle {
        ShutdownHandle {
            sender: self.shutdown_tx.clone(),
        }
    }

    /// Run until shutdown.
    ///
    /// Spawns one tokio task per enabled protocol endpoint
    /// (`DExtra`, `DPlus`, `DCS`) and waits until every task returns.
    /// Returns `Ok(())` on clean shutdown.
    ///
    /// # Errors
    ///
    /// Returns [`ShellError::Io`] if binding fails before tasks start. After
    /// tasks start, [`ShellError::Endpoints`] retains every endpoint failure,
    /// including typed [`ShellError::Task`] causes. The first failure requests
    /// shutdown of the other endpoints and every task is joined before return.
    ///
    /// # Cancellation safety
    ///
    /// Consuming `self` prevents a second run from reusing client state. Dropping
    /// the future requests abort through the `JoinSet`; task destruction is
    /// asynchronous and is not joined by `Drop`. Signal [`ShutdownHandle`] and
    /// await this future for confirmed completion. A send pass finishes before
    /// observing shutdown, with a two-second deadline for each recipient.
    pub async fn run(self) -> Result<(), ShellError> {
        let mut tasks: JoinSet<Result<(), ShellError>> = JoinSet::new();

        if *self.shutdown_rx.borrow() {
            return Ok(());
        }
        // Bind every enabled socket before starting any task. A bind failure
        // therefore cannot strand previously started endpoint tasks.
        let dextra_socket = match (&self.dextra, self.dextra_socket) {
            (Some(_), Some(socket)) => Some(socket),
            (Some(_), None) => Some(Arc::new(
                UdpSocket::bind(self.config.bind_addr_for(ProtocolKind::DExtra)).await?,
            )),
            (None, _) => None,
        };
        let dplus_socket = match (&self.dplus, self.dplus_socket) {
            (Some(_), Some(socket)) => Some(socket),
            (Some(_), None) => Some(Arc::new(
                UdpSocket::bind(self.config.bind_addr_for(ProtocolKind::DPlus)).await?,
            )),
            (None, _) => None,
        };
        let dcs_socket = match (&self.dcs, self.dcs_socket) {
            (Some(_), Some(socket)) => Some(socket),
            (Some(_), None) => Some(Arc::new(
                UdpSocket::bind(self.config.bind_addr_for(ProtocolKind::Dcs)).await?,
            )),
            (None, _) => None,
        };

        // Each protocol binds its own resolved address (explicit
        // override → ephemeral → verbatim single-protocol → standard
        // port; see `ReflectorConfig::bind_addr_for`) so the
        // library-default config with all three protocols enabled
        // can start on a fixed base port without `AddrInUse`.
        if let (Some(endpoint), Some(socket)) = (self.dextra.as_ref(), dextra_socket) {
            let shutdown = self.shutdown_rx.clone();
            let endpoint = Arc::clone(endpoint);
            let _handle = tasks.spawn(async move { endpoint.run(socket, shutdown).await });
        }

        if let (Some(endpoint), Some(socket)) = (self.dplus.as_ref(), dplus_socket) {
            let shutdown = self.shutdown_rx.clone();
            let endpoint = Arc::clone(endpoint);
            let _handle = tasks.spawn(async move { endpoint.run(socket, shutdown).await });
        }

        if let (Some(endpoint), Some(socket)) = (self.dcs.as_ref(), dcs_socket) {
            let shutdown = self.shutdown_rx.clone();
            let endpoint = Arc::clone(endpoint);
            let _handle = tasks.spawn(async move { endpoint.run(socket, shutdown).await });
        }

        let mut failures = Vec::new();
        while let Some(join_result) = tasks.join_next().await {
            match join_result {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    failures.push(e);
                    let _previous = self.shutdown_tx.send_replace(true);
                }
                Err(join_err) => {
                    failures.push(ShellError::Task(join_err));
                    let _previous = self.shutdown_tx.send_replace(true);
                }
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(ShellError::Endpoints { failures })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Reflector;
    use crate::reflector::authorizer::AllowAllAuthorizer;
    use crate::reflector::config::ReflectorConfig;
    use dstar_gateway_core::types::ProtocolKind;
    use dstar_gateway_core::types::{Module, RoutingCallsign};
    use std::collections::HashSet;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::sync::Arc;
    use tokio::net::UdpSocket;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    const BIND: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);
    const REFLECTOR: RoutingCallsign = match RoutingCallsign::from_padded_bytes(*b"REF030 ") {
        Ok(value) => value,
        Err(_) => unreachable!(),
    };

    fn module_set(modules: &[Module]) -> HashSet<Module> {
        let mut set = HashSet::new();
        for &m in modules {
            let _ = set.insert(m);
        }
        set
    }

    fn config() -> Result<ReflectorConfig, Box<dyn std::error::Error>> {
        let cfg = ReflectorConfig::builder()
            .callsign(REFLECTOR)
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
            .callsign(REFLECTOR)
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
    async fn multi_protocol_run_binds_per_protocol_never_the_shared_config_bind() -> TestResult {
        // Regression trap for `run()` binding `config.bind` once per
        // enabled protocol (the old behavior, which aborted with
        // AddrInUse on any fixed port). The explicit per-protocol
        // overrides resolve to three ephemeral ports, so correct code
        // never touches the fixed `bind` port at all, while regressed
        // code binds it three times and fails with AddrInUse
        // regardless of that port's availability. This deliberately
        // avoids binding the standard 30001/20001/30051 ports (rule 4
        // is covered port-free by the `bind_addr_for` unit tests in
        // config.rs); squatting them live would collide with a
        // running polaris or an overlapping test suite.
        let ephemeral = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);
        let cfg = ReflectorConfig::builder()
            .callsign(REFLECTOR)
            .module_set(module_set(&[Module::C]))
            .bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 45123))
            .protocol_bind(ProtocolKind::DExtra, ephemeral)
            .protocol_bind(ProtocolKind::DPlus, ephemeral)
            .protocol_bind(ProtocolKind::Dcs, ephemeral)
            .build()?;
        assert_eq!(
            cfg.enabled_protocols.len(),
            3,
            "library default enables all three protocols"
        );
        let reflector = Reflector::new(cfg, AllowAllAuthorizer);
        let shutdown = reflector.shutdown_handle();
        let run_task = tokio::spawn(reflector.run());
        // Give all three endpoint tasks a moment to bind and start;
        // a bind conflict surfaces as an early Err from `run`.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        shutdown.shutdown();
        let result = tokio::time::timeout(std::time::Duration::from_secs(2), run_task).await??;
        assert!(
            result.is_ok(),
            "all three endpoints must start; AddrInUse means run() reverted \
             to binding config.bind once per protocol: {result:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn shutdown_before_run_returns_cleanly() -> TestResult {
        let socket = UdpSocket::bind("127.0.0.1:0").await?;
        let bound = socket.local_addr()?;
        let cfg = ReflectorConfig::builder()
            .callsign(REFLECTOR)
            .module_set(module_set(&[Module::C]))
            .bind(bound)
            .disable(ProtocolKind::DPlus)
            .disable(ProtocolKind::Dcs)
            .build()?;
        let reflector = Reflector::new_with_socket(cfg, AllowAllAuthorizer, Arc::new(socket));
        reflector.shutdown_handle().shutdown();
        // `run` should return immediately because shutdown is already `true`.
        let result =
            tokio::time::timeout(std::time::Duration::from_secs(2), reflector.run()).await?;
        assert!(result.is_ok());
        Ok(())
    }

    struct UnwindingAuthorizer;

    impl crate::ClientAuthorizer for UnwindingAuthorizer {
        fn authorize(
            &self,
            _request: &crate::LinkAttempt,
        ) -> Result<crate::AccessPolicy, crate::RejectReason> {
            std::panic::resume_unwind(Box::new("injected endpoint failure"))
        }
    }

    #[tokio::test]
    async fn endpoint_panic_retains_typed_cause_and_joins_other_endpoints() -> TestResult {
        let dextra = Arc::new(UdpSocket::bind(BIND).await?);
        let dplus = Arc::new(UdpSocket::bind(BIND).await?);
        let peer = dextra.local_addr()?;
        let config = ReflectorConfig::builder()
            .callsign(REFLECTOR)
            .module_set(HashSet::from([Module::C]))
            .bind(BIND)
            .disable(ProtocolKind::Dcs)
            .build()?;
        let reflector = Reflector::new_with_sockets(
            config,
            UnwindingAuthorizer,
            super::ProtocolSockets {
                dextra: Some(dextra),
                dplus: Some(dplus),
                dcs: None,
            },
        );
        let retained = Arc::downgrade(reflector.dplus.as_ref().ok_or("DPlus endpoint")?);
        let _shutdown = reflector.shutdown_handle();
        let task = tokio::spawn(reflector.run());
        let client = UdpSocket::bind(BIND).await?;
        let _sent = client.send_to(b"W1AW    BC\0", peer).await?;
        let result = tokio::time::timeout(std::time::Duration::from_secs(2), task).await??;
        let Err(crate::ShellError::Endpoints { failures }) = result else {
            return Err(format!("unexpected completion: {result:?}").into());
        };
        assert_eq!(failures.len(), 1, "one endpoint failed");
        assert!(
            matches!(failures.first(), Some(crate::ShellError::Task(error)) if error.is_panic()),
            "{failures:?}"
        );
        assert!(
            retained.upgrade().is_none(),
            "run must join and drop the other endpoint before returning"
        );
        Ok(())
    }
}
