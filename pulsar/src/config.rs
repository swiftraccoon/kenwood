// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later

//! Construction-validated TOML and programmatic recorder settings.

use std::collections::HashSet;
use std::fmt;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::num::NonZeroU16;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::{Duration, Instant};

use dmr_rewind::{DestinationId, SubscriberId};
use serde::Deserialize;

use crate::capture::{CaptureRecordLimit, CaptureRecordLimitError};

/// Configuration parsing, loading or construction failure.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// The configuration file could not be read.
    #[error("read {path}: {source}")]
    Io {
        /// Path that failed to read.
        path: PathBuf,
        /// Underlying filesystem error.
        #[source]
        source: std::io::Error,
    },
    /// Input did not match the TOML schema.
    #[error("parse: {0}")]
    Parse(#[from] toml::de::Error),
    /// The capture retention limit was invalid.
    #[error("max_capture_records_per_call: {0}")]
    CaptureLimit(#[from] CaptureRecordLimitError),
    /// A terminal or outbound destination ID was invalid.
    #[error("{0}")]
    Terminal(#[from] dmr_rewind::ConfigError),
    /// The output directory contained no path component.
    #[error("recordings_dir must not be empty")]
    EmptyDirectory,
    /// No master connection was configured.
    #[error("no [[master]] connections configured")]
    EmptyMasters,
    /// Master names repeated under ASCII case folding.
    #[error("duplicate master name {name:?}")]
    DuplicateMaster {
        /// Repeated name.
        name: String,
    },
    /// An operator label contained no non-whitespace text.
    #[error("master name must not be empty")]
    EmptyName,
    /// A host was neither an IP literal nor a syntactically valid ASCII DNS name.
    #[error("invalid master host {host:?}; use an IP literal or ASCII DNS name")]
    Host {
        /// Rejected host.
        host: String,
    },
    /// A credential name did not follow the portable environment-name grammar.
    #[error("invalid password environment name {name:?}; use [A-Za-z_][A-Za-z0-9_]*")]
    PasswordVariable {
        /// Rejected name.
        name: String,
    },
    /// The remote UDP port was zero.
    #[error("master port must be greater than zero")]
    ZeroPort,
    /// A literal master IP and local bind used different IP families.
    #[error("literal master address and local bind must use the same IP family")]
    AddressFamily,
    /// A master had no group subscription.
    #[error("master talkgroups must not be empty")]
    EmptyTalkgroups,
    /// An outbound subscription repeated within the same kind.
    #[error("duplicate {kind:?} destination {destination}")]
    DuplicateDestination {
        /// Group or private subscription list.
        kind: dmr_rewind::SubscriptionKind,
        /// Repeated ID.
        destination: DestinationId,
    },
    /// A hang interval was zero or a session timeout did not exceed keepalive.
    #[error("invalid {kind:?} interval {duration:?}")]
    Interval {
        /// Setting being checked.
        kind: IntervalKind,
        /// Rejected duration.
        duration: Duration,
    },
    /// A positive interval exceeded the runtime clock's representation.
    #[error("{kind:?} interval {duration:?} is too large for the runtime clock")]
    ClockRange {
        /// Setting being checked.
        kind: IntervalKind,
        /// Rejected duration.
        duration: Duration,
    },
}

/// A recorder timing policy, distinct from UTC metadata timestamps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntervalKind {
    /// Monotonic quiet time before finalizing an incomplete call.
    CallHang,
    /// Valid-packet timeout of an Open Terminal connection.
    ServerTimeout,
}

/// Nonempty trimmed operator label; filesystem output sanitizes it separately.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MasterName(String);

impl MasterName {
    /// Validate a nonempty trimmed label.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::EmptyName`] for empty or whitespace-only input.
    pub fn new(mut value: String) -> Result<Self, ConfigError> {
        if value.trim().is_empty() {
            return Err(ConfigError::EmptyName);
        }
        if value.trim().len() != value.len() {
            value = value.trim().to_owned();
        }
        Ok(Self(value))
    }

    /// Borrow the label.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for MasterName {
    type Err = ConfigError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value.to_owned())
    }
}

impl AsRef<str> for MasterName {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}
impl From<MasterName> for String {
    fn from(value: MasterName) -> Self {
        value.0
    }
}

/// An IP literal or ASCII DNS name without a port or URL scheme.
///
/// DNS labels contain letters, digits and interior hyphens, with at most 63
/// bytes per label and 253 bytes total excluding an optional final dot.
/// Construction checks syntax; DNS resolution remains fallible network I/O.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MasterHost(String);

impl MasterHost {
    /// Check host syntax before constructing configuration.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::Host`] for empty, non-ASCII or malformed hosts.
    pub fn new(mut value: String) -> Result<Self, ConfigError> {
        if value.trim().len() != value.len() {
            value = value.trim().to_owned();
        }
        let dns = value.strip_suffix('.').unwrap_or(&value);
        let valid = value.parse::<IpAddr>().is_ok()
            || (!dns.is_empty()
                && dns.len() <= 253
                && dns.split('.').all(|label| {
                    !label.is_empty()
                        && label.len() <= 63
                        && label
                            .as_bytes()
                            .first()
                            .is_some_and(u8::is_ascii_alphanumeric)
                        && label
                            .as_bytes()
                            .last()
                            .is_some_and(u8::is_ascii_alphanumeric)
                        && label
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                }));
        if valid {
            Ok(Self(value))
        } else {
            Err(ConfigError::Host { host: value })
        }
    }

    /// Borrow the host text accepted by Tokio's resolver.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for MasterHost {
    type Err = ConfigError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value.to_owned())
    }
}

impl AsRef<str> for MasterHost {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}
impl From<MasterHost> for String {
    fn from(value: MasterHost) -> Self {
        value.0
    }
}

/// Portable environment variable name `[A-Za-z_][A-Za-z0-9_]*`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PasswordVariable(String);

impl PasswordVariable {
    /// Validate the name, retaining its case exactly.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::PasswordVariable`] for an invalid name.
    pub fn new(value: String) -> Result<Self, ConfigError> {
        let mut bytes = value.bytes();
        let valid = bytes
            .next()
            .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
            && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_');
        if valid {
            Ok(Self(value))
        } else {
            Err(ConfigError::PasswordVariable { name: value })
        }
    }

    /// Borrow the exact environment name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for PasswordVariable {
    type Err = ConfigError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value.to_owned())
    }
}

impl AsRef<str> for PasswordVariable {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}
impl From<PasswordVariable> for String {
    fn from(value: PasswordVariable) -> Self {
        value.0
    }
}

/// Fully validated recorder settings; see the [configuration workflow](crate#library-workflows).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    recordings_dir: PathBuf,
    max_capture_records_per_call: CaptureRecordLimit,
    call_hang: Duration,
    server_timeout: Duration,
    masters: Vec<MasterConfig>,
}

impl Config {
    /// Construct with a three-second hang and fifteen-second session timeout.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::EmptyDirectory`], [`ConfigError::EmptyMasters`]
    /// or [`ConfigError::DuplicateMaster`] for invalid paths/master lists.
    pub fn new(
        recordings_dir: PathBuf,
        limit: CaptureRecordLimit,
        masters: Vec<MasterConfig>,
    ) -> Result<Self, ConfigError> {
        if recordings_dir.as_os_str().is_empty() {
            return Err(ConfigError::EmptyDirectory);
        }
        if masters.is_empty() {
            return Err(ConfigError::EmptyMasters);
        }
        let mut names = HashSet::new();
        for master in &masters {
            if !names.insert(master.name.as_str().to_ascii_lowercase()) {
                return Err(ConfigError::DuplicateMaster {
                    name: master.name.as_str().to_owned(),
                });
            }
        }
        Ok(Self {
            recordings_dir,
            max_capture_records_per_call: limit,
            call_hang: Duration::from_secs(3),
            server_timeout: Duration::from_secs(15),
            masters,
        })
    }

    /// Set a positive monotonic inactivity interval.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::Interval`] for zero or [`ConfigError::ClockRange`]
    /// when the runtime clock cannot represent the interval.
    pub fn with_call_hang(mut self, duration: Duration) -> Result<Self, ConfigError> {
        validate_interval(IntervalKind::CallHang, duration)?;
        self.call_hang = duration;
        Ok(self)
    }

    /// Set the valid-packet timeout, strictly greater than five seconds.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::Interval`] at five seconds or less, or
    /// [`ConfigError::ClockRange`] for an unrepresentable interval.
    pub fn with_server_timeout(mut self, duration: Duration) -> Result<Self, ConfigError> {
        validate_interval(IntervalKind::ServerTimeout, duration)?;
        self.server_timeout = duration;
        Ok(self)
    }

    /// Borrow the output directory.
    #[must_use]
    pub fn recordings_dir(&self) -> &Path {
        &self.recordings_dir
    }
    /// Return the shared per-call retention limit.
    #[must_use]
    pub const fn max_capture_records_per_call(&self) -> CaptureRecordLimit {
        self.max_capture_records_per_call
    }
    /// Return the monotonic inactivity interval.
    #[must_use]
    pub const fn call_hang(&self) -> Duration {
        self.call_hang
    }
    /// Return the valid-packet timeout.
    #[must_use]
    pub const fn server_timeout(&self) -> Duration {
        self.server_timeout
    }
    /// Borrow all master connections in configured order.
    #[must_use]
    pub fn masters(&self) -> &[MasterConfig] {
        &self.masters
    }
    /// Consume configuration into its master connections.
    #[must_use]
    pub fn into_masters(self) -> Vec<MasterConfig> {
        self.masters
    }

    /// Read and validate a TOML file.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::Io`] on file errors; parsing/construction errors
    /// are the same as [`FromStr::from_str`].
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        text.parse()
    }
}

impl FromStr for Config {
    type Err = ConfigError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let raw: RawConfig = toml::from_str(text)?;
        let masters = raw
            .masters
            .into_iter()
            .map(RawMaster::validate)
            .collect::<Result<Vec<_>, _>>()?;
        Self::new(
            raw.recordings_dir,
            CaptureRecordLimit::new(raw.max_capture_records_per_call)?,
            masters,
        )?
        .with_call_hang(Duration::from_secs(raw.call_hang_secs))?
        .with_server_timeout(Duration::from_secs(raw.server_timeout_secs))
    }
}

/// A receive-only master connection with distinct operator and wire ID policies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MasterConfig {
    name: MasterName,
    host: MasterHost,
    port: NonZeroU16,
    dmr_id: SubscriberId,
    password_env: PasswordVariable,
    bind: SocketAddr,
    talkgroups: Vec<DestinationId>,
    private_ids: Vec<DestinationId>,
}

impl MasterConfig {
    /// Construct a master with group subscriptions and an ephemeral IPv4 bind.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::EmptyTalkgroups`] for no groups, or
    /// [`ConfigError::DuplicateDestination`] for repeated groups.
    pub fn new(
        name: MasterName,
        host: MasterHost,
        port: NonZeroU16,
        dmr_id: SubscriberId,
        password_env: PasswordVariable,
        talkgroups: Vec<DestinationId>,
    ) -> Result<Self, ConfigError> {
        if talkgroups.is_empty() {
            return Err(ConfigError::EmptyTalkgroups);
        }
        validate_destinations(dmr_rewind::SubscriptionKind::Group, &talkgroups)?;
        let bind = if host.as_str().parse::<IpAddr>().is_ok_and(|ip| ip.is_ipv6()) {
            SocketAddr::new(IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED), 0)
        } else {
            default_bind()
        };
        Ok(Self {
            name,
            host,
            port,
            dmr_id,
            password_env,
            bind,
            talkgroups,
            private_ids: Vec::new(),
        })
    }

    /// Select a local bind address; DNS results must contain the same IP family.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::AddressFamily`] for a mismatched literal IP.
    pub fn with_bind(mut self, bind: SocketAddr) -> Result<Self, ConfigError> {
        if self
            .host()
            .parse::<IpAddr>()
            .is_ok_and(|ip| ip.is_ipv4() != bind.is_ipv4())
        {
            return Err(ConfigError::AddressFamily);
        }
        self.bind = bind;
        Ok(self)
    }

    /// Set private-call subscriptions, preserving group subscriptions.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::DuplicateDestination`] for repeated private IDs.
    pub fn with_private_ids(mut self, ids: Vec<DestinationId>) -> Result<Self, ConfigError> {
        validate_destinations(dmr_rewind::SubscriptionKind::Private, &ids)?;
        self.private_ids = ids;
        Ok(self)
    }

    /// Borrow the operator label.
    #[must_use]
    pub fn name(&self) -> &str {
        self.name.as_str()
    }
    /// Borrow the validated resolver input.
    #[must_use]
    pub fn host(&self) -> &str {
        self.host.as_str()
    }
    /// Return the nonzero remote port.
    #[must_use]
    pub const fn port(&self) -> NonZeroU16 {
        self.port
    }
    /// Return the seven-digit subscriber allocation.
    #[must_use]
    pub const fn dmr_id(&self) -> SubscriberId {
        self.dmr_id
    }
    /// Borrow the credential environment name.
    #[must_use]
    pub fn password_env(&self) -> &str {
        self.password_env.as_str()
    }
    /// Return the local bind endpoint.
    #[must_use]
    pub const fn bind(&self) -> SocketAddr {
        self.bind
    }
    /// Borrow group destinations.
    #[must_use]
    pub fn talkgroups(&self) -> &[DestinationId] {
        &self.talkgroups
    }
    /// Borrow private destinations.
    #[must_use]
    pub fn private_ids(&self) -> &[DestinationId] {
        &self.private_ids
    }

    /// Read the credential with a redacted debug representation.
    ///
    /// # Errors
    ///
    /// Returns [`PasswordError`] for an absent, non-Unicode or empty value.
    pub fn load_password(&self) -> Result<SecretString, PasswordError> {
        match std::env::var(self.password_env()) {
            Ok(value) if value.is_empty() => Err(PasswordError::Empty {
                variable: self.password_env().to_owned(),
            }),
            Ok(value) => Ok(SecretString(value)),
            Err(std::env::VarError::NotPresent) => Err(PasswordError::Missing {
                variable: self.password_env().to_owned(),
            }),
            Err(std::env::VarError::NotUnicode(_)) => Err(PasswordError::NotUnicode {
                variable: self.password_env().to_owned(),
            }),
        }
    }
}

/// Credential whose [`Debug`] implementation redacts its contents.
#[derive(Clone, PartialEq, Eq)]
pub struct SecretString(String);

impl SecretString {
    /// Borrow credential bytes as UTF-8; never log or serialize this value.
    #[must_use]
    pub fn expose_secret(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretString([REDACTED])")
    }
}

/// Credential lookup failure, retaining the variable name but no secret bytes.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PasswordError {
    /// Variable was absent.
    #[error("password environment variable {variable:?} is not set")]
    Missing {
        /// Variable name.
        variable: String,
    },
    /// Value was not Unicode.
    #[error("password environment variable {variable:?} is not valid Unicode")]
    NotUnicode {
        /// Variable name.
        variable: String,
    },
    /// Value was empty.
    #[error("password environment variable {variable:?} is empty")]
    Empty {
        /// Variable name.
        variable: String,
    },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    #[serde(default = "default_recordings_dir")]
    recordings_dir: PathBuf,
    max_capture_records_per_call: u64,
    #[serde(default = "default_call_hang_secs")]
    call_hang_secs: u64,
    #[serde(default = "default_server_timeout_secs")]
    server_timeout_secs: u64,
    #[serde(default, rename = "master")]
    masters: Vec<RawMaster>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawMaster {
    name: String,
    host: String,
    #[serde(default = "default_master_port")]
    port: u16,
    dmr_id: u32,
    password_env: String,
    #[serde(default = "default_bind")]
    bind: SocketAddr,
    talkgroups: Vec<u32>,
    #[serde(default)]
    private_ids: Vec<u32>,
}

impl RawMaster {
    fn validate(self) -> Result<MasterConfig, ConfigError> {
        MasterConfig::new(
            MasterName::new(self.name)?,
            MasterHost::new(self.host)?,
            NonZeroU16::new(self.port).ok_or(ConfigError::ZeroPort)?,
            SubscriberId::new(self.dmr_id)?,
            PasswordVariable::new(self.password_env)?,
            self.talkgroups
                .into_iter()
                .map(DestinationId::new)
                .collect::<Result<_, _>>()?,
        )?
        .with_bind(self.bind)?
        .with_private_ids(
            self.private_ids
                .into_iter()
                .map(DestinationId::new)
                .collect::<Result<_, _>>()?,
        )
    }
}

fn validate_destinations(
    kind: dmr_rewind::SubscriptionKind,
    ids: &[DestinationId],
) -> Result<(), ConfigError> {
    let mut seen = HashSet::new();
    for &destination in ids {
        if !seen.insert(destination) {
            return Err(ConfigError::DuplicateDestination { kind, destination });
        }
    }
    Ok(())
}

fn validate_interval(kind: IntervalKind, duration: Duration) -> Result<(), ConfigError> {
    let minimum = match kind {
        IntervalKind::CallHang => Duration::ZERO,
        IntervalKind::ServerTimeout => dmr_rewind::DEFAULT_KEEPALIVE_INTERVAL,
    };
    if duration <= minimum {
        return Err(ConfigError::Interval { kind, duration });
    }
    if Instant::now().checked_add(duration).is_none() {
        return Err(ConfigError::ClockRange { kind, duration });
    }
    Ok(())
}

fn default_recordings_dir() -> PathBuf {
    PathBuf::from("dmr-recordings")
}
const fn default_call_hang_secs() -> u64 {
    3
}
const fn default_server_timeout_secs() -> u64 {
    15
}
const fn default_master_port() -> u16 {
    54_006
}
const fn default_bind() -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    type TestResult = Result<(), Box<dyn std::error::Error>>;
    const GOOD: &str = "max_capture_records_per_call = 20000\n[[master]]\nname = 'BM-US'\nhost = 'localhost'\ndmr_id = 3101234\npassword_env = 'PULSAR_PASSWORD'\ntalkgroups = [91, 3100]\nprivate_ids = [1234567]";

    #[test]
    fn toml_and_programmatic_construction_share_validation() -> TestResult {
        let config: Config = GOOD.parse()?;
        assert_eq!(config.recordings_dir(), Path::new("dmr-recordings"));
        assert_eq!(config.max_capture_records_per_call().get(), 20_000);
        assert_eq!(config.call_hang(), Duration::from_secs(3));
        assert_eq!(config.server_timeout(), Duration::from_secs(15));
        let master = config.masters().first().ok_or("missing master")?;
        assert_eq!(master.dmr_id().get(), 3_101_234);
        assert_eq!(
            master
                .talkgroups()
                .iter()
                .map(|id| id.get())
                .collect::<Vec<_>>(),
            [91, 3100]
        );
        assert!(matches!(
            config.clone().with_call_hang(Duration::ZERO),
            Err(ConfigError::Interval {
                kind: IntervalKind::CallHang,
                ..
            })
        ));
        assert!(matches!(
            config.clone().with_server_timeout(Duration::from_secs(5)),
            Err(ConfigError::Interval {
                kind: IntervalKind::ServerTimeout,
                ..
            })
        ));
        assert!(matches!(
            config.clone().with_call_hang(Duration::MAX),
            Err(ConfigError::ClockRange { .. })
        ));
        assert!(matches!(
            Config::new(
                PathBuf::new(),
                config.max_capture_records_per_call(),
                config.masters().to_vec()
            ),
            Err(ConfigError::EmptyDirectory)
        ));
        assert!(matches!(
            Config::new(
                "output".into(),
                config.max_capture_records_per_call(),
                vec![master.clone(), master.clone()]
            ),
            Err(ConfigError::DuplicateMaster { .. })
        ));
        let duplicate = master
            .clone()
            .with_private_ids(vec![DestinationId::new(91)?, DestinationId::new(91)?]);
        assert!(matches!(
            duplicate,
            Err(ConfigError::DuplicateDestination {
                kind: dmr_rewind::SubscriptionKind::Private,
                ..
            })
        ));
        Ok(())
    }

    #[test]
    fn scalar_names_ports_ids_and_lists_reject_invalid_input() -> TestResult {
        for host in ["localhost", "127.0.0.1", "::1", "a-b.example."] {
            let _valid = MasterHost::new(host.to_owned())?;
        }
        for host in [
            "",
            "a..b",
            "-x",
            "x-",
            "https://localhost",
            "[::1]:4",
            "a b",
            "é.test",
        ] {
            assert!(
                matches!(
                    MasterHost::new(host.to_owned()),
                    Err(ConfigError::Host { .. })
                ),
                "{host}"
            );
        }
        for name in ["", "1PASSWORD", "A=B", "A\0B", "A B"] {
            assert!(matches!(
                PasswordVariable::new(name.to_owned()),
                Err(ConfigError::PasswordVariable { .. })
            ));
        }
        for name in ["_", "PASSWORD", "Password_01"] {
            let _valid = PasswordVariable::new(name.to_owned())?;
        }
        assert!(matches!(
            MasterName::new(" \t".into()),
            Err(ConfigError::EmptyName)
        ));
        for (before, after) in [
            ("dmr_id = 3101234", "dmr_id = 0"),
            ("talkgroups = [91, 3100]", "talkgroups = [0]"),
            ("talkgroups = [91, 3100]", "talkgroups = [16777216]"),
        ] {
            assert!(matches!(
                GOOD.replace(before, after).parse::<Config>(),
                Err(ConfigError::Terminal(_))
            ));
        }
        assert!(matches!(
            GOOD.replace("host =", "port = 0\nhost =").parse::<Config>(),
            Err(ConfigError::ZeroPort)
        ));
        assert!(matches!(
            GOOD.replace("[91, 3100]", "[]").parse::<Config>(),
            Err(ConfigError::EmptyTalkgroups)
        ));
        assert!(matches!(
            GOOD.replace("[91, 3100]", "[91,91]").parse::<Config>(),
            Err(ConfigError::DuplicateDestination { .. })
        ));
        assert!(matches!(
            "max_capture_records_per_call = 1".parse::<Config>(),
            Err(ConfigError::EmptyMasters)
        ));
        assert!(matches!(
            GOOD.replace("20000", "0").parse::<Config>(),
            Err(ConfigError::CaptureLimit(CaptureRecordLimitError::Zero))
        ));
        Ok(())
    }

    #[test]
    fn secret_debug_is_redacted() {
        let secret = SecretString("do-not-print-me".to_owned());
        assert_eq!(format!("{secret:?}"), "SecretString([REDACTED])");
        assert_eq!(secret.expose_secret(), "do-not-print-me");
    }
}
