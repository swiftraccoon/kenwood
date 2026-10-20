# kenwood-schema

Model-neutral scalar codecs and atomic masked-byte planning for
`kenwood-thd75` and `kenwood-tmd750`. No I/O, no clock, no model dependency;
`thiserror` is the only dependency.

## Rules

- The boundary is scalar storage: encoding, decoding, byte order, fixed text, masked bits and claim conflicts. Catalogs, descriptor identity, address resolution, image bounds, writable regions, page types and session policy stay in each model's `memory::schema` facade.
- `FieldCodec::new` validates `CodecSpec` before encoding or decoding is possible; its fields stay private. Both generated registries construct constant codecs, so changing a specification variant or its fields means changing the templates in `mcp-d75-extract/src/rustgen.rs` and regenerating both registries with the commands in `mcp-d75-extract/AGENTS.md`.
- `ByteClaims::merge_atomic` is all or nothing: a conflict anywhere in a batch, including between two of its own bytes, leaves every earlier claim intact, and diagnostics name the first owner of the conflicting bit. The `patch/tests.rs` cases (`a_late_conflict_preserves_all_existing_claims_and_rejects_the_prefix`, `conflicts_within_a_new_batch_leave_no_claims`, `failed_batch_can_be_followed_by_a_valid_assignment`) pin it.
- `StringEncoding::MemoryMap` is printable ASCII only. A model's extended character set needs its own established contract; never add a guessed code page here.
- `ValueDomain::Stored` admits any value the storage width can hold, for decoding what a radio stored; it never makes that value writable. Writes use `ValueDomain::Writable` plus `validate_choices` for catalog domains.
- Metadata validity is not a resource limit: `CodecSpec::FixedString` and `CodecSpec::Bytes` accept any positive length and encoding allocates one `MaskedByte` per storage byte, so callers cap `FieldCodec::encoded_len()` before encoding runtime-supplied metadata.
