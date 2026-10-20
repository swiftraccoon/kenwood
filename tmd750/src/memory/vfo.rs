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
//! little-endian hertz in whole megahertz; then band B's step segment table
//! at [`StepSegment::address`], a receive step and a mode byte per segment.
//! All of it lies inside the standard backup schedule. Tuning by frequency
//! also reads the slot's Auto Repeater Offset menu setting
//! (`radio.AutoOffset`).
//!
//! On firmware 1.02 (type `K,2,1`) with PM Off active, slot 0's current record
//! and selected VFO band record of each band equaled that band's `FO` record.
//! A tune of band B written over the operation-panel USB changed only the
//! frequency bytes of those two records, and `FO` reported the new frequency
//! after the session. Setting a VFO frequency with `FQ` stored the records
//! [`VfoMemory::frequency_record`] computes: within the selected VFO band it
//! changed only the receive frequency, the receive step when band B entered
//! another step segment, and the shift under Auto Repeater Offset; into
//! another VFO band it took that band's stored record. After `SF` selected
//! 12.5 kHz on band B at 223.020 MHz, entering 300.000 MHz and then 223.020
//! MHz tuned 223.0125 MHz with the 12.5 kHz step: the segment kept the
//! selected step and the entry rounded down to it. The radio wrote the
//! current record into the selected VFO band's record no later than its next
//! programming-mode entry, so tuning away first left the earlier record.

use std::fmt;

use crate::error::{SchemaError, SettingName, ValidationError};
use crate::memory::chain;
use crate::memory::{MemoryImage, MenuFieldId};
use crate::radio::programming::PageReplacement;
use crate::types::{
    Band, CHANNEL_RECORD_SIZE, Frequency, OperatingMode, ShiftDirection, SlotIndex, StepSize,
    StoredChannel,
};
use kenwood_schema::DecodedFieldValue;

#[cfg(test)]
#[path = "vfo_tests.rs"]
mod tests;

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
/// Slot 0 address of the band B step segment table.
const SEGMENTS: u32 = 0x5_04DA;
/// Bytes of one VFO band record: a channel record and eight bytes kept
/// unchanged.
pub const VFO_BAND_RECORD_SIZE: usize = 48;

/// The band B step segments in stored order, in whole megahertz: the lower
/// bound and the first megahertz above the segment.
const SEGMENT_RANGES_MHZ: [(u16, u16); 17] = [
    (108, 136),
    (136, 144),
    (144, 148),
    (148, 174),
    (174, 216),
    (216, 220),
    (220, 225),
    (225, 410),
    (225, 410),
    (225, 410),
    (225, 410),
    (225, 410),
    (225, 410),
    (410, 420),
    (420, 450),
    (450, 470),
    (470, 524),
];

/// The TM-D750A Auto Repeater Offset plan of the User Manual: receive ranges
/// in hertz, lower bound included and upper bound excluded, and their shift.
/// Every other frequency is simplex.
const TYPE_K_OFFSETS: [(u32, u32, ShiftDirection); 8] = [
    (145_100_000, 145_500_000, ShiftDirection::Minus),
    (146_000_000, 146_400_000, ShiftDirection::Plus),
    (146_600_000, 147_000_000, ShiftDirection::Minus),
    (147_000_000, 147_400_000, ShiftDirection::Plus),
    (147_600_000, 148_000_000, ShiftDirection::Minus),
    (223_920_000, 225_000_000, ShiftDirection::Minus),
    (442_000_000, 445_000_000, ShiftDirection::Plus),
    (447_000_000, 450_000_000, ShiftDirection::Minus),
];

/// The shift the type `K` Auto Repeater Offset plan gives `frequency`.
fn type_k_shift(frequency: Frequency) -> ShiftDirection {
    let hz = frequency.as_hz();
    TYPE_K_OFFSETS
        .iter()
        .find(|(lower, end, _)| (*lower..*end).contains(&hz))
        .map_or(ShiftDirection::Simplex, |(_, _, shift)| *shift)
}

/// A band B frequency segment with its own stored receive step: selecting a
/// step there stores it, and tuning band B by frequency into another segment
/// takes that segment's step.
///
/// The ranges are the official program's list for type `K`, lower bound
/// included and upper bound excluded; entries 7 to 12 all cover 225 to 410
/// MHz, and [`StepSegment::of`] takes the first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StepSegment(u8);

integer_formats!(StepSegment, index);

impl TryFrom<u8> for StepSegment {
    type Error = ValidationError;
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        if value < 17 {
            Ok(Self(value))
        } else {
            Err(ValidationError::SettingOutOfRange {
                setting: SettingName::StepSegment,
                value: u64::from(value),
                max: 16,
            })
        }
    }
}

impl From<StepSegment> for u8 {
    fn from(value: StepSegment) -> Self {
        value.index()
    }
}

impl StepSegment {
    /// The segments in stored order.
    pub const ALL: [Self; 17] = [
        Self(0),
        Self(1),
        Self(2),
        Self(3),
        Self(4),
        Self(5),
        Self(6),
        Self(7),
        Self(8),
        Self(9),
        Self(10),
        Self(11),
        Self(12),
        Self(13),
        Self(14),
        Self(15),
        Self(16),
    ];

    /// The first segment holding `frequency`.
    #[must_use]
    pub fn of(frequency: Frequency) -> Self {
        let mhz = frequency.as_hz() / 1_000_000;
        Self::ALL
            .into_iter()
            .find(|segment| {
                let (lower, end) = segment.range_mhz();
                (u32::from(lower)..u32::from(end)).contains(&mhz)
            })
            .unwrap_or_else(|| unreachable!("the segments cover every Frequency"))
    }

    /// The stored index, 0 to 16.
    #[must_use]
    pub const fn index(self) -> u8 {
        self.0
    }

    /// The range in whole megahertz: the lower bound and the first megahertz
    /// above the segment.
    #[must_use]
    pub fn range_mhz(self) -> (u16, u16) {
        SEGMENT_RANGES_MHZ
            .get(usize::from(self.0))
            .copied()
            .unwrap_or_else(|| unreachable!("one range per segment"))
    }

    /// Image address of the segment's step byte in `slot`; its mode byte
    /// follows.
    #[must_use]
    pub const fn address(self, slot: SlotIndex) -> u32 {
        SEGMENTS + 2 * self.0 as u32 + slot.offset()
    }
}

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
    segments: [[u8; 2]; 17],
    auto_offset: bool,
}

impl VfoMemory {
    /// Decode the VFO memory of `slot`.
    ///
    /// # Errors
    ///
    /// Returns [`VfoError::Record`] for a record [`StoredChannel::from_bytes`]
    /// refuses, [`VfoError::Selection`] for a selected index that names no
    /// VFO band of its band, [`VfoError::MalformedLimits`] for a limit pair
    /// that is not whole megahertz inside its VFO band's range, and
    /// [`VfoError::AutoOffset`] when the Auto Repeater Offset setting does not
    /// decode.
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
            segments: StepSegment::ALL
                .map(|segment| chain::stored_field::<2>(image, segment.address(slot))),
            auto_offset: matches!(
                image
                    .slot(slot)
                    .read(&MenuFieldId::RADIO_AUTO_OFFSET.field().descriptor)
                    .map_err(|source| VfoError::AutoOffset { source })?,
                DecodedFieldValue::Bool(true)
            ),
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

    /// The receive step band B's `segment` stores, or `None` when its stored
    /// byte names no step.
    #[must_use]
    pub fn segment_step(&self, segment: StepSegment) -> Option<StepSize> {
        let [step, _mode] = self.segments.get(usize::from(segment.0)).copied()?;
        StepSize::try_from(step).ok()
    }

    /// Whether the slot's Auto Repeater Offset (`radio.AutoOffset`) is on.
    #[must_use]
    pub const fn auto_repeater_offset(&self) -> bool {
        self.auto_offset
    }

    /// The VFO band and record `band` holds after its frequency is set to
    /// `frequency` alone, as `FQ` sets it in VFO mode.
    ///
    /// The record starts as the current record when the VFO band of `band`
    /// whose limits hold `frequency` is the selected one, and as that VFO
    /// band's stored record otherwise. On band B, a frequency in another
    /// [`StepSegment`] than the starting record's takes that segment's stored
    /// receive step; the transmit step is kept. The receive frequency is
    /// `frequency` rounded down to a multiple of the receive step, kept as
    /// given with the 8.33 kHz step. With [`Self::auto_repeater_offset`] on, a
    /// record without reverse, split or DV mode takes the shift of the
    /// TM-D750A Auto Repeater Offset plan in the User Manual; its offset and
    /// tone fields are kept.
    ///
    /// # Errors
    ///
    /// Returns [`VfoError::OutsideLimits`] when no VFO band of `band` admits
    /// `frequency`, or the rounded frequency falls outside that VFO band's
    /// limits.
    pub fn frequency_record(
        &self,
        band: Band,
        frequency: Frequency,
    ) -> Result<(VfoBand, StoredChannel), VfoError> {
        let target = self.vfo_band_for(band, frequency)?;
        let start = if target == self.selected(band) {
            self.current(band)
        } else {
            &self.band(target).record
        };
        let mut record = start.clone();
        let segment = StepSegment::of(frequency);
        if band == Band::B
            && StepSegment::of(start.receive_frequency) != segment
            && let Some(step) = self.segment_step(segment)
        {
            record.receive_step = step;
        }
        let step = record.receive_step;
        let hz = if step == StepSize::Hz8330 {
            frequency.as_hz()
        } else {
            frequency.as_hz() / step.as_hz() * step.as_hz()
        };
        let outside = VfoError::OutsideLimits { band, hz };
        let tuned = Frequency::new(hz).map_err(|_| outside.clone())?;
        if !self.band(target).limits.contains(tuned) {
            return Err(outside);
        }
        record.receive_frequency = tuned;
        if self.auto_offset
            && !record.reverse
            && !record.split()
            && record.mode.mode() != OperatingMode::Dv
        {
            record.shift = type_k_shift(tuned);
        }
        Ok((target, record))
    }

    /// Set `band`'s frequency to `frequency` alone: store the record of
    /// [`Self::frequency_record`] with [`Self::tune`].
    ///
    /// # Errors
    ///
    /// Returns the [`Self::frequency_record`] errors.
    pub fn tune_frequency(
        &mut self,
        band: Band,
        frequency: Frequency,
    ) -> Result<VfoBand, VfoError> {
        let (_, record) = self.frequency_record(band, frequency)?;
        self.tune(band, record)
    }

    /// Store `record` as `band`'s current record and as the record of the
    /// VFO band of `band` whose limits contain its frequency, and select that
    /// VFO band: what an `FO` write of the whole record does. On band B the
    /// record's receive step also becomes its [`StepSegment`]'s stored step,
    /// as the radio stores a step selected in that segment.
    ///
    /// # Errors
    ///
    /// Returns [`VfoError::OutsideLimits`] when no VFO band of `band`
    /// admits the record's receive frequency.
    pub fn tune(&mut self, band: Band, record: StoredChannel) -> Result<VfoBand, VfoError> {
        let selected = self.vfo_band_for(band, record.receive_frequency)?;
        if band == Band::B {
            self.remember_step(&record);
        }
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
    /// when `band` is the selected VFO band. On band B the record's receive
    /// step also becomes its [`StepSegment`]'s stored step.
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
        if band.band() == Band::B {
            self.remember_step(&record);
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
        for (segment, bytes) in StepSegment::ALL.into_iter().zip(&self.segments) {
            put(segment.address(self.slot), bytes);
        }
        chain::changed_pages(image, start, &span)
    }

    /// Store `record`'s receive step as the step of its band B step segment.
    fn remember_step(&mut self, record: &StoredChannel) {
        let segment = StepSegment::of(record.receive_frequency);
        if let Some([step, _mode]) = self.segments.get_mut(usize::from(segment.0)) {
            *step = record.receive_step.as_raw();
        }
    }

    /// The VFO band of `band` whose limits contain `frequency`.
    fn vfo_band_for(&self, band: Band, frequency: Frequency) -> Result<VfoBand, VfoError> {
        self.bands
            .iter()
            .find(|memory| memory.band.band() == band && memory.limits.contains(frequency))
            .map(|memory| memory.band)
            .ok_or(VfoError::OutsideLimits {
                band,
                hz: frequency.as_hz(),
            })
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
        source: ValidationError,
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
    /// The slot's Auto Repeater Offset setting did not decode.
    #[error("the Auto Repeater Offset setting: {source}")]
    AutoOffset {
        /// The decoding error.
        source: SchemaError,
    },
}
