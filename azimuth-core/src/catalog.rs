//! Swift-facing view of the generated MCP-D75 setting schema.

use std::collections::BTreeSet;
use std::sync::Arc;

use kenwood_schema::{CodecSpec, FieldValue, StringEncoding};
use kenwood_thd75::memory::{MCP_D75_MENU_FIELDS, MenuField, PatchPlanner, PatchSet, menu_field};

use crate::SettingSnapshotId;

/// Top-level MCP-D75 setting group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum SettingMenu {
    /// General radio settings.
    Radio,
    /// GPS settings.
    Gps,
    /// APRS settings.
    Aprs,
    /// D-STAR settings.
    Dv,
}

impl SettingMenu {
    fn from_schema(value: &str) -> Option<Self> {
        match value {
            "radio" => Some(Self::Radio),
            "gps" => Some(Self::Gps),
            "aprs" => Some(Self::Aprs),
            "dv" => Some(Self::Dv),
            _ => None,
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

/// Radio lifecycle required to change one stored setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum SettingWritePolicy {
    /// The setting may be changed through the ordinary stale-safe MCP batch.
    Generic,
    /// The setting requires a purpose-built operation that owns disruption,
    /// restart, and reconnection outside the generic editor.
    DedicatedLifecycle,
}

/// One labeled raw value in a finite setting domain.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Object)]
pub struct SettingOption {
    /// Raw integer stored by the radio.
    raw_value: u64,
    /// English MCP-D75 label when one is available.
    label: Option<String>,
    /// Stable source member name for diagnostics and unlabeled choices.
    member: String,
}

#[uniffi::export]
impl SettingOption {
    /// Return the exact stored integer choice.
    #[must_use]
    pub const fn raw_value(&self) -> u64 {
        self.raw_value
    }
    /// Copy the English label, when the schema supplies one.
    #[must_use]
    pub fn label(&self) -> Option<String> {
        self.label.clone()
    }
    /// Copy the stable source member name.
    #[must_use]
    pub fn member(&self) -> String {
        self.member.clone()
    }
}

/// Display-to-storage integer transform declared by the schema.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Object)]
pub struct SettingStorageTransform {
    /// Unit accepted by the transform.
    input_unit: String,
    /// Encoding multiplier in `raw = round(display * numerator / denominator)`.
    numerator: i64,
    /// Encoding divisor in `raw = round(display * numerator / denominator)`.
    denominator: i64,
    /// Decimal places used when presenting a decoded value.
    display_decimal_places: u8,
}

#[uniffi::export]
impl SettingStorageTransform {
    /// Copy the declared input unit.
    #[must_use]
    pub fn input_unit(&self) -> String {
        self.input_unit.clone()
    }
    /// Return the positive encoding multiplier.
    #[must_use]
    pub const fn numerator(&self) -> i64 {
        self.numerator
    }
    /// Return the positive encoding divisor.
    #[must_use]
    pub const fn denominator(&self) -> i64 {
        self.denominator
    }
    /// Return the number of decimal places used by the catalog projection.
    #[must_use]
    pub const fn display_decimal_places(&self) -> u8 {
        self.display_decimal_places
    }
}

/// Complete app-facing metadata for one stored MCP-D75 field.
///
/// Created only by [`setting_catalog`]. Getters return immutable scalar values
/// or owned foreign snapshots; no constructor or setter permits conflicting
/// value-shape, layout, transform or lifecycle metadata.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Object)]
pub struct SettingRecord {
    /// Stable identifier used by reads, plans, and writes.
    id: String,
    /// Human-readable name derived from the stable identifier.
    display_name: String,
    /// Top-level setting group.
    menu: SettingMenu,
    /// Accepted value shape.
    value_kind: SettingValueKind,
    /// Exact text encoding constraint for fixed strings; absent for non-text fields.
    text_encoding: Option<SettingTextEncoding>,
    /// Required presentation path for the raw typed value.
    presentation: SettingPresentation,
    /// Absolute byte offset in a raw MCP image.
    offset: u64,
    /// Encoded byte width.
    byte_length: u64,
    /// Owned bit mask for a packed field.
    bit_mask: Option<u8>,
    /// Right shift for a packed integer field.
    bit_shift: Option<u8>,
    /// Inclusive unsigned minimum when applicable.
    unsigned_min: Option<u64>,
    /// Inclusive unsigned maximum when applicable.
    unsigned_max: Option<u64>,
    /// Inclusive signed minimum when applicable.
    signed_min: Option<i64>,
    /// Inclusive signed maximum when applicable.
    signed_max: Option<i64>,
    /// Qualified enum type from the schema, when applicable.
    enum_type: Option<String>,
    /// Finite labeled choices.
    options: Vec<Arc<SettingOption>>,
    /// Finite unlabeled raw choices.
    allowed_values: Vec<u64>,
    /// Optional display-to-storage transform.
    storage_transform: Option<Arc<SettingStorageTransform>>,
    /// Whether the value is a large persistent blob.
    is_blob: bool,
    /// Lifecycle policy enforced before any generic setting write.
    write_policy: SettingWritePolicy,
    /// Whether changing this field requires a radio restart.
    requires_restart: bool,
    /// Whether changing this field invalidates the active transport session.
    requires_reconnect: bool,
    /// User-facing reason a dedicated-lifecycle field is read-only here.
    write_restriction: Option<String>,
}

#[uniffi::export]
impl SettingRecord {
    /// Copy the stable setting identifier.
    #[must_use]
    pub fn id(&self) -> String {
        self.id.clone()
    }
    /// Copy the human-readable setting name.
    #[must_use]
    pub fn display_name(&self) -> String {
        self.display_name.clone()
    }
    /// Return the top-level schema group.
    #[must_use]
    pub const fn menu(&self) -> SettingMenu {
        self.menu
    }
    /// Return the accepted stored value shape.
    #[must_use]
    pub const fn value_kind(&self) -> SettingValueKind {
        self.value_kind
    }
    /// Return the exact fixed-string encoding, when applicable.
    #[must_use]
    pub const fn text_encoding(&self) -> Option<SettingTextEncoding> {
        self.text_encoding
    }
    /// Return the required presentation path.
    #[must_use]
    pub const fn presentation(&self) -> SettingPresentation {
        self.presentation
    }
    /// Return the absolute image byte offset.
    #[must_use]
    pub const fn offset(&self) -> u64 {
        self.offset
    }
    /// Return the encoded byte width.
    #[must_use]
    pub const fn byte_length(&self) -> u64 {
        self.byte_length
    }
    /// Return the packed-field ownership mask, when applicable.
    #[must_use]
    pub const fn bit_mask(&self) -> Option<u8> {
        self.bit_mask
    }
    /// Return the packed integer right shift, when applicable.
    #[must_use]
    pub const fn bit_shift(&self) -> Option<u8> {
        self.bit_shift
    }
    /// Return the unsigned inclusive minimum, when applicable.
    #[must_use]
    pub const fn unsigned_min(&self) -> Option<u64> {
        self.unsigned_min
    }
    /// Return the unsigned inclusive maximum, when applicable.
    #[must_use]
    pub const fn unsigned_max(&self) -> Option<u64> {
        self.unsigned_max
    }
    /// Return the signed inclusive minimum, when applicable.
    #[must_use]
    pub const fn signed_min(&self) -> Option<i64> {
        self.signed_min
    }
    /// Return the signed inclusive maximum, when applicable.
    #[must_use]
    pub const fn signed_max(&self) -> Option<i64> {
        self.signed_max
    }
    /// Copy the qualified enum name, when applicable.
    #[must_use]
    pub fn enum_type(&self) -> Option<String> {
        self.enum_type.clone()
    }
    /// Copy immutable references to the finite labeled choices.
    #[must_use]
    pub fn options(&self) -> Vec<Arc<SettingOption>> {
        self.options.clone()
    }
    /// Copy the finite unlabeled raw choices.
    #[must_use]
    pub fn allowed_values(&self) -> Vec<u64> {
        self.allowed_values.clone()
    }
    /// Share the immutable scaling projection, when present.
    #[must_use]
    pub fn storage_transform(&self) -> Option<Arc<SettingStorageTransform>> {
        self.storage_transform.clone()
    }
    /// Return whether the field requires a blob editor.
    #[must_use]
    pub const fn is_blob(&self) -> bool {
        self.is_blob
    }
    /// Return the enforced write lifecycle.
    #[must_use]
    pub const fn write_policy(&self) -> SettingWritePolicy {
        self.write_policy
    }
    /// Return whether changing the field requires a restart.
    #[must_use]
    pub const fn requires_restart(&self) -> bool {
        self.requires_restart
    }
    /// Return whether changing the field invalidates the transport.
    #[must_use]
    pub const fn requires_reconnect(&self) -> bool {
        self.requires_reconnect
    }
    /// Copy the reason the generic editor cannot change this field.
    #[must_use]
    pub fn write_restriction(&self) -> Option<String> {
        self.write_restriction.clone()
    }
}

/// Generated catalog metadata that cannot be projected faithfully.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, uniffi::Error)]
pub enum SettingCatalogError {
    /// A field names a group outside the four supported schema groups.
    #[error("setting {setting_id} has unknown menu group {menu}")]
    UnknownMenu {
        /// Stable setting identifier.
        setting_id: String,
        /// Unrecognized generated group.
        menu: String,
    },
    /// An image offset or width cannot fit the foreign `u64` representation.
    #[error("setting {setting_id} has an unrepresentable image layout")]
    UnrepresentableLayout {
        /// Stable setting identifier.
        setting_id: String,
    },
}

const DV_GATEWAY_MODE_SETTING_ID: &str = "dv.DvGatewayModeDvGateway";
const USB_FUNCTION_SETTING_ID: &str = "radio.UsbFunction";

#[derive(Debug, Clone, Copy)]
struct SettingLifecycle {
    write_policy: SettingWritePolicy,
    requires_restart: bool,
    requires_reconnect: bool,
    write_restriction: Option<&'static str>,
}

impl SettingLifecycle {
    const GENERIC: Self = Self {
        write_policy: SettingWritePolicy::Generic,
        requires_restart: false,
        requires_reconnect: false,
        write_restriction: None,
    };
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

/// One optimistic setting mutation bound to a retained snapshot.
///
/// `snapshot_id` and `expected_value` must come from the same
/// [`SettingReadResult`](crate::SettingReadResult). The core checks both the
/// typed value and every byte of each cached source page before any write.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct SettingChange {
    /// Stable setting identifier.
    pub setting_id: String,
    /// Snapshot that supplied `expected_value`.
    pub snapshot_id: SettingSnapshotId,
    /// Value shown to the user during review.
    pub expected_value: SettingValue,
    /// Value to write.
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

/// Return all 400 stored MCP-D75 setting records.
///
/// The records are projected directly from `kenwood-thd75`'s generated
/// registry, so the app and Rust write path cannot drift onto separate field
/// definitions. Most records permit generic stale-safe writes; disruptive
/// records declare [`SettingWritePolicy::DedicatedLifecycle`] and are
/// rejected by the generic planner.
///
/// # Errors
///
/// Returns [`SettingCatalogError`] if a generated group or image layout cannot
/// be represented faithfully. No partial catalog is returned.
#[uniffi::export]
pub fn setting_catalog() -> Result<Vec<Arc<SettingRecord>>, SettingCatalogError> {
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
    transform
        .decode_display(raw_value)
        .ok_or_else(|| SettingConversionError::InvalidDisplayValue {
            setting_id,
            detail: "raw value is not representable through the storage transform".to_owned(),
        })
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
    let raw = transform.encode_display(display_value).ok_or_else(|| {
        SettingConversionError::InvalidDisplayValue {
            setting_id: setting_id.clone(),
            detail: "display value is not representable through the storage transform (it must \
                     be finite and scale to a nonnegative exact integer)"
                .to_owned(),
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
/// before the radio operation. This function is suitable for rejecting
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

fn validate_change(change: &SettingChange) -> Result<(), String> {
    let field = menu_field(&change.setting_id)
        .ok_or_else(|| format!("unknown setting identifier {}", change.setting_id))?;
    validate_write_policy(field)?;
    field
        .validate_stored_value(change.expected_value.as_field_value())
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

fn validate_write_policy(field: &MenuField) -> Result<(), String> {
    let lifecycle = setting_lifecycle(field.descriptor.name);
    if lifecycle.write_policy == SettingWritePolicy::Generic {
        return Ok(());
    }
    Err(lifecycle.write_restriction.map_or_else(
        || "this setting requires a dedicated radio lifecycle".to_owned(),
        str::to_owned,
    ))
}

fn validate_batch(changes: &[SettingChange]) -> Result<(), String> {
    let snapshot = changes
        .first()
        .map(|change| change.snapshot_id)
        .ok_or_else(|| "a setting plan must contain at least one change".to_owned())?;
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

fn setting_record(field: &MenuField) -> Result<Arc<SettingRecord>, SettingCatalogError> {
    let codec = field.descriptor.codec;
    let lifecycle = setting_lifecycle(field.descriptor.name);
    let (bit_mask, bit_shift) = match codec.spec() {
        CodecSpec::BitBool { mask } => (Some(mask), None),
        CodecSpec::BitField { mask, shift, .. } => (Some(mask), Some(shift)),
        _ => (None, None),
    };
    let (unsigned_min, unsigned_max) = match codec.spec() {
        CodecSpec::Byte { min, max } | CodecSpec::BitField { min, max, .. } => {
            (Some(u64::from(min)), Some(u64::from(max)))
        }
        CodecSpec::Unsigned { min, max, .. } => (Some(min), Some(max)),
        _ => (None, None),
    };
    let (signed_min, signed_max) = match codec.spec() {
        CodecSpec::Signed { min, max, .. } => (Some(min), Some(max)),
        _ => (None, None),
    };

    let menu =
        SettingMenu::from_schema(field.menu).ok_or_else(|| SettingCatalogError::UnknownMenu {
            setting_id: field.descriptor.name.to_owned(),
            menu: field.menu.to_owned(),
        })?;
    let offset = u64::try_from(field.descriptor.offset).map_err(|_error| {
        SettingCatalogError::UnrepresentableLayout {
            setting_id: field.descriptor.name.to_owned(),
        }
    })?;
    let byte_length = u64::try_from(codec.encoded_len()).map_err(|_error| {
        SettingCatalogError::UnrepresentableLayout {
            setting_id: field.descriptor.name.to_owned(),
        }
    })?;

    Ok(Arc::new(SettingRecord {
        id: field.descriptor.name.to_owned(),
        display_name: humanize_identifier(field.descriptor.name),
        menu,
        value_kind: value_kind(field),
        text_encoding: match codec.spec() {
            CodecSpec::FixedString {
                encoding: StringEncoding::Utf8,
                ..
            } => Some(SettingTextEncoding::Utf8),
            CodecSpec::FixedString {
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
        offset,
        byte_length,
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
            .map(|option| {
                Arc::new(SettingOption {
                    raw_value: option.raw,
                    label: option.label.map(str::to_owned),
                    member: option.member.to_owned(),
                })
            })
            .collect(),
        allowed_values: field.allowed_values.to_vec(),
        storage_transform: field.storage_transform.map(|transform| {
            Arc::new(SettingStorageTransform {
                input_unit: transform.input_unit().to_owned(),
                numerator: i64::from(transform.numerator()),
                denominator: i64::from(transform.denominator()),
                display_decimal_places: 1,
            })
        }),
        is_blob: field.is_blob,
        write_policy: lifecycle.write_policy,
        requires_restart: lifecycle.requires_restart,
        requires_reconnect: lifecycle.requires_reconnect,
        write_restriction: lifecycle.write_restriction.map(str::to_owned),
    }))
}

fn setting_lifecycle(identifier: &str) -> SettingLifecycle {
    match identifier {
        DV_GATEWAY_MODE_SETTING_ID => SettingLifecycle {
            write_policy: SettingWritePolicy::DedicatedLifecycle,
            requires_restart: true,
            requires_reconnect: true,
            write_restriction: Some(
                "Menu 650 changes the radio's control protocol and reboots the radio; use the dedicated DV Gateway lifecycle instead of the generic settings editor.",
            ),
        },
        USB_FUNCTION_SETTING_ID => SettingLifecycle {
            write_policy: SettingWritePolicy::DedicatedLifecycle,
            requires_restart: true,
            requires_reconnect: true,
            write_restriction: Some(
                "Menu 980 replaces the active USB control interface; changing it needs a purpose-built disconnect and reconnect lifecycle that is unavailable in the generic settings editor.",
            ),
        },
        _ => SettingLifecycle::GENERIC,
    }
}

const fn value_kind(field: &MenuField) -> SettingValueKind {
    if !field.options.is_empty() || !field.allowed_values.is_empty() {
        return SettingValueKind::Choice;
    }
    match field.descriptor.codec.spec() {
        CodecSpec::Bool | CodecSpec::BitBool { .. } => SettingValueKind::Boolean,
        CodecSpec::Byte { .. } | CodecSpec::BitField { .. } | CodecSpec::Unsigned { .. } => {
            SettingValueKind::UnsignedInteger
        }
        CodecSpec::Signed { .. } => SettingValueKind::SignedInteger,
        CodecSpec::FixedString { .. } => SettingValueKind::Text,
        CodecSpec::Bytes { .. } => SettingValueKind::Bytes,
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
    fn catalog_projects_every_authoritative_field_once() -> TestResult {
        let catalog = setting_catalog()?;
        let identifiers: BTreeSet<&str> = catalog.iter().map(|record| record.id.as_str()).collect();

        assert_eq!(catalog.len(), 400, "catalog must expose all stored fields");
        assert_eq!(
            identifiers.len(),
            catalog.len(),
            "setting identifiers must be unique"
        );
        assert_eq!(catalog.len(), MCP_D75_MENU_FIELDS.len());
        Ok(())
    }

    #[test]
    fn catalog_preserves_schema_shapes() -> TestResult {
        let catalog = setting_catalog()?;
        for (record, field) in catalog.iter().zip(MCP_D75_MENU_FIELDS) {
            assert_eq!(record.id, field.descriptor.name);
            assert_eq!(
                record.byte_length,
                u64::try_from(field.descriptor.codec.encoded_len())?
            );
            assert_eq!(record.options.len(), field.options.len());
            assert_eq!(record.allowed_values, field.allowed_values);
            let expected_text_encoding = match field.descriptor.codec.spec() {
                CodecSpec::FixedString {
                    encoding: StringEncoding::Utf8,
                    ..
                } => Some(SettingTextEncoding::Utf8),
                CodecSpec::FixedString {
                    encoding: StringEncoding::MemoryMap,
                    ..
                } => Some(SettingTextEncoding::MemoryMapAscii),
                _ => None,
            };
            assert_eq!(record.text_encoding, expected_text_encoding);
        }
        Ok(())
    }

    #[test]
    fn catalog_exports_exact_text_encoding_constraints() -> TestResult {
        let catalog = setting_catalog()?;
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
        Ok(())
    }

    #[test]
    fn disruptive_settings_export_dedicated_lifecycle_truth() -> TestResult {
        let catalog = setting_catalog()?;
        let dedicated: Vec<&Arc<SettingRecord>> = catalog
            .iter()
            .filter(|record| record.write_policy == SettingWritePolicy::DedicatedLifecycle)
            .collect();

        assert_eq!(
            dedicated
                .iter()
                .map(|record| record.id.as_str())
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([DV_GATEWAY_MODE_SETTING_ID, USB_FUNCTION_SETTING_ID])
        );
        for record in dedicated {
            assert!(record.requires_restart, "{} must report restart", record.id);
            assert!(
                record.requires_reconnect,
                "{} must report transport replacement",
                record.id
            );
            assert!(
                record
                    .write_restriction
                    .as_deref()
                    .is_some_and(|detail| detail.contains("generic settings editor")),
                "{} must explain why the generic editor is unavailable",
                record.id
            );
        }
        Ok(())
    }

    #[test]
    fn plan_validation_rejects_unknown_and_out_of_domain_values() -> TestResult {
        let invalid = vec![
            SettingChange {
                setting_id: "radio.AutoWeatherScan".to_owned(),
                snapshot_id: SettingSnapshotId::new(7)?,
                expected_value: SettingValue::Unsigned { value: 0 },
                desired_value: SettingValue::Unsigned { value: 3 },
            },
            SettingChange {
                setting_id: "radio.DoesNotExist".to_owned(),
                snapshot_id: SettingSnapshotId::new(7)?,
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
        Ok(())
    }

    #[test]
    fn valid_batch_builds_one_coalesced_patch_plan() -> TestResult {
        let changes = vec![
            SettingChange {
                setting_id: "radio.TxEqualizerFmNfm".to_owned(),
                snapshot_id: SettingSnapshotId::new(11)?,
                expected_value: SettingValue::Boolean { value: false },
                desired_value: SettingValue::Boolean { value: true },
            },
            SettingChange {
                setting_id: "radio.TxEqualizerDv".to_owned(),
                snapshot_id: SettingSnapshotId::new(11)?,
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
    fn generic_validation_rejects_disruptive_settings_and_mixed_batches() -> TestResult {
        let ordinary = SettingChange {
            setting_id: "radio.Beep".to_owned(),
            snapshot_id: SettingSnapshotId::new(11)?,
            expected_value: SettingValue::Boolean { value: false },
            desired_value: SettingValue::Boolean { value: true },
        };
        let gateway = SettingChange {
            setting_id: DV_GATEWAY_MODE_SETTING_ID.to_owned(),
            snapshot_id: SettingSnapshotId::new(11)?,
            expected_value: SettingValue::Unsigned { value: 0 },
            desired_value: SettingValue::Unsigned { value: 1 },
        };
        let usb = SettingChange {
            setting_id: USB_FUNCTION_SETTING_ID.to_owned(),
            snapshot_id: SettingSnapshotId::new(11)?,
            expected_value: SettingValue::Unsigned { value: 0 },
            desired_value: SettingValue::Unsigned { value: 1 },
        };

        for disruptive in [&gateway, &usb] {
            let report = validate_setting_changes(vec![disruptive.clone()]);
            assert!(
                !report.accepted,
                "{} must be rejected",
                disruptive.setting_id
            );
            assert!(
                report
                    .changes
                    .first()
                    .and_then(|change| change.detail.as_deref())
                    .is_some_and(|detail| detail.contains("generic settings editor")),
                "{} must name the required lifecycle: {report:?}",
                disruptive.setting_id
            );
        }

        let mixed = validate_setting_changes(vec![ordinary, gateway]);
        assert!(!mixed.accepted, "a mixed batch must fail as a whole");
        assert!(mixed.changes.first().is_some_and(|change| change.accepted));
        assert!(
            mixed.changes.get(1).is_some_and(|change| !change.accepted),
            "the disruptive member must carry the rejection: {mixed:?}"
        );
        Ok(())
    }

    #[test]
    fn off_menu_snapshot_value_can_be_replaced_but_not_requested() -> TestResult {
        let replace = SettingChange {
            setting_id: "radio.Pf1PfKey".to_owned(),
            snapshot_id: SettingSnapshotId::new(12)?,
            expected_value: SettingValue::Unsigned { value: 31 },
            desired_value: SettingValue::Unsigned { value: 30 },
        };
        let report = validate_setting_changes(vec![replace.clone()]);
        let (patches, fields) = build_patch_plan(&[replace]).map_err(std::io::Error::other)?;

        assert!(
            report.accepted,
            "an exact off-menu expected value must remain usable for concurrency control: {report:?}"
        );
        assert_eq!(fields.len(), 1);
        assert_eq!(patches.len(), 1);

        let preserve = SettingChange {
            setting_id: "radio.Pf1PfKey".to_owned(),
            snapshot_id: SettingSnapshotId::new(12)?,
            expected_value: SettingValue::Unsigned { value: 30 },
            desired_value: SettingValue::Unsigned { value: 31 },
        };
        let rejected = validate_setting_changes(vec![preserve]);
        assert!(
            !rejected.accepted,
            "off-menu desired values must stay rejected"
        );
        let rejection_detail = rejected
            .changes
            .first()
            .and_then(|change| change.detail.as_deref());
        assert!(
            rejection_detail.is_some_and(|detail| detail.contains("invalid desired value")),
            "the rejection must identify the unsafe side of the change: {rejected:?}"
        );
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
    fn blob_and_all_scaled_fields_require_specialized_presentation() -> TestResult {
        let catalog = setting_catalog()?;
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
        Ok(())
    }

    #[test]
    fn catalog_rejects_unknown_groups_instead_of_relabeling_them() -> TestResult {
        let mut field = *MCP_D75_MENU_FIELDS.first().ok_or("missing schema field")?;
        field.menu = "unknown";
        let result = setting_record(&field);
        assert!(
            matches!(result, Err(SettingCatalogError::UnknownMenu { ref setting_id, ref menu }) if setting_id == field.descriptor.name && menu == "unknown"),
            "{result:?}"
        );
        assert_eq!(SettingMenu::from_schema("radio"), Some(SettingMenu::Radio));
        Ok(())
    }
}
