# lodestar-core + mcp-d75-extract Review

## lodestar-core (Rust ↔ Swift FFI)

### Real bugs / lossy behavior

- `lodestar-core/src/mmdvm.rs:81` — the `From<mmdvm_core::MmdvmError>` catch-all maps every unknown upstream variant to `Self::BadStart { actual: 0 }`. Swift receives an error text `"expected 0xE0 frame-start, got 0x00"` regardless of the real cause (e.g. `VersionPayloadDecode`, `StatusPayloadDecode`). Should have an `Other(String)` variant; today diagnostics are misleading and the enum is `#[non_exhaustive]` so this WILL happen on the next mmdvm-core release.

- `lodestar-core/src/reflector.rs:35` — `impl From<CoreProtocolKind> for ReflectorProtocol` uses `_ => Self::DPlus`. Any upstream-added core protocol silently becomes a DPlus reflector; the user then connects with the wrong protocol handshake and sees an obscure timeout instead of a legible error.

- `lodestar-core/src/session.rs:1330-1338` — `translate_event`'s `Event::VoiceEnd` fallback returns `text: None, position: None`. The comment claims "`handle_event` always produces the authoritative VoiceEnd", but `translate_event` is also called by `handle_event`'s `_ => translate_event(event)` arm (line 1094). Currently that arm never sees `VoiceEnd` (matched above), so it's dead — but if a future `Event::VoiceEnd` variant change makes it fall through, slow-data text/GPS would silently vanish from the recently-heard entry. The comment is a load-bearing invariant that isn't enforced.

- `lodestar-core/src/session.rs:749-755` — `TxTextState::ingest` increments `frame_index` with `.wrapping_add(1).max(1)`; the intent is "never let the natural u8 wrap present a 0 to the collector", but the sync path already forces 0 on `[0x55,0x55,0x55]`, so `.max(1)` is silently hiding the fact that this counter is not the D-STAR seq — it's a local counter unrelated to the outbound seq passed to `session.send_voice`. Passing an unrelated index to `SlowDataTextCollector::push` can misalign half-blocks whenever the outbound stream skips a superframe boundary without a sync frame. Compare with `StreamSlowDataState::push` which uses the real reflector `seq` — TX side does not, so the assembled TX text may misparse on long TX bursts.

### Error mapping / boundary quality (minor)

- `lodestar-core/src/session.rs:376-380` — `BadHeaderLength { expected: u32, got: u32 }` and `BadVoiceLength` cross the FFI as `u32`, but the caller always passes a `Vec<u8>` whose length is `usize`; the conversion uses `unwrap_or(u32::MAX)`. Not a crash, but a >4 GiB header would silently report `4_294_967_295` bytes to Swift — cosmetic.

- `lodestar-core/src/session.rs:606` — `let _ = backend.shutdown.send(());` — if the task already exited but its `JoinHandle` has not been reaped, we then `await backend.join`. There's no timeout, so a wedged `disconnect()` future in the task (e.g. a `dstar-gateway` shell that never resolves) hangs the Swift caller forever. `disconnect()` in `dstar-gateway` presumably has an internal timeout, but this crate does not defend against a violation of that assumption.

### Panics across FFI

- All `unwrap()` sites in `lodestar-core` are `unwrap_or`/`unwrap_or_default`/`unwrap_or_else` (Grep at `unwrap`); no plain `.unwrap()` or `panic!`. Clippy `unwrap_used`/`panic`/`indexing_slicing` are all `deny` in `Cargo.toml:92-95`. Good.
- `lodestar-core/src/audio.rs:210` — `self.inner.lock().unwrap_or_else(PoisonError::into_inner)`: this deliberately silences poisoning to keep the Swift app alive after a hypothetical Rust panic while holding the audio lock. Correct choice for FFI, but if the pipeline invariant was mid-mutation the next `push_voice` may emit clicks. Acceptable.

### Concurrency / lifecycle

- Reflector session teardown chain (`session.rs:599-612`, `run_session_task` at :983-1039) is sound: dropping `Arc<ReflectorSession>` drops `Mutex<Option<Backend>>`, which drops the `oneshot::Sender<()>` (closes the channel), which wakes `_ = &mut shutdown` in the `select!` and triggers `session.disconnect().await`. Verified.
- `lodestar-core/src/session.rs:672-687` — `connect_reflector` constructs channels (line 652-653) BEFORE the handshake. If any of the arm's `connect_*` call errors out before we `tokio::spawn`, the channels are just dropped — no leak.

### Docs / `#[expect]` review

- Every `#[expect(...)]` in `lodestar-core` (13 sites listed by Grep) has a `reason = "..."` justification. The `needless_pass_by_value` group is legitimate — UniFFI's `sequence<u8>`/`String` mapping forces owned args. The `cast_possible_truncation`/`cast_precision_loss` on `audio.rs:214, 229` correctly document the bounded-range analysis. No suspicious suppressions.
- `lodestar-core/src/mcp.rs:238-239` — the doc claim "`McpError::OffsetOutOfRange` is not reachable for `u8 offset`" is currently true (`u8` fits any 256-byte page). But the error variant remains reachable via its `pub` visibility from Swift; a future signature widening (as the doc anticipates) would silently succeed if reviewers assume the doc still applies.

### FFI surface

- `lodestar-core/src/lodestar.udl:1-36` — the UDL file only declares 4 functions (`version`, `encode_cat`, `parse_cat_line`, `default_reflectors`) plus enums/dictionaries. Everything else (audio, mcp, mmdvm, session) comes in via `#[uniffi::export]` proc-macros. The UDL never redeclares `ReflectorProtocol`, which is instead defined via `uniffi::Enum` derive at `reflector.rs:19` — but the UDL still declares one at line 23-27. Both compile because UniFFI's UDL-vs-procmacro merge does not detect this collision at generation time. If they ever diverge (a new protocol variant added only to the Rust enum), the UDL copy silently wins for a `Reflector` dictionary parse. Latent trap.

## Swift bridge / lifecycle claims

- `lodestar/Shared/UI/LodestarShell.swift:57-68` — the "linked sessions survive backgrounding" contract is implemented correctly: on iOS scenePhase `.background`, teardown runs ONLY when `reflector.state != .connected`. A live link keeps the `AVAudioEngine` pipeline running (`ReflectorAudioPlayer.beginKeepAlive` — see `ReflectorCoordinator.swift:248, 408`), which holds the process open with the `UIBackgroundModes: audio` entitlement. Matches the commit claim.
- `lodestar/Shared/Relay/SessionCoordinator.swift:73-74` — the `shutdown()` docstring says "Called on `scenePhase` transitions to `.background` / `.inactive`", but `LodestarShell.swift:81-85` explicitly does NOT shut down on `.inactive` (documented rationale: Notification Center pulls, incoming calls). Doc drift.
- `lodestar/Shared/Transport/USBSerialTransport.swift:74-98` — correctly reports unavailability via `link.servicePresent()` and throws `RadioTransportError.openFailed(reason:)`; there is no `Ok(())` with no side effects. The dext is genuinely optional.
- `lodestar/Shared/Relay/RelayCoordinator.swift:309-314` — `freshStreamId()`'s `repeat...while sid == 0` loop can never fire because `UInt16.random(in: 1...UInt16.max)` cannot produce 0. Harmless but the loop reads as if there's a real hazard.

## mcp-d75-extract

### Real bugs

- `mcp-d75-extract/src/rustgen.rs:73` — `((1_i64 << bits) - 1)` in the unsigned path. `bits = width * 8` where `width` is validated `1..=8`; `bits == 64` is filtered by the earlier `if bits == 64` guard at line 70. For all remaining widths (1..=7 → bits 8..=56), the shift is safe. Correct.
- `mcp-d75-extract/src/rustgen.rs:155` — bit-field `mask = ((1_i64 << width) - 1) << bit`. Bounded by the `bit + width > 8` guard above. Safe.
- `mcp-d75-extract/src/sources.rs:38-52` — `collect_cs_files` recurses on `path.is_dir()`, which follows symlinks. A symlink cycle inside the ILSpy output directory would recurse until stack exhaustion (aborting the process). Since input is developer-controlled, low severity, but the tool has no depth cap.
- `mcp-d75-extract/src/records.rs:60` — `let bases = (0..count_i64).map(|index| base + stride * index).collect();` uses non-checked arithmetic on `i64`. With adversarial input `base = i64::MAX, stride = i64::MAX` the multiplication overflows and panics in debug / wraps in release. In practice `count`, `base`, and `stride` come from parsed C# integer literals of the reviewed serializers, so bounded — but a hostile `--source-dir` could crash the tool.
- `mcp-d75-extract/src/language.rs:65-69` — `decode_utf16` defaults BOMless payloads to little-endian silently. Kenwood ships BOM-marked files, but a stripped file gets misdecoded rather than rejected, contradicting the "BOM-marked UTF-16" error message.

### Path-traversal / output handling

- `mcp-d75-extract/src/cli.rs:68-75` — `write_or_check` writes to `path` verbatim (with `create_dir_all(parent)`). No traversal check; if the caller passes `--output ../../../../etc/foo` it would write there. This is a dev CLI, not a network service, so acceptable — but worth calling out because the CLI also runs from `./ci-local.sh`.

### `#[expect]` review

- Every `#[expect(...)]` in `mcp-d75-extract` (3 sites) carries `reason = "..."`. The `too_many_arguments` reasons at `schema.rs:29` and `records.rs:185` are legitimate signature-preservation of a helper split for length. The `struct_field_names` in `csharp.rs:11` mirrors the reference extractor's constant names. No suspicious suppressions.

### Docs / comments

- `mcp-d75-extract/src/rustgen.rs:157-160` — the `bit_field_codec_lines` docstring says "`BitBool` for boolean values, `BitField` else"; the code additionally rejects `width != 1` for booleans with an explicit error. Correct and matches doc.
- `mcp-d75-extract/src/records.rs:16` — the `record_offset_layout` docstring says "checked linear or one-override base formula"; the implementation handles exactly `linear_base_re` and `piecewise_base_re` and errors on anything else. Correct.

### No TODO/FIXME/HACK found in either crate.

## Summary of most-important findings

1. `lodestar-core/src/mmdvm.rs:81` — lossy fallthrough on `mmdvm_core::MmdvmError` — Swift sees a fake "BadStart 0x00" for any new upstream variant.
2. `lodestar-core/src/reflector.rs:35` — `_ => DPlus` silently miscategorizes any new core protocol variant, causing wrong-handshake connect failures.
3. `lodestar-core/src/session.rs:749-755` — TX-side `TxTextState` uses a synthetic frame_index rather than the real reflector seq passed to the collector, which can misparse the assembled TX text on long transmissions.
4. `lodestar-core/src/lodestar.udl` vs `reflector.rs` `#[derive(uniffi::Enum)]` at `:19` — duplicate `ReflectorProtocol` definitions can silently diverge.
5. `lodestar-core/src/session.rs:599-612` — `disconnect()` awaits `backend.join` with no external timeout; depends on internal timeouts in `dstar-gateway`.
6. `lodestar/Shared/Relay/SessionCoordinator.swift:73-74` — stale docstring about scenePhase (behavior was changed in `LodestarShell.swift` to skip `.inactive`).
7. `mcp-d75-extract/src/sources.rs:38-52` — recursive walk follows symlinks with no cycle protection.
8. `mcp-d75-extract/src/language.rs:65-69` — BOMless UTF-16 silently defaults to LE despite the error message claiming a BOM is required.
