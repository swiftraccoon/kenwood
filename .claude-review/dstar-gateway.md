# D-STAR reflector stack code review

Scope: `dstar-gateway-core/`, `dstar-gateway/`, `dstar-gateway-server/`

## dstar-gateway-core (codecs, sessions)

### Codec parsing

- `dstar-gateway-core/src/codec/dcs/decode.rs:280-281` — DCS EOT is inferred when `slow_data == [0x55, 0x55, 0x55]`. Legit mid-stream frames whose 3-byte slow_data field happens to hold the D-STAR sync pattern (which IS `0x55 0x55 0x55` at sync intervals) will be flagged `is_end`, prematurely killing streams and (via `on_dcs_voice`) forcing state back to `Linked`. Test `voice_eot_marker_alone_also_detected` locks the behavior in. Mostly-benign correctness caveat but user-visible.
- `dstar-gateway-core/src/codec/dextra/decode.rs:103-114` — `extract_callsign` extracts bytes `[0..8]` verbatim. For the 11-byte LINK/UNLINK packet the wire spec places the callsign in `[0..7]` with a space pad at `[7]`, but a peer that puts a non-space byte at `[7]` will have that byte silently stored as part of the callsign the server records. Poll/unlink then compare byte-for-byte against that mutated value. Not a memory-safety bug, but the doc comment above the function claims callers verify slice ≥ 8 bytes even though the code no-ops on short slices — misleading contract.
- `dstar-gateway-core/src/codec/dplus/decode.rs:154-176` — `decode_dsvt_server` uses byte `[16]` as the "seq" for both `VoiceData` and `VoiceEot`. In the DPlus wire layout byte 16 is the seq for the 29-byte VOICE frame and the 32-byte EOT (seq | 0x40 is at [16]). But comments on `parse_dsvt_common` say "Stream id at [14..16] little-endian" while sequence-byte reads live at [16]. Consistent, so not a bug, but there's no verification of the EOT trailer at `[26..32]`. A conforming DPlus reflector never sends 32-byte non-EOT DSVT packets, so shape checks are sufficient.
- `dstar-gateway-core/src/codec/dplus/auth.rs:96-137` — `validate_chunk_header` correctly rejects `chunk_len < 8` and truncated remainder. Note `chunk_len` is bounded to 0x0FFF, so `cursor + chunk_len` cannot overflow `usize` on any platform. Loop terminates because `chunk_len ≥ 8` guarantees cursor advances. Safe.

### DPlus TCP auth / host-list

- **`dstar-gateway/src/auth/client.rs:358-381`** — `read_response` `loop { stream.read }` collects into an unbounded `Vec<u8>`, capped only by the 5-second per-read timeout. A malicious or misconfigured auth endpoint that continuously trickles data below the per-read window can OOM the process. The auth server is trusted-by-DNS (plain TCP, no TLS, no cert pinning); a hostile DNS response or a route hijack turns this into a remote memory-exhaustion primitive against every gateway consumer. Add a hard cap (e.g. 1 MiB — parseable chunks are ≤4 KiB).
- `dstar-gateway-core/src/codec/dplus/auth.rs` — no challenge-response, no HMAC, no signature. The "auth" is a plaintext TCP hello followed by the server returning a host list; the security promise is "auth.dstargateway.org told us these are the reflectors." Documented behavior matching ircDDBGateway, but consumers should know: if the auth server (or the TCP path to it) is compromised, an attacker chooses the reflector IPs your client dials.
- `dstar-gateway/src/auth/client.rs:393-431` — build_auth_packet is fine; callsign type invariant makes the 8-byte copy safe.

### Server session state machine

- **`dstar-gateway-core/src/session/server/core.rs:290-319` (`on_dextra_link`) and `604-639` (`on_dcs_link`)** — When the session is already `Linked` (or, for DCS, even `Streaming`), a fresh LINK is accepted and `self.client_callsign = Some(callsign)` unconditionally overwrites the stored callsign. The authorizer runs at the shell only; nothing here rejects a callsign that differs from the currently-linked one. Combined with the shell's `link_capacity_reject` fast-path (line 843 of endpoint.rs: `if module_of(peer) == Some(module) { return None }`), a peer whose address is already in the pool sails past the caps check. Consequence: any attacker who can send a UDP packet from the source address of an existing peer (LAN spoof, on-path attacker, or NAT-shared clients) can silently rewrite the linked callsign — impersonation with no log line. DCS additionally emits a fresh `ClientLinked` event without clearing the stream, which desynchronizes downstream metrics.
- `dstar-gateway-core/src/session/server/core.rs:415-442` (`on_dplus_link1`) — Every LINK1, including retransmits from a `Linked` or `Streaming`-adjacent state, enqueues another 5-byte ACK. There is no per-peer rate limit on LINK1 processing (the token bucket is voice-fan-out only). Peer can spam LINK1 forever and get echoed → mild amplification.
- **`dstar-gateway-core/src/session/server/core.rs:495-516` (`on_dplus_unlink`)** — DPlus UNLINK carries no callsign on the wire, so the handler transitions to Closed and emits `ClientUnlinked` based on source address alone. An attacker who can spoof a target peer's UDP source can boot them off the reflector with a single 5-byte packet. Inherent to the wire protocol; not fixable in this codec, but worth documenting to consumers.
- **`dstar-gateway-server/src/tokio_shell/endpoint.rs:564-591` (DPlus LINK1 pool-slot allocation)** — LINK1 has no callsign so the authorizer cannot gate it; the pool entry is created on any LINK1, held until `max_total_clients` (default 250) or the 30-s keepalive-inactivity sweep evicts it. An attacker sending LINK1 from many spoofed source-port tuples fills the pool in <250 packets and blocks legitimate peers for 30 seconds. There is no per-source-IP quota. Rate-limit LINK1 per source IP or shorten the mid-handshake inactivity timeout.
- `dstar-gateway-core/src/session/outbox.rs` — Outbox is an unbounded `BinaryHeap`. Not currently exploitable because `drive_core` in `endpoint.rs:1540` drains it fully every tick, but any future codepath that enqueues without immediately draining (e.g. rate-limited replies) would create an unbounded per-peer queue.

### Cross-protocol forwarding

- **`dstar-gateway-server/src/tokio_shell/transcode.rs:150-189` and `endpoint.rs:915-957`** — Cross-protocol header sanitation is nonexistent. The originator's `DStarHeader` (rpt1/rpt2/my_call/my_suffix and all three flag bytes) is copied byte-for-byte into the target-protocol encoder. A DExtra client can put an arbitrary `REF999 G` in rpt2 and any callsign in `my_call`, and DCS/DPlus subscribers see those exact fields. On DCS, `dcs::encode_voice` at `encode.rs:298-321` copies attacker-controlled bytes into every 100-byte fan-out frame including `flag1/2/3` at [4..7]. Not a memory issue (bounded copies), but a spoofing/injection primitive: a DExtra flooder can announce voice streams that appear to originate from arbitrary reflectors/operators on DCS/DPlus modules. The scoping note "cross_protocol_forwarding = false" default limits blast radius, but any operator who enables it exposes DCS/DPlus modules to arbitrary DExtra-side header content.
- `dstar-gateway-server/src/tokio_shell/transcode.rs:200-210` — `StreamStart` → DCS branch fabricates a `VoiceFrame::silence()` because DCS has no separate header packet. Downstream DCS decoders will see one leading silence frame per cross-protocol stream. Correctness caveat noted in the comment.

### Fetcher / hosts

- **`dstar-gateway/src/hosts_fetcher/fetcher.rs:8` and `52-60`** — URL is hard-coded `http://xlxapi.rlx.lu/...` (not HTTPS). Any on-path attacker replaces the returned reflector list with their own, redirecting every subsequent client dial to attacker-controlled IPs. Switch to HTTPS and cache the previous good directory for fallback. The comments in `Cargo.toml:27-34` show rustls-tls is already available.
- **`dstar-gateway/src/hosts_fetcher/fetcher.rs:58`** — `.text().await` on the reqwest response has no `Content-Length` cap and no read budget. reqwest 0.12's default body limit is effectively unbounded for `.text()`; a hostile upstream (given the plain-HTTP MITM above) can send gigabytes and OOM the process. Wrap with `reqwest::Response::bytes_stream()` and cap at, say, 1 MiB.
- No atomic-write / file-persistence exists in this fetcher — nothing is written to disk here — so path-traversal concerns don't apply.
- `dstar-gateway-core/src/hosts.rs:54-82` (`HostFile::parse`) and `134-164` (`parse_xlx_directory`) — string handling is safe; port defaults to `default_port` if the third field fails to parse (documented). No overflow, no panic paths.

### Reflector server / pool

- `dstar-gateway-server/src/reflector/reflector.rs:277-333` — `run()` binds sockets from the config; graceful shutdown works. `CROSS_PROTOCOL_BUS_CAPACITY = 256` is a compile-time constant. Under sustained cross-protocol traffic a slow endpoint will `broadcast::error::RecvError::Lagged` (handled at `endpoint.rs:1694`) — the "drop stale frames" behavior is documented and correct.
- `dstar-gateway-server/src/client_pool/pool.rs:114-149` (`insert`/`remove`) — Insert/remove touch the forward and reverse maps in sequence and the doc-comment correctly warns "not cancel-safe." The caller (`endpoint.rs`) never `.await` cancels between the two `.lock().await`s in practice, but the `handle_inbound` doc at endpoint.rs:404-410 is the only barrier. Any future `tokio::select!` around `handle_inbound` risks partial pool state.
- `dstar-gateway-server/src/tokio_shell/endpoint.rs:1644` — `interval.tick().await` is used to consume the immediate first tick. Non-issue, but this comment ("first sweep runs one full period after startup") relies on `MissedTickBehavior::Delay` — check that a very short `keepalive_interval` (e.g. 100 ms cap on line 1637) doesn't produce keepalive bursts to freshly-linked peers.
- `dstar-gateway-server/src/tokio_shell/endpoint.rs:1101-1135` (`encode_keepalive_for`) — DCS keepalive: `payload.extend_from_slice(self.settings.reflector_callsign.as_bytes().as_slice().get(..7)?)`. `as_bytes()` on `Callsign` returns `&[u8; 8]`, so `.get(..7)` always succeeds — the `?` is dead. Not a bug, but confusing.
- `dstar-gateway-server/src/bin/polaris.rs:83` — `polaris` production binary hard-codes `AllowAllAuthorizer`. Documented as a "test reflector," but the crate ships this as its named binary. Any deployment of `polaris` accepts every callsign / every peer with `ReadWrite` access.

### DoS / rate limits

- `dstar-gateway-server/src/reflector/config.rs:266` — `tx_rate_limit_frames_per_sec: 60.0` gates only voice fan-out per-peer TX budget. There is no ingress rate limit — a peer can send arbitrarily many `Poll`/`Link1`/malformed datagrams per second; each is decoded, dispatched, and the pool lock is taken. The maintenance sweep sits ABOVE the socket arm in `run()` (endpoint.rs:1666-1674) to avoid starvation, good — but the CPU cost per malformed datagram (decode + pool.contains lock) is untamed.
- `dstar-gateway-server/src/client_pool/pool.rs:243-262` — Send-failure eviction threshold hard-coded to `DEFAULT_UNHEALTHY_THRESHOLD = 5`. Documented "will be configurable via ReflectorConfig in a follow-up patch." Latent TODO.

### Suspicious `#[expect]` / weak reasons

- All `#[expect(clippy::unwrap_used, reason = "test helper: … a zero would panic only this test")]` in test modules are fine. `#[expect(clippy::result_large_err, ...)]` blocks in session.rs/core.rs have solid reasons. `#[expect(clippy::cast_possible_truncation, reason = "mask guarantees 0..=255")]` at auth.rs:244 / :246 is a genuine correctness statement. No weak `reason` strings surfaced.

### TODO/FIXME/HACK

- `dprs/parser.rs:55` — comment illustration, not code.
- `dcs/decode.rs:705` and `dextra/decode.rs:652` — `b"XXXX"` string literals in tests, not real markers.
- No `TODO`/`FIXME`/`HACK` markers in server or shell crates.

### Docs

- `dstar-gateway-core/src/codec/dextra/decode.rs:85-102` — Doc says "Callers must have already verified that the slice is at least 8 bytes long." Implementation actually defends with `bytes.get(..8)` and returns a space-padded default — the promise is stricter than the guarantee.
- `dstar-gateway-server/src/tokio_shell/endpoint.rs:1101-1135` — Doc claims the returned `Option` allows short-circuit on missing entries, but every internal `?` fires only on impossible conditions (`.get(..7)` on a `&[u8; 8]`).

### Highest-priority remediations

1. Wrap `hosts_fetcher::fetch_xlx_directory` and `AuthClient::read_response` with a hard byte cap. Switch the XLX URL to HTTPS.
2. Sanitize cross-protocol headers: at `transcode.rs`, force `flag1/2/3 = 0` and rewrite `rpt1[7]`/`rpt2[7]` (module) and the reflector-callsign portion of `rpt2` to the local reflector's identity before re-encoding. Otherwise "cross-protocol forwarding" is a spoofing amplifier.
3. In `on_dextra_link`/`on_dcs_link`, reject a LINK whose callsign differs from the currently-stored one; require an explicit UNLINK first.
4. Add a per-source-IP LINK1 quota (or shorten the pre-authenticated-slot timeout) so DPlus pools cannot be filled with spoofed 5-byte packets in a burst.
5. Replace the `AllowAllAuthorizer` default in the `polaris` binary with a config-driven authorizer, or rename the binary to make its permissive default unmistakable.
