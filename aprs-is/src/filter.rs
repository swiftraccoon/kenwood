//! APRS-IS server-side filter expressions.
//!
//! Per <https://www.aprs-is.net/javAPRSFilter.aspx>, APRS-IS servers
//! accept a small filter-expression language for selecting which
//! packets to deliver to a client connection. Each filter line is a
//! whitespace-separated list of clauses; matching is **OR** across
//! clauses (any clause matching delivers the packet). Each clause
//! type has its own single-character key.
//!
//! This enum covers all spec-defined clause types. Use
//! [`AprsIsFilter::raw`] for forms the spec adds after this code's
//! release date or for vendor-specific extensions.

/// Structured APRS-IS filter expression.
///
/// One variant per spec-defined clause type. Multiple filters
/// combine with [`AprsIsFilter::join`] (space-separated, OR-matched).
///
/// Negation is supported on a per-clause basis via
/// [`AprsIsFilter::negated`] which wraps any clause as `-clause`
/// per the spec's "Negation prefix" rule.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum AprsIsFilter {
    /// Range filter `r/lat/lon/distance_km`: packets from stations
    /// within the given radius of the centre coordinate.
    Range {
        /// Centre latitude in degrees (positive = North).
        lat: f64,
        /// Centre longitude in degrees (positive = East).
        lon: f64,
        /// Radius in kilometres.
        distance_km: f64,
    },
    /// Area / bounding-box filter `a/latN/lonW/latS/lonE`: packets
    /// inside the rectangular area defined by NW and SE corner coords.
    Area {
        /// North-edge latitude.
        lat1: f64,
        /// West-edge longitude.
        lon1: f64,
        /// South-edge latitude.
        lat2: f64,
        /// East-edge longitude.
        lon2: f64,
    },
    /// Prefix filter `p/aa/bb/cc/…`: packets whose source callsign
    /// begins with any of the listed prefixes (case-insensitive).
    Prefix(Vec<String>),
    /// Budlist filter `b/call1/call2/…`: packets from exactly these
    /// stations.
    Budlist(Vec<String>),
    /// Object filter `o/obj1/obj2/…`: object reports with these
    /// names (substring match per the spec).
    Object(Vec<String>),
    /// Strict-object filter `os/obj1/obj2/…`: like [`Self::Object`]
    /// but enforces 9-char object names and must be the last clause
    /// on the filter line per javAPRSFilter.html.
    StrictObject(Vec<String>),
    /// Type filter `t/poimqstunw`: single-string of frame-type
    /// characters selecting which packet types to include.
    ///
    /// The spec set is exactly `p o i m q s t u n w` per the
    /// javAPRSFilter.html "t" table (`t/poimqstunw`):
    /// - `p` position packets
    /// - `o` objects
    /// - `i` items
    /// - `m` message
    /// - `q` query
    /// - `s` status
    /// - `t` telemetry
    /// - `u` user-defined
    /// - `n` NWS-format messages and objects
    /// - `w` weather
    ///
    /// The string is passed through verbatim, so only these characters
    /// match server-side; any other letter selects nothing.
    Type(String),
    /// Station-centric type filter `t/poimqstuw/call/km`: like
    /// [`Self::Type`] but restricted to stations within `distance_km`
    /// of `callsign`.
    TypeAround {
        /// Frame-type chars (same set as [`Self::Type`]).
        types: String,
        /// Centre station callsign.
        callsign: String,
        /// Radius in km.
        distance_km: f64,
    },
    /// Symbol filter `s/pri/alt/over`: primary-table symbols,
    /// alternate-table symbols, and overlay characters (each field
    /// is a string of single-char symbol codes; empty fields are
    /// allowed).
    Symbol {
        /// Primary-table symbol codes.
        primary: String,
        /// Alternate-table symbol codes.
        alternate: String,
        /// Overlay characters.
        overlay: String,
    },
    /// Digipeater filter `d/digi1/digi2/…`: packets digipeated by
    /// any of the listed stations.
    Digi(Vec<String>),
    /// Entry-station filter `e/call1/call2/…`: packets whose APRS-IS
    /// q-construct names any of the listed entry servers/IGates.
    Entry(Vec<String>),
    /// Q-construct filter `q/con/…`: packets whose q-construct
    /// matches the listed letters. Each entry is the third character
    /// of a `qA?` construct (e.g. `R` selects `qAR`).
    QConstruct(String),
    /// My-range filter `m/km`: packets within `distance_km` of the
    /// client's *own* location (the server uses the position from the
    /// client's most recent position report).
    MyRange {
        /// Radius in km.
        distance_km: f64,
    },
    /// Group-message filter `g/name1/name2/…`: bulletins addressed
    /// to any of the listed groups.
    Group(Vec<String>),
    /// Friend filter `f/call/dist`: packets from stations within
    /// `distance_km` of the listed friend callsign's last position.
    Friend {
        /// Station to centre on.
        callsign: String,
        /// Distance in km.
        distance_km: f64,
    },
    /// Unproto / destination filter `u/proto1/proto2/…`: packets
    /// whose AX.25 destination address matches any of the listed
    /// tocalls (e.g. `APRS`, `APK005`).
    Unproto(Vec<String>),
    /// Negated clause `-<clause>`: packets matching the inner clause
    /// are **excluded** from delivery, even if another clause would
    /// have included them.
    Negated(Box<Self>),
    /// Raw literal filter string for forms not covered by this enum.
    /// Use for vendor-specific extensions or filters added by the
    /// spec after this code's release date.
    Raw(String),
}

impl AprsIsFilter {
    /// Build a raw literal filter expression.
    #[must_use]
    pub fn raw(s: impl Into<String>) -> Self {
        Self::Raw(s.into())
    }

    /// Wrap any filter as its negated form (`-<clause>` on the wire).
    ///
    /// Per javAPRSFilter.html, a negated clause excludes matching
    /// packets even when another clause would have included them.
    /// Double-negation re-folds to a positive clause.
    #[must_use]
    pub fn negated(inner: Self) -> Self {
        // Fold `Negated(Negated(x))` back to `x` to avoid emitting
        // `--clause` on the wire (which servers parse inconsistently).
        if let Self::Negated(boxed) = inner {
            *boxed
        } else {
            Self::Negated(Box::new(inner))
        }
    }

    /// Format this filter as the exact wire-format string APRS-IS
    /// servers expect after the `filter ` keyword in the login line.
    #[must_use]
    pub fn as_wire(&self) -> String {
        match self {
            Self::Range {
                lat,
                lon,
                distance_km,
            } => format!("r/{lat}/{lon}/{distance_km}"),
            Self::Area {
                lat1,
                lon1,
                lat2,
                lon2,
            } => format!("a/{lat1}/{lon1}/{lat2}/{lon2}"),
            Self::Prefix(parts) => format!("p/{}", parts.join("/")),
            Self::Budlist(parts) => format!("b/{}", parts.join("/")),
            Self::Object(parts) => format!("o/{}", parts.join("/")),
            Self::StrictObject(parts) => format!("os/{}", parts.join("/")),
            Self::Type(chars) => format!("t/{chars}"),
            Self::TypeAround {
                types,
                callsign,
                distance_km,
            } => format!("t/{types}/{callsign}/{distance_km}"),
            Self::Symbol {
                primary,
                alternate,
                overlay,
            } => format!("s/{primary}/{alternate}/{overlay}"),
            Self::Digi(parts) => format!("d/{}", parts.join("/")),
            Self::Entry(parts) => format!("e/{}", parts.join("/")),
            Self::QConstruct(chars) => format!("q/{chars}"),
            Self::MyRange { distance_km } => format!("m/{distance_km}"),
            Self::Group(parts) => format!("g/{}", parts.join("/")),
            Self::Friend {
                callsign,
                distance_km,
            } => format!("f/{callsign}/{distance_km}"),
            Self::Unproto(parts) => format!("u/{}", parts.join("/")),
            Self::Negated(inner) => format!("-{}", inner.as_wire()),
            Self::Raw(s) => s.clone(),
        }
    }

    /// Combine multiple filter clauses into a single filter string by
    /// joining with spaces. APRS-IS allows an OR of any number of
    /// clauses in a single `filter` directive; negated clauses then
    /// subtract from the OR.
    #[must_use]
    pub fn join(filters: &[Self]) -> String {
        filters
            .iter()
            .map(Self::as_wire)
            .collect::<Vec<_>>()
            .join(" ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aprs_is_filter_range_wire_format() {
        let f = AprsIsFilter::Range {
            lat: 35.25,
            lon: -97.75,
            distance_km: 100.0,
        };
        assert_eq!(f.as_wire(), "r/35.25/-97.75/100");
    }

    #[test]
    fn aprs_is_filter_type_and_prefix() {
        let f = AprsIsFilter::Type("po".to_owned());
        assert_eq!(f.as_wire(), "t/po");
        let f = AprsIsFilter::Prefix(vec!["KK".to_owned(), "W1".to_owned()]);
        assert_eq!(f.as_wire(), "p/KK/W1");
    }

    #[test]
    fn aprs_is_filter_join_multiple() {
        let filters = vec![
            AprsIsFilter::Range {
                lat: 35.0,
                lon: -97.0,
                distance_km: 50.0,
            },
            AprsIsFilter::Type("p".to_owned()),
        ];
        let joined = AprsIsFilter::join(&filters);
        assert!(joined.contains("r/35"), "missing range clause: {joined:?}");
        assert!(joined.contains("t/p"), "missing type clause: {joined:?}");
        assert!(joined.contains(' '), "missing separator: {joined:?}");
    }

    #[test]
    fn aprs_is_filter_raw_passthrough() {
        let f = AprsIsFilter::raw("m/50");
        assert_eq!(f.as_wire(), "m/50");
    }

    #[test]
    fn aprs_is_filter_strict_object() {
        let f = AprsIsFilter::StrictObject(vec!["FIRE".to_owned(), "EMS".to_owned()]);
        assert_eq!(f.as_wire(), "os/FIRE/EMS");
    }

    #[test]
    fn aprs_is_filter_type_around() {
        let f = AprsIsFilter::TypeAround {
            types: "po".to_owned(),
            callsign: "N0CALL".to_owned(),
            distance_km: 50.0,
        };
        assert_eq!(f.as_wire(), "t/po/N0CALL/50");
    }

    #[test]
    fn aprs_is_filter_symbol_triplet() {
        let f = AprsIsFilter::Symbol {
            primary: ">".to_owned(),
            alternate: String::new(),
            overlay: String::new(),
        };
        assert_eq!(f.as_wire(), "s/>//");
    }

    #[test]
    fn aprs_is_filter_digi_and_entry() {
        let f = AprsIsFilter::Digi(vec!["W1AW".to_owned()]);
        assert_eq!(f.as_wire(), "d/W1AW");
        let f = AprsIsFilter::Entry(vec!["T2TEST".to_owned()]);
        assert_eq!(f.as_wire(), "e/T2TEST");
    }

    #[test]
    fn aprs_is_filter_qconstruct_my_range_group_unproto() {
        assert_eq!(AprsIsFilter::QConstruct("R".to_owned()).as_wire(), "q/R");
        assert_eq!(
            AprsIsFilter::MyRange { distance_km: 20.0 }.as_wire(),
            "m/20"
        );
        assert_eq!(AprsIsFilter::Group(vec!["WX".to_owned()]).as_wire(), "g/WX");
        assert_eq!(
            AprsIsFilter::Unproto(vec!["APK005".to_owned()]).as_wire(),
            "u/APK005"
        );
    }

    #[test]
    fn aprs_is_filter_negation_prefixes_minus() {
        let inner = AprsIsFilter::Prefix(vec!["CW".to_owned()]);
        let negated = AprsIsFilter::negated(inner);
        assert_eq!(negated.as_wire(), "-p/CW");
    }

    #[test]
    fn aprs_is_filter_double_negation_folds() {
        // Negating twice should produce the original positive clause
        // rather than the wire-syntax-invalid `--p/CW` form.
        let inner = AprsIsFilter::Prefix(vec!["CW".to_owned()]);
        let once = AprsIsFilter::negated(inner);
        let twice = AprsIsFilter::negated(once);
        assert_eq!(twice.as_wire(), "p/CW");
    }

    #[test]
    fn aprs_is_filter_type_spec_set_round_trips() {
        // Guards the doc-conformance fix: the javAPRSFilter.html "t"
        // table set is exactly `poimqstunw` (no `c`/`h`). The variant
        // passes its string through verbatim, so a clause built from the
        // documented set must round-trip unchanged.
        let f = AprsIsFilter::Type("poimqstunw".to_owned());
        assert_eq!(f.as_wire(), "t/poimqstunw");
    }
}
