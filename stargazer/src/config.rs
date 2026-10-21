// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later

//! TOML configuration: global settings plus one `[[record]]` entry
//! per reflector, expanded into per-module [`Target`]s at load time.

use std::collections::HashSet;
use std::fmt;
use std::num::NonZeroU16;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use dstar_gateway_core::hosts::{HostAddress, HostAddressError};
use dstar_gateway_core::{DcsReflector, Module, ProtocolKind, RoutingCallsign};
use serde::Deserialize;

use crate::capture::{CaptureDurationLimit, ConcurrentCaptureLimit};

/// Reflector routing name safe as one filesystem component on supported platforms.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ReflectorName {
    text: String,
    routing: RoutingCallsign,
}

impl ReflectorName {
    /// Validate at most seven ASCII routing bytes and normalize letters to uppercase.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::ReflectorName`] for an empty or oversized name,
    /// a path separator, Windows reserved name, control byte or reserved punctuation.
    pub fn new(value: &str) -> Result<Self, ConfigError> {
        let routing = Self::validate(value)?;
        Ok(Self {
            text: value.to_ascii_uppercase(),
            routing,
        })
    }

    fn validate(value: &str) -> Result<RoutingCallsign, ConfigError> {
        if value.is_empty()
            || value.len() > 7
            || value.ends_with([' ', '.'])
            || value
                .bytes()
                .any(|byte| !(b' '..=b'~').contains(&byte) || b"<>:\"/\\|?*".contains(&byte))
        {
            return Err(ConfigError::ReflectorName);
        }
        let mut upper = [b' '; 7];
        for (output, input) in upper.iter_mut().zip(value.bytes()) {
            *output = input.to_ascii_uppercase();
        }
        let device = upper.split(|byte| *byte == b'.').next().unwrap_or_default();
        let device = device.trim_ascii_end();
        if matches!(device, b"CON" | b"PRN" | b"AUX" | b"NUL")
            || matches!(
                device,
                [b'C', b'O', b'M', b'1'..=b'9'] | [b'L', b'P', b'T', b'1'..=b'9']
            )
        {
            return Err(ConfigError::ReflectorName);
        }
        RoutingCallsign::from_padded_bytes(upper).map_err(ConfigError::Callsign)
    }

    /// Borrow the normalized routing name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// Obtain the validated padded routing callsign.
    #[must_use]
    pub const fn routing_callsign(&self) -> RoutingCallsign {
        self.routing
    }
}

impl fmt::Display for ReflectorName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
impl AsRef<str> for ReflectorName {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}
impl FromStr for ReflectorName {
    type Err = ConfigError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}
impl TryFrom<&str> for ReflectorName {
    type Error = ConfigError;
    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}
impl TryFrom<String> for ReflectorName {
    type Error = ConfigError;
    fn try_from(mut value: String) -> Result<Self, Self::Error> {
        let routing = Self::validate(&value)?;
        value.make_ascii_uppercase();
        Ok(Self {
            text: value,
            routing,
        })
    }
}
impl From<ReflectorName> for String {
    fn from(value: ReflectorName) -> Self {
        value.text
    }
}
impl Ord for ReflectorName {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.text.cmp(&other.text)
    }
}
impl PartialOrd for ReflectorName {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// A checked reflector/module directory and control identity.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TargetKey {
    reflector: ReflectorName,
    module: Module,
}
impl TargetKey {
    /// Pair a checked reflector name and module.
    #[must_use]
    pub const fn new(reflector: ReflectorName, module: Module) -> Self {
        Self { reflector, module }
    }
    /// Reflector routing name.
    #[must_use]
    pub const fn reflector(&self) -> &ReflectorName {
        &self.reflector
    }
    /// Remote module.
    #[must_use]
    pub const fn module(&self) -> Module {
        self.module
    }
}
impl fmt::Display for TargetKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}-{}", self.reflector, self.module)
    }
}
impl FromStr for TargetKey {
    type Err = ConfigError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (name, module) = value.rsplit_once('-').ok_or(ConfigError::TargetKey)?;
        Ok(Self::new(ReflectorName::new(name)?, parse_module(module)?))
    }
}
impl Ord for TargetKey {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.reflector
            .cmp(&other.reflector)
            .then_with(|| self.module.as_char().cmp(&other.module.as_char()))
    }
}
impl PartialOrd for TargetKey {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

pub(crate) const fn protocol_name(protocol: ProtocolKind) -> &'static str {
    match protocol {
        ProtocolKind::DPlus => "dplus",
        ProtocolKind::DExtra => "dextra",
        ProtocolKind::Dcs => "dcs",
        _ => "unsupported",
    }
}

/// One expanded record target: a single `(reflector, module)` pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// Reflector callsign as written in config, uppercased (e.g. `"REF030"`).
    pub(crate) reflector: ReflectorName,
    /// Typed reflector callsign (used in session setup; DCS embeds it
    /// in wire packets and validates it).
    pub(crate) reflector_callsign: RoutingCallsign,
    /// Protocol to connect with.
    pub(crate) protocol: ProtocolKind,
    /// Host name or IP to connect to (resolved at connect time).
    pub(crate) host: HostAddress,
    /// UDP port.
    pub(crate) port: NonZeroU16,
    /// Reflector module to link to.
    pub(crate) module: Module,
}

impl Target {
    /// Construct an endpoint; DCS additionally requires a `DCS` routing prefix.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::Callsign`] for an invalid DCS identity or
    /// [`ConfigError::Protocol`] for an unsupported future protocol.
    pub fn new(
        reflector: ReflectorName,
        protocol: ProtocolKind,
        host: HostAddress,
        port: NonZeroU16,
        module: Module,
    ) -> Result<Self, ConfigError> {
        let reflector_callsign = reflector.routing_callsign();
        match protocol {
            ProtocolKind::Dcs => {
                let _identity =
                    DcsReflector::try_from(reflector_callsign).map_err(ConfigError::Callsign)?;
            }
            ProtocolKind::DPlus | ProtocolKind::DExtra => {}
            _ => return Err(ConfigError::Protocol),
        }
        Ok(Self {
            reflector,
            reflector_callsign,
            protocol,
            host,
            port,
            module,
        })
    }
    /// Reflector/module directory identity.
    #[must_use]
    pub fn key(&self) -> TargetKey {
        TargetKey::new(self.reflector.clone(), self.module)
    }
    /// Normalized reflector name.
    #[must_use]
    pub const fn reflector(&self) -> &ReflectorName {
        &self.reflector
    }
    /// Selected transport protocol.
    #[must_use]
    pub const fn protocol(&self) -> ProtocolKind {
        self.protocol
    }
    /// DNS name or IP literal resolved at connection time.
    #[must_use]
    pub const fn host(&self) -> &HostAddress {
        &self.host
    }
    /// Nonzero UDP port.
    #[must_use]
    pub const fn port(&self) -> NonZeroU16 {
        self.port
    }
    /// Remote reflector module.
    #[must_use]
    pub const fn module(&self) -> Module {
        self.module
    }
}

/// Fully validated configuration.
#[derive(Debug, Clone)]
pub struct Config {
    /// Operator callsign used for reflector login and `DPlus` auth.
    pub(crate) callsign: RoutingCallsign,
    /// Base directory recordings are written under.
    pub(crate) recordings_dir: PathBuf,
    /// Whether to decode and write a WAV alongside the raw AMBE.
    pub(crate) write_wav: bool,
    /// Operator-selected maximum retained prefix for one transmission.
    pub(crate) capture_limit: CaptureDurationLimit,
    /// Operator-selected maximum number of simultaneous stream states.
    pub(crate) concurrent_capture_limit: ConcurrentCaptureLimit,
    /// Local module letter presented in rpt1 (A-E).
    pub(crate) local_module: Module,
    /// Expanded record targets, one per `(reflector, module)`.
    pub(crate) targets: Vec<Target>,
}

/// Configuration loading and validation failures.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// The file could not be read.
    #[error("read {path}: {source}")]
    Io {
        /// Path that failed to read.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// The file is not valid TOML for the stargazer schema.
    #[error("parse: {0}")]
    Parse(#[from] toml::de::Error),
    /// A routing callsign is invalid for the selected protocol.
    #[error("routing callsign: {0}")]
    Callsign(#[source] dstar_gateway_core::TypeError),
    /// Reflector text is not a safe seven-byte routing/path component.
    #[error("invalid reflector path component")]
    ReflectorName,
    /// A target key is not a reflector name followed by one module.
    #[error("target must be <reflector>-<module>")]
    TargetKey,
    /// A protocol is not one of dplus, dextra and dcs.
    #[error("protocol must be dplus, dextra or dcs")]
    Protocol,
    /// A module is not exactly one ASCII letter.
    #[error("module must be one ASCII letter A-Z")]
    Module,
    /// The local module exceeds the application's A-E policy.
    #[error("local_module must be A-E under this application's configuration policy")]
    LocalModule,
    /// A UDP endpoint has port zero.
    #[error("UDP port must be nonzero")]
    Port,
    /// A DNS hostname or IP literal is invalid.
    #[error("host: {0}")]
    Host(#[source] HostAddressError),
    /// A record has no selected modules.
    #[error("modules list is empty")]
    NoModules,
    /// The same reflector/module appears more than once.
    #[error("duplicate target {0}")]
    Duplicate(TargetKey),
    /// No recording target was supplied.
    #[error("no [[record]] targets configured")]
    NoTargets,
    /// The duration cannot be represented by the capture engine.
    #[error("max_capture_seconds: {0}")]
    CaptureLimit(#[source] crate::capture::CaptureLimitError),
    /// The concurrent stream bound cannot be represented.
    #[error("max_concurrent_captures: {0}")]
    ConcurrentLimit(#[source] crate::capture::ConcurrentCaptureLimitError),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    callsign: String,
    #[serde(default = "default_recordings_dir")]
    recordings_dir: PathBuf,
    #[serde(default = "default_true")]
    write_wav: bool,
    max_capture_seconds: u64,
    max_concurrent_captures: u64,
    #[serde(default = "default_local_module")]
    local_module: String,
    #[serde(default, rename = "record")]
    records: Vec<RawRecord>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRecord {
    reflector: String,
    protocol: String,
    host: String,
    port: Option<u16>,
    modules: Vec<String>,
}

fn default_recordings_dir() -> PathBuf {
    PathBuf::from("recordings")
}

const fn default_true() -> bool {
    true
}

fn default_local_module() -> String {
    "D".to_string()
}

impl Config {
    /// Parse a complete checked configuration from TOML text.
    ///
    /// # Errors
    ///
    /// Returns the corresponding [`ConfigError`] for malformed TOML, routing or
    /// endpoint syntax, invalid capture limits, empty targets or duplicate identities.
    pub fn from_toml(text: &str) -> Result<Self, ConfigError> {
        let raw: RawConfig = toml::from_str(text)?;

        let callsign = RoutingCallsign::new(&raw.callsign).map_err(ConfigError::Callsign)?;

        let local_module = parse_module(&raw.local_module)?;
        if !matches!(local_module.as_char(), 'A'..='E') {
            return Err(ConfigError::LocalModule);
        }

        let capture_limit = CaptureDurationLimit::try_from_seconds(raw.max_capture_seconds)
            .map_err(ConfigError::CaptureLimit)?;
        let concurrent_capture_limit =
            ConcurrentCaptureLimit::try_from_count(raw.max_concurrent_captures)
                .map_err(ConfigError::ConcurrentLimit)?;

        let mut targets = Vec::new();
        let mut seen = HashSet::new();
        for rec in &raw.records {
            let reflector = ReflectorName::new(rec.reflector.trim())?;
            let protocol = match rec.protocol.as_str() {
                "dplus" => ProtocolKind::DPlus,
                "dextra" => ProtocolKind::DExtra,
                "dcs" => ProtocolKind::Dcs,
                _ => return Err(ConfigError::Protocol),
            };
            let host = HostAddress::new(&rec.host).map_err(ConfigError::Host)?;
            let port = NonZeroU16::new(rec.port.unwrap_or_else(|| protocol.default_port()))
                .ok_or(ConfigError::Port)?;
            if rec.modules.is_empty() {
                return Err(ConfigError::NoModules);
            }
            for m in &rec.modules {
                let module = parse_module(m)?;
                let key = TargetKey::new(reflector.clone(), module);
                if !seen.insert(key.clone()) {
                    return Err(ConfigError::Duplicate(key));
                }
                targets.push(Target::new(
                    reflector.clone(),
                    protocol,
                    host.clone(),
                    port,
                    module,
                )?);
            }
        }
        if targets.is_empty() {
            return Err(ConfigError::NoTargets);
        }

        Ok(Self {
            callsign,
            recordings_dir: raw.recordings_dir,
            write_wav: raw.write_wav,
            capture_limit,
            concurrent_capture_limit,
            local_module,
            targets,
        })
    }

    /// Load and validate the configuration file at `path`.
    ///
    /// # Errors
    ///
    /// [`ConfigError::Io`] if the file cannot be read, otherwise as
    /// [`Config::from_toml`].
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        Self::from_toml(&text)
    }

    /// Checked login routing identity.
    #[must_use]
    pub const fn callsign(&self) -> RoutingCallsign {
        self.callsign
    }
    /// Root of the recording archive.
    #[must_use]
    pub fn recordings_dir(&self) -> &Path {
        &self.recordings_dir
    }
    /// Whether to publish decoded WAV files.
    #[must_use]
    pub const fn write_wav(&self) -> bool {
        self.write_wav
    }
    /// Maximum retained codec duration for one stream.
    #[must_use]
    pub const fn capture_limit(&self) -> CaptureDurationLimit {
        self.capture_limit
    }
    /// Maximum simultaneous retained or suppressed stream states.
    #[must_use]
    pub const fn concurrent_capture_limit(&self) -> ConcurrentCaptureLimit {
        self.concurrent_capture_limit
    }
    /// Local A-E module used when linking.
    #[must_use]
    pub const fn local_module(&self) -> Module {
        self.local_module
    }
    /// Complete expanded immutable target list.
    #[must_use]
    pub fn targets(&self) -> &[Target] {
        &self.targets
    }
}

impl FromStr for Config {
    type Err = ConfigError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::from_toml(value)
    }
}

fn parse_module(s: &str) -> Result<Module, ConfigError> {
    let mut chars = s.trim().chars();
    let (Some(c), None) = (chars.next(), chars.next()) else {
        return Err(ConfigError::Module);
    };
    Module::try_from_char(c.to_ascii_uppercase()).map_err(|_| ConfigError::Module)
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    const GOOD: &str = r#"
callsign = "W1AW"
max_capture_seconds = 3600
max_concurrent_captures = 4

[[record]]
reflector = "ref030"
protocol = "dplus"
host = "ref030.dstargateway.org"
modules = ["C"]

[[record]]
reflector = "XLX039"
protocol = "dextra"
host = "xlx039.example.org"
port = 30201
modules = ["A", "B"]
"#;

    #[test]
    fn parses_and_expands_targets() -> TestResult {
        let cfg = Config::from_toml(GOOD)?;
        assert_eq!(cfg.targets.len(), 3);
        assert_eq!(cfg.recordings_dir, PathBuf::from("recordings"));
        assert!(cfg.write_wav);
        assert_eq!(cfg.capture_limit.as_seconds(), 3600);
        assert_eq!(cfg.capture_limit.as_frames(), 180_000);
        assert_eq!(cfg.concurrent_capture_limit.get(), 4);
        assert_eq!(cfg.local_module, Module::D);
        let t0 = cfg.targets.first().ok_or("no target 0")?;
        assert_eq!(t0.reflector.as_str(), "REF030", "reflector is uppercased");
        assert_eq!(t0.port.get(), 20001, "dplus default port");
        assert_eq!(t0.module, Module::C);
        let t2 = cfg.targets.get(2).ok_or("no target 2")?;
        assert_eq!(t2.port.get(), 30201, "explicit port wins");
        assert_eq!(t2.module, Module::B);
        Ok(())
    }

    #[test]
    fn rejects_unknown_protocol() {
        let bad = GOOD.replace("dextra", "dmr");
        let result = Config::from_toml(&bad);
        assert!(
            matches!(result, Err(ConfigError::Protocol)),
            "got {result:?}"
        );
    }

    #[test]
    fn rejects_bad_module() {
        let bad = GOOD.replace("\"C\"", "\"CC\"");
        let result = Config::from_toml(&bad);
        assert!(matches!(result, Err(ConfigError::Module)), "got {result:?}");
    }

    #[test]
    fn rejects_local_module_outside_a_to_e() {
        let bad = format!("local_module = \"G\"\n{GOOD}");
        let result = Config::from_toml(&bad);
        assert!(
            matches!(result, Err(ConfigError::LocalModule)),
            "got {result:?}"
        );
    }

    #[test]
    fn rejects_duplicate_target() {
        let bad = GOOD.replace("[\"A\", \"B\"]", "[\"A\", \"A\"]");
        let result = Config::from_toml(&bad);
        assert!(
            matches!(result, Err(ConfigError::Duplicate(_))),
            "got {result:?}"
        );
    }

    #[test]
    fn rejects_empty_config() {
        let result = Config::from_toml(
            "callsign = \"W1AW\"\nmax_capture_seconds = 60\nmax_concurrent_captures = 1\n",
        );
        assert!(
            matches!(result, Err(ConfigError::NoTargets)),
            "got {result:?}"
        );
    }

    #[test]
    fn target_path_and_endpoint_boundaries_are_checked() -> TestResult {
        for value in [
            "", "../..", "a/b", "a\\b", "a:b", "CON", "NUL.txt", "COM1", "LPT9", "name.", "name ",
            "a?b", "abcdefgh",
        ] {
            assert!(ReflectorName::new(value).is_err(), "accepted {value:?}");
        }
        assert_eq!(ReflectorName::new("ref030")?.as_str(), "REF030");
        let target = "REF030-C".parse::<TargetKey>()?;
        assert_eq!(target.to_string(), "REF030-C");
        assert!("../..-C".parse::<TargetKey>().is_err());
        assert!("REF030-C/D".parse::<TargetKey>().is_err());
        assert!(Config::from_toml(&GOOD.replace("port = 30201", "port = 0")).is_err());
        assert!(
            Config::from_toml(&GOOD.replace("xlx039.example.org", "https://example.org")).is_err()
        );
        assert!(Config::from_toml(&GOOD.replace("dextra", "dcs")).is_err());
        assert!(Config::from_toml(&GOOD.replace("W1AW", "12345678")).is_err());
        Ok(())
    }

    #[test]
    fn capture_limit_is_required_and_positive() {
        let missing = GOOD.replace("max_capture_seconds = 3600\n", "");
        assert!(
            matches!(Config::from_toml(&missing), Err(ConfigError::Parse(_))),
            "a missing retention policy must fail closed"
        );

        let zero = GOOD.replace("max_capture_seconds = 3600", "max_capture_seconds = 0");
        let result = Config::from_toml(&zero);
        assert!(
            matches!(
                result,
                Err(ConfigError::CaptureLimit(
                    crate::capture::CaptureLimitError::Zero
                ))
            ),
            "got {result:?}"
        );

        let missing_concurrent = GOOD.replace("max_concurrent_captures = 4\n", "");
        assert!(
            matches!(
                Config::from_toml(&missing_concurrent),
                Err(ConfigError::Parse(_))
            ),
            "a missing aggregate retention policy must fail closed"
        );

        let zero_concurrent =
            GOOD.replace("max_concurrent_captures = 4", "max_concurrent_captures = 0");
        let result = Config::from_toml(&zero_concurrent);
        assert!(
            matches!(
                result,
                Err(ConfigError::ConcurrentLimit(
                    crate::capture::ConcurrentCaptureLimitError::Zero
                ))
            ),
            "got {result:?}"
        );
    }
}
