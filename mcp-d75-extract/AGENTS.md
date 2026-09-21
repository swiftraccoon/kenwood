# mcp-d75-extract

## Non-negotiables

- **The TH-D75 registry is the regression proof.** Regenerating
  `thd75/src/memory/menu_fields.rs` from the real MCP-D75 assembly must
  reproduce the committed file byte for byte; only a deliberate bump changes
  `MCP_D75_SCHEMA_VERSION`. Any other changed byte is an extractor defect, and
  no generated output is ever hand-edited.
- **Both manifests are gated.** `extract --check --strict-known-layout` must
  pass for `thd75/data/mcp_d75_menu_schema.json` and
  `tmd750/data/mcp_d750_menu_schema.json` before any commit touching this
  crate. Regenerate deliberately and say why in the commit.
- **Manifests are typed and deterministic.** JSON key order is serde struct
  declaration order; every list is in source or spec order or sorted; no
  timestamps, absolute paths or `HashMap` iteration in an output.
- **Nothing obfuscated in `src/model/`.** Writer class, write methods, resource
  singleton, per-slot detail classes and record-list backing fields are
  discovered in `src/discovery.rs` with exactly-one-candidate checks, recorded
  only as `source.*` provenance. The sole pinned private identifiers are the
  position-record storage symbols in `record_symbols`.
- **Fail closed.** Every unresolved offset, ambiguous discovery, unverified
  pinned fact, unmatched nested call or untyped written value is an
  `ExtractError` naming class, method and line. No warnings, no partial
  manifests, no `value_type: "unknown"` in an output.
- **Fixtures are pinned inputs.** `tests/fixtures/` reproduces structural facts
  from the reviewed decompilations; never regenerate or reformat them.
- The TM-D750 callsign codec keeps the official writer's zero padding; the
  space-padding override is TH-D75 only, and
  `callsign_records_preserve_the_official_nul_padding` pins both.
- `cargo test` covers the pinned fixtures only; the real assemblies are
  exercised solely by the gated `extract --check` commands, whose inputs are
  local and not reproducible from the repository.
- `property_re` and `nested_call_line_re` need negative lookahead
  (`fancy_regex`); anchor fancy patterns with `^[ \t]*`, never `^\s*`, or the
  class index's blanked `own_text` blows the backtracking limit. Everything
  else uses `regex`.

## Structural rules learned from the real programs

- **Anchor setters are not menu properties.** Every per-slot class, private
  sub-writers included, exposes the public `OffsetProgrammableMemoryAddress`
  setter, so `ModelSpec::is_anchor` excludes declared anchors when deciding
  whether a nested-call target is a record list or a private writer.
- **Fixed-base private writers pin their lowest address.** `verify_fixed_base`
  gathers candidates from assignments and from the offset argument of every
  classifiable direct write, requires the pinned base to appear, and rejects
  any symbolic candidate below it. Literal-only assignments are loop counters
  and only ever confirm a base.
- **Enum types resolve through the class index.** D750 detail classes declare
  properties with enums of the serializer class; `ClassIndex::types` resolves
  nested, qualified and enclosing-class enums to `<declaring class>.<enum>` at
  classification, the codec carries `enum_type` onward, and
  `attach_enum_catalogs` reads definitions by that name. Combo labels are keyed
  by the declaring class, so they join without further mapping.

## Firmware release workflow

1. Unpack the new MCP installer, then `7z x` the MSI and its `Data1.cab`.
2. Run the gated `--check` command for that radio; it fails on the changed
   source digest.
3. Extract to a scratch path with the new `--mcp-version` and `--firmware`,
   without `--strict-known-layout` on the first run. Resolve every error by
   extending the spec, never by loosening a check.
4. `cargo run -q -p mcp-d75-extract -- diff <committed> <scratch>` and review
   every line; moved or re-coded fields point at hand-written code to change.
5. Update the spec's `reviewed` counts, extract strictly to the committed path,
   regenerate the TH-D75 registry, update the accepted firmware identities in
   the radio crate, run the full gate, and tag the previous commit
   `thd75-fw-<old>` or `tmd750-fw-<old>` if older firmware support matters.
