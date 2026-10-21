//! APRS-IS Q-construct classification and `IGate` path rewriting.
//!
//! Q-constructs identify packet entry and tracing in the APRS-IS path.
//! Servers insert constructs for their connection type; an RF `IGate` appends
//! `qAR` for messaging support or `qAO` when it provides no return RF path.
//!
//! This module exposes:
//!
//! - The [`QConstruct`] enum (all ten spec values).
//! - [`format_is_packet_with_qconstruct`]: a *naive* line builder
//!   that simply appends `,qXX,GATE` to a path. Suitable when the
//!   caller has already computed the correct construct and gate
//!   callsign; not a full `IGate`.
//! - [`igate_format_for_is`]: a *strict*, append-only `IGate` path
//!   rewriter that implements the <https://www.aprs-is.net/q.aspx> and
//!   <https://www.aprs-is.net/IGating.aspx> algorithm: refuses to gate
//!   when the sender opted out, then gates the heard path **verbatim**
//!   (every hop preserved, including its has-been-repeated `*` marker)
//!   and appends exactly `,<qconstruct>,<IGATECALL>` to the end. The
//!   q-construct is `qAR` for a usable RF return-message path and `qAO`
//!   otherwise. Both require a verified Internet login when sent.
//!   IGating.aspx is explicit: "No modification of
//!   the TNC2 format line should be made except to add ,qAR,IGATECALL
//!   to the end of the path."
//!

use ax25_codec::{Ax25Address, RouteEntry};

use crate::uplink::{AprsIsUplinkLine, AprsIsUplinkLineError};

/// APRS-IS Q-construct tag (path identifier that records how a packet
/// entered the APRS-IS network).
///
/// Per <https://www.aprs-is.net/q.aspx>, every packet seen by an APRS-IS
/// server records its entry connection or tracing mode with this path tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum QConstruct {
    /// `qAC`: directly received from a matching verified client login.
    QAC,
    /// `qAX`: directly received from an unverified login; deprecated upstream.
    QAX,
    /// `qAU`: received directly through UDP submission.
    QAU,
    /// `qAo`: gated through a client-only port with matching legacy gate identity.
    QAo,
    /// `qAO`: RF gate without messaging, or indirect/send-only client-port traffic.
    QAO,
    /// `qAS`: server-originated traffic or a server packet without a Q-construct.
    QAS,
    /// `qAr`: received indirectly from a remote gate using the legacy `,I` path.
    QAr,
    /// `qAR`: gated from RF by a verified login.
    QAR,
    /// `qAZ`: server-client command packet. The packet is generated
    /// by the server, client, or `IGate` and must not be propagated
    /// further.
    QAZ,
    /// `qAI`: trace packet. Each server adds login identification
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
    /// The trailing `*` (has-been-repeated) marker, if present, is
    /// trimmed before matching: a path element like `qAR*` decodes to
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
/// The caller selects the construct and gate identity. Use
/// [`igate_format_for_is`] to apply RF gating opt-outs and loop checks.
///
/// # Errors
///
/// Returns [`AprsIsUplinkLineError::EmbeddedNewline`] if any supplied text
/// contains a carriage return or line feed, or
/// [`AprsIsUplinkLineError::TooLong`] if the completed wire line exceeds 512
/// bytes including its trailing `CRLF`.
/// [`AprsIsUplinkLineError::InvalidPacket`] retains malformed header diagnostics,
/// including a second Q-construct already present in the path.
pub fn format_is_packet_with_qconstruct<'path, I>(
    source: &str,
    destination: &str,
    path: I,
    qconstruct: QConstruct,
    gate_callsign: &'path str,
    data: &str,
) -> Result<AprsIsUplinkLine, AprsIsUplinkLineError>
where
    I: IntoIterator<Item = &'path str>,
{
    crate::format_is_packet(
        source,
        destination,
        path.into_iter().chain([qconstruct.as_str(), gate_callsign]),
        data,
    )
}

// ---------------------------------------------------------------------------
// IGate path rewriter (q.aspx)
// ---------------------------------------------------------------------------

/// Reasons an `IGate` refuses to gate an RF packet into APRS-IS.
///
/// Each variant corresponds to a specific gating rule from
/// <https://www.aprs-is.net/IGating.aspx>. Returned directly by
/// [`igate_rewritten_path`] and wrapped by
/// [`IGateFormatError::GatingRefused`] from [`igate_format_for_is`] so
/// callers can log why a packet was suppressed.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum IGateError {
    /// The source callsign is `TCPIP` or `TCPXX`, indicating the packet
    /// originated from APRS-IS; gating would create a loop.
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
    /// The `IGate`'s own callsign appears in the path: the packet has
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

/// Why an RF packet could not become one APRS-IS uplink line.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum IGateFormatError {
    /// APRS-IS gating rules prohibit forwarding this RF packet.
    #[error("RF packet is not eligible for APRS-IS gating: {0}")]
    GatingRefused(#[from] IGateError),

    /// The resulting bytes violate APRS-IS framing or length rules.
    #[error("RF packet cannot form a safe APRS-IS uplink line: {0}")]
    InvalidUplinkLine(#[from] AprsIsUplinkLineError),

    /// The gate holds no station identity, so the q-construct path
    /// (which embeds the gate callsign) cannot be formed.
    #[error("IGate has no station identity to embed in the q-construct path")]
    MissingIdentity,
}

/// The RF return-message capability advertised for one forwarded packet.
///
/// The client-generated constructs follow <https://www.aprs-is.net/q.aspx>.
/// An RF receive-only gate still uses a verified Internet login to upload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IgateMessaging {
    /// This gateway can return messages to the packet's RF source.
    Bidirectional,
    /// This gateway has no RF return path to this source, including an RF
    /// receive-only gateway or a source outside its return-message coverage.
    ReceiveOnly,
}

impl IgateMessaging {
    /// Returns `qAR` for a return-message path or `qAO` without one.
    #[must_use]
    pub const fn qconstruct(self) -> QConstruct {
        match self {
            Self::Bidirectional => QConstruct::QAR,
            Self::ReceiveOnly => QConstruct::QAO,
        }
    }
}

/// Format an APRS-IS line by rewriting an RF-heard packet's path per
/// the canonical `IGate` algorithm at <https://www.aprs-is.net/q.aspx>
/// and <https://www.aprs-is.net/IGating.aspx>.
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
///    example confirms the marker survives: `AE5PL>APRS,WIDE1*` gates
///    to `AE5PL>APRS,WIDE1*,qAR,AE5PL-10`. Unused hops are **not**
///    dropped and `*` is **not** stripped; doing either would mislead
///    receivers about the propagation history and violate the
///    append-only rule.
/// 3. **Append the q-construct + `IGate` callsign** at the end. The
///    construct is [`QConstruct::QAR`] for an RF return-message path,
///    [`QConstruct::QAO`] (letter O) without one.
/// 4. **Normalize terminal TNC framing.** Some radios put one or more `CR`
///    or `LF` delimiter bytes at the end of the AX.25 information field.
///    TNC2/APRS-IS represents those as line framing, not payload, so they are
///    removed before the one canonical `CRLF` is appended. Framing bytes
///    anywhere else remain an error, preventing line injection.
/// 5. **Serialize** as a validated, CRLF-terminated
///    [`AprsIsUplinkLine`] without decoding the information bytes.
///
/// # Parameters
///
/// - `source`: AX.25 source address of the RF packet.
/// - `destination`: AX.25 destination address (typically an `APxxxx`
///   tocall).
/// - `rf_path`: Digipeater path slots from the RF frame. Each slot is
///   gated verbatim, preserving its has-been-repeated `*` marker.
/// - `info`: Packet information field bytes. Non-UTF-8 Mic-E data is preserved
///   exactly; terminal TNC `CR`/`LF` delimiters are normalized as described
///   above.
/// - `igate`: This `IGate`'s callsign + SSID, as appears on the wire
///   after the q-construct.
/// - `messaging`: The RF return-message capability for this source.
///
/// # Errors
///
/// Returns [`IGateFormatError::GatingRefused`] when the packet must not
/// be gated. Returns [`IGateFormatError::InvalidUplinkLine`] if the
/// information field contains a framing byte or the complete line would
/// exceed the APRS-IS 512-byte limit.
///
/// # Examples
///
/// ```
/// use ax25_codec::{Ax25Address, RouteEntry};
/// use aprs_is::{IgateMessaging, igate_format_for_is};
///
/// # fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let src = Ax25Address::new("W1AW", 0)?;
/// let dst = Ax25Address::new("APK005", 0)?;
/// let mut wide1 = RouteEntry::new("WIDE1", 1)?;
/// wide1.has_repeated = true;
/// let path = vec![wide1];
/// let igate = Ax25Address::new("N0CALL", 7)?;
/// let messaging = IgateMessaging::Bidirectional;
///
/// let line = igate_format_for_is(
///     &src, &dst, &path,
///     b"!4903.50N/07201.75W-",
///     &igate, messaging,
/// )?;
/// // The heard `WIDE1-1*` is preserved verbatim (marker and all);
/// // only `,qAR,N0CALL-7` is appended.
/// assert_eq!(
///     line.as_bytes(),
///     b"W1AW>APK005,WIDE1-1*,qAR,N0CALL-7:!4903.50N/07201.75W-\r\n",
/// );
/// # Ok(())
/// # }
/// ```
///
/// # Notes
///
/// - The output line is byte-for-byte ready to send to an APRS-IS
///   server via [`crate::AprsIsClient::send_uplink_line`].
/// - This function does **not** modify the input packet; rewriting is
///   pure-data.
pub fn igate_format_for_is(
    source: &Ax25Address,
    destination: &Ax25Address,
    rf_path: &[RouteEntry],
    info: &[u8],
    igate: &Ax25Address,
    messaging: IgateMessaging,
) -> Result<AprsIsUplinkLine, IGateFormatError> {
    let path_parts = igate_rewritten_path(source, rf_path, info, igate, messaging)?;
    let source_str = source.to_string();
    let destination_str = destination.to_string();

    let mut body = Vec::new();
    body.extend_from_slice(source_str.as_bytes());
    body.push(b'>');
    body.extend_from_slice(destination_str.as_bytes());
    for part in &path_parts {
        body.push(b',');
        body.extend_from_slice(part.as_bytes());
    }
    body.push(b':');
    body.extend_from_slice(trim_terminal_tnc_framing(info));

    Ok(AprsIsUplinkLine::from_body_bytes(&body)?)
}

fn trim_terminal_tnc_framing(mut info: &[u8]) -> &[u8] {
    while matches!(info.last(), Some(b'\r' | b'\n')) {
        info = info
            .get(..info.len() - 1)
            .unwrap_or_else(|| unreachable!("a last byte implies a non-empty slice"));
    }
    info
}

/// Convenience wrapper around [`igate_format_for_is`] for callers that
/// already have a fully-formed `Ax25Packet` and just want the IS line.
///
/// Equivalent to passing `packet.header.source`, `packet.header.destination`,
/// `packet.header.digipeaters`, and `packet.information()` to
/// [`igate_format_for_is`].
///
/// # Errors
///
/// See [`igate_format_for_is`].
pub fn igate_format_packet_for_is(
    packet: &ax25_codec::Ax25Packet,
    igate: &Ax25Address,
    messaging: IgateMessaging,
) -> Result<AprsIsUplinkLine, IGateFormatError> {
    igate_format_for_is(
        &packet.header.source,
        &packet.header.destination,
        packet.header.digipeaters.as_slice(),
        packet.information(),
        igate,
        messaging,
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
    messaging: IgateMessaging,
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
    let qconstruct = messaging.qconstruct();
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

    fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
        haystack
            .windows(needle.len())
            .any(|window| window == needle)
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
    fn format_is_packet_with_qconstruct_injects_tag() -> TestResult {
        let pkt = format_is_packet_with_qconstruct(
            "N0CALL",
            "APK005",
            ["WIDE1-1"],
            QConstruct::QAC,
            "N0CALL",
            "!4903.50N/07201.75W-",
        )?;
        assert_eq!(
            pkt.as_bytes(),
            b"N0CALL>APK005,WIDE1-1,qAC,N0CALL:!4903.50N/07201.75W-\r\n"
        );
        Ok(())
    }

    #[test]
    fn format_is_packet_with_qconstruct_rejects_framing_in_every_text_field() {
        for result in [
            format_is_packet_with_qconstruct(
                "N0CALL\rFORGED",
                "APK005",
                [],
                QConstruct::QAC,
                "GATE",
                "data",
            ),
            format_is_packet_with_qconstruct(
                "N0CALL",
                "APK005\nFORGED",
                [],
                QConstruct::QAC,
                "GATE",
                "data",
            ),
            format_is_packet_with_qconstruct(
                "N0CALL",
                "APK005",
                ["WIDE1-1\r\nFORGED"],
                QConstruct::QAC,
                "GATE",
                "data",
            ),
            format_is_packet_with_qconstruct(
                "N0CALL",
                "APK005",
                [],
                QConstruct::QAC,
                "GATE\nFORGED",
                "data",
            ),
            format_is_packet_with_qconstruct(
                "N0CALL",
                "APK005",
                [],
                QConstruct::QAC,
                "GATE",
                "data\r\nFORGED>APRS:packet",
            ),
        ] {
            assert!(matches!(
                result,
                Err(AprsIsUplinkLineError::EmbeddedNewline { .. })
            ));
        }
    }

    #[test]
    fn format_is_packet_with_qconstruct_enforces_wire_limit() -> TestResult {
        let prefix = b"N0CALL>APK005,qAC,GATE:";
        let maximum_data = "X".repeat(crate::MAX_IS_LINE_BYTES - prefix.len() - 2);
        let maximum = format_is_packet_with_qconstruct(
            "N0CALL",
            "APK005",
            [],
            QConstruct::QAC,
            "GATE",
            &maximum_data,
        )?;
        assert_eq!(maximum.as_bytes().len(), crate::MAX_IS_LINE_BYTES);

        let oversized_data = format!("{maximum_data}X");
        assert!(matches!(
            format_is_packet_with_qconstruct(
                "N0CALL",
                "APK005",
                [],
                QConstruct::QAC,
                "GATE",
                &oversized_data,
            ),
            Err(AprsIsUplinkLineError::TooLong { .. })
        ));
        Ok(())
    }

    // ----- igate_format_for_is rewriter -----

    #[test]
    fn igate_format_with_return_messaging_appends_qar() -> TestResult {
        // A heard (H-bit set) hop is gated verbatim with its `*`
        // marker; only `,qAR,N0CALL-7` is appended.
        let src = addr("W1AW", 0);
        let dst = addr("APK005", 0);
        let path = vec![used_digi("WIDE1", 1)];
        let igate = addr("N0CALL", 7);
        let messaging = IgateMessaging::Bidirectional;

        let line = igate_format_for_is(
            &src,
            &dst,
            &path,
            b"!4903.50N/07201.75W-",
            &igate,
            messaging,
        )?;
        assert_eq!(
            line.as_bytes(),
            b"W1AW>APK005,WIDE1-1*,qAR,N0CALL-7:!4903.50N/07201.75W-\r\n"
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
        let messaging = IgateMessaging::ReceiveOnly;

        let line = igate_format_for_is(
            &src,
            &dst,
            &path,
            b"!4903.50N/07201.75W-",
            &igate,
            messaging,
        )?;
        assert_eq!(
            line.as_bytes(),
            b"W1AW>APK005,WIDE1-1*,qAO,N0CALL-7:!4903.50N/07201.75W-\r\n"
        );
        // Guard against the qAO/qAr and qAO/qAo confusions.
        assert!(
            contains_bytes(line.as_bytes(), b",qAO,"),
            "expected capital-O qAO: {line:?}"
        );
        assert!(
            !contains_bytes(line.as_bytes(), b",qAr,"),
            "must not emit server qAr: {line:?}"
        );
        assert!(
            !contains_bytes(line.as_bytes(), b",qAo,"),
            "must not emit server qAo: {line:?}"
        );
        Ok(())
    }

    #[test]
    fn igate_format_preserves_heard_path_verbatim() -> TestResult {
        // IGating.aspx: append-only. The full heard path, including a
        // *later* unrepeated hop, is preserved verbatim, with the
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
        let messaging = IgateMessaging::Bidirectional;

        let line = igate_format_for_is(&src, &dst, &path, b"!data", &igate, messaging)?;
        assert_eq!(
            line.as_bytes(),
            b"W1AW>APK005,WIDE1-1*,WIDE2-2*,WIDE3-3,qAR,N0CALL-7:!data\r\n"
        );
        Ok(())
    }

    #[test]
    fn igate_format_preserves_unused_digipeaters() -> TestResult {
        // Append-only gating keeps every heard hop, including unused hops.
        let src = addr("W1AW", 0);
        let dst = addr("APK005", 0);
        let path = vec![used_digi("WIDE1", 1), unused_digi("WIDE2", 1)];
        let igate = addr("N0CALL", 7);
        let messaging = IgateMessaging::Bidirectional;

        let line = igate_format_for_is(&src, &dst, &path, b"test", &igate, messaging)?;
        // Heard hop keeps `*`; unrepeated hop is kept without `*`.
        assert!(
            contains_bytes(line.as_bytes(), b",WIDE1-1*,WIDE2-1,qAR,"),
            "full heard path should be preserved: {line:?}"
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
        let messaging = IgateMessaging::Bidirectional;

        let line = igate_format_for_is(&src, &dst, &path, b"test", &igate, messaging)?;
        assert_eq!(
            line.as_bytes()
                .windows(b",qAR,".len())
                .filter(|window| *window == b",qAR,")
                .count(),
            1,
            "exactly one qAR: {line:?}"
        );
        Ok(())
    }

    #[test]
    fn igate_format_refuses_nogate_path() {
        let src = addr("W1AW", 0);
        let dst = addr("APK005", 0);
        let path = vec![used_digi("WIDE1", 1), used_digi("NOGATE", 0)];
        let igate = addr("N0CALL", 7);
        let messaging = IgateMessaging::Bidirectional;

        let result = igate_format_for_is(&src, &dst, &path, b"test", &igate, messaging);
        assert_eq!(
            result,
            Err(IGateFormatError::GatingRefused(
                IGateError::PathBlocksGating
            ))
        );
    }

    #[test]
    fn igate_format_refuses_rfonly_path() {
        let src = addr("W1AW", 0);
        let dst = addr("APK005", 0);
        let path = vec![used_digi("RFONLY", 0)];
        let igate = addr("N0CALL", 7);
        let messaging = IgateMessaging::Bidirectional;

        assert_eq!(
            igate_format_for_is(&src, &dst, &path, b"test", &igate, messaging),
            Err(IGateFormatError::GatingRefused(IGateError::PathIsRfOnly))
        );
    }

    #[test]
    fn igate_format_refuses_path_with_tcpip() {
        let src = addr("W1AW", 0);
        let dst = addr("APK005", 0);
        let path = vec![used_digi("TCPIP", 0)];
        let igate = addr("N0CALL", 7);
        let messaging = IgateMessaging::Bidirectional;

        assert_eq!(
            igate_format_for_is(&src, &dst, &path, b"test", &igate, messaging),
            Err(IGateFormatError::GatingRefused(
                IGateError::PathAlreadyGated
            ))
        );
    }

    #[test]
    fn igate_format_refuses_source_tcpip() {
        let src = addr("TCPIP", 0);
        let dst = addr("APK005", 0);
        let path = vec![];
        let igate = addr("N0CALL", 7);
        let messaging = IgateMessaging::Bidirectional;

        assert_eq!(
            igate_format_for_is(&src, &dst, &path, b"test", &igate, messaging),
            Err(IGateFormatError::GatingRefused(
                IGateError::SourceIsInternet
            ))
        );
    }

    #[test]
    fn igate_format_refuses_third_party() {
        let src = addr("W1AW", 0);
        let dst = addr("APK005", 0);
        let path = vec![];
        let igate = addr("N0CALL", 7);
        let messaging = IgateMessaging::Bidirectional;

        // Info field starts with `}` (third-party header).
        assert_eq!(
            igate_format_for_is(&src, &dst, &path, b"}wrapped", &igate, messaging),
            Err(IGateFormatError::GatingRefused(
                IGateError::ThirdPartyPacket
            ))
        );
    }

    #[test]
    fn igate_format_refuses_loop_with_own_call() {
        let src = addr("W1AW", 0);
        let dst = addr("APK005", 0);
        let igate = addr("N0CALL", 7);
        // Same callsign+SSID in the path → loop.
        let path = vec![used_digi("N0CALL", 7)];
        let messaging = IgateMessaging::Bidirectional;

        assert_eq!(
            igate_format_for_is(&src, &dst, &path, b"test", &igate, messaging),
            Err(IGateFormatError::GatingRefused(IGateError::LoopDetected))
        );
    }

    #[test]
    fn igate_format_handles_empty_path() -> TestResult {
        // Direct hop, no digipeaters in the RF path.
        let src = addr("W1AW", 0);
        let dst = addr("APK005", 0);
        let path = vec![];
        let igate = addr("N0CALL", 7);
        let messaging = IgateMessaging::Bidirectional;

        let line = igate_format_for_is(
            &src,
            &dst,
            &path,
            b"!4903.50N/07201.75W-",
            &igate,
            messaging,
        )?;
        assert_eq!(
            line.as_bytes(),
            b"W1AW>APK005,qAR,N0CALL-7:!4903.50N/07201.75W-\r\n"
        );
        Ok(())
    }

    #[test]
    fn igate_format_preserves_non_utf8_info_exactly() -> TestResult {
        // Mic-E and binary weather data carry bytes at or above 0x80.
        let src = addr("W1AW", 0);
        let dst = addr("APK005", 0);
        let path = vec![];
        let igate = addr("N0CALL", 7);
        let messaging = IgateMessaging::Bidirectional;

        let info: &[u8] = &[b'`', 0xC1, 0x82, b'X'];
        let line = igate_format_for_is(&src, &dst, &path, info, &igate, messaging)?;
        let mut expected = b"W1AW>APK005,qAR,N0CALL-7:".to_vec();
        expected.extend_from_slice(info);
        expected.extend_from_slice(b"\r\n");
        assert_eq!(line.as_bytes(), expected);
        Ok(())
    }

    #[test]
    fn igate_format_rejects_information_field_framing_bytes() {
        let src = addr("W1AW", 0);
        let dst = addr("APK005", 0);
        let igate = addr("N0CALL", 7);
        let messaging = IgateMessaging::Bidirectional;

        let result =
            igate_format_for_is(&src, &dst, &[], b"good\r\nEVIL>X:forged", &igate, messaging);
        assert!(matches!(
            result,
            Err(IGateFormatError::InvalidUplinkLine(
                AprsIsUplinkLineError::EmbeddedNewline { .. }
            ))
        ));
    }

    #[test]
    fn igate_format_normalizes_only_terminal_tnc_framing() -> TestResult {
        let src = addr("W1AW", 0);
        let dst = addr("APK005", 0);
        let igate = addr("N0CALL", 7);
        let messaging = IgateMessaging::Bidirectional;

        for terminated in [b">status\r".as_slice(), b">status\n", b">status\r\n"] {
            let line = igate_format_for_is(&src, &dst, &[], terminated, &igate, messaging)?;
            assert_eq!(line.as_bytes(), b"W1AW>APK005,qAR,N0CALL-7:>status\r\n");
        }

        let injected = igate_format_for_is(
            &src,
            &dst,
            &[],
            b">status\r\nEVIL>APRS:forged\r",
            &igate,
            messaging,
        );
        assert!(matches!(
            injected,
            Err(IGateFormatError::InvalidUplinkLine(
                AprsIsUplinkLineError::EmbeddedNewline { .. }
            ))
        ));
        Ok(())
    }

    #[test]
    fn igate_format_enforces_framed_line_limit() -> TestResult {
        let src = addr("W1AW", 0);
        let dst = addr("APK005", 0);
        let igate = addr("N0CALL", 7);
        let messaging = IgateMessaging::Bidirectional;
        let prefix = b"W1AW>APK005,qAR,N0CALL-7:";

        let maximum_info = vec![b'A'; crate::MAX_IS_LINE_BYTES - prefix.len() - 2];
        let maximum = igate_format_for_is(&src, &dst, &[], &maximum_info, &igate, messaging)?;
        assert_eq!(maximum.as_bytes().len(), crate::MAX_IS_LINE_BYTES);

        let oversized_info = vec![b'A'; maximum_info.len() + 1];
        assert!(matches!(
            igate_format_for_is(&src, &dst, &[], &oversized_info, &igate, messaging),
            Err(IGateFormatError::InvalidUplinkLine(
                AprsIsUplinkLineError::TooLong { .. }
            ))
        ));
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
        let messaging = IgateMessaging::Bidirectional;

        let parts = igate_rewritten_path(&src, &path, b"test", &igate, messaging)?;
        assert_eq!(parts, vec!["WIDE1-1*", "WIDE2-1", "qAR", "N0CALL-7"]);
        Ok(())
    }

    #[test]
    fn igate_rewritten_path_receive_only_uses_qao() -> TestResult {
        let src = addr("W1AW", 0);
        let path = vec![used_digi("WIDE1", 1)];
        let igate = addr("N0CALL", 7);
        let messaging = IgateMessaging::ReceiveOnly;

        let parts = igate_rewritten_path(&src, &path, b"test", &igate, messaging)?;
        assert_eq!(parts, vec!["WIDE1-1*", "qAO", "N0CALL-7"]);
        Ok(())
    }
}
