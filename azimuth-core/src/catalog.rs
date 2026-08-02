//! Swift-facing view of the generated MCP-D75 setting schema.

use std::collections::BTreeSet;

use kenwood_thd75::memory::schema::StringEncoding;
use kenwood_thd75::memory::{
    FieldCodec, FieldValue, MCP_D75_MENU_FIELDS, MenuField, PatchPlanner, PatchSet, menu_field,
};

/// Top-level MCP-D75 setting group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum SettingMenu {
    /// General radio configuration.
    Radio,
    /// GPS configuration.
    Gps,
    /// APRS configuration.
    Aprs,
    /// D-STAR configuration.
    Dv,
}

impl SettingMenu {
    fn from_schema(value: &str) -> Self {
        match value {
            "gps" => Self::Gps,
            "aprs" => Self::Aprs,
            "dv" => Self::Dv,
            _ => Self::Radio,
        }
    }
}

/// Value shape accepted by one setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum SettingValueKind {
    /// A true or false value.
    Boolean,
    /// One choice from a finite raw-value domain.
    Choice,
    /// An unsigned integer.
    UnsignedInteger,
    /// A signed integer.
    SignedInteger,
    /// Text with a fixed maximum encoded width.
    Text,
    /// An exact-length byte sequence.
    Bytes,
}

/// Character encoding accepted by one fixed-width text setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum SettingTextEncoding {
    /// The field accepts arbitrary UTF-8 whose encoded bytes fit its width.
    Utf8,
    /// The radio's model-dependent memory-map encoding is only safe for ASCII.
    MemoryMapAscii,
}

/// How the app must present a stored setting value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum SettingPresentation {
    /// The typed stored value is also the user-facing value.
    Direct,
    /// The stored integer requires the declared scaling transform.
    ScaledInteger,
    /// Large binary content requires a specialized editor.
    Blob,
}

/// One labeled raw value in a finite setting domain.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct SettingOption {
    /// Raw integer stored by the radio.
    pub raw_value: u64,
    /// English MCP-D75 label when one is available.
    pub label: Option<String>,
    /// Stable source member name for diagnostics and unlabeled choices.
    pub member: String,
}

/// Display-to-storage integer transform declared by the schema.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct SettingStorageTransform {
    /// Unit accepted by the transform.
    pub input_unit: String,
    /// Encoding multiplier in `raw = round(display * numerator / denominator)`.
    pub numerator: i64,
    /// Encoding divisor in `raw = round(display * numerator / denominator)`.
    pub denominator: i64,
    /// Decimal places used when presenting a decoded value.
    pub display_decimal_places: u8,
}

/// Complete app-facing metadata for one writable MCP-D75 field.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct SettingRecord {
    /// Stable identifier used by reads, plans, and writes.
    pub id: String,
    /// Human-readable name derived from the stable identifier.
    pub display_name: String,
    /// Top-level setting group.
    pub menu: SettingMenu,
    /// Accepted value shape.
    pub value_kind: SettingValueKind,
    /// Exact text encoding constraint for fixed strings; absent for non-text fields.
    pub text_encoding: Option<SettingTextEncoding>,
    /// Required presentation path for the raw typed value.
    pub presentation: SettingPresentation,
    /// Absolute byte offset in a raw MCP image.
    pub offset: u64,
    /// Encoded byte width.
    pub byte_length: u64,
    /// Owned bit mask for a packed field.
    pub bit_mask: Option<u8>,
    /// Right shift for a packed integer field.
    pub bit_shift: Option<u8>,
    /// Inclusive unsigned minimum when applicable.
    pub unsigned_min: Option<u64>,
    /// Inclusive unsigned maximum when applicable.
    pub unsigned_max: Option<u64>,
    /// Inclusive signed minimum when applicable.
    pub signed_min: Option<i64>,
    /// Inclusive signed maximum when applicable.
    pub signed_max: Option<i64>,
    /// Qualified enum type from the schema, when applicable.
    pub enum_type: Option<String>,
    /// Finite labeled choices.
    pub options: Vec<SettingOption>,
    /// Finite unlabeled raw choices.
    pub allowed_values: Vec<u64>,
    /// Optional display-to-storage transform.
    pub storage_transform: Option<SettingStorageTransform>,
    /// Whether the value is a large persistent blob.
    pub is_blob: bool,
}

/// Rejected raw/display conversion.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, uniffi::Error)]
pub enum SettingConversionError {
    /// The setting identifier is not in the 400-field catalog.
    #[error("unknown setting identifier {setting_id}")]
    UnknownSetting {
        /// Rejected identifier.
        setting_id: String,
    },
    /// The field does not declare a display scaling transform.
    #[error("setting {setting_id} does not use a scaled display value")]
    NoTransform {
        /// Rejected identifier.
        setting_id: String,
    },
    /// The supplied display number is not finite or cannot be represented.
    #[error("invalid display value for {setting_id}: {detail}")]
    InvalidDisplayValue {
        /// Setting identifier.
        setting_id: String,
        /// Rejection detail.
        detail: String,
    },
    /// The converted raw value lies outside the authoritative field domain.
    #[error("converted value for {setting_id} is outside its domain: {detail}")]
    OutOfRange {
        /// Setting identifier.
        setting_id: String,
        /// Schema validation detail.
        detail: String,
    },
}

/// Typed value read from or proposed for a radio setting.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum SettingValue {
    /// Boolean value.
    Boolean {
        /// Value payload.
        value: bool,
    },
    /// Unsigned integer or choice raw value.
    Unsigned {
        /// Value payload.
        value: u64,
    },
    /// Signed integer value.
    Signed {
        /// Value payload.
        value: i64,
    },
    /// Text value.
    Text {
        /// Value payload.
        value: String,
    },
    /// Exact byte value.
    Bytes {
        /// Value payload.
        value: Vec<u8>,
    },
}

impl SettingValue {
    pub(crate) fn as_field_value(&self) -> FieldValue<'_> {
        match self {
            Self::Boolean { value } => FieldValue::Bool(*value),
            Self::Unsigned { value } => FieldValue::Unsigned(*value),
            Self::Signed { value } => FieldValue::Signed(*value),
            Self::Text { value } => FieldValue::Text(value),
            Self::Bytes { value } => FieldValue::Bytes(value),
        }
    }
}

/// One user-approved optimistic setting mutation.
///
/// `snapshot_id` and `expected_value` must come from the same
/// [`SettingReadResult`](crate::SettingReadResult). The core checks both the
/// typed value and every byte of each cached source page before any write.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct SettingChange {
    /// Stable setting identifier.
    pub setting_id: String,
    /// Snapshot that supplied `expected_value`.
    pub snapshot_id: u64,
    /// Value shown to the user before approval.
    pub expected_value: SettingValue,
    /// Approved value to apply automatically.
    pub desired_value: SettingValue,
}

/// Validation result for one proposed change.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct SettingChangeValidation {
    /// Stable setting identifier supplied by the plan.
    pub setting_id: String,
    /// Whether this individual change has valid types and domains.
    pub accepted: bool,
    /// Rejection detail when `accepted` is false.
    pub detail: Option<String>,
}

/// Pure validation report for a complete proposed setting batch.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct SettingPlanValidation {
    /// Whether the entire batch can be staged.
    pub accepted: bool,
    /// Per-change type and domain results.
    pub changes: Vec<SettingChangeValidation>,
    /// Batch-level rejection such as mixed snapshots or duplicate fields.
    pub batch_error: Option<String>,
}

/// Return all 400 writable MCP-D75 setting records.
///
/// The records are projected directly from `kenwood-thd75`'s generated
/// registry, so the app and Rust write path cannot drift onto separate field
/// definitions.
#[must_use]
#[uniffi::export]
pub fn setting_catalog() -> Vec<SettingRecord> {
    MCP_D75_MENU_FIELDS.iter().map(setting_record).collect()
}

/// Convert a raw stored integer to its declared display unit.
///
/// Current transformed coordinate fields return seconds rounded to one
/// decimal place. All other fields return
/// [`SettingConversionError::NoTransform`] so Swift cannot accidentally label
/// a raw integer as a display value.
///
/// # Errors
///
/// Returns an error for an unknown field, a direct field, or malformed
/// transform metadata.
#[uniffi::export]
pub fn decode_setting_display_value(
    setting_id: String,
    raw_value: u64,
) -> Result<f64, SettingConversionError> {
    let field = transformed_field(&setting_id)?;
    let transform = field
        .storage_transform
        .ok_or_else(|| SettingConversionError::NoTransform {
            setting_id: setting_id.clone(),
        })?;
    if transform.numerator == 0 {
        return Err(SettingConversionError::InvalidDisplayValue {
            setting_id,
            detail: "transform numerator is zero".to_owned(),
        });
    }
    let raw = raw_value.to_string().parse::<f64>().map_err(|error| {
        SettingConversionError::InvalidDisplayValue {
            setting_id: setting_id.clone(),
            detail: error.to_string(),
        }
    })?;
    let numerator = transform
        .numerator
        .to_string()
        .parse::<f64>()
        .map_err(|error| SettingConversionError::InvalidDisplayValue {
            setting_id: setting_id.clone(),
            detail: error.to_string(),
        })?;
    let denominator = transform
        .denominator
        .to_string()
        .parse::<f64>()
        .map_err(|error| SettingConversionError::InvalidDisplayValue {
            setting_id: setting_id.clone(),
            detail: error.to_string(),
        })?;
    Ok((raw * denominator / numerator * 10.0).round() / 10.0)
}

/// Convert a display-unit value to the authoritative raw stored integer.
///
/// This applies `round(display * numerator / denominator)` and then runs the
/// same field-domain validator used by batch execution.
///
/// # Errors
///
/// Returns an error for an unknown or direct field, a non-finite value, or a
/// converted integer outside the field's schema domain.
#[uniffi::export]
pub fn encode_setting_display_value(
    setting_id: String,
    display_value: f64,
) -> Result<u64, SettingConversionError> {
    let field = transformed_field(&setting_id)?;
    let transform = field
        .storage_transform
        .ok_or_else(|| SettingConversionError::NoTransform {
            setting_id: setting_id.clone(),
        })?;
    if !display_value.is_finite() || transform.denominator == 0 {
        return Err(SettingConversionError::InvalidDisplayValue {
            setting_id,
            detail: "value must be finite and the transform divisor must be nonzero".to_owned(),
        });
    }
    let numerator = transform
        .numerator
        .to_string()
        .parse::<f64>()
        .map_err(|error| SettingConversionError::InvalidDisplayValue {
            setting_id: setting_id.clone(),
            detail: error.to_string(),
        })?;
    let denominator = transform
        .denominator
        .to_string()
        .parse::<f64>()
        .map_err(|error| SettingConversionError::InvalidDisplayValue {
            setting_id: setting_id.clone(),
            detail: error.to_string(),
        })?;
    let encoded = (display_value * numerator / denominator).round();
    if encoded < 0.0 {
        return Err(SettingConversionError::InvalidDisplayValue {
            setting_id,
            detail: "converted raw value is negative".to_owned(),
        });
    }
    let raw = format!("{encoded:.0}").parse::<u64>().map_err(|error| {
        SettingConversionError::InvalidDisplayValue {
            setting_id: setting_id.clone(),
            detail: error.to_string(),
        }
    })?;
    validate_value(field, &SettingValue::Unsigned { value: raw })
        .map_err(|detail| SettingConversionError::OutOfRange { setting_id, detail })?;
    Ok(raw)
}

/// Validate a complete setting plan without touching the radio.
///
/// The same planner runs again inside
/// [`AutomationController::apply_setting_changes`](crate::AutomationController::apply_setting_changes)
/// after the user accepts the plan. This function is suitable for rejecting
/// malformed on-device model output before it reaches the review UI.
#[must_use]
#[uniffi::export]
#[expect(
    clippy::needless_pass_by_value,
    reason = "UniFFI sequence arguments cross the Swift boundary as owned vectors"
)]
pub fn validate_setting_changes(changes: Vec<SettingChange>) -> SettingPlanValidation {
    validate_changes(&changes)
}

pub(crate) fn validate_changes(changes: &[SettingChange]) -> SettingPlanValidation {
    let validations: Vec<SettingChangeValidation> = changes
        .iter()
        .map(|change| match validate_change(change) {
            Ok(()) => SettingChangeValidation {
                setting_id: change.setting_id.clone(),
                accepted: true,
                detail: None,
            },
            Err(detail) => SettingChangeValidation {
                setting_id: change.setting_id.clone(),
                accepted: false,
                detail: Some(detail),
            },
        })
        .collect();

    let batch_error = if changes.is_empty() {
        Some("a setting plan must contain at least one change".to_owned())
    } else if validations.iter().any(|result| !result.accepted) {
        Some("one or more setting changes failed schema validation".to_owned())
    } else {
        validate_batch(changes).err()
    };

    SettingPlanValidation {
        accepted: batch_error.is_none(),
        changes: validations,
        batch_error,
    }
}

pub(crate) fn build_patch_plan(
    changes: &[SettingChange],
) -> Result<(PatchSet, Vec<&'static MenuField>), String> {
    let report = validate_changes(changes);
    if let Some(error) = report.batch_error {
        return Err(error);
    }

    let mut planner = PatchPlanner::new();
    let mut fields = Vec::with_capacity(changes.len());
    for change in changes {
        let field = menu_field(&change.setting_id)
            .ok_or_else(|| format!("unknown setting identifier {}", change.setting_id))?;
        field
            .plan_value(&mut planner, change.desired_value.as_field_value())
            .map_err(|error| error.to_string())?;
        fields.push(field);
    }
    let patches = planner.finish().map_err(|error| error.to_string())?;
    Ok((patches, fields))
}

pub(crate) const fn encoded_len(codec: FieldCodec) -> usize {
    match codec {
        FieldCodec::Byte { .. }
        | FieldCodec::Bool
        | FieldCodec::BitBool { .. }
        | FieldCodec::BitField { .. } => 1,
        FieldCodec::FixedString { len, .. } | FieldCodec::Bytes { len } => len,
        FieldCodec::Unsigned { width, .. } | FieldCodec::Signed { width, .. } => width as usize,
    }
}

fn validate_change(change: &SettingChange) -> Result<(), String> {
    let field = menu_field(&change.setting_id)
        .ok_or_else(|| format!("unknown setting identifier {}", change.setting_id))?;
    validate_value(field, &change.expected_value)
        .map_err(|error| format!("invalid expected value: {error}"))?;
    validate_value(field, &change.desired_value)
        .map_err(|error| format!("invalid desired value: {error}"))
}

fn transformed_field(setting_id: &str) -> Result<&'static MenuField, SettingConversionError> {
    let field = menu_field(setting_id).ok_or_else(|| SettingConversionError::UnknownSetting {
        setting_id: setting_id.to_owned(),
    })?;
    if field.storage_transform.is_none() {
        return Err(SettingConversionError::NoTransform {
            setting_id: setting_id.to_owned(),
        });
    }
    Ok(field)
}

fn validate_value(field: &MenuField, value: &SettingValue) -> Result<(), String> {
    let mut planner = PatchPlanner::new();
    field
        .plan_value(&mut planner, value.as_field_value())
        .map_err(|error| error.to_string())?;
    let _patches = planner.finish().map_err(|error| error.to_string())?;
    Ok(())
}

fn validate_batch(changes: &[SettingChange]) -> Result<(), String> {
    let snapshot = changes
        .first()
        .map(|change| change.snapshot_id)
        .ok_or_else(|| "a setting plan must contain at least one change".to_owned())?;
    if snapshot == 0 {
        return Err("setting snapshot identifier zero is invalid".to_owned());
    }
    if changes.iter().any(|change| change.snapshot_id != snapshot) {
        return Err("all setting changes must use the same snapshot".to_owned());
    }

    let mut identifiers = BTreeSet::new();
    for change in changes {
        if !identifiers.insert(change.setting_id.as_str()) {
            return Err(format!(
                "setting {} appears more than once in the batch",
                change.setting_id
            ));
        }
    }

    let mut planner = PatchPlanner::new();
    for change in changes {
        let field = menu_field(&change.setting_id)
            .ok_or_else(|| format!("unknown setting identifier {}", change.setting_id))?;
        field
            .plan_value(&mut planner, change.desired_value.as_field_value())
            .map_err(|error| error.to_string())?;
    }
    let _patches = planner.finish().map_err(|error| error.to_string())?;
    Ok(())
}

fn setting_record(field: &MenuField) -> SettingRecord {
    let codec = field.descriptor.codec;
    let (bit_mask, bit_shift) = match codec {
        FieldCodec::BitBool { mask } => (Some(mask), None),
        FieldCodec::BitField { mask, shift, .. } => (Some(mask), Some(shift)),
        _ => (None, None),
    };
    let (unsigned_min, unsigned_max) = match codec {
        FieldCodec::Byte { min, max } | FieldCodec::BitField { min, max, .. } => {
            (Some(u64::from(min)), Some(u64::from(max)))
        }
        FieldCodec::Unsigned { min, max, .. } => (Some(min), Some(max)),
        _ => (None, None),
    };
    let (signed_min, signed_max) = match codec {
        FieldCodec::Signed { min, max, .. } => (Some(min), Some(max)),
        _ => (None, None),
    };

    SettingRecord {
        id: field.descriptor.name.to_owned(),
        display_name: humanize_identifier(field.descriptor.name),
        menu: SettingMenu::from_schema(field.menu),
        value_kind: value_kind(field),
        text_encoding: match codec {
            FieldCodec::FixedString {
                encoding: StringEncoding::Utf8,
                ..
            } => Some(SettingTextEncoding::Utf8),
            FieldCodec::FixedString {
                encoding: StringEncoding::MemoryMap,
                ..
            } => Some(SettingTextEncoding::MemoryMapAscii),
            _ => None,
        },
        presentation: if field.is_blob {
            SettingPresentation::Blob
        } else if field.storage_transform.is_some() {
            SettingPresentation::ScaledInteger
        } else {
            SettingPresentation::Direct
        },
        offset: u64::try_from(field.descriptor.offset).unwrap_or(u64::MAX),
        byte_length: u64::try_from(encoded_len(codec)).unwrap_or(u64::MAX),
        bit_mask,
        bit_shift,
        unsigned_min,
        unsigned_max,
        signed_min,
        signed_max,
        enum_type: field.enum_type.map(str::to_owned),
        options: field
            .options
            .iter()
            .map(|option| SettingOption {
                raw_value: option.raw,
                label: option.label.map(str::to_owned),
                member: option.member.to_owned(),
            })
            .collect(),
        allowed_values: field.allowed_values.to_vec(),
        storage_transform: field
            .storage_transform
            .map(|transform| SettingStorageTransform {
                input_unit: transform.input_unit.to_owned(),
                numerator: transform.numerator,
                denominator: transform.denominator,
                display_decimal_places: 1,
            }),
        is_blob: field.is_blob,
    }
}

const fn value_kind(field: &MenuField) -> SettingValueKind {
    if !field.options.is_empty() || !field.allowed_values.is_empty() {
        return SettingValueKind::Choice;
    }
    match field.descriptor.codec {
        FieldCodec::Bool | FieldCodec::BitBool { .. } => SettingValueKind::Boolean,
        FieldCodec::Byte { .. } | FieldCodec::BitField { .. } | FieldCodec::Unsigned { .. } => {
            SettingValueKind::UnsignedInteger
        }
        FieldCodec::Signed { .. } => SettingValueKind::SignedInteger,
        FieldCodec::FixedString { .. } => SettingValueKind::Text,
        FieldCodec::Bytes { .. } => SettingValueKind::Bytes,
    }
}

fn humanize_identifier(identifier: &str) -> String {
    let local = identifier
        .split_once('.')
        .map_or(identifier, |(_, remainder)| remainder);
    let mut output = String::with_capacity(local.len().saturating_add(8));
    let mut previous: Option<char> = None;
    for character in local.chars() {
        match character {
            '.' | '[' => {
                if !output.ends_with(' ') {
                    output.push(' ');
                }
            }
            ']' => {}
            _ => {
                if character.is_ascii_uppercase()
                    && previous
                        .is_some_and(|value| value.is_ascii_lowercase() || value.is_ascii_digit())
                    && !output.ends_with(' ')
                {
                    output.push(' ');
                }
                output.push(character);
            }
        }
        previous = Some(character);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn catalog_projects_every_authoritative_field_once() {
        let catalog = setting_catalog();
        let identifiers: BTreeSet<&str> = catalog.iter().map(|record| record.id.as_str()).collect();

        assert_eq!(
            catalog.len(),
            400,
            "catalog must expose all writable fields"
        );
        assert_eq!(
            identifiers.len(),
            catalog.len(),
            "setting identifiers must be unique"
        );
        assert_eq!(catalog.len(), MCP_D75_MENU_FIELDS.len());
    }

    #[test]
    fn catalog_preserves_schema_shapes() {
        let catalog = setting_catalog();
        for (record, field) in catalog.iter().zip(MCP_D75_MENU_FIELDS) {
            assert_eq!(record.id, field.descriptor.name);
            assert_eq!(
                record.byte_length,
                u64::try_from(encoded_len(field.descriptor.codec)).unwrap_or(u64::MAX)
            );
            assert_eq!(record.options.len(), field.options.len());
            assert_eq!(record.allowed_values, field.allowed_values);
            let expected_text_encoding = match field.descriptor.codec {
                FieldCodec::FixedString {
                    encoding: StringEncoding::Utf8,
                    ..
                } => Some(SettingTextEncoding::Utf8),
                FieldCodec::FixedString {
                    encoding: StringEncoding::MemoryMap,
                    ..
                } => Some(SettingTextEncoding::MemoryMapAscii),
                _ => None,
            };
            assert_eq!(record.text_encoding, expected_text_encoding);
        }
    }

    #[test]
    fn catalog_exports_exact_text_encoding_constraints() {
        let catalog = setting_catalog();
        let power_on = catalog
            .iter()
            .find(|record| record.id == "radio.PowerOnMessage")
            .and_then(|record| record.text_encoding);
        let bluetooth_name = catalog
            .iter()
            .find(|record| record.id == "radio.BluetoothDeviceName")
            .and_then(|record| record.text_encoding);

        assert_eq!(power_on, Some(SettingTextEncoding::MemoryMapAscii));
        assert_eq!(bluetooth_name, Some(SettingTextEncoding::Utf8));
    }

    #[test]
    fn plan_validation_rejects_unknown_and_out_of_domain_values() {
        let invalid = vec![
            SettingChange {
                setting_id: "radio.AutoWeatherScan".to_owned(),
                snapshot_id: 7,
                expected_value: SettingValue::Unsigned { value: 0 },
                desired_value: SettingValue::Unsigned { value: 3 },
            },
            SettingChange {
                setting_id: "radio.DoesNotExist".to_owned(),
                snapshot_id: 7,
                expected_value: SettingValue::Boolean { value: false },
                desired_value: SettingValue::Boolean { value: true },
            },
        ];
        let report = validate_setting_changes(invalid);

        assert!(!report.accepted, "malformed model plan must be rejected");
        assert_eq!(report.changes.len(), 2);
        assert!(
            report.changes.iter().all(|change| !change.accepted),
            "each malformed change needs a clear rejection"
        );
    }

    #[test]
    fn valid_batch_builds_one_coalesced_patch_plan() -> TestResult {
        let changes = vec![
            SettingChange {
                setting_id: "radio.TxEqualizerFmNfm".to_owned(),
                snapshot_id: 11,
                expected_value: SettingValue::Boolean { value: false },
                desired_value: SettingValue::Boolean { value: true },
            },
            SettingChange {
                setting_id: "radio.TxEqualizerDv".to_owned(),
                snapshot_id: 11,
                expected_value: SettingValue::Boolean { value: false },
                desired_value: SettingValue::Boolean { value: true },
            },
        ];
        let report = validate_setting_changes(changes.clone());
        let (patches, fields) = build_patch_plan(&changes).map_err(std::io::Error::other)?;

        assert!(report.accepted, "valid batch must pass: {report:?}");
        assert_eq!(fields.len(), 2);
        assert_eq!(patches.len(), 1, "shared-byte changes must coalesce");
        Ok(())
    }

    #[test]
    fn scaled_coordinate_helpers_keep_raw_and_seconds_distinct() -> TestResult {
        let identifier = "gps.MyPositionList[0].LatitudeSecondEncoded".to_owned();
        let raw = encode_setting_display_value(identifier.clone(), 30.0)?;
        let seconds = decode_setting_display_value(identifier, raw)?;

        assert_eq!(raw, 5_000);
        assert!((seconds - 30.0).abs() < f64::EPSILON);
        Ok(())
    }

    #[test]
    fn blob_and_all_scaled_fields_require_specialized_presentation() {
        let catalog = setting_catalog();
        let scaled = catalog
            .iter()
            .filter(|record| record.presentation == SettingPresentation::ScaledInteger)
            .count();
        let blobs = catalog
            .iter()
            .filter(|record| record.presentation == SettingPresentation::Blob)
            .count();

        assert_eq!(scaled, 16, "all coordinate components need scaling");
        assert_eq!(blobs, 1, "the power-on bitmap needs a specialized editor");
    }
}
