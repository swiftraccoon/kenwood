//! Cross-record manifest validation before projection, generation or comparison.

use std::collections::{BTreeMap, BTreeSet};

use crate::address::{Address, RecordBase};
use crate::error::{ExtractError, Result};
use crate::manifest::{
    Codec, Dimension, Domain, ManifestCandidate, Menu, OffsetLayout, Record, RecordEntry, Role,
    SCHEMA_VERSION, StorageTransform,
};

fn require(condition: bool, message: impl Into<String>) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(ExtractError::InvalidInput {
            message: message.into(),
        })
    }
}

fn unique<'a, I: IntoIterator<Item = &'a str>>(values: I, context: &str) -> Result<()> {
    let mut seen = BTreeSet::new();
    for value in values {
        require(
            !value.is_empty() && seen.insert(value),
            format!("{context}: empty or duplicate name {value:?}"),
        )?;
    }
    Ok(())
}

fn codec(codec: &Codec) -> Result<u64> {
    let value_type = codec.value_type();
    require(
        value_type != Some(crate::ValueType::Unknown),
        "unsupported codec value type",
    )?;
    require(
        (value_type == Some(crate::ValueType::Enum)) == codec.enum_type().is_some(),
        "enum value type and catalog reference disagree",
    )?;
    require(
        codec.csharp_type().is_none_or(|kind| !kind.is_empty()),
        "empty C# type provenance",
    )?;
    let semantic_kind = match codec {
        Codec::Bool { .. } => matches!(
            value_type,
            Some(crate::ValueType::Bool | crate::ValueType::Constant)
        ),
        Codec::FixedString { .. } => matches!(
            value_type,
            Some(crate::ValueType::String | crate::ValueType::Constant)
        ),
        Codec::RawBytes { encoding, .. } => {
            matches!(
                value_type,
                Some(crate::ValueType::Bytes | crate::ValueType::Constant)
            ) || (*encoding == Some(crate::RawEncoding::Ipv4DottedQuad)
                && value_type == Some(crate::ValueType::String))
        }
        Codec::Byte { .. }
        | Codec::BitField { .. }
        | Codec::UnsignedLe { .. }
        | Codec::SignedLe { .. } => matches!(
            value_type,
            Some(
                crate::ValueType::Bool
                    | crate::ValueType::Byte
                    | crate::ValueType::Int
                    | crate::ValueType::Uint
                    | crate::ValueType::Enum
                    | crate::ValueType::Constant
            )
        ),
        Codec::ClearRange { .. } => value_type.is_none(),
    };
    require(semantic_kind, "codec and semantic value type disagree")?;
    match codec {
        Codec::BitField { bit, width, .. } => require(
            *width != 0 && bit.checked_add(*width).is_some_and(|end| end <= 8),
            "bit field exceeds one byte",
        )?,
        Codec::UnsignedLe { width, .. } | Codec::SignedLe { width, .. } => require(
            (1..=8).contains(width),
            "integer codec width must be 1 through 8 bytes",
        )?,
        Codec::FixedString { padding, .. } => {
            require(
                u8::try_from(*padding).is_ok(),
                "fixed-string padding exceeds one byte",
            )?;
        }
        Codec::ClearRange { fill, .. } => {
            require(u8::try_from(*fill).is_ok(), "clear fill exceeds one byte")?;
        }
        Codec::RawBytes {
            encoding: Some(crate::RawEncoding::Ipv4DottedQuad),
            length,
            ..
        } => require(*length == Some(4), "invalid encoded raw-byte field")?,
        Codec::Byte { .. } | Codec::Bool { .. } | Codec::RawBytes { encoding: None, .. } => {}
    }
    let length = codec
        .encoded_length()
        .ok_or_else(|| ExtractError::InvalidInput {
            message: "codec has no verified byte length".to_owned(),
        })?;
    require(length != 0, "codec length is zero")?;
    Ok(length)
}

fn codec_bounds(codec: &Codec) -> Option<(i128, i128)> {
    match codec {
        Codec::Byte { .. } => Some((0, 255)),
        Codec::Bool { .. } => Some((0, 1)),
        Codec::BitField { width, .. } => Some((0, (1_i128 << width) - 1)),
        Codec::UnsignedLe { width, .. } => Some((0, (1_i128 << (width * 8)) - 1)),
        Codec::SignedLe { width, .. } => {
            let limit = 1_i128 << (width * 8 - 1);
            Some((-limit, limit - 1))
        }
        Codec::FixedString { .. } | Codec::RawBytes { .. } | Codec::ClearRange { .. } => None,
    }
}

fn field_contract(
    role: Role,
    codec: &Codec,
    value_domain: Option<&Domain>,
    writable: Option<bool>,
    reason: Option<&str>,
) -> Result<()> {
    require(
        (role == Role::Clear) == matches!(codec, Codec::ClearRange { .. }),
        "clear role and codec disagree",
    )?;
    require(
        (role == Role::Constant) == (codec.value_type() == Some(crate::ValueType::Constant)),
        "constant role and value type disagree",
    )?;
    require(
        (writable == Some(false)) == reason.is_some(),
        "write restriction and reason disagree",
    )?;
    require(
        reason.is_none_or(|value| !value.is_empty()),
        "empty write-restriction reason",
    )?;
    domain(value_domain)?;
    if let Some(value_domain) = value_domain {
        let (minimum, maximum) = codec_bounds(codec).ok_or_else(|| ExtractError::InvalidInput {
            message: "integer domain on non-integer codec".to_owned(),
        })?;
        let fits = |value: i64| (minimum..=maximum).contains(&i128::from(value));
        require(
            match value_domain {
                Domain::Range { min, max, .. } => fits(*min) && fits(*max),
                Domain::Choices { allowed_values, .. } => {
                    allowed_values.iter().all(|value| fits(*value))
                }
            },
            "field domain exceeds its encoded width",
        )?;
    }
    Ok(())
}

fn enum_catalogs(catalogs: &[crate::manifest::EnumCatalog]) -> Result<()> {
    unique(
        catalogs.iter().map(|catalog| catalog.name.as_str()),
        "enum catalog names",
    )?;
    for catalog in catalogs {
        unique(
            catalog.options.iter().map(|option| option.member.as_str()),
            "enum member names",
        )?;
        require(
            !catalog.options.is_empty() && !catalog.csharp_name.is_empty(),
            "incomplete enum catalog",
        )?;
        let bounds = match catalog.underlying_type.as_str() {
            "byte" => (0, i128::from(u8::MAX)),
            "sbyte" => (i128::from(i8::MIN), i128::from(i8::MAX)),
            "short" => (i128::from(i16::MIN), i128::from(i16::MAX)),
            "ushort" => (0, i128::from(u16::MAX)),
            "int" => (i128::from(i32::MIN), i128::from(i32::MAX)),
            "uint" => (0, i128::from(u32::MAX)),
            "long" => (i128::from(i64::MIN), i128::from(i64::MAX)),
            "ulong" => (0, i128::from(u64::MAX)),
            _ => {
                return Err(ExtractError::InvalidInput {
                    message: "unsupported C# enum underlying type".to_owned(),
                });
            }
        };
        require(
            catalog
                .options
                .iter()
                .all(|option| (bounds.0..=bounds.1).contains(&i128::from(option.value))),
            "enum member exceeds its underlying integer domain",
        )?;
    }
    Ok(())
}

fn enum_reference(
    codec: &Codec,
    catalogs: &BTreeMap<&str, &crate::manifest::EnumCatalog>,
) -> Result<()> {
    if let Some(name) = codec.enum_type() {
        let catalog = catalogs
            .get(name)
            .ok_or_else(|| ExtractError::InvalidInput {
                message: format!("missing enum catalog {name}"),
            })?;
        let (minimum, maximum) = codec_bounds(codec).ok_or_else(|| ExtractError::InvalidInput {
            message: "enum uses a non-integer codec".to_owned(),
        })?;
        require(
            catalog
                .options
                .iter()
                .all(|option| (minimum..=maximum).contains(&i128::from(option.value))),
            "enum member exceeds its field codec",
        )?;
    }
    Ok(())
}

fn domain(domain: Option<&Domain>) -> Result<()> {
    match domain {
        Some(Domain::Range {
            min,
            max,
            step,
            provenance,
        }) => {
            require(
                min <= max && *step != 0 && !provenance.is_empty(),
                "invalid range domain",
            )?;
        }
        Some(Domain::Choices {
            allowed_values,
            provenance,
        }) => {
            require(
                !allowed_values.is_empty() && !provenance.is_empty(),
                "empty choices domain or provenance",
            )?;
            require(
                allowed_values
                    .windows(2)
                    .all(|pair| matches!(pair, [left, right] if left < right)),
                "choices must be strictly increasing",
            )?;
        }
        None => {}
    }
    Ok(())
}

fn transform(transform: Option<&StorageTransform>) -> Result<()> {
    if let Some(transform) = transform {
        require(
            !transform.input_unit.is_empty()
                && !transform.encode.is_empty()
                && !transform.decode.is_empty()
                && matches!(u32::try_from(transform.numerator), Ok(1..))
                && matches!(u32::try_from(transform.denominator), Ok(1..)),
            "invalid storage transform",
        )?;
    }
    Ok(())
}

fn address(
    address: &Address,
    length: u64,
    dimensions: &[Dimension],
    image_length: u64,
) -> Result<()> {
    unique(
        address.terms.iter().map(|term| term.dimension.as_str()),
        "address dimensions",
    )?;
    let mut last = address.base;
    for term in &address.terms {
        let dimension = dimensions
            .iter()
            .find(|dimension| dimension.name == term.dimension)
            .ok_or_else(|| ExtractError::InvalidInput {
                message: format!("undeclared address dimension {}", term.dimension),
            })?;
        require(
            term.stride != 0
                && dimension
                    .anchors
                    .iter()
                    .any(|anchor| anchor.stride == term.stride),
            "dimension stride is not declared by an anchor",
        )?;
        last = dimension
            .count
            .checked_sub(1)
            .and_then(|index| index.checked_mul(term.stride))
            .and_then(|scaled| last.checked_add(scaled))
            .ok_or_else(|| ExtractError::InvalidInput {
                message: "address dimension overflows".to_owned(),
            })?;
    }
    require(
        last.checked_add(length)
            .is_some_and(|end| end <= image_length),
        "field exceeds the image at a dimension boundary",
    )
}

fn layout(layout: &OffsetLayout, count: u64) -> Result<Vec<u64>> {
    require(
        (layout.kind == crate::RecordLayout::Fixed) == layout.stride.is_none(),
        "record layout and stride disagree",
    )?;
    require(
        layout.stride.is_none_or(|stride| stride != 0),
        "record stride is zero",
    )?;
    require(
        layout.kind != crate::RecordLayout::Fixed || count == 1,
        "fixed record layout requires one record",
    )?;
    require(
        (layout.kind == crate::RecordLayout::LinearWithOverride) != layout.overrides.is_empty(),
        "record override kind and entries disagree",
    )?;
    unique(
        layout.overrides.iter().map(|(index, _)| index.as_str()),
        "record override indices",
    )?;
    let overrides = layout
        .overrides
        .iter()
        .map(|(text, base)| {
            let index = text
                .parse::<u64>()
                .map_err(|error| ExtractError::source("parse record override index", error))?;
            require(
                index.to_string() == *text,
                "record override index is not canonical decimal",
            )?;
            Ok((index, *base))
        })
        .collect::<Result<Vec<_>>>()?;
    RecordBase {
        base: layout.base,
        stride: layout.stride.unwrap_or(0),
        overrides,
        terms: layout.terms.clone(),
    }
    .bases(count)
}

fn record(
    record: &Record,
    dimensions: &[Dimension],
    image_length: u64,
    catalogs: &BTreeMap<&str, &crate::manifest::EnumCatalog>,
) -> Result<()> {
    require(
        !record.source_class.is_empty()
            && !record.source_file.is_empty()
            && !record.write_method.is_empty()
            && record.write_method_line != 0,
        "incomplete record provenance",
    )?;
    require(
        record.count != 0 && record.count <= image_length,
        "record count exceeds image byte capacity",
    )?;
    let bases = layout(&record.offset_layout, record.count)?;
    require(
        bases == record.record_base_offsets,
        "record bases disagree with their layout",
    )?;
    require(
        record.operation_count_per_record == record.fields.len() as u64,
        "record operation count differs",
    )?;
    let fields: Vec<_> = record
        .fields
        .iter()
        .filter(|field| field.role == Role::Field)
        .collect();
    require(
        record.field_count_per_record == fields.len() as u64,
        "record public field count differs",
    )?;
    unique(
        record
            .fields
            .iter()
            .filter_map(|field| field.name.as_deref()),
        "record field names",
    )?;
    for (sequence, field) in record.fields.iter().enumerate() {
        require(
            field.sequence == sequence as u64,
            "record field sequence differs",
        )?;
        let _length = codec(&field.codec)?;
        field_contract(
            field.role,
            &field.codec,
            field.domain.as_ref(),
            field.writable,
            field.not_writable_reason.as_deref(),
        )?;
        enum_reference(&field.codec, catalogs)?;
        transform(field.storage_transform.as_ref())?;
        require(
            field.role != Role::Field || field.name.is_some(),
            "public record field has no name",
        )?;
        if let Some(aliases) = &field.aliases {
            unique(aliases.iter().map(String::as_str), "record aliases")?;
        }
        for base in &bases {
            let base = base.checked_add(field.relative_offset).ok_or_else(|| {
                ExtractError::InvalidInput {
                    message: "record field offset overflows".to_owned(),
                }
            })?;
            address(
                &Address {
                    base,
                    terms: record.offset_layout.terms.clone(),
                },
                codec(&field.codec)?,
                dimensions,
                image_length,
            )?;
        }
    }
    expanded_record(record, &fields, &bases, dimensions, image_length)
}

fn expanded_record(
    record: &Record,
    fields: &[&crate::manifest::RecordField],
    bases: &[u64],
    dimensions: &[Dimension],
    image_length: u64,
) -> Result<()> {
    let expected = record
        .count
        .checked_mul(fields.len() as u64)
        .ok_or_else(|| ExtractError::InvalidInput {
            message: "expanded field count overflows".to_owned(),
        })?;
    require(
        record.expanded_fields.len() as u64 == expected,
        "expanded field count differs",
    )?;
    unique(
        record
            .expanded_fields
            .iter()
            .map(|field| field.name.as_str()),
        "expanded field names",
    )?;
    for field in &record.expanded_fields {
        let base = usize::try_from(field.record_index)
            .ok()
            .and_then(|index| bases.get(index))
            .ok_or_else(|| ExtractError::InvalidInput {
                message: "expanded field index exceeds record count".to_owned(),
            })?;
        let source = fields
            .iter()
            .find(|source| {
                source.name.as_ref().is_some_and(|name| {
                    field.name == format!("{}[{}].{name}", record.name, field.record_index)
                })
            })
            .ok_or_else(|| ExtractError::InvalidInput {
                message: format!("expanded field {} has no source", field.name),
            })?;
        require(
            base.checked_add(source.relative_offset) == Some(field.offset)
                && field.address.base == field.offset
                && field.address.terms == record.offset_layout.terms
                && field.offset_hex == crate::offset_hex(field.offset)
                && field.codec == source.codec
                && field.domain == source.domain
                && field.storage_transform == source.storage_transform
                && field.aliases == source.aliases
                && field.writable == source.writable
                && field.not_writable_reason == source.not_writable_reason
                && field.writer_class == record.source_class,
            "expanded field disagrees with its source",
        )?;
        address(
            &field.address,
            codec(&field.codec)?,
            dimensions,
            image_length,
        )?;
    }
    Ok(())
}

fn menu_provenance(menu: &Menu) -> Result<()> {
    require(
        !menu.public_name.is_empty()
            && !menu.csharp_class.is_empty()
            && !menu.source_file.is_empty()
            && !menu.write_method.is_empty()
            && menu.write_method_line != 0,
        "incomplete menu provenance",
    )?;
    let detail = [
        menu.detail_class.is_some(),
        menu.detail_write_method.is_some(),
        menu.detail_write_method_line.is_some(),
    ];
    require(
        detail
            .iter()
            .all(|present| *present == detail.first().copied().unwrap_or(false)),
        "incomplete detail-writer provenance",
    )?;
    require(
        menu.detail_class
            .as_ref()
            .is_none_or(|class| !class.is_empty())
            && menu
                .detail_write_method
                .as_ref()
                .is_none_or(|method| !method.is_empty())
            && menu.detail_write_method_line.is_none_or(|line| line != 0),
        "empty detail-writer provenance",
    )?;
    Ok(())
}

fn menu(menu: &Menu, dimensions: &[Dimension], image_length: u64) -> Result<()> {
    menu_provenance(menu)?;
    require(
        menu.operation_count == menu.operations.len() as u64,
        "menu operation count differs",
    )?;
    require(
        menu.field_count
            == menu
                .operations
                .iter()
                .filter(|operation| operation.role == Role::Field)
                .count() as u64,
        "menu public field count differs",
    )?;
    unique(
        menu.operations
            .iter()
            .filter_map(|operation| operation.name.as_deref()),
        "menu operation names",
    )?;
    enum_catalogs(&menu.enum_types)?;
    let catalogs: BTreeMap<_, _> = menu
        .enum_types
        .iter()
        .map(|catalog| (catalog.name.as_str(), catalog))
        .collect();
    for (sequence, operation) in menu.operations.iter().enumerate() {
        require(
            operation.sequence == sequence as u64,
            "menu operation sequence differs",
        )?;
        require(
            operation.offset == operation.address.base
                && operation.offset_hex == crate::offset_hex(operation.offset),
            "operation address views disagree",
        )?;
        require(
            operation.role != Role::Field || operation.name.is_some(),
            "public operation has no name",
        )?;
        address(
            &operation.address,
            codec(&operation.codec)?,
            dimensions,
            image_length,
        )?;
        field_contract(
            operation.role,
            &operation.codec,
            operation.domain.as_ref(),
            operation.writable,
            operation.not_writable_reason.as_deref(),
        )?;
        enum_reference(&operation.codec, &catalogs)?;
        require(
            operation.writer_class == menu.csharp_class
                || menu.detail_class.as_ref() == Some(&operation.writer_class),
            "operation writer is outside its menu",
        )?;
    }
    unique(
        menu.repeated_records.iter().map(|entry| match entry {
            RecordEntry::Extracted(record) => record.name.as_str(),
            RecordEntry::Unsupported(record) => record.name.as_str(),
        }),
        "record names",
    )?;
    for entry in &menu.repeated_records {
        match entry {
            RecordEntry::Extracted(value) => record(value, dimensions, image_length, &catalogs)?,
            RecordEntry::Unsupported(value) => {
                require(
                    value.count != 0
                        && value.count <= image_length
                        && value.call_count != 0
                        && !value.unsupported_public_reason.is_empty(),
                    "invalid private record catalog",
                )?;
                for base in layout(&value.offset_layout, value.count)? {
                    address(
                        &Address {
                            base,
                            terms: value.offset_layout.terms.clone(),
                        },
                        1,
                        dimensions,
                        image_length,
                    )?;
                }
            }
        }
    }
    Ok(())
}

fn provenance(candidate: &ManifestCandidate) -> Result<()> {
    let model = candidate.model.radio.specification();
    require(
        candidate.model.product == model.product()
            && candidate.model.mcp_product == model.mcp_product()
            && candidate.model.image_length == model.image_length(),
        "model identity or image length differs from the supported radio",
    )?;
    require(
        candidate.generator == env!("CARGO_PKG_NAME"),
        "unknown generator",
    )?;
    let digest_valid =
        |digest: &str| digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit());
    require(
        digest_valid(&candidate.source.normalized_source_sha256),
        "invalid normalized-source SHA-256",
    )?;
    require(
        !candidate.source.writer_class.is_empty() && !candidate.source.resource_class.is_empty(),
        "incomplete source class provenance",
    )?;
    if let Some(language) = &candidate.source.language_file {
        require(
            !language.file_name.is_empty() && digest_valid(&language.sha256),
            "invalid language-file provenance",
        )?;
    }
    require(
        candidate.dimensions.len() == model.dimensions.len(),
        "dimension roster differs from model",
    )?;
    for (dimension, expected) in candidate.dimensions.iter().zip(model.dimensions) {
        require(
            dimension.name == expected.name
                && dimension.count == expected.count
                && dimension.anchors.len() == expected.anchors.len(),
            "dimension differs from model",
        )?;
        for (anchor, expected) in dimension.anchors.iter().zip(expected.anchors) {
            require(
                anchor.property == expected.property && anchor.stride == expected.stride,
                "dimension anchor differs from model",
            )?;
        }
    }
    require(
        candidate.menus.len() == model.menus.len(),
        "menu roster differs from model",
    )?;
    for (menu, expected) in candidate.menus.iter().zip(model.menus) {
        require(
            menu.menu == expected.key && menu.public_name == expected.property,
            "menu identity or order differs from model",
        )?;
    }
    source_maps(candidate)
}

fn source_maps(candidate: &ManifestCandidate) -> Result<()> {
    let source = &candidate.source;
    unique(
        source
            .serializer_classes
            .iter()
            .map(|(key, _)| key.as_str()),
        "source serializers",
    )?;
    unique(
        source.write_methods.iter().map(|(key, _)| key.as_str()),
        "source writer methods",
    )?;
    unique(
        source.detail_classes.iter().map(|(key, _)| key.as_str()),
        "source detail classes",
    )?;
    require(
        source.serializer_classes.len() == candidate.menus.len()
            && source.write_methods.len() == candidate.menus.len()
            && source.detail_classes.len()
                == candidate
                    .menus
                    .iter()
                    .filter(|menu| menu.detail_class.is_some())
                    .count(),
        "source-map roster differs from menus",
    )?;
    for menu in &candidate.menus {
        require(
            source
                .serializer_classes
                .iter()
                .any(|(key, class)| key == &menu.menu && class == &menu.csharp_class),
            "source serializer differs from menu",
        )?;
        let method = source
            .write_methods
            .iter()
            .find(|(key, _)| key == &menu.menu)
            .map(|(_, method)| method)
            .ok_or_else(|| ExtractError::InvalidInput {
                message: "missing source writer method".to_owned(),
            })?;
        require(
            method.class == menu.csharp_class
                && method.line == menu.write_method_line
                && menu
                    .write_method
                    .split_once('(')
                    .is_some_and(|(name, _)| name == method.method),
            "source writer method differs from menu",
        )?;
        if let Some(detail) = &menu.detail_class {
            require(
                source
                    .detail_classes
                    .iter()
                    .any(|(key, class)| key == &menu.menu && class == detail),
                "source detail class differs from menu",
            )?;
        }
    }
    Ok(())
}

pub(crate) fn manifest(candidate: &ManifestCandidate) -> Result<()> {
    require(
        candidate.schema_version == SCHEMA_VERSION,
        "unsupported manifest schema version",
    )?;
    provenance(candidate)?;
    require(
        !candidate.generator.is_empty()
            && !candidate.model.product.is_empty()
            && !candidate.model.mcp_product.is_empty()
            && !candidate.release.mcp_version.is_empty()
            && !candidate.release.assembly_version.is_empty()
            && !candidate.release.firmware_target.is_empty(),
        "manifest identity is incomplete",
    )?;
    require(
        [
            &candidate.release.mcp_version,
            &candidate.release.assembly_version,
            &candidate.release.firmware_target,
        ]
        .iter()
        .all(|label| label.bytes().all(|byte| byte.is_ascii_graphic())),
        "release labels require ASCII words",
    )?;
    unique(
        candidate
            .dimensions
            .iter()
            .map(|dimension| dimension.name.as_str()),
        "dimensions",
    )?;
    for dimension in &candidate.dimensions {
        require(
            dimension.count != 0 && !dimension.anchors.is_empty(),
            "dimension has no indices or anchors",
        )?;
        unique(
            dimension
                .anchors
                .iter()
                .map(|anchor| anchor.property.as_str()),
            "dimension anchors",
        )?;
        require(
            dimension.anchors.iter().all(|anchor| anchor.stride != 0),
            "dimension anchor stride is zero",
        )?;
    }
    unique(
        candidate.menus.iter().map(|menu| menu.menu.as_str()),
        "menus",
    )?;
    for value in &candidate.menus {
        menu(value, &candidate.dimensions, candidate.model.image_length)?;
    }
    let summary = &candidate.summary;
    let expected = crate::extract::summarize(
        &candidate.menus,
        candidate.dimensions.len(),
        usize::try_from(summary.combo_enum_type_count)
            .map_err(|error| ExtractError::source("combo type count", error))?,
        usize::try_from(summary.combo_option_mapping_count)
            .map_err(|error| ExtractError::source("combo option count", error))?,
    )?;
    require(
        *summary == expected,
        "manifest summary differs from its records",
    )
}
