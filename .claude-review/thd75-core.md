# `thd75/` crate deep review

Scope: `thd75/src/{protocol,radio,memory,sdcard,types}/` (excluding `transport/` and `aprs/`). Notes: the "137 unwrap/expect" figure the caller flagged for `radio/programming.rs` is a false positive — `grep` counts the test-harness `mock.expect(cmd, response)` calls (a mock-transport method for scripting expected traffic), not `Option::unwrap`/`Result::expect`. Actual `.unwrap()` and `.expect("...")` counts in the whole crate's non-transport code: **0**. Panics come from other primitives (raw indexing, `assert!`, integer overflow in debug), several of which are real hazards; those are called out below.

---

## `protocol/`

### Bugs / correctness

- **`protocol/core.rs:331`** — wire-round-trip for shift is lossy in the extended range. `parse_channel_fields` accepts `shift_val` up to 15 (`ShiftDirection` accepts 0–15) and stores it in `ChannelMemory.shift`, but the reconstructed `flags_0a_raw` is `… | (shift_val & 0x07)`. On serialize (`serialize_channel_fields`, line 77 & 95) wire field [12] is `ch.flags_0a_raw & 0x07` — *not* `ch.shift`. A channel read in VFO mode with the documented extended value 8 therefore serializes back with wire shift = 0. `ChannelMemory.shift`'s docstring warns "extended values like 8 that don't fit in the 3-bit copy," but the loss on wire round-trip is not called out and the two encoders disagree.

- **`protocol/mod.rs:2079`** — the guard `if frame_str.len() < 2` combined with `frame_str.get(..2)` prevents the mnemonic slice from panicking, but the immediately-following `frame_str[2..]` (line 2079) is only safe because `.get(..2) == Some(_)` already proved byte 2 is a char boundary. This is correct today, but relies on invariant chains; a reordering of these two guards would introduce a panic on multi-byte-first-char input. Consider `frame_str.get(2..)` for defense in depth.

- **`protocol/control.rs:112`** — `parse_bl` docstring says "Taking the last comma-separated field handles both formats," but the code calls `split_once(',')` which returns the *second* field, not the last. Harmless for the observed `BL 0,3` form but misleading (a hypothetical `BL 0,1,3` would parse as `1,3` and fail rather than yield `3`).

- **`protocol/codec.rs:59`** — the 64 KB overflow drain uses `drain(..self.buffer.len() - MAX_BUFFER)` which discards the *oldest* bytes. Since the codec is CAT-only (`\r`-delimited ASCII), the tail-preserving behaviour is right: keeps the most recent (likely-in-progress) frame. Documented behaviour matches implementation.

### Docs

- **`protocol/programming.rs:19`** — module comment says "The `0M` handler is at firmware address `0xC002F01C`." This is dev-note trivia in a public docstring; harmless, but doesn't belong in user docs.

---

## `radio/`

### Bugs / correctness

- **`radio/programming.rs:1223`** — `let page = start_page + i` in `write_pages_raw` and `let page = start_page + i` on line 1176 in `read_pages_raw` can overflow `u16` in debug (panic) or wrap in release if `start_page + page_count > u16::MAX`. The internal callers stay within `TOTAL_PAGES = 1955`, but `write_memory_pages`/`read_memory_pages` are *public* and accept a caller-supplied `start_page: u16` with no upper bound. A caller passing `start_page = 65500, data.len() = 100 * PAGE_SIZE` will panic in debug builds. Add a `start_page.checked_add(page_count_u16).ok_or(...)` up front.

- **`radio/programming.rs:274-279`** — `#[expect(clippy::cast_possible_truncation, reason = "…page_count <= TOTAL_PAGES = 1955…")]` in `write_memory_pages`. The reason is **false** — nothing in that function bounds `page_count` to 1955; it comes from `data.len() / PAGE_SIZE` for a caller-supplied slice. If `data.len()` corresponds to > 65535 pages, `i as u16` truncates and the loop that checks for factory-calibration pages skips some. Defence-in-depth (the low-level `write_single_page_unverified` re-checks) keeps this from being catastrophic, but the suppression comment materially misdescribes the invariant.

- **`radio/programming.rs:26-30`** — module docstring: "The USB connection does not survive the programming mode transition. […] After calling any method in this module, the `Radio` instance should be dropped and a fresh connection established." This is now **stale** — `exit_programming_mode` calls `self.reconnect()` (line 1075), and every high-level entry point (`read_memory_image`, `write_memory_image`, `modify_memory_page`, `read_channel_names`, `read_channel_flags`, `read_all_channels`, …) uses that exit path. Only `modify_memory_page_detached` intentionally leaves the link dead. This misleading top-level warning will drive users to drop+reconnect their `Radio` unnecessarily.

- **`radio/programming.rs:635-636` and `radio/tuning.rs:133-137`** — same-shape stale docs. `read_all_channel_names`'s `# Connection lifetime` section and `find_channel_by_name`'s "the USB connection will have been reset" note both promise a state the reconnect-in-`exit_programming_mode` implementation no longer produces.

- **`radio/mod.rs:594-600`** — `get_cached_mode` returns `None` for any band that isn't `A` or `B`. `check_mode_compatibility` (line 606-624) therefore silently allows write commands targeting sub-bands (e.g. `Band::C`) to proceed with no warning — a real risk since the D75 does have sub-band identifiers. Might be intentional (sub-bands don't have independent VFO/mem state), but the silent-fallthrough deserves a comment.

- **`radio/programming.rs:1441-1446`** — `write_single_page_unverified` returns `WriteNotAcknowledged` if the byte read isn't `ACK` (0x06). But **the ACK read succeeded, meaning 1 byte was consumed**. If the radio sent, say, a stray `?` or a byte from a queued push, the next command will not see it (drained), but a real radio-side write failure indicated by some other byte gets propagated correctly. No bug, just noting the state machine is exact here.

- **`radio/programming.rs:1094-1099`** — `exit_programming_mode_detached` sets `self.desynced = true` *before* writing the exit byte, so a failure to write still leaves `desynced` set correctly; but it also sets `mcp_active = false` before the write. If the exit write fails, the caller sees an error but subsequent CAT will *not* return `McpInterrupted` — instead, `drain_stale_input` will run first because of `desynced` and the code will try normal CAT against a possibly-still-in-MCP radio. `recover_from_interrupted_mcp` (line 1141) is guarded on `mcp_active`, so it won't help. Consider only clearing `mcp_active` after the exit byte is confirmed written.

### Suspicious suppressions

- **`radio/dstar.rs:27, 33, 39`** — `const SLOT_URCALL: DstarSlot = match DstarSlot::new(1) { Ok(s) => s, Err(_) => unreachable!() };` — the inputs (1, 2, 3) are inside `DstarSlot`'s domain (1–6), and evaluation happens in `const` context so a mismatched constant would fail compilation before shipping. Safe.

- **`radio/mod.rs:175-180`** — `#[expect(clippy::unused_async)] on connect` — the reason ("callers can `.await` it uniformly with sibling constructors") is legitimate.

### Docs

- **`radio/mod.rs:307`** — `identify` docstring says "Verify the radio identity" — it does *not* actually verify; it just returns whatever model string the radio sends. Renaming or documenting explicitly would prevent surprises.

- **`radio/programming.rs:1054-1078` / `1063-1078`** — `exit_programming_mode` docstring says "waits out the radio's reset and brings the link back so the caller gets a radio that answers CAT commands"; this **is** what the code does. But the module doc contradicts it (see above).

### Concurrency

- The `Radio` state machine holds `desynced`, `mcp_active`, `mcp_saved_timeout`, `last_cmd_time` as plain fields. No shared-state locking or `Sync` gymnastics required — `Radio` methods take `&mut self` so a single owner is enforced. `link_state_tx`/`notifications` are already `Send + Sync` via tokio's broadcast/watch. No lock-order concerns because there are no locks.

- **`radio/programming.rs:1246-1256`** — the retry-once policy in `read_single_page` is correct for the specific failure modes (`Timeout`, `McpPageMismatch`) since it drains before retry. Not idempotent-retrying `Transport` errors is the right call.

---

## `memory/`

### Bugs / correctness

- **`memory/mod.rs:273-276`** — `modify_setting` **panics** if the closure changes more than one byte. This is documented behaviour, and there's an intentional `#[should_panic]` test, but a real-world caller who invokes `modify_setting(|w| { w.set_key_beep(true); w.set_beep_volume(5); })` gets a crash on production hardware, not an error. Prefer returning `Result<Option<(u16,u8)>, MultiByteChange>` or `Vec<(u16,u8)>`.

- **`memory/mod.rs:392, 402`** — `read_region`/`write_region` compute `offset + len` and `offset + data.len()` without `checked_add`. `usize` overflow is possible on 32-bit targets with hostile inputs (or arithmetic-panic in debug). Not exploitable in practice but easy to defend.

- **`memory/schema.rs:815`** — `.checked_shl(u32::from(shift)).unwrap_or(0)` silently produces 0 on shift-overflow. It's guarded by `validate_bit_codec` (line 807) which rejects `shift >= 8`, so it's unreachable in practice. The `unwrap_or(0)` is defense-in-depth rather than a hidden bug.

- **`memory/schema.rs:594-619`** — `apply_to_image` correctly validates every byte range up-front before mutating anything (all-or-nothing). Good pattern, cleanly implemented.

### Docs

- **`memory/schema.rs:594-608`** — `apply_to_image` comment says "The whole set is validated against the image bounds before any byte is modified" — matches implementation.

- **`memory/settings.rs:339`** — `timeout_timer` returns `0 if unreadable` but the valid range is 0–10 (0 is a legal timer index). No sentinel exists for "unreadable" vs "byte value 0". Caller cannot distinguish. Same pattern applies to `scan_restart_time`, `scan_restart_carrier`, `emr_volume_level`, etc. Not a bug, but a class-wide API-shape concern worth flagging.

---

## `sdcard/`

### Bugs / correctness

- **`sdcard/audio.rs:174-177`** — `find_chunk`: `let chunk_size = read_u32_le(…) as usize; let padded = (chunk_size + 1) & !1; offset += 8 + padded;`. On 32-bit targets, `chunk_size + 1` can overflow (`u32::MAX` chunk size in a crafted WAV → wrap to 0 → tiny padded step → infinite loop). On 64-bit it's safe (u32-max fits in usize). Consider `checked_add` and `checked_add(8)` since these files come from an SD card that a user could put arbitrary bytes on.

- **`sdcard/gps_log.rs:221-222`** — `verify_checksum` uses `&sentence[1..star_pos]` and `&sentence[star_pos + 1..star_pos + 3]`. `star_pos` comes from `find('*')` (byte index at an ASCII byte, guaranteed char boundary). `star_pos + 1` is one byte past `*`; safe. `star_pos + 3` is only guaranteed to be within-length by `star_pos + 3 > sentence.len()` check, **not** to be on a char boundary. A crafted `.nme` file with a multi-byte UTF-8 character in the checksum position panics here. Similar concern for `parse_coordinate` (`value[..deg_end]`, line 248): `deg_end = dot_pos - 2` computed from a byte-index `find('.')`; not guaranteed to be a char boundary if the coordinate contains any pre-'.' non-ASCII. Real GPS logs are ASCII so this is corner-case, but the parser panics on adversarial input.

- **`sdcard/gps_log.rs:154-163`** — `std::str::from_utf8(data).unwrap_or("")` returns `""` on any UTF-8 failure; then the fallback allocates a lossy Latin-1 copy. Fine, but the phrasing "If UTF-8 failed, try as Latin-1" is imprecise: it also triggers if the file is legitimately empty-after-decode (a UTF-8 log with only whitespace, since `text.is_empty()` is `false` in that case — actually only triggers if from_utf8 fails). Documentation could be clearer.

- **`sdcard/mod.rs:239-253`** — `read_u16_le` / `read_u32_le` compute `offset + 2` / `offset + 4` without `checked_add`. `.get(offset..offset+N)` returns `None` on out-of-range as intended, but the arithmetic itself can panic in debug on huge offsets. Real files can't reach this, but the "callers are expected to have validated" docstring undersells the silent `0` return on failure. That silent `0` masks real errors — for example, WAV parsing that trusts `data_length` from a truncated chunk-header read gets `0` and computes a `duration_secs` of `0/x = 0`, so a truncated file parses "successfully" with zero-duration metadata.

- **`sdcard/config.rs:220`** — `let rx_freq = ch_bytes.split_first_chunk::<4>().map_or(0, ...)` — treats a channel with wall-clock RX freq 0 as unused. Consistent with `.d75` semantics but note that `0x00000000` and `0xFFFFFFFF` both mean "unused" per the field docstring; the code checks both. Correct.

### Docs

- The claim that `.d75` files start with a 256-byte header is consistent everywhere; `HEADER_SIZE = 0x100 = 256`. No drift.

---

## `types/`

### Bugs / correctness

- **`types/channel.rs:334-336`** — `ChannelMemory::to_bytes` hardcodes byte 0x0E bits 3:2 to `3` (`| 0x0C`), and `from_bytes` (line 412-426) reads only bits 5:4 and 1:0, silently dropping bits 7:6 and 3:2. A round-trip through `ChannelMemory::to_bytes` therefore rewrites byte 0x0E in a lossy way. The `ChannelMemory` docstring claims "Maps byte-for-byte to the firmware's internal representation at `DAT_c0012634`" — but this only holds for a channel that already has bits 7:6 = 0 and bits 3:2 = `11`. `ChannelMemory::to_bytes` is currently only called from tests (production flash writes go through `FlashChannel::to_bytes` which correctly preserves `byte0e_reserved`). Delete `ChannelMemory::to_bytes`/`from_bytes` or explicitly document that they are for CAT-wire round-trip only, not flash.

- **`types/frequency.rs:127-131`** — `Display` truncates below the kHz digit: `(self.0 % 1_000_000) / 1_000`. For `Frequency::new(145_190_100)` it prints `"145.190 MHz"`, silently dropping the 100 Hz. That's fine for a display, but combining it with the doc "in MHz with three decimal places" gives an impression of full precision. Note the drop.

- **`types/channel.rs:122`** — `let end = bytes.iter().position(|&b| b == 0).unwrap_or(8).min(8);` in `ChannelName::from_bytes` — the `.unwrap_or(8)` returns 8 when no null is found; the trailing `.min(8)` caps searches that find a late null past byte 8 down to 8. Correct behaviour, but the docstring only mentions "up to 8 characters"; the actual behaviour (a null at byte 15 counts as byte 8) is not obvious.

---

## Cross-cutting suggestions

1. **Kill the "137 unwrap/expect" red herring.** Replace `.expect(cmd, resp)` on `MockTransport` with something like `.script(cmd, resp)` so grep-based safety audits don't false-positive.

2. **Guard the two public MCP page ranges (`write_memory_pages`, `read_memory_pages`)** against `start_page + count > u16::MAX` — one `checked_add` up front makes the whole call chain overflow-free.

3. **Rewrite `modify_setting`** to return `Result<Vec<(u16,u8)>, _>` (or a stream) instead of asserting on multi-byte changes; the panic is a foot-gun for real callers.

4. **Purge stale connection-lifetime docs** across `radio/programming.rs` module docs, `read_all_channel_names`, and `find_channel_by_name` — the reconnect-in-exit path invalidates the "you must drop and reconnect" warnings; users otherwise write defensive teardown code that isn't needed.

5. **Consider deleting `ChannelMemory::to_bytes`/`from_bytes` from `types/channel.rs`** — they are test-only, encode a lossy round-trip, and contradict the "byte-for-byte firmware" docstring.

6. **Harden `sdcard/audio.rs::find_chunk`** with `checked_add` and cap by `data.len()` to eliminate the 32-bit-target infinite-loop-on-hostile-WAV path.
