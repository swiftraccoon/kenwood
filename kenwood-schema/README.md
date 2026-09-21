# kenwood-schema

Model-neutral field codecs, finite writable domains, and atomic masked-byte
planning. The crate performs no I/O and has no radio-model dependency.

The shared engine owns scalar encoding and decoding, byte-order conversion,
fixed-text interpretation, masked-bit merging, and conflict detection. Model
libraries retain their generated catalogs, descriptor identity checks, address
resolution, memory geometry, writable regions, and radio-session policy.

## Choose the right API

For radio configuration, start with the
[TH-D75 schema facade](https://swiftraccoon.github.io/kenwood/kenwood_thd75/memory/schema/index.html)
in `kenwood-thd75` or the
[TM-D750 schema facade](https://swiftraccoon.github.io/kenwood/kenwood_tmd750/memory/schema/index.html)
in `kenwood-tmd750`. They expose these same scalar types while adding the
model's catalog identity, address resolution, and writable-region checks.
Calling this crate directly performs none of them.

Use this crate directly when implementing a model-neutral codec consumer or a
new model facade. It is an unpublished workspace package, not a crates.io
release. An external Cargo package can select it from the Git workspace:

```toml
[dependencies]
kenwood-schema = { git = "https://github.com/swiftraccoon/kenwood" }
```

For a local checkout, use a path relative to the consuming `Cargo.toml` instead:

```toml
[dependencies]
kenwood-schema = { path = "../kenwood/kenwood-schema" }
```

Start with `FieldCodec::encode` or `FieldCodec::decode`, apply finite catalog
constraints with `validate_choices`, then collect resolved assignments with
`ByteClaims::merge_atomic`. The connected recipe below shows the boundary
between field-relative codec bytes and caller-owned image addresses.

## Encode a field

`FieldCodec` describes storage. Every encode and decode validates its metadata,
including integer width and representable domains, masks, and nonempty lengths.
`FieldValue` is borrowed input; `DecodedFieldValue` owns decoded output.
Encoded bytes carry offsets relative to the start of the field.

Metadata validity is not a resource limit. `FixedString` and `Bytes` accept
any positive storage length. Before encoding runtime-supplied metadata, cap
`FieldCodec::encoded_len()` against your application's memory budget and
address space: encoding allocates one `MaskedByte` per storage byte, not one
byte of output per byte of input. Text and raw-byte decoding also allocate
owned output. The codec does not inherit a model facade's image-size limit.

```rust
use kenwood_schema::{FieldCodec, FieldValue, TextPolicy, ValueDomain};

let codec = FieldCodec::BitField {
    mask: 0x1C,
    shift: 2,
    min: 0,
    max: 7,
};
let encoded = codec.encode(
    FieldValue::Unsigned(5),
    ValueDomain::Writable,
    TextPolicy::ExactPadding,
)?;
assert_eq!(encoded.len(), 1);
for byte in encoded {
    assert_eq!(byte.offset(), 0);
    assert_eq!(byte.mask(), 0x1C);
    assert_eq!(byte.value(), 0x14);
    assert_eq!(byte.apply(0xE3), 0xF7);
}
# Ok::<(), Box<dyn std::error::Error>>(())
```

`ValueDomain::Writable` enforces the codec's declared numeric range.
`ValueDomain::Stored` permits any value representable by its storage width;
it does not make that value writable. Finite enum and choice constraints are
separate: `validate_choices` requires membership in every nonempty supplied
domain. Its API example demonstrates rejection by either domain. A model
validates both storage and catalog policy before planning.

## Decode with an explicit policy

`FieldCodec::decode` takes exactly one complete field slice. It does not
resolve image addresses, fill missing bytes, or establish snapshot coverage.
`DecodeOptions` makes interpretation choices explicit:

| Policy | Contract |
| --- | --- |
| `BooleanDecoding::Canonical` | A whole-byte boolean must be exactly zero or one. |
| `BooleanDecoding::NonZero` | A whole-byte boolean decodes any nonzero byte as true. |
| `TextPolicy::ExactPadding` | NUL padding requires an all-NUL tail; non-NUL padding is removed only from the end. Encoding rejects text that would not round-trip exactly. |
| `TextPolicy::FirstTerminator` | Decoding stops at the first NUL or padding byte; later bytes are ignored. Encoding rejects embedded NUL and padding bytes. |

Both text policies require valid UTF-8 in the semantic text they select.
`StringEncoding::MemoryMap` is the printable-ASCII subset only, without
guessing a model's extended character encoding. The `TextPolicy` example
shows the same stored bytes being rejected or truncated by the two policies.
A policy selects how stored bytes are interpreted; it does not state which
values a given firmware accepts.

```rust
use kenwood_schema::{
    BooleanDecoding, DecodeOptions, DecodedFieldValue, Endian, FieldCodec,
    TextPolicy, ValueDomain,
};

let codec = FieldCodec::Unsigned {
    width: 2,
    endian: Endian::Little,
    min: 0,
    max: 100,
};
let stored = codec.decode(&[200, 0], DecodeOptions {
    domain: ValueDomain::Stored,
    boolean: BooleanDecoding::Canonical,
    text: TextPolicy::ExactPadding,
})?;
assert_eq!(stored, DecodedFieldValue::Unsigned(200));
# Ok::<(), Box<dyn std::error::Error>>(())
```

## Encode, resolve, merge, and apply

`MaskedByte` validates nonempty bit ownership and rejects values outside the
mask. Its offset is meaningful only in the caller's address space.
`ByteClaims::merge_atomic` coalesces disjoint bits and accepts equal repeated
assignments. Conflicting values reject the entire batch, including its earlier
bytes, without changing prior claims. This also applies to conflicts within
one incoming batch. Diagnostics retain the first owner of each claimed bit.
The `ByteClaims::merge_atomic` API example demonstrates rejection followed by
reuse of the unchanged planner.

This synthetic eight-byte image has one caller-defined writable region. Each
field span is checked against that region before its encoding allocates output.
Each encoded byte is then relocated with checked arithmetic before entering the
shared planner. No image byte changes until every assignment has merged.

```rust
use kenwood_schema::{ByteClaims, Endian, FieldCodec, FieldValue, TextPolicy, ValueDomain};

let mut image = [0xE3; 8];
let writable = 2..7;
let fields = [
    ("flags", 2_usize, FieldCodec::BitField {
        mask: 0x1C, shift: 2, min: 0, max: 7,
    }, FieldValue::Unsigned(5)),
    ("counter", 5_usize, FieldCodec::Unsigned {
        width: 2, endian: Endian::Little, min: 0, max: u16::MAX.into(),
    }, FieldValue::Unsigned(0x1234)),
];
let mut claims = ByteClaims::new();
for (name, base, codec, value) in fields {
    codec.validate()?;
    let end = base.checked_add(codec.encoded_len()).ok_or("field span overflow")?;
    let span = base..end;
    let _original_field = image.get(span.clone()).ok_or("field outside image")?;
    if span.start < writable.start || span.end > writable.end {
        return Err("field outside writable region".into());
    }
    let encoded = codec.encode(value, ValueDomain::Writable, TextPolicy::ExactPadding)?;
    let resolved = encoded.into_iter().map(|byte| {
        let offset = base.checked_add(byte.offset()).ok_or("byte offset overflow")?;
        if !span.contains(&offset) {
            return Err("encoded byte outside admitted field");
        }
        Ok(byte.with_offset(offset))
    }).collect::<Result<Vec<_>, &str>>()?;
    claims.merge_atomic(name, &resolved)?;
}

for (_owner, byte) in claims.into_claims() {
    let original = image.get_mut(byte.offset()).ok_or("claim outside image")?;
    *original = byte.apply(*original);
}
assert_eq!(image, [0xE3, 0xE3, 0xF7, 0xE3, 0xE3, 0x34, 0x12, 0xE3]);
# Ok::<(), Box<dyn std::error::Error>>(())
```

The model resolves relative field offsets before merging. Address-space,
image-bound, and writable-region checks belong to the model's planner; they
must all pass before it produces a usable page plan. Finished claims are sorted
by offset and grouped into model-specific page types. Everything between a
merged plan and a device write stays in the model crate: fresh-page guards,
recovery journaling, readback, protocol deadlines, cancellation, and restoration.

Errors are typed as `CodecError`, `ChoiceError`, and `PatchError`. Codec offsets
are field-relative; patch offsets use the caller's resolved address space.
Models attach field names and their own addressing or validation errors.

Experimental API; breaking changes prioritize correctness and clarity.
License: GPL-2.0-or-later.
