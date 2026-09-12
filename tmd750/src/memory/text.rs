//! Offline, typed editing of explicitly supported configuration strings.
//!
//! This module interprets memory images using the generated software layout.
//! It does not emulate a keyboard, open a connection, or write a radio. An
//! exact firmware-label match is distinct from hardware qualification; an
//! explicitly unqualified interpretation never enables the radio write gate.

use std::{fmt, str::FromStr};

use super::{
    FieldCodec, FieldValue, MCP_D750_SCHEMA_FIRMWARE_IDENTITIES, MemoryImage, MenuField,
    PatchPlanner, PatchSet, StringEncoding, is_supported_schema_target, menu_field,
};
use crate::error::SchemaError;
use crate::types::{FirmwareIdentity, RadioModel, SlotIndex};

/// A supported configuration string, not an arbitrary menu-field selector.
///
/// List numbers are one-based labels. Programmable-memory slot indices are
/// separately supplied through [`TextScope`] and remain zero-based.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TextSetting {
    /// First global programmable-memory name.
    PmName1,
    /// Second global programmable-memory name.
    PmName2,
    /// Third global programmable-memory name.
    PmName3,
    /// Fourth global programmable-memory name.
    PmName4,
    /// Fifth global programmable-memory name.
    PmName5,
    /// First D-STAR MY callsign in a programmable-memory slot.
    DstarMyCallsign1,
    /// Second D-STAR MY callsign in a programmable-memory slot.
    DstarMyCallsign2,
    /// Third D-STAR MY callsign in a programmable-memory slot.
    DstarMyCallsign3,
    /// Fourth D-STAR MY callsign in a programmable-memory slot.
    DstarMyCallsign4,
    /// Fifth D-STAR MY callsign in a programmable-memory slot.
    DstarMyCallsign5,
    /// Sixth D-STAR MY callsign in a programmable-memory slot.
    DstarMyCallsign6,
    /// Memo paired with the first D-STAR MY callsign.
    DstarMemo1,
    /// Memo paired with the second D-STAR MY callsign.
    DstarMemo2,
    /// Memo paired with the third D-STAR MY callsign.
    DstarMemo3,
    /// Memo paired with the fourth D-STAR MY callsign.
    DstarMemo4,
    /// Memo paired with the fifth D-STAR MY callsign.
    DstarMemo5,
    /// Memo paired with the sixth D-STAR MY callsign.
    DstarMemo6,
    /// First D-STAR transmit message in a programmable-memory slot.
    DstarMessage1,
    /// Second D-STAR transmit message in a programmable-memory slot.
    DstarMessage2,
    /// Third D-STAR transmit message in a programmable-memory slot.
    DstarMessage3,
    /// Fourth D-STAR transmit message in a programmable-memory slot.
    DstarMessage4,
    /// Fifth D-STAR transmit message in a programmable-memory slot.
    DstarMessage5,
    /// Power-on message in a programmable-memory slot.
    PowerOnMessage,
}

impl TextSetting {
    /// Every supported string, in stable presentation order.
    #[must_use]
    pub const fn all() -> &'static [Self] {
        &[
            Self::PmName1,
            Self::PmName2,
            Self::PmName3,
            Self::PmName4,
            Self::PmName5,
            Self::DstarMyCallsign1,
            Self::DstarMyCallsign2,
            Self::DstarMyCallsign3,
            Self::DstarMyCallsign4,
            Self::DstarMyCallsign5,
            Self::DstarMyCallsign6,
            Self::DstarMemo1,
            Self::DstarMemo2,
            Self::DstarMemo3,
            Self::DstarMemo4,
            Self::DstarMemo5,
            Self::DstarMemo6,
            Self::DstarMessage1,
            Self::DstarMessage2,
            Self::DstarMessage3,
            Self::DstarMessage4,
            Self::DstarMessage5,
            Self::PowerOnMessage,
        ]
    }

    /// Stable, case-sensitive command-line spelling.
    #[must_use]
    pub const fn key(self) -> &'static str {
        self.names().0
    }

    /// Resolve storage constraints from the generated registry.
    ///
    /// # Errors
    ///
    /// Returns [`TextError::RegistryFieldMissing`] or
    /// [`TextError::RegistryFieldNotText`] if the generated entry changes.
    pub fn metadata(self) -> Result<TextMetadata, TextError> {
        let field = self.field()?;
        let FieldCodec::FixedString {
            len,
            encoding,
            padding,
        } = field.descriptor.codec
        else {
            return Err(TextError::RegistryFieldNotText { setting: self });
        };
        if field.is_blob {
            return Err(TextError::RegistryFieldNotText { setting: self });
        }
        Ok(TextMetadata {
            field_name: field.descriptor.name,
            scope: if field.descriptor.is_per_slot() {
                TextScopeKind::PerSlot
            } else {
                TextScopeKind::Global
            },
            max_bytes: len,
            encoding,
            padding,
        })
    }

    fn field(self) -> Result<&'static MenuField, TextError> {
        menu_field(self.names().1).ok_or(TextError::RegistryFieldMissing { setting: self })
    }

    const fn names(self) -> (&'static str, &'static str) {
        match self {
            Self::PmName1 => ("pm-name-1", "pm.PmName1"),
            Self::PmName2 => ("pm-name-2", "pm.PmName2"),
            Self::PmName3 => ("pm-name-3", "pm.PmName3"),
            Self::PmName4 => ("pm-name-4", "pm.PmName4"),
            Self::PmName5 => ("pm-name-5", "pm.PmName5"),
            Self::DstarMyCallsign1 => (
                "dstar-my-callsign-1",
                "dv.MyCallsignDvGatewayList[0].MyCallsignDvGateway",
            ),
            Self::DstarMyCallsign2 => (
                "dstar-my-callsign-2",
                "dv.MyCallsignDvGatewayList[1].MyCallsignDvGateway",
            ),
            Self::DstarMyCallsign3 => (
                "dstar-my-callsign-3",
                "dv.MyCallsignDvGatewayList[2].MyCallsignDvGateway",
            ),
            Self::DstarMyCallsign4 => (
                "dstar-my-callsign-4",
                "dv.MyCallsignDvGatewayList[3].MyCallsignDvGateway",
            ),
            Self::DstarMyCallsign5 => (
                "dstar-my-callsign-5",
                "dv.MyCallsignDvGatewayList[4].MyCallsignDvGateway",
            ),
            Self::DstarMyCallsign6 => (
                "dstar-my-callsign-6",
                "dv.MyCallsignDvGatewayList[5].MyCallsignDvGateway",
            ),
            Self::DstarMemo1 => (
                "dstar-memo-1",
                "dv.MyCallsignDvGatewayList[0].MemoDvGateway",
            ),
            Self::DstarMemo2 => (
                "dstar-memo-2",
                "dv.MyCallsignDvGatewayList[1].MemoDvGateway",
            ),
            Self::DstarMemo3 => (
                "dstar-memo-3",
                "dv.MyCallsignDvGatewayList[2].MemoDvGateway",
            ),
            Self::DstarMemo4 => (
                "dstar-memo-4",
                "dv.MyCallsignDvGatewayList[3].MemoDvGateway",
            ),
            Self::DstarMemo5 => (
                "dstar-memo-5",
                "dv.MyCallsignDvGatewayList[4].MemoDvGateway",
            ),
            Self::DstarMemo6 => (
                "dstar-memo-6",
                "dv.MyCallsignDvGatewayList[5].MemoDvGateway",
            ),
            Self::DstarMessage1 => ("dstar-message-1", "dv.MyDvMessageList[0].MyDvMessage"),
            Self::DstarMessage2 => ("dstar-message-2", "dv.MyDvMessageList[1].MyDvMessage"),
            Self::DstarMessage3 => ("dstar-message-3", "dv.MyDvMessageList[2].MyDvMessage"),
            Self::DstarMessage4 => ("dstar-message-4", "dv.MyDvMessageList[3].MyDvMessage"),
            Self::DstarMessage5 => ("dstar-message-5", "dv.MyDvMessageList[4].MyDvMessage"),
            Self::PowerOnMessage => ("power-on-message", "radio.PowerOnMessage"),
        }
    }
}

impl fmt::Display for TextSetting {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.key())
    }
}

impl FromStr for TextSetting {
    type Err = TextError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::all()
            .iter()
            .copied()
            .find(|setting| setting.key() == value)
            .ok_or_else(|| TextError::UnknownSetting {
                value: value.to_owned(),
            })
    }
}

/// Whether a string is global or requires one programmable-memory slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextScopeKind {
    /// Shared across all programmable-memory slots.
    Global,
    /// Stored separately in each of the six programmable-memory slots.
    PerSlot,
}

/// Explicit location of one supported configuration string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextScope {
    /// A global field; rejected for per-slot settings.
    Global,
    /// One zero-based programmable-memory slot; rejected for global settings.
    Slot(SlotIndex),
}

impl TextScope {
    const fn kind(self) -> TextScopeKind {
        match self {
            Self::Global => TextScopeKind::Global,
            Self::Slot(_) => TextScopeKind::PerSlot,
        }
    }

    const fn slot(self) -> Option<SlotIndex> {
        match self {
            Self::Global => None,
            Self::Slot(slot) => Some(slot),
        }
    }
}

/// Storage facts resolved from the generated descriptor, not UI assumptions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextMetadata {
    /// Exact generated registry field name.
    pub field_name: &'static str,
    /// Required scope.
    pub scope: TextScopeKind,
    /// Maximum encoded byte count, not Unicode character count.
    pub max_bytes: usize,
    /// Generated encoding; `MemoryMap` currently accepts printable ASCII.
    pub encoding: StringEncoding,
    /// Byte filling the unused fixed-string capacity.
    pub padding: u8,
}

/// Relationship between supplied firmware provenance and the software layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextLayoutQualification {
    /// The supplied identity exactly matches a registry target label.
    ///
    /// This is not evidence of hardware-tested interpretation or writes.
    RegistryTargetMatched,
    /// The caller explicitly requested offline interpretation without qualification.
    ///
    /// Decoded labels, offsets, and previews are software-layout hypotheses.
    UnqualifiedInterpretation,
}

/// Immutable, firmware-labeled view of supported configuration strings.
///
/// Firmware provenance is supplied by the caller; image size does not identify
/// firmware. Neither constructor establishes live-radio compatibility.
///
/// The following zero-filled image is a synthetic example, not a radio backup.
/// The original image is borrowed immutably; applying the preview is an explicit
/// operation on a separate byte buffer.
///
/// ```
/// use kenwood_tmd750::{FirmwareIdentity, MemoryImage};
/// use kenwood_tmd750::memory::{TextImage, TextScope, TextSetting};
/// use kenwood_tmd750::types::IMAGE_LENGTH;
///
/// let image = MemoryImage::from_bytes(vec![0; IMAGE_LENGTH])?;
/// let firmware = FirmwareIdentity::new("1.00")?;
/// let text = TextImage::new(&image, &firmware)?;
/// let preview = text.preview(TextSetting::PmName1, TextScope::Global, "FIELD")?;
/// assert_eq!(preview.before(), "");
/// assert_eq!(preview.after(), "FIELD");
///
/// let mut edited = image.as_bytes().to_vec();
/// preview.patches().apply_to_image(&mut edited)?;
/// let edited = MemoryImage::from_bytes(edited)?;
/// assert_eq!(
///     TextImage::new(&edited, &firmware)?.read(TextSetting::PmName1, TextScope::Global)?,
///     "FIELD"
/// );
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, Clone)]
pub struct TextImage<'a> {
    image: &'a MemoryImage,
    firmware: FirmwareIdentity,
    qualification: TextLayoutQualification,
}

impl<'a> TextImage<'a> {
    /// Require an exact firmware identity accepted by the generated registry.
    ///
    /// # Errors
    ///
    /// Returns [`TextError::UnsupportedFirmware`] for any other identity,
    /// including firmware 1.02. For explicit offline research only, see
    /// [`Self::interpret_unqualified`].
    pub fn new(image: &'a MemoryImage, firmware: &FirmwareIdentity) -> Result<Self, TextError> {
        if !is_supported_schema_target(RadioModel::TmD750, firmware) {
            return Err(TextError::UnsupportedFirmware {
                actual: firmware.clone(),
                accepted: MCP_D750_SCHEMA_FIRMWARE_IDENTITIES,
            });
        }
        Ok(Self {
            image,
            firmware: firmware.clone(),
            qualification: TextLayoutQualification::RegistryTargetMatched,
        })
    }

    /// Explicitly interpret an image using an unqualified software layout.
    ///
    /// Read results and previews remain unqualified even if `firmware` happens
    /// to match a registry target. Callers should display this status and the
    /// original firmware identity. This does not enable live writes.
    #[must_use]
    pub fn interpret_unqualified(image: &'a MemoryImage, firmware: &FirmwareIdentity) -> Self {
        Self {
            image,
            firmware: firmware.clone(),
            qualification: TextLayoutQualification::UnqualifiedInterpretation,
        }
    }

    /// Layout qualification attached to every read and preview from this view.
    #[must_use]
    pub const fn qualification(&self) -> TextLayoutQualification {
        self.qualification
    }

    /// Original, caller-supplied firmware provenance.
    #[must_use]
    pub const fn firmware(&self) -> &FirmwareIdentity {
        &self.firmware
    }

    /// Decode one string, removing only trailing descriptor padding.
    ///
    /// Interior spaces are retained. Invalid encodings or embedded control
    /// characters are errors, not silently truncated or replacement-decoded.
    ///
    /// # Errors
    ///
    /// Returns registry, scope, address, encoding, or control-character errors.
    pub fn read(&self, setting: TextSetting, scope: TextScope) -> Result<String, TextError> {
        let metadata = resolve_scope(setting, scope)?;
        let field = setting.field()?;
        let start = field.descriptor.address(scope.slot())?.as_usize();
        let bytes = self
            .image
            .as_bytes()
            .get(start..start + metadata.max_bytes)
            .ok_or_else(|| SchemaError::OutOfBounds {
                field: metadata.field_name,
                address: u64::try_from(start).unwrap_or(u64::MAX),
                len: metadata.max_bytes,
                image_length: self.image.as_bytes().len(),
            })?;
        decode(setting, metadata, bytes)
    }

    /// Validate text and plan an offline patch without changing the source image.
    ///
    /// Byte limits and encoding come from the registry. No case folding,
    /// truncation, callsign syntax validation, or radio-side UI behavior is
    /// implied. The preview's `after` value omits trailing storage padding;
    /// meaningful interior spaces remain intact. The existing live-write
    /// firmware gate is independent and unchanged.
    ///
    /// # Errors
    ///
    /// Returns read errors for the original value, invalid input errors, and
    /// the checked region/overlap errors from [`PatchPlanner`].
    pub fn preview(
        &self,
        setting: TextSetting,
        scope: TextScope,
        value: &str,
    ) -> Result<TextPreview, TextError> {
        let metadata = resolve_scope(setting, scope)?;
        validate_characters(setting, value)?;
        let mut planner = PatchPlanner::new();
        let _planned = planner.set_menu(setting.field()?, scope.slot(), FieldValue::Text(value))?;
        Ok(TextPreview {
            setting,
            scope,
            before: self.read(setting, scope)?,
            after: decode(setting, metadata, value.as_bytes())?,
            patches: planner.finish()?,
            firmware: self.firmware.clone(),
            qualification: self.qualification,
        })
    }
}

/// An offline, single-setting patch with its provenance and decoded comparison.
///
/// Applying the patch to another image does not establish that image's origin
/// or firmware compatibility. Preserve the associated firmware and
/// qualification when displaying or exporting the preview.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextPreview {
    setting: TextSetting,
    scope: TextScope,
    before: String,
    after: String,
    patches: PatchSet,
    firmware: FirmwareIdentity,
    qualification: TextLayoutQualification,
}

impl TextPreview {
    /// Setting whose storage is covered by this preview.
    #[must_use]
    pub const fn setting(&self) -> TextSetting {
        self.setting
    }

    /// Exact scope selected by the caller.
    #[must_use]
    pub const fn scope(&self) -> TextScope {
        self.scope
    }

    /// Decoded value from the unchanged source image.
    #[must_use]
    pub fn before(&self) -> &str {
        &self.before
    }

    /// Decoded planned value, without trailing storage padding.
    #[must_use]
    pub fn after(&self) -> &str {
        &self.after
    }

    /// Checked patches for offline inspection or application to an image copy.
    #[must_use]
    pub const fn patches(&self) -> &PatchSet {
        &self.patches
    }

    /// Original firmware provenance, not a claim about a connected radio.
    #[must_use]
    pub const fn firmware(&self) -> &FirmwareIdentity {
        &self.firmware
    }

    /// Qualification retained from the view that created this preview.
    #[must_use]
    pub const fn qualification(&self) -> TextLayoutQualification {
        self.qualification
    }
}

/// Failure to select, interpret, or plan one supported configuration string.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum TextError {
    /// The string is not one of [`TextSetting::all`]'s stable keys.
    #[error("unknown text setting {value:?}")]
    UnknownSetting {
        /// Rejected spelling.
        value: String,
    },
    /// The registry no longer contains the supported field.
    #[error("generated registry is missing the field for {setting}")]
    RegistryFieldMissing {
        /// Selected setting.
        setting: TextSetting,
    },
    /// The registry no longer describes a non-blob fixed string.
    #[error("generated registry does not describe {setting} as an editable fixed string")]
    RegistryFieldNotText {
        /// Selected setting.
        setting: TextSetting,
    },
    /// The caller's explicit scope disagrees with the generated descriptor.
    #[error("{setting} requires {expected:?} scope, got {actual:?}")]
    ScopeMismatch {
        /// Selected setting.
        setting: TextSetting,
        /// Generated scope requirement.
        expected: TextScopeKind,
        /// Scope supplied by the caller.
        actual: TextScopeKind,
    },
    /// The firmware provenance does not exactly match the registry target.
    #[error(
        "firmware {actual} does not match text-layout targets {accepted:?}; only explicit unqualified offline interpretation is available"
    )]
    UnsupportedFirmware {
        /// Firmware identity supplied by the caller.
        actual: FirmwareIdentity,
        /// Exact identities accepted by the registry.
        accepted: &'static [&'static str],
    },
    /// An embedded control character cannot be used as editable text.
    #[error("{setting} contains control character {character:?}")]
    ControlCharacter {
        /// Selected setting.
        setting: TextSetting,
        /// Rejected character, including NUL and terminal control characters.
        character: char,
    },
    /// Stored text is not valid UTF-8.
    #[error("{setting} is not valid UTF-8: {source}")]
    InvalidUtf8 {
        /// Selected setting.
        setting: TextSetting,
        /// Exact decoding failure.
        source: std::str::Utf8Error,
    },
    /// Generated address, encoding, byte-limit, or patch validation failed.
    #[error(transparent)]
    Schema(#[from] SchemaError),
}

fn resolve_scope(setting: TextSetting, scope: TextScope) -> Result<TextMetadata, TextError> {
    let metadata = setting.metadata()?;
    if metadata.scope != scope.kind() {
        return Err(TextError::ScopeMismatch {
            setting,
            expected: metadata.scope,
            actual: scope.kind(),
        });
    }
    Ok(metadata)
}

fn decode(setting: TextSetting, metadata: TextMetadata, bytes: &[u8]) -> Result<String, TextError> {
    let end = bytes
        .iter()
        .rposition(|byte| *byte != metadata.padding)
        .map_or(0, |index| index + 1);
    let meaningful = bytes.get(..end).unwrap_or_default();
    if metadata.encoding == StringEncoding::MemoryMap
        && let Some(value) = meaningful
            .iter()
            .copied()
            .find(|byte| !byte.is_ascii_graphic() && *byte != b' ')
    {
        return Err(SchemaError::TextByte {
            field: metadata.field_name,
            encoding: "MemoryMap",
            value,
        }
        .into());
    }
    let text = std::str::from_utf8(meaningful)
        .map_err(|source| TextError::InvalidUtf8 { setting, source })?;
    validate_characters(setting, text)?;
    Ok(text.to_owned())
}

fn validate_characters(setting: TextSetting, value: &str) -> Result<(), TextError> {
    if let Some(character) = value.chars().find(|character| character.is_control()) {
        return Err(TextError::ControlCharacter { setting, character });
    }
    Ok(())
}

#[cfg(test)]
#[path = "text_tests.rs"]
mod tests;
