//! Exact CAT identities reported by a TH-D75.

use std::fmt;
use std::str::FromStr;

use crate::error::ValidationError;

/// Radio model accepted by this TH-D75-specific library.
///
/// The `ID` response is not free-form product text. A connection is a TH-D75
/// connection only when the radio returns the exact `TH-D75` identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RadioModel {
    /// Kenwood TH-D75A or TH-D75E. CAT does not distinguish the regional
    /// suffix in its `ID` response.
    ThD75,
}

impl RadioModel {
    /// Exact CAT `ID` payload for a TH-D75.
    pub const TH_D75_ID: &str = "TH-D75";

    /// Return the exact CAT model identity.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ThD75 => Self::TH_D75_ID,
        }
    }
}

impl TryFrom<&str> for RadioModel {
    type Error = ValidationError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            Self::TH_D75_ID => Ok(Self::ThD75),
            _ => Err(ValidationError::UnsupportedRadioModel {
                model: value.to_owned(),
            }),
        }
    }
}

impl TryFrom<String> for RadioModel {
    type Error = ValidationError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_from(value.as_str())
    }
}

impl FromStr for RadioModel {
    type Err = ValidationError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::try_from(value)
    }
}

impl fmt::Display for RadioModel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl AsRef<str> for RadioModel {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

/// Exact, bounded token returned by the CAT `FV` command.
///
/// Known values include `1.03`, `1.03.000`, and `1.03.AZM`. Unknown tokens
/// remain representable so policy can reject them explicitly instead of
/// confusing an unfamiliar but well-formed firmware with malformed wire
/// data. The CAT field is one to eight visible ASCII bytes with no trimming
/// or normalization.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FirmwareIdentity(String);

impl FirmwareIdentity {
    /// Maximum number of bytes in the CAT firmware identity field.
    pub const MAX_LEN: usize = 8;

    /// Validate and copy an exact CAT firmware identity.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::FirmwareIdentityLength`] for an empty or
    /// overlong token. Returns [`ValidationError::InvalidFirmwareIdentityByte`]
    /// when a byte is not visible seven-bit ASCII.
    pub fn new(value: &str) -> Result<Self, ValidationError> {
        Self::try_from(value.to_owned())
    }

    /// Return the exact CAT payload without normalization.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Return the encoded byte length.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.0.len()
    }

    /// Return whether the identity is empty.
    ///
    /// A constructed firmware identity is never empty; this method makes the
    /// invariant explicit for generic string-like code.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        false
    }
}

impl TryFrom<String> for FirmwareIdentity {
    type Error = ValidationError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if !(1..=Self::MAX_LEN).contains(&value.len()) {
            return Err(ValidationError::FirmwareIdentityLength {
                len: value.len(),
                max: Self::MAX_LEN,
            });
        }
        if let Some((offset, byte)) = value
            .bytes()
            .enumerate()
            .find(|(_, byte)| !byte.is_ascii_graphic())
        {
            return Err(ValidationError::InvalidFirmwareIdentityByte {
                offset,
                value: byte,
            });
        }
        Ok(Self(value))
    }
}

impl TryFrom<&str> for FirmwareIdentity {
    type Error = ValidationError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl FromStr for FirmwareIdentity {
    type Err = ValidationError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl fmt::Display for FirmwareIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl AsRef<str> for FirmwareIdentity {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

/// Exact eight-byte serial number returned by the CAT `AE` command.
///
/// The independent TH-D75 CAT specification defines this as an eight-character
/// field. The radio has been observed returning values such as `C3C10368` and
/// `C5310165`. This type deliberately does not infer an undocumented serial
/// number grammar from those examples: every fixed-width printable-ASCII value
/// that can occupy one comma-separated CAT field remains representable.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SerialNumber(String);

impl SerialNumber {
    /// Exact encoded width of an `AE` serial-number field.
    pub const WIRE_LEN: usize = 8;

    /// Validate and copy a radio serial number.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::IdentityFieldLength`] unless `value` is
    /// exactly eight bytes. Returns
    /// [`ValidationError::InvalidIdentityFieldByte`] for a control byte,
    /// non-ASCII byte, or comma.
    pub fn new(value: &str) -> Result<Self, ValidationError> {
        validate_identity_field(value, "serial number", Self::WIRE_LEN)?;
        Ok(Self(value.to_owned()))
    }

    /// Return the exact serial number without normalization.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<&str> for SerialNumber {
    type Error = ValidationError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl TryFrom<String> for SerialNumber {
    type Error = ValidationError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        validate_identity_field(&value, "serial number", Self::WIRE_LEN)?;
        Ok(Self(value))
    }
}

impl FromStr for SerialNumber {
    type Err = ValidationError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl fmt::Display for SerialNumber {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl AsRef<str> for SerialNumber {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

/// Exact three-byte model code returned alongside [`SerialNumber`] by `AE`.
///
/// Hardware observations include `K01`, but no retained evidence proves that
/// every model code follows that example's letter-plus-digits shape. The value
/// therefore remains an opaque, fixed-width printable-ASCII CAT field.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ModelCode(String);

impl ModelCode {
    /// Exact encoded width of an `AE` model-code field.
    pub const WIRE_LEN: usize = 3;

    /// Validate and copy a radio model code.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::IdentityFieldLength`] unless `value` is
    /// exactly three bytes. Returns
    /// [`ValidationError::InvalidIdentityFieldByte`] for a control byte,
    /// non-ASCII byte, or comma.
    pub fn new(value: &str) -> Result<Self, ValidationError> {
        validate_identity_field(value, "model code", Self::WIRE_LEN)?;
        Ok(Self(value.to_owned()))
    }

    /// Return the exact model code without normalization.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<&str> for ModelCode {
    type Error = ValidationError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl TryFrom<String> for ModelCode {
    type Error = ValidationError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        validate_identity_field(&value, "model code", Self::WIRE_LEN)?;
        Ok(Self(value))
    }
}

impl FromStr for ModelCode {
    type Err = ValidationError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl fmt::Display for ModelCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl AsRef<str> for ModelCode {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

/// Complete typed response to the CAT `AE` radio-serial-information query.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SerialInformation {
    serial_number: SerialNumber,
    model_code: ModelCode,
}

impl SerialInformation {
    /// Combine the two independently validated `AE` fields.
    #[must_use]
    pub const fn new(serial_number: SerialNumber, model_code: ModelCode) -> Self {
        Self {
            serial_number,
            model_code,
        }
    }

    /// Return the radio's exact serial number.
    #[must_use]
    pub const fn serial_number(&self) -> &SerialNumber {
        &self.serial_number
    }

    /// Return the opaque three-byte model code.
    #[must_use]
    pub const fn model_code(&self) -> &ModelCode {
        &self.model_code
    }

    /// Split this value into its two validated identity fields.
    #[must_use]
    pub fn into_parts(self) -> (SerialNumber, ModelCode) {
        (self.serial_number, self.model_code)
    }
}

/// Market/region code returned by the CAT `TY` command.
///
/// The wire value is `E`, `J`, or `K` for the established Europe, Japan, and
/// United States model regions. `Other` preserves the radio's explicit `0`
/// fallback without claiming a more specific market.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RadioRegion {
    /// Firmware fallback code `0` for a market other than E, J, or K.
    Other,
    /// European market code `E`.
    Europe,
    /// Japanese market code `J`.
    Japan,
    /// United States market code `K`.
    UnitedStates,
}

impl RadioRegion {
    /// Return the exact one-byte CAT region code.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Other => "0",
            Self::Europe => "E",
            Self::Japan => "J",
            Self::UnitedStates => "K",
        }
    }
}

impl TryFrom<&str> for RadioRegion {
    type Error = ValidationError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "0" => Ok(Self::Other),
            "E" => Ok(Self::Europe),
            "J" => Ok(Self::Japan),
            "K" => Ok(Self::UnitedStates),
            _ => Err(ValidationError::UnknownRadioRegion {
                code: value.to_owned(),
            }),
        }
    }
}

impl FromStr for RadioRegion {
    type Err = ValidationError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::try_from(value)
    }
}

impl fmt::Display for RadioRegion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl AsRef<str> for RadioRegion {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

/// Opaque hardware-variant nibble returned by the CAT `TY` command.
///
/// Firmware proves only that this is one uppercase hexadecimal digit. Values
/// `0` through `F` therefore remain representable; observed value `2` is not
/// promoted to a semantic enum whose other variants would be guesses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HardwareVariant(u8);

impl HardwareVariant {
    /// Largest value representable by the one-digit hexadecimal field.
    pub const MAX: u8 = 0x0F;

    /// Construct a hardware variant from its numeric value.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::HardwareVariantOutOfRange`] when `value`
    /// does not fit one hexadecimal digit.
    pub const fn new(value: u8) -> Result<Self, ValidationError> {
        if value <= Self::MAX {
            Ok(Self(value))
        } else {
            Err(ValidationError::HardwareVariantOutOfRange { value })
        }
    }

    /// Return the numeric value represented by the CAT hexadecimal digit.
    #[must_use]
    pub const fn as_raw(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for HardwareVariant {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<HardwareVariant> for u8 {
    fn from(value: HardwareVariant) -> Self {
        value.as_raw()
    }
}

impl fmt::Display for HardwareVariant {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:X}", self.0)
    }
}

/// Complete typed response to the CAT `TY` radio-type query.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RadioType {
    region: RadioRegion,
    hardware_variant: HardwareVariant,
}

impl RadioType {
    /// Combine the two independently validated `TY` fields.
    #[must_use]
    pub const fn new(region: RadioRegion, hardware_variant: HardwareVariant) -> Self {
        Self {
            region,
            hardware_variant,
        }
    }

    /// Return the radio's market/region code.
    #[must_use]
    pub const fn region(self) -> RadioRegion {
        self.region
    }

    /// Return the opaque hardware-variant nibble.
    #[must_use]
    pub const fn hardware_variant(self) -> HardwareVariant {
        self.hardware_variant
    }

    /// Split this value into its validated region and variant fields.
    #[must_use]
    pub const fn into_parts(self) -> (RadioRegion, HardwareVariant) {
        (self.region, self.hardware_variant)
    }
}

fn validate_identity_field(
    value: &str,
    field: &'static str,
    expected: usize,
) -> Result<(), ValidationError> {
    if value.len() != expected {
        return Err(ValidationError::IdentityFieldLength {
            field,
            actual: value.len(),
            expected,
        });
    }

    if let Some((offset, value)) = value
        .bytes()
        .enumerate()
        .find(|(_, value)| !(b' '..=b'~').contains(value) || *value == b',')
    {
        return Err(ValidationError::InvalidIdentityFieldByte {
            field,
            offset,
            value,
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn model_accepts_only_the_exact_th_d75_identity() -> TestResult {
        assert_eq!(RadioModel::try_from("TH-D75")?, RadioModel::ThD75);
        assert_eq!(RadioModel::ThD75.to_string(), "TH-D75");

        for rejected in ["", "TH-D74", "TH-D75 ", "th-d75"] {
            assert!(
                RadioModel::try_from(rejected).is_err(),
                "accepted non-TH-D75 identity {rejected:?}"
            );
        }
        Ok(())
    }

    #[test]
    fn firmware_identity_preserves_known_and_unknown_tokens() -> TestResult {
        for value in ["1.03", "1.03.000", "1.03.AZM", "1.04"] {
            let identity = FirmwareIdentity::new(value)?;
            assert_eq!(identity.as_str(), value);
            assert_eq!(identity.len(), value.len());
            assert_eq!(identity.to_string(), value);
        }
        Ok(())
    }

    #[test]
    fn firmware_identity_rejects_malformed_wire_values() {
        for rejected in ["", "123456789", " 1.03", "1.03 ", "1\n03", "1.0é"] {
            assert!(
                FirmwareIdentity::new(rejected).is_err(),
                "accepted malformed firmware identity {rejected:?}"
            );
        }
    }

    #[test]
    fn serial_information_preserves_every_proven_field_value() -> TestResult {
        let information =
            SerialInformation::new(SerialNumber::new("C3C10368")?, ModelCode::new("K01")?);
        assert_eq!(information.serial_number().as_str(), "C3C10368");
        assert_eq!(information.model_code().as_str(), "K01");

        let opaque = SerialInformation::new(SerialNumber::new("A B!~123")?, ModelCode::new(" ?~")?);
        assert_eq!(opaque.into_parts().0.as_str(), "A B!~123");
        Ok(())
    }

    #[test]
    fn serial_information_rejects_wrong_width_and_cat_delimiters() {
        for value in [
            "",
            "1234567",
            "123456789",
            "1234,678",
            "1234\r678",
            "123456é",
        ] {
            assert!(
                SerialNumber::new(value).is_err(),
                "accepted malformed serial number {value:?}"
            );
        }
        for value in ["", "K0", "K001", "K,1", "K\r1", "é01"] {
            assert!(
                ModelCode::new(value).is_err(),
                "accepted malformed model code {value:?}"
            );
        }
    }

    #[test]
    fn radio_region_is_exact_and_exhaustive_over_firmware_outputs() -> TestResult {
        for (wire, expected) in [
            ("0", RadioRegion::Other),
            ("E", RadioRegion::Europe),
            ("J", RadioRegion::Japan),
            ("K", RadioRegion::UnitedStates),
        ] {
            let region = RadioRegion::try_from(wire)?;
            assert_eq!(region, expected);
            assert_eq!(region.as_str(), wire);
            assert_eq!(region.to_string(), wire);
        }

        for rejected in ["", "k", "X", "KK"] {
            assert!(
                RadioRegion::try_from(rejected).is_err(),
                "accepted impossible firmware region {rejected:?}"
            );
        }
        Ok(())
    }

    #[test]
    fn hardware_variant_preserves_the_whole_hex_nibble_domain() -> TestResult {
        for value in 0..=HardwareVariant::MAX {
            let variant = HardwareVariant::new(value)?;
            assert_eq!(variant.as_raw(), value);
            assert_eq!(u8::from(variant), value);
            assert_eq!(variant.to_string(), format!("{value:X}"));
        }
        assert!(HardwareVariant::new(0x10).is_err());
        Ok(())
    }
}
