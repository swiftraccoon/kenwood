//! Fixed catalog mapping changed addresses to candidate field locations.
//!
//! A match names the field whose registry range covers the address; addresses
//! outside the catalog reach the caller as unclassified changes.

use kenwood_tmd750::memory::{
    FieldCodec, FieldDescriptor, ReflectorTerminalPreflight, SLOT_TERM, StringEncoding,
    TextSetting, menu_field,
};
use kenwood_tmd750::protocol::mcp::regions;
use kenwood_tmd750::{Address, Region, SlotIndex};
use serde::Serialize;

use crate::{AppResult, CommandError};

const CALLSIGNS: [TextSetting; 6] = [
    TextSetting::DstarMyCallsign1,
    TextSetting::DstarMyCallsign2,
    TextSetting::DstarMyCallsign3,
    TextSetting::DstarMyCallsign4,
    TextSetting::DstarMyCallsign5,
    TextSetting::DstarMyCallsign6,
];

const MEMOS: [TextSetting; 6] = [
    TextSetting::DstarMemo1,
    TextSetting::DstarMemo2,
    TextSetting::DstarMemo3,
    TextSetting::DstarMemo4,
    TextSetting::DstarMemo5,
    TextSetting::DstarMemo6,
];

/// One field's location in the generated registry layout.
#[derive(Debug, Clone, Serialize)]
pub(super) struct FieldLocation {
    /// Stable public menu or text-setting key, not a generated member name.
    pub(super) key: &'static str,
    /// Zero-based PM slot; globals have no slot and zero means PM Off.
    pub(super) slot: Option<u8>,
    /// Address of the field's first byte.
    pub(super) start: u32,
    /// Full encoded width, including text padding.
    pub(super) length: usize,
}

impl FieldLocation {
    fn end(&self) -> AppResult<u32> {
        self.start
            .checked_add(u32::try_from(self.length)?)
            .ok_or_else(|| {
                CommandError(format!("Terminal catalog range overflows for {}", self.key)).into()
            })
    }

    fn contains(&self, address: Address) -> bool {
        address
            .as_u32()
            .checked_sub(self.start)
            .and_then(|offset| usize::try_from(offset).ok())
            .is_some_and(|offset| offset < self.length)
    }
}

/// Every catalogued field across all PM slots, sorted by absolute address.
#[derive(Debug)]
pub(super) struct Catalog {
    fields: Vec<FieldLocation>,
}

#[derive(Debug, Clone, Copy)]
enum Shape {
    Byte,
    Text(usize),
}

#[derive(Debug, Clone, Copy)]
struct Specification {
    key: &'static str,
    shape: Shape,
    per_slot: bool,
}

impl Catalog {
    /// Build the catalog from the registry, checking its ranges do not overlap.
    ///
    /// Reads no capture.
    pub(super) fn new() -> AppResult<Self> {
        let mut catalog = Self { fields: Vec::new() };
        for descriptor in ReflectorTerminalPreflight::required_fields()? {
            catalog.append(preflight_specification(descriptor)?, descriptor)?;
        }
        for setting in MEMOS {
            let metadata = setting.metadata()?;
            let field = menu_field(metadata.field_name).ok_or_else(|| {
                CommandError(format!("Terminal catalog descriptor missing for {setting}"))
            })?;
            catalog.append(
                Specification {
                    key: setting.key(),
                    shape: Shape::Text(4),
                    per_slot: true,
                },
                &field.descriptor,
            )?;
        }
        catalog.validate()
    }

    /// The catalogued field covering `address`, or `None` when it is unclassified.
    pub(super) fn classify(&self, address: Address) -> Option<&FieldLocation> {
        self.fields.iter().find(|field| field.contains(address))
    }

    fn append(
        &mut self,
        specification: Specification,
        descriptor: &FieldDescriptor,
    ) -> AppResult<()> {
        let scope_matches = if specification.per_slot {
            descriptor.terms == [SLOT_TERM]
        } else {
            descriptor.terms.is_empty()
        };
        let shape_matches = match (specification.shape, descriptor.codec) {
            (Shape::Byte, FieldCodec::Byte { .. }) => true,
            (
                Shape::Text(expected),
                FieldCodec::FixedString {
                    len,
                    encoding: StringEncoding::Utf8,
                    padding: 0,
                },
            ) => len == expected,
            _ => false,
        };
        if !scope_matches || !shape_matches {
            return Err(Box::new(CommandError(format!(
                "Terminal catalog descriptor shape changed for {}",
                specification.key
            ))));
        }
        if specification.per_slot {
            for slot in SlotIndex::all() {
                self.append_location(specification.key, descriptor, Some(slot))?;
            }
        } else {
            self.append_location(specification.key, descriptor, None)?;
        }
        Ok(())
    }

    fn append_location(
        &mut self,
        key: &'static str,
        descriptor: &FieldDescriptor,
        slot: Option<SlotIndex>,
    ) -> AppResult<()> {
        self.fields.push(FieldLocation {
            key,
            slot: slot.map(SlotIndex::index),
            start: descriptor.address(slot)?.as_u32(),
            length: descriptor.codec.encoded_len(),
        });
        Ok(())
    }

    fn validate(mut self) -> AppResult<Self> {
        self.fields.sort_by_key(|field| field.start);
        let coverage = regions::menu_regions();
        for field in &self.fields {
            let range = Region::new(field.start, field.end()?)?;
            if !coverage.iter().any(|region| region.contains_region(range)) {
                return Err(Box::new(CommandError(format!(
                    "Terminal catalog field {} is outside standard configuration coverage",
                    field.key
                ))));
            }
        }
        for (left, right) in self.fields.iter().zip(self.fields.iter().skip(1)) {
            if left.end()? > right.start {
                return Err(Box::new(CommandError(format!(
                    "Terminal catalog fields {} and {} overlap",
                    left.key, right.key
                ))));
            }
        }
        Ok(self)
    }
}

fn preflight_specification(descriptor: &FieldDescriptor) -> AppResult<Specification> {
    let (key, shape, per_slot) = match descriptor.name {
        "format.Version" => ("image-format", Shape::Byte, false),
        "pm.PmSelect" => ("active-pm", Shape::Byte, false),
        "radio.UsbFunction" => ("menu-980-usb-function", Shape::Byte, true),
        "radio.DvGatewayInterface" => ("menu-986-gateway-interface", Shape::Byte, true),
        "dv.DvGatewayModeDvGateway" => ("menu-650-gateway-mode", Shape::Byte, true),
        "dv.MyCallsignSelectDvGateway" => ("menu-651-my-selection", Shape::Byte, true),
        "dv.SelectTerminalMode" => ("menu-670-terminal-type", Shape::Byte, true),
        "dv.RPT1DvGateway" => ("menu-671-rpt1", Shape::Text(8), true),
        "dv.RPT2DvGateway" => ("menu-672-rpt2", Shape::Text(8), true),
        _ => {
            for setting in CALLSIGNS {
                if setting.metadata()?.field_name == descriptor.name {
                    return Ok(Specification {
                        key: setting.key(),
                        shape: Shape::Text(8),
                        per_slot: true,
                    });
                }
            }
            return Err(Box::new(CommandError(
                "Terminal preflight exposed an unsupported catalog field".to_owned(),
            )));
        }
    };
    Ok(Specification {
        key,
        shape,
        per_slot,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use kenwood_tmd750::memory::Term;

    type TestResult = AppResult<()>;

    #[test]
    fn catalog_has_two_globals_and_nineteen_fields_in_each_pm_slot() -> TestResult {
        let catalog = Catalog::new()?;
        assert_eq!(catalog.fields.len(), 116, "finite field count changed");
        assert_eq!(
            catalog
                .fields
                .iter()
                .map(|field| field.length)
                .sum::<usize>(),
            560,
            "candidate ranges must cover complete fields, including padding"
        );
        assert_eq!(
            catalog
                .fields
                .iter()
                .filter(|field| field.slot.is_none())
                .count(),
            2,
            "global format and active PM must not be duplicated for every slot"
        );
        for slot in SlotIndex::all() {
            assert_eq!(
                catalog
                    .fields
                    .iter()
                    .filter(|field| field.slot == Some(slot.index()))
                    .count(),
                19,
                "every slot must include all six callsign and memo pairs"
            );
        }
        Ok(())
    }

    #[test]
    fn every_field_is_half_open_and_ordered_without_overlap() -> TestResult {
        let catalog = Catalog::new()?;
        for field in &catalog.fields {
            for offset in 0..field.length {
                let address = Address::new(field.start)?.checked_add(u32::try_from(offset)?)?;
                let found = catalog
                    .classify(address)
                    .ok_or("field byte was unclassified")?;
                assert_eq!((found.key, found.slot), (field.key, field.slot));
            }
            for address in [field.start - 1, field.end()?] {
                assert!(
                    catalog
                        .classify(Address::new(address)?)
                        .is_none_or(|found| { (found.key, found.slot) != (field.key, field.slot) }),
                    "field {} must exclude adjacent byte {address}",
                    field.key
                );
            }
        }
        assert!(
            catalog
                .fields
                .iter()
                .zip(catalog.fields.iter().skip(1))
                .all(|(a, b)| a.start < b.start),
            "catalog order must be deterministic by absolute address"
        );
        Ok(())
    }

    #[test]
    fn scalar_text_and_memo_locations_match_independently_pinned_boundaries() -> TestResult {
        let catalog = Catalog::new()?;
        for (address, key, slot, length) in [
            (10, "image-format", None, 1),
            (323_593, "active-pm", None, 1),
            (329_037, "menu-986-gateway-interface", Some(0), 1),
            (331_784, "dstar-my-callsign-1", Some(0), 8),
            (331_792, "dstar-memo-1", Some(0), 4),
            (331_852 + 5 * 8192, "dstar-memo-6", Some(5), 4),
            (331_864 + 5 * 8192, "menu-672-rpt2", Some(5), 8),
        ] {
            let field = catalog
                .classify(Address::new(address)?)
                .ok_or("pinned field missing")?;
            assert_eq!(
                (field.key, field.slot, field.start, field.length),
                (key, slot, address, length)
            );
        }
        Ok(())
    }

    #[test]
    fn descriptor_addresses_and_lengths_agree_in_every_slot() -> TestResult {
        let catalog = Catalog::new()?;
        let mut descriptors = ReflectorTerminalPreflight::required_fields()?;
        for setting in MEMOS {
            let metadata = setting.metadata()?;
            descriptors.push(
                &menu_field(metadata.field_name)
                    .ok_or("memo missing")?
                    .descriptor,
            );
        }
        for descriptor in descriptors {
            for slot in SlotIndex::all() {
                let address = descriptor.address(Some(slot))?;
                let field = catalog.classify(address).ok_or("descriptor unclassified")?;
                assert_eq!(field.start, address.as_u32());
                assert_eq!(field.length, descriptor.codec.encoded_len());
                assert_eq!(field.slot, descriptor.is_per_slot().then_some(slot.index()));
            }
        }
        Ok(())
    }

    #[test]
    fn omitted_image_bytes_and_other_settings_remain_unclassified() -> TestResult {
        let catalog = Catalog::new()?;
        for address in [0, 8, 48, 323_594, 329_038, 331_779, 393_216] {
            assert!(
                catalog.classify(Address::new(address)?).is_none(),
                "address {address} is not a candidate Terminal field"
            );
        }
        Ok(())
    }

    #[test]
    fn serialized_locations_expose_public_keys_and_no_schema_member_names() -> TestResult {
        let json = serde_json::to_string(&Catalog::new()?.fields)?;
        for internal in ["format.Version", "pm.PmSelect", "DvGateway", "pm_slot"] {
            assert!(
                !json.contains(internal),
                "internal member {internal} leaked into output"
            );
        }
        assert!(
            json.contains("dstar-memo-6"),
            "memo identity must remain explicit"
        );
        Ok(())
    }

    #[test]
    fn masked_wrong_width_wrong_padding_and_wrong_scope_descriptors_are_rejected() {
        let mut catalog = Catalog { fields: Vec::new() };
        let byte = Specification {
            key: "test-scalar",
            shape: Shape::Byte,
            per_slot: false,
        };
        for codec in [FieldCodec::BitBool { mask: 1 }, FieldCodec::Bool] {
            let descriptor = FieldDescriptor::new("fixture", 10, codec);
            assert!(
                catalog.append(byte, &descriptor).is_err(),
                "masked or non-byte shape must fail"
            );
        }
        let text = Specification {
            key: "test-text",
            shape: Shape::Text(4),
            per_slot: false,
        };
        for (len, encoding, padding) in [
            (8, StringEncoding::Utf8, 0),
            (4, StringEncoding::MemoryMap, 0),
            (4, StringEncoding::Utf8, 32),
        ] {
            let descriptor = FieldDescriptor::new(
                "fixture",
                10,
                FieldCodec::FixedString {
                    len,
                    encoding,
                    padding,
                },
            );
            assert!(
                catalog.append(text, &descriptor).is_err(),
                "text storage shape must match exactly"
            );
        }
        let slotted = Specification {
            per_slot: true,
            ..byte
        };
        for terms in [
            &[][..],
            &[Term {
                dimension: "pm_slot",
                stride: 4096,
            }][..],
            &[Term {
                dimension: "other",
                stride: 8192,
            }][..],
            &[SLOT_TERM, SLOT_TERM][..],
        ] {
            let descriptor = FieldDescriptor::with_terms(
                "fixture",
                10,
                terms,
                FieldCodec::Byte { min: 0, max: 255 },
            );
            assert!(
                catalog.append(slotted, &descriptor).is_err(),
                "slot term must match exactly once"
            );
        }
        let descriptor = FieldDescriptor::with_terms(
            "fixture",
            10,
            &[SLOT_TERM],
            FieldCodec::Byte { min: 0, max: 255 },
        );
        assert!(
            catalog.append(byte, &descriptor).is_err(),
            "global field cannot acquire a slot term"
        );
        assert!(
            catalog.fields.is_empty(),
            "rejected shapes must add no locations"
        );
    }

    #[test]
    fn overlapping_unread_empty_and_overflowing_ranges_are_rejected() {
        let field = FieldLocation {
            key: "fixture",
            slot: None,
            start: 10,
            length: 1,
        };
        assert!(
            (Catalog {
                fields: vec![field.clone(), field.clone()]
            })
            .validate()
            .is_err(),
            "overlapping candidate fields must never use first-match attribution"
        );
        for (start, length) in [(0, 1), (8, 41), (10, 0), (u32::MAX, 1)] {
            let invalid = FieldLocation {
                start,
                length,
                ..field.clone()
            };
            assert!(
                (Catalog {
                    fields: vec![invalid]
                })
                .validate()
                .is_err(),
                "invalid or uncaptured range {start}+{length} must fail"
            );
        }
    }
}
