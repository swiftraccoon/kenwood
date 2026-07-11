// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later

//! TOML configuration: global settings plus one `[[record]]` entry
//! per reflector, expanded into per-module [`Target`]s at load time.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use dstar_gateway_core::{Callsign, Module};
use serde::Deserialize;

/// Which D-STAR reflector protocol a target speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolChoice {
    /// `DPlus` (REF reflectors, TCP auth then UDP).
    Dplus,
    /// `DExtra` (XRF and XLX reflectors).
    Dextra,
    /// DCS.
    Dcs,
}

impl ProtocolChoice {
    /// Well-known UDP port for the protocol.
    #[must_use]
    pub const fn default_port(self) -> u16 {
        match self {
            Self::Dplus => 20001,
            Self::Dextra => 30001,
            Self::Dcs => 30051,
        }
    }

    /// Lowercase protocol name as used in config and metadata.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Dplus => "dplus",
            Self::Dextra => "dextra",
            Self::Dcs => "dcs",
        }
    }
}

/// One expanded record target: a single `(reflector, module)` pair.
#[derive(Debug, Clone)]
pub struct Target {
    /// Reflector callsign as written in config, uppercased (e.g. `"REF030"`).
    pub reflector: String,
    /// Typed reflector callsign (used in session setup; DCS embeds it
    /// in wire packets and validates it).
    pub reflector_callsign: Callsign,
    /// Protocol to connect with.
    pub protocol: ProtocolChoice,
    /// Host name or IP to connect to (resolved at connect time).
    pub host: String,
    /// UDP port.
    pub port: u16,
    /// Reflector module to link to.
    pub module: Module,
}

/// Fully validated configuration.
#[derive(Debug, Clone)]
pub struct Config {
    /// Operator callsign used for reflector login and `DPlus` auth.
    pub callsign: Callsign,
    /// Base directory recordings are written under.
    pub recordings_dir: PathBuf,
    /// Whether to decode and write a WAV alongside the raw AMBE.
    pub write_wav: bool,
    /// Local module letter presented in rpt1 (A-E).
    pub local_module: Module,
    /// Expanded record targets, one per `(reflector, module)`.
    pub targets: Vec<Target>,
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
    /// The file is not valid TOML for our schema.
    #[error("parse: {0}")]
    Parse(#[from] toml::de::Error),
    /// The file parsed but a value is invalid.
    #[error("invalid config: {0}")]
    Invalid(String),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    callsign: String,
    #[serde(default = "default_recordings_dir")]
    recordings_dir: PathBuf,
    #[serde(default = "default_true")]
    write_wav: bool,
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

/// Parse and validate a configuration from TOML text.
///
/// # Errors
///
/// Returns [`ConfigError::Parse`] on malformed TOML and
/// [`ConfigError::Invalid`] on schema violations (bad callsign or
/// module, unknown protocol, no targets, duplicate targets).
pub fn parse(text: &str) -> Result<Config, ConfigError> {
    let raw: RawConfig = toml::from_str(text)?;

    let callsign = Callsign::try_from_str(&raw.callsign)
        .map_err(|e| ConfigError::Invalid(format!("callsign {:?}: {e}", raw.callsign)))?;

    let local_module = parse_module(&raw.local_module)?;
    if !matches!(local_module.as_char(), 'A'..='E') {
        return Err(ConfigError::Invalid(format!(
            "local_module must be A-E (xlxd-derived reflectors silently drop others), got {}",
            local_module.as_char()
        )));
    }

    let mut targets = Vec::new();
    let mut seen: HashSet<(String, char)> = HashSet::new();
    for rec in &raw.records {
        let reflector = rec.reflector.trim().to_uppercase();
        let reflector_callsign = Callsign::try_from_str(&reflector)
            .map_err(|e| ConfigError::Invalid(format!("reflector {reflector:?}: {e}")))?;
        let protocol = match rec.protocol.as_str() {
            "dplus" => ProtocolChoice::Dplus,
            "dextra" => ProtocolChoice::Dextra,
            "dcs" => ProtocolChoice::Dcs,
            other => {
                return Err(ConfigError::Invalid(format!(
                    "protocol must be dplus|dextra|dcs, got {other:?}"
                )));
            }
        };
        if rec.modules.is_empty() {
            return Err(ConfigError::Invalid(format!(
                "reflector {reflector}: modules list is empty"
            )));
        }
        for m in &rec.modules {
            let module = parse_module(m)?;
            if !seen.insert((reflector.clone(), module.as_char())) {
                return Err(ConfigError::Invalid(format!(
                    "duplicate target {reflector} module {}",
                    module.as_char()
                )));
            }
            targets.push(Target {
                reflector: reflector.clone(),
                reflector_callsign,
                protocol,
                host: rec.host.clone(),
                port: rec.port.unwrap_or_else(|| protocol.default_port()),
                module,
            });
        }
    }
    if targets.is_empty() {
        return Err(ConfigError::Invalid(
            "no [[record]] targets configured".to_string(),
        ));
    }

    Ok(Config {
        callsign,
        recordings_dir: raw.recordings_dir,
        write_wav: raw.write_wav,
        local_module,
        targets,
    })
}

/// Load and validate the configuration file at `path`.
///
/// # Errors
///
/// [`ConfigError::Io`] if the file cannot be read, otherwise as
/// [`parse`].
pub fn load(path: &Path) -> Result<Config, ConfigError> {
    let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    parse(&text)
}

fn parse_module(s: &str) -> Result<Module, ConfigError> {
    let mut chars = s.trim().chars();
    let (Some(c), None) = (chars.next(), chars.next()) else {
        return Err(ConfigError::Invalid(format!(
            "module must be a single letter A-Z, got {s:?}"
        )));
    };
    Module::try_from_char(c.to_ascii_uppercase())
        .map_err(|e| ConfigError::Invalid(format!("module {s:?}: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    const GOOD: &str = r#"
callsign = "W1AW"

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
        let cfg = parse(GOOD)?;
        assert_eq!(cfg.targets.len(), 3);
        assert_eq!(cfg.recordings_dir, PathBuf::from("recordings"));
        assert!(cfg.write_wav);
        assert_eq!(cfg.local_module, Module::D);
        let t0 = cfg.targets.first().ok_or("no target 0")?;
        assert_eq!(t0.reflector, "REF030", "reflector is uppercased");
        assert_eq!(t0.port, 20001, "dplus default port");
        assert_eq!(t0.module, Module::C);
        let t2 = cfg.targets.get(2).ok_or("no target 2")?;
        assert_eq!(t2.port, 30201, "explicit port wins");
        assert_eq!(t2.module, Module::B);
        Ok(())
    }

    #[test]
    fn rejects_unknown_protocol() {
        let bad = GOOD.replace("dextra", "dmr");
        let result = parse(&bad);
        assert!(
            matches!(result, Err(ConfigError::Invalid(ref m)) if m.contains("protocol")),
            "got {result:?}"
        );
    }

    #[test]
    fn rejects_bad_module() {
        let bad = GOOD.replace("\"C\"", "\"CC\"");
        let result = parse(&bad);
        assert!(
            matches!(result, Err(ConfigError::Invalid(_))),
            "got {result:?}"
        );
    }

    #[test]
    fn rejects_local_module_outside_a_to_e() {
        let bad = format!("local_module = \"G\"\n{GOOD}");
        let result = parse(&bad);
        assert!(
            matches!(result, Err(ConfigError::Invalid(ref m)) if m.contains("local_module")),
            "got {result:?}"
        );
    }

    #[test]
    fn rejects_duplicate_target() {
        let bad = GOOD.replace("[\"A\", \"B\"]", "[\"A\", \"A\"]");
        let result = parse(&bad);
        assert!(
            matches!(result, Err(ConfigError::Invalid(ref m)) if m.contains("duplicate")),
            "got {result:?}"
        );
    }

    #[test]
    fn rejects_empty_config() {
        let result = parse("callsign = \"W1AW\"\n");
        assert!(
            matches!(result, Err(ConfigError::Invalid(ref m)) if m.contains("no [[record]]")),
            "got {result:?}"
        );
    }
}
