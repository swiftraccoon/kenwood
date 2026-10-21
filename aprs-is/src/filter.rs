//! Checked APRS-IS server filter clauses and ordered expression construction.
//!
//! Supported forms follow <https://www.aprs-is.net/javAPRSFilter.aspx>.
//! Includes add subscriptions; exclusions remove matches from those
//! subscriptions. Default server messaging delivery remains independent.

use std::fmt;

use aprs::{Latitude, Longitude};

use crate::{AprsIsIdentity, QConstruct};

/// Why a filter value or expression cannot be constructed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum FilterError {
    /// A distance was nonfinite, zero, or negative.
    #[error("filter distance must be finite and positive")]
    InvalidDistance,
    /// A list was empty or contained an invalid single-operand name pattern.
    #[error("filter patterns require nonempty ASCII names without slash or whitespace")]
    InvalidPatterns,
    /// A type selector was empty or contained a letter outside `poimqstunw`.
    #[error("filter types must be selected from poimqstunw")]
    InvalidTypes,
    /// A symbol field contained a control, space, slash, or non-ASCII byte.
    #[error("symbol selectors must be printable ASCII; encode slash as vertical bar")]
    InvalidSymbols,
    /// An overlay was outside ASCII digits and uppercase letters.
    #[error("filter overlays must be ASCII digits or uppercase letters")]
    InvalidOverlays,
    /// A raw extension was empty or contained whitespace or control bytes.
    #[error("a raw filter extension must be one nonempty ASCII clause")]
    InvalidExtension,
    /// A Q-construct selector list was empty.
    #[error("a Q-construct filter must select at least one construct")]
    EmptyQConstructs,
    /// North preceded south, or west exceeded east.
    #[error("filter area requires north >= south and west <= east")]
    ReversedArea,
    /// A strict-object clause appeared before another clause.
    #[error("a strict-object filter must be the final clause")]
    StrictObjectOrder,
    /// More than nine range, area, or friend clauses occurred.
    #[error("filter expression exceeds a nine-clause spatial limit")]
    SpatialLimit,
    /// The expression exceeded the complete APRS-IS wire-line limit.
    #[error("filter expression exceeds the APRS-IS line bound")]
    TooLong,
}

/// A finite positive filter radius in kilometres.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct FilterDistance(f64);

impl FilterDistance {
    /// Checks a radius expressed in kilometres.
    ///
    /// # Errors
    ///
    /// Returns [`FilterError::InvalidDistance`] for nonfinite, zero, or negative values.
    pub fn new(kilometres: f64) -> Result<Self, FilterError> {
        if !kilometres.is_finite() || kilometres <= 0.0 {
            return Err(FilterError::InvalidDistance);
        }
        Ok(Self(kilometres))
    }

    /// Returns the radius in kilometres.
    #[must_use]
    pub const fn kilometres(self) -> f64 {
        self.0
    }
}

impl fmt::Display for FilterDistance {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl TryFrom<f64> for FilterDistance {
    type Error = FilterError;
    fn try_from(value: f64) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<FilterDistance> for f64 {
    fn from(value: FilterDistance) -> Self {
        value.kilometres()
    }
}

impl fmt::LowerExp for FilterDistance {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::LowerExp::fmt(&self.0, formatter)
    }
}

impl fmt::UpperExp for FilterDistance {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::UpperExp::fmt(&self.0, formatter)
    }
}

/// A nonempty sequence of printable single-operand name patterns.
///
/// Slash and whitespace are rejected so each name remains one filter operand.
/// Asterisks retain wildcard meaning; object selectors encode slash as `|`
/// and wildcard as `~` according to the server grammar.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FilterPatterns(Vec<String>);

impl FilterPatterns {
    /// Collects checked patterns in their supplied order.
    ///
    /// # Errors
    ///
    /// Returns [`FilterError::InvalidPatterns`] for an empty list, an empty or
    /// oversized pattern, a slash, whitespace, or a non-ASCII byte.
    pub fn new<I, S>(patterns: I) -> Result<Self, FilterError>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let patterns: Vec<String> = patterns.into_iter().map(Into::into).collect();
        if patterns.is_empty()
            || patterns.iter().any(|pattern| {
                !(1..=crate::MAX_IS_LINE_BYTES).contains(&pattern.len())
                    || pattern
                        .bytes()
                        .any(|byte| !byte.is_ascii_graphic() || byte == b'/')
            })
        {
            return Err(FilterError::InvalidPatterns);
        }
        Ok(Self(patterns))
    }

    /// Returns the validated patterns in wire order.
    #[must_use]
    pub fn patterns(&self) -> &[String] {
        &self.0
    }

    /// Iterates over the checked patterns in wire order.
    pub fn iter(&self) -> std::slice::Iter<'_, String> {
        self.0.iter()
    }
}

impl AsRef<[String]> for FilterPatterns {
    fn as_ref(&self) -> &[String] {
        self.patterns()
    }
}

impl IntoIterator for FilterPatterns {
    type Item = String;
    type IntoIter = std::vec::IntoIter<String>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl<'patterns> IntoIterator for &'patterns FilterPatterns {
    type Item = &'patterns String;
    type IntoIter = std::slice::Iter<'patterns, String>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl TryFrom<Vec<String>> for FilterPatterns {
    type Error = FilterError;
    fn try_from(value: Vec<String>) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

/// A nonempty sequence of packet-type selector letters from `poimqstunw`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FilterTypes(String);

impl FilterTypes {
    /// Checks the packet-type selector alphabet.
    ///
    /// # Errors
    ///
    /// Returns [`FilterError::InvalidTypes`] for empty or unknown selectors.
    pub fn new(types: impl Into<String>) -> Result<Self, FilterError> {
        let types = types.into();
        if types.is_empty() || !types.bytes().all(|byte| b"poimqstunw".contains(&byte)) {
            return Err(FilterError::InvalidTypes);
        }
        Ok(Self(types))
    }

    /// Borrows the selector letters in their supplied order.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Printable symbol selectors, with `|` representing a slash symbol.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct FilterSymbols(String);

impl FilterSymbols {
    /// Checks a primary or alternate symbol selector; an empty set is valid.
    ///
    /// # Errors
    ///
    /// Returns [`FilterError::InvalidSymbols`] for slash, whitespace, controls,
    /// or non-ASCII bytes.
    pub fn new(symbols: impl Into<String>) -> Result<Self, FilterError> {
        let symbols = symbols.into();
        if symbols
            .bytes()
            .any(|byte| !byte.is_ascii_graphic() || byte == b'/')
        {
            return Err(FilterError::InvalidSymbols);
        }
        Ok(Self(symbols))
    }

    /// Borrows the already escaped wire selectors.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A possibly empty set of uppercase-letter or digit symbol overlays.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct FilterOverlays(String);

impl FilterOverlays {
    /// Checks overlay selectors, preserving their supplied order.
    ///
    /// # Errors
    ///
    /// Returns [`FilterError::InvalidOverlays`] for any other byte.
    pub fn new(overlays: impl Into<String>) -> Result<Self, FilterError> {
        let overlays = overlays.into();
        if overlays
            .bytes()
            .any(|byte| !byte.is_ascii_uppercase() && !byte.is_ascii_digit())
        {
            return Err(FilterError::InvalidOverlays);
        }
        Ok(Self(overlays))
    }

    /// Borrows the overlay selectors.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A nonempty selection of typed Q-constructs in wire order.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FilterQConstructs(Vec<QConstruct>);

impl FilterQConstructs {
    /// Collects selected constructs without inventing a default selector.
    ///
    /// # Errors
    ///
    /// Returns [`FilterError::EmptyQConstructs`] for an empty iterator.
    pub fn new(constructs: impl IntoIterator<Item = QConstruct>) -> Result<Self, FilterError> {
        let constructs: Vec<_> = constructs.into_iter().collect();
        if constructs.is_empty() {
            return Err(FilterError::EmptyQConstructs);
        }
        Ok(Self(constructs))
    }

    /// Borrows the selected constructs.
    #[must_use]
    pub fn constructs(&self) -> &[QConstruct] {
        &self.0
    }

    /// Iterates over the selected constructs in wire order.
    pub fn iter(&self) -> std::slice::Iter<'_, QConstruct> {
        self.0.iter()
    }
}

impl AsRef<[QConstruct]> for FilterQConstructs {
    fn as_ref(&self) -> &[QConstruct] {
        self.constructs()
    }
}

impl IntoIterator for FilterQConstructs {
    type Item = QConstruct;
    type IntoIter = std::vec::IntoIter<QConstruct>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl<'constructs> IntoIterator for &'constructs FilterQConstructs {
    type Item = &'constructs QConstruct;
    type IntoIter = std::slice::Iter<'constructs, QConstruct>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl TryFrom<Vec<QConstruct>> for FilterQConstructs {
    type Error = FilterError;
    fn try_from(value: Vec<QConstruct>) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

/// One ASCII extension clause whose server-specific grammar is unchecked.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FilterExtension(String);

impl FilterExtension {
    /// Checks extension framing while preserving its unmodified text.
    ///
    /// # Errors
    ///
    /// Returns [`FilterError::InvalidExtension`] for empty, whitespace-bearing,
    /// or non-ASCII clauses, or [`FilterError::TooLong`] above the wire-line bound.
    pub fn new(clause: impl Into<String>) -> Result<Self, FilterError> {
        let clause = clause.into();
        if clause.is_empty() || clause.bytes().any(|byte| !byte.is_ascii_graphic()) {
            return Err(FilterError::InvalidExtension);
        }
        if clause.len() > crate::MAX_IS_LINE_BYTES {
            return Err(FilterError::TooLong);
        }
        Ok(Self(clause))
    }

    /// Borrows the extension's wire text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

macro_rules! checked_text_conversions {
    ($name:ty) => {
        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                self.as_str()
            }
        }
        impl std::str::FromStr for $name {
            type Err = FilterError;
            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::new(value)
            }
        }
        impl TryFrom<String> for $name {
            type Error = FilterError;
            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }
        impl From<$name> for String {
            fn from(value: $name) -> Self {
                value.0
            }
        }
        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(self.as_str())
            }
        }
    };
}

checked_text_conversions!(FilterTypes);
checked_text_conversions!(FilterSymbols);
checked_text_conversions!(FilterOverlays);
checked_text_conversions!(FilterExtension);

/// North-west and south-east corners of a non-wrapping filter rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FilterArea {
    north: Latitude,
    west: Longitude,
    south: Latitude,
    east: Longitude,
}

impl FilterArea {
    /// Checks the ordering of four validated geographic coordinates.
    ///
    /// # Errors
    ///
    /// Returns [`FilterError::ReversedArea`] if north is south of the south edge
    /// or west exceeds east. Use separate areas across the antimeridian.
    pub fn new(
        north: Latitude,
        west: Longitude,
        south: Latitude,
        east: Longitude,
    ) -> Result<Self, FilterError> {
        if north < south || west > east {
            return Err(FilterError::ReversedArea);
        }
        Ok(Self {
            north,
            west,
            south,
            east,
        })
    }

    /// Returns the north-west and south-east corners in that order.
    #[must_use]
    pub const fn corners(self) -> [(Latitude, Longitude); 2] {
        [(self.north, self.west), (self.south, self.east)]
    }
}

/// One checked APRS-IS subscription or exclusion clause.
///
/// [`Self::join`] checks cross-clause limits. Raw extensions preserve vendor
/// syntax after clause-framing validation.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum AprsIsFilter {
    /// `r/lat/lon/km`: centre coordinate and positive radius.
    Range {
        /// Centre latitude in decimal degrees.
        lat: Latitude,
        /// Centre longitude in decimal degrees.
        lon: Longitude,
        /// Radius in kilometres.
        distance_km: FilterDistance,
    },
    /// `a/north/west/south/east`: checked rectangular area.
    Area(FilterArea),
    /// `p/...`: source callsign prefixes.
    Prefix(FilterPatterns),
    /// `b/...`: exact source patterns with optional wildcards.
    Budlist(FilterPatterns),
    /// `o/...`: exact object names with optional wildcards.
    Object(FilterPatterns),
    /// `os/...`: exact object or item patterns; this clause must come last.
    StrictObject(FilterPatterns),
    /// `t/...`: packet-type selectors.
    Type(FilterTypes),
    /// `t/types/call/km`: packet types around a station.
    TypeAround {
        /// Packet-type selectors.
        types: FilterTypes,
        /// Centre station identity.
        callsign: AprsIsIdentity,
        /// Radius in kilometres.
        distance_km: FilterDistance,
    },
    /// `s/primary/alternate/overlays`: table and overlay selectors.
    Symbol {
        /// Primary-table selectors.
        primary: FilterSymbols,
        /// Alternate-table selectors.
        alternate: FilterSymbols,
        /// Overlay selectors.
        overlay: FilterOverlays,
    },
    /// `d/...`: repeated stations, with optional wildcards.
    Digi(FilterPatterns),
    /// `e/...`: identities immediately following the Q-construct.
    Entry(FilterPatterns),
    /// `q/...`: constructs selected by case-sensitive final letters.
    QConstruct(FilterQConstructs),
    /// `q//I`: positions from gates identified by `qAr`, `qAo`, or `qAR`.
    IgatePositions,
    /// `m/km`: radius around the client's last reported position.
    MyRange {
        /// Radius in kilometres.
        distance_km: FilterDistance,
    },
    /// `g/...`: message addressees, with optional wildcards.
    Group(FilterPatterns),
    /// `f/call/km`: radius around a friend's last reported position.
    Friend {
        /// Centre station identity.
        callsign: AprsIsIdentity,
        /// Radius in kilometres.
        distance_km: FilterDistance,
    },
    /// `u/...`: packet destination patterns.
    Unproto(FilterPatterns),
    /// Excludes the inner subscription's matches; nested negation cancels.
    Negated(Box<Self>),
    /// Checked clause framing with explicitly unchecked extension grammar.
    Raw(FilterExtension),
}

impl AprsIsFilter {
    /// Constructs a raw extension after clause-framing validation.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`FilterExtension::new`].
    pub fn raw(clause: impl Into<String>) -> Result<Self, FilterError> {
        Ok(Self::Raw(FilterExtension::new(clause)?))
    }

    /// Toggles the clause's exclusion prefix.
    #[must_use]
    pub fn negated(inner: Self) -> Self {
        if let Self::Negated(inner) = inner {
            *inner
        } else {
            Self::Negated(Box::new(inner))
        }
    }

    /// Allocates the clause's canonical wire representation.
    #[must_use]
    pub fn to_wire(&self) -> String {
        match self {
            Self::Range {
                lat,
                lon,
                distance_km,
            } => format!("r/{}/{}/{distance_km}", lat.as_degrees(), lon.as_degrees()),
            Self::Area(area) => format!(
                "a/{}/{}/{}/{}",
                area.north.as_degrees(),
                area.west.as_degrees(),
                area.south.as_degrees(),
                area.east.as_degrees()
            ),
            Self::Prefix(parts) => format!("p/{}", parts.0.join("/")),
            Self::Budlist(parts) => format!("b/{}", parts.0.join("/")),
            Self::Object(parts) => format!("o/{}", parts.0.join("/")),
            Self::StrictObject(parts) => format!("os/{}", parts.0.join("/")),
            Self::Type(types) => format!("t/{}", types.as_str()),
            Self::TypeAround {
                types,
                callsign,
                distance_km,
            } => format!("t/{}/{callsign}/{distance_km}", types.as_str()),
            Self::Symbol {
                primary,
                alternate,
                overlay,
            } => format!(
                "s/{}/{}/{}",
                primary.as_str(),
                alternate.as_str(),
                overlay.as_str()
            ),
            Self::Digi(parts) => format!("d/{}", parts.0.join("/")),
            Self::Entry(parts) => format!("e/{}", parts.0.join("/")),
            Self::QConstruct(constructs) => {
                let letters: String = constructs
                    .0
                    .iter()
                    .filter_map(|construct| construct.as_str().chars().last())
                    .collect();
                format!("q/{letters}")
            }
            Self::IgatePositions => "q//I".to_owned(),
            Self::MyRange { distance_km } => format!("m/{distance_km}"),
            Self::Group(parts) => format!("g/{}", parts.0.join("/")),
            Self::Friend {
                callsign,
                distance_km,
            } => format!("f/{callsign}/{distance_km}"),
            Self::Unproto(parts) => format!("u/{}", parts.0.join("/")),
            Self::Negated(inner) => {
                let wire = inner.to_wire();
                wire.strip_prefix('-')
                    .map_or_else(|| format!("-{wire}"), str::to_owned)
            }
            Self::Raw(extension) => extension.as_str().to_owned(),
        }
    }

    /// Joins streamed clauses, enforcing spatial limits and strict-object order.
    ///
    /// # Errors
    ///
    /// Returns [`FilterError::StrictObjectOrder`] if another clause follows `os`,
    /// [`FilterError::SpatialLimit`] above nine range, area, or friend clauses,
    /// or [`FilterError::TooLong`] above the wire-line bound. The configuration
    /// builder additionally checks the login's total length.
    pub fn join<'filters, I>(filters: I) -> Result<String, FilterError>
    where
        I: IntoIterator<Item = &'filters Self>,
    {
        let mut expression = String::new();
        let mut strict_seen = false;
        let mut spatial_counts = [0_u8; 3];
        for filter in filters {
            if strict_seen {
                return Err(FilterError::StrictObjectOrder);
            }
            let wire = filter.to_wire();
            let body = wire.strip_prefix('-').unwrap_or(&wire);
            strict_seen = body.starts_with("os/");
            let counter = match body.split('/').next() {
                Some("r") => spatial_counts.get_mut(0),
                Some("a") => spatial_counts.get_mut(1),
                Some("f") => spatial_counts.get_mut(2),
                _ => None,
            };
            if let Some(counter) = counter {
                *counter += 1;
                if *counter > 9 {
                    return Err(FilterError::SpatialLimit);
                }
            }
            if !expression.is_empty() {
                expression.push(' ');
            }
            if expression.len().saturating_add(wire.len()) > crate::MAX_IS_LINE_BYTES {
                return Err(FilterError::TooLong);
            }
            expression.push_str(&wire);
        }
        Ok(expression)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AprsIsAddressField;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn spatial_domains_and_wire_units() -> TestResult {
        for invalid in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 0.0, -1.0] {
            assert_eq!(
                FilterDistance::new(invalid),
                Err(FilterError::InvalidDistance)
            );
        }
        let range = AprsIsFilter::Range {
            lat: Latitude::new(35.25)?,
            lon: Longitude::new(-97.75)?,
            distance_km: FilterDistance::new(100.0)?,
        };
        assert_eq!(range.to_wire(), "r/35.25/-97.75/100");
        assert_eq!(
            FilterArea::new(
                Latitude::new(0.0)?,
                Longitude::new(1.0)?,
                Latitude::new(1.0)?,
                Longitude::new(2.0)?
            ),
            Err(FilterError::ReversedArea)
        );
        let area = FilterArea::new(
            Latitude::new(2.0)?,
            Longitude::new(-2.0)?,
            Latitude::new(1.0)?,
            Longitude::new(-1.0)?,
        )?;
        assert_eq!(AprsIsFilter::Area(area).to_wire(), "a/2/-2/1/-1");
        Ok(())
    }

    #[test]
    fn name_type_and_symbol_selectors_preserve_their_grammar() -> TestResult {
        let patterns = FilterPatterns::new(["KK", "W1*"])?;
        for (filter, expected) in [
            (AprsIsFilter::Prefix(patterns.clone()), "p/KK/W1*"),
            (AprsIsFilter::Budlist(patterns.clone()), "b/KK/W1*"),
            (AprsIsFilter::Object(patterns.clone()), "o/KK/W1*"),
            (AprsIsFilter::Digi(patterns.clone()), "d/KK/W1*"),
            (AprsIsFilter::Entry(patterns.clone()), "e/KK/W1*"),
            (AprsIsFilter::Group(patterns.clone()), "g/KK/W1*"),
            (AprsIsFilter::Unproto(patterns), "u/KK/W1*"),
        ] {
            assert_eq!(filter.to_wire(), expected);
        }
        assert_eq!(
            AprsIsFilter::Type(FilterTypes::new("poimqstunw")?).to_wire(),
            "t/poimqstunw"
        );
        for invalid in ["", "c", "po/m", "p o", "é"] {
            assert!(FilterTypes::new(invalid).is_err());
        }
        for invalid in ["", "NAME/next", "bad name", "é"] {
            assert!(FilterPatterns::new([invalid]).is_err());
        }
        assert_eq!(
            FilterPatterns::new(["ABCDEFGHI*"])?.patterns(),
            &["ABCDEFGHI*".to_owned()]
        );
        assert!(FilterPatterns::new(["X".repeat(crate::MAX_IS_LINE_BYTES + 1)]).is_err());
        assert!(FilterPatterns::new(std::iter::empty::<String>()).is_err());
        let symbol = AprsIsFilter::Symbol {
            primary: FilterSymbols::new(">|")?,
            alternate: FilterSymbols::default(),
            overlay: FilterOverlays::new("T1")?,
        };
        assert_eq!(symbol.to_wire(), "s/>|//T1");
        assert!(FilterSymbols::new("/").is_err());
        assert!(FilterOverlays::new("a").is_err());
        Ok(())
    }

    #[test]
    fn station_qconstruct_and_extension_forms() -> TestResult {
        let callsign = AprsIsIdentity::new("N0CALL", AprsIsAddressField::Source)?;
        let radius = FilterDistance::new(50.0)?;
        assert_eq!(
            AprsIsFilter::TypeAround {
                types: FilterTypes::new("po")?,
                callsign: callsign.clone(),
                distance_km: radius
            }
            .to_wire(),
            "t/po/N0CALL/50"
        );
        assert_eq!(
            AprsIsFilter::Friend {
                callsign,
                distance_km: radius
            }
            .to_wire(),
            "f/N0CALL/50"
        );
        assert_eq!(
            AprsIsFilter::MyRange {
                distance_km: radius
            }
            .to_wire(),
            "m/50"
        );
        assert_eq!(
            AprsIsFilter::QConstruct(FilterQConstructs::new([QConstruct::QAr, QConstruct::QAR])?)
                .to_wire(),
            "q/rR"
        );
        assert!(FilterQConstructs::new([]).is_err());
        assert_eq!(AprsIsFilter::IgatePositions.to_wire(), "q//I");
        assert_eq!(AprsIsFilter::raw("vendor/one")?.to_wire(), "vendor/one");
        for invalid in ["", "m/1\r\nforged", "m/1 t/p", "m/1\0"] {
            assert!(AprsIsFilter::raw(invalid).is_err());
        }
        Ok(())
    }

    #[test]
    fn joining_checks_order_limits_and_nested_negation() -> TestResult {
        let strict = AprsIsFilter::StrictObject(FilterPatterns::new(["FIRE", "EMS"])?);
        let ordinary = AprsIsFilter::Type(FilterTypes::new("p")?);
        assert_eq!(AprsIsFilter::join([&ordinary, &strict])?, "t/p os/FIRE/EMS");
        assert_eq!(
            AprsIsFilter::join([&strict, &ordinary]),
            Err(FilterError::StrictObjectOrder)
        );
        assert_eq!(
            AprsIsFilter::join([&strict, &strict]),
            Err(FilterError::StrictObjectOrder)
        );
        let spatial = AprsIsFilter::raw("r/1/2/3")?;
        assert!(AprsIsFilter::join(std::iter::repeat_n(&spatial, 9)).is_ok());
        assert_eq!(
            AprsIsFilter::join(std::iter::repeat_n(&spatial, 10)),
            Err(FilterError::SpatialLimit)
        );
        let negated = AprsIsFilter::negated(ordinary.clone());
        assert_eq!(negated.to_wire(), "-t/p");
        assert_eq!(AprsIsFilter::negated(negated).to_wire(), ordinary.to_wire());
        assert_eq!(
            AprsIsFilter::Negated(Box::new(AprsIsFilter::Negated(Box::new(ordinary)))).to_wire(),
            "t/p"
        );
        Ok(())
    }
}
