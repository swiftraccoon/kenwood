//! APRS-IS Q-construct classification and `IGate` path rewriting.

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
    /// `qAZ` — not gated (server-added as a diagnostic).
    QAZ,
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
        }
    }

    /// Parse a path element as a Q-construct if it matches one of the
    /// well-known forms. Returns `None` otherwise.
    #[must_use]
    pub fn from_path_element(s: &str) -> Option<Self> {
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
            _ => None,
        }
    }
}

impl std::fmt::Display for QConstruct {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Format an APRS-IS packet with an explicit Q-construct.
///
/// Injects the Q-construct just before the gate callsign — the form
/// required for packets originated by a client application. Per the
/// APRS-IS spec, clients add `qAC` or `qAX` depending on whether they
/// authenticated.
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
