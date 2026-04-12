//! Configuration for the stargazer service.
//!
//! Configuration is loaded from a TOML file and can be overridden by environment
//! variables. The TOML file is divided into sections matching the service tiers:
//!
//! - `[postgres]` — Database connection pool settings.
//! - `[rdio]` — Rdio API upload endpoint and retry policy.
//! - `[tier1]` — Discovery sweep intervals for Pi-Star, XLX API, and ircDDB.
//! - `[tier2]` — XLX UDP JSON monitor concurrency and idle thresholds.
//! - `[tier3]` — Deep D-STAR protocol connections for voice capture.
//! - `[audio]` — MP3 encoding parameters.
//! - `[server]` — HTTP API bind address.
//!
//! Environment variable overrides follow the `STARGAZER_SECTION_FIELD` pattern:
//!
//! | Variable | Overrides |
//! |----------|-----------|
//! | `STARGAZER_POSTGRES_URL` | `postgres.url` |
//! | `STARGAZER_RDIO_ENDPOINT` | `rdio.endpoint` |
//! | `STARGAZER_RDIO_API_KEY` | `rdio.api_key` |
//! | `STARGAZER_TIER3_DPLUS_CALLSIGN` | `tier3.dplus_callsign` |
//! | `STARGAZER_SERVER_LISTEN` | `server.listen` |

use std::net::SocketAddr;
use std::path::Path;

use serde::Deserialize;

/// Top-level configuration for the stargazer service.
///
/// Loaded from a TOML file via [`load`], with selective environment variable
/// overrides applied afterwards.
#[derive(Debug, Deserialize)]
pub(crate) struct Config {
    /// `PostgreSQL` connection pool configuration.
    #[serde(default)]
    pub(crate) postgres: PostgresConfig,

    /// Rdio API upload endpoint configuration.
    #[serde(default)]
    pub(crate) rdio: RdioConfig,

    /// Tier 1: discovery and sweep configuration.
    #[serde(default)]
    pub(crate) tier1: Tier1Config,

    /// Tier 2: XLX live monitoring configuration.
    #[serde(default)]
    pub(crate) tier2: Tier2Config,

    /// Tier 3: deep connect and voice recording configuration.
    #[serde(default)]
    pub(crate) tier3: Tier3Config,

    /// Audio encoding configuration.
    #[serde(default)]
    pub(crate) audio: AudioConfig,

    /// HTTP API server configuration.
    #[serde(default)]
    pub(crate) server: ServerConfig,
}

/// `PostgreSQL` connection pool settings.
///
/// The `url` field is the `libpq`-style connection string. The pool size
/// controls how many connections sqlx keeps open concurrently.
#[derive(Debug, Deserialize)]
pub(crate) struct PostgresConfig {
    /// `PostgreSQL` connection URL.
    ///
    /// Default: `"postgres://stargazer:pass@localhost/stargazer"`
    #[serde(default = "default_postgres_url")]
    pub(crate) url: String,

    /// Maximum number of connections in the pool.
    ///
    /// Default: `10`
    #[serde(default = "default_max_connections")]
    pub(crate) max_connections: u32,
}

impl Default for PostgresConfig {
    fn default() -> Self {
        Self {
            url: default_postgres_url(),
            max_connections: default_max_connections(),
        }
    }
}

/// Rdio API upload configuration.
///
/// Stargazer uploads completed voice streams to an `SDRTrunk`-compatible Rdio
/// API server using the `POST /api/call-upload` multipart protocol.
#[derive(Debug, Deserialize)]
pub(crate) struct RdioConfig {
    /// Full URL of the Rdio API call-upload endpoint.
    ///
    /// Default: `"http://rdio-api:8080/api/call-upload"`
    #[serde(default = "default_rdio_endpoint")]
    pub(crate) endpoint: String,

    /// API key sent with each upload as the `key` form field.
    ///
    /// Default: `"stargazer-key"`
    #[serde(default = "default_rdio_api_key")]
    pub(crate) api_key: String,

    /// Seconds between upload retry attempts for failed streams.
    ///
    /// Default: `30`
    #[serde(default = "default_retry_interval_secs")]
    pub(crate) retry_interval_secs: u64,

    /// Maximum number of upload attempts before marking a stream as failed.
    ///
    /// Default: `10`
    #[serde(default = "default_max_retries")]
    pub(crate) max_retries: u32,
}

impl Default for RdioConfig {
    fn default() -> Self {
        Self {
            endpoint: default_rdio_endpoint(),
            api_key: default_rdio_api_key(),
            retry_interval_secs: default_retry_interval_secs(),
            max_retries: default_max_retries(),
        }
    }
}

/// Tier 1: discovery sweep configuration.
///
/// Controls how often each external data source is polled to build the
/// reflector registry.
#[derive(Debug, Deserialize)]
pub(crate) struct Tier1Config {
    /// Poll interval for Pi-Star host files, in seconds.
    ///
    /// The Pi-Star host file changes rarely; daily polling is sufficient.
    ///
    /// Default: `86400` (24 hours)
    #[serde(default = "default_pistar")]
    pub(crate) pistar: u64,

    /// Poll interval for XLX API reflector-list, in seconds.
    ///
    /// Default: `600` (10 minutes)
    #[serde(default = "default_xlx_api")]
    pub(crate) xlx_api: u64,

    /// Poll interval for ircDDB last-heard page scrapes, in seconds.
    ///
    /// Default: `60` (1 minute)
    #[serde(default = "default_ircddb")]
    pub(crate) ircddb: u64,
}

impl Default for Tier1Config {
    fn default() -> Self {
        Self {
            pistar: default_pistar(),
            xlx_api: default_xlx_api(),
            ircddb: default_ircddb(),
        }
    }
}

/// Tier 2: XLX live monitoring configuration.
///
/// Controls how many XLX reflectors are monitored concurrently via the UDP
/// JSON monitor protocol (port 10001) and when idle monitors are disconnected.
#[derive(Debug, Deserialize)]
pub(crate) struct Tier2Config {
    /// Maximum number of concurrent UDP JSON monitor connections.
    ///
    /// Default: `100`
    #[serde(default = "default_max_concurrent_monitors")]
    pub(crate) max_concurrent_monitors: usize,

    /// Seconds of inactivity before disconnecting a Tier 2 monitor.
    ///
    /// Default: `600` (10 minutes)
    #[serde(default = "default_tier2_idle_disconnect_secs")]
    pub(crate) idle_disconnect_secs: u64,

    /// Seconds of recent activity required to consider a reflector "active"
    /// and eligible for Tier 2 monitoring.
    ///
    /// Default: `1800` (30 minutes)
    #[serde(default = "default_activity_threshold_secs")]
    pub(crate) activity_threshold_secs: u64,
}

impl Default for Tier2Config {
    fn default() -> Self {
        Self {
            max_concurrent_monitors: default_max_concurrent_monitors(),
            idle_disconnect_secs: default_tier2_idle_disconnect_secs(),
            activity_threshold_secs: default_activity_threshold_secs(),
        }
    }
}

/// Tier 3: deep D-STAR protocol connection configuration.
///
/// Controls how many reflectors are simultaneously connected at the D-STAR
/// protocol level for voice capture, and the callsign used for `DPlus`
/// authentication.
#[derive(Debug, Deserialize)]
pub(crate) struct Tier3Config {
    /// Maximum number of concurrent D-STAR protocol connections.
    ///
    /// Default: `20`
    #[serde(default = "default_max_concurrent_connections")]
    pub(crate) max_concurrent_connections: usize,

    /// Seconds of silence before disconnecting a Tier 3 session.
    ///
    /// Default: `300` (5 minutes)
    #[serde(default = "default_tier3_idle_disconnect_secs")]
    pub(crate) idle_disconnect_secs: u64,

    /// Whether Tier 2 activity automatically promotes reflectors to Tier 3.
    ///
    /// Default: `true`
    #[serde(default = "default_auto_promote")]
    pub(crate) auto_promote: bool,

    /// Callsign used for `DPlus` authentication with `auth.dstargateway.org`.
    ///
    /// Must be a valid amateur radio callsign registered with the `DPlus`
    /// gateway trust system.
    ///
    /// Default: `"N0CALL"`
    #[serde(default = "default_dplus_callsign")]
    pub(crate) dplus_callsign: String,
}

impl Default for Tier3Config {
    fn default() -> Self {
        Self {
            max_concurrent_connections: default_max_concurrent_connections(),
            idle_disconnect_secs: default_tier3_idle_disconnect_secs(),
            auto_promote: default_auto_promote(),
            dplus_callsign: default_dplus_callsign(),
        }
    }
}

/// Audio encoding configuration.
///
/// Controls the output format and quality of decoded voice streams.
#[derive(Debug, Deserialize)]
pub(crate) struct AudioConfig {
    /// Audio output format. Currently only `"mp3"` is supported.
    ///
    /// Default: `"mp3"`
    #[serde(default = "default_audio_format")]
    pub(crate) format: String,

    /// MP3 constant bitrate in kbps.
    ///
    /// D-STAR voice is narrow-band (8 kHz sample rate); 64 kbps provides
    /// good quality without excessive file size.
    ///
    /// Default: `64`
    #[serde(default = "default_mp3_bitrate")]
    pub(crate) mp3_bitrate: u32,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            format: default_audio_format(),
            mp3_bitrate: default_mp3_bitrate(),
        }
    }
}

/// HTTP API server configuration.
#[derive(Debug, Deserialize)]
pub(crate) struct ServerConfig {
    /// Socket address to bind the HTTP API server to.
    ///
    /// Default: `"0.0.0.0:8080"`
    #[serde(default = "default_listen")]
    pub(crate) listen: String,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            listen: default_listen(),
        }
    }
}

// ---------------------------------------------------------------------------
// Default value functions for serde
// ---------------------------------------------------------------------------

fn default_postgres_url() -> String {
    String::from("postgres://stargazer:pass@localhost/stargazer")
}

const fn default_max_connections() -> u32 {
    10
}

fn default_rdio_endpoint() -> String {
    String::from("http://rdio-api:8080/api/call-upload")
}

fn default_rdio_api_key() -> String {
    String::from("stargazer-key")
}

const fn default_retry_interval_secs() -> u64 {
    30
}

const fn default_max_retries() -> u32 {
    10
}

const fn default_pistar() -> u64 {
    86400
}

const fn default_xlx_api() -> u64 {
    600
}

const fn default_ircddb() -> u64 {
    60
}

const fn default_max_concurrent_monitors() -> usize {
    100
}

const fn default_tier2_idle_disconnect_secs() -> u64 {
    600
}

const fn default_activity_threshold_secs() -> u64 {
    1800
}

const fn default_max_concurrent_connections() -> usize {
    20
}

const fn default_tier3_idle_disconnect_secs() -> u64 {
    300
}

const fn default_auto_promote() -> bool {
    true
}

fn default_dplus_callsign() -> String {
    String::from("N0CALL")
}

fn default_audio_format() -> String {
    String::from("mp3")
}

const fn default_mp3_bitrate() -> u32 {
    64
}

fn default_listen() -> String {
    String::from("0.0.0.0:8080")
}

/// Loads configuration from a TOML file, then applies environment variable
/// overrides.
///
/// # Environment variable overrides
///
/// The following environment variables, when set, override the corresponding
/// TOML fields:
///
/// - `STARGAZER_POSTGRES_URL` overrides `postgres.url`
/// - `STARGAZER_RDIO_ENDPOINT` overrides `rdio.endpoint`
/// - `STARGAZER_RDIO_API_KEY` overrides `rdio.api_key`
/// - `STARGAZER_TIER3_DPLUS_CALLSIGN` overrides `tier3.dplus_callsign`
/// - `STARGAZER_SERVER_LISTEN` overrides `server.listen`
///
/// # Errors
///
/// Returns an error if the file cannot be read or contains invalid TOML, or
/// if the `STARGAZER_SERVER_LISTEN` environment variable contains an invalid
/// socket address.
pub(crate) fn load(path: &Path) -> Result<Config, Box<dyn std::error::Error>> {
    let contents = std::fs::read_to_string(path)?;
    let mut config: Config = toml::from_str(&contents)?;

    // Apply environment variable overrides.
    if let Ok(val) = std::env::var("STARGAZER_POSTGRES_URL") {
        config.postgres.url = val;
    }
    if let Ok(val) = std::env::var("STARGAZER_RDIO_ENDPOINT") {
        config.rdio.endpoint = val;
    }
    if let Ok(val) = std::env::var("STARGAZER_RDIO_API_KEY") {
        config.rdio.api_key = val;
    }
    if let Ok(val) = std::env::var("STARGAZER_TIER3_DPLUS_CALLSIGN") {
        config.tier3.dplus_callsign = val;
    }
    if let Ok(val) = std::env::var("STARGAZER_SERVER_LISTEN") {
        // Validate the override is a valid socket address early.
        let _: SocketAddr = val.parse()?;
        config.server.listen = val;
    }

    Ok(config)
}
