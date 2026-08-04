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
//! Save/load failures are logged at `warn` and ignored; the user
//! still gets defaults if the file is missing or malformed, and a
//! corrupt file is overwritten on the next save.

use std::path::PathBuf;

use tracing::{debug, warn};

/// Maximum entries kept in the recent-connections list.
pub(crate) const RECENTS_CAP: usize = 8;

/// Marks settings whose persisted reflector addresses were created after
/// automatic plaintext-XLX directory ingestion was removed. Older settings
/// cannot distinguish a manual address from one copied out of that directory,
/// so their host-bearing fields need a one-time reset.
const REFLECTOR_ADDRESS_EPOCH: &str = "bundled-manual-dplus-v1";

/// Which clock timestamps are displayed in (heard list, event log).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum TimeMode {
    /// The operator's local timezone (falls back to UTC when the
    /// local offset couldn't be detected at startup).
    #[default]
    Local,
    /// UTC, the ham logging convention.
    Utc,
}

impl TimeMode {
    /// Stable settings-file token.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Utc => "utc",
        }
    }

    /// Parse the settings-file token; unknown values fall back to
    /// the default so a future token can't brick old builds.
    fn from_str(s: &str) -> Self {
        match s {
            "utc" => Self::Utc,
            _ => Self::Local,
        }
    }
}

/// Whether decoded RX audio plays raw or through the learned live
/// waveform enhancer ("Enhance RX audio" in the settings popup).
///
/// An enum rather than a bare bool so [`Settings`] / `App` stay under
/// the `struct_excessive_bools` ceiling, mirroring [`TimeMode`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum RxAudioMode {
    /// Raw decoder output (the default).
    #[default]
    Raw,
    /// Decoder output routed through the causal live enhancer.
    Enhanced,
}

impl RxAudioMode {
    /// Stable settings-file token.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Raw => "raw",
            Self::Enhanced => "enhanced",
        }
    }

    /// Parse the settings-file token; unknown values fall back to
    /// the default so a future token can't brick old builds.
    fn from_str(s: &str) -> Self {
        match s {
            "enhanced" => Self::Enhanced,
            _ => Self::Raw,
        }
    }

    /// True when the enhancer is selected.
    pub(crate) const fn is_enhanced(self) -> bool {
        matches!(self, Self::Enhanced)
    }
}

/// One remembered reflector connection (a favorite or a recent).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SavedHost {
    /// Reflector callsign (e.g. `REF030`).
    pub(crate) callsign: String,
    /// Host name or IP.
    pub(crate) host: String,
    /// UDP port (string form, matching the form fields it fills).
    pub(crate) port: String,
    /// Protocol family as its `Debug` repr (`DExtra`, `DPlus`, `Dcs`).
    pub(crate) protocol: String,
    /// Reflector module letter.
    pub(crate) module: char,
}

/// Insert (or promote) `entry` at the front of `recents`,
/// deduplicating and truncating to [`RECENTS_CAP`].
pub(crate) fn push_recent(recents: &mut Vec<SavedHost>, entry: SavedHost) {
    recents.retain(|e| e != &entry);
    recents.insert(0, entry);
    recents.truncate(RECENTS_CAP);
}

/// `callsign|host|port|protocol|module`; `|` never appears in
/// callsigns, hostnames, ports, or protocol names.
fn encode_saved(s: &SavedHost) -> String {
    format!(
        "{}|{}|{}|{}|{}",
        s.callsign, s.host, s.port, s.protocol, s.module
    )
}

/// Parse the [`encode_saved`] format; `None` on any shape mismatch.
fn decode_saved(v: &str) -> Option<SavedHost> {
    let mut parts = v.split('|');
    let callsign = parts.next()?.to_owned();
    let host = parts.next()?.to_owned();
    let port = parts.next()?.to_owned();
    let protocol = parts.next()?.to_owned();
    let module = parts.next()?.chars().next()?;
    if parts.next().is_some() {
        return None;
    }
    Some(SavedHost {
        callsign,
        host,
        port,
        protocol,
        module,
    })
}

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
    /// Protocol family, stored as the `Debug` repr (`DExtra`, `DPlus`, `Dcs`).
    pub(crate) protocol: String,
    /// Local module letter.
    pub(crate) local_module: char,
    /// Reflector module letter.
    pub(crate) reflector_module: char,
    /// Auto-reconnect after a reflector-driven disconnect.
    pub(crate) reconnect_on_drop: bool,
    /// Persist the heard-list across launches.
    pub(crate) persist_heard_list: bool,
    /// Timestamp display mode (heard list + event log).
    pub(crate) time_mode: TimeMode,
    /// RX playback mode (raw decoder output vs live enhancement).
    pub(crate) rx_audio: RxAudioMode,
    /// Slow-data text message to transmit (≤20 chars).
    pub(crate) tx_message: String,
    /// GPS beacon enabled.
    pub(crate) tx_beacon_enabled: bool,
    /// Beacon latitude (edit string, parsed at push time).
    pub(crate) tx_lat: String,
    /// Beacon longitude (edit string, parsed at push time).
    pub(crate) tx_lon: String,
    /// Beacon APRS symbol glyph.
    pub(crate) tx_symbol: String,
    /// Beacon free-text comment.
    pub(crate) tx_comment: String,
    /// Audio input device name (empty = host default).
    pub(crate) input_device: String,
    /// Audio output device name (empty = host default).
    pub(crate) output_device: String,
    /// Starred reflectors, pinned atop the directory picker.
    pub(crate) favorites: Vec<SavedHost>,
    /// Recent successful connections, most recent first.
    pub(crate) recents: Vec<SavedHost>,
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
            time_mode: TimeMode::default(),
            rx_audio: RxAudioMode::default(),
            tx_message: String::new(),
            tx_beacon_enabled: false,
            tx_lat: String::new(),
            tx_lon: String::new(),
            tx_symbol: "/".into(),
            tx_comment: String::new(),
            input_device: String::new(),
            output_device: String::new(),
            favorites: Vec::new(),
            recents: Vec::new(),
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
                debug!(path = %path.display(), error = %e, "no settings file, using defaults");
                return Self::default();
            }
        };
        match parse(&raw) {
            Ok(mut s) => {
                if migrate_legacy_reflector_addresses(&raw, &mut s) {
                    warn!(
                        path = %path.display(),
                        "discarded saved reflector addresses from before the trusted-source migration"
                    );
                }
                debug!(path = %path.display(), "loaded settings");
                s
            }
            Err(e) => {
                warn!(path = %path.display(), error = %e, "settings file malformed, using defaults");
                Self::default()
            }
        }
    }

    /// Persist to disk. Logs and swallows errors: settings persistence
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
    out.push_str("# sextant GUI settings, auto-saved on disconnect / app exit.\n");
    push_string(&mut out, "reflector_address_epoch", REFLECTOR_ADDRESS_EPOCH);
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
    push_string(&mut out, "time_mode", s.time_mode.as_str());
    push_string(&mut out, "rx_audio", s.rx_audio.as_str());
    push_string(&mut out, "tx_message", &s.tx_message);
    push_bool(&mut out, "tx_beacon_enabled", s.tx_beacon_enabled);
    push_string(&mut out, "tx_lat", &s.tx_lat);
    push_string(&mut out, "tx_lon", &s.tx_lon);
    push_string(&mut out, "tx_symbol", &s.tx_symbol);
    push_string(&mut out, "tx_comment", &s.tx_comment);
    push_string(&mut out, "input_device", &s.input_device);
    push_string(&mut out, "output_device", &s.output_device);
    for (i, f) in s.favorites.iter().enumerate() {
        push_string(&mut out, &format!("favorite.{i}"), &encode_saved(f));
    }
    for (i, r) in s.recents.iter().enumerate() {
        push_string(&mut out, &format!("recent.{i}"), &encode_saved(r));
    }
    out
}

/// Reset host-bearing state from settings written before reflector-address
/// provenance could be distinguished. This intentionally retains unrelated
/// operator, radio, audio, and UI settings. Returns whether a migration ran.
fn migrate_legacy_reflector_addresses(raw: &str, settings: &mut Settings) -> bool {
    let is_current = raw.lines().any(|line| {
        let Some((key, value)) = split_kv(line.trim()) else {
            return false;
        };
        key == "reflector_address_epoch"
            && parse_quoted(value).as_deref() == Some(REFLECTOR_ADDRESS_EPOCH)
    });
    if is_current {
        return false;
    }

    let defaults = Settings::default();
    settings.reflector_host = defaults.reflector_host;
    settings.reflector_port = defaults.reflector_port;
    settings.reflector_callsign = defaults.reflector_callsign;
    settings.protocol = defaults.protocol;
    settings.favorites.clear();
    settings.recents.clear();
    true
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
        // Saved-host lists use numbered keys (`favorite.N`,
        // `recent.N`); entries load in file order, which is the
        // order `serialize` wrote them.
        if key.starts_with("favorite.") || key.starts_with("recent.") {
            let value = parse_quoted(value).ok_or_else(|| {
                format!("line {}: value must be a double-quoted string", lineno + 1)
            })?;
            match decode_saved(&value) {
                Some(entry) if key.starts_with("favorite.") => out.favorites.push(entry),
                Some(entry) => out.recents.push(entry),
                // Malformed entries are skipped, not fatal: one bad
                // line must not cost the user their whole settings.
                None => warn!(key, "skipping malformed saved-host entry"),
            }
            continue;
        }
        // Boolean keys carry a bare `true` / `false`, not a quoted
        // string, so handle them before the quoted-string parse.
        if key == "reconnect_on_drop" || key == "persist_heard_list" || key == "tx_beacon_enabled" {
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
            match key {
                "reconnect_on_drop" => out.reconnect_on_drop = flag,
                "persist_heard_list" => out.persist_heard_list = flag,
                _ => out.tx_beacon_enabled = flag,
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
            "time_mode" => out.time_mode = TimeMode::from_str(&value),
            "rx_audio" => out.rx_audio = RxAudioMode::from_str(&value),
            "tx_message" => out.tx_message = value,
            "tx_lat" => out.tx_lat = value,
            "tx_lon" => out.tx_lon = value,
            "tx_symbol" => out.tx_symbol = value,
            "tx_comment" => out.tx_comment = value,
            "input_device" => out.input_device = value,
            "output_device" => out.output_device = value,
            // Unknown keys are ignored: forward-compat for future
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
                // Unknown escape sequence: bail.
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
    use super::{
        RECENTS_CAP, REFLECTOR_ADDRESS_EPOCH, RxAudioMode, SavedHost, Settings, TimeMode,
        migrate_legacy_reflector_addresses, parse, push_recent, serialize,
    };

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
            time_mode: TimeMode::Utc,
            rx_audio: RxAudioMode::Enhanced,
            tx_message: "73 de sextant".into(),
            tx_beacon_enabled: true,
            tx_lat: "41.7148".into(),
            tx_lon: "-72.7273".into(),
            tx_symbol: "-".into(),
            tx_comment: "home QTH".into(),
            input_device: "Built-in Microphone".into(),
            output_device: "External Headphones".into(),
            favorites: vec![saved("REF001")],
            recents: vec![saved("XRF757")],
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
    fn unknown_time_mode_falls_back_to_local() -> TestResult {
        let raw = "time_mode = \"martian\"\n";
        let parsed = parse(raw).map_err(|e| format!("parse: {e}"))?;
        assert_eq!(parsed.time_mode, TimeMode::Local);
        Ok(())
    }

    #[test]
    fn unknown_rx_audio_mode_falls_back_to_raw() -> TestResult {
        let raw = "rx_audio = \"psychoacoustic\"\n";
        let parsed = parse(raw).map_err(|e| format!("parse: {e}"))?;
        assert_eq!(parsed.rx_audio, RxAudioMode::Raw);
        Ok(())
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

    fn saved(callsign: &str) -> SavedHost {
        SavedHost {
            callsign: callsign.to_owned(),
            host: "example.org".into(),
            port: "20001".into(),
            protocol: "DPlus".into(),
            module: 'C',
        }
    }

    #[test]
    fn favorites_and_recents_roundtrip() -> TestResult {
        let original = Settings {
            favorites: vec![saved("REF030"), saved("XRF757")],
            recents: vec![saved("DCS001")],
            ..Settings::default()
        };
        let parsed = parse(&serialize(&original)).map_err(|e| format!("parse: {e}"))?;
        assert_eq!(parsed.favorites, original.favorites);
        assert_eq!(parsed.recents, original.recents);
        Ok(())
    }

    #[test]
    fn legacy_settings_discard_unprovenanced_reflector_addresses() -> TestResult {
        let raw = concat!(
            "callsign = \"KQ4NIT\"\n",
            "reflector_host = \"attacker-controlled.example\"\n",
            "reflector_port = \"30001\"\n",
            "reflector_callsign = \"XLX999\"\n",
            "protocol = \"DExtra\"\n",
            "favorite.0 = \"XLX999|attacker-controlled.example|30001|DExtra|C\"\n",
            "recent.0 = \"XLX999|attacker-controlled.example|30001|DExtra|C\"\n",
        );
        let mut parsed = parse(raw).map_err(|e| format!("parse: {e}"))?;

        assert!(migrate_legacy_reflector_addresses(raw, &mut parsed));
        assert_eq!(parsed.callsign, "KQ4NIT", "unrelated settings survive");
        assert_eq!(parsed.reflector_host, "127.0.0.1");
        assert_eq!(parsed.reflector_callsign, "POLARIS");
        assert!(parsed.favorites.is_empty());
        assert!(parsed.recents.is_empty());

        let migrated_raw = serialize(&parsed);
        let mut reloaded = parse(&migrated_raw).map_err(|e| format!("parse: {e}"))?;
        assert!(
            !migrate_legacy_reflector_addresses(&migrated_raw, &mut reloaded),
            "the persisted marker makes the migration idempotent"
        );
        assert_eq!(reloaded, parsed);
        Ok(())
    }

    #[test]
    fn current_settings_epoch_preserves_explicit_addresses() -> TestResult {
        let original = Settings {
            reflector_host: "manual.example".into(),
            favorites: vec![saved("XRF757")],
            ..Settings::default()
        };
        let raw = serialize(&original);
        assert!(raw.contains(REFLECTOR_ADDRESS_EPOCH));
        let mut parsed = parse(&raw).map_err(|e| format!("parse: {e}"))?;

        assert!(!migrate_legacy_reflector_addresses(&raw, &mut parsed));
        assert_eq!(parsed.reflector_host, "manual.example");
        assert_eq!(parsed.favorites, original.favorites);
        Ok(())
    }

    #[test]
    fn malformed_saved_host_is_skipped_not_fatal() -> TestResult {
        let raw = "callsign = \"W1AW\"\nfavorite.0 = \"only|three|fields\"\n";
        let parsed = parse(raw).map_err(|e| format!("parse: {e}"))?;
        assert!(parsed.favorites.is_empty(), "malformed entry skipped");
        assert_eq!(parsed.callsign, "W1AW", "rest of the file still parses");
        Ok(())
    }

    #[test]
    fn push_recent_promotes_dedupes_and_caps() {
        let mut recents = Vec::new();
        for i in 0..10 {
            push_recent(&mut recents, saved(&format!("REF{i:03}")));
        }
        assert_eq!(recents.len(), RECENTS_CAP, "capped at {RECENTS_CAP}");
        assert_eq!(
            recents.first().map(|r| r.callsign.as_str()),
            Some("REF009"),
            "most recent first"
        );
        // Re-connecting to an existing entry promotes, not duplicates.
        push_recent(&mut recents, saved("REF005"));
        assert_eq!(recents.len(), RECENTS_CAP);
        assert_eq!(
            recents.first().map(|r| r.callsign.as_str()),
            Some("REF005"),
            "existing entry promoted to front"
        );
        let count = recents.iter().filter(|r| r.callsign == "REF005").count();
        assert_eq!(count, 1, "no duplicate entries");
    }

    #[test]
    fn embedded_quotes_roundtrip() -> TestResult {
        // Adversarial input: a callsign containing a quote (won't
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
