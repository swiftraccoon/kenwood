//! Checked outgoing report values and caller-selected AX.25 routing.
//!
//! See the [packet construction workflows](crate#packet-construction-and-parsing)
//! for position, object and Mic-E reports, and the
//! [message workflow](crate#messages-and-transmission-state) for retained routing.

use ax25_codec::{Ax25Address, DigipeaterPath};

use crate::{
    AprsError, AprsReportTimestamp, AprsSymbol, Course, ItemName, Latitude, Longitude, MiceMessage,
    MiceSpeed, MiceStatusText, ObjectName, PositionReportText, ReportState, Tocall,
};

/// Owned source, software identity and digipeater path for an APRS UI frame.
///
/// Encoding consumes this header and transfers its allocations into the packet.
/// Mic-E reports derive their destination from position and message bits;
/// [`MiceReport`] therefore takes source and path separately.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AprsFrameHeader {
    pub(crate) source: Ax25Address,
    pub(crate) tocall: Tocall,
    pub(crate) path: DigipeaterPath,
}

impl AprsFrameHeader {
    /// Selects the checked source, software identity and complete relay path.
    #[must_use]
    pub const fn new(source: Ax25Address, tocall: Tocall, path: DigipeaterPath) -> Self {
        Self {
            source,
            tocall,
            path,
        }
    }
    /// Borrows the source station's complete address.
    #[must_use]
    pub const fn source(&self) -> &Ax25Address {
        &self.source
    }
    /// Borrows the software identity used as the destination callsign.
    #[must_use]
    pub const fn tocall(&self) -> &Tocall {
        &self.tocall
    }
    /// Borrows the ordered relay path.
    #[must_use]
    pub const fn path(&self) -> &DigipeaterPath {
        &self.path
    }
}

/// An uncompressed geographic position, symbol and checked trailing text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PositionReport {
    pub(crate) latitude: Latitude,
    pub(crate) longitude: Longitude,
    pub(crate) symbol: AprsSymbol,
    pub(crate) comment: PositionReportText,
}

impl PositionReport {
    /// Retains checked coordinates, symbol and text without copying their storage.
    #[must_use]
    pub const fn new(
        latitude: Latitude,
        longitude: Longitude,
        symbol: AprsSymbol,
        comment: PositionReportText,
    ) -> Self {
        Self {
            latitude,
            longitude,
            symbol,
            comment,
        }
    }
    /// Returns latitude in its checked geographic domain.
    #[must_use]
    pub const fn latitude(&self) -> Latitude {
        self.latitude
    }
    /// Returns longitude in its checked geographic domain.
    #[must_use]
    pub const fn longitude(&self) -> Longitude {
        self.longitude
    }
    /// Returns the symbol-table/code pair.
    #[must_use]
    pub const fn symbol(&self) -> AprsSymbol {
        self.symbol
    }
    /// Borrows the trailing report text.
    #[must_use]
    pub const fn comment(&self) -> &PositionReportText {
        &self.comment
    }
}

/// An outgoing named object with a required observation timestamp.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectReport {
    pub(crate) name: ObjectName,
    pub(crate) state: ReportState,
    pub(crate) timestamp: AprsReportTimestamp,
    pub(crate) position: PositionReport,
}

impl ObjectReport {
    /// Retains a checked name, live/killed state, timestamp and position.
    #[must_use]
    pub const fn new(
        name: ObjectName,
        state: ReportState,
        timestamp: AprsReportTimestamp,
        position: PositionReport,
    ) -> Self {
        Self {
            name,
            state,
            timestamp,
            position,
        }
    }
    /// Borrows the unpadded object name.
    #[must_use]
    pub const fn name(&self) -> &ObjectName {
        &self.name
    }
    /// Returns the live or killed state.
    #[must_use]
    pub const fn state(&self) -> ReportState {
        self.state
    }
    /// Returns the observation timestamp.
    #[must_use]
    pub const fn timestamp(&self) -> AprsReportTimestamp {
        self.timestamp
    }
    /// Borrows the position and its trailing text.
    #[must_use]
    pub const fn position(&self) -> &PositionReport {
        &self.position
    }
}

/// An outgoing named item and its position, without a timestamp.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemReport {
    pub(crate) name: ItemName,
    pub(crate) state: ReportState,
    pub(crate) position: PositionReport,
}

impl ItemReport {
    /// Retains a checked item name, live/killed state and position.
    #[must_use]
    pub const fn new(name: ItemName, state: ReportState, position: PositionReport) -> Self {
        Self {
            name,
            state,
            position,
        }
    }
    /// Borrows the item name.
    #[must_use]
    pub const fn name(&self) -> &ItemName {
        &self.name
    }
    /// Returns the live or killed state.
    #[must_use]
    pub const fn state(&self) -> ReportState {
        self.state
    }
    /// Borrows the position and trailing text.
    #[must_use]
    pub const fn position(&self) -> &PositionReport {
        &self.position
    }
}

/// Checked Mic-E position, motion, status selector and trailing text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MiceReport {
    pub(crate) latitude: Latitude,
    pub(crate) longitude: Longitude,
    pub(crate) speed: MiceSpeed,
    pub(crate) course: Course,
    pub(crate) message: MiceMessage,
    pub(crate) symbol: AprsSymbol,
    pub(crate) status_text: MiceStatusText,
}

impl MiceReport {
    /// Retains checked position and motion with the standard `OffDuty` selector.
    ///
    /// # Errors
    /// Returns [`AprsError::UnrepresentableMiceLongitude`] if rounding to
    /// hundredths of a minute reaches 180 degrees, outside Mic-E's degree table.
    pub fn new(
        latitude: Latitude,
        longitude: Longitude,
        speed: MiceSpeed,
        course: Course,
        symbol: AprsSymbol,
        status_text: MiceStatusText,
    ) -> Result<Self, AprsError> {
        if (longitude.as_degrees().abs() * 6_000.0).round() >= 180.0 * 6_000.0 {
            return Err(AprsError::UnrepresentableMiceLongitude);
        }
        Ok(Self {
            latitude,
            longitude,
            speed,
            course,
            message: MiceMessage::OffDuty,
            symbol,
            status_text,
        })
    }
    /// Selects one of the seven standard Mic-E messages or emergency.
    #[must_use]
    pub const fn with_message(mut self, message: MiceMessage) -> Self {
        self.message = message;
        self
    }
    /// Returns the checked latitude.
    #[must_use]
    pub const fn latitude(&self) -> Latitude {
        self.latitude
    }
    /// Returns the checked longitude.
    #[must_use]
    pub const fn longitude(&self) -> Longitude {
        self.longitude
    }
    /// Returns the Mic-E wire speed in knots.
    #[must_use]
    pub const fn speed(&self) -> MiceSpeed {
        self.speed
    }
    /// Returns the checked course.
    #[must_use]
    pub const fn course(&self) -> Course {
        self.course
    }
    /// Returns the Mic-E message selector.
    #[must_use]
    pub const fn message(&self) -> MiceMessage {
        self.message
    }
    /// Returns the symbol-table/code pair.
    #[must_use]
    pub const fn symbol(&self) -> AprsSymbol {
        self.symbol
    }
    /// Borrows the trailing status text.
    #[must_use]
    pub const fn status_text(&self) -> &MiceStatusText {
        &self.status_text
    }
}
