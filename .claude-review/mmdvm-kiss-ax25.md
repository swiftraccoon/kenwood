# Codec Review — kenwood packet-radio crates

Scope: `mmdvm-core/`, `mmdvm/`, `kiss-tnc/`, `ax25-codec/`.

## kiss-tnc/

**`kiss-tnc/src/decoder.rs:71-73`** — `push()` is unbounded; the max-frame-len cap is only checked from `next_frame()`. A caller that pushes many chunks (or one large chunk) before ever polling — a plausible pattern behind an async reader — grows the internal `Vec` to whatever the peer sends before any cap kicks in.

**`kiss-tnc/src/decoder.rs:88-142`** — Repeated single-byte `Vec::drain(..1)` inside the loop is O(n) per call, so a hostile burst of all-FEND bytes (`FEND FEND FEND …`) is processed in O(n²). Bounded by `max_frame_len` at the default of 1024 (~1 M ops) but pathological if a caller raises the cap; also produces one loop iteration per byte for such input.

**`kiss-tnc/src/frame.rs:165-184`** — `decode_kiss_frame` accepts a `Return` frame with a payload (`[FEND, 0xFF, junk, FEND]`) and puts `junk` into `data`. The encoder then silently drops it on re-encode (line 76-83), so `encode(decode(x)) ≠ x` for this input. Prefer to reject non-empty Return payloads.

**`kiss-tnc/src/frame.rs:139-144`** — Trims leading in-frame FENDs but never trailing FENDs; combined with the outer `data.last() == FEND` strip this is fine only because the streaming decoder never hands it more than one trailing FEND. A caller invoking `decode_kiss_frame` directly on `[FEND, type, body, FEND, FEND]` gets `UnexpectedFrameDelimiter` from the inner FEND scan. Contract is subtle — worth documenting.

**`kiss-tnc/src/lib.rs:16`** — `DEFAULT_MAX_FRAME_LEN = 1024` is smaller than a max-size AX.25 v2.2 information frame (up to ~2100 bytes header+I) even before KISS stuffing. Fine for APRS/v2.0, but the doc claim "comfortably exceeds a maximum-size AX.25 frame even after worst-case KISS byte stuffing" is wrong for v2.2 IL.

## ax25-codec/

**`ax25-codec/src/frame.rs:112-136`** — `decode_address` is a `fn(bytes: [u8; 7])`, but line 130 and 211 read `bytes.get(6).ok_or(Ax25Error::PacketTooShort)?` on the same fixed-size array — unreachable and misleading (the error name suggests a different failure mode than what could actually trip it). Similarly `data.get(13)` at line 198 after the `< 16` gate at line 171. Not a bug, just dead defensive code.

**`ax25-codec/src/frame.rs:133`** — `Callsign::new(&callsign).map_err(|_| Ax25Error::InvalidCallsignByte(0))` and line 134 uses `ssid_raw`; both swallow the real inner error and, for callsign, report byte `0x00` which is misleading in a wire report.

**`ax25-codec/src/frame.rs:252-281`** — `build_ax25` panics on `> MAX_DIGIPEATERS`. `Ax25Packet` has `pub digipeaters: Vec<RouteEntry>` with no invariants — any caller that constructs one and pushes 9+ digipeaters (e.g. from application logic on top of a parsed frame) will panic in an encode. Prefer returning `Result` from `build_ax25` or newtyping the digipeater list.

**`ax25-codec/src/frame.rs:112-136`** — Decoder tolerates callsign bytes whose reserved bit 0 is set (the AX.25 spec requires bit 0 = 0 on the first six address bytes) — we just do `b >> 1` and drop the LSB, so `A|0x01 << 1` = `0x83` decodes cleanly as `'A'` and the H/C bit misalignment hint is lost. Accepted silently.

**`ax25-codec/src/frame.rs:169-240`** — `parse_ax25` does not consume or validate a trailing FCS. Doc says KISS strips it, but the crate is offered as a general AX.25 codec; a caller applying it to raw AX.25 (from a software modem) will have the two FCS bytes appended to `info` without warning. Worth a `parse_ax25_with_fcs` variant or a stronger doc note.

**`ax25-codec/src/control.rs:49-87`** — SABME (protocol-v2.2 modulo-128 setup, `0x6F`) is silently classified as `UnnumberedKind::Other(0x6F)` — the pinning test at line 246 asserts this is deliberate. Legit for scope, but any downstream that decides a frame is "not connection-setup" solely from `UnnumberedKind` will mis-handle SABME.

## mmdvm-core/

**`mmdvm-core/src/frame.rs:99-142`** — `decode_frame` treats `length == 0` as the extended-form marker unconditionally. If a firmware bug ever emits a bogus `[0xE0, 0x00, ...]` header when it meant something else, the decoder will happily accumulate 255-510 bytes waiting for a frame that won't arrive, until the shell's `RX_BUFFER_HARD_CAP` clears it. Reference-faithful — flagged only because it's a hostile-input amplifier (1 byte → 510-byte wait).

**`mmdvm-core/src/frame.rs:112-116`** — Extended-form `frame_len = usize::from(length2) + 255` also allows `length2 == 0` (frame_len 255), which collides with the single-byte form of length 255 — the same on-wire size is encodable two ways. Reference matches. Worth a note in the module docs that the two forms overlap at 255 rather than the current phrasing that implies they are cleanly separated at 256.

**`mmdvm-core/src/frame.rs:126-134`** — Two `data.get(...)` calls guarded by comments "Impossible: … but the lint-safe get() path is cheap". They return `Ok(None)` on the "impossible" branch, meaning a proven-complete frame silently disappears if the invariant is ever broken (say by a refactor). Prefer a debug-assert or `unreachable!`-with-context so a bug fails loudly.

**`mmdvm-core/src/config.rs:17-18`** — `TODO: full SetConfig encoding` — module exposes `ModemConfig` but has no encoder. Any consumer that expects to call `SetConfig` today is out of luck.

**`mmdvm-core/src/status.rs:198-234`** — v1 status: comment (line 36-40) says v1 layout starts with `proto(0), mode(1), state(2)…`. In MMDVMHost the v1 status payload's first byte is actually the protocol-version echo, and the Rust code reads it as such via the `+1` offsets. The comment is correct but easy to misread — worth spelling out "payload[0] is a protocol-version echo present only in v1".

**`mmdvm-core/src/mode.rs:76-92`** — `ModemMode::from_byte` collapses every unknown byte to `Idle`. `parse_v2` uses it for the mode field; a modem reporting a garbled mode byte will therefore be reported as Idle to consumers. Lenient by choice, but a v2 modem in state `MODE_ERROR (100)` and a v2 modem in an unknown state are now indistinguishable from Idle in the parsed status. `parse_v1`/`_v2` should keep the raw byte alongside so consumers can detect this.

## mmdvm/ (tokio shell)

**`mmdvm/src/tokio_shell/modem_loop.rs:425-458`** — ACK/NAK-to-`pending_set_mode` correlation is done purely on the reply's command byte. If `set_mode()` on the handle side times out (2 s), the handle's caller returns `ResponseTimeout` but the loop's `pending_set_mode` is still Some. A subsequent `set_mode()` replaces it (line 308), and a late ACK for the FIRST SetMode then satisfies the SECOND caller — reporting a stale-cached success. Consider clearing `pending_set_mode` on the handle-side timeout, or attaching a request-ID.

**`mmdvm/src/tokio_shell/modem_loop.rs:487-498`** — `handle_version` overwrites `self.protocol_version = v.protocol` with whatever byte the modem returns. `handle_status` then uses `if self.protocol_version >= 2 { parse_v2 }` (line 502). A modem reporting `protocol = 99` would silently route every status through the v2 parser. Better to accept only `1` or `2` and treat other values as protocol-violation.

**`mmdvm/src/tokio_shell/modem_loop.rs:655-678`** — `write_frame` wraps `write_all + flush` in a 5 s `tokio::time::timeout`. `AsyncWriteExt::write_all` is **not** cancellation-safe; on a timeout expiry mid-write the transport can be left with partial-frame bytes on the wire. In the current code the loop then returns `ShellError::Io`, drops the transport, and the modem drains — acceptable, but should be documented as "write timeout is fatal" (currently only implicit).

**`mmdvm/src/tokio_shell/modem_loop.rs:293-357`** — `apply_command` awaits `write_frame` inline. During a slow write (up to 5 s) the loop cannot service `command_rx`, `status_tick`, `playout_tick`, or `transport.read`. With `biased` select ordering this is fine for one command but chains: every command sends its own frame synchronously. A burst of commands that each take close to `WRITE_TIMEOUT` could starve reads for tens of seconds and burst status polls afterwards. Consider posting writes to a channel drained on the playout tick.

**`mmdvm/src/tokio_shell/modem_loop.rs:174-182`** — Initial handshake sends `GetVersion` then `GetStatus` before entering select. The very next status response can arrive before the version response (some firmwares reply out of order); with `protocol_version` initialized to `2` (line 119) a real-v1 modem will surface the first status as a `ProtocolViolation` (parse_v2 length mismatch) rather than as `Status`. Consider gating the status parse until a version response has landed.

**`mmdvm/src/tokio_shell/modem_loop.rs:361-398`** — `drain_rx` runs synchronously (not `.await`) inside the `transport.read` select branch. A single 512-byte chunk containing many short valid frames dispatches them all in one turn, potentially emitting hundreds of events (mostly dropped by the 256-slot channel) with no chance for the status/playout ticks to interleave. Bounded (RX_READ_CHUNK is 512) but worth capping frames-per-drain if you want tighter tick fairness.

**`mmdvm/src/tokio_shell/modem_loop.rs:405-418`** — `resync_rx_buffer` uses `iter().skip(1).position(...)` and then `drain(..=offset)`. `offset` is relative to `skip(1)`, so `drain(..=offset)` drops `offset + 1` bytes from the front — correctly preserving the next start byte. Traced OK; recommend a comment that shows the +1 arithmetic explicitly, since this is easy to break in a follow-up edit.

**`mmdvm/src/tokio_shell/modem_loop.rs:428-431 / 441-444`** — On ACK/NAK the frame payload's first byte is trusted as the "command that was ACK'd" and reported unqualified via `Event::Ack{command}` — but there's no verification the ACK was in response to something we sent. A misbehaving modem streaming spurious ACKs would drive consumer state (and, for `MMDVM_SET_MODE` matches, resolve arbitrary pending futures). Same class of concern as the SetMode correlation above.

**`mmdvm/src/tokio_shell/tx_queue.rs:138-145`** — Strict `>` gate on `dstar_space` matches the reference. But `slots_required` is a `u8` and `dstar_space > head.slots_required` compares unsigned — fine, no underflow. `drain_tx_queue` at modem_loop.rs:647 does `saturating_sub` on the same value — consistent.

## Attribution

All MMDVM files carry the `Portions of this file are derived from MMDVMHost by Jonathan Naylor G4KLX, Copyright (C) 2015-2026, licensed under GPL-2.0-or-later.` header consistently. Workspace `LICENSE` and `LICENSES/` present. No missing attribution found.

## `#[expect]` audit

Single site: `mmdvm-core/src/frame.rs:70-73`. `clippy::cast_possible_truncation` with rationale "bounded by MAX_PAYLOAD_LEN check above". Bound is at line 63; the cast at line 74 is `(3 + frame.payload.len()) as u8` where `payload.len() <= 252 ⇒ 3 + len <= 255`. Reason is truthful.

## TODO/FIXME/HACK

Only `mmdvm-core/src/config.rs:17` (`TODO: full SetConfig encoding`). No FIXME/HACK/XXX.

## Highest-impact bugs

1. **modem_loop.rs:425-458** — `pending_set_mode` correlation is racy across handle-side timeouts. Real-world reproducible.
2. **frame.rs:252-281 (ax25-codec)** — `build_ax25` panics on >8 digipeaters with only a pub-field invariant. Trippable from safe code.
3. **decoder.rs:71 + 88-142 (kiss-tnc)** — Unbounded `push`, plus O(n²) drain on all-FEND streams, are the two adversarial-input DoS handles; both are latent, capped by DEFAULT_MAX_FRAME_LEN.
4. **frame.rs:165-184 (kiss-tnc)** — Lenient Return-with-payload accepted then silently truncated on re-encode; asymmetry callers won't expect.
5. **modem_loop.rs:487-498** — Unknown `protocol` values from a `GetVersion` response are accepted verbatim and steer status parsing.
