//! Offline interpretation of the settings relevant to Reflector Terminal mode.
//!
//! This adapter performs no I/O and produces no patch. Every value and finding
//! describes the supplied image bytes; the radio's current state is never
//! consulted.

use std::fmt;

use super::{
    DecodedFieldValue, FieldCodec, FieldDescriptor, MemoryImage, StringEncoding,
    TextLayoutQualification, is_supported_schema_target, menu_field,
};
use crate::error::SchemaError;
use crate::types::{FirmwareIdentity, RadioModel, SlotIndex};

// The image-format marker is not a menu setting and has no generated menu entry.
const FORMAT: FieldDescriptor =
    FieldDescriptor::new("format.Version", 10, FieldCodec::Byte { min: 0, max: 0 });

/// USB connector requested for an offline Reflector Terminal comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalUsbRoute {
    /// Main-unit USB connector.
    MainUnit,
    /// Control-panel USB connector.
    ControlPanel,
}

impl fmt::Display for TerminalUsbRoute {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::MainUnit => "USB (Main Unit)",
            Self::ControlPanel => "USB (Panel)",
        })
    }
}

/// Captured Menu 980 USB function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalUsbFunction {
    /// Virtual serial interface; USB audio is available on the main unit only.
    ComAndAudio,
    /// Mass-storage function, not the serial interface requested here.
    MassStorage,
}

impl fmt::Display for TerminalUsbFunction {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::ComAndAudio => "COM+AF In/Out",
            Self::MassStorage => "Mass Storage",
        })
    }
}

/// Captured Menu 986 DV Gateway interface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalGatewayRoute {
    /// Main-unit USB connector.
    MainUnit,
    /// Control-panel USB connector.
    ControlPanel,
    /// Bluetooth interface.
    Bluetooth,
}

impl TerminalGatewayRoute {
    const fn matches(self, desired: TerminalUsbRoute) -> bool {
        matches!(
            (self, desired),
            (Self::MainUnit, TerminalUsbRoute::MainUnit)
                | (Self::ControlPanel, TerminalUsbRoute::ControlPanel)
        )
    }
}

impl fmt::Display for TerminalGatewayRoute {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::MainUnit => "USB (Main Unit)",
            Self::ControlPanel => "USB (Panel)",
            Self::Bluetooth => "Bluetooth",
        })
    }
}

/// Captured Menu 650 DV Gateway mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalGatewayMode {
    /// DV Gateway was stored as Off.
    Off,
    /// DV Gateway was stored as Direct Mode.
    Direct,
    /// DV Gateway was stored as Terminal Mode.
    Terminal,
}

impl fmt::Display for TerminalGatewayMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Off => "Off",
            Self::Direct => "Direct Mode",
            Self::Terminal => "Terminal Mode",
        })
    }
}

/// Captured Menu 670 Terminal subtype.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalMode {
    /// Reflector Terminal mode.
    Reflector,
    /// Repeater Terminal mode.
    Repeater,
}

impl fmt::Display for TerminalMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Reflector => "Reflector TERM Mode",
            Self::Repeater => "Repeater TERM Mode",
        })
    }
}

/// Index within one PM slot's six Menu 651 MY entries, not a PM slot index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalMyCallsignIndex(u8);

impl TerminalMyCallsignIndex {
    /// Zero-based index, always in `0..6`.
    #[must_use]
    pub const fn index(self) -> u8 {
        self.0
    }
}

impl fmt::Display for TerminalMyCallsignIndex {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0 + 1)
    }
}

/// A stored setting that blocks Reflector Terminal use or warrants review.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalFinding {
    /// The selected MY entry is empty or contains only spaces.
    MissingMyCallsign,
    /// The MY entry lacks the manual's recommended eighth-position letter.
    MyCallsignSuffixNeedsReview,
    /// The stored gateway route differs from the requested USB connector.
    RouteMismatch,
    /// Menu 980 selects mass storage instead of serial communication.
    MassStorage,
    /// Menu 670 selects Repeater instead of Reflector Terminal.
    WrongTerminalMode,
    /// The requested PM slot was not the captured active PM selection.
    DifferentCapturedActivePm,
    /// Menu 650 was captured as Direct or Terminal, not Off.
    GatewayAlreadyActive,
}

impl fmt::Display for TerminalFinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::MissingMyCallsign => "selected Menu 651 MY callsign is empty",
            Self::MyCallsignSuffixNeedsReview => {
                "Menu 651 MY callsign suffix needs operator review"
            }
            Self::RouteMismatch => "Menu 986 differs from the requested USB connector",
            Self::MassStorage => "Menu 980 selects Mass Storage, not serial communication",
            Self::WrongTerminalMode => "Menu 670 selects Repeater, not Reflector Terminal",
            Self::DifferentCapturedActivePm => "requested PM differs from captured active PM",
            Self::GatewayAlreadyActive => {
                "Menu 650 was captured as active; current mode is unknown"
            }
        })
    }
}

/// Typed interpretation of historical Reflector Terminal settings.
///
/// The caller supplies image coverage and firmware provenance. For sparse
/// captures, cover every [`Self::required_fields`] descriptor with actual
/// captured bytes before constructing a dense image; a filled gap decodes as
/// though it had been read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReflectorTerminalPreflight {
    /// Caller-supplied firmware identity, retained without rewriting.
    pub firmware: FirmwareIdentity,
    /// Exact label match or explicitly unqualified software interpretation.
    pub qualification: TextLayoutQualification,
    /// Requested zero-based PM slot: 0 is PM Off; 1–5 are PM 1–5.
    pub slot: SlotIndex,
    /// Active PM selection read from the image.
    pub captured_active_slot: SlotIndex,
    /// Requested connector used only for the offline comparison.
    pub desired_route: TerminalUsbRoute,
    /// Supported image format marker; successful decoding always returns zero.
    pub format_version: u8,
    /// Captured Menu 980 value in the requested PM slot.
    pub usb_function: TerminalUsbFunction,
    /// Captured Menu 986 value in the requested PM slot.
    pub gateway_route: TerminalGatewayRoute,
    /// Captured Menu 650 value.
    pub gateway_mode: TerminalGatewayMode,
    /// Captured Menu 670 value in the requested PM slot.
    pub terminal_mode: TerminalMode,
    /// Index within the requested PM slot's six MY entries, not a PM slot index.
    pub selected_my_callsign: TerminalMyCallsignIndex,
    /// Selected MY text, preserving spaces and removing trailing NUL padding only.
    pub my_callsign: String,
    /// Captured Menu 671 text; no route value is substituted automatically.
    pub rpt1: String,
    /// Captured Menu 672 text; no route value is substituted automatically.
    pub rpt2: String,
    /// Findings in the fixed order of the checks that produce them.
    pub findings: Vec<TerminalFinding>,
}

impl ReflectorTerminalPreflight {
    /// Every descriptor required to interpret any selected MY entry.
    ///
    /// Includes all six MY strings because their selector is itself captured
    /// data. Resolve each descriptor with `Some(slot)`; the global format and PM
    /// selector are unchanged by that argument. Memo fields are not read.
    ///
    /// # Errors
    ///
    /// Returns an error if a required generated field is missing or its storage
    /// shape no longer matches this adapter. All menu addresses come from the
    /// generated registry; only the non-menu image-format marker is separate.
    pub fn required_fields() -> Result<Vec<&'static FieldDescriptor>, TerminalPreflightError> {
        let mut result = vec![&FORMAT];
        for field in Field::SCALARS.into_iter().chain([Field::Rpt1, Field::Rpt2]) {
            result.push(field.descriptor()?);
        }
        for index in 0..6 {
            result.push(Field::MyCallsign(index).descriptor()?);
        }
        Ok(result)
    }

    /// Interpret settings, requiring `firmware` to match a registry target label.
    ///
    /// Accepted labels are [`super::MCP_D750_SCHEMA_FIRMWARE_IDENTITIES`]; the
    /// result carries [`TextLayoutQualification::RegistryTargetMatched`].
    ///
    /// # Errors
    ///
    /// Rejects an unsupported firmware label, unknown image format or scalar,
    /// changed registry shape, invalid UTF-8, or unsupported text spelling.
    pub fn read(
        image: &MemoryImage,
        firmware: &FirmwareIdentity,
        slot: SlotIndex,
        desired_route: TerminalUsbRoute,
    ) -> Result<Self, TerminalPreflightError> {
        if !is_supported_schema_target(RadioModel::TmD750, firmware) {
            return Err(TerminalPreflightError::UnsupportedFirmware {
                actual: firmware.clone(),
            });
        }
        Self::decode(
            image,
            firmware,
            slot,
            desired_route,
            TextLayoutQualification::RegistryTargetMatched,
        )
    }

    /// Interpret the image without the firmware check applied by [`Self::read`].
    ///
    /// The result carries [`TextLayoutQualification::UnqualifiedInterpretation`]
    /// even when the supplied label matches a registry target.
    ///
    /// # Errors
    ///
    /// Rejects unknown formats or scalars, changed registry shape, invalid UTF-8,
    /// and unsupported spelling just as [`Self::read`] does.
    pub fn interpret_unqualified(
        image: &MemoryImage,
        firmware: &FirmwareIdentity,
        slot: SlotIndex,
        desired_route: TerminalUsbRoute,
    ) -> Result<Self, TerminalPreflightError> {
        Self::decode(
            image,
            firmware,
            slot,
            desired_route,
            TextLayoutQualification::UnqualifiedInterpretation,
        )
    }

    fn decode(
        image: &MemoryImage,
        firmware: &FirmwareIdentity,
        slot: SlotIndex,
        desired_route: TerminalUsbRoute,
        qualification: TextLayoutQualification,
    ) -> Result<Self, TerminalPreflightError> {
        let format_version = read_byte(&FORMAT, "image format", image, slot)?;
        if format_version != 0 {
            return Err(TerminalPreflightError::UnsupportedFormatVersion {
                actual: format_version,
            });
        }
        let active = Field::ActivePm.byte(image, slot)?;
        let captured_active_slot =
            SlotIndex::new(active).map_err(|_| Field::ActivePm.unknown(active))?;
        let usb_function = match Field::UsbFunction.byte(image, slot)? {
            0 => TerminalUsbFunction::ComAndAudio,
            1 => TerminalUsbFunction::MassStorage,
            value => return Err(Field::UsbFunction.unknown(value)),
        };
        let gateway_route = match Field::GatewayRoute.byte(image, slot)? {
            0 => TerminalGatewayRoute::MainUnit,
            1 => TerminalGatewayRoute::ControlPanel,
            2 => TerminalGatewayRoute::Bluetooth,
            value => return Err(Field::GatewayRoute.unknown(value)),
        };
        let gateway_mode = match Field::GatewayMode.byte(image, slot)? {
            0 => TerminalGatewayMode::Off,
            1 => TerminalGatewayMode::Direct,
            2 => TerminalGatewayMode::Terminal,
            value => return Err(Field::GatewayMode.unknown(value)),
        };
        let terminal_mode = match Field::TerminalMode.byte(image, slot)? {
            0 => TerminalMode::Reflector,
            1 => TerminalMode::Repeater,
            value => return Err(Field::TerminalMode.unknown(value)),
        };
        let index = Field::MySelection.byte(image, slot)?;
        if index >= 6 {
            return Err(Field::MySelection.unknown(index));
        }
        let selected_my_callsign = TerminalMyCallsignIndex(index);
        let my_callsign = Field::MyCallsign(index).text(image, slot)?;
        let rpt1 = Field::Rpt1.text(image, slot)?;
        let rpt2 = Field::Rpt2.text(image, slot)?;
        let mut result = Self {
            firmware: firmware.clone(),
            qualification,
            slot,
            captured_active_slot,
            desired_route,
            format_version,
            usb_function,
            gateway_route,
            gateway_mode,
            terminal_mode,
            selected_my_callsign,
            my_callsign,
            rpt1,
            rpt2,
            findings: Vec::new(),
        };
        result.findings = result.compare();
        Ok(result)
    }

    fn compare(&self) -> Vec<TerminalFinding> {
        let mut findings = Vec::new();
        if self.my_callsign.bytes().all(|byte| byte == b' ') {
            findings.push(TerminalFinding::MissingMyCallsign);
        } else if !self.my_callsign.as_bytes().get(7).is_some_and(|suffix| {
            suffix.is_ascii_uppercase() && !matches!(suffix, b'G' | b'I' | b'S')
        }) {
            findings.push(TerminalFinding::MyCallsignSuffixNeedsReview);
        }
        if !self.gateway_route.matches(self.desired_route) {
            findings.push(TerminalFinding::RouteMismatch);
        }
        if self.usb_function == TerminalUsbFunction::MassStorage {
            findings.push(TerminalFinding::MassStorage);
        }
        if self.terminal_mode != TerminalMode::Reflector {
            findings.push(TerminalFinding::WrongTerminalMode);
        }
        if self.slot != self.captured_active_slot {
            findings.push(TerminalFinding::DifferentCapturedActivePm);
        }
        if self.gateway_mode != TerminalGatewayMode::Off {
            findings.push(TerminalFinding::GatewayAlreadyActive);
        }
        findings
    }
}

/// Failure to interpret historical Terminal settings.
///
/// [`fmt::Display`] uses menu labels; the underlying schema error stays
/// available through [`std::error::Error::source`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum TerminalPreflightError {
    /// Supplied provenance does not match an accepted registry target label.
    #[error("firmware {actual} requires explicit unqualified offline interpretation")]
    UnsupportedFirmware {
        /// Original supplied firmware identity.
        actual: FirmwareIdentity,
    },
    /// The image-format marker is not the supported zero value.
    #[error("unsupported captured image format {actual}")]
    UnsupportedFormatVersion {
        /// Exact captured byte.
        actual: u8,
    },
    /// A menu selector has no supported typed interpretation.
    #[error("{field} has unknown captured value {value}")]
    UnknownScalar {
        /// Stable menu or PM label, not an internal member name.
        field: &'static str,
        /// Exact captured byte, preserved without normalization.
        value: u8,
    },
    /// A required generated field is absent or changed storage shape.
    #[error("{field} does not have a supported registry descriptor")]
    RegistryField {
        /// Stable menu or PM label.
        field: &'static str,
    },
    /// A descriptor could not be addressed or decoded.
    #[error("{field} could not be decoded from the supplied image")]
    FieldDecode {
        /// Stable menu or PM label.
        field: &'static str,
        /// Original address or codec error.
        #[source]
        source: SchemaError,
    },
    /// Stored text is not valid UTF-8.
    #[error("{field} contains invalid UTF-8")]
    InvalidUtf8 {
        /// Stable menu label.
        field: &'static str,
        /// Exact decoding failure.
        #[source]
        source: std::str::Utf8Error,
    },
    /// Text contains something other than uppercase ASCII, digits, or spaces.
    #[error("{field} contains unsupported character {character:?}")]
    UnsafeText {
        /// Stable menu label.
        field: &'static str,
        /// Exact rejected character; no replacement decoding is performed.
        character: char,
    },
}

#[derive(Debug, Clone, Copy)]
enum Field {
    ActivePm,
    UsbFunction,
    GatewayRoute,
    GatewayMode,
    MySelection,
    TerminalMode,
    MyCallsign(u8),
    Rpt1,
    Rpt2,
}

impl Field {
    const SCALARS: [Self; 6] = [
        Self::ActivePm,
        Self::UsbFunction,
        Self::GatewayRoute,
        Self::GatewayMode,
        Self::MySelection,
        Self::TerminalMode,
    ];

    const fn label(self) -> &'static str {
        match self {
            Self::ActivePm => "captured active PM",
            Self::UsbFunction => "Menu 980 USB function",
            Self::GatewayRoute => "Menu 986 DV Gateway interface",
            Self::GatewayMode => "Menu 650 DV Gateway mode",
            Self::MySelection => "Menu 651 MY selection",
            Self::TerminalMode => "Menu 670 Terminal subtype",
            Self::MyCallsign(_) => "Menu 651 MY callsign",
            Self::Rpt1 => "Menu 671 RPT1",
            Self::Rpt2 => "Menu 672 RPT2",
        }
    }

    fn descriptor(self) -> Result<&'static FieldDescriptor, TerminalPreflightError> {
        let name = match self {
            Self::ActivePm => "pm.PmSelect",
            Self::UsbFunction => "radio.UsbFunction",
            Self::GatewayRoute => "radio.DvGatewayInterface",
            Self::GatewayMode => "dv.DvGatewayModeDvGateway",
            Self::MySelection => "dv.MyCallsignSelectDvGateway",
            Self::TerminalMode => "dv.SelectTerminalMode",
            Self::MyCallsign(0) => "dv.MyCallsignDvGatewayList[0].MyCallsignDvGateway",
            Self::MyCallsign(1) => "dv.MyCallsignDvGatewayList[1].MyCallsignDvGateway",
            Self::MyCallsign(2) => "dv.MyCallsignDvGatewayList[2].MyCallsignDvGateway",
            Self::MyCallsign(3) => "dv.MyCallsignDvGatewayList[3].MyCallsignDvGateway",
            Self::MyCallsign(4) => "dv.MyCallsignDvGatewayList[4].MyCallsignDvGateway",
            Self::MyCallsign(5) => "dv.MyCallsignDvGatewayList[5].MyCallsignDvGateway",
            Self::MyCallsign(value) => return Err(Self::MySelection.unknown(value)),
            Self::Rpt1 => "dv.RPT1DvGateway",
            Self::Rpt2 => "dv.RPT2DvGateway",
        };
        let field = menu_field(name).ok_or_else(|| self.registry_error())?;
        let descriptor = &field.descriptor;
        let expected_slot = !matches!(self, Self::ActivePm);
        let expected_codec = match self {
            Self::MyCallsign(_) | Self::Rpt1 | Self::Rpt2 => matches!(
                descriptor.codec,
                FieldCodec::FixedString {
                    len: 8,
                    encoding: StringEncoding::Utf8,
                    padding: 0,
                }
            ),
            _ => matches!(descriptor.codec, FieldCodec::Byte { .. }),
        };
        if field.is_blob || descriptor.is_per_slot() != expected_slot || !expected_codec {
            return Err(self.registry_error());
        }
        Ok(descriptor)
    }

    const fn registry_error(self) -> TerminalPreflightError {
        TerminalPreflightError::RegistryField {
            field: self.label(),
        }
    }

    const fn unknown(self, value: u8) -> TerminalPreflightError {
        TerminalPreflightError::UnknownScalar {
            field: self.label(),
            value,
        }
    }

    fn byte(self, image: &MemoryImage, slot: SlotIndex) -> Result<u8, TerminalPreflightError> {
        read_byte(self.descriptor()?, self.label(), image, slot)
    }

    fn text(self, image: &MemoryImage, slot: SlotIndex) -> Result<String, TerminalPreflightError> {
        let descriptor = self.descriptor()?;
        let address = descriptor.address(Some(slot)).map_err(|source| {
            TerminalPreflightError::FieldDecode {
                field: self.label(),
                source,
            }
        })?;
        let start = address.as_usize();
        let bytes = image
            .as_bytes()
            .get(start..start + descriptor.codec.encoded_len())
            .ok_or_else(|| TerminalPreflightError::FieldDecode {
                field: self.label(),
                source: SchemaError::OutOfBounds {
                    field: descriptor.name,
                    address: u64::from(address.as_u32()),
                    len: descriptor.codec.encoded_len(),
                    image_length: image.as_bytes().len(),
                },
            })?;
        let end = bytes
            .iter()
            .rposition(|byte| *byte != 0)
            .map_or(0, |index| index + 1);
        let meaningful = bytes.get(..end).ok_or_else(|| self.registry_error())?;
        let text = std::str::from_utf8(meaningful).map_err(|source| {
            TerminalPreflightError::InvalidUtf8 {
                field: self.label(),
                source,
            }
        })?;
        if let Some(character) = text.chars().find(|character| {
            !character.is_ascii_uppercase() && !character.is_ascii_digit() && *character != ' '
        }) {
            return Err(TerminalPreflightError::UnsafeText {
                field: self.label(),
                character,
            });
        }
        Ok(text.to_owned())
    }
}

fn read_byte(
    descriptor: &FieldDescriptor,
    label: &'static str,
    image: &MemoryImage,
    slot: SlotIndex,
) -> Result<u8, TerminalPreflightError> {
    let value = image.slot(slot).read(descriptor).map_err(|source| {
        TerminalPreflightError::FieldDecode {
            field: label,
            source,
        }
    })?;
    let DecodedFieldValue::Unsigned(value) = value else {
        return Err(TerminalPreflightError::RegistryField { field: label });
    };
    u8::try_from(value).map_err(|_| TerminalPreflightError::RegistryField { field: label })
}

#[cfg(test)]
#[path = "terminal_tests.rs"]
mod tests;
