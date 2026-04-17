//! APRS-IS server-side filter expressions.

/// Structured APRS-IS filter expression.
///
/// Per <http://www.aprs-is.net/javAPRSFilter.aspx>, APRS-IS servers
/// accept a small query language for selecting which packets to deliver
/// to a client connection. Each filter is one or more tokens separated
/// by spaces. This enum covers the commonly-used forms; use
/// [`AprsIsFilter::raw`] to drop in any literal filter string for
/// advanced cases.
#[derive(Debug, Clone, PartialEq)]
pub enum AprsIsFilter {
    /// Range filter `r/lat/lon/distance_km` — packets from stations
    /// within the given radius.
    Range {
        /// Centre latitude in degrees (positive = North).
        lat: f64,
        /// Centre longitude in degrees (positive = East).
        lon: f64,
        /// Radius in kilometres.
        distance_km: f64,
    },
    /// Area / box filter `a/lat1/lon1/lat2/lon2` — packets within a
    /// lat/lon bounding box (NW and SE corners).
    Area {
        /// Northwest latitude.
        lat1: f64,
        /// Northwest longitude.
        lon1: f64,
        /// Southeast latitude.
        lat2: f64,
        /// Southeast longitude.
        lon2: f64,
    },
    /// Prefix filter `p/aa/bb/cc` — packets whose source callsign
    /// begins with any of the given prefixes.
    Prefix(Vec<String>),
    /// Budlist filter `b/call1/call2` — packets from exactly these
    /// stations.
    Budlist(Vec<String>),
    /// Object filter `o/obj1/obj2` — object reports with these names.
    Object(Vec<String>),
    /// Type filter `t/poimntqsu` — characters select which frame types
    /// are wanted (p=position, o=object, i=item, m=message, n=nws,
    /// t=telemetry, q=query, s=status, u=user-defined).
    Type(String),
    /// Symbol filter `s/sym1sym2/...` — symbols to include.
    Symbol(String),
    /// "Friend" / range-around-station filter `f/call/distance_km`.
    Friend {
        /// Station to centre on.
        callsign: String,
        /// Distance in km.
        distance_km: f64,
    },
    /// Group message filter `g/name` — bulletins addressed to this
    /// group.
    Group(String),
    /// Raw literal filter string for advanced / uncommon cases.
    Raw(String),
}

impl AprsIsFilter {
    /// Build a raw literal filter expression.
    #[must_use]
    pub fn raw(s: impl Into<String>) -> Self {
        Self::Raw(s.into())
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
            Self::Type(chars) => format!("t/{chars}"),
            Self::Symbol(chars) => format!("s/{chars}"),
            Self::Friend {
                callsign,
                distance_km,
            } => format!("f/{callsign}/{distance_km}"),
            Self::Group(name) => format!("g/{name}"),
            Self::Raw(s) => s.clone(),
        }
    }

    /// Combine multiple filter clauses into a single filter string by
    /// joining with spaces — APRS-IS allows an OR of any number of
    /// clauses in a single `filter` directive.
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
}
