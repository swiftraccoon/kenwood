// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

//! Persistent GUI form state.
//!
//! Saves the user-editable fields (callsign, reflector host/port,
//! protocol, modules) to a `key = value` text file in the platform's
//! config directory and reloads them on the next launch. Hand-rolled
//! parser keeps the dep footprint at zero.
//!
//! The file lives at:
//! - macOS:   `~/Library/Application Support/sextant/settings.toml`
//! - Linux:   `~/.config/sextant/settings.toml` (or `$XDG_CONFIG_HOME/sextant/settings.toml`)
//! - Windows: `%APPDATA%\sextant\settings.toml`
//!
//! `.toml` extension is for editor syntax highlighting; the actual
//! parser only handles `key = "value"` lines so it's a strict subset
//! of TOML.
//!
//! Save/load failures are logged at `warn` and ignored — the user
//! still gets defaults if the file is missing or malformed, and a
//! corrupt file is overwritten on the next save.

use std::path::PathBuf;

use tracing::{debug, warn};

/// User-editable form state that survives across app launches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Settings {
    /// Operator callsign.
    pub(crate) callsign: String,
    /// Reflector host (DNS name or IP).
    pub(crate) reflector_host: String,
    /// Reflector UDP port (string form to round-trip empty / partial input).
    pub(crate) reflector_port: String,
    /// Reflector callsign (e.g. `REF030`).
    pub(crate) reflector_callsign: String,
    /// Protocol family — stored as the `Debug` repr (`DExtra`, `DPlus`, `Dcs`).
    pub(crate) protocol: String,
    /// Local module letter.
    pub(crate) local_module: char,
    /// Reflector module letter.
    pub(crate) reflector_module: char,
    /// Auto-reconnect after a reflector-driven disconnect.
    pub(crate) reconnect_on_drop: bool,
    /// Persist the heard-list across launches.
    pub(crate) persist_heard_list: bool,
    /// Audio input device name (empty = host default).
    pub(crate) input_device: String,
    /// Audio output device name (empty = host default).
    pub(crate) output_device: String,
}

impl Default for Settings {
    /// Defaults match the historical hardcoded values in `App::new`
    /// so first-launch UX is unchanged.
    fn default() -> Self {
        Self {
            callsign: "W1TEST".into(),
            reflector_host: "127.0.0.1".into(),
            reflector_port: "30001".into(),
            reflector_callsign: "POLARIS".into(),
            protocol: "DExtra".into(),
            local_module: 'C',
            reflector_module: 'C',
            reconnect_on_drop: false,
            persist_heard_list: false,
            input_device: String::new(),
            output_device: String::new(),
        }
    }
}

impl Settings {
    /// Resolve the settings file path for the current platform.
    /// Returns `None` if no config dir is available (e.g. headless CI).
    fn path() -> Option<PathBuf> {
        let mut dir = dirs_next::config_dir()?;
        dir.push("sextant");
        Some(dir.join("settings.toml"))
    }

    /// Load from disk. Falls back to [`Self::default`] silently on
    /// any error (missing file, corrupt content, IO failure).
    pub(crate) fn load_or_default() -> Self {
        let Some(path) = Self::path() else {
            return Self::default();
        };
        let raw = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => {
                debug!(path = %path.display(), error = %e, "no settings file — using defaults");
                return Self::default();
            }
        };
        match parse(&raw) {
            Ok(s) => {
                debug!(path = %path.display(), "loaded settings");
                s
            }
            Err(e) => {
                warn!(path = %path.display(), error = %e, "settings file malformed — using defaults");
                Self::default()
            }
        }
    }

    /// Persist to disk. Logs and swallows errors — settings persistence
    /// must never block app shutdown or interaction.
    pub(crate) fn save(&self) {
        let Some(path) = Self::path() else {
            return;
        };
        if let Some(parent) = path.parent()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            warn!(path = %parent.display(), error = %e, "could not create settings dir");
            return;
        }
        let body = serialize(self);
        if let Err(e) = std::fs::write(&path, body) {
            warn!(path = %path.display(), error = %e, "could not write settings");
        } else {
            debug!(path = %path.display(), "saved settings");
        }
    }
}

/// Serialize to a `key = "value"` line per field. Quotes are bare
/// double-quotes; values containing `"` or backslash are escaped.
/// Callsigns / hosts / ports / module letters never need escaping in
/// practice, but the escape keeps adversarial inputs safe.
fn serialize(s: &Settings) -> String {
    let mut out = String::with_capacity(256);
    out.push_str("# sextant GUI settings — auto-saved on disconnect / app exit.\n");
    push_string(&mut out, "callsign", &s.callsign);
    push_string(&mut out, "reflector_host", &s.reflector_host);
    push_string(&mut out, "reflector_port", &s.reflector_port);
    push_string(&mut out, "reflector_callsign", &s.reflector_callsign);
    push_string(&mut out, "protocol", &s.protocol);
    push_string(&mut out, "local_module", &s.local_module.to_string());
    push_string(
        &mut out,
        "reflector_module",
        &s.reflector_module.to_string(),
    );
    push_bool(&mut out, "reconnect_on_drop", s.reconnect_on_drop);
    push_bool(&mut out, "persist_heard_list", s.persist_heard_list);
    push_string(&mut out, "input_device", &s.input_device);
    push_string(&mut out, "output_device", &s.output_device);
    out
}

fn push_string(out: &mut String, key: &str, value: &str) {
    out.push_str(key);
    out.push_str(" = \"");
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            other => out.push(other),
        }
    }
    out.push_str("\"\n");
}

/// Serialize a bare `key = true` / `key = false` line.
fn push_bool(out: &mut String, key: &str, value: bool) {
    out.push_str(key);
    out.push_str(if value { " = true\n" } else { " = false\n" });
}

/// Parse a settings file; missing keys fall back to defaults.
fn parse(raw: &str) -> Result<Settings, String> {
    let mut out = Settings::default();
    for (lineno, line) in raw.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let (key, value) = split_kv(trimmed)
            .ok_or_else(|| format!("line {}: not a `key = value` pair", lineno + 1))?;
        // Boolean keys carry a bare `true` / `false`, not a quoted
        // string — handle them before the quoted-string parse.
        if key == "reconnect_on_drop" || key == "persist_heard_list" {
            let flag = match value {
                "true" => true,
                "false" => false,
                other => {
                    return Err(format!(
                        "line {}: expected true/false, got {other:?}",
                        lineno + 1
                    ));
                }
            };
            if key == "reconnect_on_drop" {
                out.reconnect_on_drop = flag;
            } else {
                out.persist_heard_list = flag;
            }
            continue;
        }
        let value = parse_quoted(value)
            .ok_or_else(|| format!("line {}: value must be a double-quoted string", lineno + 1))?;
        match key {
            "callsign" => out.callsign = value,
            "reflector_host" => out.reflector_host = value,
            "reflector_port" => out.reflector_port = value,
            "reflector_callsign" => out.reflector_callsign = value,
            "protocol" => out.protocol = value,
            "local_module" => {
                if let Some(ch) = value.chars().next() {
                    out.local_module = ch;
                }
            }
            "reflector_module" => {
                if let Some(ch) = value.chars().next() {
                    out.reflector_module = ch;
                }
            }
            "input_device" => out.input_device = value,
            "output_device" => out.output_device = value,
            // Unknown keys are ignored — forward-compat for future
            // settings without breaking older sextant installs.
            _ => {}
        }
    }
    Ok(out)
}

fn split_kv(line: &str) -> Option<(&str, &str)> {
    let eq = line.find('=')?;
    let (key, rest) = line.split_at(eq);
    let value = rest.get(1..)?.trim_start();
    Some((key.trim_end(), value))
}

fn parse_quoted(value: &str) -> Option<String> {
    let inner = value.strip_prefix('"')?.strip_suffix('"')?;
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.next()? {
                '"' => out.push('"'),
                '\\' => out.push('\\'),
                // Unknown escape sequence — bail.
                _ => return None,
            }
        } else {
            out.push(ch);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::{Settings, parse, serialize};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn defaults_match_historical_hardcoded_values() {
        let s = Settings::default();
        assert_eq!(s.callsign, "W1TEST");
        assert_eq!(s.reflector_host, "127.0.0.1");
        assert_eq!(s.reflector_port, "30001");
        assert_eq!(s.reflector_callsign, "POLARIS");
        assert_eq!(s.protocol, "DExtra");
        assert_eq!(s.local_module, 'C');
        assert_eq!(s.reflector_module, 'C');
    }

    #[test]
    fn roundtrip_preserves_all_fields() -> TestResult {
        let original = Settings {
            callsign: "KQ4NIT".into(),
            reflector_host: "ref030.example.org".into(),
            reflector_port: "30001".into(),
            reflector_callsign: "REF030".into(),
            protocol: "DPlus".into(),
            local_module: 'A',
            reflector_module: 'B',
            reconnect_on_drop: true,
            persist_heard_list: true,
            input_device: "Built-in Microphone".into(),
            output_device: "External Headphones".into(),
        };
        let serialized = serialize(&original);
        let parsed = parse(&serialized).map_err(|e| format!("parse: {e}"))?;
        assert_eq!(parsed, original);
        Ok(())
    }

    #[test]
    fn malformed_line_is_rejected() {
        let raw = "callsign = \"W1AW\"\nbroken line without equals\n";
        let result = parse(raw);
        assert!(
            matches!(result, Err(ref e) if e.contains("line 2")),
            "parser must reject malformed lines and report line number, got: {result:?}"
        );
    }

    #[test]
    fn unknown_keys_are_ignored_for_forward_compat() -> TestResult {
        let raw = "callsign = \"W1AW\"\nfuture_key = \"value\"\n";
        let parsed = parse(raw).map_err(|e| format!("parse: {e}"))?;
        assert_eq!(parsed.callsign, "W1AW");
        Ok(())
    }

    #[test]
    fn comments_and_blank_lines_are_skipped() -> TestResult {
        let raw = "# comment\n\ncallsign = \"W1AW\"\n# another\n";
        let parsed = parse(raw).map_err(|e| format!("parse: {e}"))?;
        assert_eq!(parsed.callsign, "W1AW");
        Ok(())
    }

    #[test]
    fn embedded_quotes_roundtrip() -> TestResult {
        // Adversarial input — a callsign containing a quote (won't
        // happen in practice but the escape must still survive a round
        // trip).
        let original = Settings {
            callsign: "W\"AW".into(),
            ..Settings::default()
        };
        let serialized = serialize(&original);
        let parsed = parse(&serialized).map_err(|e| format!("parse: {e}"))?;
        assert_eq!(parsed.callsign, "W\"AW");
        Ok(())
    }
}
