// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later

//! TOML configuration loading and validation.

use std::collections::HashSet;
use std::fmt;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

use crate::capture::CaptureRecordLimit;

/// Largest destination ID representable by the 24-bit DMR protocol.
pub const MAX_DMR_ID: u32 = 0x00FF_FFFF;
/// Smallest valid seven-digit subscriber DMR ID for an Open Terminal.
pub const MIN_TERMINAL_DMR_ID: u32 = 1_000_000;
/// Largest valid seven-digit subscriber DMR ID for an Open Terminal.
pub const MAX_TERMINAL_DMR_ID: u32 = 9_999_999;

/// Fully validated Pulsar configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Base directory under which recordings are stored.
    pub recordings_dir: PathBuf,
    /// Maximum repeatable event records retained for one call.
    pub max_capture_records_per_call: CaptureRecordLimit,
    /// Seconds without call traffic before an open call is finalized.
    pub call_hang_secs: u64,
    /// Seconds without a valid server packet before a session times out.
    pub server_timeout_secs: u64,
    /// `BrandMeister` masters to record from.
    pub masters: Vec<MasterConfig>,
}

impl Config {
    /// Call inactivity interval.
    #[must_use]
    pub const fn call_hang(&self) -> Duration {
        Duration::from_secs(self.call_hang_secs)
    }

    /// Open Terminal session timeout interval.
    #[must_use]
    pub const fn server_timeout(&self) -> Duration {
        Duration::from_secs(self.server_timeout_secs)
    }
}

/// One receive-only `BrandMeister` Open Terminal connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MasterConfig {
    /// Unique operator-facing name, also used in output paths.
    pub name: String,
    /// Master host name or IP address.
    pub host: String,
    /// Master UDP port.
    pub port: u16,
    /// Seven-digit subscriber DMR ID used by the software terminal.
    pub dmr_id: u32,
    /// Environment variable containing the user's hotspot-security password.
    pub password_env: String,
    /// Local UDP address to bind.
    pub bind: SocketAddr,
    /// Group-call destinations to subscribe to.
    pub talkgroups: Vec<u32>,
    /// Private-call destinations to subscribe to.
    pub private_ids: Vec<u32>,
}

impl MasterConfig {
    /// Read this master's password from its configured environment variable.
    ///
    /// The returned value has a redacted [`Debug`] implementation. Call
    /// [`SecretString::expose_secret`] only while constructing an
    /// authentication digest, and do not log or serialize its result.
    ///
    /// # Errors
    ///
    /// Returns [`PasswordError`] when the variable is absent, is not valid
    /// Unicode, or contains an empty password.
    pub fn load_password(&self) -> Result<SecretString, PasswordError> {
        match std::env::var(&self.password_env) {
            Ok(value) if value.is_empty() => Err(PasswordError::Empty {
                variable: self.password_env.clone(),
            }),
            Ok(value) => Ok(SecretString(value)),
            Err(std::env::VarError::NotPresent) => Err(PasswordError::Missing {
                variable: self.password_env.clone(),
            }),
            Err(std::env::VarError::NotUnicode(_)) => Err(PasswordError::NotUnicode {
                variable: self.password_env.clone(),
            }),
        }
    }
}

/// Hotspot-security password whose debug representation never reveals its value.
#[derive(Clone, PartialEq, Eq)]
pub struct SecretString(String);

impl SecretString {
    /// Borrow the secret for authentication.
    ///
    /// Callers must not log, format, or serialize the returned string.
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

/// Password environment lookup failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PasswordError {
    /// The named environment variable does not exist.
    #[error("password environment variable {variable:?} is not set")]
    Missing {
        /// Environment variable name.
        variable: String,
    },
    /// The named environment variable is not valid Unicode.
    #[error("password environment variable {variable:?} is not valid Unicode")]
    NotUnicode {
        /// Environment variable name.
        variable: String,
    },
    /// The named environment variable exists but is empty.
    #[error("password environment variable {variable:?} is empty")]
    Empty {
        /// Environment variable name.
        variable: String,
    },
}

/// Configuration loading and validation failure.
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
    /// The file is not valid TOML for the Pulsar schema.
    #[error("parse: {0}")]
    Parse(#[from] toml::de::Error),
    /// A parsed setting violates a semantic constraint.
    #[error("invalid config: {0}")]
    Invalid(String),
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

/// Parse and validate Pulsar configuration from TOML.
///
/// # Errors
///
/// Returns [`ConfigError::Parse`] for malformed TOML and
/// [`ConfigError::Invalid`] for invalid values or relationships.
pub fn parse(text: &str) -> Result<Config, ConfigError> {
    let raw: RawConfig = toml::from_str(text)?;
    let max_capture_records_per_call =
        CaptureRecordLimit::try_from_count(raw.max_capture_records_per_call).map_err(|error| {
            ConfigError::Invalid(format!("max_capture_records_per_call: {error}"))
        })?;
    if raw.recordings_dir.as_os_str().is_empty() {
        return Err(ConfigError::Invalid(
            "recordings_dir must not be empty".to_string(),
        ));
    }
    if raw.call_hang_secs == 0 {
        return Err(ConfigError::Invalid(
            "call_hang_secs must be greater than zero".to_string(),
        ));
    }
    if raw.server_timeout_secs <= 5 {
        return Err(ConfigError::Invalid(
            "server_timeout_secs must be greater than the 5-second keepalive interval".to_string(),
        ));
    }
    if raw.masters.is_empty() {
        return Err(ConfigError::Invalid(
            "no [[master]] connections configured".to_string(),
        ));
    }

    let mut seen_names = HashSet::new();
    let mut masters = Vec::with_capacity(raw.masters.len());
    for raw_master in raw.masters {
        let name = nonempty("master name", &raw_master.name)?;
        let folded_name = name.to_ascii_lowercase();
        if !seen_names.insert(folded_name) {
            return Err(ConfigError::Invalid(format!(
                "duplicate master name {name:?}"
            )));
        }
        let host = nonempty(&format!("master {name:?} host"), &raw_master.host)?;
        if raw_master.port == 0 {
            return Err(ConfigError::Invalid(format!(
                "master {name:?} port must be greater than zero"
            )));
        }
        if !(MIN_TERMINAL_DMR_ID..=MAX_TERMINAL_DMR_ID).contains(&raw_master.dmr_id) {
            return Err(ConfigError::Invalid(format!(
                "master {name:?} dmr_id {} must be exactly seven digits ({}..={})",
                raw_master.dmr_id, MIN_TERMINAL_DMR_ID, MAX_TERMINAL_DMR_ID
            )));
        }
        let password_env = nonempty(
            &format!("master {name:?} password_env"),
            &raw_master.password_env,
        )?;
        validate_ids(&name, "talkgroups", &raw_master.talkgroups, true)?;
        validate_ids(&name, "private_ids", &raw_master.private_ids, false)?;

        masters.push(MasterConfig {
            name,
            host,
            port: raw_master.port,
            dmr_id: raw_master.dmr_id,
            password_env,
            bind: raw_master.bind,
            talkgroups: raw_master.talkgroups,
            private_ids: raw_master.private_ids,
        });
    }

    Ok(Config {
        recordings_dir: raw.recordings_dir,
        max_capture_records_per_call,
        call_hang_secs: raw.call_hang_secs,
        server_timeout_secs: raw.server_timeout_secs,
        masters,
    })
}

/// Load and validate the TOML configuration at `path`.
///
/// # Errors
///
/// Returns [`ConfigError::Io`] if the file cannot be read, otherwise the
/// errors documented by [`parse`].
pub fn load(path: &Path) -> Result<Config, ConfigError> {
    let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    parse(&text)
}

fn nonempty(label: &str, value: &str) -> Result<String, ConfigError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        Err(ConfigError::Invalid(format!("{label} must not be empty")))
    } else {
        Ok(trimmed.to_string())
    }
}

fn validate_ids(master: &str, field: &str, ids: &[u32], required: bool) -> Result<(), ConfigError> {
    if required && ids.is_empty() {
        return Err(ConfigError::Invalid(format!(
            "master {master:?} {field} must not be empty"
        )));
    }
    let mut seen = HashSet::new();
    for id in ids {
        if !(1..=MAX_DMR_ID).contains(id) {
            return Err(ConfigError::Invalid(format!(
                "master {master:?} {field} ID {id} is outside 1..={MAX_DMR_ID}"
            )));
        }
        if !seen.insert(*id) {
            return Err(ConfigError::Invalid(format!(
                "master {master:?} {field} contains duplicate ID {id}"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    const GOOD: &str = r#"
max_capture_records_per_call = 20000

[[master]]
name = "BM-US"
host = "3103.master.brandmeister.network"
dmr_id = 3101234
password_env = "PULSAR_BM_US_PASSWORD"
talkgroups = [91, 3100]
private_ids = [1234567]
"#;

    #[test]
    fn parses_defaults_and_subscriptions() -> TestResult {
        let config = parse(GOOD)?;
        assert_eq!(config.recordings_dir, PathBuf::from("dmr-recordings"));
        assert_eq!(config.max_capture_records_per_call.get(), 20_000);
        assert_eq!(config.call_hang(), Duration::from_secs(3));
        assert_eq!(config.server_timeout(), Duration::from_secs(15));
        let master = config.masters.first().ok_or("missing master")?;
        assert_eq!(master.name, "BM-US");
        assert_eq!(master.port, 54_006);
        assert_eq!(master.dmr_id, 3_101_234);
        assert_eq!(master.bind, default_bind());
        assert_eq!(master.talkgroups, [91, 3100]);
        assert_eq!(master.private_ids, [1_234_567]);
        Ok(())
    }

    #[test]
    fn explicit_globals_and_connection_values_win() -> TestResult {
        let text = format!(
            "recordings_dir = \"archive\"\ncall_hang_secs = 5\n\
             server_timeout_secs = 22\n{GOOD}"
        )
        .replace(
            "talkgroups = [91, 3100]",
            "port = 54007\nbind = \"127.0.0.1:12345\"\ntalkgroups = [91]",
        );
        let config = parse(&text)?;
        let master = config.masters.first().ok_or("missing master")?;
        assert_eq!(config.recordings_dir, PathBuf::from("archive"));
        assert_eq!(config.call_hang_secs, 5);
        assert_eq!(config.server_timeout_secs, 22);
        assert_eq!(master.port, 54_007);
        assert_eq!(master.bind, "127.0.0.1:12345".parse()?);
        Ok(())
    }

    #[test]
    fn rejects_empty_and_duplicate_masters() -> TestResult {
        let empty = parse("max_capture_records_per_call = 20000");
        assert!(
            matches!(empty, Err(ConfigError::Invalid(ref message)) if message.contains("[[master]]")),
            "got {empty:?}"
        );

        let master_start = GOOD.find("[[master]]").ok_or("missing master table")?;
        let duplicate = parse(&format!("{GOOD}\n{}", &GOOD[master_start..]));
        assert!(
            matches!(duplicate, Err(ConfigError::Invalid(ref message)) if message.contains("duplicate master")),
            "got {duplicate:?}"
        );
        Ok(())
    }

    #[test]
    fn requires_a_nonzero_capture_record_limit() -> TestResult {
        let missing = parse(&GOOD.replace("max_capture_records_per_call = 20000\n\n", ""));
        assert!(
            matches!(missing, Err(ConfigError::Parse(ref error)) if error.to_string().contains("max_capture_records_per_call")),
            "got {missing:?}"
        );

        let zero = parse(&GOOD.replace(
            "max_capture_records_per_call = 20000",
            "max_capture_records_per_call = 0",
        ));
        assert!(
            matches!(zero, Err(ConfigError::Invalid(ref message)) if message == "max_capture_records_per_call: capture record count must be greater than zero"),
            "got {zero:?}"
        );

        let one = parse(&GOOD.replace(
            "max_capture_records_per_call = 20000",
            "max_capture_records_per_call = 1",
        ))?;
        assert_eq!(one.max_capture_records_per_call.get(), 1);
        Ok(())
    }

    #[test]
    fn rejects_zero_and_oversized_destination_ids() {
        for id in [0, MAX_DMR_ID + 1] {
            let text = GOOD.replace("[91, 3100]", &format!("[{id}]"));
            let result = parse(&text);
            assert!(
                matches!(result, Err(ConfigError::Invalid(ref message)) if message.contains("outside")),
                "ID {id} produced {result:?}"
            );
        }
    }

    #[test]
    fn rejects_empty_or_duplicate_talkgroups() {
        let empty = parse(&GOOD.replace("[91, 3100]", "[]"));
        assert!(
            matches!(empty, Err(ConfigError::Invalid(ref message)) if message.contains("must not be empty")),
            "got {empty:?}"
        );
        let duplicate = parse(&GOOD.replace("[91, 3100]", "[91, 91]"));
        assert!(
            matches!(duplicate, Err(ConfigError::Invalid(ref message)) if message.contains("duplicate ID")),
            "got {duplicate:?}"
        );
    }

    #[test]
    fn rejects_zero_timeouts_and_port() {
        let zero_hang = parse(&format!("call_hang_secs = 0\n{GOOD}"));
        assert!(
            matches!(zero_hang, Err(ConfigError::Invalid(ref message)) if message.contains("call_hang_secs")),
            "got {zero_hang:?}"
        );
        for timeout in [0, 5] {
            let result = parse(&format!("server_timeout_secs = {timeout}\n{GOOD}"));
            assert!(
                matches!(result, Err(ConfigError::Invalid(ref message)) if message.contains("server_timeout_secs")),
                "got {result:?}"
            );
        }
        let zero_port = parse(&GOOD.replace("password_env =", "port = 0\npassword_env ="));
        assert!(
            matches!(zero_port, Err(ConfigError::Invalid(ref message)) if message.contains("port")),
            "got {zero_port:?}"
        );
    }

    #[test]
    fn accepts_only_seven_digit_terminal_dmr_ids() -> TestResult {
        for id in [MIN_TERMINAL_DMR_ID, MAX_TERMINAL_DMR_ID] {
            let config = parse(&GOOD.replace("dmr_id = 3101234", &format!("dmr_id = {id}")))?;
            assert_eq!(config.masters.first().ok_or("missing master")?.dmr_id, id);
        }

        for id in [0, MIN_TERMINAL_DMR_ID - 1, MAX_TERMINAL_DMR_ID + 1] {
            let result = parse(&GOOD.replace("dmr_id = 3101234", &format!("dmr_id = {id}")));
            assert!(
                matches!(result, Err(ConfigError::Invalid(ref message)) if message.contains("dmr_id") && message.contains("seven digits")),
                "DMR ID {id} produced {result:?}"
            );
        }
        Ok(())
    }

    #[test]
    fn rejects_legacy_application_id_field() {
        let result = parse(&GOOD.replace("dmr_id = 3101234", "app_id = 1234"));
        assert!(
            matches!(result, Err(ConfigError::Parse(ref error)) if error.to_string().contains("app_id")),
            "got {result:?}"
        );
    }

    #[test]
    fn secret_debug_is_redacted() {
        let secret = SecretString("do-not-print-me".to_string());
        let debug = format!("{secret:?}");
        assert!(!debug.contains("do-not-print-me"), "{debug}");
        assert!(debug.contains("REDACTED"), "{debug}");
        assert_eq!(secret.expose_secret(), "do-not-print-me");
    }
}
