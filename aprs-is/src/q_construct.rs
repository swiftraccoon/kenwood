//! APRS-IS Q-construct classification and `IGate` path rewriting.
//!
//! The Q-construct is a single ASCII token, beginning with `qA`, that
//! every packet carries in its APRS-IS path. It records *how* the
//! packet entered the network — directly from RF via a verified `IGate`,
//! from a server peer, from a client app, and so on. APRS-IS servers
//! propagate the construct unchanged; the originating station (`IGate`
//! or client) inserts it.
//!
//! This module exposes:
//!
//! - The [`QConstruct`] enum (all nine spec values).
//! - [`format_is_packet_with_qconstruct`] — a *naive* line builder
//!   that simply appends `,qXX,GATE` to a path. Suitable when the
//!   caller has already computed the correct construct and gate
//!   callsign; not a full `IGate`.
//! - [`igate_format_for_is`] — a *strict*, append-only `IGate` path
//!   rewriter that implements the <http://www.aprs-is.net/q.aspx> and
//!   <http://www.aprs-is.net/IGating.aspx> algorithm: refuses to gate
//!   when the sender opted out, then gates the heard path **verbatim**
//!   (every hop preserved, including its has-been-repeated `*` marker)
//!   and appends exactly `,<qconstruct>,<IGATECALL>` to the end. The
//!   q-construct is `qAR` for a verified login and `qAO` for a
//!   receive-only login. IGating.aspx is explicit: "No modification of
//!   the TNC2 format line should be made except to add ,qAR,IGATECALL
//!   to the end of the path."
//!
//! Earlier versions of this crate advertised "`IGate` path rewriting"
//! while shipping only the naive append helper. The strict rewriter
//! lives in [`igate_format_for_is`].

use std::borrow::Cow;

use ax25_codec::{Ax25Address, RouteEntry};

use crate::line::format_is_packet;
use crate::login::Passcode;

/// APRS-IS Q-construct tag (path identifier that records how a packet
/// entered the APRS-IS network).
///
/// Per <http://www.aprs-is.net/q.aspx>, every packet seen by an APRS-IS
/// server has exactly one Q-construct inserted into its path. Servers
/// that relay packets propagate the construct unchanged; servers that
/// originate packets add one based on the packet's source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum QConstruct {
    /// `qAC` — client-owned, server verified the login.
    QAC,
    /// `qAX` — client-owned, server did *not* verify the login.
    QAX,
    /// `qAU` — client-owned, received via UDP submit.
    QAU,
    /// `qAo` — server-owned, received from a different server.
    QAo,
    /// `qAO` — server-owned, originated on RF (`IGATE`).
    QAO,
    /// `qAS` — server-owned, received from a peer.
    QAS,
    /// `qAr` — gated from RF with no callsign substitution.
    QAr,
    /// `qAR` — gated from RF by a verified login.
    QAR,
    /// `qAZ` — server-client command packet. The packet is generated
    /// by the server, client, or `IGate` and must not be propagated
    /// further.
    QAZ,
    /// `qAI` — trace packet. Each server adds login identification
    /// (this construct + originating server callsign) so the packet's
    /// network path can be reconstructed. Defined in the q.aspx
    /// Client Generated table.
    QAI,
}

impl QConstruct {
    /// Wire form of the construct (the exact 3-character token inserted
    /// into the APRS path).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::QAC => "qAC",
            Self::QAX => "qAX",
            Self::QAU => "qAU",
            Self::QAo => "qAo",
            Self::QAO => "qAO",
            Self::QAS => "qAS",
            Self::QAr => "qAr",
            Self::QAR => "qAR",
            Self::QAZ => "qAZ",
            Self::QAI => "qAI",
        }
    }

    /// Parse a path element as a Q-construct if it matches one of the
    /// well-known forms. Returns `None` otherwise.
    ///
    /// The leading `*` (has-been-repeated) marker, if present, is
    /// trimmed before matching — a path element like `qAR*` decodes to
    /// `Some(QAR)`. Per q.aspx the construct should never carry that
    /// marker on the wire, but tolerant parsing keeps the rewriter
    /// robust against malformed upstream input.
    #[must_use]
    pub fn from_path_element(s: &str) -> Option<Self> {
        let s = s.strip_suffix('*').unwrap_or(s);
        match s {
            "qAC" => Some(Self::QAC),
            "qAX" => Some(Self::QAX),
            "qAU" => Some(Self::QAU),
            "qAo" => Some(Self::QAo),
            "qAO" => Some(Self::QAO),
            "qAS" => Some(Self::QAS),
            "qAr" => Some(Self::QAr),
            "qAR" => Some(Self::QAR),
            "qAZ" => Some(Self::QAZ),
            "qAI" => Some(Self::QAI),
            _ => None,
        }
    }
}

impl std::fmt::Display for QConstruct {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Format an APRS-IS packet with an explicit Q-construct appended.
///
/// Injects the Q-construct + gate callsign immediately after `path`,
/// producing `source>destination,path,qXX,gate:data\r\n`.
///
/// # Scope (and what this is **not**)
///
/// This is a *naive* line builder for callers that have already
/// computed the correct construct externally. It does not:
///
/// - validate the packet against `IGate` gating rules
///   (`NOGATE` / `RFONLY` / `TCPIP` / `TCPXX`),
/// - strip has-been-repeated (`*`) markers from the path,
/// - remove an existing q-construct from `path` before appending,
/// - pick `qAR` vs `qAr` from the login verification state.
///
/// Use [`igate_format_for_is`] when you need the full
/// <http://www.aprs-is.net/q.aspx> rewriting algorithm.
#[must_use]
pub fn format_is_packet_with_qconstruct(
    source: &str,
    destination: &str,
    path: &[&str],
    qconstruct: QConstruct,
    gate_callsign: &str,
    data: &str,
) -> String {
    let mut packet = format!("{source}>{destination}");
    for p in path {
        packet.push(',');
        packet.push_str(p);
    }
    packet.push(',');
    packet.push_str(qconstruct.as_str());
    packet.push(',');
    packet.push_str(gate_callsign);
    packet.push(':');
    packet.push_str(data);
    packet.push_str("\r\n");
    packet
}

// ---------------------------------------------------------------------------
// IGate path rewriter (q.aspx)
// ---------------------------------------------------------------------------

/// Reasons an `IGate` refuses to gate an RF packet into APRS-IS.
///
/// Each variant corresponds to a specific gating rule from
/// <http://www.aprs-is.net/IGating.aspx>. Returned by
/// [`igate_format_for_is`] so the caller can log *why* the packet was
/// suppressed rather than silently dropping it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum IGateError {
    /// The source callsign is `TCPIP` or `TCPXX`, indicating the packet
    /// originated from APRS-IS — gating would create a loop.
    SourceIsInternet,
    /// The path contains a `NOGATE` element: the originator opted out
    /// of gating.
    PathBlocksGating,
    /// The path contains an `RFONLY` element: same intent as
    /// `NOGATE` but explicitly RF-only.
    PathIsRfOnly,
    /// The path already contains `TCPIP` or `TCPXX`: the packet has
    /// already been gated and gating again would create a loop.
    PathAlreadyGated,
    /// The `IGate`'s own callsign appears in the path — the packet has
    /// already visited this station and re-gating would loop.
    LoopDetected,
    /// The packet's info field begins with `}` (third-party header),
    /// which per APRS 1.0.1 §17 means it has already been gated once.
    ThirdPartyPacket,
}

impl std::fmt::Display for IGateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let reason = match self {
            Self::SourceIsInternet => "source callsign is TCPIP/TCPXX (packet came from APRS-IS)",
            Self::PathBlocksGating => "path contains NOGATE marker",
            Self::PathIsRfOnly => "path contains RFONLY marker",
            Self::PathAlreadyGated => "path already contains TCPIP/TCPXX",
            Self::LoopDetected => "IGate's callsign is in the path (loop)",
            Self::ThirdPartyPacket => "info field begins with '}' (third-party packet)",
        };
        f.write_str(reason)
    }
}

impl std::error::Error for IGateError {}

/// Pick the Q-construct to insert when gating an RF-heard packet into
/// APRS-IS, per the "Client Generated" table at
/// <http://www.aprs-is.net/q.aspx>.
///
/// The choice depends solely on the `IGate`'s login state:
///
/// - A verified login (real callsign + valid passcode) uses
///   [`QConstruct::QAR`] — q.aspx: "qAR - Gated packet from RF. Packet
///   is placed on APRS-IS by an `IGate` from RF."
/// - A receive-only login uses [`QConstruct::QAO`] (capital letter O) —
///   q.aspx: "qAO - (letter O) Gated packet from RF without messaging…
///   Receive-only `IGates` will use this exclusively for all packets
///   gated to APRS-IS."
///
/// Note: `qAr` (lowercase r) and `qAo` (lowercase o) are *server-side*
/// constructs for packets that arrived **indirectly** via the legacy
/// `,I` path from a remote `IGate` (q.aspx "Server Generated" table) —
/// they are never inserted by the `IGate` that heard the packet on RF,
/// so they are not selectable here. The other constructs (`qAS`,
/// `qAC`, `qAX`, `qAU`, `qAZ`, `qAI`) are likewise not applicable to an
/// `IGate` placing RF traffic onto APRS-IS.
#[must_use]
const fn qconstruct_for_igate(passcode: Passcode) -> QConstruct {
    match passcode {
        Passcode::Verified(_) => QConstruct::QAR,
        Passcode::ReceiveOnly => QConstruct::QAO,
    }
}

/// Format an APRS-IS line by rewriting an RF-heard packet's path per
/// the canonical `IGate` algorithm at <http://www.aprs-is.net/q.aspx>
/// and <http://www.aprs-is.net/IGating.aspx>.
///
/// # Algorithm
///
/// 1. **Refuse to gate** if any of the following apply:
///    - `source.callsign` is `TCPIP` or `TCPXX`
///      ([`IGateError::SourceIsInternet`]),
///    - `info` begins with `}` (third-party packet)
///      ([`IGateError::ThirdPartyPacket`]),
///    - `rf_path` contains `NOGATE` ([`IGateError::PathBlocksGating`]),
///    - `rf_path` contains `RFONLY` ([`IGateError::PathIsRfOnly`]),
///    - `rf_path` contains `TCPIP` or `TCPXX`
///      ([`IGateError::PathAlreadyGated`]),
///    - `rf_path` contains the `IGate`'s own callsign
///      ([`IGateError::LoopDetected`]).
/// 2. **Gate the heard path verbatim.** Every hop in `rf_path` is
///    preserved exactly as heard, **including its trailing `*`
///    has-been-repeated marker**. IGating.aspx is explicit: "No
///    modification of the TNC2 format line should be made except to add
///    ,qAR,IGATECALL to the end of the path." q.aspx's RF-gating
///    example confirms the marker survives — `AE5PL>APRS,WIDE1*` gates
///    to `AE5PL>APRS,WIDE1*,qAR,AE5PL-10`. Unused hops are **not**
///    dropped and `*` is **not** stripped; doing either would mislead
///    receivers about the propagation history and violate the
///    append-only rule.
/// 3. **Append the q-construct + `IGate` callsign** at the end. The
///    construct is [`QConstruct::QAR`] for a verified login,
///    [`QConstruct::QAO`] (letter O) for a receive-only login.
/// 4. **Serialise** as a CRLF-terminated TNC2 line via the existing
///    [`crate::format_is_packet`] helper.
///
/// # Parameters
///
/// - `source`: AX.25 source address of the RF packet.
/// - `destination`: AX.25 destination address (typically an `APxxxx`
///   tocall).
/// - `rf_path`: Digipeater path slots from the RF frame. Each slot is
///   gated verbatim, preserving its has-been-repeated `*` marker.
/// - `info`: Packet info field bytes. Decoded losslessly for the
///   data portion of the IS line; non-UTF-8 bytes become U+FFFD.
/// - `igate`: This `IGate`'s callsign + SSID, as appears on the wire
///   after the q-construct.
/// - `passcode`: The `IGate`'s login state. Verified → `qAR`,
///   receive-only → `qAO` (letter O).
///
/// # Errors
///
/// Returns [`IGateError`] when the packet must not be gated. Callers
/// typically log this and drop the packet rather than treating it as
/// a hard failure.
///
/// # Examples
///
/// ```
/// use ax25_codec::{Ax25Address, RouteEntry};
/// use aprs_is::{Passcode, igate_format_for_is};
///
/// let src = Ax25Address::new("W1AW", 0).unwrap();
/// let dst = Ax25Address::new("APK005", 0).unwrap();
/// let mut wide1 = RouteEntry::new("WIDE1", 1).unwrap();
/// wide1.has_repeated = true;
/// let path = vec![wide1];
/// let igate = Ax25Address::new("N0CALL", 7).unwrap();
/// let pass = Passcode::Verified(12345);
///
/// let line = igate_format_for_is(
///     &src, &dst, &path,
///     b"!4903.50N/07201.75W-",
///     &igate, pass,
/// ).unwrap();
/// // The heard `WIDE1-1*` is preserved verbatim (marker and all);
/// // only `,qAR,N0CALL-7` is appended.
/// assert_eq!(
///     line,
///     "W1AW>APK005,WIDE1-1*,qAR,N0CALL-7:!4903.50N/07201.75W-\r\n",
/// );
/// ```
///
/// # Notes
///
/// - The output line is byte-for-byte ready to send to an APRS-IS
///   server via [`crate::AprsIsClient::send_raw_line`].
/// - This function does **not** modify the input packet; rewriting is
///   pure-data.
pub fn igate_format_for_is(
    source: &Ax25Address,
    destination: &Ax25Address,
    rf_path: &[RouteEntry],
    info: &[u8],
    igate: &Ax25Address,
    passcode: Passcode,
) -> Result<String, IGateError> {
    // Rule 1a: source TCPIP/TCPXX → packet originated from IS.
    let src_call = source.callsign.as_str();
    if src_call == "TCPIP" || src_call == "TCPXX" {
        return Err(IGateError::SourceIsInternet);
    }

    // Rule 1b: third-party header → already gated once.
    if info.first() == Some(&b'}') {
        return Err(IGateError::ThirdPartyPacket);
    }

    // Rule 1c-f: walk the path checking each markers.
    for entry in rf_path {
        let call = entry.address.callsign.as_str();
        match call {
            "NOGATE" => return Err(IGateError::PathBlocksGating),
            "RFONLY" => return Err(IGateError::PathIsRfOnly),
            "TCPIP" | "TCPXX" => return Err(IGateError::PathAlreadyGated),
            _ if entry.address == *igate => return Err(IGateError::LoopDetected),
            _ => {}
        }
    }

    // Gate the heard path verbatim, then append the q-construct +
    // IGate callsign. IGating.aspx: "No modification of the TNC2 format
    // line should be made except to add ,qAR,IGATECALL to the end of
    // the path." Each hop is serialised via `RouteEntry::Display`,
    // which preserves the trailing `*` has-been-repeated marker (the
    // q.aspx example keeps `WIDE1*`). Unused hops are *not* dropped and
    // `*` is *not* stripped. A real RF AX.25 frame never carries a
    // q-construct on the wire (q-constructs only exist on APRS-IS), and
    // `RouteEntry` callsigns are validated uppercase alphanumerics, so
    // `qAR`/`qAr`/… can never appear as a heard hop — no q-construct
    // filtering is needed here.
    let qconstruct = qconstruct_for_igate(passcode);
    let mut path_parts: Vec<String> = Vec::with_capacity(rf_path.len() + 2);
    for entry in rf_path {
        path_parts.push(entry.to_string());
    }
    path_parts.push(qconstruct.as_str().to_owned());
    path_parts.push(igate.to_string());

    // Build the final line. The info field may contain non-UTF-8
    // bytes (Mic-E, raw weather) — preserve them via lossy decode for
    // the IS line; callers that need byte-exact fidelity should use
    // [`format_is_packet`] directly with a pre-decoded `&str`.
    let path_refs: Vec<&str> = path_parts.iter().map(String::as_str).collect();
    let data: Cow<'_, str> = String::from_utf8_lossy(info);
    let source_str = source.to_string();
    let destination_str = destination.to_string();
    Ok(format_is_packet(
        &source_str,
        &destination_str,
        &path_refs,
        &data,
    ))
}

/// Convenience wrapper around [`igate_format_for_is`] for callers that
/// already have a fully-formed `Ax25Packet` and just want the IS line.
///
/// Equivalent to passing `packet.source`, `packet.destination`,
/// `packet.digipeaters`, `packet.info` to [`igate_format_for_is`].
///
/// # Errors
///
/// See [`igate_format_for_is`].
pub fn igate_format_packet_for_is(
    packet: &ax25_codec::Ax25Packet,
    igate: &Ax25Address,
    passcode: Passcode,
) -> Result<String, IGateError> {
    igate_format_for_is(
        &packet.source,
        &packet.destination,
        &packet.digipeaters,
        &packet.info,
        igate,
        passcode,
    )
}

/// Construct the rewritten path elements an `IGate` would emit for an RF
/// packet, **without** building the final TNC2 line.
///
/// Exposed for callers that want to log or inspect the path before
/// serialising. The returned vector contains everything between the
/// `>DEST` and the `:` colon in the IS line: the heard digipeater
/// path verbatim (each hop keeping its `*` has-been-repeated marker),
/// followed by the q-construct and the `IGate` callsign.
///
/// # Errors
///
/// See [`igate_format_for_is`] for the gating-refusal cases.
pub fn igate_rewritten_path(
    source: &Ax25Address,
    rf_path: &[RouteEntry],
    info: &[u8],
    igate: &Ax25Address,
    passcode: Passcode,
) -> Result<Vec<String>, IGateError> {
    let src_call = source.callsign.as_str();
    if src_call == "TCPIP" || src_call == "TCPXX" {
        return Err(IGateError::SourceIsInternet);
    }
    if info.first() == Some(&b'}') {
        return Err(IGateError::ThirdPartyPacket);
    }
    for entry in rf_path {
        let call = entry.address.callsign.as_str();
        match call {
            "NOGATE" => return Err(IGateError::PathBlocksGating),
            "RFONLY" => return Err(IGateError::PathIsRfOnly),
            "TCPIP" | "TCPXX" => return Err(IGateError::PathAlreadyGated),
            _ if entry.address == *igate => return Err(IGateError::LoopDetected),
            _ => {}
        }
    }

    // Append-only: gate the heard path verbatim (preserving each `*`),
    // then add the q-construct + IGate callsign. See
    // `igate_format_for_is` for the IGating.aspx / q.aspx rationale.
    let qconstruct = qconstruct_for_igate(passcode);
    let mut path_parts: Vec<String> = Vec::with_capacity(rf_path.len() + 2);
    for entry in rf_path {
        path_parts.push(entry.to_string());
    }
    path_parts.push(qconstruct.as_str().to_owned());
    path_parts.push(igate.to_string());
    Ok(path_parts)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn addr(call: &str, ssid: u8) -> Ax25Address {
        Ax25Address::new(call, ssid)
            .unwrap_or_else(|_| unreachable!("statically valid callsign in test: {call}-{ssid}"))
    }

    fn used_digi(call: &str, ssid: u8) -> RouteEntry {
        let mut entry = RouteEntry::new(call, ssid)
            .unwrap_or_else(|_| unreachable!("statically valid digi in test: {call}-{ssid}"));
        entry.has_repeated = true;
        entry
    }

    fn unused_digi(call: &str, ssid: u8) -> RouteEntry {
        RouteEntry::new(call, ssid)
            .unwrap_or_else(|_| unreachable!("statically valid digi in test: {call}-{ssid}"))
    }

    #[test]
    fn qconstruct_round_trip() {
        let all = [
            QConstruct::QAC,
            QConstruct::QAX,
            QConstruct::QAU,
            QConstruct::QAo,
            QConstruct::QAO,
            QConstruct::QAS,
            QConstruct::QAr,
            QConstruct::QAR,
            QConstruct::QAZ,
            QConstruct::QAI,
        ];
        for q in all {
            assert_eq!(
                QConstruct::from_path_element(q.as_str()),
                Some(q),
                "round-trip failed for {q:?}"
            );
        }
        assert_eq!(QConstruct::from_path_element("WIDE1-1"), None);
    }

    #[test]
    fn qconstruct_from_path_element_strips_star() {
        // The wire form should never carry the H-bit `*` on a
        // q-construct slot, but tolerant parsing recognises it anyway.
        assert_eq!(QConstruct::from_path_element("qAR*"), Some(QConstruct::QAR));
    }

    #[test]
    fn format_is_packet_with_qconstruct_injects_tag() {
        let pkt = format_is_packet_with_qconstruct(
            "N0CALL",
            "APK005",
            &["WIDE1-1"],
            QConstruct::QAC,
            "N0CALL",
            "!4903.50N/07201.75W-",
        );
        assert_eq!(
            pkt,
            "N0CALL>APK005,WIDE1-1,qAC,N0CALL:!4903.50N/07201.75W-\r\n"
        );
    }

    // ----- igate_format_for_is rewriter -----

    #[test]
    fn igate_format_verified_appends_qar() -> TestResult {
        // A heard (H-bit set) hop is gated verbatim with its `*`
        // marker; only `,qAR,N0CALL-7` is appended.
        let src = addr("W1AW", 0);
        let dst = addr("APK005", 0);
        let path = vec![used_digi("WIDE1", 1)];
        let igate = addr("N0CALL", 7);
        let pass = Passcode::Verified(12_345);

        let line = igate_format_for_is(&src, &dst, &path, b"!4903.50N/07201.75W-", &igate, pass)?;
        assert_eq!(
            line,
            "W1AW>APK005,WIDE1-1*,qAR,N0CALL-7:!4903.50N/07201.75W-\r\n"
        );
        Ok(())
    }

    #[test]
    fn igate_format_receive_only_appends_qao() -> TestResult {
        // q.aspx: "Receive-only IGates will use [qAO] exclusively for
        // all packets gated to APRS-IS." Capital letter O, not qAr.
        let src = addr("W1AW", 0);
        let dst = addr("APK005", 0);
        let path = vec![used_digi("WIDE1", 1)];
        let igate = addr("N0CALL", 7);
        let pass = Passcode::ReceiveOnly;

        let line = igate_format_for_is(&src, &dst, &path, b"!4903.50N/07201.75W-", &igate, pass)?;
        assert_eq!(
            line,
            "W1AW>APK005,WIDE1-1*,qAO,N0CALL-7:!4903.50N/07201.75W-\r\n"
        );
        // Guard against the qAO/qAr and qAO/qAo confusions.
        assert!(line.contains(",qAO,"), "expected capital-O qAO: {line}");
        assert!(!line.contains(",qAr,"), "must not emit server qAr: {line}");
        assert!(!line.contains(",qAo,"), "must not emit server qAo: {line}");
        Ok(())
    }

    #[test]
    fn igate_format_preserves_heard_path_verbatim() -> TestResult {
        // IGating.aspx: append-only. The full heard path — including a
        // *later* unrepeated hop — is preserved verbatim, with the
        // has-been-repeated `*` on the hops that carry it. Only
        // `,qAR,N0CALL-7` is appended.
        let src = addr("W1AW", 0);
        let dst = addr("APK005", 0);
        let path = vec![
            used_digi("WIDE1", 1),
            used_digi("WIDE2", 2),
            unused_digi("WIDE3", 3),
        ];
        let igate = addr("N0CALL", 7);
        let pass = Passcode::Verified(12_345);

        let line = igate_format_for_is(&src, &dst, &path, b"!data", &igate, pass)?;
        assert_eq!(
            line,
            "W1AW>APK005,WIDE1-1*,WIDE2-2*,WIDE3-3,qAR,N0CALL-7:!data\r\n"
        );
        Ok(())
    }

    #[test]
    fn igate_format_preserves_unused_digipeaters() -> TestResult {
        // Append-only gating keeps every heard hop, even ones whose
        // H-bit is clear — the older "drop unused slots" behavior
        // violated IGating.aspx and is gone.
        let src = addr("W1AW", 0);
        let dst = addr("APK005", 0);
        let path = vec![used_digi("WIDE1", 1), unused_digi("WIDE2", 1)];
        let igate = addr("N0CALL", 7);
        let pass = Passcode::Verified(12_345);

        let line = igate_format_for_is(&src, &dst, &path, b"test", &igate, pass)?;
        // Heard hop keeps `*`; unrepeated hop is kept without `*`.
        assert!(
            line.contains(",WIDE1-1*,WIDE2-1,qAR,"),
            "full heard path should be preserved: {line}"
        );
        Ok(())
    }

    #[test]
    fn igate_format_emits_single_qconstruct() -> TestResult {
        // A heard RF frame can never carry a q-construct: q-constructs
        // only exist on APRS-IS (q.aspx), and `RouteEntry` callsigns are
        // validated uppercase ASCII alphanumerics, so `qAR`/`qAS`/… can
        // never appear as a heard hop. The only q-construct in the
        // output is therefore the IGate-authoritative one, exactly once.
        let src = addr("W1AW", 0);
        let dst = addr("APK005", 0);
        let path = vec![used_digi("WIDE1", 1)];
        let igate = addr("N0CALL", 7);
        let pass = Passcode::Verified(12_345);

        let line = igate_format_for_is(&src, &dst, &path, b"test", &igate, pass)?;
        assert_eq!(line.matches(",qAR,").count(), 1, "exactly one qAR: {line}");
        Ok(())
    }

    #[test]
    fn igate_format_refuses_nogate_path() {
        let src = addr("W1AW", 0);
        let dst = addr("APK005", 0);
        let path = vec![used_digi("WIDE1", 1), used_digi("NOGATE", 0)];
        let igate = addr("N0CALL", 7);
        let pass = Passcode::Verified(12_345);

        let result = igate_format_for_is(&src, &dst, &path, b"test", &igate, pass);
        assert_eq!(result, Err(IGateError::PathBlocksGating));
    }

    #[test]
    fn igate_format_refuses_rfonly_path() {
        let src = addr("W1AW", 0);
        let dst = addr("APK005", 0);
        let path = vec![used_digi("RFONLY", 0)];
        let igate = addr("N0CALL", 7);
        let pass = Passcode::Verified(12_345);

        assert_eq!(
            igate_format_for_is(&src, &dst, &path, b"test", &igate, pass),
            Err(IGateError::PathIsRfOnly)
        );
    }

    #[test]
    fn igate_format_refuses_path_with_tcpip() {
        let src = addr("W1AW", 0);
        let dst = addr("APK005", 0);
        let path = vec![used_digi("TCPIP", 0)];
        let igate = addr("N0CALL", 7);
        let pass = Passcode::Verified(12_345);

        assert_eq!(
            igate_format_for_is(&src, &dst, &path, b"test", &igate, pass),
            Err(IGateError::PathAlreadyGated)
        );
    }

    #[test]
    fn igate_format_refuses_source_tcpip() {
        let src = addr("TCPIP", 0);
        let dst = addr("APK005", 0);
        let path = vec![];
        let igate = addr("N0CALL", 7);
        let pass = Passcode::Verified(12_345);

        assert_eq!(
            igate_format_for_is(&src, &dst, &path, b"test", &igate, pass),
            Err(IGateError::SourceIsInternet)
        );
    }

    #[test]
    fn igate_format_refuses_third_party() {
        let src = addr("W1AW", 0);
        let dst = addr("APK005", 0);
        let path = vec![];
        let igate = addr("N0CALL", 7);
        let pass = Passcode::Verified(12_345);

        // Info field starts with `}` (third-party header).
        assert_eq!(
            igate_format_for_is(&src, &dst, &path, b"}wrapped", &igate, pass),
            Err(IGateError::ThirdPartyPacket)
        );
    }

    #[test]
    fn igate_format_refuses_loop_with_own_call() {
        let src = addr("W1AW", 0);
        let dst = addr("APK005", 0);
        let igate = addr("N0CALL", 7);
        // Same callsign+SSID in the path → loop.
        let path = vec![used_digi("N0CALL", 7)];
        let pass = Passcode::Verified(12_345);

        assert_eq!(
            igate_format_for_is(&src, &dst, &path, b"test", &igate, pass),
            Err(IGateError::LoopDetected)
        );
    }

    #[test]
    fn igate_format_handles_empty_path() -> TestResult {
        // Direct hop, no digipeaters in the RF path.
        let src = addr("W1AW", 0);
        let dst = addr("APK005", 0);
        let path = vec![];
        let igate = addr("N0CALL", 7);
        let pass = Passcode::Verified(12_345);

        let line = igate_format_for_is(&src, &dst, &path, b"!4903.50N/07201.75W-", &igate, pass)?;
        assert_eq!(line, "W1AW>APK005,qAR,N0CALL-7:!4903.50N/07201.75W-\r\n");
        Ok(())
    }

    #[test]
    fn igate_format_preserves_non_utf8_info_lossily() -> TestResult {
        // Mic-E and binary weather data carry bytes ≥ 0x80. The lossy
        // decode replaces invalid UTF-8 sequences with U+FFFD; the line
        // remains a well-formed Rust `String`.
        let src = addr("W1AW", 0);
        let dst = addr("APK005", 0);
        let path = vec![];
        let igate = addr("N0CALL", 7);
        let pass = Passcode::Verified(12_345);

        let info: &[u8] = &[b'`', 0xC1, 0x82, b'X'];
        let line = igate_format_for_is(&src, &dst, &path, info, &igate, pass)?;
        assert!(line.starts_with("W1AW>APK005,qAR,N0CALL-7:`"));
        assert!(line.contains('\u{FFFD}'), "expected U+FFFD in {line}");
        Ok(())
    }

    #[test]
    fn igate_error_display_messages_are_actionable() -> TestResult {
        let mut s = String::new();
        for err in [
            IGateError::SourceIsInternet,
            IGateError::PathBlocksGating,
            IGateError::PathIsRfOnly,
            IGateError::PathAlreadyGated,
            IGateError::LoopDetected,
            IGateError::ThirdPartyPacket,
        ] {
            s.clear();
            write!(s, "{err}")?;
            assert!(
                !s.is_empty(),
                "Display should produce a non-empty message for {err:?}"
            );
        }
        Ok(())
    }

    #[test]
    fn igate_rewritten_path_returns_components() -> TestResult {
        // Append-only: the heard path is preserved verbatim (with `*`
        // on the heard hop and the unrepeated hop kept), then the
        // q-construct and IGate callsign follow.
        let src = addr("W1AW", 0);
        let path = vec![used_digi("WIDE1", 1), unused_digi("WIDE2", 1)];
        let igate = addr("N0CALL", 7);
        let pass = Passcode::Verified(12_345);

        let parts = igate_rewritten_path(&src, &path, b"test", &igate, pass)?;
        assert_eq!(parts, vec!["WIDE1-1*", "WIDE2-1", "qAR", "N0CALL-7"]);
        Ok(())
    }

    #[test]
    fn igate_rewritten_path_receive_only_uses_qao() -> TestResult {
        let src = addr("W1AW", 0);
        let path = vec![used_digi("WIDE1", 1)];
        let igate = addr("N0CALL", 7);
        let pass = Passcode::ReceiveOnly;

        let parts = igate_rewritten_path(&src, &path, b"test", &igate, pass)?;
        assert_eq!(parts, vec!["WIDE1-1*", "qAO", "N0CALL-7"]);
        Ok(())
    }
}
