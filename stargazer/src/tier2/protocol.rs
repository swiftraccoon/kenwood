//! XLX UDP JSON monitor protocol message types.
//!
//! XLX reflectors (xlxd) expose a UDP push-notification interface on port 10001
//! for real-time activity monitoring. The protocol is simple:
//!
//! 1. Client sends `"hello"` as a UDP datagram to the reflector's IP on port
//!    10001.
//! 2. The server responds with **three** separate UDP datagrams, each containing
//!    one JSON object:
//!    - [`MonitorMessage::Reflector`]: reflector identity and available modules.
//!    - [`MonitorMessage::Nodes`]: snapshot of all currently connected nodes.
//!    - [`MonitorMessage::Stations`]: snapshot of recently heard stations.
//! 3. After the initial dump, the server pushes updates as events occur:
//!    - [`MonitorMessage::Nodes`]: whenever a node connects or disconnects.
//!    - [`MonitorMessage::Stations`]: whenever a station is heard.
//!    - [`MonitorMessage::OnAir`]: when a station starts transmitting.
//!    - [`MonitorMessage::OffAir`]: when a station stops transmitting.
//! 4. Client sends `"bye"` to disconnect cleanly.
//!
//! Each UDP datagram contains exactly one complete JSON object (no framing, no
//! length prefix). The maximum node dump is 250 entries per datagram, and the
//! server's update period is approximately 10 seconds.
//!
//! # Parsing strategy
//!
//! Because all messages arrive as untagged JSON objects, [`parse`] attempts to
//! deserialize each shape in a specific order. The order matters because some
//! shapes are subsets of others — for example, a `{"nodes":[...]}` object would
//! match both the nodes shape and a hypothetical catch-all. The parse order is:
//!
//! 1. `OnAir` / `OffAir` — smallest, most distinctive keys.
//! 2. `Reflector` — has the unique `"reflector"` + `"modules"` combination.
//! 3. `Nodes` — has the `"nodes"` array key.
//! 4. `Stations` — has the `"stations"` array key.
//! 5. `Unknown` — fallback for unrecognized messages (logged, not fatal).

use serde::Deserialize;

/// Identity and module list for a reflector.
///
/// Sent once as the first message after connecting. The `reflector` field is
/// the reflector's callsign (e.g. `"XLX039  "` with padding) and `modules`
/// lists which module letters (A-Z) are available.
///
/// Example JSON:
/// ```json
/// {"reflector":"XLX039  ","modules":["A","B","C","D","E"]}
/// ```
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ReflectorInfo {
    /// Reflector callsign, possibly right-padded with spaces.
    pub(crate) reflector: String,

    /// Available module letters (e.g. `["A", "B", "C"]`).
    pub(crate) modules: Vec<String>,
}

/// A single connected node entry from the `"nodes"` array.
///
/// Represents a gateway or hotspot currently linked to the reflector. The
/// `callsign` includes the module suffix (e.g. `"W1AW  B"` — the trailing
/// letter is the node's local module). The `linkedto` field indicates which
/// reflector module the node is linked to.
///
/// The JSON payload also carries `module` (the node's local module letter —
/// redundant with the suffix of `callsign`) and `time` (a server-locale
/// human-readable timestamp). These are ignored by serde because they do
/// not drive any business logic; the upstream xlxd code treats them as
/// display-only, and the `connected_nodes` table stores its own normalized
/// timestamps populated by the monitor.
///
/// Example JSON element:
/// ```json
/// {"callsign":"W1AW  B","module":"B","linkedto":"A","time":"Tuesday Nov 17..."}
/// ```
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct NodeInfo {
    /// Node callsign with module suffix (e.g. `"W1AW  B"`).
    pub(crate) callsign: String,

    /// Reflector module letter the node is linked to (e.g. `"A"`).
    pub(crate) linkedto: String,
}

/// A single heard station entry from the `"stations"` array.
///
/// Represents an operator who was recently heard transmitting through a node
/// linked to the reflector. The JSON payload also carries `node` (the relaying
/// gateway callsign) and `time` (a server-locale human-readable timestamp).
/// These are ignored by serde because activity is tracked via the reflector +
/// module + callsign triple, and timestamps are normalized to `Utc::now()`
/// when the observation is inserted into `activity_log`.
///
/// Example JSON element:
/// ```json
/// {"callsign":"W1AW    ","node":"W1AW  B ","module":"B","time":"Tuesday Nov 17..."}
/// ```
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct StationInfo {
    /// Operator callsign, possibly right-padded with spaces.
    pub(crate) callsign: String,

    /// Reflector module letter where the station was heard.
    pub(crate) module: String,
}

/// A parsed XLX monitor protocol message.
///
/// Each variant corresponds to one of the JSON object shapes the xlxd monitor
/// protocol can produce. See the [module-level documentation](self) for the
/// full protocol description and parse ordering rationale.
#[derive(Debug, Clone)]
pub(crate) enum MonitorMessage {
    /// Reflector identity and available modules.
    ///
    /// Sent once immediately after the initial `"hello"` handshake.
    Reflector(ReflectorInfo),

    /// Snapshot or update of connected nodes.
    ///
    /// Sent once during the initial handshake (full snapshot), then again
    /// whenever a node connects or disconnects (incremental update with the
    /// full current list).
    Nodes(Vec<NodeInfo>),

    /// Snapshot or update of recently heard stations.
    ///
    /// Sent once during the initial handshake, then again whenever a station
    /// is heard.
    Stations(Vec<StationInfo>),

    /// A station has started transmitting (keyed up).
    ///
    /// The contained string is the operator callsign (possibly padded).
    OnAir(String),

    /// A station has stopped transmitting (unkeyed).
    ///
    /// The contained string is the operator callsign (possibly padded).
    OffAir(String),

    /// An unrecognized JSON message.
    ///
    /// Logged for diagnostic purposes but not processed further. This
    /// accommodates future protocol extensions without breaking the client.
    Unknown(String),
}

// ---------------------------------------------------------------------------
// Internal serde helper structs for untagged deserialization.
//
// Because the XLX monitor protocol uses untagged JSON objects (no
// discriminator field), we attempt to deserialize each shape independently.
// These helper structs exist solely for serde; the public API uses
// `MonitorMessage`.
// ---------------------------------------------------------------------------

/// Helper for `{"onair":"CALLSIGN"}`.
#[derive(Deserialize)]
struct OnAirMsg {
    onair: String,
}

/// Helper for `{"offair":"CALLSIGN"}`.
#[derive(Deserialize)]
struct OffAirMsg {
    offair: String,
}

/// Helper for `{"reflector":"...","modules":[...]}`.
#[derive(Deserialize)]
struct ReflectorMsg {
    reflector: String,
    modules: Vec<String>,
}

/// Helper for `{"nodes":[...]}`.
#[derive(Deserialize)]
struct NodesMsg {
    nodes: Vec<NodeInfo>,
}

/// Helper for `{"stations":[...]}`.
#[derive(Deserialize)]
struct StationsMsg {
    stations: Vec<StationInfo>,
}

/// Attempts to parse a raw UDP datagram payload into a [`MonitorMessage`].
///
/// Returns `Some(message)` on successful parse, or `None` if the data is not
/// valid UTF-8 or not valid JSON (e.g., a stray/corrupt datagram).
///
/// # Parse order
///
/// The shapes are tried in a deliberate order to avoid ambiguous matches:
///
/// 1. **`OnAir`** — Tiny single-key object `{"onair":"..."}`. Tried first
///    because it is the most common real-time event during active transmissions
///    and is unambiguous (unique key name).
///
/// 2. **`OffAir`** — Tiny single-key object `{"offair":"..."}`. Same rationale
///    as `OnAir`.
///
/// 3. **`Reflector`** — Two-key object `{"reflector":"...","modules":[...]}`.
///    The key combination is unique, and this is only sent once per session, so
///    false positives are not a concern.
///
/// 4. **`Nodes`** — Single-key object `{"nodes":[...]}` containing an array of
///    node entries. Tried before `Stations` because node updates are more
///    frequent than station updates in practice.
///
/// 5. **`Stations`** — Single-key object `{"stations":[...]}` containing an
///    array of station entries.
///
/// 6. **`Unknown`** — If none of the above match, the raw JSON text is wrapped
///    in `Unknown` for diagnostic logging. This ensures forward compatibility
///    with potential protocol extensions.
pub(crate) fn parse(data: &[u8]) -> Option<MonitorMessage> {
    // The XLX monitor protocol uses UTF-8 JSON. Reject non-UTF-8 immediately.
    let text = std::str::from_utf8(data).ok()?;

    // Try OnAir first — smallest, most frequent real-time event.
    if let Ok(msg) = serde_json::from_str::<OnAirMsg>(text) {
        return Some(MonitorMessage::OnAir(msg.onair));
    }

    // Try OffAir — same rationale as OnAir.
    if let Ok(msg) = serde_json::from_str::<OffAirMsg>(text) {
        return Some(MonitorMessage::OffAir(msg.offair));
    }

    // Try Reflector — unique two-key shape, sent once per session.
    if let Ok(msg) = serde_json::from_str::<ReflectorMsg>(text) {
        return Some(MonitorMessage::Reflector(ReflectorInfo {
            reflector: msg.reflector,
            modules: msg.modules,
        }));
    }

    // Try Nodes — array of connected node entries.
    if let Ok(msg) = serde_json::from_str::<NodesMsg>(text) {
        return Some(MonitorMessage::Nodes(msg.nodes));
    }

    // Try Stations — array of heard station entries.
    if let Ok(msg) = serde_json::from_str::<StationsMsg>(text) {
        return Some(MonitorMessage::Stations(msg.stations));
    }

    // Unrecognized JSON — log the raw text for diagnostics. This preserves
    // forward compatibility: new message types from future xlxd versions
    // won't crash the client.
    Some(MonitorMessage::Unknown(text.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_onair_message() {
        let data = br#"{"onair":"W1AW    "}"#;
        let msg = parse(data);
        assert!(
            matches!(&msg, Some(MonitorMessage::OnAir(cs)) if cs == "W1AW    "),
            "expected OnAir, got {msg:?}"
        );
    }

    #[test]
    fn parse_offair_message() {
        let data = br#"{"offair":"W1AW    "}"#;
        let msg = parse(data);
        assert!(
            matches!(&msg, Some(MonitorMessage::OffAir(cs)) if cs == "W1AW    "),
            "expected OffAir, got {msg:?}"
        );
    }

    #[test]
    fn parse_reflector_message() {
        let data = br#"{"reflector":"XLX039  ","modules":["A","B","C"]}"#;
        let msg = parse(data);
        assert!(
            matches!(&msg, Some(MonitorMessage::Reflector(info)) if info.reflector == "XLX039  " && info.modules.len() == 3),
            "expected Reflector, got {msg:?}"
        );
    }

    #[test]
    fn parse_nodes_message() {
        let data = br#"{"nodes":[{"callsign":"W1AW  B","module":"B","linkedto":"A","time":"Tue Nov 17"}]}"#;
        let msg = parse(data);
        assert!(
            matches!(&msg, Some(MonitorMessage::Nodes(nodes)) if nodes.len() == 1),
            "expected Nodes, got {msg:?}"
        );
    }

    #[test]
    fn parse_stations_message() {
        let data = br#"{"stations":[{"callsign":"W1AW    ","node":"W1AW  B ","module":"B","time":"Tue Nov 17"}]}"#;
        let msg = parse(data);
        assert!(
            matches!(&msg, Some(MonitorMessage::Stations(stations)) if stations.len() == 1),
            "expected Stations, got {msg:?}"
        );
    }

    #[test]
    fn parse_empty_nodes_array() {
        let data = br#"{"nodes":[]}"#;
        let msg = parse(data);
        assert!(
            matches!(&msg, Some(MonitorMessage::Nodes(nodes)) if nodes.is_empty()),
            "expected empty Nodes, got {msg:?}"
        );
    }

    #[test]
    fn parse_unknown_message() {
        let data = br#"{"something":"unexpected"}"#;
        let msg = parse(data);
        assert!(
            matches!(&msg, Some(MonitorMessage::Unknown(_))),
            "expected Unknown, got {msg:?}"
        );
    }

    #[test]
    fn parse_invalid_utf8_returns_none() {
        let data: &[u8] = &[0xFF, 0xFE, 0xFD];
        let msg = parse(data);
        assert!(
            msg.is_none(),
            "expected None for invalid UTF-8, got {msg:?}"
        );
    }

    #[test]
    fn parse_non_json_utf8_returns_unknown() {
        // Valid UTF-8 but not valid JSON. Falls through all serde attempts
        // and is wrapped in Unknown for forward-compatibility logging.
        let data = b"not json at all";
        let msg = parse(data);
        assert!(
            matches!(&msg, Some(MonitorMessage::Unknown(s)) if s == "not json at all"),
            "expected Unknown, got {msg:?}"
        );
    }
}
