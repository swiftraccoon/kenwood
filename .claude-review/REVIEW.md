# kenwood workspace — deep code review

Eight parallel review agents covered every crate in the workspace (~191 k Rust LOC across 20 crates plus the Swift Lodestar app). Full per-subsystem findings live alongside this file (`thd75-core.md`, `transport-aprs.md`, `dstar-gateway.md`, `mmdvm-kiss-ax25.md`, `mbelib-rs.md`, `dsp-audio.md`, `tui-repl.md`, `lodestar-mcp.md`). This document ranks the findings that stood out — security first, then correctness, then documentation.

Overall shape: this is a defensively-written codebase. `unsafe_code = "forbid"`, clippy `pedantic`+`nursery`+`cargo` denied workspace-wide, and every `#[expect(...)]` I audited carries a substantive `reason = "..."` — the reasons check out with only two exceptions (below). The subsystems that see hostile bytes (APRS parser, KISS, AX.25, DPlus/DExtra/DCS codecs, mbelib decoder) show a very disciplined `.get()/unwrap_or` pattern rather than raw indexing. The panic-on-hostile-input surface is genuinely small. The residual issues cluster in three areas: reflector-server auth/DoS gaps that predate this workspace's spec, one FCC-relevant TUI safety gap, and a scatter of stale docstrings and lossy enum fallthroughs that will bite when upstream crates add variants.

The claim "137 unwrap/expect in `radio/programming.rs`" that framed the task is a false positive — those are calls to `MockTransport::expect(bytes, response)` (a test-harness helper), not `.unwrap()`/`.expect(...)` panics. The whole `thd75/` crate outside `transport/` has **zero** production panics of that form.

---

## P0 — Ship-blockers if this were a security review

### 1. Cross-protocol header forwarding is an on-air spoofing amplifier

`dstar-gateway-server/src/tokio_shell/transcode.rs:150-189` + `endpoint.rs:915-957`

When `cross_protocol_forwarding = true` (opt-in but supported), a DExtra client's `DStarHeader` — including `rpt1`, `rpt2`, `my_call`, `my_suffix`, and all three flag bytes — is copied verbatim into the DCS/DPlus re-encoder. Any DExtra peer can inject arbitrary reflector/operator callsigns that surface on DCS/DPlus subscribers as if they came from a different station. Force `flag1/2/3 = 0` and rewrite the reflector-callsign portion of `rpt2` (and `rpt1[7]`/`rpt2[7]` module bytes) to the local reflector's identity before re-encoding.

### 2. Hosts-fetcher and auth client have no upstream size caps and no TLS

- `dstar-gateway/src/hosts_fetcher/fetcher.rs:8, 52-60` — XLX directory URL is hard-coded `http://xlxapi.rlx.lu/...`. On-path attacker rewrites the reflector list; every subsequent client dial goes to attacker-chosen IPs. reqwest's `rustls-tls` is already a dep — switch to HTTPS.
- `dstar-gateway/src/hosts_fetcher/fetcher.rs:58` — `.text().await` has no `Content-Length` cap. A hostile upstream can stream gigabytes and OOM.
- `dstar-gateway/src/auth/client.rs:358-381` — `read_response` accumulates into an unbounded `Vec<u8>`, throttled only by a per-read timeout. A slowloris-style upstream OOMs the process. Cap at ~1 MiB (real chunks are ≤4 KiB).

### 3. Reflector session state machine accepts LINK on already-linked sessions

`dstar-gateway-core/src/session/server/core.rs:290-319` (`on_dextra_link`) and `604-639` (`on_dcs_link`)

When the session is already `Linked` (or, for DCS, even `Streaming`), a fresh LINK overwrites `self.client_callsign` with no callsign-equality check. Combined with the shell's `link_capacity_reject` fast-path (`endpoint.rs:843` returns `None` when the peer address is already in the pool), any attacker who can send a UDP packet with the source address of an existing peer — LAN spoof, on-path, NAT-shared client — silently rewrites the linked callsign. No log line, no event. Fix: reject a LINK whose callsign differs from the currently-stored one; require an explicit UNLINK first.

### 4. DPlus LINK1 pool exhaustion

`dstar-gateway-server/src/tokio_shell/endpoint.rs:564-591`

LINK1 carries no callsign, so the authorizer cannot gate it; every LINK1 gets a pool slot held for 30 s. An attacker sending LINK1 from spoofed source-port tuples fills the pool in <250 packets and blocks legit peers for 30 s. Rate-limit LINK1 per source IP, or shorten the pre-authenticated-slot timeout.

### 5. APRS digipeater accepts source-spoofed loops

`aprs/src/digipeater.rs:250-269, 395-404`

Loop detection only trips on entries whose `has_repeated == true`. A packet whose *source* callsign is our own is happily accepted and relayed. This is a well-known real APRS attack; a defense-in-depth check on `packet.source.callsign == self.callsign` would close it. Related: SSID compare is strict `==` when comparing repeated hops, so a packet digipeated with our callsign at the wrong SSID slips through.

### 6. `polaris` binary ships with `AllowAllAuthorizer`

`dstar-gateway-server/src/bin/polaris.rs:83`

Documented as a "test reflector," but nothing in the binary name says so. Anyone deploying `polaris` from a release build accepts every callsign with `ReadWrite`. Either wire an authorizer through config or rename the binary (e.g. `polaris-open`) so its permissive default is unmistakable.

---

## P1 — Correctness bugs that will bite real users

### 7. TUI can transmit with no operator confirmation

`thd75-tui/src/app.rs:1994-2010, 1411-1414, 2534-2545`

- Pressing `b` on the APRS panel sends `BeaconPosition { lat: 0.0, lon: 0.0 }` — no dialog, no `--yes` gate. The REPL has an entire `confirm::tx_confirm` gate for exactly this; the TUI ignores it and can transmit a null-island beacon by keypress mistake.
- Composing a message via `M` and pressing Enter transmits immediately with no confirmation.
- `+`/`-` on the beacon-type setting can flip to `Auto`/`SmartBeaconing`/`PTT` — modes that make the radio's firmware transmit unattended — with no gate. The REPL guards these; the TUI doesn't.

Also affects the REPL: `thd75-repl/src/commands.rs:822, 840, 786` — `reflector`, `unreflector`, and `urcall` skip the `tx_confirm` gate that the sibling `cq` uses.

### 8. Lodestar-core lossy enum fallthroughs (Swift-facing)

- `lodestar-core/src/mmdvm.rs:81` — `From<mmdvm_core::MmdvmError>` catch-all maps every unknown variant to `Self::BadStart { actual: 0 }`. Swift shows the wrong error text for every future upstream error variant, and the enum is `#[non_exhaustive]` so this WILL happen.
- `lodestar-core/src/reflector.rs:35` — `impl From<CoreProtocolKind>` uses `_ => Self::DPlus`. Any new protocol variant silently connects with the wrong handshake and shows the user a mystery timeout.
- Both should carry an `Other(String)` variant rather than fall through.

### 9. TxTextState uses a synthetic frame index, not the reflector seq

`lodestar-core/src/session.rs:749-755`

`TxTextState::ingest` increments a local `frame_index` with `.wrapping_add(1).max(1)` and passes it to `SlowDataTextCollector::push`, while the RX-side `StreamSlowDataState::push` uses the real reflector `seq`. If an outbound stream ever skips a superframe boundary without a sync frame, the assembled TX text misparses. RX and TX should use the same seq source.

### 10. MMDVM SetMode ACK correlation is racy

`mmdvm/src/tokio_shell/modem_loop.rs:425-458`

If `set_mode()` on the handle side times out (2 s), `pending_set_mode` stays `Some`. A subsequent `set_mode()` replaces it, and a late ACK for the FIRST SetMode satisfies the SECOND caller with a stale-cached success. Fix: clear `pending_set_mode` on the handle-side timeout, or attach a request-ID. Same class of concern: `handle_version` accepts any byte as protocol version and steers status parsing through v2 for values ≥2 (`modem_loop.rs:487-498`).

### 11. KISS decoder has adversarial-input DoS handles

- `kiss-tnc/src/decoder.rs:71` — `push()` is unbounded; the max-frame-len cap is only enforced from `next_frame()`. A producer that never polls grows the buffer without limit.
- `kiss-tnc/src/decoder.rs:88-142` — repeated single-byte `Vec::drain(..1)` inside the loop is O(n²) on an all-FEND burst. Bounded by the default 1024-byte cap, unbounded if a caller raises it.
- `kiss-tnc/src/frame.rs:165-184` — `Return` frame with a payload decodes, then encodes back to a shorter frame — `encode(decode(x)) != x`.

### 12. AX.25 `build_ax25` panics on >8 digipeaters

`ax25-codec/src/frame.rs:252-281`

`Ax25Packet.digipeaters: Vec<RouteEntry>` is `pub` with no invariant. Any safe caller pushing 9+ entries panics inside `build_ax25`. Prefer `Result` or a newtyped digipeater list.

### 13. Mic-E longitude parser accepts out-of-range bytes

`aprs/src/mic_e.rs:120-137`

Bytes are validated `>= 28` but not `<= 127`. A byte `0xFF` yields a longitude >180° instead of the intended `InvalidCoordinates` error. Reject `byte > 127` at the parser entry.

### 14. mbelib encoder debug-panic and adaptive-smoothing sign inversion

- `mbelib-rs/src/encode/quantize.rs:648` — `vuv.num_bands - 1` panics in debug when `num_bands == 0`. Use `saturating_sub(1)`.
- `mbelib-rs/src/adaptive.rs:161` — `tm` can go negative on sustained high error counts, then multiplies magnitudes by a negative scale, inverting sign of all bands. Not a panic; audibly wrong. Clamp `tm.max(MIN_TM)`.
- `mbelib-rs/src/ecc.rs:192-207` — Golay error counter ignores parity-bit corrections, so a codeword with 3 parity-only flips returns `errs = 0`. Faithful to mbelib but a security-relevant divergence from proper decoding. The top-level `errs2 > 3` gate never fires from this input class.

### 15. thd75 public MCP page-range integer overflow

`thd75/src/radio/programming.rs:1176, 1223, 274-279`

`write_memory_pages` / `read_memory_pages` accept caller-supplied `start_page: u16` and compute `start_page + i` without `checked_add`. On `start_page = 65500, page_count = 100`, debug builds panic and release wraps to a wrong page. Also, the `#[expect(cast_possible_truncation)]` at line 274 justifies the cast by claiming `page_count <= TOTAL_PAGES = 1955` — but nothing in that function enforces that bound. Add a `checked_add` guard at the entry.

### 16. `modify_setting` panics on multi-byte closures

`thd75/src/memory/mod.rs:273-276`

Documented behavior with a `#[should_panic]` test, but a real caller writing `|w| { w.set_key_beep(true); w.set_beep_volume(5); }` gets a hardware-time crash instead of an error. Prefer `Result<Vec<(u16,u8)>, MultiByteChange>`.

### 17. Under-piped-stdin, REPL blocking prompts consume the next scripted command

`thd75-repl/src/main.rs:2544-2604, 3013-3022, 2079-2082`

`ensure_host_files`, `exit_dstar` teardown, and `guide_exit_terminal_mode` all use blocking `stdin.read_line` without checking `SCRIPT_MODE`. Under piped input they silently swallow the next line as a "y/n" answer. Silent script corruption.

---

## P2 — Notable-but-lower-severity

- `aprs-is/src/client.rs:451` — case-sensitive `logresp`/`verified`/`unverified` matching; also `contains("verified")` substring-matches `notverified`. Server that emits uppercase or unusual phrasing is silently treated as not-yet-authenticated (or worse, hostile server pretends `unverified` is `verified`).
- `aprs/src/station_list.rs:205-215` — `haversine_km` NaN on floating-point antipodal points silently drops peers from `nearby()` radius filter. Fix with `a.min(1.0).sqrt().asin()`.
- `aprs/src/smart_beaconing.rs:340` — no ordering check on `slow_rate_secs` vs `fast_rate_secs`. A misconfigured swap triggers a beacon storm exactly at high speed. Validate in the config constructor.
- `thd75/src/transport/bluetooth.rs:150` + `bluetooth_mac.m:305-317` — narrow-window UAF: `bt_rfcomm_close` frees `RfcommContext` *after* releasing the mutex; a `spawn_blocking` write that acquires the mutex after free reads `ctx->state` on freed memory. Reference-count the context or drop `free()` inside the mutex.
- `stargazer/src/harvest.rs:918` — unattended cron harvester reads response body with `.bytes().await` and no cap. Add a ~5 MB cap on `.dvrec` bodies.
- `stargazer/src/writer.rs:280-286` — recordings path built from `rec.origin.reflector` (a `String` field with no path-safety filter). Trusted today because upstream constrains it; add the same `sanitize_callsign`-style filter as a defense.
- `sextant/src/audio.rs:1431` — non-16-bit PCM WAV plays or transmits as silence with no user-visible error.
- `sextant/src/audio.rs:1591, 1746, 1768, 1810` — speaker underruns/overruns silently drop samples with no counter.
- `mmdvm-core/src/mode.rs:76-92` — every unknown mode byte collapses to `Idle`. A modem in `MODE_ERROR (100)` and a modem in an unknown state are indistinguishable. Keep the raw byte alongside.
- `if-dsp/src/spectrum.rs:120` — `write_psd` is labelled "one-sided PSD" but has no window-power compensation and no 2× doubling for non-DC/Nyquist bins. Rename to `write_power_spectrum` or add the compensation.
- `mcp-d75-extract/src/sources.rs:38-52` — recursive walker follows symlinks with no cycle protection.
- `mcp-d75-extract/src/language.rs:65-69` — BOMless UTF-16 silently defaults to LE even though the error message claims a BOM is required.

---

## P3 — Docs and comments that drift from the code

- `thd75/src/radio/programming.rs:26-30, 635-636` and `thd75/src/radio/tuning.rs:133-137` — top-level and per-function docs still warn "drop and reconnect after programming mode." `exit_programming_mode` now reconnects for you (`programming.rs:1075`); users are writing unnecessary teardown code.
- `mbelib-rs/src/tables.rs:50-53` — `W0_TABLE` docstring claims filler values for indices 120..=123; the table content contradicts (monotonically decreasing across the whole range).
- `mbelib-rs/src/encode/dc_rmv.rs:30-31` — docstring says "Panics if …"; the function iterates with `zip`, cannot panic.
- `mbelib-rs/src/decode.rs:657-659` — `T5/T6/T7` tone tables have no citation to mbelib source line numbers; a single-bit transcription error silently misroutes tone frames.
- `thd75/src/radio/mod.rs:307` — `identify` is documented as "Verify the radio identity" but does no verification.
- `thd75/src/types/channel.rs:334-336` — `ChannelMemory::to_bytes` docstring says "byte-for-byte firmware representation," but the implementation hardcodes bits 3:2 of byte 0x0E to `11` and drops bits 7:6 on read. Test-only today; delete or narrow the docstring.
- `aprs-is/src/client.rs:216-238` — client-struct docstring claims "not Send-across-the-await"; the struct is in fact `Send`.
- `thd75/src/protocol/programming.rs:19` — public docstring exposes firmware address `0xC002F01C`. Move to internal notes.
- `if-dsp/src/channelizer.rs:74-76` — comment says `MODE_TAPS = 255` for all modes; `AmDemod` is built with `DECIM_TAPS = 63` at line 114. Either use `MODE_TAPS` for AM too or explain the exception.
- `lodestar/Shared/Relay/SessionCoordinator.swift:73-74` — `shutdown()` doc says "called on `.background`/`.inactive`"; `LodestarShell.swift:81-85` explicitly skips `.inactive`. Doc drift.
- `dstar-gateway-core/src/codec/dextra/decode.rs:85-102` — doc says "callers must verify slice ≥ 8 bytes"; implementation defends anyway. Docstring is stricter than the code guarantees.
- `thd75-repl/src/help_text.rs:448-453` — `CONFIRM_HELP` lists an outdated subset of the commands that actually prompt (`object`, `mice`, `compressed`, `status`, `motion` also prompt but are missing).
- `kiss-tnc/src/lib.rs:16` — doc claim "`DEFAULT_MAX_FRAME_LEN` comfortably exceeds a maximum-size AX.25 frame even after worst-case KISS byte stuffing" is wrong for AX.25 v2.2 IL.
- `lodestar-core/src/lodestar.udl:23-27` — declares `ReflectorProtocol` a second time on top of the `#[derive(uniffi::Enum)]` at `reflector.rs:19`. Silent drift risk if a variant is added in only one place.

---

## Feature / architectural suggestions

- **Per-source-IP quotas on the reflector's ingress socket.** All the DoS handles above resolve to "an attacker who can send UDP packets can fill queues/pools." A simple token bucket keyed on `SocketAddr::ip()` would defuse most of them at once.
- **First-class TX-safety abstraction in the TUI.** Extract the REPL's `tx_confirm` and reuse it from the TUI so every wire-emitting action goes through the same gate. Would fix P1 §7 as a single change.
- **Distinguish "unset" from "value 0" in `types::settings` accessors.** The `unwrap_or(0)` idiom on setting getters silently collapses "unreadable memory" and "value zero" into the same output. Return `Option<u8>` or a `SettingValue<u8>` sentinel so callers can react to missing fields.
- **Introduce an `Other(String)` variant on every FFI-facing error enum** (`lodestar-core::mmdvm::MmdvmError`, `reflector::ReflectorProtocol`) so upstream `#[non_exhaustive]` additions surface a diagnostic rather than a wrong-answer default.
- **Sanitize cross-protocol D-STAR headers** as a transcoder responsibility; today each protocol encoder trusts whatever headers land in it. A `transcode.rs::rewrite_headers(target, local_reflector, header)` helper called from every cross-protocol branch would centralize the fix.
- **Add ingress rate metrics per protocol per peer** to the reflector; without them, LINK1 floods and Poll floods are invisible to the operator until the pool is full.
- **Move `MockTransport::expect` to `MockTransport::script`** across the whole workspace so grep-based audits for `.expect(` don't false-positive against test fixtures. Would have saved this review's opening premise from being wrong.
- **A workspace-wide `tx-safety` unit-test harness** that walks every public API mutation in `thd75-tui`/`thd75-repl` and asserts the `Radio::keying_state()` transitions only happen from within a `tx_confirm`-marked callsite. Cheap regression net for future TUI features.

---

## What I did not find

- No `TODO`/`FIXME`/`HACK` markers anywhere except `mmdvm-core/src/config.rs:17` (missing SetConfig encoder) and one dev-note. Very unusual for a codebase this large.
- No `.unwrap()` / `.expect(...)` panics in production code across `thd75/`, `thd75-repl/`, `sextant/`, `stargazer/`, `if-dsp/`, `lodestar-core/`, or `mbelib-rs/`. The `unwrap_used` / `expect_used` / `panic` / `indexing_slicing` clippy lints are doing their job.
- No unjustified `#[allow]`/`#[expect]` attributes surfaced except two: `thd75/src/radio/programming.rs:274-279` (reason doesn't match code) and one weak reason in a `pending_set_mode` comment. Every other attribute had a substantive `reason = "..."` I could verify against the code.
- No unsafe blocks outside the documented allowlist (macOS `IOBluetooth`, UniFFI scaffolding in `lodestar-core`). `unsafe_code = "forbid"` is enforced.
- No obvious secret-leak, credential-log, or path-traversal issue in the reviewed production paths (`stargazer::harvest::is_safe_basename` is a recently-fixed model of how to do this properly).

Per-subsystem detail files sit next to this one in `.claude-review/`.
