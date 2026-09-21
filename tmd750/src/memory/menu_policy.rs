//! Ordinary-update classification and value validation for registered fields.
//!
//! Classification covers the compiled menu registry only. Address resolution,
//! radio identity, captured-page coverage, and operating-state guards belong to
//! [`crate::radio::menu::MenuUpdatePlan`], which applies them regardless of a
//! field's classification here.

use super::{FieldCodec, FieldValue, MenuField, My1Callsign};
use crate::error::SchemaError;

/// Whether a menu field belongs in an ordinary configuration update.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum MenuWritePolicy {
    /// The field has a supported value domain and no excluded lifecycle effect.
    Ordinary,
    /// Storage is known, but the writable semantic domain is not yet supported.
    UnresolvedDomain,
    /// Changing this field requires explicit operating-state or link handling.
    LifecycleRequired,
    /// Scalar configuration updates do not support this binary field.
    Binary,
}

/// Failure to classify or validate an ordinary menu assignment.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum MenuWritePolicyError {
    /// This field cannot use the ordinary update lifecycle.
    #[error("{field} cannot use ordinary menu updates: {policy:?}")]
    NotOrdinary {
        /// Registry field whose policy prevents the update.
        field: &'static str,
        /// The field's classification, which is not `Ordinary`.
        policy: MenuWritePolicy,
    },
    /// Text contains a terminator and would not round-trip exactly.
    #[error("{field} text contains a storage terminator")]
    EmbeddedTerminator {
        /// Registry field requiring nonterminating input text.
        field: &'static str,
    },
    /// MY callsign text violates the supported character, length, or shape rules.
    #[error("{field} requires empty text or a supported MY callsign of at most eight ASCII bytes")]
    InvalidMyCallsign {
        /// Registry MY callsign field whose supplied text was rejected.
        field: &'static str,
    },
    /// A value violates its supplemental domain, registry domain, or codec.
    #[error(transparent)]
    Schema(#[from] SchemaError),
}

impl MenuField {
    /// Classify this field for the ordinary menu-update lifecycle.
    ///
    /// Binary fields take precedence over lifecycle restrictions. Numeric
    /// storage capacity alone is not a supported semantic domain. Supplemental
    /// selector domains are enforced by [`Self::validate_ordinary_value`].
    #[must_use]
    pub fn write_policy(&self) -> MenuWritePolicy {
        if self.is_blob || matches!(self.descriptor.codec, FieldCodec::Bytes { .. }) {
            MenuWritePolicy::Binary
        } else if requires_lifecycle(self.descriptor.name) {
            MenuWritePolicy::LifecycleRequired
        } else if has_unresolved_domain(self) {
            MenuWritePolicy::UnresolvedDomain
        } else {
            MenuWritePolicy::Ordinary
        }
    }

    /// Validate one exact stored value for a field classified `Ordinary`.
    ///
    /// Group links accept group indices `0..=29` or `255` for not linked.
    /// Gateway MY selection accepts `0..=5`. DV message selection accepts `0`
    /// for off or `1..=5` for a message. Coordinate hemisphere selectors accept
    /// `0` or `1`. MY callsign entries accept exact empty text for clearing.
    /// Nonempty MY text uses this API's uppercase ASCII letter, digit, and space
    /// policy, at most eight bytes, with a contiguous initial callsign and the
    /// supported final-module restrictions. Other fields use the registry's
    /// existing domains and codecs.
    /// Text is not trimmed, padded by the caller, or normalized; embedded
    /// storage terminators are rejected.
    ///
    /// # Errors
    ///
    /// Returns [`MenuWritePolicyError::NotOrdinary`] when [`Self::write_policy`]
    /// is not `Ordinary`, [`MenuWritePolicyError::EmbeddedTerminator`] for text
    /// holding NUL or the field's padding byte,
    /// [`MenuWritePolicyError::InvalidMyCallsign`] for rejected MY text, and
    /// [`MenuWritePolicyError::Schema`] for a supplemental-domain, registry-domain,
    /// or codec failure.
    pub fn validate_ordinary_value(
        &self,
        value: FieldValue<'_>,
    ) -> Result<(), MenuWritePolicyError> {
        let field = self.descriptor.name;
        let policy = self.write_policy();
        if policy != MenuWritePolicy::Ordinary {
            return Err(MenuWritePolicyError::NotOrdinary { field, policy });
        }
        if let Some(domain) = supplemental_domain(field) {
            let FieldValue::Unsigned(raw) = value else {
                return Err(SchemaError::TypeMismatch {
                    field,
                    expected: "unsigned",
                    actual: value.kind_name(),
                }
                .into());
            };
            if !domain.contains(raw) {
                return Err(SchemaError::DisallowedValue { field, value: raw }.into());
            }
        }
        if let (FieldCodec::FixedString { padding, .. }, FieldValue::Text(text)) =
            (self.descriptor.codec, value)
            && text
                .as_bytes()
                .iter()
                .any(|byte| *byte == 0 || *byte == padding)
        {
            return Err(MenuWritePolicyError::EmbeddedTerminator { field });
        }
        if is_my_callsign(field) {
            validate_my_callsign(field, value)?;
        }
        self.validate_patch_value(value)?;
        let _encoded = self.descriptor.encode(value)?;
        Ok(())
    }
}

fn is_my_callsign(name: &str) -> bool {
    matches!(
        name.strip_prefix("dv.MyCallsignDvGatewayList[")
            .and_then(|suffix| suffix.strip_suffix("].MyCallsignDvGateway")),
        Some("0" | "1" | "2" | "3" | "4" | "5")
    )
}

fn validate_my_callsign(
    field: &'static str,
    value: FieldValue<'_>,
) -> Result<(), MenuWritePolicyError> {
    let FieldValue::Text(text) = value else {
        return Err(SchemaError::TypeMismatch {
            field,
            expected: "text",
            actual: value.kind_name(),
        }
        .into());
    };
    if text.is_empty() {
        return Ok(());
    }
    let invalid = || MenuWritePolicyError::InvalidMyCallsign { field };
    let _validated = My1Callsign::new(text).map_err(|_error| invalid())?;
    // Padding is local to validation; the caller's exact text remains unchanged.
    let mut padded = [b' '; 8];
    padded
        .get_mut(..text.len())
        .ok_or_else(invalid)?
        .copy_from_slice(text.as_bytes());
    let prefix = padded
        .iter()
        .take(6)
        .take_while(|byte| **byte != b' ')
        .count();
    let valid_prefix = prefix == 6
        || (prefix != 0
            && padded
                .get(prefix..7)
                .is_some_and(|bytes| bytes.iter().all(|byte| *byte == b' ')));
    let reserved_module = matches!(
        padded.get(6..),
        Some([b' ', b'G' | b'I' | b'S' | b'0'..=b'9'])
    );
    if valid_prefix && !reserved_module {
        Ok(())
    } else {
        Err(invalid())
    }
}

fn requires_lifecycle(name: &str) -> bool {
    // The ordinary workflow does not switch the active PM bank, reconfigure
    // transports, or enable automatic transmissions. Classification is
    // intentionally value-independent; disabling also needs that lifecycle.
    name.starts_with("ipnet.")
        || matches!(
            name,
            "pm.PmSelect"
                | "pm.PmAutoStore"
                | "radio.RepeaterMode"
                | "radio.RepeaterTxHold"
                | "radio.RepeaterIdTx"
                | "radio.RemoteAnswerBack"
                | "radio.BluetoothOnOff"
                | "radio.BluetoothDeviceName"
                | "radio.BluetoothAutoConnect"
                | "radio.PowerKey"
                | "radio.AutoPowerOff"
                | "radio.UsbFunction"
                | "radio.UsbAudioInOut"
                | "radio.PcOutputInterfaceGps"
                | "radio.PcOutputInterfaceAprs"
                | "radio.KissModeInterface"
                | "radio.DigitalModeInterface"
                | "radio.DvGatewayInterface"
                | "radio.ExternalDataBand"
                | "radio.ExternalDataSpeed"
                | "radio.TxInhibit"
                | "radio.Vox"
                | "radio.VoxTxOnBusy"
                | "radio.DigitalAutoReply"
                | "gps.PcOutput"
                | "gps.GpsPortBaudRate"
                | "gps.GpsPortInput"
                | "gps.GpsPortOutput"
                | "aprs.WeatherTx"
                | "aprs.WeatherTxInterval"
                | "aprs.TncDataSpeed"
                | "aprs.TncDataBand"
                | "aprs.AutoChSetting"
                | "aprs.Ptt"
                | "aprs.BeaconTxMethod"
                | "aprs.BeaconTxInterval"
                | "aprs.BeaconTxDecay"
                | "aprs.BeaconTxProportion"
                | "aprs.AutoReplyOnOff"
                | "aprs.AutoReplyDelayTime"
                | "aprs.DigipeatMyCallsign"
                | "aprs.UIdigipeat"
                | "aprs.UItrace"
                | "aprs.UIflood"
                | "aprs.PcOutput"
                | "dv.DirectReplyTxRx"
                | "dv.AutoReplyTimingTxRx"
                | "dv.AutoReply"
                | "dv.AutoTxGpsDataTx"
                | "dv.DvGatewayModeDvGateway"
                | "dv.SelectTerminalMode"
                | "dv.GatewayType"
        )
        || (name.starts_with("aprs.ObjectList[")
            && (name.ends_with(".ObjectTxFormat") || name.ends_with(".ObjectTxInterval")))
}

fn has_unresolved_domain(field: &MenuField) -> bool {
    // These numeric registry entries lack a semantic domain. The other such
    // entries have the supplemental selector domains below. Do not substitute
    // a byte's capacity for an operating range when extending this list.
    matches!(
        field.descriptor.name,
        "radio.TimeZone"
            | "gps.Distance"
            | "aprs.SlowRateTime"
            | "aprs.FastRateTime"
            | "aprs.TurnAngleDeg"
            | "aprs.TurnSlopeDegSpeed"
            | "aprs.TurnTimeTime"
            | "aprs.FilterPositionLimit"
            | "aprs.ObjectUsedNo"
            | "aprs.UIcheck"
            | "aprs.IconTableOverlay"
            | "aprs.IconSymbol"
            | "aprs.StatusTextSelect"
            | "dv.ReflectorHostsSelect"
            | "dv.AutoUpdateUrlSelect"
    ) || (field.options.is_empty()
        && field.allowed_values.is_empty()
        && supplemental_domain(field.descriptor.name).is_none()
        && (field.enum_type.is_some() || numeric_capacity_only(field.descriptor.codec)))
}

fn numeric_capacity_only(codec: FieldCodec) -> bool {
    // The generated codec retains semantic bounds but not their provenance.
    // Full storage bounds therefore require choices or a reviewed supplement.
    // Use wider arithmetic so eight-byte limits never overflow or wrap.
    match codec {
        FieldCodec::Byte { min, max } => min == 0 && max == u8::MAX,
        FieldCodec::BitField {
            mask,
            shift,
            min,
            max,
        } => shift >= 8 || mask == 0 || (min == 0 && max >= (mask >> shift)),
        FieldCodec::Unsigned {
            width, min, max, ..
        } => {
            !(1..=8).contains(&width)
                || (min == 0 && u128::from(max) >= (1_u128 << (u32::from(width) * 8)) - 1)
        }
        FieldCodec::Signed {
            width, min, max, ..
        } => {
            if !(1..=8).contains(&width) {
                return true;
            }
            let magnitude = 1_i128 << (u32::from(width) * 8 - 1);
            i128::from(min) <= -magnitude && i128::from(max) >= magnitude - 1
        }
        FieldCodec::Bool
        | FieldCodec::BitBool { .. }
        | FieldCodec::FixedString { .. }
        | FieldCodec::Bytes { .. } => false,
    }
}

#[derive(Debug, Clone, Copy)]
enum SupplementalDomain {
    GroupLink,
    SixSelections,
    Hemisphere,
}

impl SupplementalDomain {
    const fn contains(self, raw: u64) -> bool {
        match self {
            Self::GroupLink => raw <= 29 || raw == 255,
            Self::SixSelections => raw <= 5,
            Self::Hemisphere => raw <= 1,
        }
    }
}

fn supplemental_domain(name: &str) -> Option<SupplementalDomain> {
    if matches!(
        name,
        "dv.MyCallsignSelectDvGateway" | "dv.MyDvMessageSelect"
    ) {
        Some(SupplementalDomain::SixSelections)
    } else if name.strip_prefix("radio.GroupLink").is_some_and(|index| {
        matches!(
            index,
            "0" | "1"
                | "2"
                | "3"
                | "4"
                | "5"
                | "6"
                | "7"
                | "8"
                | "9"
                | "10"
                | "11"
                | "12"
                | "13"
                | "14"
                | "15"
                | "16"
                | "17"
                | "18"
                | "19"
                | "20"
                | "21"
                | "22"
                | "23"
                | "24"
                | "25"
                | "26"
                | "27"
                | "28"
                | "29"
        )
    }) {
        Some(SupplementalDomain::GroupLink)
    } else if name.rsplit_once('.').is_some_and(|(record, member)| {
        matches!(member, "NorthSouth" | "EastWest")
            && matches!(
                record,
                "gps.MyPositionList[0]"
                    | "gps.MyPositionList[1]"
                    | "gps.MyPositionList[2]"
                    | "gps.MyPositionList[3]"
                    | "gps.MyPositionList[4]"
                    | "aprs.ObjectList[0]"
                    | "aprs.ObjectList[1]"
                    | "aprs.ObjectList[2]"
            )
    }) {
        Some(SupplementalDomain::Hemisphere)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{Endian, MCP_D750_MENU_FIELDS, menu_field};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn field(name: &str) -> Result<&'static MenuField, Box<dyn std::error::Error>> {
        menu_field(name).ok_or_else(|| format!("missing registry field {name}").into())
    }

    #[test]
    fn ordinary_text_boolean_and_enum_fields_remain_available() -> TestResult {
        for (name, value) in [
            ("pm.PmName2", FieldValue::Text("HOME")),
            ("radio.Beep", FieldValue::Bool(false)),
            ("radio.MeterType", FieldValue::Unsigned(1)),
            (
                "dv.MyCallsignDvGatewayList[0].MyCallsignDvGateway",
                FieldValue::Text("KQ4NIT"),
            ),
        ] {
            let selected = field(name)?;
            assert_eq!(
                selected.write_policy(),
                MenuWritePolicy::Ordinary,
                "ordinary field {name} must remain available"
            );
            selected.validate_ordinary_value(value)?;
        }
        Ok(())
    }

    #[test]
    fn lifecycle_controls_are_rejected_even_when_disabling() -> TestResult {
        for name in [
            "pm.PmSelect",
            "pm.PmAutoStore",
            "radio.RepeaterMode",
            "radio.AutoPowerOff",
            "radio.UsbFunction",
            "radio.KissModeInterface",
            "radio.DvGatewayInterface",
            "radio.BluetoothOnOff",
            "radio.TxInhibit",
            "radio.Vox",
            "gps.GpsPortBaudRate",
            "aprs.BeaconTxMethod",
            "aprs.AutoReplyOnOff",
            "aprs.UItrace",
            "aprs.UIflood",
            "aprs.UIdigipeat",
            "aprs.ObjectList[2].ObjectTxInterval",
            "dv.AutoTxGpsDataTx",
            "dv.DvGatewayModeDvGateway",
            "dv.SelectTerminalMode",
            "ipnet.WirelessLan",
        ] {
            let selected = field(name)?;
            assert_eq!(
                selected.write_policy(),
                MenuWritePolicy::LifecycleRequired,
                "{name} requires explicit lifecycle handling"
            );
            assert_eq!(
                selected.validate_ordinary_value(FieldValue::Bool(false)),
                Err(MenuWritePolicyError::NotOrdinary {
                    field: selected.descriptor.name,
                    policy: MenuWritePolicy::LifecycleRequired,
                }),
                "{name} must reject ordinary updates before value validation"
            );
        }
        Ok(())
    }

    #[test]
    fn missing_numeric_domains_and_binary_storage_are_not_admitted() -> TestResult {
        for name in [
            "radio.TimeZone",
            "gps.Distance",
            "aprs.SlowRateTime",
            "aprs.FilterPositionLimit",
            "aprs.ObjectUsedNo",
            "aprs.IconSymbol",
            "dv.ReflectorHostsSelect",
        ] {
            let selected = field(name)?;
            assert_eq!(
                selected.validate_ordinary_value(FieldValue::Unsigned(0)),
                Err(MenuWritePolicyError::NotOrdinary {
                    field: selected.descriptor.name,
                    policy: MenuWritePolicy::UnresolvedDomain,
                }),
                "{name} storage bounds must not replace its semantic domain"
            );
        }
        for name in ["radio.PoweronBitmap", "ipnet.IpAddress"] {
            let selected = field(name)?;
            assert_eq!(
                selected.validate_ordinary_value(FieldValue::Bytes(&[0; 4])),
                Err(MenuWritePolicyError::NotOrdinary {
                    field: selected.descriptor.name,
                    policy: MenuWritePolicy::Binary,
                }),
                "{name} binary storage must not become a scalar update"
            );
        }
        Ok(())
    }

    #[test]
    fn every_group_link_has_the_exact_noncontiguous_domain() -> TestResult {
        for index in 0..30 {
            let selected = field(&format!("radio.GroupLink{index}"))?;
            assert_eq!(
                selected.write_policy(),
                MenuWritePolicy::Ordinary,
                "group link {index} has a supported supplemental domain"
            );
            for raw in 0..=256 {
                let result = selected.validate_ordinary_value(FieldValue::Unsigned(raw));
                if raw <= 29 || raw == 255 {
                    result?;
                } else {
                    assert_eq!(
                        result,
                        Err(MenuWritePolicyError::Schema(SchemaError::DisallowedValue {
                            field: selected.descriptor.name,
                            value: raw,
                        })),
                        "group link {index} must reject raw value {raw}"
                    );
                }
            }
        }
        Ok(())
    }

    #[test]
    fn my_and_message_selectors_reject_out_of_domain_values_and_types() -> TestResult {
        for name in ["dv.MyCallsignSelectDvGateway", "dv.MyDvMessageSelect"] {
            let selected = field(name)?;
            for raw in 0..=5 {
                selected.validate_ordinary_value(FieldValue::Unsigned(raw))?;
            }
            for raw in [6, 255, u64::MAX] {
                assert_eq!(
                    selected.validate_ordinary_value(FieldValue::Unsigned(raw)),
                    Err(MenuWritePolicyError::Schema(SchemaError::DisallowedValue {
                        field: selected.descriptor.name,
                        value: raw,
                    })),
                    "{name} must reject out-of-domain selector {raw}"
                );
            }
            assert_eq!(
                selected.validate_ordinary_value(FieldValue::Bool(false)),
                Err(MenuWritePolicyError::Schema(SchemaError::TypeMismatch {
                    field: selected.descriptor.name,
                    expected: "unsigned",
                    actual: "boolean",
                })),
                "{name} requires an unsigned selector, not a boolean"
            );
        }
        Ok(())
    }

    #[test]
    fn ordinary_admission_still_validates_text_and_registry_codecs() -> TestResult {
        let name = field("pm.PmName1")?;
        assert_eq!(
            name.validate_ordinary_value(FieldValue::Text("A\0B")),
            Err(MenuWritePolicyError::EmbeddedTerminator {
                field: "pm.PmName1",
            }),
            "ordinary typed text must reject embedded storage terminators"
        );
        assert_eq!(
            name.validate_ordinary_value(FieldValue::Text("12345678901234567")),
            Err(MenuWritePolicyError::Schema(SchemaError::Codec {
                field: "pm.PmName1",
                source: kenwood_schema::CodecError::TextTooLong {
                    actual: 17,
                    max: 16
                },
            })),
            "ordinary text must retain its byte-length bound"
        );
        let meter = field("radio.MeterType")?;
        assert_eq!(
            meter.validate_ordinary_value(FieldValue::Unsigned(255)),
            Err(MenuWritePolicyError::Schema(SchemaError::DisallowedValue {
                field: "radio.MeterType",
                value: 255,
            })),
            "ordinary admission must retain the registry enum domain"
        );
        Ok(())
    }

    #[test]
    fn full_numeric_storage_requires_a_domain_and_registry_counts_stay_reviewed() {
        let mut ordinary = 0;
        let mut unresolved = 0;
        let mut lifecycle = 0;
        let mut binary = 0;
        for selected in MCP_D750_MENU_FIELDS {
            match selected.write_policy() {
                MenuWritePolicy::Ordinary => ordinary += 1,
                MenuWritePolicy::UnresolvedDomain => unresolved += 1,
                MenuWritePolicy::LifecycleRequired => lifecycle += 1,
                MenuWritePolicy::Binary => binary += 1,
            }
            if selected.options.is_empty()
                && selected.allowed_values.is_empty()
                && numeric_capacity_only(selected.descriptor.codec)
                && selected.write_policy() == MenuWritePolicy::Ordinary
            {
                assert!(
                    supplemental_domain(selected.descriptor.name).is_some(),
                    "unresolved full-storage numeric domain admitted: {}",
                    selected.descriptor.name
                );
            }
        }
        assert_eq!(
            (ordinary, unresolved, lifecycle, binary),
            (350, 15, 60, 6),
            "registry or policy changes require reviewing every admission class"
        );
    }

    #[test]
    fn unlisted_numeric_fallbacks_fail_closed_but_explicit_domains_remain() -> TestResult {
        let mut selected = *field("radio.Beep")?;
        selected.descriptor.name = "radio.FutureNumericField";
        for codec in [
            FieldCodec::Byte {
                min: 0,
                max: u8::MAX,
            },
            FieldCodec::Unsigned {
                width: 2,
                endian: Endian::Little,
                min: 0,
                max: 65_535,
            },
            FieldCodec::Unsigned {
                width: 8,
                endian: Endian::Big,
                min: 0,
                max: u64::MAX,
            },
            FieldCodec::Signed {
                width: 2,
                endian: Endian::Little,
                min: -32_768,
                max: 32_767,
            },
            FieldCodec::Signed {
                width: 8,
                endian: Endian::Big,
                min: i64::MIN,
                max: i64::MAX,
            },
            FieldCodec::BitField {
                mask: 0x0C,
                shift: 2,
                min: 0,
                max: 3,
            },
            FieldCodec::BitField {
                mask: 0x04,
                shift: 2,
                min: 0,
                max: 1,
            },
        ] {
            selected.descriptor.codec = codec;
            assert_eq!(
                selected.write_policy(),
                MenuWritePolicy::UnresolvedDomain,
                "unlisted numeric fallback must fail closed: {codec:?}"
            );
            selected.allowed_values = &[0, 1];
            assert_eq!(
                selected.write_policy(),
                MenuWritePolicy::Ordinary,
                "explicit choices establish a domain for {codec:?}"
            );
            selected.allowed_values = &[];
        }
        for codec in [
            FieldCodec::Byte { min: 2, max: 12 },
            FieldCodec::Unsigned {
                width: 2,
                endian: Endian::Little,
                min: 2,
                max: 1800,
            },
            FieldCodec::Signed {
                width: 4,
                endian: Endian::Little,
                min: -500,
                max: 15_000,
            },
            FieldCodec::BitField {
                mask: 0x0C,
                shift: 2,
                min: 0,
                max: 2,
            },
        ] {
            selected.descriptor.codec = codec;
            assert_eq!(
                selected.write_policy(),
                MenuWritePolicy::Ordinary,
                "a semantic bound narrower than capacity remains supported: {codec:?}"
            );
        }
        Ok(())
    }

    #[test]
    fn coordinate_hemisphere_bits_keep_their_exact_two_choice_domain() -> TestResult {
        for (list, count) in [("gps.MyPositionList", 5), ("aprs.ObjectList", 3)] {
            for index in 0..count {
                for member in ["NorthSouth", "EastWest"] {
                    let selected = field(&format!("{list}[{index}].{member}"))?;
                    for raw in [0, 1] {
                        selected.validate_ordinary_value(FieldValue::Unsigned(raw))?;
                    }
                    assert_eq!(
                        selected.validate_ordinary_value(FieldValue::Unsigned(2)),
                        Err(MenuWritePolicyError::Schema(SchemaError::DisallowedValue {
                            field: selected.descriptor.name,
                            value: 2,
                        })),
                        "{} must reject values outside its two hemispheres",
                        selected.descriptor.name
                    );
                }
            }
        }
        Ok(())
    }

    #[test]
    fn every_my_callsign_entry_preserves_supported_text_and_rejects_invalid_input() -> TestResult {
        for index in 0..6 {
            let selected = field(&format!(
                "dv.MyCallsignDvGatewayList[{index}].MyCallsignDvGateway"
            ))?;
            for text in [
                "", "A", "ABCDE", "KQ4NIT", "ABCDEF  ", "A      C", "KQ4NIT C", "KQ4NITZ9",
            ] {
                selected.validate_ordinary_value(FieldValue::Text(text))?;
                let assignment = crate::MenuAssignment::new(
                    selected.descriptor.name,
                    Some(crate::SlotIndex::new(0)?),
                    text,
                )?;
                assert_eq!(
                    assignment.value(),
                    &crate::memory::DecodedFieldValue::Text(text.to_owned()),
                    "MY entry {index} must preserve exact valid text, including clearing and spaces"
                );
            }
            for text in [
                "kq4nit",
                "KQ4NIT/",
                "é",
                "        ",
                " KQ4NIT",
                "KQ 4NIT",
                "A     B",
                "123456789",
                "PRIVATE!",
            ] {
                let error = selected.validate_ordinary_value(FieldValue::Text(text));
                assert_eq!(
                    error,
                    Err(MenuWritePolicyError::InvalidMyCallsign {
                        field: selected.descriptor.name,
                    }),
                    "MY entry {index} must reject invalid characters, shape, or byte length"
                );
                let Err(error) = error else {
                    return Err("invalid MY text was accepted".into());
                };
                assert!(
                    !error.to_string().contains(text),
                    "MY validation diagnostics must not echo input"
                );
                assert!(
                    crate::MenuAssignment::new(
                        selected.descriptor.name,
                        Some(crate::SlotIndex::new(0)?),
                        text,
                    )
                    .is_err(),
                    "ordinary assignment construction must enforce MY entry {index} validation"
                );
            }
            for suffix in b"GIS0123456789" {
                let text = format!("KQ4NIT {}", char::from(*suffix));
                assert_eq!(
                    selected.validate_ordinary_value(FieldValue::Text(&text)),
                    Err(MenuWritePolicyError::InvalidMyCallsign {
                        field: selected.descriptor.name
                    }),
                    "MY entry {index} must reject each reserved final module after a space"
                );
            }
            assert_eq!(
                selected.validate_ordinary_value(FieldValue::Unsigned(0)),
                Err(MenuWritePolicyError::Schema(SchemaError::TypeMismatch {
                    field: selected.descriptor.name,
                    expected: "text",
                    actual: "unsigned",
                })),
                "MY entry {index} requires typed text"
            );
        }
        Ok(())
    }
}
