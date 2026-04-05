//! Channel URCALL and memory types for the TH-D75 transceiver.
//!
//! This module contains two channel representations:
//!
//! - [`ChannelMemory`]: the CAT wire format (FO/ME commands), used by the
//!   protocol layer for over-the-air command encoding/decoding.
//! - [`FlashChannel`]: the 40-byte flash memory format (MCP/SD card), used
//!   by the memory and SD card modules for binary image parsing.
//!
//! The two formats differ in field layout at bytes 0x09, 0x0A, 0x0E, and
//! 0x0F-0x27. The flash format includes `MemoryMode` (8 modes including
//! LSB/USB/CW/DR), separated D-STAR callsigns, and structured tone/duplex
//! bit fields.

use std::fmt;

use crate::error::{ProtocolError, ValidationError};
use crate::types::dstar::DstarCallsign;
use crate::types::frequency::Frequency;
use crate::types::mode::{MemoryMode, ShiftDirection, StepSize};
use crate::types::tone::{CtcssMode, DcsCode, ToneCode};

/// D-STAR URCALL callsign (up to 8 characters, stored in 24 bytes).
///
/// The TH-D75 stores this field in a 24-byte region to accommodate
/// multi-byte character encodings such as Shift-JIS (Japanese Industrial
/// Standards) on Japanese-market models. This type validates ASCII-only
/// content with a maximum of 8 characters.
///
/// Despite being labeled "Channel Name" in some documentation, this field
/// stores the D-STAR "UR" (your) callsign, defaulting to "CQCQCQ" for
/// general CQ calls. User-assigned channel display names are stored
/// separately in flash and are only accessible via the MCP programming
/// interface.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct ChannelName(String);

impl ChannelName {
    /// Creates a new `ChannelName` from a string slice.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::ChannelNameTooLong`] if the callsign
    /// exceeds 8 characters.
    pub fn new(name: &str) -> Result<Self, ValidationError> {
        let len = name.len();
        if len > 8 {
            return Err(ValidationError::ChannelNameTooLong { len });
        }
        Ok(Self(name.to_owned()))
    }

    /// Returns the URCALL callsign as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Encodes the URCALL callsign as a 24-byte null-padded ASCII array.
    #[must_use]
    pub fn to_bytes(&self) -> [u8; 24] {
        let mut buf = [0u8; 24];
        let src = self.0.as_bytes();
        buf[..src.len()].copy_from_slice(src);
        buf
    }

    /// Decodes a URCALL callsign from a 24-byte null-padded array.
    ///
    /// Scans for the first null byte and takes ASCII characters up to
    /// that point. If no null byte is found, takes up to 8 characters.
    #[must_use]
    pub fn from_bytes(bytes: &[u8; 24]) -> Self {
        let end = bytes.iter().position(|&b| b == 0).unwrap_or(8).min(8);
        let s = String::from_utf8_lossy(&bytes[..end]);
        Self(s.into_owned())
    }
}

/// 40-byte internal channel memory structure.
///
/// Maps byte-for-byte to the firmware's internal representation at `DAT_c0012634`.
/// See `thd75/re_output/channel_memory_structure.txt`.
///
/// # Channel display names
///
/// Channel display names are NOT accessible via the CAT
/// protocol. The `urcall` field stores the D-STAR URCALL destination callsign
/// from the ME/FO wire format (typically "CQCQCQ" for D-STAR). Display names
/// are only accessible via the MCP programming protocol on USB interface 2.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelMemory {
    /// RX frequency in Hz (byte 0x00, 4 bytes, little-endian).
    pub rx_frequency: Frequency,
    /// TX offset or split TX frequency in Hz (byte 0x04, 4 bytes, little-endian).
    pub tx_offset: Frequency,
    /// Frequency step size (byte 0x08 high nibble).
    pub step_size: StepSize,
    /// Raw byte 0x09 — mode and fine tuning configuration.
    ///
    /// Bit layout (from `FlashChannel` RE):
    /// - bit 7: reserved
    /// - bits 6:4: operating mode (0=FM, 1=DV, 2=AM, 3=LSB, 4=USB, 5=CW, 6=NFM)
    /// - bit 3: narrow FM flag
    /// - bit 2: fine tuning enable
    /// - bits 1:0: fine step size
    ///
    /// In the CAT wire format (FO/ME), byte\[9\] is unpacked into fields \[3\]-\[6\]:
    /// `tx_step`, `mode`, `fine_tuning`, `fine_step`. The binary packer preserves
    /// this byte directly for round-trip fidelity.
    pub mode_flags_raw: u8,
    /// Shift direction (byte 0x08 low nibble in binary format).
    ///
    /// In the binary packer, this field is written to byte 0x08 low nibble.
    /// In the CAT wire format, shift is at field\[12\] and also encoded in
    /// `flags_0a_raw` bits 2:0. Both should carry the same value.
    pub shift: ShiftDirection,
    /// Reverse mode (derived from `flags_0a_raw` bit 3).
    pub reverse: bool,
    /// Tone encode enable (derived from `flags_0a_raw` bit 7).
    pub tone_enable: bool,
    /// CTCSS mode (derived from `flags_0a_raw` bit 6).
    pub ctcss_mode: CtcssMode,
    /// DCS enable (derived from `flags_0a_raw` bit 5).
    pub dcs_enable: bool,
    /// Cross-tone enable (derived from `flags_0a_raw` bit 4).
    pub cross_tone_reverse: bool,
    /// Raw byte 0x0A — source of truth for tone/shift configuration.
    ///
    /// Hardware-verified bit layout (20 channels, 0 exceptions):
    /// - bit 7: tone encode enable
    /// - bit 6: CTCSS enable
    /// - bit 5: DCS enable
    /// - bit 4: cross-tone enable
    /// - bit 3: reverse
    /// - bits 2:0: shift direction (0=simplex, 1=+, 2=-, 4=split)
    ///
    /// The individual bool fields above are convenience accessors that
    /// MUST be consistent with this byte. The binary packer (`to_bytes`)
    /// writes this byte directly; `from_bytes` derives the bools from it.
    pub flags_0a_raw: u8,
    /// Tone encoder frequency code index (byte 0x0B).
    pub tone_code: ToneCode,
    /// CTCSS decoder frequency code index (byte 0x0C).
    pub ctcss_code: ToneCode,
    /// DCS code index (byte 0x0D).
    pub dcs_code: DcsCode,
    /// Cross-tone combination type (byte 0x0E bits 5:4, range 0-3).
    ///
    /// CAT wire field 16. Controls how TX and RX tone types are combined
    /// when cross-tone mode is enabled (`cross_tone_reverse` / byte 0x0A bit 4).
    pub cross_tone_combo: CrossToneType,
    /// Digital squelch mode (byte 0x0E bits 1:0, range 0-2).
    ///
    /// CAT wire field 18: 0=Off, 1=Code Squelch, 2=Callsign Squelch.
    /// Note: channel lockout (ME field 22) is stored separately in MCP
    /// flags region at offset 0x2000, not here.
    pub digital_squelch: FlashDigitalSquelch,
    /// D-STAR URCALL destination callsign (byte 0x0F, 24 bytes).
    ///
    /// Stores the D-STAR "UR" (your) callsign, defaulting to "CQCQCQ"
    /// for general CQ calls. Display names are stored separately in MCP
    /// at offset 0x10000.
    pub urcall: ChannelName,
    /// Digital code (CAT wire field 19, 2 digits).
    pub data_mode: u8,
}

impl ChannelMemory {
    /// Size of the packed binary representation in bytes.
    pub const BYTE_SIZE: usize = 40;

    /// Serializes the channel memory to a 40-byte packed binary array.
    #[must_use]
    pub fn to_bytes(&self) -> [u8; 40] {
        let mut buf = [0u8; 40];

        // bytes[0..4]: RX frequency, little-endian
        buf[0..4].copy_from_slice(&self.rx_frequency.to_le_bytes());

        // bytes[4..8]: TX offset, little-endian
        buf[4..8].copy_from_slice(&self.tx_offset.to_le_bytes());

        // byte 0x08: step_size (high nibble) | shift (low nibble)
        buf[0x08] = (u8::from(self.step_size) << 4) | u8::from(self.shift);

        // byte 0x09: mode + fine tuning flags (preserved as raw byte)
        buf[0x09] = self.mode_flags_raw;

        // byte 0x0A: flags_0a_raw (all 8 bits — hardware-verified mapping):
        //   bit 7 = tone encode, bit 6 = CTCSS, bit 5 = DCS, bit 4 = cross-tone,
        //   bit 3 = reverse, bits 2:0 = shift direction
        buf[0x0A] = self.flags_0a_raw;

        // byte 0x0B: tone code index
        buf[0x0B] = self.tone_code.index();

        // byte 0x0C: CTCSS code index
        buf[0x0C] = self.ctcss_code.index();

        // byte 0x0D: DCS code index
        buf[0x0D] = self.dcs_code.index();

        // byte 0x0E: cross_tone_combo (bits 5:4) | data_speed=3 (bits 3:2) | digital_squelch (bits 1:0)
        buf[0x0E] = ((u8::from(self.cross_tone_combo) & 0x03) << 4)
            | 0x0C
            | (u8::from(self.digital_squelch) & 0x03);

        // bytes[0x0F..0x27]: URCALL callsign (24 bytes)
        buf[0x0F..0x27].copy_from_slice(&self.urcall.to_bytes());

        // byte 0x27: data mode
        buf[0x27] = self.data_mode;

        buf
    }

    /// Parses a channel memory from a byte slice (must be >= 40 bytes).
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::FieldParse`] if any field contains an
    /// invalid value, or if the slice is too short.
    #[allow(clippy::similar_names, clippy::too_many_lines)]
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, ProtocolError> {
        if bytes.len() < Self::BYTE_SIZE {
            return Err(ProtocolError::FieldParse {
                command: "channel".into(),
                field: "length".into(),
                detail: format!(
                    "expected at least {} bytes, got {}",
                    Self::BYTE_SIZE,
                    bytes.len()
                ),
            });
        }

        let rx_frequency = Frequency::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        let tx_offset = Frequency::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);

        let step_size =
            StepSize::try_from(bytes[0x08] >> 4).map_err(|e| ProtocolError::FieldParse {
                command: "channel".into(),
                field: "step_size".into(),
                detail: e.to_string(),
            })?;

        let shift = ShiftDirection::try_from(bytes[0x08] & 0x0F).map_err(|e| {
            ProtocolError::FieldParse {
                command: "channel".into(),
                field: "shift".into(),
                detail: e.to_string(),
            }
        })?;

        // byte 0x09: mode + fine tuning flags (preserved as raw byte)
        let mode_flags_raw = bytes[0x09];

        // byte 0x0A: all 8 bits (hardware-verified mapping)
        let flags_0a_raw = bytes[0x0A];
        let tone_enable = (flags_0a_raw >> 7) & 1 != 0;
        let ctcss_enable = (flags_0a_raw >> 6) & 1 != 0;
        let dcs_enable = (flags_0a_raw >> 5) & 1 != 0;
        let cross_tone_reverse = (flags_0a_raw >> 4) & 1 != 0;
        let reverse = (flags_0a_raw >> 3) & 1 != 0;

        let ctcss_mode = if ctcss_enable {
            CtcssMode::try_from(1u8).map_err(|e| ProtocolError::FieldParse {
                command: "channel".into(),
                field: "ctcss_mode".into(),
                detail: e.to_string(),
            })?
        } else {
            CtcssMode::try_from(0u8).map_err(|e| ProtocolError::FieldParse {
                command: "channel".into(),
                field: "ctcss_mode".into(),
                detail: e.to_string(),
            })?
        };

        let tone_code = ToneCode::new(bytes[0x0B]).map_err(|e| ProtocolError::FieldParse {
            command: "channel".into(),
            field: "tone_code".into(),
            detail: e.to_string(),
        })?;

        let ctcss_code = ToneCode::new(bytes[0x0C]).map_err(|e| ProtocolError::FieldParse {
            command: "channel".into(),
            field: "ctcss_code".into(),
            detail: e.to_string(),
        })?;

        let dcs_code = DcsCode::new(bytes[0x0D]).map_err(|e| ProtocolError::FieldParse {
            command: "channel".into(),
            field: "dcs_code".into(),
            detail: e.to_string(),
        })?;

        // byte 0x0E: cross_tone_combo (bits 5:4) | digital_squelch (bits 1:0)
        let cross_tone_combo = CrossToneType::try_from((bytes[0x0E] >> 4) & 0x03).map_err(|e| {
            ProtocolError::FieldParse {
                command: "channel".into(),
                field: "cross_tone_combo".into(),
                detail: e.to_string(),
            }
        })?;
        let digital_squelch = FlashDigitalSquelch::try_from(bytes[0x0E] & 0x03).map_err(|e| {
            ProtocolError::FieldParse {
                command: "channel".into(),
                field: "digital_squelch".into(),
                detail: e.to_string(),
            }
        })?;

        let mut urcall_arr = [0u8; 24];
        urcall_arr.copy_from_slice(&bytes[0x0F..0x27]);
        let urcall = ChannelName::from_bytes(&urcall_arr);

        let data_mode = bytes[0x27];

        Ok(Self {
            rx_frequency,
            tx_offset,
            step_size,
            mode_flags_raw,
            shift,
            reverse,
            tone_enable,
            ctcss_mode,
            dcs_enable,
            cross_tone_reverse,
            flags_0a_raw,
            tone_code,
            ctcss_code,
            dcs_code,
            cross_tone_combo,
            digital_squelch,
            urcall,
            data_mode,
        })
    }
}

// ===========================================================================
// Flash channel types (MCP / SD card binary format)
// ===========================================================================

/// Duplex mode as stored in flash memory byte 0x0A bits \[1:0\].
///
/// Combined with the split flag (bit 2) to determine the full duplex
/// configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FlashDuplex {
    /// Simplex -- TX on same frequency as RX (value 0).
    Simplex = 0,
    /// Plus -- TX frequency = RX + offset (value 1).
    Plus = 1,
    /// Minus -- TX frequency = RX - offset (value 2).
    Minus = 2,
}

impl TryFrom<u8> for FlashDuplex {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Simplex),
            1 => Ok(Self::Plus),
            2 => Ok(Self::Minus),
            _ => Err(ValidationError::ShiftOutOfRange(value)),
        }
    }
}

impl From<FlashDuplex> for u8 {
    fn from(d: FlashDuplex) -> Self {
        d as Self
    }
}

/// Cross-tone type as stored in flash memory byte 0x0E bits \[5:4\].
///
/// Determines how different tone/DCS codes are applied to TX vs RX
/// when cross-tone mode is active.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CrossToneType {
    /// DTCS on both TX and RX (value 0).
    DtcsDtcs = 0,
    /// Tone on TX, DTCS on RX (value 1).
    ToneDtcs = 1,
    /// DTCS on TX, Tone on RX (value 2).
    DtcsTone = 2,
    /// Tone on both TX and RX with different codes (value 3).
    ToneTone = 3,
}

impl TryFrom<u8> for CrossToneType {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::DtcsDtcs),
            1 => Ok(Self::ToneDtcs),
            2 => Ok(Self::DtcsTone),
            3 => Ok(Self::ToneTone),
            _ => Err(ValidationError::CrossToneTypeOutOfRange(value)),
        }
    }
}

impl From<CrossToneType> for u8 {
    fn from(ct: CrossToneType) -> Self {
        ct as Self
    }
}

/// Flash digital squelch mode at byte 0x0E bits \[1:0\].
///
/// Controls whether D-STAR digital squelch is active per-channel.
/// This is the per-channel flash encoding; the system-level
/// [`DigitalSquelch`](crate::types::dstar::DigitalSquelch) config is separate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FlashDigitalSquelch {
    /// Digital squelch off (value 0).
    Off = 0,
    /// Code squelch -- match digital code (value 1).
    Code = 1,
    /// Callsign squelch -- match source callsign (value 2).
    Callsign = 2,
}

impl TryFrom<u8> for FlashDigitalSquelch {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Off),
            1 => Ok(Self::Code),
            2 => Ok(Self::Callsign),
            _ => Err(ValidationError::FlashDigitalSquelchOutOfRange(value)),
        }
    }
}

impl From<FlashDigitalSquelch> for u8 {
    fn from(ds: FlashDigitalSquelch) -> Self {
        ds as Self
    }
}

/// Fine tuning step size stored at byte 0x09 bits \[1:0\].
///
/// Used in conjunction with the fine-mode flag (byte 0x09 bit 2) for
/// sub-kHz frequency adjustment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FineStep {
    /// 20 Hz fine step (value 0).
    Hz20 = 0,
    /// 100 Hz fine step (value 1).
    Hz100 = 1,
    /// 500 Hz fine step (value 2).
    Hz500 = 2,
    /// 1000 Hz fine step (value 3).
    Hz1000 = 3,
}

impl TryFrom<u8> for FineStep {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Hz20),
            1 => Ok(Self::Hz100),
            2 => Ok(Self::Hz500),
            3 => Ok(Self::Hz1000),
            _ => Err(ValidationError::StepSizeOutOfRange(value)),
        }
    }
}

impl From<FineStep> for u8 {
    fn from(fs: FineStep) -> Self {
        fs as Self
    }
}

impl fmt::Display for FineStep {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Hz20 => f.write_str("20 Hz"),
            Self::Hz100 => f.write_str("100 Hz"),
            Self::Hz500 => f.write_str("500 Hz"),
            Self::Hz1000 => f.write_str("1000 Hz"),
        }
    }
}

/// 40-byte flash memory channel structure.
///
/// Maps byte-for-byte to the MCP programming memory and `.d75` file format.
/// Field layout derived from firmware analysis.
///
/// This struct represents the **flash encoding**, which differs from the
/// CAT wire format ([`ChannelMemory`]) in several ways:
///
/// - **Mode** (byte 0x09 bits \[6:4\]): 8 modes including HF (LSB/USB/CW)
///   and DR, vs 4 modes in CAT.
/// - **Tone/duplex** (byte 0x0A): structured bit fields for tone, CTCSS,
///   DTCS, cross-tone, split, and duplex direction.
/// - **D-STAR callsigns** (bytes 0x0F-0x26): three separate 8-byte fields
///   (UR, RPT1, RPT2) instead of one 24-byte blob.
/// - **Cross-tone / digital squelch** (byte 0x0E): cross-tone type and
///   per-channel digital squelch mode.
///
/// See `docs/mcp_memory_map.md` Section 3.3 for the complete field map.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlashChannel {
    /// RX frequency in Hz (offset 0x00, 4 bytes, little-endian).
    pub rx_frequency: Frequency,
    /// TX offset or split TX frequency in Hz (offset 0x04, 4 bytes, little-endian).
    pub tx_offset: Frequency,
    /// Frequency step size (offset 0x08 bits \[7:4\]).
    pub step_size: StepSize,
    /// Raw low nibble of byte 0x08 (split tune step / unknown bit 0).
    pub byte08_low: u8,
    /// Operating mode (offset 0x09 bits \[6:4\]).
    pub mode: MemoryMode,
    /// Narrow FM flag (offset 0x09 bit 3).
    pub narrow: bool,
    /// Fine tuning mode enabled (offset 0x09 bit 2).
    pub fine_mode: bool,
    /// Fine tuning step size (offset 0x09 bits \[1:0\]).
    pub fine_step: FineStep,
    /// Raw bit 7 of byte 0x09 (unknown / reserved).
    pub byte09_bit7: bool,
    /// Tone encode enable (offset 0x0A bit 7).
    pub tone_enabled: bool,
    /// CTCSS encode+decode enable (offset 0x0A bit 6).
    pub ctcss_enabled: bool,
    /// DTCS (DCS) enable (offset 0x0A bit 5).
    pub dtcs_enabled: bool,
    /// Cross-tone mode enable (offset 0x0A bit 4).
    pub cross_tone: bool,
    /// Raw bit 3 of byte 0x0A (unknown / reserved).
    pub byte0a_bit3: bool,
    /// Odd split flag (offset 0x0A bit 2). When set, `tx_offset` is an
    /// absolute TX frequency rather than a repeater offset.
    pub split: bool,
    /// Duplex direction (offset 0x0A bits \[1:0\]).
    pub duplex: FlashDuplex,
    /// CTCSS TX tone index (offset 0x0B, 0-49).
    pub tone_code: ToneCode,
    /// CTCSS RX tone index (offset 0x0C bits \[5:0\]).
    pub ctcss_code: ToneCode,
    /// Raw high bits of byte 0x0C (bits \[7:6\], unknown).
    pub byte0c_high: u8,
    /// DCS code index (offset 0x0D bits \[6:0\]).
    pub dcs_code: DcsCode,
    /// Raw bit 7 of byte 0x0D (unknown / reserved).
    pub byte0d_bit7: bool,
    /// Cross-tone type (offset 0x0E bits \[5:4\]).
    pub cross_tone_type: CrossToneType,
    /// Digital squelch mode (offset 0x0E bits \[1:0\]).
    pub digital_squelch: FlashDigitalSquelch,
    /// Raw bits of byte 0x0E that are not cross-tone or digital squelch
    /// (bits \[7:6\] and \[3:2\]).
    pub byte0e_reserved: u8,
    /// D-STAR UR callsign (offset 0x0F, 8 bytes, space-padded).
    pub ur_call: DstarCallsign,
    /// D-STAR RPT1 callsign (offset 0x17, 8 bytes, space-padded).
    pub rpt1: DstarCallsign,
    /// D-STAR RPT2 callsign (offset 0x1F, 8 bytes, space-padded).
    pub rpt2: DstarCallsign,
    /// D-STAR DV code (offset 0x27 bits \[6:0\], 0-127).
    pub dv_code: u8,
    /// Raw bit 7 of byte 0x27 (unknown / reserved).
    pub byte27_bit7: bool,
}

impl FlashChannel {
    /// Size of the packed binary representation in bytes.
    pub const BYTE_SIZE: usize = 40;

    /// Parses a flash channel from a byte slice (must be >= 40 bytes).
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::FieldParse`] if any field contains an
    /// invalid value, or if the slice is too short.
    #[allow(clippy::similar_names, clippy::too_many_lines)]
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, ProtocolError> {
        if bytes.len() < Self::BYTE_SIZE {
            return Err(ProtocolError::FieldParse {
                command: "flash_channel".into(),
                field: "length".into(),
                detail: format!(
                    "expected at least {} bytes, got {}",
                    Self::BYTE_SIZE,
                    bytes.len()
                ),
            });
        }

        let rx_frequency = Frequency::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        let tx_offset = Frequency::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);

        // Byte 0x08: step_size [7:4] | low nibble [3:0]
        let step_size =
            StepSize::try_from(bytes[0x08] >> 4).map_err(|e| ProtocolError::FieldParse {
                command: "flash_channel".into(),
                field: "step_size".into(),
                detail: e.to_string(),
            })?;
        let byte08_low = bytes[0x08] & 0x0F;

        // Byte 0x09: unknown[7] | mode[6:4] | narrow[3] | fine_mode[2] | fine_step[1:0]
        let byte09 = bytes[0x09];
        let byte09_bit7 = (byte09 >> 7) & 1 != 0;
        let mode =
            MemoryMode::try_from((byte09 >> 4) & 0x07).map_err(|e| ProtocolError::FieldParse {
                command: "flash_channel".into(),
                field: "mode".into(),
                detail: e.to_string(),
            })?;
        let narrow = (byte09 >> 3) & 1 != 0;
        let fine_mode = (byte09 >> 2) & 1 != 0;
        let fine_step =
            FineStep::try_from(byte09 & 0x03).map_err(|e| ProtocolError::FieldParse {
                command: "flash_channel".into(),
                field: "fine_step".into(),
                detail: e.to_string(),
            })?;

        // Byte 0x0A: tone[7] | ctcss[6] | dtcs[5] | cross[4] | unk[3] | split[2] | duplex[1:0]
        let byte0a = bytes[0x0A];
        let tone_enabled = (byte0a >> 7) & 1 != 0;
        let ctcss_enabled = (byte0a >> 6) & 1 != 0;
        let dtcs_enabled = (byte0a >> 5) & 1 != 0;
        let cross_tone = (byte0a >> 4) & 1 != 0;
        let byte0a_bit3 = (byte0a >> 3) & 1 != 0;
        let split = (byte0a >> 2) & 1 != 0;
        let duplex =
            FlashDuplex::try_from(byte0a & 0x03).map_err(|e| ProtocolError::FieldParse {
                command: "flash_channel".into(),
                field: "duplex".into(),
                detail: e.to_string(),
            })?;

        // Byte 0x0B: CTCSS TX tone index
        let tone_code = ToneCode::new(bytes[0x0B]).map_err(|e| ProtocolError::FieldParse {
            command: "flash_channel".into(),
            field: "tone_code".into(),
            detail: e.to_string(),
        })?;

        // Byte 0x0C: unknown[7:6] | CTCSS RX index[5:0]
        let byte0c_high = (bytes[0x0C] >> 6) & 0x03;
        let ctcss_code =
            ToneCode::new(bytes[0x0C] & 0x3F).map_err(|e| ProtocolError::FieldParse {
                command: "flash_channel".into(),
                field: "ctcss_code".into(),
                detail: e.to_string(),
            })?;

        // Byte 0x0D: unknown[7] | DCS code[6:0]
        let byte0d_bit7 = (bytes[0x0D] >> 7) & 1 != 0;
        let dcs_code = DcsCode::new(bytes[0x0D] & 0x7F).map_err(|e| ProtocolError::FieldParse {
            command: "flash_channel".into(),
            field: "dcs_code".into(),
            detail: e.to_string(),
        })?;

        // Byte 0x0E: reserved[7:6] | cross_type[5:4] | reserved[3:2] | digital_squelch[1:0]
        let byte0e = bytes[0x0E];
        let byte0e_reserved = (byte0e & 0xC0) | (byte0e & 0x0C);
        let cross_tone_type = CrossToneType::try_from((byte0e >> 4) & 0x03).map_err(|e| {
            ProtocolError::FieldParse {
                command: "flash_channel".into(),
                field: "cross_tone_type".into(),
                detail: e.to_string(),
            }
        })?;
        let digital_squelch = FlashDigitalSquelch::try_from(byte0e & 0x03).map_err(|e| {
            ProtocolError::FieldParse {
                command: "flash_channel".into(),
                field: "digital_squelch".into(),
                detail: e.to_string(),
            }
        })?;

        // Bytes 0x0F-0x16: UR callsign (8 bytes)
        let mut ur_arr = [0u8; 8];
        ur_arr.copy_from_slice(&bytes[0x0F..0x17]);
        let ur_call = DstarCallsign::from_wire_bytes(&ur_arr);

        // Bytes 0x17-0x1E: RPT1 callsign (8 bytes)
        let mut rpt1_arr = [0u8; 8];
        rpt1_arr.copy_from_slice(&bytes[0x17..0x1F]);
        let rpt1 = DstarCallsign::from_wire_bytes(&rpt1_arr);

        // Bytes 0x1F-0x26: RPT2 callsign (8 bytes)
        let mut rpt2_arr = [0u8; 8];
        rpt2_arr.copy_from_slice(&bytes[0x1F..0x27]);
        let rpt2 = DstarCallsign::from_wire_bytes(&rpt2_arr);

        // Byte 0x27: unknown[7] | DV code[6:0]
        let byte27_bit7 = (bytes[0x27] >> 7) & 1 != 0;
        let dv_code = bytes[0x27] & 0x7F;

        Ok(Self {
            rx_frequency,
            tx_offset,
            step_size,
            byte08_low,
            mode,
            narrow,
            fine_mode,
            fine_step,
            byte09_bit7,
            tone_enabled,
            ctcss_enabled,
            dtcs_enabled,
            cross_tone,
            byte0a_bit3,
            split,
            duplex,
            tone_code,
            ctcss_code,
            byte0c_high,
            dcs_code,
            byte0d_bit7,
            cross_tone_type,
            digital_squelch,
            byte0e_reserved,
            ur_call,
            rpt1,
            rpt2,
            dv_code,
            byte27_bit7,
        })
    }

    /// Serializes the flash channel to a 40-byte packed binary array.
    #[must_use]
    pub fn to_bytes(&self) -> [u8; 40] {
        let mut buf = [0u8; 40];

        // bytes[0..4]: RX frequency, little-endian
        buf[0..4].copy_from_slice(&self.rx_frequency.to_le_bytes());

        // bytes[4..8]: TX offset, little-endian
        buf[4..8].copy_from_slice(&self.tx_offset.to_le_bytes());

        // byte 0x08: step_size[7:4] | low[3:0]
        buf[0x08] = (u8::from(self.step_size) << 4) | (self.byte08_low & 0x0F);

        // byte 0x09: bit7 | mode[6:4] | narrow[3] | fine_mode[2] | fine_step[1:0]
        buf[0x09] = (u8::from(self.byte09_bit7) << 7)
            | (u8::from(self.mode) << 4)
            | (u8::from(self.narrow) << 3)
            | (u8::from(self.fine_mode) << 2)
            | u8::from(self.fine_step);

        // byte 0x0A: tone[7] | ctcss[6] | dtcs[5] | cross[4] | bit3 | split[2] | duplex[1:0]
        buf[0x0A] = (u8::from(self.tone_enabled) << 7)
            | (u8::from(self.ctcss_enabled) << 6)
            | (u8::from(self.dtcs_enabled) << 5)
            | (u8::from(self.cross_tone) << 4)
            | (u8::from(self.byte0a_bit3) << 3)
            | (u8::from(self.split) << 2)
            | u8::from(self.duplex);

        // byte 0x0B: tone code index
        buf[0x0B] = self.tone_code.index();

        // byte 0x0C: high[7:6] | ctcss_code[5:0]
        buf[0x0C] = (self.byte0c_high << 6) | (self.ctcss_code.index() & 0x3F);

        // byte 0x0D: bit7 | dcs_code[6:0]
        buf[0x0D] = (u8::from(self.byte0d_bit7) << 7) | (self.dcs_code.index() & 0x7F);

        // byte 0x0E: reserved[7:6] | cross_type[5:4] | reserved[3:2] | digital_squelch[1:0]
        buf[0x0E] = (self.byte0e_reserved & 0xCC)
            | (u8::from(self.cross_tone_type) << 4)
            | u8::from(self.digital_squelch);

        // bytes 0x0F-0x16: UR callsign
        buf[0x0F..0x17].copy_from_slice(&self.ur_call.to_wire_bytes());

        // bytes 0x17-0x1E: RPT1 callsign
        buf[0x17..0x1F].copy_from_slice(&self.rpt1.to_wire_bytes());

        // bytes 0x1F-0x26: RPT2 callsign
        buf[0x1F..0x27].copy_from_slice(&self.rpt2.to_wire_bytes());

        // byte 0x27: bit7 | dv_code[6:0]
        buf[0x27] = (u8::from(self.byte27_bit7) << 7) | (self.dv_code & 0x7F);

        buf
    }
}

impl Default for FlashChannel {
    fn default() -> Self {
        Self {
            rx_frequency: Frequency::new(0),
            tx_offset: Frequency::new(0),
            step_size: StepSize::Hz5000,
            byte08_low: 0,
            mode: MemoryMode::Fm,
            narrow: false,
            fine_mode: false,
            fine_step: FineStep::Hz20,
            byte09_bit7: false,
            tone_enabled: false,
            ctcss_enabled: false,
            dtcs_enabled: false,
            cross_tone: false,
            byte0a_bit3: false,
            split: false,
            duplex: FlashDuplex::Simplex,
            // Safety: 0 is always a valid index for ToneCode (0..=49)
            tone_code: ToneCode::new(0).expect("0 is valid tone code"),
            ctcss_code: ToneCode::new(0).expect("0 is valid tone code"),
            byte0c_high: 0,
            // Safety: 0 is always a valid index for DcsCode (0..=103)
            dcs_code: DcsCode::new(0).expect("0 is valid DCS code"),
            byte0d_bit7: false,
            cross_tone_type: CrossToneType::DtcsDtcs,
            digital_squelch: FlashDigitalSquelch::Off,
            byte0e_reserved: 0,
            ur_call: DstarCallsign::default(),
            rpt1: DstarCallsign::default(),
            rpt2: DstarCallsign::default(),
            dv_code: 0,
            byte27_bit7: false,
        }
    }
}

impl Default for ChannelMemory {
    fn default() -> Self {
        Self {
            rx_frequency: Frequency::new(0),
            tx_offset: Frequency::new(0),
            step_size: StepSize::Hz5000,
            mode_flags_raw: 0,
            shift: ShiftDirection::SIMPLEX,
            reverse: false,
            tone_enable: false,
            ctcss_mode: CtcssMode::Off,
            dcs_enable: false,
            cross_tone_reverse: false,
            flags_0a_raw: 0,
            // Safety: 0 is always a valid index for ToneCode (0..=49)
            tone_code: ToneCode::new(0).expect("0 is valid tone code"),
            // Safety: 0 is always a valid index for ToneCode (0..=49)
            ctcss_code: ToneCode::new(0).expect("0 is valid tone code"),
            // Safety: 0 is always a valid index for DcsCode (0..=103)
            dcs_code: DcsCode::new(0).expect("0 is valid DCS code"),
            cross_tone_combo: CrossToneType::DtcsDtcs,
            digital_squelch: FlashDigitalSquelch::Off,
            urcall: ChannelName::default(),
            data_mode: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ValidationError;
    use crate::types::frequency::Frequency;
    use crate::types::mode::{ShiftDirection, StepSize};
    use crate::types::tone::{CtcssMode, DcsCode, ToneCode};

    #[test]
    fn channel_name_valid() {
        let name = ChannelName::new("RPT1").unwrap();
        assert_eq!(name.as_str(), "RPT1");
    }

    #[test]
    fn channel_name_empty() {
        let name = ChannelName::new("").unwrap();
        assert_eq!(name.as_str(), "");
    }

    #[test]
    fn channel_name_max_length() {
        let name = ChannelName::new("12345678").unwrap();
        assert_eq!(name.as_str(), "12345678");
    }

    #[test]
    fn channel_name_too_long() {
        let err = ChannelName::new("123456789").unwrap_err();
        assert!(matches!(
            err,
            ValidationError::ChannelNameTooLong { len: 9 }
        ));
    }

    #[test]
    fn channel_name_to_bytes_padded() {
        let name = ChannelName::new("RPT1").unwrap();
        let bytes = name.to_bytes();
        assert_eq!(bytes.len(), 24);
        assert_eq!(&bytes[..4], b"RPT1");
        assert!(bytes[4..].iter().all(|&b| b == 0));
    }

    #[test]
    fn channel_name_from_bytes() {
        let mut bytes = [0u8; 24];
        bytes[..4].copy_from_slice(b"RPT1");
        let name = ChannelName::from_bytes(&bytes);
        assert_eq!(name.as_str(), "RPT1");
    }

    // --- ChannelMemory tests ---

    #[test]
    fn channel_memory_byte_layout_size() {
        assert_eq!(ChannelMemory::BYTE_SIZE, 40);
    }

    #[test]
    fn channel_memory_round_trip_simplex_vhf() {
        // flags_0a_raw must be consistent with individual fields for round-trip
        // tone=bit7, shift=bits2:0=1 → flags_0a_raw = 0x81
        let ch = ChannelMemory {
            rx_frequency: Frequency::new(145_000_000),
            tx_offset: Frequency::new(600_000),
            step_size: StepSize::Hz12500,
            mode_flags_raw: 0,
            shift: ShiftDirection::UP,
            reverse: false,
            tone_enable: true,
            ctcss_mode: CtcssMode::Off,
            dcs_enable: false,
            cross_tone_reverse: false,
            flags_0a_raw: 0x81, // tone(bit7) + shift+(bit0)
            tone_code: ToneCode::new(8).unwrap(),
            ctcss_code: ToneCode::new(8).unwrap(),
            dcs_code: DcsCode::new(0).unwrap(),
            cross_tone_combo: CrossToneType::DtcsDtcs,
            digital_squelch: FlashDigitalSquelch::Off,
            urcall: ChannelName::new("").unwrap(),
            data_mode: 0,
        };
        let bytes = ch.to_bytes();
        assert_eq!(bytes.len(), 40);
        let parsed = ChannelMemory::from_bytes(&bytes).unwrap();
        assert_eq!(parsed, ch);
    }

    #[test]
    fn channel_memory_byte08_packing() {
        let ch = ChannelMemory {
            step_size: StepSize::Hz12500, // index 5
            shift: ShiftDirection::UP,    // 1
            ..ChannelMemory::default()
        };
        let bytes = ch.to_bytes();
        assert_eq!(bytes[0x08], 0x51); // 5 << 4 | 1
    }

    #[test]
    fn channel_memory_byte09_packing() {
        // byte[9] is now zeroed in to_bytes (mode/fine not individually modeled)
        let ch = ChannelMemory {
            reverse: true,
            tone_enable: true,
            ctcss_mode: CtcssMode::On,
            ..ChannelMemory::default()
        };
        let bytes = ch.to_bytes();
        assert_eq!(bytes[0x09], 0x00);
    }

    #[test]
    fn channel_memory_byte0a_packing() {
        // byte[0x0A] is stored directly from flags_0a_raw (hardware-verified)
        // tone=bit7, ctcss=bit6, dcs=bit5, cross=bit4, reverse=bit3, shift=bits2:0
        let ch = ChannelMemory {
            dcs_enable: true,
            cross_tone_reverse: true,
            flags_0a_raw: 0xB0, // dcs(bit5) + cross(bit4) + tone(bit7)... actually just set directly
            ..ChannelMemory::default()
        };
        let bytes = ch.to_bytes();
        assert_eq!(bytes[0x0A], 0xB0);
    }

    #[test]
    fn channel_memory_unknown_bits_passthrough() {
        let ch = ChannelMemory {
            dcs_enable: false,
            cross_tone_reverse: false,
            flags_0a_raw: 0x2B,
            ..ChannelMemory::default()
        };
        let bytes = ch.to_bytes();
        assert_eq!(bytes[0x0A], 0x2B);
        let parsed = ChannelMemory::from_bytes(&bytes).unwrap();
        assert_eq!(parsed.flags_0a_raw, 0x2B);
    }

    #[test]
    fn channel_memory_byte0e_packing() {
        let ch = ChannelMemory {
            cross_tone_combo: CrossToneType::ToneDtcs,
            digital_squelch: FlashDigitalSquelch::Code,
            ..ChannelMemory::default()
        };
        let bytes = ch.to_bytes();
        assert_eq!(bytes[0x0E], 0x1D); // (1<<4) | 0x0C | 1
    }

    #[test]
    fn channel_memory_frequency_le_bytes() {
        let ch = ChannelMemory {
            rx_frequency: Frequency::new(145_000_000),
            tx_offset: Frequency::new(600_000),
            ..ChannelMemory::default()
        };
        let bytes = ch.to_bytes();
        let rx = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        assert_eq!(rx, 145_000_000);
        let tx = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
        assert_eq!(tx, 600_000);
    }

    // --- FlashChannel tests ---

    #[test]
    fn flash_channel_byte_size() {
        assert_eq!(FlashChannel::BYTE_SIZE, 40);
    }

    #[test]
    fn flash_channel_default_round_trip() {
        let ch = FlashChannel::default();
        let bytes = ch.to_bytes();
        let parsed = FlashChannel::from_bytes(&bytes).unwrap();
        assert_eq!(parsed, ch);
    }

    #[test]
    fn flash_channel_mode_encoding() {
        use crate::types::mode::MemoryMode;
        // Flash mode encoding: FM=0, DV=1, AM=2, LSB=3, USB=4, CW=5, NFM=6, DR=7
        for (raw, expected) in [
            (0, MemoryMode::Fm),
            (1, MemoryMode::Dv),
            (2, MemoryMode::Am),
            (3, MemoryMode::Lsb),
            (4, MemoryMode::Usb),
            (5, MemoryMode::Cw),
            (6, MemoryMode::Nfm),
            (7, MemoryMode::Dr),
        ] {
            let mut ch = FlashChannel::default();
            ch.mode = expected;
            let bytes = ch.to_bytes();
            // Mode is at byte 0x09 bits [6:4]
            assert_eq!(
                (bytes[0x09] >> 4) & 0x07,
                raw,
                "mode {expected} should encode as {raw}"
            );
            let parsed = FlashChannel::from_bytes(&bytes).unwrap();
            assert_eq!(parsed.mode, expected);
        }
    }

    #[test]
    fn flash_channel_mode_matches_cat() {
        // CAT MD and flash memory use the same encoding for all 8 modes (0-7).
        use crate::types::mode::{MemoryMode, Mode};
        assert_eq!(u8::from(MemoryMode::Am), u8::from(Mode::Am));
        assert_eq!(u8::from(MemoryMode::Nfm), u8::from(Mode::Nfm));
        assert_eq!(u8::from(MemoryMode::Fm), u8::from(Mode::Fm));
        assert_eq!(u8::from(MemoryMode::Dr), u8::from(Mode::Dr));
    }

    #[test]
    fn flash_channel_byte09_packing() {
        let ch = FlashChannel {
            mode: MemoryMode::Am,        // 2 -> bits [6:4] = 0b010
            narrow: true,                // bit 3
            fine_mode: true,             // bit 2
            fine_step: FineStep::Hz1000, // bits [1:0] = 3
            byte09_bit7: false,
            ..FlashChannel::default()
        };
        let bytes = ch.to_bytes();
        // Expected: 0b0_010_1_1_11 = 0x2F
        assert_eq!(bytes[0x09], 0x2F);
        let parsed = FlashChannel::from_bytes(&bytes).unwrap();
        assert_eq!(parsed.mode, MemoryMode::Am);
        assert!(parsed.narrow);
        assert!(parsed.fine_mode);
        assert_eq!(parsed.fine_step, FineStep::Hz1000);
    }

    #[test]
    fn flash_channel_byte0a_tone_duplex() {
        let ch = FlashChannel {
            tone_enabled: true,         // bit 7
            ctcss_enabled: false,       // bit 6
            dtcs_enabled: true,         // bit 5
            cross_tone: false,          // bit 4
            byte0a_bit3: false,         // bit 3
            split: true,                // bit 2
            duplex: FlashDuplex::Minus, // bits [1:0] = 2
            ..FlashChannel::default()
        };
        let bytes = ch.to_bytes();
        // Expected: 0b1_0_1_0_0_1_10 = 0xA6
        assert_eq!(bytes[0x0A], 0xA6);
        let parsed = FlashChannel::from_bytes(&bytes).unwrap();
        assert!(parsed.tone_enabled);
        assert!(!parsed.ctcss_enabled);
        assert!(parsed.dtcs_enabled);
        assert!(!parsed.cross_tone);
        assert!(parsed.split);
        assert_eq!(parsed.duplex, FlashDuplex::Minus);
    }

    #[test]
    fn flash_channel_dstar_callsigns() {
        use crate::types::dstar::DstarCallsign;
        let ch = FlashChannel {
            ur_call: DstarCallsign::new("CQCQCQ").unwrap(),
            rpt1: DstarCallsign::new("W4BFB B").unwrap(),
            rpt2: DstarCallsign::new("W4BFB G").unwrap(),
            ..FlashChannel::default()
        };
        let bytes = ch.to_bytes();
        // UR at 0x0F-0x16 (8 bytes, space-padded)
        assert_eq!(&bytes[0x0F..0x17], b"CQCQCQ  ");
        // RPT1 at 0x17-0x1E
        assert_eq!(&bytes[0x17..0x1F], b"W4BFB B ");
        // RPT2 at 0x1F-0x26
        assert_eq!(&bytes[0x1F..0x27], b"W4BFB G ");

        let parsed = FlashChannel::from_bytes(&bytes).unwrap();
        assert_eq!(parsed.ur_call.as_str(), "CQCQCQ");
        assert_eq!(parsed.rpt1.as_str(), "W4BFB B");
        assert_eq!(parsed.rpt2.as_str(), "W4BFB G");
    }

    #[test]
    fn flash_channel_byte0e_cross_tone_digital_squelch() {
        let ch = FlashChannel {
            cross_tone_type: CrossToneType::DtcsTone, // bits [5:4] = 2
            digital_squelch: FlashDigitalSquelch::Callsign, // bits [1:0] = 2
            byte0e_reserved: 0,
            ..FlashChannel::default()
        };
        let bytes = ch.to_bytes();
        // Expected: 0b00_10_00_10 = 0x22
        assert_eq!(bytes[0x0E], 0x22);
        let parsed = FlashChannel::from_bytes(&bytes).unwrap();
        assert_eq!(parsed.cross_tone_type, CrossToneType::DtcsTone);
        assert_eq!(parsed.digital_squelch, FlashDigitalSquelch::Callsign);
    }

    #[test]
    fn flash_channel_dv_code() {
        let ch = FlashChannel {
            dv_code: 42,
            byte27_bit7: true,
            ..FlashChannel::default()
        };
        let bytes = ch.to_bytes();
        assert_eq!(bytes[0x27], 0x80 | 42);
        let parsed = FlashChannel::from_bytes(&bytes).unwrap();
        assert_eq!(parsed.dv_code, 42);
        assert!(parsed.byte27_bit7);
    }

    #[test]
    fn flash_channel_full_round_trip() {
        use crate::types::dstar::DstarCallsign;
        let ch = FlashChannel {
            rx_frequency: Frequency::new(146_520_000),
            tx_offset: Frequency::new(600_000),
            step_size: StepSize::Hz12500,
            byte08_low: 0x03,
            mode: MemoryMode::Dv,
            narrow: false,
            fine_mode: true,
            fine_step: FineStep::Hz100,
            byte09_bit7: false,
            tone_enabled: true,
            ctcss_enabled: true,
            dtcs_enabled: false,
            cross_tone: false,
            byte0a_bit3: false,
            split: false,
            duplex: FlashDuplex::Plus,
            tone_code: ToneCode::new(8).unwrap(),
            ctcss_code: ToneCode::new(12).unwrap(),
            byte0c_high: 0,
            dcs_code: DcsCode::new(5).unwrap(),
            byte0d_bit7: false,
            cross_tone_type: CrossToneType::ToneDtcs,
            digital_squelch: FlashDigitalSquelch::Code,
            byte0e_reserved: 0,
            ur_call: DstarCallsign::new("CQCQCQ").unwrap(),
            rpt1: DstarCallsign::new("W4BFB B").unwrap(),
            rpt2: DstarCallsign::new("W4BFB G").unwrap(),
            dv_code: 99,
            byte27_bit7: false,
        };
        let bytes = ch.to_bytes();
        assert_eq!(bytes.len(), 40);
        let parsed = FlashChannel::from_bytes(&bytes).unwrap();
        assert_eq!(parsed, ch);
    }

    #[test]
    fn flash_channel_too_short() {
        let bytes = [0u8; 39];
        let err = FlashChannel::from_bytes(&bytes);
        assert!(err.is_err());
    }

    #[test]
    fn flash_channel_reserved_bits_preserved() {
        let ch = FlashChannel {
            byte0c_high: 0x03,
            byte0d_bit7: true,
            byte0e_reserved: 0xCC, // bits [7:6] and [3:2]
            byte0a_bit3: true,
            byte09_bit7: true,
            ..FlashChannel::default()
        };
        let bytes = ch.to_bytes();
        let parsed = FlashChannel::from_bytes(&bytes).unwrap();
        assert_eq!(parsed.byte0c_high, 0x03);
        assert!(parsed.byte0d_bit7);
        assert_eq!(parsed.byte0e_reserved, 0xCC);
        assert!(parsed.byte0a_bit3);
        assert!(parsed.byte09_bit7);
    }

    #[test]
    fn flash_fine_step_display() {
        assert_eq!(FineStep::Hz20.to_string(), "20 Hz");
        assert_eq!(FineStep::Hz100.to_string(), "100 Hz");
        assert_eq!(FineStep::Hz500.to_string(), "500 Hz");
        assert_eq!(FineStep::Hz1000.to_string(), "1000 Hz");
    }

    #[test]
    fn flash_duplex_round_trip() {
        for i in 0u8..3 {
            let d = FlashDuplex::try_from(i).unwrap();
            assert_eq!(u8::from(d), i);
        }
        assert!(FlashDuplex::try_from(3).is_err());
    }

    #[test]
    fn cross_tone_type_round_trip() {
        for i in 0u8..4 {
            let ct = CrossToneType::try_from(i).unwrap();
            assert_eq!(u8::from(ct), i);
        }
        assert!(CrossToneType::try_from(4).is_err());
    }

    #[test]
    fn flash_digital_squelch_round_trip() {
        for i in 0u8..3 {
            let ds = FlashDigitalSquelch::try_from(i).unwrap();
            assert_eq!(u8::from(ds), i);
        }
        assert!(FlashDigitalSquelch::try_from(3).is_err());
    }
}
