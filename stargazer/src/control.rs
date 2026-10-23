// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later

//! Checked target commands and retained supervisor retirement.
//!
//! Unix socket requests are bounded to 128 bytes, 32 concurrent clients and
//! ten seconds including the coordinator reply. A forwarded command continues
//! if its client disconnects. Other platforms can use the coordinator directly;
//! the command-line socket transport is Unix-only.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;
#[cfg(unix)]
use tokio::sync::{mpsc, oneshot};

use crate::config::{Config, ConfigError, Target, TargetKey};
use crate::session::SupervisorSummary;
use crate::writer::Writer;

/// Unix control socket filename under the recordings directory.
pub const SOCKET_FILE: &str = ".stargazer-ctl.sock";
/// Persisted disabled target identities under the recordings directory.
pub const DISABLED_FILE: &str = ".disabled-targets";
const DRAIN_TIMEOUT: Duration = Duration::from_secs(6);
#[cfg(unix)]
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
#[cfg(unix)]
const MAX_COMMAND_BYTES: usize = 128;
#[cfg(unix)]
const MAX_CLIENTS: usize = 32;

/// A validated local recorder command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Report configured and retiring targets.
    Status,
    /// Stop this target and persist its disabled state.
    Disable(TargetKey),
    /// Clear this target's disabled state and start after retirement finishes.
    Enable(TargetKey),
    /// Apply endpoint/protocol/target changes from the configuration file.
    Reload,
}

/// A request has invalid command syntax or target identity.
#[derive(Debug, thiserror::Error)]
pub enum CommandError {
    /// Unknown verb or wrong number of arguments.
    #[error("usage: status | disable <TARGET> | enable <TARGET> | reload")]
    Usage,
    /// The target is not a safe reflector/module identity.
    #[error("target: {0}")]
    Target(#[source] ConfigError),
}
impl FromStr for Command {
    type Err = CommandError;
    fn from_str(line: &str) -> Result<Self, Self::Err> {
        let mut words = line.split_whitespace();
        let verb = words.next().unwrap_or_default();
        let argument = words.next();
        if words.next().is_some() {
            return Err(CommandError::Usage);
        }
        match (verb.to_ascii_lowercase().as_str(), argument) {
            ("status", None) => Ok(Self::Status),
            ("reload", None) => Ok(Self::Reload),
            ("disable", Some(target)) => {
                Ok(Self::Disable(target.parse().map_err(CommandError::Target)?))
            }
            ("enable", Some(target)) => {
                Ok(Self::Enable(target.parse().map_err(CommandError::Target)?))
            }
            _ => Err(CommandError::Usage),
        }
    }
}

fn load_disabled(path: &Path) -> std::io::Result<BTreeSet<TargetKey>> {
    use std::io::{BufRead as _, Read as _};
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeSet::new()),
        Err(error) => return Err(error),
    };
    let mut reader = std::io::BufReader::new(file);
    let mut disabled = BTreeSet::new();
    loop {
        let mut line = Vec::new();
        let _count = reader.by_ref().take(12).read_until(b'\n', &mut line)?;
        if line.is_empty() {
            break;
        }
        if line.len() == 12 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "disabled target line exceeds its routing width",
            ));
        }
        let text = std::str::from_utf8(&line)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?
            .trim();
        if text.is_empty() {
            continue;
        }
        let target = text
            .parse()
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        let _present = disabled.insert(target);
    }
    Ok(disabled)
}

fn save_disabled(
    path: &Path,
    disabled: &BTreeSet<TargetKey>,
) -> Result<(), crate::writer::WriteError> {
    use std::fmt::Write as _;
    let mut body = String::new();
    for target in disabled {
        let _written = writeln!(body, "{target}");
    }
    crate::writer::replace_state(path, body.as_bytes())
}

/// One completed target supervisor and its typed final outcome.
#[derive(Debug)]
pub struct TargetCompletion {
    /// Reflector/module whose task was joined.
    pub target: TargetKey,
    /// Bounded write/connect/session statistics, or a task panic/cancellation.
    pub result: Result<SupervisorSummary, tokio::task::JoinError>,
}

/// Human-readable command response and joined task outcomes.
#[derive(Debug)]
pub struct CommandResponse {
    message: String,
    completions: Vec<TargetCompletion>,
}
impl CommandResponse {
    /// State and next action suitable for a terminal response.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
    /// Joined supervisors, including typed failures.
    #[must_use]
    pub fn completions(&self) -> &[TargetCompletion] {
        &self.completions
    }
}

#[derive(Debug)]
struct RunningTarget {
    target: Target,
    shutdown: watch::Sender<bool>,
    join: tokio::task::JoinHandle<SupervisorSummary>,
    retiring: bool,
}

/// Target supervisor registry that retains every task until it is joined.
///
/// Failed/cancelled command waits preserve retiring entries. Call [`Self::reconcile`]
/// periodically to collect completed retirements and start queued replacements.
/// Complete [`Self::shutdown_all`] before dropping the registry to observe all
/// outcomes. Drop requests stop without blocking; it cannot await task completion.
#[derive(Debug)]
pub struct Coordinator {
    config: Config,
    config_path: PathBuf,
    writer: Arc<Writer>,
    running: BTreeMap<TargetKey, RunningTarget>,
    disabled: BTreeSet<TargetKey>,
    completions: Vec<TargetCompletion>,
    stopping: bool,
}

impl Coordinator {
    /// Read persisted disabled targets before spawning any network task.
    ///
    /// # Errors
    ///
    /// Returns filesystem or malformed persisted-target errors.
    pub fn new(config: Config, config_path: PathBuf, writer: Arc<Writer>) -> std::io::Result<Self> {
        let disabled = load_disabled(&config.recordings_dir.join(DISABLED_FILE))?;
        Ok(Self {
            config,
            config_path,
            writer,
            running: BTreeMap::new(),
            disabled,
            completions: Vec::new(),
            stopping: false,
        })
    }
    /// Local socket pathname for the configured archive root.
    #[must_use]
    pub fn socket_path(&self) -> PathBuf {
        self.config.recordings_dir.join(SOCKET_FILE)
    }
    /// Start configured targets that are neither disabled nor already retained.
    pub fn spawn_initial(&mut self) {
        let keys: Vec<_> = self.config.targets.iter().map(Target::key).collect();
        for key in keys {
            self.spawn_target(&key);
        }
    }
    /// Active plus retiring supervisor tasks whose outcomes remain owned here.
    #[must_use]
    pub fn running_len(&self) -> usize {
        self.running.len()
    }
    /// Whether a target is stopping while its task is still retained.
    #[must_use]
    pub fn is_retiring(&self, key: &TargetKey) -> bool {
        self.running.get(key).is_some_and(|target| target.retiring)
    }

    fn spawn_target(&mut self, key: &TargetKey) {
        if self.stopping || self.disabled.contains(key) || self.running.contains_key(key) {
            return;
        }
        let Some(target) = self
            .config
            .targets
            .iter()
            .find(|target| target.key() == *key)
            .cloned()
        else {
            return;
        };
        let (shutdown, receiver) = watch::channel(false);
        let join = tokio::spawn(crate::session::run_supervisor(
            target.clone(),
            self.config.callsign,
            self.config.local_module,
            self.config.capture_limit,
            self.config.concurrent_capture_limit,
            self.writer.clone(),
            receiver,
        ));
        let _previous = self.running.insert(
            key.clone(),
            RunningTarget {
                target,
                shutdown,
                join,
                retiring: false,
            },
        );
    }

    async fn stop_target(&mut self, key: &TargetKey) {
        let Some(running) = self.running.get_mut(key) else {
            return;
        };
        running.retiring = true;
        let _delivered = running.shutdown.send(true);
        let Ok(result) = tokio::time::timeout(DRAIN_TIMEOUT, &mut running.join).await else {
            return;
        };
        let _joined = self.running.remove(key);
        self.completions.push(TargetCompletion {
            target: key.clone(),
            result,
        });
    }

    /// Join finished tasks and start enabled replacements after their old task ends.
    ///
    /// Cancellation retains every task not yet returned in the result. This method
    /// awaits only handles already marked finished by Tokio.
    pub async fn reconcile(&mut self) -> Vec<TargetCompletion> {
        self.collect_finished().await;
        std::mem::take(&mut self.completions)
    }

    async fn collect_finished(&mut self) {
        let keys: Vec<_> = self
            .running
            .iter()
            .filter(|(_, target)| target.join.is_finished())
            .map(|(key, _)| key.clone())
            .collect();
        for key in keys {
            if let Some(target) = self.running.get_mut(&key) {
                let result = (&mut target.join).await;
                let _joined = self.running.remove(&key);
                self.completions.push(TargetCompletion {
                    target: key,
                    result,
                });
            }
        }
        self.spawn_initial();
    }

    fn persist_disabled(&self) -> String {
        match save_disabled(
            &self.config.recordings_dir.join(DISABLED_FILE),
            &self.disabled,
        ) {
            Ok(()) => String::new(),
            Err(error) => format!("; persistence failed: {error}"),
        }
    }

    /// Apply a command and return state text plus all task outcomes joined during it.
    ///
    /// Disable waits up to six seconds, then reports retirement while retaining
    /// the handle. Enable/reload cannot create a replacement until that handle
    /// is joined. Local retirement makes no claim that a peer acknowledged an
    /// interrupted handshake. Cancellation preserves applied stop/disable state.
    pub async fn handle(&mut self, command: Command) -> CommandResponse {
        self.collect_finished().await;
        let message = match command {
            Command::Status => self.status_text(),
            Command::Disable(key) => {
                if !self.config.targets.iter().any(|target| target.key() == key)
                    && !self.running.contains_key(&key)
                {
                    format!("unknown target {key}; see status")
                } else {
                    let _inserted = self.disabled.insert(key.clone());
                    let warning = self.persist_disabled();
                    self.stop_target(&key).await;
                    if self.running.contains_key(&key) {
                        format!("{key} disabled; retiring local session{warning}")
                    } else {
                        format!("{key} disabled; local supervisor stopped{warning}")
                    }
                }
            }
            Command::Enable(key) => {
                if self.config.targets.iter().any(|target| target.key() == key) {
                    let _removed = self.disabled.remove(&key);
                    let warning = self.persist_disabled();
                    self.spawn_target(&key);
                    if self.is_retiring(&key) {
                        format!("{key} enabled; waiting for previous supervisor{warning}")
                    } else {
                        format!("{key} enabled; supervising{warning}")
                    }
                } else {
                    format!("unknown target {key}; see status")
                }
            }
            Command::Reload => self.reload().await,
        };
        let completions = std::mem::take(&mut self.completions);
        CommandResponse {
            message,
            completions,
        }
    }

    fn status_text(&self) -> String {
        let mut keys: BTreeSet<_> = self.config.targets.iter().map(Target::key).collect();
        keys.extend(self.running.keys().cloned());
        keys.into_iter()
            .map(|key| {
                let state = if self.is_retiring(&key) {
                    "retiring local session"
                } else if self.disabled.contains(&key) {
                    "DISABLED (local supervisor stopped)"
                } else if self.running.contains_key(&key) {
                    "running"
                } else {
                    "starting"
                };
                format!("{key:<12} {state}")
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    async fn reload(&mut self) -> String {
        let new = match Config::load(&self.config_path) {
            Ok(config) => config,
            Err(error) => return format!("config reload failed; targets unchanged: {error}"),
        };
        let mut notes = Vec::new();
        if new.callsign != self.config.callsign
            || new.local_module != self.config.local_module
            || new.recordings_dir != self.config.recordings_dir
            || new.write_wav != self.config.write_wav
            || new.capture_limit != self.config.capture_limit
            || new.concurrent_capture_limit != self.config.concurrent_capture_limit
        {
            notes.push("global recorder settings need a restart".to_owned());
        }
        self.config.targets = new.targets;
        let stop: Vec<_> = self
            .running
            .iter()
            .filter(|(_, running)| !self.config.targets.contains(&running.target))
            .map(|(key, _)| key.clone())
            .collect();
        for key in stop {
            self.stop_target(&key).await;
            notes.push(if self.is_retiring(&key) {
                format!("retiring {key}")
            } else {
                format!("stopped {key}")
            });
        }
        let start: Vec<_> = self
            .config
            .targets
            .iter()
            .map(Target::key)
            .filter(|key| !self.running.contains_key(key) && !self.disabled.contains(key))
            .collect();
        for key in start {
            self.spawn_target(&key);
            notes.push(format!("started {key}"));
        }
        if notes.is_empty() {
            notes.push("no target changes".to_owned());
        }
        format!("reloaded: {}", notes.join("; "))
    }

    /// Request stop for every task, then join every task and blocking writer.
    ///
    /// There is no timeout on filesystem completion. Cancellation leaves the
    /// registry in its stopping state with unjoined handles retained; call again
    /// to finish. No new supervisor starts once shutdown begins.
    pub async fn shutdown_all(&mut self) -> Vec<TargetCompletion> {
        self.stopping = true;
        for target in self.running.values_mut() {
            target.retiring = true;
            let _delivered = target.shutdown.send(true);
        }
        let keys: Vec<_> = self.running.keys().cloned().collect();
        for key in keys {
            if let Some(target) = self.running.get_mut(&key) {
                let result = (&mut target.join).await;
                let _joined = self.running.remove(&key);
                self.completions.push(TargetCompletion {
                    target: key,
                    result,
                });
            }
        }
        std::mem::take(&mut self.completions)
    }
}
impl Drop for Coordinator {
    fn drop(&mut self) {
        for target in self.running.values() {
            let _delivered = target.shutdown.send(true);
        }
    }
}

/// A bounded Unix socket request forwarded to the registry.
#[cfg(unix)]
#[derive(Debug)]
pub struct CommandRequest {
    /// Validated command.
    pub command: Command,
    /// Terminal response channel; a dropped receiver does not roll back the command.
    pub reply: oneshot::Sender<String>,
}

/// Failure determining whether a Unix socket already has a listener.
#[cfg(unix)]
#[derive(Debug, thiserror::Error)]
pub enum ProbeError {
    /// Connection attempt exceeded two seconds; occupancy remains unknown.
    #[error("control socket probe exceeded two seconds")]
    Deadline,
    /// Error other than an absent or refused socket.
    #[error("control socket probe: {0}")]
    Io(#[source] std::io::Error),
}

/// Detect an occupied Unix control socket without requiring a status reply.
///
/// # Errors
///
/// A two-second timeout or uncertain I/O failure returns [`ProbeError`]; callers
/// must not treat either as permission to remove the socket and start a recorder.
#[cfg(unix)]
pub async fn recorder_already_running(path: &Path) -> Result<bool, ProbeError> {
    match tokio::time::timeout(
        Duration::from_secs(2),
        tokio::net::UnixStream::connect(path),
    )
    .await
    {
        Ok(Ok(_stream)) => Ok(true),
        Ok(Err(error))
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            ) =>
        {
            Ok(false)
        }
        Ok(Err(error)) => Err(ProbeError::Io(error)),
        Err(_) => Err(ProbeError::Deadline),
    }
}

/// Bounded Unix socket server failures.
#[cfg(unix)]
#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    /// Eight consecutive listener accept operations failed.
    #[error("control socket accept: {0}")]
    Accept(#[source] std::io::Error),
    /// A retained request handler panicked or was cancelled unexpectedly.
    #[error("control request task: {0}")]
    Task(#[source] tokio::task::JoinError),
}

/// Serve bounded requests while retaining every client task in a join set.
///
/// Closing `requests` stops acceptance and aborts/joins pending clients. Cancelling
/// the server aborts its client tasks through the join set. Each client has a
/// ten-second total budget; already-forwarded commands continue in the registry.
///
/// # Errors
///
/// Returns [`ServerError`] after repeated listener errors or a handler task failure.
#[cfg(unix)]
pub async fn serve_socket(
    listener: tokio::net::UnixListener,
    requests: mpsc::Sender<CommandRequest>,
) -> Result<(), ServerError> {
    let mut handlers = tokio::task::JoinSet::new();
    let mut accept_failures = 0_u8;
    loop {
        tokio::select! {
            biased;
            () = requests.closed() => break,
            joined = handlers.join_next(), if !handlers.is_empty() => {
                if let Some(Err(error)) = joined { return Err(ServerError::Task(error)); }
            }
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    accept_failures = 0;
                    if handlers.len() < MAX_CLIENTS {
                        let requests = requests.clone();
                        let _abort = handlers.spawn(async move { let _result = tokio::time::timeout(REQUEST_TIMEOUT, serve_client(stream, requests)).await; });
                    }
                }
                Err(error) => {
                    accept_failures += 1;
                    if accept_failures >= 8 { return Err(ServerError::Accept(error)); }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            }
        }
    }
    handlers.abort_all();
    while handlers.join_next().await.is_some() {}
    Ok(())
}

#[cfg(unix)]
async fn serve_client(
    mut stream: tokio::net::UnixStream,
    requests: mpsc::Sender<CommandRequest>,
) -> std::io::Result<()> {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let mut bytes = Vec::with_capacity(MAX_COMMAND_BYTES);
    let mut byte = [0];
    loop {
        if stream.read(&mut byte).await? == 0 || byte == *b"\n" {
            break;
        }
        if bytes.len() == MAX_COMMAND_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "control command exceeds 128 bytes",
            ));
        }
        bytes.push(
            *byte
                .first()
                .ok_or_else(|| std::io::Error::other("read buffer"))?,
        );
    }
    let line = std::str::from_utf8(&bytes)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    let response = match line.parse::<Command>() {
        Err(error) => error.to_string(),
        Ok(command) => {
            let (reply, receiver) = oneshot::channel();
            if requests
                .send(CommandRequest { command, reply })
                .await
                .is_err()
            {
                "recorder is shutting down".to_owned()
            } else {
                receiver
                    .await
                    .unwrap_or_else(|_| "recorder dropped the request".to_owned())
            }
        }
    };
    stream.write_all(response.as_bytes()).await?;
    stream.write_all(b"\n").await?;
    stream.shutdown().await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(not(unix))]
    use tokio::sync::oneshot;
    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn config(
        path: &Path,
        recordings: &Path,
        host: &str,
        port: u16,
        reflector: &str,
    ) -> std::io::Result<()> {
        std::fs::write(
            path,
            format!(
                "callsign = \"W1AW\"\nmax_capture_seconds = 60\nmax_concurrent_captures = 4\nwrite_wav = false\nrecordings_dir = {:?}\n[[record]]\nreflector = {reflector:?}\nprotocol = \"dextra\"\nhost = {host:?}\nport = {port}\nmodules = [\"C\"]\n",
                recordings.to_string_lossy()
            ),
        )
    }

    #[test]
    fn commands_validate_syntax_and_path_identity() -> TestResult {
        assert_eq!(" status ".parse::<Command>()?, Command::Status);
        assert_eq!("reload".parse::<Command>()?, Command::Reload);
        assert_eq!(
            "disable ref030-c".parse::<Command>()?,
            Command::Disable("REF030-C".parse()?)
        );
        assert_eq!(
            "enable REF054-C".parse::<Command>()?,
            Command::Enable("REF054-C".parse()?)
        );
        for invalid in [
            "explode",
            "disable",
            "disable a b",
            "",
            "disable ../..-C",
            "enable CON-C",
        ] {
            assert!(invalid.parse::<Command>().is_err(), "{invalid}");
        }
        Ok(())
    }

    #[tokio::test]
    async fn coordinator_disable_enable_and_endpoint_reload_join_old_tasks() -> TestResult {
        let directory = tempfile::tempdir()?;
        let recordings = directory.path().join("recordings");
        std::fs::create_dir_all(&recordings)?;
        let path = directory.path().join("stargazer.toml");
        config(&path, &recordings, "127.0.0.1", 1, "XRF001")?;
        let mut registry = Coordinator::new(
            Config::load(&path)?,
            path.clone(),
            Arc::new(Writer::new(recordings.clone(), false)),
        )?;
        registry.spawn_initial();
        assert_eq!(registry.running_len(), 1);
        let key = "XRF001-C".parse::<TargetKey>()?;
        let response = registry.handle(Command::Disable(key.clone())).await;
        assert!(response.message().contains("disabled"));
        assert_eq!(registry.running_len(), 0);
        assert_eq!(response.completions().len(), 1);
        assert!(load_disabled(&recordings.join(DISABLED_FILE))?.contains(&key));
        let _enabled = registry.handle(Command::Enable(key.clone())).await;
        assert_eq!(registry.running_len(), 1);
        assert!(!load_disabled(&recordings.join(DISABLED_FILE))?.contains(&key));
        config(&path, &recordings, "127.0.0.2", 2, "XRF001")?;
        let response = registry.handle(Command::Reload).await;
        assert_eq!(
            response.completions().len(),
            1,
            "same-key endpoint change must retire the old task"
        );
        assert_eq!(
            registry
                .running
                .get(&key)
                .ok_or("replacement")?
                .target
                .port()
                .get(),
            2
        );
        config(&path, &recordings, "127.0.0.1", 1, "XRF002")?;
        let response = registry.handle(Command::Reload).await;
        assert!(response.message().contains("stopped XRF001-C"));
        assert!(response.message().contains("started XRF002-C"));
        let completed = registry.shutdown_all().await;
        assert_eq!(completed.len(), 1);
        assert!(completed.iter().all(|entry| entry.result.is_ok()));
        assert_eq!(registry.running_len(), 0);
        Ok(())
    }

    #[tokio::test]
    async fn cancelled_retirement_keeps_handle_and_prevents_replacement() -> TestResult {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("stargazer.toml");
        config(&path, directory.path(), "127.0.0.1", 1, "XRF001")?;
        let mut registry = Coordinator::new(
            Config::load(&path)?,
            path,
            Arc::new(Writer::new(directory.path().to_path_buf(), false)),
        )?;
        let target = registry.config.targets.first().ok_or("target")?.clone();
        let key = target.key();
        let (release, released) = oneshot::channel();
        let (shutdown, _receiver) = watch::channel(false);
        let join = tokio::spawn(async move {
            let _released = released.await;
            SupervisorSummary::default()
        });
        let _previous = registry.running.insert(
            key.clone(),
            RunningTarget {
                target,
                shutdown,
                join,
                retiring: false,
            },
        );
        assert!(
            tokio::time::timeout(
                Duration::from_millis(1),
                registry.handle(Command::Disable(key.clone()))
            )
            .await
            .is_err()
        );
        assert!(registry.is_retiring(&key));
        let response = registry.handle(Command::Enable(key.clone())).await;
        assert!(response.message().contains("waiting for previous"));
        assert_eq!(registry.running_len(), 1);
        let mut shutdown = Box::pin(registry.shutdown_all());
        assert!(
            tokio::time::timeout(Duration::from_millis(1), &mut shutdown)
                .await
                .is_err()
        );
        drop(shutdown);
        assert_eq!(registry.running_len(), 1);
        let _delivered = release.send(());
        let completed = registry.shutdown_all().await;
        assert_eq!(completed.len(), 1);
        assert!(completed.first().ok_or("completion")?.result.is_ok());
        assert_eq!(registry.running_len(), 0);
        Ok(())
    }

    #[tokio::test]
    async fn cancelled_shutdown_retains_already_joined_outcomes() -> TestResult {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("stargazer.toml");
        config(&path, directory.path(), "127.0.0.1", 1, "XRF001")?;
        let mut registry = Coordinator::new(
            Config::load(&path)?,
            path,
            Arc::new(Writer::new(directory.path().to_path_buf(), false)),
        )?;
        let first = registry.config.targets.first().ok_or("target")?.clone();
        let second = Target::new(
            crate::config::ReflectorName::new("XRF002")?,
            first.protocol(),
            first.host().clone(),
            first.port(),
            first.module(),
        )?;
        let (release, released) = oneshot::channel();
        let first_join = tokio::spawn(async { SupervisorSummary::default() });
        let second_join = tokio::spawn(async move {
            let _released = released.await;
            SupervisorSummary::default()
        });
        let (first_stop, _first_receiver) = watch::channel(false);
        let (second_stop, _second_receiver) = watch::channel(false);
        let _old = registry.running.insert(
            first.key(),
            RunningTarget {
                target: first,
                shutdown: first_stop,
                join: first_join,
                retiring: false,
            },
        );
        let _old = registry.running.insert(
            second.key(),
            RunningTarget {
                target: second,
                shutdown: second_stop,
                join: second_join,
                retiring: false,
            },
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(10), registry.shutdown_all())
                .await
                .is_err()
        );
        assert_eq!(registry.completions.len(), 1);
        assert_eq!(registry.running_len(), 1);
        let _delivered = release.send(());
        let completed = registry.shutdown_all().await;
        assert_eq!(
            completed.len(),
            2,
            "cancellation must preserve outcomes already joined before another task completes"
        );
        Ok(())
    }

    #[test]
    fn disabled_set_roundtrips_and_rejects_malformed_input() -> TestResult {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join(DISABLED_FILE);
        assert!(load_disabled(&path)?.is_empty());
        let disabled = ["REF030-C".parse()?, "REF068-B".parse()?]
            .into_iter()
            .collect();
        save_disabled(&path, &disabled)?;
        assert_eq!(load_disabled(&path)?, disabled);
        save_disabled(&path, &BTreeSet::new())?;
        assert!(load_disabled(&path)?.is_empty());
        std::fs::write(&path, "../..-C\n")?;
        assert!(load_disabled(&path).is_err());
        std::fs::write(&path, "X".repeat(1_000_000))?;
        assert!(load_disabled(&path).is_err());
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn socket_roundtrip_and_occupied_listener_probe_are_bounded() -> TestResult {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        let directory = tempfile::tempdir()?;
        let path = directory.path().join(SOCKET_FILE);
        assert!(!recorder_already_running(&path).await?);
        let listener = tokio::net::UnixListener::bind(&path)?;
        assert!(
            recorder_already_running(&path).await?,
            "an occupied listener need not answer status before excluding a second recorder"
        );
        let (sender, mut receiver) = mpsc::channel::<CommandRequest>(4);
        let server = tokio::spawn(serve_socket(listener, sender));
        let responder = tokio::spawn(async move {
            while let Some(request) = receiver.recv().await {
                let _delivered = request.reply.send(format!("got {:?}", request.command));
            }
        });
        let mut stream = tokio::net::UnixStream::connect(&path).await?;
        stream.write_all(b"disable ref030-c\n").await?;
        let mut response = String::new();
        let _count = stream.read_to_string(&mut response).await?;
        assert!(response.contains("Disable"));
        let mut stream = tokio::net::UnixStream::connect(&path).await?;
        stream.write_all(b"explode\n").await?;
        let mut response = String::new();
        let _count = stream.read_to_string(&mut response).await?;
        assert!(response.contains("usage"));
        let mut stream = tokio::net::UnixStream::connect(&path).await?;
        stream.write_all(&[b'x'; MAX_COMMAND_BYTES + 1]).await?;
        let mut byte = [0];
        assert!(matches!(stream.read(&mut byte).await, Ok(0) | Err(_)));
        responder.abort();
        let _cancelled = responder.await;
        assert!(server.await?.is_ok());
        assert!(!recorder_already_running(&path).await?);
        Ok(())
    }
}
