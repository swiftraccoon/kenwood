//! The six CALL channels: an FM and a DV channel for each of the radio's
//! three bands.
//!
//! Their records and flags occupy memory-channel table indexes 1132 to 1137
//! in [`CallChannel::ALL`] order; their names sit four name slots later, at
//! name-table indexes 1136 to 1141. On firmware 1.02 (type `K,2,1`), CALL mode
//! answers `FO band` with the stored record of the CALL channel for the range
//! the band was tuned to and the FM or DV mode selected before CALL was
//! entered, and answers `N` to `MD` and `FO` writes. [`CallChannelPlan`]
//! replaces stored records over MCP instead. Whether the radio displays the
//! stored names is not established.

use std::collections::BTreeMap;
use std::fmt;

use crate::error::{Error, ValidationError};
use crate::memory::MemoryImage;
use crate::radio::programming::PageReplacement;
use crate::types::stored::{data_address_at, flag_address_at, name_address_at};
use crate::types::{
    Address, CHANNEL_NAME_SIZE, CHANNEL_RECORD_SIZE, ChannelName, FLAG_RECORD_SIZE, Frequency,
    OperatingMode, PAGE_SIZE, Page, StoredChannel,
};

/// The band a CALL channel belongs to.
///
/// The receive ranges are the ones the official programming software keeps a
/// CALL frequency inside for type `K,2,1`; [`CallChannelPlan::new`] enforces
/// them, lower bound included and upper bound excluded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CallBand {
    /// 144 MHz band, receive range 136 to 174 MHz.
    Vhf,
    /// 220 MHz band, receive range 216 to 260 MHz.
    Band220,
    /// 430 MHz band, receive range 410 to 470 MHz.
    Uhf,
}

impl CallBand {
    /// The bands in table order.
    pub const ALL: [Self; 3] = [Self::Vhf, Self::Band220, Self::Uhf];

    /// Lowest admitted receive frequency, in hertz.
    #[must_use]
    pub const fn minimum_hz(self) -> u32 {
        match self {
            Self::Vhf => 136_000_000,
            Self::Band220 => 216_000_000,
            Self::Uhf => 410_000_000,
        }
    }

    /// First receive frequency above the band, in hertz.
    #[must_use]
    pub const fn end_hz(self) -> u32 {
        match self {
            Self::Vhf => 174_000_000,
            Self::Band220 => 260_000_000,
            Self::Uhf => 470_000_000,
        }
    }

    /// Whether `frequency` lies in the band's receive range.
    #[must_use]
    pub const fn contains(self, frequency: Frequency) -> bool {
        let hz = frequency.as_hz();
        hz >= self.minimum_hz() && hz < self.end_hz()
    }

    const fn position(self) -> u16 {
        match self {
            Self::Vhf => 0,
            Self::Band220 => 1,
            Self::Uhf => 2,
        }
    }
}

impl fmt::Display for CallBand {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Vhf => "VHF",
            Self::Band220 => "220 MHz",
            Self::Uhf => "UHF",
        })
    }
}

/// Whether a CALL channel is the band's FM or DV channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CallMode {
    /// The FM channel; its record holds FM.
    Fm,
    /// The D-STAR channel; its record holds DV.
    Dv,
}

impl CallMode {
    /// The operating mode a record of this kind holds.
    #[must_use]
    pub const fn operating_mode(self) -> OperatingMode {
        match self {
            Self::Fm => OperatingMode::Fm,
            Self::Dv => OperatingMode::Dv,
        }
    }

    const fn position(self) -> u16 {
        match self {
            Self::Fm => 0,
            Self::Dv => 1,
        }
    }
}

/// One of the six CALL channels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CallChannel {
    band: CallBand,
    mode: CallMode,
}

impl CallChannel {
    /// Table index of the first CALL record and flag.
    pub const FIRST_INDEX: u16 = 1132;
    /// Name-table index of the first CALL name.
    pub const FIRST_NAME_INDEX: u16 = 1136;
    /// The channels in table order: VHF FM, VHF DV, 220 MHz FM, 220 MHz DV,
    /// UHF FM, UHF DV.
    pub const ALL: [Self; 6] = [
        Self::new(CallBand::Vhf, CallMode::Fm),
        Self::new(CallBand::Vhf, CallMode::Dv),
        Self::new(CallBand::Band220, CallMode::Fm),
        Self::new(CallBand::Band220, CallMode::Dv),
        Self::new(CallBand::Uhf, CallMode::Fm),
        Self::new(CallBand::Uhf, CallMode::Dv),
    ];

    /// The channel of `band` and `mode`.
    #[must_use]
    pub const fn new(band: CallBand, mode: CallMode) -> Self {
        Self { band, mode }
    }

    /// The band.
    #[must_use]
    pub const fn band(self) -> CallBand {
        self.band
    }

    /// FM or DV.
    #[must_use]
    pub const fn mode(self) -> CallMode {
        self.mode
    }

    /// Position in [`Self::ALL`], from 0 to 5.
    #[must_use]
    pub const fn position(self) -> u16 {
        2 * self.band.position() + self.mode.position()
    }

    /// Table index of the record and flag, 1132 to 1137.
    #[must_use]
    pub const fn index(self) -> u16 {
        Self::FIRST_INDEX + self.position()
    }

    /// Image address of the flag record.
    #[must_use]
    pub const fn flag_address(self) -> usize {
        flag_address_at(self.index())
    }

    /// Image address of the channel record.
    #[must_use]
    pub const fn data_address(self) -> usize {
        data_address_at(self.index())
    }

    /// Image address of the name, four name slots after the record's index.
    #[must_use]
    pub const fn name_address(self) -> usize {
        name_address_at(Self::FIRST_NAME_INDEX + self.position())
    }
}

impl fmt::Display for CallChannel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mode = match self.mode {
            CallMode::Fm => "FM",
            CallMode::Dv => "DV",
        };
        write!(formatter, "CALL {} {mode}", self.band)
    }
}

/// A CALL channel's stored flag, record and name.
///
/// The flag is kept raw: byte 0 is `0x01`, `0x02` or `0x03` for the VHF,
/// 220 MHz and UHF channels, byte 1 is `0xFF`, byte 2 holds group 34 and byte
/// 3 is `0xFF` on firmware 1.02. A flag whose byte 0 is `0xFF` marks the slot
/// empty and leaves the record undecoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallChannelEntry {
    channel: CallChannel,
    flag: [u8; FLAG_RECORD_SIZE],
    record: Option<StoredChannel>,
    raw_record: [u8; CHANNEL_RECORD_SIZE],
    name: ChannelName,
}

impl CallChannelEntry {
    /// Decode the flag, record and name of `channel`.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::StoredChannelLength`] for a slice of the
    /// wrong length and, for a stored slot, the [`StoredChannel::from_bytes`]
    /// errors.
    pub fn from_records(
        channel: CallChannel,
        flag: &[u8],
        record: &[u8],
        name: &[u8],
    ) -> Result<Self, ValidationError> {
        let flag: [u8; FLAG_RECORD_SIZE] = flag
            .try_into()
            .map_err(|_| ValidationError::StoredChannelLength { actual: flag.len() })?;
        let raw_record: [u8; CHANNEL_RECORD_SIZE] =
            record
                .try_into()
                .map_err(|_| ValidationError::StoredChannelLength {
                    actual: record.len(),
                })?;
        let name: [u8; CHANNEL_NAME_SIZE] = name
            .try_into()
            .map_err(|_| ValidationError::StoredChannelLength { actual: name.len() })?;
        let record = if flag[0] == crate::types::BandCode::EMPTY {
            None
        } else {
            Some(StoredChannel::from_bytes(&raw_record)?)
        };
        Ok(Self {
            channel,
            flag,
            record,
            raw_record,
            name: ChannelName::from_bytes(name),
        })
    }

    /// The channel.
    #[must_use]
    pub const fn channel(&self) -> CallChannel {
        self.channel
    }

    /// The four flag bytes.
    #[must_use]
    pub const fn flag(&self) -> [u8; FLAG_RECORD_SIZE] {
        self.flag
    }

    /// The decoded record of a stored slot.
    #[must_use]
    pub const fn record(&self) -> Option<&StoredChannel> {
        self.record.as_ref()
    }

    /// The 40 record bytes, decoded or not.
    #[must_use]
    pub const fn raw_record(&self) -> &[u8; CHANNEL_RECORD_SIZE] {
        &self.raw_record
    }

    /// The stored name.
    #[must_use]
    pub const fn name(&self) -> ChannelName {
        self.name
    }
}

impl fmt::Display for CallChannelEntry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.record {
            Some(record) => {
                write!(
                    formatter,
                    "{}: {} {}",
                    self.channel, record.receive_frequency, record.mode
                )?;
                let name = self.name.text();
                if !name.is_empty() {
                    write!(formatter, " \"{name}\"")?;
                }
                Ok(())
            }
            None => write!(formatter, "{}: empty", self.channel),
        }
    }
}

/// New records for CALL channels, planned as whole record-page writes.
///
/// Each record must keep its channel's band range and mode; the flags and
/// names stay as stored. Apply the replacements with
/// [`crate::radio::McpSession::compare_exchange_pages`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallChannelPlan {
    records: BTreeMap<CallChannel, StoredChannel>,
}

impl CallChannelPlan {
    /// Collect one record per channel.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::DuplicateCallChannel`] when a channel
    /// appears twice, [`ValidationError::CallFrequencyOutOfRange`] when a
    /// receive frequency lies outside the channel's [`CallBand`] range, and
    /// [`ValidationError::CallChannelMode`] when a record's mode is not
    /// [`CallMode::operating_mode`] of its channel.
    pub fn new(
        records: impl IntoIterator<Item = (CallChannel, StoredChannel)>,
    ) -> Result<Self, ValidationError> {
        let mut plan = BTreeMap::new();
        for (channel, record) in records {
            let band = channel.band();
            if !band.contains(record.receive_frequency) {
                return Err(ValidationError::CallFrequencyOutOfRange {
                    channel,
                    hz: record.receive_frequency.as_hz(),
                    minimum_hz: band.minimum_hz(),
                    end_hz: band.end_hz(),
                });
            }
            let expected = channel.mode().operating_mode();
            if record.mode != expected {
                return Err(ValidationError::CallChannelMode {
                    channel,
                    expected,
                    actual: record.mode,
                });
            }
            if plan.insert(channel, record).is_some() {
                return Err(ValidationError::DuplicateCallChannel { channel });
            }
        }
        Ok(Self { records: plan })
    }

    /// The planned channels and records, in table order.
    pub fn records(&self) -> impl Iterator<Item = (CallChannel, &StoredChannel)> {
        self.records
            .iter()
            .map(|(channel, record)| (*channel, record))
    }

    /// The record pages the plan touches, each once, in address order.
    #[must_use]
    pub fn pages(&self) -> Vec<Page> {
        let mut pages: Vec<Page> = Vec::new();
        for channel in self.records.keys() {
            if let Some(page) = record_page(*channel)
                && pages.last() != Some(&page)
            {
                pages.push(page);
            }
        }
        pages
    }

    /// The writes of the touched pages whose bytes change, each keeping the
    /// other records on its page as `image` holds them.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::CallChannelNotStored`] when `image`'s flag
    /// marks a planned channel empty, and the [`PageReplacement::new`]
    /// errors.
    pub fn page_replacements(&self, image: &MemoryImage) -> Result<Vec<PageReplacement>, Error> {
        let bytes = image.as_bytes();
        for channel in self.records.keys() {
            if bytes.get(channel.flag_address()) == Some(&crate::types::BandCode::EMPTY) {
                return Err(ValidationError::CallChannelNotStored { channel: *channel }.into());
            }
        }
        let mut replacements = Vec::new();
        for page in self.pages() {
            let start = page.address().as_usize();
            let expected = bytes.get(start..start + PAGE_SIZE).unwrap_or_default();
            let mut replacement = expected.to_vec();
            for (channel, record) in &self.records {
                if record_page(*channel) != Some(page) {
                    continue;
                }
                let offset = channel.data_address() - start;
                for (destination, source) in
                    replacement.iter_mut().skip(offset).zip(record.to_bytes())
                {
                    *destination = source;
                }
            }
            if replacement != expected {
                replacements.push(PageReplacement::new(page, expected, &replacement)?);
            }
        }
        Ok(replacements)
    }
}

/// The 256-byte page holding `channel`'s record.
fn record_page(channel: CallChannel) -> Option<Page> {
    let start = channel.data_address() / PAGE_SIZE * PAGE_SIZE;
    let address = Address::new(u32::try_from(start).ok()?).ok()?;
    Page::new(address, PAGE_SIZE).ok()
}

#[cfg(test)]
#[path = "call_tests.rs"]
mod tests;
