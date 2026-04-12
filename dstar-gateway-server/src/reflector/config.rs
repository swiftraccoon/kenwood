//! `ReflectorConfig` — multi-client reflector configuration.
//!
//! The configuration uses a typestate builder (mirroring
//! `SessionBuilder` in `dstar-gateway-core`) so required fields
//! (`callsign`, `modules`, `bind`) are enforced at compile time. All
//! other fields have sensible defaults drawn from the rewrite
//! design spec Section 8.

use std::collections::HashSet;
use std::marker::PhantomData;
use std::net::SocketAddr;
use std::time::Duration;

use dstar_gateway_core::types::{Callsign, Module, ProtocolKind};

/// Marker indicating a required `ReflectorConfigBuilder` field has NOT been set.
#[derive(Debug)]
pub struct Missing;

/// Marker indicating a required `ReflectorConfigBuilder` field HAS been set.
#[derive(Debug)]
pub struct Provided;

/// Errors produced by [`ReflectorConfigBuilder::build`].
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// `modules` was set but contained zero entries.
    #[error("reflector module set must contain at least one module")]
    EmptyModules,
}

/// Complete configuration for a multi-client reflector.
///
/// Construct via [`Self::builder`] — the typestate builder enforces
/// that `callsign`, `modules`, and `bind` are set before `build` is
/// callable. All other fields carry defaults documented on
/// [`ReflectorConfigBuilder`].
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct ReflectorConfig {
    /// Reflector's own callsign (e.g. `REF030`).
    pub callsign: Callsign,
    /// Modules this reflector exposes.
    pub modules: HashSet<Module>,
    /// UDP bind address.
    pub bind: SocketAddr,
    /// Maximum clients allowed per module.
    pub max_clients_per_module: usize,
    /// Maximum clients allowed across all modules.
    pub max_total_clients: usize,
    /// Which protocol endpoints are enabled.
    ///
    /// Default: all three (`DPlus`, `DExtra`, `DCS`). Use
    /// [`ReflectorConfigBuilder::disable`] to remove one from the
    /// set before building.
    pub enabled_protocols: HashSet<ProtocolKind>,
    /// Interval between keepalive polls sent to each client.
    pub keepalive_interval: Duration,
    /// Inactivity window after which a silent client is evicted.
    pub keepalive_inactivity_timeout: Duration,
    /// Inactivity window after which a stalled voice stream is closed.
    pub voice_inactivity_timeout: Duration,
    /// Whether voice from one protocol should be forwarded to clients on another protocol.
    pub cross_protocol_forwarding: bool,
    /// Per-client TX rate limit, in voice frames per second.
    ///
    /// Defaults to `60.0` (3× the nominal 20 fps D-STAR voice rate)
    /// so a legitimate voice stream never hits the limit, but a
    /// client trying to saturate the reflector with a burst of
    /// 200+ fps gets rate-limited. Set higher if you expect large
    /// bursts of legitimate traffic.
    pub tx_rate_limit_frames_per_sec: f64,
}

impl ReflectorConfig {
    /// Check whether a specific protocol endpoint is enabled.
    #[must_use]
    pub fn is_enabled(&self, protocol: ProtocolKind) -> bool {
        self.enabled_protocols.contains(&protocol)
    }

    /// Start building a [`ReflectorConfig`].
    ///
    /// The returned builder is a typestate: [`ReflectorConfigBuilder::build`]
    /// only compiles when `.callsign()`, `.module_set()`, and
    /// `.bind()` have all been called. Skipping any of the three is
    /// a compile error — the type parameters flip from [`Missing`]
    /// to [`Provided`] as each setter is invoked.
    ///
    /// # Example
    ///
    /// ```
    /// use std::collections::HashSet;
    /// use dstar_gateway_core::types::{Callsign, Module};
    /// use dstar_gateway_server::ReflectorConfig;
    ///
    /// let mut modules = HashSet::new();
    /// let _ = modules.insert(Module::try_from_char('C')?);
    ///
    /// let config = ReflectorConfig::builder()
    ///     .callsign(Callsign::try_from_str("REF999")?)
    ///     .module_set(modules)
    ///     .bind("0.0.0.0:30001".parse()?)
    ///     .disable(dstar_gateway_core::types::ProtocolKind::DPlus)
    ///     .disable(dstar_gateway_core::types::ProtocolKind::Dcs)
    ///     .build()?;
    /// assert!(config.is_enabled(dstar_gateway_core::types::ProtocolKind::DExtra));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[must_use]
    pub fn builder() -> ReflectorConfigBuilder<Missing, Missing, Missing> {
        ReflectorConfigBuilder::empty()
    }
}

/// Typestate builder for [`ReflectorConfig`].
///
/// Parameters:
///
/// - `Cs` — `Missing` or `Provided`, tracks whether the callsign has been set.
/// - `Ms` — tracks whether the module set has been provided.
/// - `Bn` — tracks whether the bind address has been set.
///
/// [`Self::build`] is only implemented when all three markers are
/// [`Provided`] — forgetting any required field turns `.build()` into
/// a compile error.
#[derive(Debug)]
pub struct ReflectorConfigBuilder<Cs, Ms, Bn> {
    callsign: Option<Callsign>,
    modules: Option<HashSet<Module>>,
    bind: Option<SocketAddr>,
    max_clients_per_module: usize,
    max_total_clients: usize,
    enabled_protocols: HashSet<ProtocolKind>,
    keepalive_interval: Duration,
    keepalive_inactivity_timeout: Duration,
    voice_inactivity_timeout: Duration,
    cross_protocol_forwarding: bool,
    tx_rate_limit_frames_per_sec: f64,
    _cs: PhantomData<Cs>,
    _ms: PhantomData<Ms>,
    _bn: PhantomData<Bn>,
}

impl ReflectorConfigBuilder<Missing, Missing, Missing> {
    /// Create an empty builder with default optional values.
    ///
    /// All three protocols are enabled by default; call
    /// [`ReflectorConfigBuilder::disable`] to remove one.
    fn empty() -> Self {
        let mut enabled = HashSet::new();
        let _a = enabled.insert(ProtocolKind::DPlus);
        let _b = enabled.insert(ProtocolKind::DExtra);
        let _c = enabled.insert(ProtocolKind::Dcs);
        Self {
            callsign: None,
            modules: None,
            bind: None,
            max_clients_per_module: 50,
            max_total_clients: 250,
            enabled_protocols: enabled,
            keepalive_interval: Duration::from_secs(1),
            keepalive_inactivity_timeout: Duration::from_secs(30),
            voice_inactivity_timeout: Duration::from_secs(2),
            cross_protocol_forwarding: false,
            tx_rate_limit_frames_per_sec: 60.0,
            _cs: PhantomData,
            _ms: PhantomData,
            _bn: PhantomData,
        }
    }
}

impl<Cs, Ms, Bn> ReflectorConfigBuilder<Cs, Ms, Bn> {
    /// Set the reflector callsign.
    #[must_use]
    pub fn callsign(self, callsign: Callsign) -> ReflectorConfigBuilder<Provided, Ms, Bn> {
        ReflectorConfigBuilder {
            callsign: Some(callsign),
            modules: self.modules,
            bind: self.bind,
            max_clients_per_module: self.max_clients_per_module,
            max_total_clients: self.max_total_clients,
            enabled_protocols: self.enabled_protocols,
            keepalive_interval: self.keepalive_interval,
            keepalive_inactivity_timeout: self.keepalive_inactivity_timeout,
            voice_inactivity_timeout: self.voice_inactivity_timeout,
            cross_protocol_forwarding: self.cross_protocol_forwarding,
            tx_rate_limit_frames_per_sec: self.tx_rate_limit_frames_per_sec,
            _cs: PhantomData,
            _ms: PhantomData,
            _bn: PhantomData,
        }
    }

    /// Set the module set (`HashSet<Module>` — pass one or more module letters).
    #[must_use]
    pub fn module_set(self, modules: HashSet<Module>) -> ReflectorConfigBuilder<Cs, Provided, Bn> {
        ReflectorConfigBuilder {
            callsign: self.callsign,
            modules: Some(modules),
            bind: self.bind,
            max_clients_per_module: self.max_clients_per_module,
            max_total_clients: self.max_total_clients,
            enabled_protocols: self.enabled_protocols,
            keepalive_interval: self.keepalive_interval,
            keepalive_inactivity_timeout: self.keepalive_inactivity_timeout,
            voice_inactivity_timeout: self.voice_inactivity_timeout,
            cross_protocol_forwarding: self.cross_protocol_forwarding,
            tx_rate_limit_frames_per_sec: self.tx_rate_limit_frames_per_sec,
            _cs: PhantomData,
            _ms: PhantomData,
            _bn: PhantomData,
        }
    }

    /// Set the UDP bind address.
    #[must_use]
    pub fn bind(self, bind: SocketAddr) -> ReflectorConfigBuilder<Cs, Ms, Provided> {
        ReflectorConfigBuilder {
            callsign: self.callsign,
            modules: self.modules,
            bind: Some(bind),
            max_clients_per_module: self.max_clients_per_module,
            max_total_clients: self.max_total_clients,
            enabled_protocols: self.enabled_protocols,
            keepalive_interval: self.keepalive_interval,
            keepalive_inactivity_timeout: self.keepalive_inactivity_timeout,
            voice_inactivity_timeout: self.voice_inactivity_timeout,
            cross_protocol_forwarding: self.cross_protocol_forwarding,
            tx_rate_limit_frames_per_sec: self.tx_rate_limit_frames_per_sec,
            _cs: PhantomData,
            _ms: PhantomData,
            _bn: PhantomData,
        }
    }

    /// Override the maximum clients per module (default `50`).
    #[must_use]
    pub const fn max_clients_per_module(mut self, value: usize) -> Self {
        self.max_clients_per_module = value;
        self
    }

    /// Override the maximum total clients (default `250`).
    #[must_use]
    pub const fn max_total_clients(mut self, value: usize) -> Self {
        self.max_total_clients = value;
        self
    }

    /// Add a protocol to the enabled set.
    #[must_use]
    pub fn enable(mut self, protocol: ProtocolKind) -> Self {
        let _inserted = self.enabled_protocols.insert(protocol);
        self
    }

    /// Remove a protocol from the enabled set.
    #[must_use]
    pub fn disable(mut self, protocol: ProtocolKind) -> Self {
        let _removed = self.enabled_protocols.remove(&protocol);
        self
    }

    /// Override the keepalive poll interval (default `1s`).
    #[must_use]
    pub const fn keepalive_interval(mut self, value: Duration) -> Self {
        self.keepalive_interval = value;
        self
    }

    /// Override the keepalive inactivity timeout (default `30s`).
    #[must_use]
    pub const fn keepalive_inactivity_timeout(mut self, value: Duration) -> Self {
        self.keepalive_inactivity_timeout = value;
        self
    }

    /// Override the voice inactivity timeout (default `2s`).
    #[must_use]
    pub const fn voice_inactivity_timeout(mut self, value: Duration) -> Self {
        self.voice_inactivity_timeout = value;
        self
    }

    /// Enable or disable cross-protocol forwarding (default `false`).
    #[must_use]
    pub const fn cross_protocol_forwarding(mut self, value: bool) -> Self {
        self.cross_protocol_forwarding = value;
        self
    }

    /// Override the per-client TX rate limit in frames per second
    /// (default `60.0`).
    ///
    /// The default is 3× the nominal 20 fps D-STAR voice rate, so
    /// legitimate voice streams never hit the limit. Lower it to
    /// tighten the `DoS` envelope; raise it if you expect large bursts
    /// of legitimate traffic.
    #[must_use]
    pub const fn tx_rate_limit_frames_per_sec(mut self, value: f64) -> Self {
        self.tx_rate_limit_frames_per_sec = value;
        self
    }
}

impl ReflectorConfigBuilder<Provided, Provided, Provided> {
    /// Finalize the configuration.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::EmptyModules`] if the supplied module
    /// set was empty. All other required fields are guaranteed
    /// non-`None` by the typestate markers.
    pub fn build(self) -> Result<ReflectorConfig, ConfigError> {
        let Some(callsign) = self.callsign else {
            unreachable!("Provided marker guarantees callsign is Some");
        };
        let Some(modules) = self.modules else {
            unreachable!("Provided marker guarantees modules is Some");
        };
        let Some(bind) = self.bind else {
            unreachable!("Provided marker guarantees bind is Some");
        };
        if modules.is_empty() {
            return Err(ConfigError::EmptyModules);
        }
        Ok(ReflectorConfig {
            callsign,
            modules,
            bind,
            max_clients_per_module: self.max_clients_per_module,
            max_total_clients: self.max_total_clients,
            enabled_protocols: self.enabled_protocols,
            keepalive_interval: self.keepalive_interval,
            keepalive_inactivity_timeout: self.keepalive_inactivity_timeout,
            voice_inactivity_timeout: self.voice_inactivity_timeout,
            cross_protocol_forwarding: self.cross_protocol_forwarding,
            tx_rate_limit_frames_per_sec: self.tx_rate_limit_frames_per_sec,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Callsign, ConfigError, Duration, HashSet, Module, ProtocolKind, ReflectorConfig, SocketAddr,
    };
    use std::net::{IpAddr, Ipv4Addr};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn module_set(modules: &[Module]) -> HashSet<Module> {
        let mut set = HashSet::new();
        for &m in modules {
            let _ = set.insert(m);
        }
        set
    }

    const BIND: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 30001);

    #[test]
    fn happy_path_builds_with_defaults() -> TestResult {
        let config = ReflectorConfig::builder()
            .callsign(Callsign::from_wire_bytes(*b"REF030  "))
            .module_set(module_set(&[Module::A, Module::B, Module::C]))
            .bind(BIND)
            .build()?;
        assert_eq!(config.callsign, Callsign::from_wire_bytes(*b"REF030  "));
        assert_eq!(config.modules.len(), 3);
        assert_eq!(config.bind, BIND);
        // Defaults
        assert_eq!(config.max_clients_per_module, 50);
        assert_eq!(config.max_total_clients, 250);
        assert!(config.is_enabled(ProtocolKind::DPlus));
        assert!(config.is_enabled(ProtocolKind::DExtra));
        assert!(config.is_enabled(ProtocolKind::Dcs));
        assert_eq!(config.keepalive_interval, Duration::from_secs(1));
        assert_eq!(config.keepalive_inactivity_timeout, Duration::from_secs(30));
        assert_eq!(config.voice_inactivity_timeout, Duration::from_secs(2));
        assert!(!config.cross_protocol_forwarding);
        Ok(())
    }

    #[test]
    fn empty_module_set_is_rejected() {
        let result = ReflectorConfig::builder()
            .callsign(Callsign::from_wire_bytes(*b"REF030  "))
            .module_set(HashSet::new())
            .bind(BIND)
            .build();
        assert!(matches!(result, Err(ConfigError::EmptyModules)));
    }

    #[test]
    fn overrides_replace_defaults() -> TestResult {
        let config = ReflectorConfig::builder()
            .callsign(Callsign::from_wire_bytes(*b"REF030  "))
            .module_set(module_set(&[Module::C]))
            .bind(BIND)
            .max_clients_per_module(10)
            .max_total_clients(40)
            .disable(ProtocolKind::DPlus)
            .disable(ProtocolKind::Dcs)
            .keepalive_interval(Duration::from_millis(500))
            .keepalive_inactivity_timeout(Duration::from_secs(10))
            .voice_inactivity_timeout(Duration::from_millis(1500))
            .cross_protocol_forwarding(true)
            .build()?;
        assert_eq!(config.max_clients_per_module, 10);
        assert_eq!(config.max_total_clients, 40);
        assert!(!config.is_enabled(ProtocolKind::DPlus));
        assert!(config.is_enabled(ProtocolKind::DExtra));
        assert!(!config.is_enabled(ProtocolKind::Dcs));
        assert_eq!(config.keepalive_interval, Duration::from_millis(500));
        assert_eq!(config.keepalive_inactivity_timeout, Duration::from_secs(10));
        assert_eq!(config.voice_inactivity_timeout, Duration::from_millis(1500));
        assert!(config.cross_protocol_forwarding);
        Ok(())
    }

    #[test]
    fn cross_protocol_forwarding_defaults_false() -> TestResult {
        let config = ReflectorConfig::builder()
            .callsign(Callsign::from_wire_bytes(*b"REF030  "))
            .module_set(module_set(&[Module::C]))
            .bind(BIND)
            .build()?;
        assert!(!config.cross_protocol_forwarding);
        Ok(())
    }
}
