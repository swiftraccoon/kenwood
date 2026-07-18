// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later

//! Runtime target control for the recorder.
//!
//! One callsign gets one session per reflector, so testing a client
//! (sextant) against a busy reflector used to mean restarting the
//! whole recorder with an edited config — dropping and relinking
//! every target. This module gives the running recorder a local
//! control socket instead: `stargazer ctl disable REF030-C` unlinks
//! exactly one target (politely, finalizing open captures), `enable`
//! reclaims it, `reload` applies config target additions/removals,
//! and `status` reports the live picture. Disables persist across
//! recorder restarts (a target you freed stays freed until you say
//! otherwise) via a marker file next to the recordings.
//!
//! The socket lives inside the recordings directory, so `ctl` finds
//! it through the same `--config` the recorder used.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, oneshot, watch};

use crate::config::{Config, Target};
use crate::writer::Writer;

/// Control socket filename (inside the recordings directory).
pub const SOCKET_FILE: &str = ".stargazer-ctl.sock";

/// Persisted disabled-target list (inside the recordings directory).
pub const DISABLED_FILE: &str = ".disabled-targets";

/// Longest we wait for one target's graceful unlink before reporting
/// it as slow (the task keeps draining in the background).
const DRAIN_TIMEOUT: Duration = Duration::from_secs(6);

/// Consecutive `accept()` failures on the control socket after which
/// the server stops (a genuinely unusable listener, not a transient
/// per-connection error). Transient errors below this bound are
/// logged and retried so a control-plane hiccup never stops recording.
const MAX_ACCEPT_FAILURES: usize = 8;

/// One parsed control command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Report every configured target's state.
    Status,
    /// Unlink one target and keep it unlinked (persistently).
    Disable(String),
    /// Clear a disable and relink the target.
    Enable(String),
    /// Re-read the config file and apply target additions/removals.
    Reload,
}

impl Command {
    /// Parse one request line (`status` / `disable REF030-C` /
    /// `enable ref030-c` / `reload`). Target names normalize to
    /// uppercase.
    ///
    /// # Errors
    ///
    /// A human-readable usage message for unknown verbs or arity.
    pub fn parse(line: &str) -> Result<Self, String> {
        const USAGE: &str = "usage: status | disable <TARGET> | enable <TARGET> | reload";
        let mut words = line.split_whitespace();
        let verb = words.next().unwrap_or("");
        let arg = words.next();
        if words.next().is_some() {
            return Err(USAGE.to_string());
        }
        match (verb.to_ascii_lowercase().as_str(), arg) {
            ("status", None) => Ok(Self::Status),
            ("reload", None) => Ok(Self::Reload),
            ("disable", Some(t)) => Ok(Self::Disable(t.to_ascii_uppercase())),
            ("enable", Some(t)) => Ok(Self::Enable(t.to_ascii_uppercase())),
            _ => Err(USAGE.to_string()),
        }
    }
}

/// Canonical registry key for a target: `"REF030-C"`.
#[must_use]
pub fn target_key(target: &Target) -> String {
    format!("{}-{}", target.reflector, target.module.as_char())
}

/// What a config reload has to do, given the current registry.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ReloadPlan {
    /// Targets to start supervising (configured, not running, not
    /// disabled).
    pub spawn: Vec<String>,
    /// Targets to stop (running but no longer configured).
    pub stop: Vec<String>,
}

/// Compute the reload diff.
#[must_use]
pub fn plan_reload(
    configured: &BTreeSet<String>,
    running: &BTreeSet<String>,
    disabled: &BTreeSet<String>,
) -> ReloadPlan {
    ReloadPlan {
        spawn: configured
            .iter()
            .filter(|k| !running.contains(*k) && !disabled.contains(*k))
            .cloned()
            .collect(),
        stop: running
            .iter()
            .filter(|k| !configured.contains(*k))
            .cloned()
            .collect(),
    }
}

/// Load the persisted disabled set (absent file = empty set).
///
/// # Errors
///
/// I/O errors other than the file not existing.
pub fn load_disabled(path: &Path) -> std::io::Result<BTreeSet<String>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeSet::new()),
        Err(e) => Err(e),
    }
}

/// Persist the disabled set atomically.
///
/// # Errors
///
/// Atomic write failures.
pub fn save_disabled(path: &Path, set: &BTreeSet<String>) -> Result<(), crate::writer::WriteError> {
    let mut body = String::new();
    for key in set {
        body.push_str(key);
        body.push('\n');
    }
    crate::writer::write_atomic(path, body.as_bytes(), true)
}

/// A command paired with its reply channel, as sent by the socket
/// listener to the coordinator.
pub type CommandRequest = (Command, oneshot::Sender<String>);

/// True when a live recorder answers on the control socket.
///
/// Used as a single-instance guard: two recorders with the same
/// callsign would displace each other on every reflector, so a
/// starting recorder refuses to run when one already answers. A
/// stale socket file with no listener behind it reports `false`.
pub async fn recorder_already_running(socket_path: &Path) -> bool {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let attempt = async {
        let mut stream = tokio::net::UnixStream::connect(socket_path).await.ok()?;
        stream.write_all(b"status\n").await.ok()?;
        let mut byte = [0u8; 1];
        let n = stream.read(&mut byte).await.ok()?;
        (n > 0).then_some(())
    };
    tokio::time::timeout(Duration::from_secs(2), attempt)
        .await
        .ok()
        .flatten()
        .is_some()
}

/// Owns the per-target supervisors and applies control commands.
#[derive(Debug)]
pub struct Coordinator {
    config: Config,
    config_path: PathBuf,
    writer: Arc<Writer>,
    running: BTreeMap<String, RunningTarget>,
    disabled: BTreeSet<String>,
}

#[derive(Debug)]
struct RunningTarget {
    shutdown: watch::Sender<bool>,
    join: tokio::task::JoinHandle<()>,
}

impl Coordinator {
    /// Build a coordinator from a loaded config, reading any
    /// persisted disabled set.
    ///
    /// # Errors
    ///
    /// I/O errors reading the disabled-set file.
    pub fn new(config: Config, config_path: PathBuf, writer: Arc<Writer>) -> std::io::Result<Self> {
        let disabled = load_disabled(&config.recordings_dir.join(DISABLED_FILE))?;
        for key in &disabled {
            tracing::warn!(target = %key, "target is disabled (persisted) — not linking");
        }
        Ok(Self {
            config,
            config_path,
            writer,
            running: BTreeMap::new(),
            disabled,
        })
    }

    /// Path of the control socket for this coordinator's recordings
    /// directory.
    #[must_use]
    pub fn socket_path(&self) -> PathBuf {
        self.config.recordings_dir.join(SOCKET_FILE)
    }

    /// Spawn supervisors for every configured, non-disabled target.
    pub fn spawn_initial(&mut self) {
        let keys: Vec<String> = self.config.targets.iter().map(target_key).collect();
        for key in keys {
            if !self.disabled.contains(&key) {
                self.spawn_target(&key);
            }
        }
    }

    /// Number of currently supervised targets.
    #[must_use]
    pub fn running_len(&self) -> usize {
        self.running.len()
    }

    fn spawn_target(&mut self, key: &str) {
        if self.running.contains_key(key) {
            return;
        }
        let Some(target) = self
            .config
            .targets
            .iter()
            .find(|t| target_key(t) == key)
            .cloned()
        else {
            return;
        };
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let join = tokio::spawn(crate::session::run_supervisor(
            target,
            self.config.callsign,
            self.config.local_module,
            Arc::clone(&self.writer),
            shutdown_rx,
        ));
        let _unused = self.running.insert(
            key.to_string(),
            RunningTarget {
                shutdown: shutdown_tx,
                join,
            },
        );
        tracing::info!(target = %key, "supervising");
    }

    /// Stop one running target gracefully (unlink + finalize).
    /// Returns a status word for the response.
    async fn stop_target(&mut self, key: &str) -> &'static str {
        let Some(running) = self.running.remove(key) else {
            return "was not running";
        };
        let _unused = running.shutdown.send(true);
        match tokio::time::timeout(DRAIN_TIMEOUT, running.join).await {
            Ok(_) => "unlinked",
            Err(_) => "unlink is taking long — draining in background",
        }
    }

    fn persist_disabled(&self) -> String {
        let path = self.config.recordings_dir.join(DISABLED_FILE);
        match save_disabled(&path, &self.disabled) {
            Ok(()) => String::new(),
            Err(e) => format!(" (WARNING: could not persist disable state: {e})"),
        }
    }

    fn configured_keys(&self) -> BTreeSet<String> {
        self.config.targets.iter().map(target_key).collect()
    }

    /// Apply one command, returning the human-readable response.
    pub async fn handle(&mut self, command: Command) -> String {
        match command {
            Command::Status => {
                let mut lines = Vec::new();
                for key in self.configured_keys() {
                    let state = if self.disabled.contains(&key) {
                        "DISABLED (slot free)"
                    } else if self.running.contains_key(&key) {
                        "running"
                    } else {
                        "starting"
                    };
                    lines.push(format!("{key:<12} {state}"));
                }
                for key in self.running.keys() {
                    if !self.configured_keys().contains(key) {
                        lines.push(format!("{key:<12} running (no longer in config)"));
                    }
                }
                lines.join("\n")
            }
            Command::Disable(key) => {
                if !self.configured_keys().contains(&key) && !self.running.contains_key(&key) {
                    return format!("unknown target {key} — see `status`");
                }
                let outcome = self.stop_target(&key).await;
                let _unused = self.disabled.insert(key.clone());
                let warn = self.persist_disabled();
                format!("{key} disabled — {outcome}; slot is free until `enable`{warn}")
            }
            Command::Enable(key) => {
                if !self.configured_keys().contains(&key) {
                    return format!("unknown target {key} — see `status`");
                }
                let _unused = self.disabled.remove(&key);
                let warn = self.persist_disabled();
                self.spawn_target(&key);
                format!("{key} enabled — relinking{warn}")
            }
            Command::Reload => match crate::config::load(&self.config_path) {
                Err(e) => format!("config reload failed (targets unchanged): {e}"),
                Ok(new_config) => {
                    let mut notes = Vec::new();
                    if new_config.callsign != self.config.callsign
                        || new_config.local_module != self.config.local_module
                        || new_config.recordings_dir != self.config.recordings_dir
                    {
                        notes.push(
                            "callsign/local_module/recordings_dir changes need a restart"
                                .to_string(),
                        );
                    }
                    self.config.targets = new_config.targets;
                    let plan = plan_reload(
                        &self.configured_keys(),
                        &self.running.keys().cloned().collect(),
                        &self.disabled,
                    );
                    for key in &plan.stop {
                        let outcome = self.stop_target(key).await;
                        notes.push(format!("stopped {key} ({outcome})"));
                    }
                    for key in &plan.spawn {
                        self.spawn_target(key);
                        notes.push(format!("started {key}"));
                    }
                    if plan.stop.is_empty() && plan.spawn.is_empty() {
                        notes.push("no target changes".to_string());
                    }
                    format!("reloaded: {}", notes.join("; "))
                }
            },
        }
    }

    /// Gracefully stop every supervisor (recorder shutdown).
    pub async fn shutdown_all(mut self) {
        let keys: Vec<String> = self.running.keys().cloned().collect();
        for key in keys {
            let _outcome = self.stop_target(&key).await;
        }
    }
}

/// Accept loop for the control socket: one line in, one response out,
/// close. Commands are forwarded to the coordinator over `requests`.
pub async fn serve_socket(
    listener: tokio::net::UnixListener,
    requests: mpsc::Sender<CommandRequest>,
) {
    use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
    // A transient accept error (ECONNABORTED from a client that aborts
    // between connect and accept, or EMFILE/ENFILE under fd pressure)
    // is NOT the listener closing — treating it as terminal would drop
    // the request sender and tear down every reflector recording (main
    // breaks its loop on a closed channel). The control plane must
    // never take the data plane down with it, so we log and keep
    // serving; only a sustained run of failures (a genuinely broken
    // listener) gives up.
    let mut consecutive_failures: usize = 0;
    loop {
        let stream = match listener.accept().await {
            Ok((stream, _addr)) => {
                consecutive_failures = 0;
                stream
            }
            Err(e) => {
                consecutive_failures += 1;
                if consecutive_failures >= MAX_ACCEPT_FAILURES {
                    tracing::error!(error = %e, "control socket accept failed repeatedly — giving up");
                    return;
                }
                tracing::warn!(error = %e, "control socket accept failed — continuing");
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            }
        };
        let requests = requests.clone();
        let _unused: tokio::task::JoinHandle<()> = tokio::spawn(async move {
            let (read_half, mut write_half) = stream.into_split();
            let mut line = String::new();
            if BufReader::new(read_half)
                .read_line(&mut line)
                .await
                .is_err()
            {
                return;
            }
            let response = match Command::parse(&line) {
                Err(usage) => usage,
                Ok(command) => {
                    let (reply_tx, reply_rx) = oneshot::channel();
                    if requests.send((command, reply_tx)).await.is_err() {
                        "recorder is shutting down".to_string()
                    } else {
                        reply_rx
                            .await
                            .unwrap_or_else(|_| "recorder dropped the request".to_string())
                    }
                }
            };
            let _unused = write_half.write_all(response.as_bytes()).await;
            let _unused = write_half.write_all(b"\n").await;
            let _unused = write_half.shutdown().await;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn commands_parse_and_normalize() {
        assert_eq!(Command::parse("status"), Ok(Command::Status));
        assert_eq!(Command::parse(" reload \n"), Ok(Command::Reload));
        assert_eq!(
            Command::parse("disable ref030-c"),
            Ok(Command::Disable("REF030-C".to_string())),
            "targets normalize to uppercase"
        );
        assert_eq!(
            Command::parse("enable REF054-C"),
            Ok(Command::Enable("REF054-C".to_string()))
        );
        let unknown = Command::parse("explode");
        assert!(
            matches!(unknown, Err(msg) if msg.contains("status")),
            "usage text lists verbs"
        );
        assert!(Command::parse("disable").is_err(), "missing target");
        assert!(Command::parse("disable a b").is_err(), "extra args");
        assert!(Command::parse("").is_err());
    }

    #[test]
    fn reload_plan_spawns_new_and_stops_removed() {
        let configured: BTreeSet<String> = ["REF001-C", "REF054-C", "REF030-C"]
            .iter()
            .map(ToString::to_string)
            .collect();
        let running: BTreeSet<String> = ["REF001-C", "REF999-Z"]
            .iter()
            .map(ToString::to_string)
            .collect();
        let disabled: BTreeSet<String> = std::iter::once("REF030-C".to_string()).collect();
        let plan = plan_reload(&configured, &running, &disabled);
        assert_eq!(
            plan.spawn,
            ["REF054-C"],
            "new target, minus the disabled one"
        );
        assert_eq!(plan.stop, ["REF999-Z"], "no longer configured");
    }

    /// A config whose single target points at a dead local port: the
    /// supervisor stays in its connect/backoff loop, which is exactly
    /// what disable must be able to interrupt.
    fn write_config(path: &Path, recordings: &Path, reflector: &str) -> std::io::Result<()> {
        std::fs::write(
            path,
            format!(
                "callsign = \"KQ4NIT\"\nlocal_module = \"D\"\nwrite_wav = false\n\
                 recordings_dir = \"{}\"\n[[record]]\nreflector = \"{reflector}\"\n\
                 protocol = \"dextra\"\nhost = \"127.0.0.1\"\nport = 1\nmodules = [\"C\"]\n",
                recordings.display()
            ),
        )
    }

    #[tokio::test]
    async fn coordinator_disable_enable_reload_lifecycle() -> TestResult {
        let dir = tempfile::tempdir()?;
        let rec_dir = dir.path().join("recordings");
        std::fs::create_dir_all(&rec_dir)?;
        let config_path = dir.path().join("stargazer.toml");
        write_config(&config_path, &rec_dir, "XRF001")?;
        let config = crate::config::load(&config_path).map_err(|e| e.to_string())?;
        let writer = Arc::new(Writer::new(rec_dir.clone(), false));
        let mut coord = Coordinator::new(config, config_path.clone(), writer)?;
        coord.spawn_initial();
        assert_eq!(coord.running_len(), 1);
        let status = coord.handle(Command::Status).await;
        assert!(status.contains("XRF001-C"), "{status}");

        // Disable interrupts the connect/backoff loop and persists.
        let resp = coord.handle(Command::Disable("XRF001-C".to_string())).await;
        assert!(resp.contains("disabled"), "{resp}");
        assert_eq!(coord.running_len(), 0);
        assert!(load_disabled(&rec_dir.join(DISABLED_FILE))?.contains("XRF001-C"));
        let status = coord.handle(Command::Status).await;
        assert!(status.contains("DISABLED"), "{status}");

        // Enable relinks and clears the persisted mark.
        let resp = coord.handle(Command::Enable("XRF001-C".to_string())).await;
        assert!(resp.contains("enabled"), "{resp}");
        assert_eq!(coord.running_len(), 1);
        assert!(!load_disabled(&rec_dir.join(DISABLED_FILE))?.contains("XRF001-C"));

        // Unknown targets are refused.
        let resp = coord.handle(Command::Disable("REF999-Z".to_string())).await;
        assert!(resp.contains("unknown"), "{resp}");

        // Reload applies target swaps: XRF001-C out, XRF002-C in.
        write_config(&config_path, &rec_dir, "XRF002")?;
        let resp = coord.handle(Command::Reload).await;
        assert!(resp.contains("started XRF002-C"), "{resp}");
        assert!(resp.contains("stopped XRF001-C"), "{resp}");
        assert_eq!(coord.running_len(), 1);

        coord.shutdown_all().await;
        Ok(())
    }

    #[tokio::test]
    async fn socket_roundtrip_parses_and_replies() -> TestResult {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        let dir = tempfile::tempdir()?;
        let sock_path = dir.path().join(SOCKET_FILE);
        let listener = tokio::net::UnixListener::bind(&sock_path)?;
        let (tx, mut rx) = mpsc::channel::<CommandRequest>(4);
        let server = tokio::spawn(serve_socket(listener, tx));
        let responder = tokio::spawn(async move {
            while let Some((cmd, reply)) = rx.recv().await {
                let _unused = reply.send(format!("got {cmd:?}"));
            }
        });

        let mut stream = tokio::net::UnixStream::connect(&sock_path).await?;
        stream.write_all(b"disable ref030-c\n").await?;
        let mut resp = String::new();
        let _bytes = stream.read_to_string(&mut resp).await?;
        assert!(resp.contains("Disable(\"REF030-C\")"), "{resp}");

        // A bad verb gets the usage text without a coordinator trip.
        let mut stream = tokio::net::UnixStream::connect(&sock_path).await?;
        stream.write_all(b"explode\n").await?;
        let mut resp = String::new();
        let _bytes = stream.read_to_string(&mut resp).await?;
        assert!(resp.contains("usage"), "{resp}");

        responder.abort();
        server.abort();
        Ok(())
    }

    #[tokio::test]
    async fn instance_guard_detects_live_recorder_and_ignores_stale_sockets() -> TestResult {
        let dir = tempfile::tempdir()?;
        let sock = dir.path().join(SOCKET_FILE);
        assert!(
            !recorder_already_running(&sock).await,
            "no socket file at all"
        );

        let listener = tokio::net::UnixListener::bind(&sock)?;
        let (tx, mut rx) = mpsc::channel::<CommandRequest>(4);
        let server = tokio::spawn(serve_socket(listener, tx));
        let responder = tokio::spawn(async move {
            while let Some((_cmd, reply)) = rx.recv().await {
                let _unused = reply.send("ok".to_string());
            }
        });
        assert!(
            recorder_already_running(&sock).await,
            "a live recorder must be detected"
        );
        server.abort();
        responder.abort();

        // The socket file lingers after the listener dies (nothing
        // unlinks it) — connect is refused, so: not running.
        assert!(
            !recorder_already_running(&sock).await,
            "stale socket file must not read as a live recorder"
        );
        Ok(())
    }

    #[test]
    fn disabled_set_roundtrips_and_absent_file_is_empty() -> TestResult {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join(DISABLED_FILE);
        assert!(load_disabled(&path)?.is_empty(), "absent file = empty");
        let set: BTreeSet<String> = ["REF030-C", "REF068-B"]
            .iter()
            .map(ToString::to_string)
            .collect();
        save_disabled(&path, &set)?;
        assert_eq!(load_disabled(&path)?, set);
        save_disabled(&path, &BTreeSet::new())?;
        assert!(load_disabled(&path)?.is_empty());
        Ok(())
    }
}
