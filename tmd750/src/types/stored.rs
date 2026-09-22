//! Memory channels as the radio stores them: the 40-byte channel record, its
//! four-byte flag record and its 16-byte name.
//!
//! The three tables sit at fixed image addresses and are indexed by one
//! physical channel number. The layouts below were read back from firmware
//! 1.02 after channels were written over CAT with every tone mode, both
//! shift directions, split, reverse and lockout set, and from the radio's own
//! APRS channel; a field marked raw is retained exactly because no observed
//! record fixes its meaning.

use std::fmt;

use crate::error::ValidationError;
use crate::types::{
    CatChannelRecord, CatMemoryChannelRecord, CrossToneField, CurrentMemorySelector, DcsCode,
    DigitalCode, DigitalSquelch, Frequency, MemoryChannelAddress, OperatingMode, ShiftDirection,
    StepSize, ToneCode, ToneMode, TransmitSetting, UrCallsign,
};

/// Image address of the flag table: four bytes per physical channel.
pub const CHANNEL_FLAGS_OFFSET: usize = 0x2000;
/// Image address of the channel records: six 40-byte records per 256-byte page.
pub const CHANNEL_DATA_OFFSET: usize = 0x4000;
/// Image address of the name table: 16 bytes per physical channel.
pub const CHANNEL_NAMES_OFFSET: usize = 0x10000;
/// Bytes in one stored channel record.
pub const CHANNEL_RECORD_SIZE: usize = 40;
/// Channel records in one 256-byte page; the last 16 bytes are unused.
pub const CHANNELS_PER_PAGE: usize = 6;
/// Bytes in one flag record.
pub const FLAG_RECORD_SIZE: usize = 4;
/// Bytes in one channel name.
pub const CHANNEL_NAME_SIZE: usize = 16;
/// Physical channels: 1,000 regular, 100 program scan, priority and APRS.
pub const PHYSICAL_CHANNEL_COUNT: u16 = 1102;

/// Zero-based position of a channel in the three tables.
///
/// Regular channels occupy 0 through 999. Program scan limits follow in
/// pairs, `L0`, `U0`, `L1`, `U1` and so on (`L5` was stored at 1010 and `U5`
/// at 1011). The priority channel is 1100 and the APRS frequency channel
/// 1101; only the APRS channel was found stored on the bench radio.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PhysicalChannel(u16);

impl PhysicalChannel {
    /// First program scan slot, `L0`.
    pub const FIRST_PROGRAM_SCAN: u16 = 1000;
    /// The priority scan channel.
    pub const PRIORITY: Self = Self(1100);
    /// The APRS frequency channel.
    pub const APRS: Self = Self(1101);

    /// Validate an index.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::SettingOutOfRange`] at or above
    /// [`PHYSICAL_CHANNEL_COUNT`].
    pub const fn new(index: u16) -> Result<Self, ValidationError> {
        if index >= PHYSICAL_CHANNEL_COUNT {
            Err(ValidationError::SettingOutOfRange {
                setting: "physical channel",
                value: index as u64,
                max: (PHYSICAL_CHANNEL_COUNT - 1) as u64,
            })
        } else {
            Ok(Self(index))
        }
    }

    /// The slot of a CAT memory address.
    #[must_use]
    pub const fn from_address(address: MemoryChannelAddress) -> Self {
        match address {
            MemoryChannelAddress::Regular(channel) => Self(channel),
            MemoryChannelAddress::ProgramScanLower(pair) => {
                Self(Self::FIRST_PROGRAM_SCAN + 2 * pair as u16)
            }
            MemoryChannelAddress::ProgramScanUpper(pair) => {
                Self(Self::FIRST_PROGRAM_SCAN + 2 * pair as u16 + 1)
            }
            MemoryChannelAddress::Priority => Self::PRIORITY,
        }
    }

    /// The channel as `MR` reports it.
    #[must_use]
    pub fn selector(self) -> CurrentMemorySelector {
        match self.0 {
            index @ 0..=999 => CurrentMemorySelector::Address(MemoryChannelAddress::Regular(index)),
            1100 => CurrentMemorySelector::Address(MemoryChannelAddress::Priority),
            1101 => CurrentMemorySelector::Aprs,
            index => {
                let pair = u8::try_from((index - Self::FIRST_PROGRAM_SCAN) / 2).unwrap_or(u8::MAX);
                if (index - Self::FIRST_PROGRAM_SCAN) % 2 == 0 {
                    CurrentMemorySelector::Address(MemoryChannelAddress::ProgramScanLower(pair))
                } else {
                    CurrentMemorySelector::Address(MemoryChannelAddress::ProgramScanUpper(pair))
                }
            }
        }
    }

    /// The index.
    #[must_use]
    pub const fn index(self) -> u16 {
        self.0
    }

    /// Image address of the flag record.
    #[must_use]
    pub const fn flag_address(self) -> usize {
        CHANNEL_FLAGS_OFFSET + FLAG_RECORD_SIZE * self.0 as usize
    }

    /// Image address of the channel record.
    #[must_use]
    pub const fn data_address(self) -> usize {
        let index = self.0 as usize;
        CHANNEL_DATA_OFFSET
            + 256 * (index / CHANNELS_PER_PAGE)
            + CHANNEL_RECORD_SIZE * (index % CHANNELS_PER_PAGE)
    }

    /// Image address of the name.
    #[must_use]
    pub const fn name_address(self) -> usize {
        CHANNEL_NAMES_OFFSET + CHANNEL_NAME_SIZE * self.0 as usize
    }

    /// Every physical channel in index order.
    pub fn all() -> impl Iterator<Item = Self> {
        (0..PHYSICAL_CHANNEL_COUNT).map(Self)
    }
}

impl fmt::Display for PhysicalChannel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.selector().fmt(formatter)
    }
}

/// An eight-byte D-STAR callsign field of a stored record, NUL padded.
///
/// Empty records and the CAT writer store `DIRECT` in both repeater fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StoredCallsign([u8; 8]);

impl StoredCallsign {
    /// The stored `DIRECT` repeater value.
    pub const DIRECT: Self = Self(*b"DIRECT\0\0");

    /// Retain eight bytes exactly.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 8]) -> Self {
        Self(bytes)
    }

    /// Store text with NUL padding.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::InvalidCallsignText`] for more than eight
    /// bytes or a byte that is not graphic ASCII or space.
    pub fn new(text: &str) -> Result<Self, ValidationError> {
        if text.len() > 8
            || !text
                .bytes()
                .all(|byte| byte.is_ascii_graphic() || byte == b' ')
        {
            return Err(ValidationError::InvalidCallsignText {
                text: text.to_owned(),
            });
        }
        let mut bytes = [0; 8];
        bytes
            .iter_mut()
            .zip(text.bytes())
            .for_each(|(slot, byte)| *slot = byte);
        Ok(Self(bytes))
    }

    /// The eight stored bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 8] {
        &self.0
    }

    /// The text before the first NUL, lossily decoded.
    #[must_use]
    pub fn text(&self) -> String {
        let end = self.0.iter().position(|&byte| byte == 0).unwrap_or(8);
        String::from_utf8_lossy(self.0.get(..end).unwrap_or_default()).into_owned()
    }
}

impl fmt::Display for StoredCallsign {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.text())
    }
}

/// The two-bit route field of a stored record (byte 14, bits 3:2), retained
/// exactly: CAT-written records carried `3`, the radio's APRS channel `0`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RouteField(u8);

impl RouteField {
    /// Highest value.
    pub const MAX: u8 = 3;

    /// Validate a two-bit value.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::SettingOutOfRange`] above [`Self::MAX`].
    pub const fn new(value: u8) -> Result<Self, ValidationError> {
        if value > Self::MAX {
            Err(ValidationError::SettingOutOfRange {
                setting: "route field",
                value: value as u64,
                max: Self::MAX as u64,
            })
        } else {
            Ok(Self(value))
        }
    }

    /// The value.
    #[must_use]
    pub const fn as_raw(self) -> u8 {
        self.0
    }
}

/// One 40-byte channel record, decoded losslessly.
///
/// Byte layout, little-endian hertz for the two frequencies:
///
/// | Bytes | Field |
/// | --- | --- |
/// | 0..4 | receive frequency |
/// | 4..8 | offset, or the transmit frequency when split is set |
/// | 8 | receive step (high nibble), transmit step (low nibble) |
/// | 9 | mode (high nibble), bit 3 raw, bits 2:0 raw |
/// | 10 | tone mode nibble (8 tone, 4 CTCSS, 2 DCS, 1 cross tone), bit 3 reverse, bit 2 split, bits 1:0 shift |
/// | 11, 12, 13 | tone, CTCSS and DCS table indexes |
/// | 14 | cross tone (high nibble), route (bits 3:2), digital squelch (bits 1:0) |
/// | 15..23, 23..31, 31..39 | URCALL, RPT1, RPT2 |
/// | 39 | digital code |
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StoredChannel {
    /// Receive frequency.
    pub receive_frequency: Frequency,
    /// Offset or split transmit frequency.
    pub transmit: TransmitSetting,
    /// Receive tuning step.
    pub receive_step: StepSize,
    /// Transmit tuning step.
    pub transmit_step: StepSize,
    /// Demodulation mode.
    pub mode: OperatingMode,
    /// Byte 9 bit 3, raw: set on the DV and AM records the CAT writer stored,
    /// clear on FM records and on the radio's APRS channel.
    pub mode_flag: bool,
    /// Byte 9 bits 2:0, raw; zero in every observed record.
    pub mode_low_bits: u8,
    /// The single active tone function.
    pub tone_mode: ToneMode,
    /// Reverse.
    pub reverse: bool,
    /// Whether bytes 4..8 hold an independent transmit frequency.
    pub split: bool,
    /// Offset direction.
    pub shift: ShiftDirection,
    /// Transmit tone table index.
    pub tone_code: ToneCode,
    /// Receive CTCSS table index.
    pub ctcss_code: ToneCode,
    /// DCS table index.
    pub dcs_code: DcsCode,
    /// Cross tone field.
    pub cross_tone: CrossToneField,
    /// Route field, raw.
    pub route: RouteField,
    /// D-STAR destination callsign.
    pub ur_call: StoredCallsign,
    /// D-STAR first repeater.
    pub rpt1: StoredCallsign,
    /// D-STAR second repeater.
    pub rpt2: StoredCallsign,
    /// D-STAR digital squelch type.
    pub digital_squelch: DigitalSquelch,
    /// D-STAR digital code.
    pub digital_code: DigitalCode,
}

impl StoredChannel {
    /// Decode one record.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::StoredChannelLength`] for any other length,
    /// and the field type's error for an out-of-domain value.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, ValidationError> {
        let record: &[u8; CHANNEL_RECORD_SIZE] =
            bytes
                .try_into()
                .map_err(|_| ValidationError::StoredChannelLength {
                    actual: bytes.len(),
                })?;
        let word = |offset: usize| -> u32 {
            let mut value = [0; 4];
            value.copy_from_slice(record.get(offset..offset + 4).unwrap_or_default());
            u32::from_le_bytes(value)
        };
        let byte = |offset: usize| -> u8 { record.get(offset).copied().unwrap_or_default() };
        let callsign = |offset: usize| -> StoredCallsign {
            let mut value = [0; 8];
            value.copy_from_slice(record.get(offset..offset + 8).unwrap_or_default());
            StoredCallsign::from_bytes(value)
        };
        let split = byte(10) & 0x04 != 0;
        let transmit_hz = word(4);
        let transmit = if split {
            TransmitSetting::Split(Frequency::new(transmit_hz)?)
        } else {
            TransmitSetting::Offset(crate::types::OffsetFrequency::new(transmit_hz)?)
        };
        let tone_nibble = byte(10) >> 4;
        let tone_mode = ToneMode::from_flags([
            tone_nibble & 0x8 != 0,
            tone_nibble & 0x4 != 0,
            tone_nibble & 0x2 != 0,
            tone_nibble & 0x1 != 0,
        ])?;
        Ok(Self {
            receive_frequency: Frequency::new(word(0))?,
            transmit,
            receive_step: StepSize::try_from(byte(8) >> 4)?,
            transmit_step: StepSize::try_from(byte(8) & 0x0F)?,
            mode: OperatingMode::from(byte(9) >> 4),
            mode_flag: byte(9) & 0x08 != 0,
            mode_low_bits: byte(9) & 0x07,
            tone_mode,
            reverse: byte(10) & 0x08 != 0,
            split,
            shift: ShiftDirection::try_from(byte(10) & 0x03)?,
            tone_code: ToneCode::new(byte(11))?,
            ctcss_code: ToneCode::new(byte(12))?,
            dcs_code: DcsCode::new(byte(13))?,
            cross_tone: CrossToneField::new(byte(14) >> 4)?,
            route: RouteField::new((byte(14) >> 2) & 0x03)?,
            ur_call: callsign(15),
            rpt1: callsign(23),
            rpt2: callsign(31),
            digital_squelch: DigitalSquelch::try_from(byte(14) & 0x03)?,
            digital_code: DigitalCode::new(byte(39))?,
        })
    }

    /// Encode the record.
    #[must_use]
    pub fn to_bytes(&self) -> [u8; CHANNEL_RECORD_SIZE] {
        let mut bytes = [0; CHANNEL_RECORD_SIZE];
        let transmit_hz = match self.transmit {
            TransmitSetting::Offset(offset) => offset.as_hz(),
            TransmitSetting::Split(frequency) => frequency.as_hz(),
        };
        let [tone, ctcss, dcs, cross] = self.tone_mode.to_flags();
        let tone_nibble =
            u8::from(tone) << 3 | u8::from(ctcss) << 2 | u8::from(dcs) << 1 | u8::from(cross);
        let fields: [(usize, &[u8]); 5] = [
            (0, &self.receive_frequency.as_hz().to_le_bytes()),
            (4, &transmit_hz.to_le_bytes()),
            (15, self.ur_call.as_bytes()),
            (23, self.rpt1.as_bytes()),
            (31, self.rpt2.as_bytes()),
        ];
        for (offset, value) in fields {
            if let Some(target) = bytes.get_mut(offset..offset + value.len()) {
                target.copy_from_slice(value);
            }
        }
        let scalars = [
            (
                8,
                self.receive_step.as_raw() << 4 | self.transmit_step.as_raw(),
            ),
            (
                9,
                u8::from(self.mode) << 4
                    | u8::from(self.mode_flag) << 3
                    | (self.mode_low_bits & 0x07),
            ),
            (
                10,
                tone_nibble << 4
                    | u8::from(self.reverse) << 3
                    | u8::from(self.split) << 2
                    | self.shift.as_raw(),
            ),
            (11, self.tone_code.as_raw()),
            (12, self.ctcss_code.as_raw()),
            (13, self.dcs_code.as_raw()),
            (
                14,
                self.cross_tone.as_raw() << 4
                    | self.route.as_raw() << 2
                    | self.digital_squelch.as_raw(),
            ),
            (39, self.digital_code.as_raw()),
        ];
        for (offset, value) in scalars {
            if let Some(target) = bytes.get_mut(offset) {
                *target = value;
            }
        }
        bytes
    }

    /// The fields `FO` and `ME` carry, without the repeater callsigns and
    /// the route field.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::InvalidCallsignText`] when the stored URCALL
    /// holds a byte the CAT field cannot carry.
    pub fn cat_record(&self) -> Result<CatChannelRecord, ValidationError> {
        Ok(CatChannelRecord {
            receive_frequency: self.receive_frequency,
            transmit: self.transmit,
            receive_step: self.receive_step,
            transmit_step: self.transmit_step,
            mode: self.mode,
            tone_mode: self.tone_mode,
            reverse: self.reverse,
            shift: self.shift,
            tone_code: self.tone_code,
            ctcss_code: self.ctcss_code,
            dcs_code: self.dcs_code,
            cross_tone: self.cross_tone,
            ur_call: UrCallsign::new(&self.ur_call.text())?,
            digital_squelch: self.digital_squelch,
            digital_code: self.digital_code,
        })
    }

    /// The stored record a CAT memory record produces when the radio stores
    /// it: `DIRECT` repeaters, route `3`, and the mode flag set for every mode
    /// except FM, as the CAT writer stored them on firmware 1.02.
    ///
    /// # Errors
    ///
    /// Returns the callsign error of [`StoredCallsign::new`].
    pub fn from_cat_record(record: &CatMemoryChannelRecord) -> Result<Self, ValidationError> {
        let channel = &record.channel;
        Ok(Self {
            receive_frequency: channel.receive_frequency,
            transmit: channel.transmit,
            receive_step: channel.receive_step,
            transmit_step: channel.transmit_step,
            mode: channel.mode,
            mode_flag: channel.mode != OperatingMode::Fm,
            mode_low_bits: 0,
            tone_mode: channel.tone_mode,
            reverse: channel.reverse,
            split: record.split,
            shift: channel.shift,
            tone_code: channel.tone_code,
            ctcss_code: channel.ctcss_code,
            dcs_code: channel.dcs_code,
            cross_tone: channel.cross_tone,
            route: RouteField(3),
            ur_call: StoredCallsign::new(channel.ur_call.as_str())?,
            rpt1: StoredCallsign::DIRECT,
            rpt2: StoredCallsign::DIRECT,
            digital_squelch: channel.digital_squelch,
            digital_code: channel.digital_code,
        })
    }
}

/// The band code in flag byte zero of a stored channel.
///
/// `0xFF` marks an empty slot. Observed stored values: `0x01` on the radio's
/// own 144.390 MHz channel, `0x05` on CAT-written 2 m channels, `0x04` at
/// 121.500 MHz, `0x02` at 223.500 MHz and `0x08` at 446.000 MHz. Their
/// bit meanings are not established, so the byte is retained exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BandCode(u8);

impl BandCode {
    /// The empty-slot marker.
    pub const EMPTY: u8 = 0xFF;

    /// Retain a stored value other than the empty marker.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::SettingOutOfRange`] for `0xFF`.
    pub const fn new(value: u8) -> Result<Self, ValidationError> {
        if value == Self::EMPTY {
            Err(ValidationError::SettingOutOfRange {
                setting: "band code",
                value: value as u64,
                max: (Self::EMPTY - 1) as u64,
            })
        } else {
            Ok(Self(value))
        }
    }

    /// The byte.
    #[must_use]
    pub const fn as_raw(self) -> u8 {
        self.0
    }
}

/// The four-byte flag record of a physical channel.
///
/// Byte 0 is the band code or `0xFF` for an empty slot, byte 1 the scan
/// lockout, byte 2 the group and byte 3 is raw (`0xFF` on empty slots and
/// the radio's APRS channel, `0x00` after a CAT write). Groups 0 through 9
/// follow the hundreds digit of a regular channel; program scan slots read
/// 30 while empty and 10 after a CAT write; the priority channel reads 31
/// and the APRS channel 35.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StoredChannelFlag {
    band: Option<BandCode>,
    scan_lockout: bool,
    group: u8,
    trailing: u8,
}

impl StoredChannelFlag {
    /// Decode one flag record.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::StoredChannelLength`] for any other length
    /// and [`ValidationError::SettingOutOfRange`] for a lockout byte other
    /// than `0` or `1`.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, ValidationError> {
        let record: &[u8; FLAG_RECORD_SIZE] =
            bytes
                .try_into()
                .map_err(|_| ValidationError::StoredChannelLength {
                    actual: bytes.len(),
                })?;
        let [band, lockout, group, trailing] = *record;
        let scan_lockout = match lockout {
            0 => false,
            1 => true,
            other => {
                return Err(ValidationError::SettingOutOfRange {
                    setting: "scan lockout flag",
                    value: u64::from(other),
                    max: 1,
                });
            }
        };
        Ok(Self {
            band: BandCode::new(band).ok(),
            scan_lockout,
            group,
            trailing,
        })
    }

    /// Encode the record.
    #[must_use]
    pub const fn to_bytes(self) -> [u8; FLAG_RECORD_SIZE] {
        let band = match self.band {
            Some(code) => code.as_raw(),
            None => BandCode::EMPTY,
        };
        [band, self.scan_lockout as u8, self.group, self.trailing]
    }

    /// Whether the slot holds a channel.
    #[must_use]
    pub const fn is_programmed(self) -> bool {
        self.band.is_some()
    }

    /// The band code of a stored channel.
    #[must_use]
    pub const fn band(self) -> Option<BandCode> {
        self.band
    }

    /// Whether memory scan skips the channel.
    #[must_use]
    pub const fn scan_lockout(self) -> bool {
        self.scan_lockout
    }

    /// The group byte.
    #[must_use]
    pub const fn group(self) -> u8 {
        self.group
    }

    /// Byte 3, raw.
    #[must_use]
    pub const fn trailing(self) -> u8 {
        self.trailing
    }
}

/// A 16-byte channel name, NUL padded.
///
/// The radio's APRS channel stores `APRS Channel`; a channel written over
/// CAT has an all-zero name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ChannelName([u8; CHANNEL_NAME_SIZE]);

impl ChannelName {
    /// Retain the bytes exactly.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; CHANNEL_NAME_SIZE]) -> Self {
        Self(bytes)
    }

    /// The bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; CHANNEL_NAME_SIZE] {
        &self.0
    }

    /// The text before the first NUL, lossily decoded.
    #[must_use]
    pub fn text(&self) -> String {
        let end = self
            .0
            .iter()
            .position(|&byte| byte == 0)
            .unwrap_or(CHANNEL_NAME_SIZE);
        String::from_utf8_lossy(self.0.get(..end).unwrap_or_default()).into_owned()
    }
}

impl fmt::Display for ChannelName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.text())
    }
}

/// One physical channel with its three stored records.
///
/// An empty slot keeps its record bytes undecoded: the radio stores
/// `0xFF` frequencies with `CQCQCQ` and `DIRECT` defaults there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredChannelEntry {
    index: PhysicalChannel,
    flag: StoredChannelFlag,
    name: ChannelName,
    channel: Option<StoredChannel>,
    raw_record: [u8; CHANNEL_RECORD_SIZE],
}

impl StoredChannelEntry {
    /// Decode the flag, record and name of one slot.
    ///
    /// The record is decoded only when the flag marks the slot programmed.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`StoredChannelFlag::from_bytes`] and, for a
    /// programmed slot, [`StoredChannel::from_bytes`].
    pub fn from_records(
        index: PhysicalChannel,
        flag: &[u8],
        record: &[u8],
        name: &[u8],
    ) -> Result<Self, ValidationError> {
        let flag = StoredChannelFlag::from_bytes(flag)?;
        let raw_record: [u8; CHANNEL_RECORD_SIZE] =
            record
                .try_into()
                .map_err(|_| ValidationError::StoredChannelLength {
                    actual: record.len(),
                })?;
        let name: [u8; CHANNEL_NAME_SIZE] = name
            .try_into()
            .map_err(|_| ValidationError::StoredChannelLength { actual: name.len() })?;
        let channel = if flag.is_programmed() {
            Some(StoredChannel::from_bytes(&raw_record)?)
        } else {
            None
        };
        Ok(Self {
            index,
            flag,
            name: ChannelName::from_bytes(name),
            channel,
            raw_record,
        })
    }

    /// The slot.
    #[must_use]
    pub const fn index(&self) -> PhysicalChannel {
        self.index
    }

    /// The flag record.
    #[must_use]
    pub const fn flag(&self) -> StoredChannelFlag {
        self.flag
    }

    /// The name.
    #[must_use]
    pub const fn name(&self) -> ChannelName {
        self.name
    }

    /// The decoded record of a programmed slot.
    #[must_use]
    pub const fn channel(&self) -> Option<&StoredChannel> {
        self.channel.as_ref()
    }

    /// The 40 record bytes, decoded or not.
    #[must_use]
    pub const fn raw_record(&self) -> &[u8; CHANNEL_RECORD_SIZE] {
        &self.raw_record
    }
}

impl fmt::Display for StoredChannelEntry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.channel {
            Some(channel) => {
                let name = self.name.text();
                write!(
                    formatter,
                    "{}: {} {}",
                    self.index, channel.receive_frequency, channel.mode
                )?;
                if !name.is_empty() {
                    write!(formatter, " \"{name}\"")?;
                }
                if self.flag.scan_lockout {
                    formatter.write_str(" lockout")?;
                }
                Ok(())
            }
            None => write!(formatter, "{}: empty", self.index),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// Records read back from firmware 1.02 after CAT writes (`ME`), with the
    /// CAT record that produced each one.
    const OBSERVED: [(&str, [u8; 40], [u8; 4]); 5] = [
        (
            "995,0121500000,0000000000,6,6,2,0,0,0,1,0,0,0,00,00,000,5,CQCQCQ,0,00,0",
            [
                0x60, 0xF1, 0x3D, 0x07, 0x00, 0x00, 0x00, 0x00, 0x66, 0x28, 0x10, 0x00, 0x00, 0x00,
                0x5C, 0x43, 0x51, 0x43, 0x51, 0x43, 0x51, 0x00, 0x00, 0x44, 0x49, 0x52, 0x45, 0x43,
                0x54, 0x00, 0x00, 0x44, 0x49, 0x52, 0x45, 0x43, 0x54, 0x00, 0x00, 0x00,
            ],
            [0x04, 0x00, 0x09, 0x00],
        ),
        (
            "996,0223500000,0001600000,8,8,1,0,0,1,0,0,0,2,20,20,050,0,W1AW/XYZ,2,07,1",
            [
                0xE0, 0x56, 0x52, 0x0D, 0x00, 0x6A, 0x18, 0x00, 0x88, 0x18, 0x22, 0x14, 0x14, 0x32,
                0x0E, 0x57, 0x31, 0x41, 0x57, 0x2F, 0x58, 0x59, 0x5A, 0x44, 0x49, 0x52, 0x45, 0x43,
                0x54, 0x00, 0x00, 0x44, 0x49, 0x52, 0x45, 0x43, 0x54, 0x00, 0x00, 0x07,
            ],
            [0x02, 0x01, 0x09, 0x00],
        ),
        (
            "997,0446000000,0005000000,9,9,0,0,1,0,0,1,0,1,12,15,010,3,W1AW,1,42,0",
            [
                0x80, 0x6B, 0x95, 0x1A, 0x40, 0x4B, 0x4C, 0x00, 0x99, 0x00, 0x49, 0x0C, 0x0F, 0x0A,
                0x3D, 0x57, 0x31, 0x41, 0x57, 0x00, 0x00, 0x00, 0x00, 0x44, 0x49, 0x52, 0x45, 0x43,
                0x54, 0x00, 0x00, 0x44, 0x49, 0x52, 0x45, 0x43, 0x54, 0x00, 0x00, 0x2A,
            ],
            [0x08, 0x00, 0x09, 0x00],
        ),
        (
            "998,0146520000,0146540000,2,2,0,1,0,0,0,0,1,0,08,08,000,0,CQCQCQ,0,00,1",
            [
                0xC0, 0xB7, 0xBB, 0x08, 0xE0, 0x05, 0xBC, 0x08, 0x22, 0x00, 0x84, 0x08, 0x08, 0x00,
                0x0C, 0x43, 0x51, 0x43, 0x51, 0x43, 0x51, 0x00, 0x00, 0x44, 0x49, 0x52, 0x45, 0x43,
                0x54, 0x00, 0x00, 0x44, 0x49, 0x52, 0x45, 0x43, 0x54, 0x00, 0x00, 0x00,
            ],
            [0x05, 0x01, 0x09, 0x00],
        ),
        (
            "999,0146520000,0000000000,2,2,0,0,0,0,0,0,0,0,08,08,000,0,CQCQCQ,0,00,0",
            [
                0xC0, 0xB7, 0xBB, 0x08, 0x00, 0x00, 0x00, 0x00, 0x22, 0x00, 0x00, 0x08, 0x08, 0x00,
                0x0C, 0x43, 0x51, 0x43, 0x51, 0x43, 0x51, 0x00, 0x00, 0x44, 0x49, 0x52, 0x45, 0x43,
                0x54, 0x00, 0x00, 0x44, 0x49, 0x52, 0x45, 0x43, 0x54, 0x00, 0x00, 0x00,
            ],
            [0x05, 0x00, 0x09, 0x00],
        ),
    ];

    /// The radio's own APRS channel at physical index 1101.
    const APRS_RECORD: [u8; 40] = [
        0x70, 0x37, 0x9B, 0x08, 0x00, 0x00, 0x00, 0x00, 0x60, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];
    const APRS_FLAG: [u8; 4] = [0x01, 0x00, 0x23, 0xFF];
    const APRS_NAME: [u8; 16] = *b"APRS Channel\0\0\0\0";

    fn cat_record(payload: &str) -> Result<CatMemoryChannelRecord, Box<dyn std::error::Error>> {
        let (_, record) = crate::protocol::channel::parse_memory_channel(payload)?;
        Ok(record)
    }

    #[test]
    fn observed_records_round_trip_and_match_their_cat_records() -> TestResult {
        for (payload, bytes, flag_bytes) in OBSERVED {
            let stored = StoredChannel::from_bytes(&bytes)?;
            assert_eq!(stored.to_bytes(), bytes, "{payload}");
            let cat = cat_record(payload)?;
            assert_eq!(StoredChannel::from_cat_record(&cat)?, stored, "{payload}");
            assert_eq!(stored.cat_record()?, cat.channel, "{payload}");
            assert_eq!(stored.split, cat.split, "{payload}");
            let flag = StoredChannelFlag::from_bytes(&flag_bytes)?;
            assert_eq!(flag.to_bytes(), flag_bytes);
            assert!(flag.is_programmed());
            assert_eq!(flag.scan_lockout(), cat.scan_lockout, "{payload}");
            assert_eq!(flag.group(), 9);
        }
        Ok(())
    }

    #[test]
    fn the_aprs_channel_decodes_as_the_radio_reports_it() -> TestResult {
        let entry = StoredChannelEntry::from_records(
            PhysicalChannel::APRS,
            &APRS_FLAG,
            &APRS_RECORD,
            &APRS_NAME,
        )?;
        let channel = entry.channel().ok_or("APRS channel is programmed")?;
        assert_eq!(channel.receive_frequency.as_hz(), 144_390_000);
        assert_eq!(channel.receive_step, StepSize::Hz12500);
        assert_eq!(channel.transmit_step, StepSize::Hz1000);
        assert_eq!(channel.mode, OperatingMode::Fm);
        assert_eq!(channel.tone_mode, ToneMode::Off);
        assert_eq!(channel.ur_call.text(), "");
        assert_eq!(entry.name().text(), "APRS Channel");
        assert_eq!(entry.flag().group(), 35);
        assert_eq!(entry.flag().trailing(), 0xFF);
        assert_eq!(
            entry.to_string(),
            "APRS: 144.390000 MHz FM \"APRS Channel\""
        );
        assert_eq!(
            channel.cat_record()?.to_wire_fields(),
            "0144390000,0000000000,6,0,0,0,0,0,0,0,0,00,00,000,0,,0,00"
        );
        Ok(())
    }

    #[test]
    fn empty_slots_keep_their_bytes_undecoded() -> TestResult {
        let mut record = [0xFF; 40];
        record[15..].copy_from_slice(b"CQCQCQ\0\0DIRECT\0\0DIRECT\0\0\0");
        let entry = StoredChannelEntry::from_records(
            PhysicalChannel::new(0)?,
            &[0xFF, 0x00, 0x00, 0xFF],
            &record,
            &[0; 16],
        )?;
        assert!(entry.channel().is_none());
        assert_eq!(entry.raw_record(), &record);
        assert!(!entry.flag().is_programmed());
        assert_eq!(entry.to_string(), "000: empty");
        assert!(
            StoredChannel::from_bytes(&record).is_err(),
            "0xFF frequency is out of range"
        );
        Ok(())
    }

    #[test]
    fn physical_indexes_follow_the_observed_slot_order() -> TestResult {
        assert_eq!(
            PhysicalChannel::from_address(MemoryChannelAddress::program_scan_lower(5)?).index(),
            1010
        );
        assert_eq!(
            PhysicalChannel::from_address(MemoryChannelAddress::program_scan_upper(5)?).index(),
            1011
        );
        assert_eq!(
            PhysicalChannel::from_address(MemoryChannelAddress::Priority).index(),
            1100
        );
        assert_eq!(PhysicalChannel::APRS.index(), 1101);
        assert!(PhysicalChannel::new(1102).is_err());
        assert_eq!(PhysicalChannel::new(999)?.data_address(), 0xE678);
        assert_eq!(PhysicalChannel::new(998)?.flag_address(), 0x2F98);
        assert_eq!(PhysicalChannel::APRS.name_address(), 0x144D0);
        assert_eq!(PhysicalChannel::APRS.data_address(), 0xF778);
        for index in PhysicalChannel::all() {
            match index.selector() {
                CurrentMemorySelector::Address(address) => {
                    assert_eq!(PhysicalChannel::from_address(address), index);
                }
                CurrentMemorySelector::Aprs => assert_eq!(index, PhysicalChannel::APRS),
            }
        }
        assert_eq!(PhysicalChannel::new(1011)?.to_string(), "U05");
        assert_eq!(PhysicalChannel::APRS.to_string(), "APRS");
        Ok(())
    }

    #[test]
    fn stored_callsigns_pad_with_nul_and_bound_their_length() -> TestResult {
        assert_eq!(StoredCallsign::new("W1AW")?.as_bytes(), b"W1AW\0\0\0\0");
        assert_eq!(StoredCallsign::DIRECT.text(), "DIRECT");
        assert!(StoredCallsign::new("W1AW/XYZ9").is_err());
        assert!(StoredCallsign::new("W1\rAW").is_err());
        assert!(RouteField::new(4).is_err());
        assert!(BandCode::new(0xFF).is_err());
        let bad_lockout = StoredChannelFlag::from_bytes(&[0x05, 0x02, 0x09, 0x00]);
        assert!(bad_lockout.is_err());
        assert!(StoredChannelFlag::from_bytes(&[0x05, 0x00, 0x09]).is_err());
        Ok(())
    }
}
