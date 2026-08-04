//! Kenwood TH-D75 D-STAR repeater-catalog TSV support.
//!
//! Kenwood distributes one source catalog containing entries for every
//! supported radio region. The catalog has 31 columns and can contain more
//! than the radio's 1,500-entry capacity. Importing on the radio first selects
//! a model and geographic region; the capacity applies to that selected
//! materialization, not to the source catalog.
//!
//! # Location
//!
//! `/KENWOOD/TH-D75/SETTING/RPT_LIST/*.tsv`
//!
//! # Encodings
//!
//! Kenwood's English-display catalog is UTF-16LE with a BOM. Its
//! Japanese-display catalog is Shift-JIS without a BOM. Both are decoded
//! strictly; malformed byte sequences are rejected rather than replaced.

use std::borrow::Cow;
use std::fmt;

use encoding_rs::SHIFT_JIS;

use super::{SdCardError, TsvField, decode_utf16le_bom, encode_utf16le_bom};
use crate::sdcard::config::ConfigFileModel;
use crate::types::{DstarCallsign, Frequency};

/// Number of fields in Kenwood's TH-D75 repeater-catalog TSV format.
pub const REPEATER_CATALOG_COLUMNS: usize = 31;

/// Maximum number of D-STAR repeater records materialized into one TH-D75.
pub const MAX_REPEATER_ENTRIES: usize = 1_500;

/// Exact header used by Kenwood's TH-D75 repeater catalogs.
pub const REPEATER_CATALOG_HEADER: &str = "Wn\tWorld Region\tCn\tCountry\tGn\tGroup\t\
Callsign\tGateway\tLockout\tName\tSub Name\tFrequency\tShift\tOffset\tMode\t\
Uplink Tone\tDownlink Tone\tPosition\tLat DD\tLat MM.mm\tN/S\tLon DDD\t\
Lon MM.mm\tE/W\tTime Zone\tTH-D74A\tTH-D74E\tTH-D74\tAux 1\tAux 2\tAux 3";

const FILE_TYPE: &str = "Kenwood repeater catalog";
const SUPPORTED_ENCODINGS: &str = "UTF-16LE with BOM or Shift-JIS without BOM";

/// An exact `Off`/`On` field in a Kenwood repeater catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum RepeaterCatalogFlag {
    /// The option or model selection is disabled.
    #[default]
    Off,
    /// The option or model selection is enabled.
    On,
}

impl RepeaterCatalogFlag {
    /// Return the exact Kenwood TSV spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::On => "On",
        }
    }

    /// Return whether this field is `On`.
    #[must_use]
    pub const fn is_on(self) -> bool {
        matches!(self, Self::On)
    }
}

impl fmt::Display for RepeaterCatalogFlag {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Frequency shift direction in a Kenwood repeater-catalog row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum RepeaterShift {
    /// No transmit-frequency shift (`Off`).
    #[default]
    Off,
    /// Transmit above the receive frequency (`+`).
    Positive,
    /// Transmit below the receive frequency (`-`).
    Negative,
}

impl RepeaterShift {
    /// Return the exact Kenwood TSV spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::Positive => "+",
            Self::Negative => "-",
        }
    }
}

impl fmt::Display for RepeaterShift {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Operating mode accepted by the D-STAR repeater catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RepeaterCatalogMode {
    /// D-STAR digital voice/data repeater.
    Digital,
}

impl RepeaterCatalogMode {
    /// Return the exact Kenwood TSV spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Digital => "Digital",
        }
    }
}

impl fmt::Display for RepeaterCatalogMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Accuracy classification for a repeater's catalog position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RepeaterPositionAccuracy {
    /// No usable position is supplied.
    Invalid,
    /// The position is approximate.
    Approximate,
    /// The position is exact.
    Exact,
}

impl RepeaterPositionAccuracy {
    /// Return the exact Kenwood TSV spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Invalid => "Invalid",
            Self::Approximate => "Approx.",
            Self::Exact => "Exact",
        }
    }
}

impl fmt::Display for RepeaterPositionAccuracy {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// North/south hemisphere field in a catalog latitude.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LatitudeHemisphere {
    /// Northern hemisphere.
    North,
    /// Southern hemisphere.
    South,
}

impl LatitudeHemisphere {
    /// Return the exact Kenwood TSV spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::North => "N",
            Self::South => "S",
        }
    }
}

/// East/west hemisphere field in a catalog longitude.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LongitudeHemisphere {
    /// Eastern hemisphere.
    East,
    /// Western hemisphere.
    West,
}

impl LongitudeHemisphere {
    /// Return the exact Kenwood TSV spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::East => "E",
            Self::West => "W",
        }
    }
}

/// Decimal minutes in a repeater position, represented exactly to hundredths.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RepeaterCoordinateMinutes {
    hundredths: u16,
    source: TsvField,
}

impl RepeaterCoordinateMinutes {
    /// Return the coordinate minutes multiplied by 100.
    #[must_use]
    pub const fn hundredths(&self) -> u16 {
        self.hundredths
    }

    /// Return the exact field spelling from the source catalog.
    #[must_use]
    pub const fn as_str(&self) -> &str {
        self.source.as_str()
    }
}

/// Signed UTC offset from a repeater-catalog row.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RepeaterUtcOffset {
    minutes: i16,
    source: TsvField,
}

impl RepeaterUtcOffset {
    /// Return signed minutes east of UTC.
    #[must_use]
    pub const fn minutes(&self) -> i16 {
        self.minutes
    }

    /// Return the exact `+HH:MM` or `-HH:MM` source field.
    #[must_use]
    pub const fn as_str(&self) -> &str {
        self.source.as_str()
    }
}

/// Per-model inclusion flags carried by Kenwood's shared catalog.
///
/// Kenwood retained the column labels `TH-D74A`, `TH-D74E`, and `TH-D74` in
/// the catalog used by both the TH-D74 and TH-D75. The TH-D75 import menu maps
/// those flags to the corresponding TH-D75A, TH-D75E, and region-neutral
/// selections.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RepeaterModelFlags {
    americas: RepeaterCatalogFlag,
    europe: RepeaterCatalogFlag,
    region_neutral: RepeaterCatalogFlag,
}

impl RepeaterModelFlags {
    /// Return the Americas-model inclusion flag.
    #[must_use]
    pub const fn th_d75a(self) -> RepeaterCatalogFlag {
        self.americas
    }

    /// Return the European-model inclusion flag.
    #[must_use]
    pub const fn th_d75e(self) -> RepeaterCatalogFlag {
        self.europe
    }

    /// Return the region-neutral-model inclusion flag.
    #[must_use]
    pub const fn th_d75(self) -> RepeaterCatalogFlag {
        self.region_neutral
    }

    /// Return whether the catalog row is enabled for `model`.
    #[must_use]
    pub const fn supports(self, model: ConfigFileModel) -> bool {
        match model {
            ConfigFileModel::ThD75A => self.americas.is_on(),
            ConfigFileModel::ThD75E => self.europe.is_on(),
            ConfigFileModel::RegionNeutral => self.region_neutral.is_on(),
        }
    }
}

/// One losslessly represented row from Kenwood's 31-column source catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepeaterCatalogEntry {
    world_region_number: u8,
    world_region: TsvField,
    country_number: u8,
    country: TsvField,
    group_number: u8,
    group: TsvField,
    callsign_source: TsvField,
    callsign: DstarCallsign,
    gateway_source: TsvField,
    gateway: DstarCallsign,
    lockout: RepeaterCatalogFlag,
    name: TsvField,
    sub_name: TsvField,
    frequency_source: TsvField,
    frequency: Frequency,
    shift: RepeaterShift,
    offset_source: TsvField,
    offset: Frequency,
    mode: RepeaterCatalogMode,
    uplink_tone: TsvField,
    downlink_tone: TsvField,
    position_accuracy: RepeaterPositionAccuracy,
    latitude_degrees: u8,
    latitude_minutes: RepeaterCoordinateMinutes,
    latitude_hemisphere: LatitudeHemisphere,
    longitude_degrees: u8,
    longitude_minutes: RepeaterCoordinateMinutes,
    longitude_hemisphere: LongitudeHemisphere,
    time_zone: RepeaterUtcOffset,
    model_flags: RepeaterModelFlags,
    aux_1: TsvField,
    aux_2: TsvField,
    aux_3: TsvField,
}

impl RepeaterCatalogEntry {
    /// Return the world-region number (`Wn`).
    #[must_use]
    pub const fn world_region_number(&self) -> u8 {
        self.world_region_number
    }

    /// Return the world-region label.
    #[must_use]
    pub const fn world_region(&self) -> &str {
        self.world_region.as_str()
    }

    /// Return the country number (`Cn`).
    #[must_use]
    pub const fn country_number(&self) -> u8 {
        self.country_number
    }

    /// Return the country label.
    #[must_use]
    pub const fn country(&self) -> &str {
        self.country.as_str()
    }

    /// Return the repeater-group number (`Gn`).
    #[must_use]
    pub const fn group_number(&self) -> u8 {
        self.group_number
    }

    /// Return the repeater-group label.
    #[must_use]
    pub const fn group(&self) -> &str {
        self.group.as_str()
    }

    /// Return the typed repeater callsign.
    #[must_use]
    pub const fn callsign(&self) -> &DstarCallsign {
        &self.callsign
    }

    /// Return the typed gateway callsign, which may be empty.
    #[must_use]
    pub const fn gateway(&self) -> &DstarCallsign {
        &self.gateway
    }

    /// Return the lockout flag.
    #[must_use]
    pub const fn lockout(&self) -> RepeaterCatalogFlag {
        self.lockout
    }

    /// Return the display name.
    #[must_use]
    pub const fn name(&self) -> &str {
        self.name.as_str()
    }

    /// Return the display sub-name.
    #[must_use]
    pub const fn sub_name(&self) -> &str {
        self.sub_name.as_str()
    }

    /// Return the receive frequency.
    #[must_use]
    pub const fn frequency(&self) -> Frequency {
        self.frequency
    }

    /// Return the shift direction.
    #[must_use]
    pub const fn shift(&self) -> RepeaterShift {
        self.shift
    }

    /// Return the shift offset.
    #[must_use]
    pub const fn offset(&self) -> Frequency {
        self.offset
    }

    /// Return the repeater operating mode.
    #[must_use]
    pub const fn mode(&self) -> RepeaterCatalogMode {
        self.mode
    }

    /// Return the uplink-tone field exactly as supplied.
    #[must_use]
    pub const fn uplink_tone(&self) -> &str {
        self.uplink_tone.as_str()
    }

    /// Return the downlink-tone field exactly as supplied.
    #[must_use]
    pub const fn downlink_tone(&self) -> &str {
        self.downlink_tone.as_str()
    }

    /// Return the position-accuracy classification.
    #[must_use]
    pub const fn position_accuracy(&self) -> RepeaterPositionAccuracy {
        self.position_accuracy
    }

    /// Return whole latitude degrees.
    #[must_use]
    pub const fn latitude_degrees(&self) -> u8 {
        self.latitude_degrees
    }

    /// Return latitude decimal minutes.
    #[must_use]
    pub const fn latitude_minutes(&self) -> &RepeaterCoordinateMinutes {
        &self.latitude_minutes
    }

    /// Return the latitude hemisphere.
    #[must_use]
    pub const fn latitude_hemisphere(&self) -> LatitudeHemisphere {
        self.latitude_hemisphere
    }

    /// Return whole longitude degrees.
    #[must_use]
    pub const fn longitude_degrees(&self) -> u8 {
        self.longitude_degrees
    }

    /// Return longitude decimal minutes.
    #[must_use]
    pub const fn longitude_minutes(&self) -> &RepeaterCoordinateMinutes {
        &self.longitude_minutes
    }

    /// Return the longitude hemisphere.
    #[must_use]
    pub const fn longitude_hemisphere(&self) -> LongitudeHemisphere {
        self.longitude_hemisphere
    }

    /// Return the station time-zone offset.
    #[must_use]
    pub const fn time_zone(&self) -> &RepeaterUtcOffset {
        &self.time_zone
    }

    /// Return all per-model source-catalog inclusion flags.
    #[must_use]
    pub const fn model_flags(&self) -> RepeaterModelFlags {
        self.model_flags
    }

    /// Return `Aux 1` exactly as supplied.
    #[must_use]
    pub const fn aux_1(&self) -> &str {
        self.aux_1.as_str()
    }

    /// Return `Aux 2` exactly as supplied.
    #[must_use]
    pub const fn aux_2(&self) -> &str {
        self.aux_2.as_str()
    }

    /// Return `Aux 3` exactly as supplied.
    #[must_use]
    pub const fn aux_3(&self) -> &str {
        self.aux_3.as_str()
    }

    const fn matches_region(&self, region: RepeaterCatalogRegion) -> bool {
        match region {
            RepeaterCatalogRegion::All => true,
            RepeaterCatalogRegion::World { world_region } => {
                self.world_region_number == world_region
            }
            RepeaterCatalogRegion::Country {
                world_region,
                country,
            } => self.world_region_number == world_region && self.country_number == country,
            RepeaterCatalogRegion::Group {
                world_region,
                country,
                group,
            } => {
                self.world_region_number == world_region
                    && self.country_number == country
                    && self.group_number == group
            }
        }
    }
}

/// An unfiltered Kenwood source catalog.
///
/// Source catalogs may legitimately exceed [`MAX_REPEATER_ENTRIES`]. Call
/// [`select`](Self::select) to create a capacity-checked radio list.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RepeaterCatalog {
    entries: Vec<RepeaterCatalogEntry>,
}

impl RepeaterCatalog {
    /// Return every source-catalog row.
    #[must_use]
    pub fn entries(&self) -> &[RepeaterCatalogEntry] {
        &self.entries
    }

    /// Return the source row count without applying radio capacity.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.entries.len()
    }

    /// Return whether the source catalog has no rows.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Materialize the model/region subset selected by the radio's importer.
    ///
    /// # Errors
    ///
    /// Returns [`SdCardError::EntryCount`] if the selected subset exceeds the
    /// TH-D75's 1,500-entry capacity. The unfiltered source size is not capped.
    pub fn select(
        &self,
        selection: RepeaterCatalogSelection,
    ) -> Result<SelectedRepeaterList, SdCardError> {
        let entries: Vec<_> = self
            .entries
            .iter()
            .filter(|entry| {
                entry.model_flags.supports(selection.model)
                    && entry.matches_region(selection.region)
            })
            .cloned()
            .collect();

        if entries.len() > MAX_REPEATER_ENTRIES {
            return Err(SdCardError::EntryCount {
                file_type: "selected D-STAR repeater list",
                maximum: MAX_REPEATER_ENTRIES,
                actual: entries.len(),
            });
        }

        Ok(SelectedRepeaterList { selection, entries })
    }
}

/// Geographic scope selected while importing a Kenwood source catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum RepeaterCatalogRegion {
    /// Include all regions enabled for the selected model.
    #[default]
    All,
    /// Select one `Wn` world region.
    World {
        /// `Wn` value from the source catalog.
        world_region: u8,
    },
    /// Select one country within a world region.
    Country {
        /// `Wn` value from the source catalog.
        world_region: u8,
        /// `Cn` value from the source catalog.
        country: u8,
    },
    /// Select one repeater group within a country.
    Group {
        /// `Wn` value from the source catalog.
        world_region: u8,
        /// `Cn` value from the source catalog.
        country: u8,
        /// `Gn` value from the source catalog.
        group: u8,
    },
}

/// Model and geographic selection applied to a source catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RepeaterCatalogSelection {
    model: ConfigFileModel,
    region: RepeaterCatalogRegion,
}

impl RepeaterCatalogSelection {
    /// Select every catalog region enabled for `model`.
    #[must_use]
    pub const fn new(model: ConfigFileModel) -> Self {
        Self {
            model,
            region: RepeaterCatalogRegion::All,
        }
    }

    /// Select `region` within the rows enabled for `model`.
    #[must_use]
    pub const fn with_region(model: ConfigFileModel, region: RepeaterCatalogRegion) -> Self {
        Self { model, region }
    }

    /// Return the selected radio model.
    #[must_use]
    pub const fn model(self) -> ConfigFileModel {
        self.model
    }

    /// Return the selected geographic scope.
    #[must_use]
    pub const fn region(self) -> RepeaterCatalogRegion {
        self.region
    }
}

/// Capacity-checked rows materialized for one TH-D75 model and region.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedRepeaterList {
    selection: RepeaterCatalogSelection,
    entries: Vec<RepeaterCatalogEntry>,
}

impl SelectedRepeaterList {
    /// Return the model/region selection used to create this list.
    #[must_use]
    pub const fn selection(&self) -> RepeaterCatalogSelection {
        self.selection
    }

    /// Return the selected rows.
    #[must_use]
    pub fn entries(&self) -> &[RepeaterCatalogEntry] {
        &self.entries
    }

    /// Return the number of selected rows.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.entries.len()
    }

    /// Return whether the selection contains no rows.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Parse an official Kenwood 31-column repeater source catalog.
///
/// The exact header is required. UTF-16LE-with-BOM and strict Shift-JIS are
/// supported. No 1,500-entry limit is applied at this stage because official
/// source catalogs contain multiple model/region subsets.
///
/// # Errors
///
/// Returns [`SdCardError`] for an unsupported encoding, mismatched header,
/// malformed field, wrong column count, or invalid callsign. Use
/// [`RepeaterCatalog::select`] to enforce radio capacity.
pub fn parse_repeater_catalog(data: &[u8]) -> Result<RepeaterCatalog, SdCardError> {
    let text = decode_catalog(data)?;
    let mut lines = text.lines();
    let actual_header = lines.next().unwrap_or_default();
    if actual_header != REPEATER_CATALOG_HEADER {
        return Err(SdCardError::HeaderMismatch {
            file_type: FILE_TYPE,
            expected: REPEATER_CATALOG_HEADER.to_owned(),
            actual: actual_header.to_owned(),
        });
    }

    let mut entries = Vec::new();
    for (line_index, line) in lines.enumerate() {
        if line.is_empty() {
            continue;
        }
        let line_number = line_index + 2;
        let columns: Vec<_> = line.split('\t').collect();
        let actual = columns.len();
        let raw = RawCatalogRow::from_columns(&columns).ok_or(SdCardError::ColumnCount {
            line: line_number,
            expected: REPEATER_CATALOG_COLUMNS,
            actual,
        })?;
        entries.push(parse_entry(&raw, line_number)?);
    }

    Ok(RepeaterCatalog { entries })
}

/// Encode an unfiltered source catalog as UTF-16LE with a BOM.
#[must_use]
pub fn write_repeater_catalog(catalog: &RepeaterCatalog) -> Vec<u8> {
    encode_entries(catalog.entries())
}

/// Encode a selected, capacity-checked radio list as UTF-16LE with a BOM.
#[must_use]
pub fn write_selected_repeater_list(list: &SelectedRepeaterList) -> Vec<u8> {
    encode_entries(list.entries())
}

fn decode_catalog(data: &[u8]) -> Result<String, SdCardError> {
    if data.starts_with(&[0xFF, 0xFE]) {
        return decode_utf16le_bom(data);
    }
    if data.starts_with(&[0xFE, 0xFF]) {
        return Err(SdCardError::UnsupportedTextEncoding {
            file_type: FILE_TYPE,
            expected: SUPPORTED_ENCODINGS,
        });
    }

    SHIFT_JIS
        .decode_without_bom_handling_and_without_replacement(data)
        .map(Cow::into_owned)
        .ok_or(SdCardError::UnsupportedTextEncoding {
            file_type: FILE_TYPE,
            expected: SUPPORTED_ENCODINGS,
        })
}

#[derive(Debug, Clone, Copy)]
struct RawCatalogRow<'a> {
    wn: &'a str,
    world_region: &'a str,
    cn: &'a str,
    country: &'a str,
    gn: &'a str,
    group: &'a str,
    callsign: &'a str,
    gateway: &'a str,
    lockout: &'a str,
    name: &'a str,
    sub_name: &'a str,
    frequency: &'a str,
    shift: &'a str,
    offset: &'a str,
    mode: &'a str,
    uplink_tone: &'a str,
    downlink_tone: &'a str,
    position: &'a str,
    lat_degrees: &'a str,
    lat_minutes: &'a str,
    north_south: &'a str,
    lon_degrees: &'a str,
    lon_minutes: &'a str,
    east_west: &'a str,
    time_zone: &'a str,
    americas_model: &'a str,
    european_model: &'a str,
    region_neutral_model: &'a str,
    aux_1: &'a str,
    aux_2: &'a str,
    aux_3: &'a str,
}

impl<'a> RawCatalogRow<'a> {
    fn from_columns(columns: &[&'a str]) -> Option<Self> {
        let &[
            wn,
            world_region,
            cn,
            country,
            gn,
            group,
            callsign,
            gateway,
            lockout,
            name,
            sub_name,
            frequency,
            shift,
            offset,
            mode,
            uplink_tone,
            downlink_tone,
            position,
            lat_degrees,
            lat_minutes,
            north_south,
            lon_degrees,
            lon_minutes,
            east_west,
            time_zone,
            americas_model,
            european_model,
            region_neutral_model,
            aux_1,
            aux_2,
            aux_3,
        ] = columns
        else {
            return None;
        };

        Some(Self {
            wn,
            world_region,
            cn,
            country,
            gn,
            group,
            callsign,
            gateway,
            lockout,
            name,
            sub_name,
            frequency,
            shift,
            offset,
            mode,
            uplink_tone,
            downlink_tone,
            position,
            lat_degrees,
            lat_minutes,
            north_south,
            lon_degrees,
            lon_minutes,
            east_west,
            time_zone,
            americas_model,
            european_model,
            region_neutral_model,
            aux_1,
            aux_2,
            aux_3,
        })
    }
}

fn parse_entry(raw: &RawCatalogRow<'_>, line: usize) -> Result<RepeaterCatalogEntry, SdCardError> {
    let world_region_number = parse_positive_u8(raw.wn, line, "Wn")?;
    let country_number = parse_positive_u8(raw.cn, line, "Cn")?;
    let group_number = parse_positive_u8(raw.gn, line, "Gn")?;
    let (callsign_source, callsign) = parse_callsign(raw.callsign, line, "Callsign", false)?;
    let (gateway_source, gateway) = parse_callsign(raw.gateway, line, "Gateway", true)?;
    let (frequency_source, frequency) = parse_frequency(raw.frequency, line, "Frequency", false)?;
    let (offset_source, offset) = parse_frequency(raw.offset, line, "Offset", true)?;
    let latitude_degrees = parse_bounded_u8(raw.lat_degrees, line, "Lat DD", 90)?;
    let latitude_minutes = parse_coordinate_minutes(raw.lat_minutes, line, "Lat MM.mm")?;
    let longitude_degrees = parse_bounded_u8(raw.lon_degrees, line, "Lon DDD", 180)?;
    let longitude_minutes = parse_coordinate_minutes(raw.lon_minutes, line, "Lon MM.mm")?;

    validate_coordinate_endpoint(latitude_degrees, &latitude_minutes, 90, line, "Latitude")?;
    validate_coordinate_endpoint(
        longitude_degrees,
        &longitude_minutes,
        180,
        line,
        "Longitude",
    )?;

    Ok(RepeaterCatalogEntry {
        world_region_number,
        world_region: parse_text(raw.world_region, line, "World Region")?,
        country_number,
        country: parse_text(raw.country, line, "Country")?,
        group_number,
        group: parse_text(raw.group, line, "Group")?,
        callsign_source,
        callsign,
        gateway_source,
        gateway,
        lockout: parse_flag(raw.lockout, line, "Lockout")?,
        name: parse_text(raw.name, line, "Name")?,
        sub_name: parse_text(raw.sub_name, line, "Sub Name")?,
        frequency_source,
        frequency,
        shift: parse_shift(raw.shift, line)?,
        offset_source,
        offset,
        mode: parse_mode(raw.mode, line)?,
        uplink_tone: parse_text(raw.uplink_tone, line, "Uplink Tone")?,
        downlink_tone: parse_text(raw.downlink_tone, line, "Downlink Tone")?,
        position_accuracy: parse_position(raw.position, line)?,
        latitude_degrees,
        latitude_minutes,
        latitude_hemisphere: parse_latitude_hemisphere(raw.north_south, line)?,
        longitude_degrees,
        longitude_minutes,
        longitude_hemisphere: parse_longitude_hemisphere(raw.east_west, line)?,
        time_zone: parse_utc_offset(raw.time_zone, line)?,
        model_flags: RepeaterModelFlags {
            americas: parse_flag(raw.americas_model, line, "TH-D74A")?,
            europe: parse_flag(raw.european_model, line, "TH-D74E")?,
            region_neutral: parse_flag(raw.region_neutral_model, line, "TH-D74")?,
        },
        aux_1: parse_text(raw.aux_1, line, "Aux 1")?,
        aux_2: parse_text(raw.aux_2, line, "Aux 2")?,
        aux_3: parse_text(raw.aux_3, line, "Aux 3")?,
    })
}

fn invalid_field(line: usize, column: &str, detail: impl Into<String>) -> SdCardError {
    SdCardError::InvalidField {
        line,
        column: column.to_owned(),
        detail: detail.into(),
    }
}

fn parse_text(value: &str, line: usize, column: &str) -> Result<TsvField, SdCardError> {
    TsvField::new(value).map_err(|error| invalid_field(line, column, error.to_string()))
}

fn parse_positive_u8(value: &str, line: usize, column: &str) -> Result<u8, SdCardError> {
    let parsed = parse_bounded_u8(value, line, column, u8::MAX)?;
    if parsed == 0 {
        return Err(invalid_field(
            line,
            column,
            "value must be greater than zero",
        ));
    }
    Ok(parsed)
}

fn parse_bounded_u8(
    value: &str,
    line: usize,
    column: &str,
    maximum: u8,
) -> Result<u8, SdCardError> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(invalid_field(
            line,
            column,
            format!("expected decimal digits, got {value:?}"),
        ));
    }
    let parsed: u8 = value.parse().map_err(|_| {
        invalid_field(
            line,
            column,
            format!("decimal value {value:?} does not fit in one byte"),
        )
    })?;
    if parsed > maximum {
        return Err(invalid_field(
            line,
            column,
            format!("value {parsed} exceeds maximum {maximum}"),
        ));
    }
    Ok(parsed)
}

fn parse_callsign(
    value: &str,
    line: usize,
    column: &str,
    empty_allowed: bool,
) -> Result<(TsvField, DstarCallsign), SdCardError> {
    let source = parse_text(value, line, column)?;
    let callsign = DstarCallsign::new(source.as_str())
        .map_err(|error| invalid_field(line, column, error.to_string()))?;
    if !empty_allowed && callsign.is_empty() {
        return Err(invalid_field(line, column, "callsign must not be empty"));
    }
    Ok((source, callsign))
}

fn parse_frequency(
    value: &str,
    line: usize,
    column: &str,
    zero_allowed: bool,
) -> Result<(TsvField, Frequency), SdCardError> {
    let source = parse_text(value, line, column)?;
    let hertz = parse_fixed_decimal(source.as_str(), 6)
        .and_then(|scaled| u32::try_from(scaled).ok())
        .ok_or_else(|| {
            invalid_field(
                line,
                column,
                format!("expected MHz decimal within the u32 Hz range, got {value:?}"),
            )
        })?;
    if !zero_allowed && hertz == 0 {
        return Err(invalid_field(
            line,
            column,
            "frequency must be greater than zero",
        ));
    }
    Ok((source, Frequency::new(hertz)))
}

fn parse_fixed_decimal(value: &str, fractional_digits: u32) -> Option<u64> {
    let mut parts = value.split('.');
    let whole = parts.next()?;
    let fraction = parts.next();
    if parts.next().is_some()
        || whole.is_empty()
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }

    let fraction = fraction.unwrap_or_default();
    if fraction.len() > usize::try_from(fractional_digits).ok()?
        || (!fraction.is_empty() && !fraction.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return None;
    }
    if value.ends_with('.') {
        return None;
    }

    let scale = 10_u64.checked_pow(fractional_digits)?;
    let whole_scaled = whole.parse::<u64>().ok()?.checked_mul(scale)?;
    let fraction_value = if fraction.is_empty() {
        0
    } else {
        let missing_digits = fractional_digits.checked_sub(u32::try_from(fraction.len()).ok()?)?;
        fraction
            .parse::<u64>()
            .ok()?
            .checked_mul(10_u64.checked_pow(missing_digits)?)?
    };
    whole_scaled.checked_add(fraction_value)
}

fn parse_flag(value: &str, line: usize, column: &str) -> Result<RepeaterCatalogFlag, SdCardError> {
    match value {
        "Off" => Ok(RepeaterCatalogFlag::Off),
        "On" => Ok(RepeaterCatalogFlag::On),
        _ => Err(invalid_field(
            line,
            column,
            format!("expected `Off` or `On`, got {value:?}"),
        )),
    }
}

fn parse_shift(value: &str, line: usize) -> Result<RepeaterShift, SdCardError> {
    match value {
        "Off" => Ok(RepeaterShift::Off),
        "+" => Ok(RepeaterShift::Positive),
        "-" => Ok(RepeaterShift::Negative),
        _ => Err(invalid_field(
            line,
            "Shift",
            format!("expected `Off`, `+`, or `-`, got {value:?}"),
        )),
    }
}

fn parse_mode(value: &str, line: usize) -> Result<RepeaterCatalogMode, SdCardError> {
    match value {
        "Digital" => Ok(RepeaterCatalogMode::Digital),
        _ => Err(invalid_field(
            line,
            "Mode",
            format!("expected `Digital`, got {value:?}"),
        )),
    }
}

fn parse_position(value: &str, line: usize) -> Result<RepeaterPositionAccuracy, SdCardError> {
    match value {
        "Invalid" => Ok(RepeaterPositionAccuracy::Invalid),
        "Approx." => Ok(RepeaterPositionAccuracy::Approximate),
        "Exact" => Ok(RepeaterPositionAccuracy::Exact),
        _ => Err(invalid_field(
            line,
            "Position",
            format!("expected `Invalid`, `Approx.`, or `Exact`, got {value:?}"),
        )),
    }
}

fn parse_coordinate_minutes(
    value: &str,
    line: usize,
    column: &str,
) -> Result<RepeaterCoordinateMinutes, SdCardError> {
    let source = parse_text(value, line, column)?;
    let hundredths = parse_fixed_decimal(source.as_str(), 2)
        .and_then(|scaled| u16::try_from(scaled).ok())
        .filter(|&scaled| scaled < 6_000)
        .ok_or_else(|| {
            invalid_field(
                line,
                column,
                format!("expected decimal minutes in 0..<60, got {value:?}"),
            )
        })?;
    Ok(RepeaterCoordinateMinutes { hundredths, source })
}

fn validate_coordinate_endpoint(
    degrees: u8,
    minutes: &RepeaterCoordinateMinutes,
    endpoint: u8,
    line: usize,
    column: &str,
) -> Result<(), SdCardError> {
    if degrees == endpoint && minutes.hundredths() != 0 {
        return Err(invalid_field(
            line,
            column,
            format!("{endpoint} degrees requires zero minutes"),
        ));
    }
    Ok(())
}

fn parse_latitude_hemisphere(value: &str, line: usize) -> Result<LatitudeHemisphere, SdCardError> {
    match value {
        "N" => Ok(LatitudeHemisphere::North),
        "S" => Ok(LatitudeHemisphere::South),
        _ => Err(invalid_field(
            line,
            "N/S",
            format!("expected `N` or `S`, got {value:?}"),
        )),
    }
}

fn parse_longitude_hemisphere(
    value: &str,
    line: usize,
) -> Result<LongitudeHemisphere, SdCardError> {
    match value {
        "E" => Ok(LongitudeHemisphere::East),
        "W" => Ok(LongitudeHemisphere::West),
        _ => Err(invalid_field(
            line,
            "E/W",
            format!("expected `E` or `W`, got {value:?}"),
        )),
    }
}

fn parse_utc_offset(value: &str, line: usize) -> Result<RepeaterUtcOffset, SdCardError> {
    let source = parse_text(value, line, "Time Zone")?;
    let (sign, body) = if let Some(body) = value.strip_prefix('+') {
        (1_i16, body)
    } else if let Some(body) = value.strip_prefix('-') {
        (-1_i16, body)
    } else {
        return Err(invalid_field(
            line,
            "Time Zone",
            format!("expected signed UTC offset, got {value:?}"),
        ));
    };
    let Some((hours, minutes)) = body.split_once(':') else {
        return Err(invalid_field(
            line,
            "Time Zone",
            format!("expected `+HH:MM` or `-HH:MM`, got {value:?}"),
        ));
    };
    if hours.len() != 2
        || minutes.len() != 2
        || !hours.bytes().all(|byte| byte.is_ascii_digit())
        || !minutes.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(invalid_field(
            line,
            "Time Zone",
            format!("expected `+HH:MM` or `-HH:MM`, got {value:?}"),
        ));
    }
    let hours: i16 = hours
        .parse()
        .map_err(|_| invalid_field(line, "Time Zone", "invalid UTC-offset hours"))?;
    let minutes: i16 = minutes
        .parse()
        .map_err(|_| invalid_field(line, "Time Zone", "invalid UTC-offset minutes"))?;
    if hours > 14 || minutes >= 60 || (hours == 14 && minutes != 0) {
        return Err(invalid_field(
            line,
            "Time Zone",
            format!("UTC offset {value:?} is outside -14:00..=+14:00"),
        ));
    }
    Ok(RepeaterUtcOffset {
        minutes: sign * (hours * 60 + minutes),
        source,
    })
}

fn encode_entries(entries: &[RepeaterCatalogEntry]) -> Vec<u8> {
    let mut text = String::new();
    text.push_str(REPEATER_CATALOG_HEADER);
    text.push_str("\r\n");
    for entry in entries {
        append_entry(&mut text, entry);
    }
    encode_utf16le_bom(&text)
}

fn append_entry(output: &mut String, entry: &RepeaterCatalogEntry) {
    let world_region_number = entry.world_region_number.to_string();
    let country_number = entry.country_number.to_string();
    let group_number = entry.group_number.to_string();
    let latitude_degrees = entry.latitude_degrees.to_string();
    let longitude_degrees = entry.longitude_degrees.to_string();
    let fields = [
        world_region_number.as_str(),
        entry.world_region.as_str(),
        country_number.as_str(),
        entry.country.as_str(),
        group_number.as_str(),
        entry.group.as_str(),
        entry.callsign_source.as_str(),
        entry.gateway_source.as_str(),
        entry.lockout.as_str(),
        entry.name.as_str(),
        entry.sub_name.as_str(),
        entry.frequency_source.as_str(),
        entry.shift.as_str(),
        entry.offset_source.as_str(),
        entry.mode.as_str(),
        entry.uplink_tone.as_str(),
        entry.downlink_tone.as_str(),
        entry.position_accuracy.as_str(),
        latitude_degrees.as_str(),
        entry.latitude_minutes.as_str(),
        entry.latitude_hemisphere.as_str(),
        longitude_degrees.as_str(),
        entry.longitude_minutes.as_str(),
        entry.longitude_hemisphere.as_str(),
        entry.time_zone.as_str(),
        entry.model_flags.americas.as_str(),
        entry.model_flags.europe.as_str(),
        entry.model_flags.region_neutral.as_str(),
        entry.aux_1.as_str(),
        entry.aux_2.as_str(),
        entry.aux_3.as_str(),
    ];
    output.push_str(&fields.join("\t"));
    output.push_str("\r\n");
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    const SIMPLEX_ROW: &str = "1\tAsia\t1\tJapan\t11\tSimplex\tDIRECT\tDIRECT\tOff\t\
145.300MHz\tDV\t145.3\tOff\t0\tDigital\tOff\tOff\tInvalid\t0\t0\tN\t0\t0\tE\t\
+09:00\tOff\tOff\tOn\tJPN\tSMP\treserved";
    const REPEATER_ROW: &str = "1\tAsia\t1\tJapan\t1\tKanto\tJP1YLA A\tJP1YLA G\tOff\t\
Akihabara430\tTokyo\t434.32\t+\t5\tDigital\tOff\tOff\tApprox.\t35\t41.97\tN\t\
139\t46.24\tE\t+09:00\tOn\tOff\tOn\tJPN\tJA1\t";

    fn catalog_text(rows: &[&str]) -> String {
        let mut text = String::from(REPEATER_CATALOG_HEADER);
        text.push_str("\r\n");
        for row in rows {
            text.push_str(row);
            text.push_str("\r\n");
        }
        text
    }

    fn utf16_catalog(rows: &[&str]) -> Vec<u8> {
        encode_utf16le_bom(&catalog_text(rows))
    }

    #[test]
    fn parses_representative_official_rows() -> TestResult {
        let catalog = parse_repeater_catalog(&utf16_catalog(&[SIMPLEX_ROW, REPEATER_ROW]))?;
        assert_eq!(catalog.len(), 2);

        let simplex = catalog.entries().first().ok_or("missing simplex row")?;
        assert_eq!(simplex.callsign().as_str(), "DIRECT");
        assert_eq!(simplex.shift(), RepeaterShift::Off);
        assert_eq!(simplex.frequency().as_hz(), 145_300_000);
        assert_eq!(
            simplex.position_accuracy(),
            RepeaterPositionAccuracy::Invalid
        );
        assert_eq!(simplex.aux_3(), "reserved");

        let repeater = catalog.entries().get(1).ok_or("missing repeater row")?;
        assert_eq!(repeater.callsign().as_str(), "JP1YLA A");
        assert_eq!(repeater.gateway().as_str(), "JP1YLA G");
        assert_eq!(repeater.shift(), RepeaterShift::Positive);
        assert_eq!(repeater.offset().as_hz(), 5_000_000);
        assert_eq!(repeater.latitude_minutes().hundredths(), 4_197);
        assert_eq!(repeater.time_zone().minutes(), 540);
        assert!(repeater.model_flags().supports(ConfigFileModel::ThD75A));
        Ok(())
    }

    #[test]
    fn source_catalog_is_not_subject_to_radio_capacity() -> TestResult {
        let rows = vec![REPEATER_ROW; 2_466];
        let catalog = parse_repeater_catalog(&utf16_catalog(&rows))?;
        assert_eq!(catalog.len(), 2_466);
        assert!(matches!(
            catalog.select(RepeaterCatalogSelection::new(ConfigFileModel::ThD75A)),
            Err(SdCardError::EntryCount {
                maximum: MAX_REPEATER_ENTRIES,
                actual: 2_466,
                ..
            })
        ));
        Ok(())
    }

    #[test]
    fn model_and_region_selection_materializes_only_matching_rows() -> TestResult {
        let catalog = parse_repeater_catalog(&utf16_catalog(&[SIMPLEX_ROW, REPEATER_ROW]))?;
        let americas = catalog.select(RepeaterCatalogSelection::new(ConfigFileModel::ThD75A))?;
        assert_eq!(americas.len(), 1);
        assert_eq!(
            americas
                .entries()
                .first()
                .ok_or("missing selected row")?
                .name(),
            "Akihabara430"
        );

        let japan_simplex = catalog.select(RepeaterCatalogSelection::with_region(
            ConfigFileModel::RegionNeutral,
            RepeaterCatalogRegion::Group {
                world_region: 1,
                country: 1,
                group: 11,
            },
        ))?;
        assert_eq!(japan_simplex.len(), 1);
        assert_eq!(
            japan_simplex
                .entries()
                .first()
                .ok_or("missing region row")?
                .shift(),
            RepeaterShift::Off
        );
        Ok(())
    }

    #[test]
    fn rejects_noncanonical_header_and_old_eight_column_dialect() {
        let wrong = encode_utf16le_bom(
            "Group Name\tName\tSub Name\tRepeater Call Sign\tGateway Call Sign\tFrequency\tDup\tOffset\r\n",
        );
        assert!(matches!(
            parse_repeater_catalog(&wrong),
            Err(SdCardError::HeaderMismatch {
                file_type: FILE_TYPE,
                ..
            })
        ));
    }

    #[test]
    fn rejects_wrong_row_width_and_noncanonical_shift() {
        let short = utf16_catalog(&["1\tAsia"]);
        assert!(matches!(
            parse_repeater_catalog(&short),
            Err(SdCardError::ColumnCount {
                line: 2,
                expected: REPEATER_CATALOG_COLUMNS,
                actual: 2,
            })
        ));

        let invalid_shift = REPEATER_ROW.replacen("\t+\t5\t", "\tDUP+\t5\t", 1);
        let invalid_shift_rows = [invalid_shift.as_str()];
        assert!(matches!(
            parse_repeater_catalog(&utf16_catalog(&invalid_shift_rows)),
            Err(SdCardError::InvalidField { column, .. }) if column == "Shift"
        ));
    }

    #[test]
    fn rejects_relaxed_frequency_grammar() {
        let scientific = REPEATER_ROW.replacen("\t434.32\t", "\t4.3432e2\t", 1);
        let scientific_rows = [scientific.as_str()];
        assert!(matches!(
            parse_repeater_catalog(&utf16_catalog(&scientific_rows)),
            Err(SdCardError::InvalidField { column, .. }) if column == "Frequency"
        ));
    }

    #[test]
    fn utf16_writer_preserves_auxiliary_fields() -> TestResult {
        let catalog = parse_repeater_catalog(&utf16_catalog(&[SIMPLEX_ROW]))?;
        let encoded = write_repeater_catalog(&catalog);
        let reparsed = parse_repeater_catalog(&encoded)?;
        assert_eq!(reparsed, catalog);
        assert_eq!(
            reparsed.entries().first().ok_or("missing row")?.aux_3(),
            "reserved"
        );
        Ok(())
    }

    #[test]
    fn parses_strict_shift_jis_catalog() -> TestResult {
        let japanese_row = REPEATER_ROW.replacen("\tAsia\t", "\tアジア\t", 1);
        let japanese_rows = [japanese_row.as_str()];
        let text = catalog_text(&japanese_rows);
        let (encoded, _, had_errors) = SHIFT_JIS.encode(&text);
        assert!(
            !had_errors,
            "representative Japanese row must encode as Shift-JIS"
        );
        let catalog = parse_repeater_catalog(encoded.as_ref())?;
        assert_eq!(
            catalog
                .entries()
                .first()
                .ok_or("missing row")?
                .world_region(),
            "アジア"
        );
        Ok(())
    }

    #[test]
    fn rejects_malformed_shift_jis_without_replacement() {
        assert!(matches!(
            parse_repeater_catalog(&[0x81]),
            Err(SdCardError::UnsupportedTextEncoding {
                file_type: FILE_TYPE,
                ..
            })
        ));
    }
}
