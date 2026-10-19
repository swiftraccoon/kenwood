//! The VFO memory of a PM slot: each band's current VFO record and the VFO
//! band it tunes, the record each of the ten VFO bands keeps, and their
//! programmable VFO limits.
//!
//! At slot 0's addresses, [`SlotIndex::offset`] higher for another slot: band
//! A's current record at `0x50100` and band B's at `0x50128`, 40-byte channel
//! records ([`StoredChannel`]); the [`VfoBand::index`] each band has selected
//! at `0x50154` and `0x50164`; the ten VFO band records at
//! [`VfoBand::record_address`], each a channel record followed by eight bytes
//! kept unchanged; and the ten limit pairs at [`VfoBand::limits_address`]:
//! the lower limit and the first frequency above the upper limit,
//! little-endian hertz in whole megahertz. All of it lies inside the standard
//! backup schedule.
//!
//! On firmware 1.02 (type `K,2,1`) with PM Off active, slot 0's current record
//! and selected VFO band record of each band equaled that band's `FO` record.
//! A tune of band B written over the operation-panel USB changed only the
//! frequency bytes of those two records, and `FO` reported the new frequency
//! after the session.

use std::fmt;

use crate::memory::MemoryImage;
use crate::memory::chain;
use crate::radio::programming::PageReplacement;
use crate::types::{Band, CHANNEL_RECORD_SIZE, Frequency, SlotIndex, StoredChannel};

/// Slot 0 address of band A's current VFO record.
const CURRENT_A: u32 = 0x5_0100;
/// Slot 0 address of band B's current VFO record.
const CURRENT_B: u32 = 0x5_0128;
/// Slot 0 address of the VFO band band A has selected.
const SELECTED_A: u32 = 0x5_0154;
/// Slot 0 address of the VFO band band B has selected.
const SELECTED_B: u32 = 0x5_0164;
/// Slot 0 address of the first VFO band record.
const RECORDS: u32 = 0x5_0200;
/// Slot 0 address of the first limit pair.
const LIMITS: u32 = 0x5_0488;
/// Bytes of the four whole pages from [`CURRENT_A`] that hold every VFO
/// field.
const SPAN: u32 = 0x400;
/// Bytes of one VFO band record: a channel record and eight bytes kept
/// unchanged.
pub const VFO_BAND_RECORD_SIZE: usize = 48;

/// One of the ten VFO bands, four on band A and six on band B.
///
/// The ranges are the default programmable VFO limits the tested radio
/// stored, lower bound included and upper bound excluded; a VFO band record
/// and its limits stay inside its range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum VfoBand {
    /// Band A, 108 to 136 MHz.
    A118,
    /// Band A, 136 to 174 MHz.
    A144,
    /// Band A, 216 to 260 MHz.
    A220,
    /// Band A, 410 to 470 MHz.
    A430,
    /// Band B, 108 to 136 MHz.
    B118,
    /// Band B, 136 to 174 MHz.
    B144,
    /// Band B, 174 to 216 MHz.
    BVhf,
    /// Band B, 216 to 410 MHz.
    B200And300,
    /// Band B, 410 to 470 MHz.
    B430,
    /// Band B, 470 to 524 MHz.
    BUhf,
}

impl VfoBand {
    /// The VFO bands in stored order.
    pub const ALL: [Self; 10] = [
        Self::A118,
        Self::A144,
        Self::A220,
        Self::A430,
        Self::B118,
        Self::B144,
        Self::BVhf,
        Self::B200And300,
        Self::B430,
        Self::BUhf,
    ];

    /// The band the VFO band belongs to.
    #[must_use]
    pub const fn band(self) -> Band {
        match self {
            Self::A118 | Self::A144 | Self::A220 | Self::A430 => Band::A,
            Self::B118 | Self::B144 | Self::BVhf | Self::B200And300 | Self::B430 | Self::BUhf => {
                Band::B
            }
        }
    }

    /// The stored index, 0 to 9.
    #[must_use]
    pub const fn index(self) -> u8 {
        match self {
            Self::A118 => 0,
            Self::A144 => 1,
            Self::A220 => 2,
            Self::A430 => 3,
            Self::B118 => 4,
            Self::B144 => 5,
            Self::BVhf => 6,
            Self::B200And300 => 7,
            Self::B430 => 8,
            Self::BUhf => 9,
        }
    }

    /// The VFO band of a stored index.
    #[must_use]
    pub const fn from_index(index: u8) -> Option<Self> {
        match index {
            0 => Some(Self::A118),
            1 => Some(Self::A144),
            2 => Some(Self::A220),
            3 => Some(Self::A430),
            4 => Some(Self::B118),
            5 => Some(Self::B144),
            6 => Some(Self::BVhf),
            7 => Some(Self::B200And300),
            8 => Some(Self::B430),
            9 => Some(Self::BUhf),
            _ => None,
        }
    }

    /// The range in whole megahertz: the lower bound and the first megahertz
    /// above the band.
    #[must_use]
    pub const fn range_mhz(self) -> (u16, u16) {
        match self {
            Self::A118 | Self::B118 => (108, 136),
            Self::A144 | Self::B144 => (136, 174),
            Self::A220 => (216, 260),
            Self::A430 | Self::B430 => (410, 470),
            Self::BVhf => (174, 216),
            Self::B200And300 => (216, 410),
            Self::BUhf => (470, 524),
        }
    }

    /// Image address of the VFO band record in `slot`: five records to a
    /// page, band B's from a new page.
    #[must_use]
    pub const fn record_address(self, slot: SlotIndex) -> u32 {
        let offset = match self {
            Self::A118 => 0,
            Self::A144 => 48,
            Self::A220 => 96,
            Self::A430 => 144,
            Self::B118 => 256,
            Self::B144 => 304,
            Self::BVhf => 352,
            Self::B200And300 => 400,
            Self::B430 => 448,
            Self::BUhf => 512,
        };
        RECORDS + offset + slot.offset()
    }

    /// Image address of the limit pair in `slot`.
    #[must_use]
    pub const fn limits_address(self, slot: SlotIndex) -> u32 {
        LIMITS + 8 * self.index() as u32 + slot.offset()
    }
}

impl fmt::Display for VfoBand {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::A118 => "A 118 MHz",
            Self::A144 => "A 144 MHz",
            Self::A220 => "A 220 MHz",
            Self::A430 => "A 430 MHz",
            Self::B118 => "B 118 MHz",
            Self::B144 => "B 144 MHz",
            Self::BVhf => "B VHF",
            Self::B200And300 => "B 200 & 300 MHz",
            Self::B430 => "B 430 MHz",
            Self::BUhf => "B UHF",
        })
    }
}

/// The programmable VFO limits of a VFO band, in whole megahertz: tuning
/// stays from the lower limit to the end of the upper limit's megahertz.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VfoLimits {
    lower_mhz: u16,
    upper_mhz: u16,
}

impl VfoLimits {
    /// Validate limits for `band`.
    ///
    /// # Errors
    ///
    /// Returns [`VfoError::InvalidLimits`] unless the band's range lower
    /// bound is at most `lower_mhz`, `lower_mhz` is at most `upper_mhz`, and
    /// `upper_mhz` lies below the range's upper bound.
    pub const fn new(band: VfoBand, lower_mhz: u16, upper_mhz: u16) -> Result<Self, VfoError> {
        let (minimum, end) = band.range_mhz();
        if minimum <= lower_mhz && lower_mhz <= upper_mhz && upper_mhz < end {
            Ok(Self {
                lower_mhz,
                upper_mhz,
            })
        } else {
            Err(VfoError::InvalidLimits {
                band,
                lower_mhz,
                upper_mhz,
            })
        }
    }

    /// The lower limit.
    #[must_use]
    pub const fn lower_mhz(self) -> u16 {
        self.lower_mhz
    }

    /// The upper limit, its whole megahertz included.
    #[must_use]
    pub const fn upper_mhz(self) -> u16 {
        self.upper_mhz
    }

    /// Whether `frequency` lies inside the limits.
    #[must_use]
    pub const fn contains(self, frequency: Frequency) -> bool {
        let hz = frequency.as_hz();
        hz >= self.lower_mhz as u32 * 1_000_000 && hz < (self.upper_mhz as u32 + 1) * 1_000_000
    }

    /// The stored form: the lower limit and the first hertz past the upper
    /// limit, little-endian.
    const fn to_bytes(self) -> [u8; 8] {
        let lower = (self.lower_mhz as u32 * 1_000_000).to_le_bytes();
        let end = ((self.upper_mhz as u32 + 1) * 1_000_000).to_le_bytes();
        [
            lower[0], lower[1], lower[2], lower[3], end[0], end[1], end[2], end[3],
        ]
    }

    /// Decode the stored pair of `band`.
    fn from_bytes(band: VfoBand, bytes: [u8; 8]) -> Result<Self, VfoError> {
        let [
            lower_0,
            lower_1,
            lower_2,
            lower_3,
            end_0,
            end_1,
            end_2,
            end_3,
        ] = bytes;
        let lower = u32::from_le_bytes([lower_0, lower_1, lower_2, lower_3]);
        let end = u32::from_le_bytes([end_0, end_1, end_2, end_3]);
        let malformed = VfoError::MalformedLimits { band, lower, end };
        if lower % 1_000_000 != 0 || end % 1_000_000 != 0 || end == 0 {
            return Err(malformed);
        }
        let lower_mhz = u16::try_from(lower / 1_000_000).map_err(|_| malformed.clone())?;
        let upper_mhz = u16::try_from(end / 1_000_000 - 1).map_err(|_| malformed.clone())?;
        Self::new(band, lower_mhz, upper_mhz).map_err(|_| malformed)
    }
}

/// One VFO band's stored record and limits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VfoBandMemory {
    band: VfoBand,
    record: StoredChannel,
    limits: VfoLimits,
}

impl VfoBandMemory {
    /// The VFO band.
    #[must_use]
    pub const fn band(&self) -> VfoBand {
        self.band
    }

    /// The record the VFO band keeps.
    #[must_use]
    pub const fn record(&self) -> &StoredChannel {
        &self.record
    }

    /// The programmable VFO limits.
    #[must_use]
    pub const fn limits(&self) -> VfoLimits {
        self.limits
    }
}

/// The VFO memory of one PM slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VfoMemory {
    slot: SlotIndex,
    current_a: StoredChannel,
    current_b: StoredChannel,
    selected_a: VfoBand,
    selected_b: VfoBand,
    bands: [VfoBandMemory; 10],
}

impl VfoMemory {
    /// Decode the VFO memory of `slot`.
    ///
    /// # Errors
    ///
    /// Returns [`VfoError::Record`] for a record [`StoredChannel::from_bytes`]
    /// refuses, [`VfoError::Selection`] for a selected index that names no
    /// VFO band of its band, and [`VfoError::MalformedLimits`] for a limit pair
    /// that is not whole megahertz inside its VFO band's range.
    pub fn read(image: &MemoryImage, slot: SlotIndex) -> Result<Self, VfoError> {
        let record = |address: u32, what: VfoRecord| {
            StoredChannel::from_bytes(&chain::stored_field::<CHANNEL_RECORD_SIZE>(image, address))
                .map_err(|source| VfoError::Record { what, source })
        };
        let selected = |address: u32, band: Band| {
            let [index] = chain::stored_field::<1>(image, address);
            VfoBand::from_index(index)
                .filter(|selected| selected.band() == band)
                .ok_or(VfoError::Selection { band, index })
        };
        let mut bands = Vec::with_capacity(VfoBand::ALL.len());
        for band in VfoBand::ALL {
            bands.push(VfoBandMemory {
                band,
                record: record(band.record_address(slot), VfoRecord::Band(band))?,
                limits: VfoLimits::from_bytes(
                    band,
                    chain::stored_field::<8>(image, band.limits_address(slot)),
                )?,
            });
        }
        Ok(Self {
            slot,
            current_a: record(CURRENT_A + slot.offset(), VfoRecord::Current(Band::A))?,
            current_b: record(CURRENT_B + slot.offset(), VfoRecord::Current(Band::B))?,
            selected_a: selected(SELECTED_A + slot.offset(), Band::A)?,
            selected_b: selected(SELECTED_B + slot.offset(), Band::B)?,
            bands: bands
                .try_into()
                .unwrap_or_else(|_| unreachable!("one memory per VFO band")),
        })
    }

    /// The slot.
    #[must_use]
    pub const fn slot(&self) -> SlotIndex {
        self.slot
    }

    /// The current VFO record of `band`.
    #[must_use]
    pub const fn current(&self, band: Band) -> &StoredChannel {
        match band {
            Band::A => &self.current_a,
            Band::B => &self.current_b,
        }
    }

    /// The VFO band `band` has selected.
    #[must_use]
    pub const fn selected(&self, band: Band) -> VfoBand {
        match band {
            Band::A => self.selected_a,
            Band::B => self.selected_b,
        }
    }

    /// The stored record and limits of `band`.
    #[must_use]
    pub fn band(&self, band: VfoBand) -> &VfoBandMemory {
        self.bands
            .get(usize::from(band.index()))
            .unwrap_or_else(|| unreachable!("one memory per VFO band, in index order"))
    }

    /// Every VFO band's record and limits, in stored order.
    pub fn bands(&self) -> impl Iterator<Item = &VfoBandMemory> {
        self.bands.iter()
    }

    /// Tune `band` to `record` as the radio does: select the VFO band of
    /// `band` whose limits contain the frequency, and store the record as
    /// both the current record and that VFO band's record.
    ///
    /// # Errors
    ///
    /// Returns [`VfoError::OutsideLimits`] when no VFO band of `band`
    /// admits the record's receive frequency.
    pub fn tune(&mut self, band: Band, record: StoredChannel) -> Result<VfoBand, VfoError> {
        let selected = self
            .bands
            .iter()
            .find(|memory| {
                memory.band.band() == band && memory.limits.contains(record.receive_frequency)
            })
            .map(|memory| memory.band)
            .ok_or(VfoError::OutsideLimits {
                band,
                hz: record.receive_frequency.as_hz(),
            })?;
        self.band_mut(selected).record = record.clone();
        match band {
            Band::A => {
                self.current_a = record;
                self.selected_a = selected;
            }
            Band::B => {
                self.current_b = record;
                self.selected_b = selected;
            }
        }
        Ok(selected)
    }

    /// Store `record` as `band`'s record, and as its band's current record
    /// when `band` is the selected VFO band.
    ///
    /// # Errors
    ///
    /// Returns [`VfoError::OutsideLimits`] when `band`'s limits leave out the
    /// record's receive frequency.
    pub fn set_band_record(
        &mut self,
        band: VfoBand,
        record: StoredChannel,
    ) -> Result<(), VfoError> {
        if !self.band(band).limits.contains(record.receive_frequency) {
            return Err(VfoError::OutsideLimits {
                band: band.band(),
                hz: record.receive_frequency.as_hz(),
            });
        }
        if self.selected(band.band()) == band {
            match band.band() {
                Band::A => self.current_a = record.clone(),
                Band::B => self.current_b = record.clone(),
            }
        }
        self.band_mut(band).record = record;
        Ok(())
    }

    /// Set the programmable VFO limits of `band`.
    ///
    /// # Errors
    ///
    /// Returns the [`VfoLimits::new`] errors, and [`VfoError::LimitsExcludeRecord`]
    /// when the limits leave out the VFO band's stored record or, for a
    /// selected VFO band, the current record.
    pub fn set_limits(
        &mut self,
        band: VfoBand,
        lower_mhz: u16,
        upper_mhz: u16,
    ) -> Result<(), VfoError> {
        let limits = VfoLimits::new(band, lower_mhz, upper_mhz)?;
        let selected = self.selected(band.band()) == band;
        let excluded = |record: &StoredChannel| !limits.contains(record.receive_frequency);
        if excluded(&self.band(band).record) || (selected && excluded(self.current(band.band()))) {
            return Err(VfoError::LimitsExcludeRecord { band });
        }
        self.band_mut(band).limits = limits;
        Ok(())
    }

    /// The writes of the slot's VFO pages whose bytes change, keeping every
    /// other byte, the bytes after each VFO band record included, as `image`
    /// holds it.
    ///
    /// Apply them with [`crate::radio::McpSession::compare_exchange_pages`],
    /// which compares each page with a fresh read before writing.
    ///
    /// # Errors
    ///
    /// Returns the [`PageReplacement::new`] errors.
    pub fn page_replacements(
        &self,
        image: &MemoryImage,
    ) -> Result<Vec<PageReplacement>, crate::Error> {
        let start = CURRENT_A + self.slot.offset();
        let mut span = chain::stored_bytes(image, start, SPAN);
        let mut put = |address: u32, bytes: &[u8]| chain::put(&mut span, address - start, bytes);
        put(CURRENT_A + self.slot.offset(), &self.current_a.to_bytes());
        put(CURRENT_B + self.slot.offset(), &self.current_b.to_bytes());
        put(SELECTED_A + self.slot.offset(), &[self.selected_a.index()]);
        put(SELECTED_B + self.slot.offset(), &[self.selected_b.index()]);
        for memory in &self.bands {
            put(
                memory.band.record_address(self.slot),
                &memory.record.to_bytes(),
            );
            put(
                memory.band.limits_address(self.slot),
                &memory.limits.to_bytes(),
            );
        }
        chain::changed_pages(image, start, &span)
    }

    fn band_mut(&mut self, band: VfoBand) -> &mut VfoBandMemory {
        self.bands
            .get_mut(usize::from(band.index()))
            .unwrap_or_else(|| unreachable!("one memory per VFO band, in index order"))
    }
}

/// Which stored VFO record a [`VfoError::Record`] names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VfoRecord {
    /// A band's current record.
    Current(Band),
    /// A VFO band's record.
    Band(VfoBand),
}

impl fmt::Display for VfoRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Current(band) => write!(formatter, "band {band}'s current VFO record"),
            Self::Band(band) => write!(formatter, "the {band} VFO record"),
        }
    }
}

/// VFO memory the library refuses.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum VfoError {
    /// A stored record did not decode.
    #[error("{what}: {source}")]
    Record {
        /// The record.
        what: VfoRecord,
        /// The decoding error.
        source: crate::error::ValidationError,
    },
    /// A selected index names no VFO band of its band.
    #[error("band {band}'s selected VFO band index {index} names none of its VFO bands")]
    Selection {
        /// The band.
        band: Band,
        /// The stored index.
        index: u8,
    },
    /// A stored limit pair is not whole megahertz inside its VFO band.
    #[error("{band} limits {lower}..{end} Hz are not whole megahertz inside the band")]
    MalformedLimits {
        /// The VFO band.
        band: VfoBand,
        /// The stored lower limit in hertz.
        lower: u32,
        /// The stored end in hertz.
        end: u32,
    },
    /// Limits fall outside the VFO band's range or are reversed.
    #[error("{band} limits {lower_mhz} to {upper_mhz} MHz are reversed or outside the band")]
    InvalidLimits {
        /// The VFO band.
        band: VfoBand,
        /// The requested lower limit.
        lower_mhz: u16,
        /// The requested upper limit.
        upper_mhz: u16,
    },
    /// No VFO band of the band admits the frequency.
    #[error("no VFO band of band {band} admits {hz} Hz")]
    OutsideLimits {
        /// The band.
        band: Band,
        /// The receive frequency in hertz.
        hz: u32,
    },
    /// New limits leave out a record the VFO band holds.
    #[error("the new {band} limits leave out a stored VFO record")]
    LimitsExcludeRecord {
        /// The VFO band.
        band: VfoBand,
    },
}

#[cfg(test)]
#[path = "vfo_tests.rs"]
mod tests;
