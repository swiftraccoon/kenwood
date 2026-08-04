//! Strict, lossless support for TH-D75 QSO-log `.csv` files.
//!
//! Despite the extension, a QSO log is an unquoted, tab-separated UTF-8
//! table. User Manual page 19-4 specifies an exact 24-column header and the
//! closed spellings used by direction, date, frequency, mode, RF power, and
//! Fast Data. Fields whose vocabulary is not established there remain
//! lossless [`TsvField`] values rather than speculative enums.
//!
//! # Location
//!
//! `/KENWOOD/TH-D75/QSO_LOG/YYYYMMDD_HHMMSS.csv`

use std::fmt;
use std::str::FromStr;

use super::{SdCardError, TsvField, TsvFieldError, decode_utf8};
use crate::types::Frequency;

pub use crate::types::QsoDirection;

/// Number of fields in one TH-D75 QSO-log row.
pub const QSO_LOG_COLUMNS: usize = 24;

/// Exact firmware header for a TH-D75 QSO log.
pub const QSO_LOG_HEADER: &str = "TX/RX\tDate\tFrequency\tMode\tMy Latitude\t\
My Longitude\tMy Altitude\tRF Power\tS Meter\tCaller\tMemo\tCalled\tRx RPT1\t\
Rx RPT2\tMessage\tRepeater Control\tBK\tEMR\tFast Data\tLatitude\tLongitude\t\
Altitude\tCourse\tSpeed";

const FILE_TYPE: &str = "QSO log";

/// Why a value cannot occupy one field in a valid TH-D75 QSO entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QsoFieldError {
    column: &'static str,
    detail: String,
}

impl QsoFieldError {
    /// Return the exact QSO-log column name associated with the error.
    #[must_use]
    pub const fn column(&self) -> &'static str {
        self.column
    }

    /// Return the validation failure without the column-name prefix.
    #[must_use]
    pub const fn detail(&self) -> &str {
        self.detail.as_str()
    }

    fn unexpected(column: &'static str, value: &str, expected: &'static str) -> Self {
        Self {
            column,
            detail: format!("expected {expected}, got {value:?}"),
        }
    }

    fn unsafe_text(column: &'static str, error: &TsvFieldError) -> Self {
        Self {
            column,
            detail: error.to_string(),
        }
    }
}

impl fmt::Display for QsoFieldError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "invalid QSO field {}: {}",
            self.column, self.detail
        )
    }
}

impl std::error::Error for QsoFieldError {}

impl QsoDirection {
    /// Return the exact firmware spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Tx => "TX",
            Self::Rx => "RX",
        }
    }
}

impl fmt::Display for QsoDirection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for QsoDirection {
    type Err = QsoFieldError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "TX" => Ok(Self::Tx),
            "RX" => Ok(Self::Rx),
            _ => Err(QsoFieldError::unexpected("TX/RX", value, "`TX` or `RX`")),
        }
    }
}

/// QSO operating mode using the exact log spellings from the manual.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum QsoMode {
    /// D-STAR digital voice (`DV`).
    Dv,
    /// Conventional frequency modulation (`FM`).
    Fm,
    /// Narrow frequency modulation (`FM-N`).
    FmN,
}

impl QsoMode {
    /// Return the exact firmware spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Dv => "DV",
            Self::Fm => "FM",
            Self::FmN => "FM-N",
        }
    }
}

impl fmt::Display for QsoMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for QsoMode {
    type Err = QsoFieldError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "DV" => Ok(Self::Dv),
            "FM" => Ok(Self::Fm),
            "FM-N" => Ok(Self::FmN),
            _ => Err(QsoFieldError::unexpected(
                "Mode",
                value,
                "`DV`, `FM`, or `FM-N`",
            )),
        }
    }
}

/// RF-power setting serialized in a QSO log.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum QsoRfPower {
    /// Super-low power (`S-LOW`).
    SLow,
    /// Low-power level 1 (`LOW1`).
    Low1,
    /// Low-power level 2 (`LOW2`).
    Low2,
    /// Medium power (`MID`).
    Mid,
    /// High power (`HIGH`).
    High,
}

impl QsoRfPower {
    /// Return the exact firmware spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SLow => "S-LOW",
            Self::Low1 => "LOW1",
            Self::Low2 => "LOW2",
            Self::Mid => "MID",
            Self::High => "HIGH",
        }
    }
}

impl fmt::Display for QsoRfPower {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for QsoRfPower {
    type Err = QsoFieldError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "S-LOW" => Ok(Self::SLow),
            "LOW1" => Ok(Self::Low1),
            "LOW2" => Ok(Self::Low2),
            "MID" => Ok(Self::Mid),
            "HIGH" => Ok(Self::High),
            _ => Err(QsoFieldError::unexpected(
                "RF Power",
                value,
                "`S-LOW`, `LOW1`, `LOW2`, `MID`, or `HIGH`",
            )),
        }
    }
}

/// Fast Data flag serialized in a D-STAR QSO row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum QsoFastData {
    /// Fast Data was disabled (`0`).
    Disabled,
    /// Fast Data was enabled (`1`).
    Enabled,
}

impl QsoFastData {
    /// Return the exact firmware spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "0",
            Self::Enabled => "1",
        }
    }

    /// Return whether Fast Data was enabled.
    #[must_use]
    pub const fn is_enabled(self) -> bool {
        matches!(self, Self::Enabled)
    }
}

impl fmt::Display for QsoFastData {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for QsoFastData {
    type Err = QsoFieldError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "0" => Ok(Self::Disabled),
            "1" => Ok(Self::Enabled),
            _ => Err(QsoFieldError::unexpected("Fast Data", value, "`0` or `1`")),
        }
    }
}

/// Calendar date and minute from the QSO `Date` column.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct QsoDateTime {
    source: TsvField,
    year: u16,
    month: u8,
    day: u8,
    hour: u8,
    minute: u8,
}

impl QsoDateTime {
    /// Parse an exact `YYYY/MM/DD HH:MM` QSO timestamp.
    ///
    /// # Errors
    ///
    /// Returns [`QsoFieldError`] for a malformed or impossible calendar value.
    pub fn new(value: &str) -> Result<Self, QsoFieldError> {
        let &[
            year_1,
            year_2,
            year_3,
            year_4,
            b'/',
            month_1,
            month_2,
            b'/',
            day_1,
            day_2,
            b' ',
            hour_1,
            hour_2,
            b':',
            minute_1,
            minute_2,
        ] = value.as_bytes()
        else {
            return Err(QsoFieldError::unexpected(
                "Date",
                value,
                "an exact `YYYY/MM/DD HH:MM` timestamp",
            ));
        };

        let digits = [
            year_1, year_2, year_3, year_4, month_1, month_2, day_1, day_2, hour_1, hour_2,
            minute_1, minute_2,
        ];
        if !digits.iter().all(u8::is_ascii_digit) {
            return Err(QsoFieldError::unexpected(
                "Date",
                value,
                "an exact `YYYY/MM/DD HH:MM` timestamp",
            ));
        }

        let year = four_digits(year_1, year_2, year_3, year_4);
        let month = two_digits(month_1, month_2);
        let day = two_digits(day_1, day_2);
        let hour = two_digits(hour_1, hour_2);
        let minute = two_digits(minute_1, minute_2);
        let maximum_day = days_in_month(year, month).ok_or_else(|| {
            QsoFieldError::unexpected("Date", value, "a valid Gregorian calendar date")
        })?;
        if year == 0 || day == 0 || day > maximum_day || hour > 23 || minute > 59 {
            return Err(QsoFieldError::unexpected(
                "Date",
                value,
                "a valid Gregorian `YYYY/MM/DD HH:MM` timestamp",
            ));
        }

        let source =
            TsvField::new(value).map_err(|error| QsoFieldError::unsafe_text("Date", &error))?;
        Ok(Self {
            source,
            year,
            month,
            day,
            hour,
            minute,
        })
    }

    /// Return the exact source timestamp.
    #[must_use]
    pub const fn as_str(&self) -> &str {
        self.source.as_str()
    }

    /// Return the four-digit year.
    #[must_use]
    pub const fn year(&self) -> u16 {
        self.year
    }

    /// Return the month in `1..=12`.
    #[must_use]
    pub const fn month(&self) -> u8 {
        self.month
    }

    /// Return the day of the month.
    #[must_use]
    pub const fn day(&self) -> u8 {
        self.day
    }

    /// Return the hour in `0..=23`.
    #[must_use]
    pub const fn hour(&self) -> u8 {
        self.hour
    }

    /// Return the minute in `0..=59`.
    #[must_use]
    pub const fn minute(&self) -> u8 {
        self.minute
    }
}

impl fmt::Display for QsoDateTime {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for QsoDateTime {
    type Err = QsoFieldError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

/// Frequency from the QSO `Frequency` column.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct QsoFrequency {
    source: TsvField,
    frequency: Frequency,
}

impl QsoFrequency {
    /// Parse an exact `xxx.xxx.xxx` frequency field.
    ///
    /// # Errors
    ///
    /// Returns [`QsoFieldError`] unless all three groups contain exactly three
    /// decimal digits.
    pub fn new(value: &str) -> Result<Self, QsoFieldError> {
        let &[
            mhz_1,
            mhz_2,
            mhz_3,
            b'.',
            khz_1,
            khz_2,
            khz_3,
            b'.',
            hz_1,
            hz_2,
            hz_3,
        ] = value.as_bytes()
        else {
            return Err(QsoFieldError::unexpected(
                "Frequency",
                value,
                "an exact `xxx.xxx.xxx` decimal frequency",
            ));
        };
        let digits = [mhz_1, mhz_2, mhz_3, khz_1, khz_2, khz_3, hz_1, hz_2, hz_3];
        if !digits.iter().all(u8::is_ascii_digit) {
            return Err(QsoFieldError::unexpected(
                "Frequency",
                value,
                "an exact `xxx.xxx.xxx` decimal frequency",
            ));
        }

        let megahertz = u32::from(three_digits(mhz_1, mhz_2, mhz_3));
        let kilohertz = u32::from(three_digits(khz_1, khz_2, khz_3));
        let hertz = u32::from(three_digits(hz_1, hz_2, hz_3));
        let frequency = Frequency::new(megahertz * 1_000_000 + kilohertz * 1_000 + hertz);
        let source = TsvField::new(value)
            .map_err(|error| QsoFieldError::unsafe_text("Frequency", &error))?;
        Ok(Self { source, frequency })
    }

    /// Construct the exact QSO-log representation of a frequency.
    ///
    /// # Errors
    ///
    /// Returns [`QsoFieldError`] above 999,999,999 Hz because the manual's
    /// fixed `xxx.xxx.xxx` field cannot represent a fourth MHz digit.
    pub fn from_frequency(frequency: Frequency) -> Result<Self, QsoFieldError> {
        let value = frequency.as_hz();
        if value > 999_999_999 {
            return Err(QsoFieldError::unexpected(
                "Frequency",
                &value.to_string(),
                "a value no greater than 999,999,999 Hz",
            ));
        }
        let megahertz = value / 1_000_000;
        let kilohertz = value % 1_000_000 / 1_000;
        let hertz = value % 1_000;
        Self::new(&format!("{megahertz:03}.{kilohertz:03}.{hertz:03}"))
    }

    /// Return the exact source field.
    #[must_use]
    pub const fn as_str(&self) -> &str {
        self.source.as_str()
    }

    /// Return the parsed frequency in hertz.
    #[must_use]
    pub const fn as_frequency(&self) -> Frequency {
        self.frequency
    }

    /// Return the parsed frequency value in hertz.
    #[must_use]
    pub const fn as_hz(&self) -> u32 {
        self.frequency.as_hz()
    }
}

impl fmt::Display for QsoFrequency {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for QsoFrequency {
    type Err = QsoFieldError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

/// One valid, lossless 24-column QSO-log record.
///
/// Fields are private, so an entry cannot be mutated into an invalid TSV row.
/// Use [`QsoEntry::builder`] to construct a record and the getters to inspect
/// it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QsoEntry {
    direction: QsoDirection,
    date: QsoDateTime,
    frequency: QsoFrequency,
    mode: QsoMode,
    my_latitude: TsvField,
    my_longitude: TsvField,
    my_altitude: TsvField,
    rf_power: QsoRfPower,
    s_meter: TsvField,
    caller: TsvField,
    memo: TsvField,
    called: TsvField,
    rx_rpt1: TsvField,
    rx_rpt2: TsvField,
    message: TsvField,
    repeater_control: TsvField,
    bk: TsvField,
    emr: TsvField,
    fast_data: QsoFastData,
    latitude: TsvField,
    longitude: TsvField,
    altitude: TsvField,
    course: TsvField,
    speed: TsvField,
}

impl QsoEntry {
    /// Start a validated entry builder with all closed fields supplied.
    ///
    /// Text fields begin empty because the firmware legitimately leaves many
    /// position and D-STAR metadata columns blank. Set every populated text
    /// column explicitly before calling [`QsoEntryBuilder::build`].
    #[must_use]
    pub const fn builder(
        direction: QsoDirection,
        date: QsoDateTime,
        frequency: QsoFrequency,
        mode: QsoMode,
        rf_power: QsoRfPower,
        fast_data: QsoFastData,
    ) -> QsoEntryBuilder {
        QsoEntryBuilder::new(direction, date, frequency, mode, rf_power, fast_data)
    }

    /// Return whether this was a transmitted or received QSO.
    #[must_use]
    pub const fn direction(&self) -> QsoDirection {
        self.direction
    }

    /// Return the validated date and time.
    #[must_use]
    pub const fn date(&self) -> &QsoDateTime {
        &self.date
    }

    /// Return the validated frequency field.
    #[must_use]
    pub const fn frequency(&self) -> &QsoFrequency {
        &self.frequency
    }

    /// Return the operating mode.
    #[must_use]
    pub const fn mode(&self) -> QsoMode {
        self.mode
    }

    /// Return `My Latitude` exactly as logged.
    #[must_use]
    pub const fn my_latitude(&self) -> &str {
        self.my_latitude.as_str()
    }

    /// Return `My Longitude` exactly as logged.
    #[must_use]
    pub const fn my_longitude(&self) -> &str {
        self.my_longitude.as_str()
    }

    /// Return `My Altitude` exactly as logged.
    #[must_use]
    pub const fn my_altitude(&self) -> &str {
        self.my_altitude.as_str()
    }

    /// Return the RF-power setting.
    #[must_use]
    pub const fn rf_power(&self) -> QsoRfPower {
        self.rf_power
    }

    /// Return `S Meter` exactly as logged.
    #[must_use]
    pub const fn s_meter(&self) -> &str {
        self.s_meter.as_str()
    }

    /// Return `Caller` exactly as logged.
    #[must_use]
    pub const fn caller(&self) -> &str {
        self.caller.as_str()
    }

    /// Return `Memo` exactly as logged.
    #[must_use]
    pub const fn memo(&self) -> &str {
        self.memo.as_str()
    }

    /// Return `Called` exactly as logged.
    #[must_use]
    pub const fn called(&self) -> &str {
        self.called.as_str()
    }

    /// Return `Rx RPT1` exactly as logged.
    #[must_use]
    pub const fn rx_rpt1(&self) -> &str {
        self.rx_rpt1.as_str()
    }

    /// Return `Rx RPT2` exactly as logged.
    #[must_use]
    pub const fn rx_rpt2(&self) -> &str {
        self.rx_rpt2.as_str()
    }

    /// Return `Message` exactly as logged.
    #[must_use]
    pub const fn message(&self) -> &str {
        self.message.as_str()
    }

    /// Return `Repeater Control` exactly as logged.
    #[must_use]
    pub const fn repeater_control(&self) -> &str {
        self.repeater_control.as_str()
    }

    /// Return `BK` exactly as logged.
    #[must_use]
    pub const fn bk(&self) -> &str {
        self.bk.as_str()
    }

    /// Return `EMR` exactly as logged.
    #[must_use]
    pub const fn emr(&self) -> &str {
        self.emr.as_str()
    }

    /// Return the Fast Data flag.
    #[must_use]
    pub const fn fast_data(&self) -> QsoFastData {
        self.fast_data
    }

    /// Return remote `Latitude` exactly as logged.
    #[must_use]
    pub const fn latitude(&self) -> &str {
        self.latitude.as_str()
    }

    /// Return remote `Longitude` exactly as logged.
    #[must_use]
    pub const fn longitude(&self) -> &str {
        self.longitude.as_str()
    }

    /// Return remote `Altitude` exactly as logged.
    #[must_use]
    pub const fn altitude(&self) -> &str {
        self.altitude.as_str()
    }

    /// Return remote `Course` exactly as logged.
    #[must_use]
    pub const fn course(&self) -> &str {
        self.course.as_str()
    }

    /// Return remote `Speed` exactly as logged.
    #[must_use]
    pub const fn speed(&self) -> &str {
        self.speed.as_str()
    }
}

/// Builder for a valid [`QsoEntry`].
///
/// The six fields with closed firmware vocabularies are required by
/// [`QsoEntry::builder`]. The remaining fields are lossless text setters and
/// are validated together by [`build`](Self::build).
#[derive(Debug, Clone)]
pub struct QsoEntryBuilder {
    direction: QsoDirection,
    date: QsoDateTime,
    frequency: QsoFrequency,
    mode: QsoMode,
    my_latitude: String,
    my_longitude: String,
    my_altitude: String,
    rf_power: QsoRfPower,
    s_meter: String,
    caller: String,
    memo: String,
    called: String,
    rx_rpt1: String,
    rx_rpt2: String,
    message: String,
    repeater_control: String,
    bk: String,
    emr: String,
    fast_data: QsoFastData,
    latitude: String,
    longitude: String,
    altitude: String,
    course: String,
    speed: String,
}

macro_rules! text_setter {
    ($name:ident, $field:ident, $documentation:literal) => {
        #[doc = $documentation]
        #[must_use]
        pub fn $name(mut self, value: impl Into<String>) -> Self {
            self.$field = value.into();
            self
        }
    };
}

impl QsoEntryBuilder {
    const fn new(
        direction: QsoDirection,
        date: QsoDateTime,
        frequency: QsoFrequency,
        mode: QsoMode,
        rf_power: QsoRfPower,
        fast_data: QsoFastData,
    ) -> Self {
        Self {
            direction,
            date,
            frequency,
            mode,
            my_latitude: String::new(),
            my_longitude: String::new(),
            my_altitude: String::new(),
            rf_power,
            s_meter: String::new(),
            caller: String::new(),
            memo: String::new(),
            called: String::new(),
            rx_rpt1: String::new(),
            rx_rpt2: String::new(),
            message: String::new(),
            repeater_control: String::new(),
            bk: String::new(),
            emr: String::new(),
            fast_data,
            latitude: String::new(),
            longitude: String::new(),
            altitude: String::new(),
            course: String::new(),
            speed: String::new(),
        }
    }

    text_setter!(
        my_latitude,
        my_latitude,
        "Set `My Latitude` without interpreting its undocumented spelling."
    );
    text_setter!(
        my_longitude,
        my_longitude,
        "Set `My Longitude` without interpreting its undocumented spelling."
    );
    text_setter!(
        my_altitude,
        my_altitude,
        "Set `My Altitude` without interpreting its undocumented spelling."
    );
    text_setter!(
        s_meter,
        s_meter,
        "Set `S Meter` exactly as emitted by the radio."
    );
    text_setter!(
        caller,
        caller,
        "Set `Caller` exactly as emitted by the radio."
    );
    text_setter!(memo, memo, "Set `Memo` exactly as emitted by the radio.");
    text_setter!(
        called,
        called,
        "Set `Called` exactly as emitted by the radio."
    );
    text_setter!(
        rx_rpt1,
        rx_rpt1,
        "Set `Rx RPT1` exactly as emitted by the radio."
    );
    text_setter!(
        rx_rpt2,
        rx_rpt2,
        "Set `Rx RPT2` exactly as emitted by the radio."
    );
    text_setter!(
        message,
        message,
        "Set `Message` exactly as emitted by the radio."
    );
    text_setter!(
        repeater_control,
        repeater_control,
        "Set `Repeater Control` exactly as emitted by the radio."
    );
    text_setter!(bk, bk, "Set `BK` exactly as emitted by the radio.");
    text_setter!(emr, emr, "Set `EMR` exactly as emitted by the radio.");
    text_setter!(
        latitude,
        latitude,
        "Set remote `Latitude` without interpreting its undocumented spelling."
    );
    text_setter!(
        longitude,
        longitude,
        "Set remote `Longitude` without interpreting its undocumented spelling."
    );
    text_setter!(
        altitude,
        altitude,
        "Set remote `Altitude` without interpreting its undocumented spelling."
    );
    text_setter!(
        course,
        course,
        "Set remote `Course` exactly as emitted by the radio."
    );
    text_setter!(
        speed,
        speed,
        "Set remote `Speed` exactly as emitted by the radio."
    );

    /// Validate every lossless text field and finish the entry.
    ///
    /// # Errors
    ///
    /// Returns [`QsoFieldError`] when a text field contains a tab, line
    /// terminator, or NUL that cannot occupy one unquoted TSV column.
    pub fn build(self) -> Result<QsoEntry, QsoFieldError> {
        Ok(QsoEntry {
            direction: self.direction,
            date: self.date,
            frequency: self.frequency,
            mode: self.mode,
            my_latitude: validated_text("My Latitude", &self.my_latitude)?,
            my_longitude: validated_text("My Longitude", &self.my_longitude)?,
            my_altitude: validated_text("My Altitude", &self.my_altitude)?,
            rf_power: self.rf_power,
            s_meter: validated_text("S Meter", &self.s_meter)?,
            caller: validated_text("Caller", &self.caller)?,
            memo: validated_text("Memo", &self.memo)?,
            called: validated_text("Called", &self.called)?,
            rx_rpt1: validated_text("Rx RPT1", &self.rx_rpt1)?,
            rx_rpt2: validated_text("Rx RPT2", &self.rx_rpt2)?,
            message: validated_text("Message", &self.message)?,
            repeater_control: validated_text("Repeater Control", &self.repeater_control)?,
            bk: validated_text("BK", &self.bk)?,
            emr: validated_text("EMR", &self.emr)?,
            fast_data: self.fast_data,
            latitude: validated_text("Latitude", &self.latitude)?,
            longitude: validated_text("Longitude", &self.longitude)?,
            altitude: validated_text("Altitude", &self.altitude)?,
            course: validated_text("Course", &self.course)?,
            speed: validated_text("Speed", &self.speed)?,
        })
    }
}

/// Parse a QSO log from raw `.csv` file bytes.
///
/// # Errors
///
/// Returns [`SdCardError`] if the input is not UTF-8, its header is not exact,
/// a row does not contain 24 columns, or any field violates its established
/// wire contract.
pub fn parse_qso_log(data: &[u8]) -> Result<Vec<QsoEntry>, SdCardError> {
    let text = decode_utf8(data, FILE_TYPE)?;
    let mut lines = text.lines();
    let actual_header = lines.next().ok_or_else(|| SdCardError::HeaderMismatch {
        file_type: FILE_TYPE,
        expected: QSO_LOG_HEADER.to_owned(),
        actual: String::new(),
    })?;
    if actual_header != QSO_LOG_HEADER {
        return Err(SdCardError::HeaderMismatch {
            file_type: FILE_TYPE,
            expected: QSO_LOG_HEADER.to_owned(),
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
        let raw = RawQsoRow::from_columns(&columns).ok_or(SdCardError::ColumnCount {
            line: line_number,
            expected: QSO_LOG_COLUMNS,
            actual,
        })?;
        entries.push(parse_entry(&raw, line_number)?);
    }
    Ok(entries)
}

/// Encode valid QSO entries as a tab-separated UTF-8 `.csv` file.
///
/// Serialization is infallible because [`QsoEntry`] fields are private and
/// every construction path validates the unquoted TSV invariants.
#[must_use]
pub fn write_qso_log(entries: &[QsoEntry]) -> Vec<u8> {
    let mut text = String::new();
    text.push_str(QSO_LOG_HEADER);
    text.push_str("\r\n");
    for entry in entries {
        text.push_str(&entry_fields(entry).join("\t"));
        text.push_str("\r\n");
    }
    text.into_bytes()
}

#[derive(Debug, Clone, Copy)]
struct RawQsoRow<'a> {
    direction: &'a str,
    date: &'a str,
    frequency: &'a str,
    mode: &'a str,
    my_latitude: &'a str,
    my_longitude: &'a str,
    my_altitude: &'a str,
    rf_power: &'a str,
    s_meter: &'a str,
    caller: &'a str,
    memo: &'a str,
    called: &'a str,
    rx_rpt1: &'a str,
    rx_rpt2: &'a str,
    message: &'a str,
    repeater_control: &'a str,
    bk: &'a str,
    emr: &'a str,
    fast_data: &'a str,
    latitude: &'a str,
    longitude: &'a str,
    altitude: &'a str,
    course: &'a str,
    speed: &'a str,
}

impl<'a> RawQsoRow<'a> {
    fn from_columns(columns: &[&'a str]) -> Option<Self> {
        let &[
            direction,
            date,
            frequency,
            mode,
            my_latitude,
            my_longitude,
            my_altitude,
            rf_power,
            s_meter,
            caller_text,
            memo,
            destination_text,
            rx_rpt1,
            rx_rpt2,
            message,
            repeater_control,
            bk,
            emr,
            fast_data,
            latitude,
            longitude,
            altitude,
            course,
            speed,
        ] = columns
        else {
            return None;
        };
        Some(Self {
            direction,
            date,
            frequency,
            mode,
            my_latitude,
            my_longitude,
            my_altitude,
            rf_power,
            s_meter,
            caller: caller_text,
            memo,
            called: destination_text,
            rx_rpt1,
            rx_rpt2,
            message,
            repeater_control,
            bk,
            emr,
            fast_data,
            latitude,
            longitude,
            altitude,
            course,
            speed,
        })
    }
}

fn parse_entry(raw: &RawQsoRow<'_>, line: usize) -> Result<QsoEntry, SdCardError> {
    let direction = raw
        .direction
        .parse()
        .map_err(|error| row_error(line, &error))?;
    let date = QsoDateTime::new(raw.date).map_err(|error| row_error(line, &error))?;
    let frequency = QsoFrequency::new(raw.frequency).map_err(|error| row_error(line, &error))?;
    let mode = raw.mode.parse().map_err(|error| row_error(line, &error))?;
    let rf_power = raw
        .rf_power
        .parse()
        .map_err(|error| row_error(line, &error))?;
    let fast_data = raw
        .fast_data
        .parse()
        .map_err(|error| row_error(line, &error))?;

    QsoEntry::builder(direction, date, frequency, mode, rf_power, fast_data)
        .my_latitude(raw.my_latitude)
        .my_longitude(raw.my_longitude)
        .my_altitude(raw.my_altitude)
        .s_meter(raw.s_meter)
        .caller(raw.caller)
        .memo(raw.memo)
        .called(raw.called)
        .rx_rpt1(raw.rx_rpt1)
        .rx_rpt2(raw.rx_rpt2)
        .message(raw.message)
        .repeater_control(raw.repeater_control)
        .bk(raw.bk)
        .emr(raw.emr)
        .latitude(raw.latitude)
        .longitude(raw.longitude)
        .altitude(raw.altitude)
        .course(raw.course)
        .speed(raw.speed)
        .build()
        .map_err(|error| row_error(line, &error))
}

fn row_error(line: usize, error: &QsoFieldError) -> SdCardError {
    SdCardError::InvalidField {
        line,
        column: error.column().to_owned(),
        detail: error.detail().to_owned(),
    }
}

fn validated_text(column: &'static str, value: &str) -> Result<TsvField, QsoFieldError> {
    TsvField::new(value).map_err(|error| QsoFieldError::unsafe_text(column, &error))
}

const fn two_digits(tens: u8, ones: u8) -> u8 {
    (tens - b'0') * 10 + (ones - b'0')
}

fn three_digits(hundreds: u8, tens: u8, ones: u8) -> u16 {
    u16::from(hundreds - b'0') * 100 + u16::from(tens - b'0') * 10 + u16::from(ones - b'0')
}

fn four_digits(thousands: u8, hundreds: u8, tens: u8, ones: u8) -> u16 {
    u16::from(thousands - b'0') * 1_000
        + u16::from(hundreds - b'0') * 100
        + u16::from(tens - b'0') * 10
        + u16::from(ones - b'0')
}

const fn days_in_month(year: u16, month: u8) -> Option<u8> {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => Some(31),
        4 | 6 | 9 | 11 => Some(30),
        2 if is_leap_year(year) => Some(29),
        2 => Some(28),
        _ => None,
    }
}

const fn is_leap_year(year: u16) -> bool {
    year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400))
}

const fn entry_fields(entry: &QsoEntry) -> [&str; QSO_LOG_COLUMNS] {
    [
        entry.direction.as_str(),
        entry.date.as_str(),
        entry.frequency.as_str(),
        entry.mode.as_str(),
        entry.my_latitude.as_str(),
        entry.my_longitude.as_str(),
        entry.my_altitude.as_str(),
        entry.rf_power.as_str(),
        entry.s_meter.as_str(),
        entry.caller.as_str(),
        entry.memo.as_str(),
        entry.called.as_str(),
        entry.rx_rpt1.as_str(),
        entry.rx_rpt2.as_str(),
        entry.message.as_str(),
        entry.repeater_control.as_str(),
        entry.bk.as_str(),
        entry.emr.as_str(),
        entry.fast_data.as_str(),
        entry.latitude.as_str(),
        entry.longitude.as_str(),
        entry.altitude.as_str(),
        entry.course.as_str(),
        entry.speed.as_str(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn sample_entry() -> Result<QsoEntry, QsoFieldError> {
        QsoEntry::builder(
            QsoDirection::Tx,
            QsoDateTime::new("2026/03/28 14:30")?,
            QsoFrequency::new("145.000.000")?,
            QsoMode::Dv,
            QsoRfPower::High,
            QsoFastData::Enabled,
        )
        .caller("W4CDR")
        .memo("raw memo")
        .called("CQCQCQ")
        .rx_rpt1("W4MOE  B")
        .rx_rpt2("W4MOE  G")
        .message("Hello")
        .repeater_control("raw-control")
        .bk("raw-bk")
        .emr("raw-emr")
        .build()
    }

    #[test]
    fn exact_closed_values_round_trip() -> TestResult {
        let entry = sample_entry()?;
        let bytes = write_qso_log(std::slice::from_ref(&entry));
        let parsed = parse_qso_log(&bytes)?;
        assert_eq!(parsed, vec![entry]);

        let parsed_entry = parsed.first().ok_or("missing parsed entry")?;
        assert_eq!(parsed_entry.direction(), QsoDirection::Tx);
        assert_eq!(parsed_entry.date().year(), 2026);
        assert_eq!(parsed_entry.frequency().as_hz(), 145_000_000);
        assert_eq!(parsed_entry.mode(), QsoMode::Dv);
        assert_eq!(parsed_entry.rf_power(), QsoRfPower::High);
        assert_eq!(parsed_entry.fast_data(), QsoFastData::Enabled);
        Ok(())
    }

    #[test]
    fn every_official_mode_power_and_direction_is_accepted() -> TestResult {
        for direction in [QsoDirection::Tx, QsoDirection::Rx] {
            assert_eq!(direction.as_str().parse::<QsoDirection>()?, direction);
        }
        for mode in [QsoMode::Dv, QsoMode::Fm, QsoMode::FmN] {
            assert_eq!(mode.as_str().parse::<QsoMode>()?, mode);
        }
        for power in [
            QsoRfPower::SLow,
            QsoRfPower::Low1,
            QsoRfPower::Low2,
            QsoRfPower::Mid,
            QsoRfPower::High,
        ] {
            assert_eq!(power.as_str().parse::<QsoRfPower>()?, power);
        }
        for fast_data in [QsoFastData::Disabled, QsoFastData::Enabled] {
            assert_eq!(fast_data.as_str().parse::<QsoFastData>()?, fast_data);
        }
        Ok(())
    }

    #[test]
    fn invented_closed_spellings_are_rejected() {
        for invalid in ["", "Tx", "TRANSMIT"] {
            assert!(
                invalid.parse::<QsoDirection>().is_err(),
                "accepted direction {invalid:?}"
            );
        }
        for invalid in ["NFM", "AM", "FMN", ""] {
            assert!(
                invalid.parse::<QsoMode>().is_err(),
                "accepted mode {invalid:?}"
            );
        }
        for invalid in ["High", "Mid", "LOW", ""] {
            assert!(
                invalid.parse::<QsoRfPower>().is_err(),
                "accepted RF power {invalid:?}"
            );
        }
        for invalid in ["", "false", "2"] {
            assert!(
                invalid.parse::<QsoFastData>().is_err(),
                "accepted Fast Data {invalid:?}"
            );
        }
    }

    #[test]
    fn date_requires_exact_valid_calendar_value() -> TestResult {
        let leap = QsoDateTime::new("2024/02/29 23:59")?;
        assert_eq!(leap.month(), 2);
        assert_eq!(leap.day(), 29);
        assert_eq!(leap.hour(), 23);
        assert_eq!(leap.minute(), 59);

        for invalid in [
            "2023/02/29 23:59",
            "2026/13/01 00:00",
            "2026/01/01 24:00",
            "2026/1/01 00:00",
            "2026-01-01 00:00",
        ] {
            assert!(
                QsoDateTime::new(invalid).is_err(),
                "accepted invalid date {invalid:?}"
            );
        }
        Ok(())
    }

    #[test]
    fn frequency_requires_exact_fixed_width_format() -> TestResult {
        let frequency = QsoFrequency::from_frequency(Frequency::new(439_310_125))?;
        assert_eq!(frequency.as_str(), "439.310.125");
        assert_eq!(frequency.as_hz(), 439_310_125);

        for invalid in ["439.31.000", "0439.310.000", "439310000", "439.310.00A"] {
            assert!(
                QsoFrequency::new(invalid).is_err(),
                "accepted invalid frequency {invalid:?}"
            );
        }
        assert!(QsoFrequency::from_frequency(Frequency::new(1_000_000_000)).is_err());
        Ok(())
    }

    #[test]
    fn undocumented_columns_are_preserved_without_interpretation() -> TestResult {
        let entry = QsoEntry::builder(
            QsoDirection::Rx,
            QsoDateTime::new("2026/08/03 09:07")?,
            QsoFrequency::new("430.125.625")?,
            QsoMode::FmN,
            QsoRfPower::Low2,
            QsoFastData::Disabled,
        )
        .my_latitude("source-defined latitude")
        .my_longitude("source-defined longitude")
        .my_altitude("source-defined altitude")
        .s_meter("source-defined meter")
        .caller("caller bytes")
        .memo("memo bytes")
        .called("called bytes")
        .rx_rpt1("rpt1 bytes")
        .rx_rpt2("rpt2 bytes")
        .message("message bytes")
        .repeater_control("control bytes")
        .bk("bk bytes")
        .emr("emr bytes")
        .latitude("remote latitude bytes")
        .longitude("remote longitude bytes")
        .altitude("remote altitude bytes")
        .course("course bytes")
        .speed("speed bytes")
        .build()?;
        let reparsed = parse_qso_log(&write_qso_log(std::slice::from_ref(&entry)))?;
        assert_eq!(reparsed, vec![entry]);
        Ok(())
    }

    #[test]
    fn builder_rejects_tsv_shape_changes() -> TestResult {
        let result = QsoEntry::builder(
            QsoDirection::Tx,
            QsoDateTime::new("2026/03/28 14:30")?,
            QsoFrequency::new("145.000.000")?,
            QsoMode::Dv,
            QsoRfPower::Mid,
            QsoFastData::Disabled,
        )
        .memo("first row\ninjected row")
        .build();
        assert!(matches!(result, Err(error) if error.column() == "Memo"));
        Ok(())
    }

    #[test]
    fn parser_rejects_bad_header_width_and_utf8() {
        assert!(matches!(
            parse_qso_log(b""),
            Err(SdCardError::HeaderMismatch {
                file_type: FILE_TYPE,
                ..
            })
        ));

        let obsolete_header = QSO_LOG_HEADER.replacen("Rx RPT1", "RPT1", 1);
        assert!(matches!(
            parse_qso_log(obsolete_header.as_bytes()),
            Err(SdCardError::HeaderMismatch { .. })
        ));

        let short = format!("{QSO_LOG_HEADER}\r\nTX\t2026/03/28 14:30\r\n");
        assert!(matches!(
            parse_qso_log(short.as_bytes()),
            Err(SdCardError::ColumnCount {
                line: 2,
                expected: QSO_LOG_COLUMNS,
                actual: 2,
            })
        ));

        let extra = format!("{QSO_LOG_HEADER}\r\n{}\r\n", ["x"; 25].join("\t"));
        assert!(matches!(
            parse_qso_log(extra.as_bytes()),
            Err(SdCardError::ColumnCount {
                line: 2,
                expected: QSO_LOG_COLUMNS,
                actual: 25,
            })
        ));

        assert!(matches!(
            parse_qso_log(b"header\r\n\xFF\r\n"),
            Err(SdCardError::InvalidUtf8 { .. })
        ));
    }

    #[test]
    fn parser_reports_the_closed_column_that_failed() -> TestResult {
        let entry = sample_entry()?;
        let valid = String::from_utf8(write_qso_log(std::slice::from_ref(&entry)))?;
        let invalid = valid.replacen("\tDV\t", "\tNFM\t", 1);
        assert!(matches!(
            parse_qso_log(invalid.as_bytes()),
            Err(SdCardError::InvalidField { line: 2, column, .. }) if column == "Mode"
        ));
        Ok(())
    }

    #[test]
    fn empty_log_round_trips() -> TestResult {
        let data = write_qso_log(&[]);
        assert!(parse_qso_log(&data)?.is_empty());
        Ok(())
    }
}
