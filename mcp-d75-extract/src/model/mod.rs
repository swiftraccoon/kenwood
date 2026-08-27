//! Per-radio specs: the stable, reviewable facts the sources alone obscure.
//!
//! A spec names public container properties, dimension anchors, record list
//! names and counts, audited domains, and reviewed counts. It never names an
//! obfuscated identifier; those are discovered (see `discovery`) and recorded
//! in the manifest's `source` provenance. The one deliberate exception is the
//! position-record symbol table, which maps the decompiler's private field
//! names inside the coordinate writers to their public meaning.

use crate::manifest::{Domain, StorageTransform};

mod thd75;
mod tmd750;

pub use thd75::THD75;
pub use tmd750::TMD750;

/// One top-level menu serializer: spec key and public container property.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MenuSpec {
    /// Manifest menu key (`radio`).
    pub key: &'static str,
    /// Public property on the memory-map container (`RadioMenuData`).
    pub property: &'static str,
}

/// A public setter property through which a class receives `stride * index`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnchorSpec {
    /// Property name (`OffsetProgrammableMemoryAddress`).
    pub property: &'static str,
    /// Bytes assigned per index.
    pub stride: u64,
}

/// One declared index dimension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DimensionSpec {
    /// Dimension name (`pm_slot`).
    pub name: &'static str,
    /// Index count.
    pub count: u64,
    /// Anchors; the first is mandatory on every per-slot class, the rest optional.
    pub anchors: &'static [AnchorSpec],
}

/// A pinned literal that an owner assigns to a record's base-address property.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BaseOverride {
    /// Public setter property (`StartAddress`).
    pub property: &'static str,
    /// Literal the owner assigns; verified against the source.
    pub value: u64,
}

/// One public repeated-record list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecordSpec {
    /// Owning menu key.
    pub menu: &'static str,
    /// Public list property (`MyPositionList`).
    pub list: &'static str,
    /// Reviewed record count (the writer's loop bound).
    pub count: u64,
    /// Pinned base-address property, when the owner assigns one.
    pub base_override: Option<BaseOverride>,
}

/// One private sub-writer that exposes no public menu properties.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrivateWriterSpec {
    /// Owning menu key.
    pub menu: &'static str,
    /// Catalog name (`private_pair_848`).
    pub name: &'static str,
    /// Slot-zero base; verified against the writer's formula or first assignment.
    pub base: u64,
    /// Record stride for indexed writers; `None` for a fixed blob.
    pub stride: Option<u64>,
    /// Nested calls the menu writer makes into this class.
    pub calls: u64,
    /// Records the writer covers.
    pub count: u64,
    /// Why it is not extracted.
    pub reason: &'static str,
}

/// A large persistent data field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlobSpec {
    /// Public property name.
    pub field: &'static str,
    /// Whether sparse radio writes may target it.
    pub writable: bool,
    /// Reason when not writable.
    pub reason: Option<&'static str>,
}

/// Fill byte for a fixed string whose stored form is not NUL-padded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaddingOverride {
    /// Record class.
    pub class: &'static str,
    /// Field name.
    pub field: &'static str,
    /// Fill byte.
    pub padding: u64,
}

/// A private helper method wrapping a property before a raw-bytes write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ValueHelperSpec {
    /// Public property passed to the helper.
    pub property: &'static str,
    /// Bytes the helper produces.
    pub length: u64,
    /// Manifest encoding tag (`ipv4_dotted_quad`).
    pub encoding: &'static str,
}

/// Counts pinned by review and checked by `--strict-known-layout`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReviewedLayout {
    /// Direct operation count per menu key.
    pub operation_counts: &'static [(&'static str, u64)],
    /// Combo-backed enum types.
    pub combo_enum_types: usize,
    /// Combo option mappings.
    pub combo_options: usize,
}

/// Storage transform data in `'static` form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StorageTransformSpec {
    /// Display unit.
    pub input_unit: &'static str,
    /// Encode formula.
    pub encode: &'static str,
    /// Decode formula.
    pub decode: &'static str,
    /// Multiplier.
    pub numerator: i64,
    /// Divisor.
    pub denominator: i64,
}

impl StorageTransformSpec {
    /// Owned manifest form.
    #[must_use]
    pub fn to_manifest(self) -> StorageTransform {
        StorageTransform {
            kind: "scaled_integer".to_owned(),
            input_unit: self.input_unit.to_owned(),
            encode: self.encode.to_owned(),
            decode: self.decode.to_owned(),
            numerator: self.numerator,
            denominator: self.denominator,
        }
    }
}

/// Public meaning of one decompiled storage symbol inside a record writer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SymbolOverride {
    /// Decompiled expression (`base.c`, `e`, `ab`).
    pub symbol: &'static str,
    /// Public name.
    pub name: &'static str,
    /// C# type used for codec classification.
    pub csharp_type: &'static str,
    /// Role override (`internal` for record markers); `None` keeps `field`.
    pub role: Option<&'static str>,
    /// Alternative names.
    pub aliases: &'static [&'static str],
    /// Encoded-storage transform.
    pub storage_transform: Option<StorageTransformSpec>,
    /// Reason when the field must not be written.
    pub not_writable_reason: Option<&'static str>,
}

/// Audited domains for direct fields keyed by `menu.Name`.
pub type DirectDomains = Vec<(String, Domain)>;

/// Audited domains for record fields keyed by `(class, field)`.
pub type RecordDomains = Vec<((&'static str, &'static str), Domain)>;

/// Everything stable about one radio's MCP program.
#[derive(Debug, Clone, Copy)]
pub struct ModelSpec {
    /// Spec id used by `--model`.
    pub id: &'static str,
    /// Marketing name.
    pub product: &'static str,
    /// Expected `AssemblyProduct`.
    pub mcp_product: &'static str,
    /// Writer image length.
    pub image_length: u64,
    /// Menus in manifest order.
    pub menus: &'static [MenuSpec],
    /// Index dimensions.
    pub dimensions: &'static [DimensionSpec],
    /// Public record lists.
    pub records: &'static [RecordSpec],
    /// Private sub-writers.
    pub private_writers: &'static [PrivateWriterSpec],
    /// Blob fields.
    pub blobs: &'static [BlobSpec],
    /// Fixed-string padding overrides.
    pub padding_overrides: &'static [PaddingOverride],
    /// Helper-wrapped raw-bytes values.
    pub value_helpers: &'static [ValueHelperSpec],
    /// Reviewed counts.
    pub reviewed: ReviewedLayout,
    /// Symbol tables per record class.
    pub record_symbols: &'static [(&'static str, &'static [SymbolOverride])],
    /// Audited domains for direct fields keyed by `menu.Name`.
    pub direct_domains: fn() -> DirectDomains,
    /// Audited domains for record fields keyed by `(class, field)`.
    pub record_domains: fn() -> RecordDomains,
}

impl ModelSpec {
    /// Menu spec by key.
    #[must_use]
    pub fn menu(&self, key: &str) -> Option<&'static MenuSpec> {
        self.menus.iter().find(|menu| menu.key == key)
    }

    /// Record lists owned by a menu, in spec order.
    #[must_use]
    pub fn records_for(&self, menu: &str) -> Vec<&'static RecordSpec> {
        self.records
            .iter()
            .filter(|record| record.menu == menu)
            .collect()
    }

    /// Private writers owned by a menu, in spec order.
    #[must_use]
    pub fn private_writers_for(&self, menu: &str) -> Vec<&'static PrivateWriterSpec> {
        self.private_writers
            .iter()
            .filter(|writer| writer.menu == menu)
            .collect()
    }

    /// Blob spec by field name.
    #[must_use]
    pub fn blob(&self, field: &str) -> Option<&'static BlobSpec> {
        self.blobs.iter().find(|blob| blob.field == field)
    }

    /// Padding override for `(class, field)`.
    #[must_use]
    pub fn padding_override(&self, class: &str, field: &str) -> Option<u64> {
        self.padding_overrides
            .iter()
            .find(|entry| entry.class == class && entry.field == field)
            .map(|entry| entry.padding)
    }

    /// Value helper for a public property.
    #[must_use]
    pub fn value_helper(&self, property: &str) -> Option<&'static ValueHelperSpec> {
        self.value_helpers
            .iter()
            .find(|helper| helper.property == property)
    }

    /// Symbol table for a record class.
    #[must_use]
    pub fn record_symbols(&self, class: &str) -> &'static [SymbolOverride] {
        self.record_symbols
            .iter()
            .find(|(name, _)| *name == class)
            .map_or(&[], |(_, symbols)| symbols)
    }

    /// Audited domain for a direct field.
    #[must_use]
    pub fn direct_domain(&self, key: &str) -> Option<Domain> {
        (self.direct_domains)()
            .into_iter()
            .find(|(name, _)| name == key)
            .map(|(_, domain)| domain)
    }

    /// Audited domain for a record field.
    #[must_use]
    pub fn record_domain(&self, class: &str, field: &str) -> Option<Domain> {
        (self.record_domains)()
            .into_iter()
            .find(|((owner, name), _)| *owner == class && *name == field)
            .map(|(_, domain)| domain)
    }

    /// Dimension by name.
    #[must_use]
    pub fn dimension(&self, name: &str) -> Option<&'static DimensionSpec> {
        self.dimensions
            .iter()
            .find(|dimension| dimension.name == name)
    }

    /// True when `property` is a declared dimension anchor setter.
    ///
    /// Anchor setters are public properties on per-slot classes, including
    /// private sub-writers, so they never count as menu properties.
    #[must_use]
    pub fn is_anchor(&self, property: &str) -> bool {
        self.dimensions
            .iter()
            .flat_map(|dimension| dimension.anchors)
            .any(|anchor| anchor.property == property)
    }
}

/// Look up a spec by its `--model` id.
#[must_use]
pub fn model_by_id(id: &str) -> Option<&'static ModelSpec> {
    match id {
        "thd75" => Some(&THD75),
        "tmd750" => Some(&TMD750),
        _ => None,
    }
}

/// Build an inclusive range domain.
#[must_use]
pub fn range_domain(min: i64, max: i64, provenance: &str) -> Domain {
    Domain::Range {
        min,
        max,
        step: 1,
        provenance: provenance.to_owned(),
    }
}

/// Build an explicit choices domain.
#[must_use]
pub fn choices_domain(values: Vec<i64>, provenance: &str) -> Domain {
    Domain::Choices {
        allowed_values: values,
        provenance: provenance.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn looks_up_models_by_id() {
        assert_eq!(model_by_id("thd75").map(|spec| spec.id), Some("thd75"));
        assert_eq!(model_by_id("tmd750").map(|spec| spec.id), Some("tmd750"));
        assert!(model_by_id("tm-d710").is_none());
    }

    #[test]
    fn thd75_spec_matches_the_reviewed_layout() {
        assert_eq!(THD75.menus.len(), 4);
        assert!(THD75.dimensions.is_empty());
        assert_eq!(THD75.records.len(), 7);
        assert_eq!(THD75.records_for("aprs").len(), 4);
        assert_eq!(THD75.private_writers_for("radio").len(), 2);
        assert_eq!(
            THD75.reviewed.operation_counts,
            &[("radio", 134), ("gps", 17), ("aprs", 85), ("dv", 31)]
        );
        assert_eq!(
            THD75.padding_override("MyCallsignDvGatewayData", "MyCallsignDvGateway"),
            Some(32)
        );
        assert!(THD75.direct_domain("radio.TimeZone").is_some());
        assert!(THD75.record_domain("ObjectData", "ObjectSymbol").is_some());
        assert_eq!(THD75.record_symbols("MyPositionData").len(), 14);
        assert_eq!(THD75.record_symbols("ObjectData").len(), 12);
        assert!(THD75.record_symbols("StatusTextData").is_empty());
    }
}
