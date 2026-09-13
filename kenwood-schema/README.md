# kenwood-schema

Model-neutral field codecs, finite writable domains, and atomic masked-byte
planning. The crate performs no I/O and has no radio-model dependency.

The shared engine owns scalar encoding and decoding, byte-order conversion,
fixed-text interpretation, masked-bit merging, and conflict detection. Model
libraries retain their generated catalogs, descriptor identity checks, address
resolution, memory geometry, writable regions, and radio-session policy.

## Encode a field

`FieldCodec` describes storage. Every encode and decode validates its metadata,
including integer width and representable domains, masks, and nonempty lengths.
`FieldValue` is borrowed input; `DecodedFieldValue` owns decoded output.
Encoded bytes carry offsets relative to the start of the field.

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
domain. A model validates both storage and catalog policy before planning.

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

Both text policies require valid UTF-8. `StringEncoding::MemoryMap` is the
printable-ASCII subset only, without guessing a model's extended character
encoding. These policies describe interpretation, not firmware qualification.

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

## Merge assignments atomically

`MaskedByte` validates nonempty bit ownership and rejects values outside the
mask. Its offset is meaningful only in the caller's address space.
`ByteClaims::merge_atomic` coalesces disjoint bits and accepts equal repeated
assignments. Conflicting values reject the entire batch, including its earlier
bytes, without changing prior claims. This also applies to conflicts within
one incoming batch. Diagnostics retain the first owner of each claimed bit.

```rust
use kenwood_schema::{ByteClaims, MaskedByte};

let mut claims = ByteClaims::new();
let lower = MaskedByte::new(8, 0x0F, 0x05)?;
claims.merge_atomic("lower", &[lower])?;
claims.merge_atomic("same request", &[lower])?;
claims.merge_atomic("upper", &[MaskedByte::new(8, 0xF0, 0xA0)?])?;

let merged: Vec<_> = claims.into_claims().collect();
assert_eq!(merged, [("lower", MaskedByte::new(8, 0xFF, 0xA5)?)]);
# Ok::<(), Box<dyn std::error::Error>>(())
```

The model resolves relative field offsets before merging. Address-space,
image-bound, and writable-region checks belong to the model's planner; they
must all pass before it produces a usable page plan. Finished claims are sorted
by offset and grouped into model-specific page types. A merged plan is not
permission to write a device: fresh-page guards, durable intent, readback,
protocol deadlines, cancellation, and restoration remain outside this crate.

Errors are typed as `CodecError`, `ChoiceError`, and `PatchError`. Codec offsets
are field-relative; patch offsets use the caller's resolved address space.
Models attach field names and their own addressing or admission errors.

Experimental API; breaking changes prioritize correctness and clarity.
License: GPL-2.0-or-later.
