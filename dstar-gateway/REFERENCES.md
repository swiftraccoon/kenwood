# External References

Every wire format that `dstar-gateway` implements was derived from
reading (and, in places, stealing constants directly from) the two
canonical GPL-licensed reference implementations: **`g4klx/ircDDBGateway`**
and **`LX3JL/xlxd`**. This file pins the exact commit hashes we
consulted and lists every file/line-range reference that made it into
the dstar-gateway source tree.

If you find a discrepancy between our implementation and the upstream
C++ reference, it is almost certainly a bug on our side. Please open
an issue with the line number you inspected.

## Pinned versions

| Project | Commit | Clone path |
|---------|--------|-----------|
| `g4klx/ircDDBGateway` | [`f5ab9f7c93c6f28147b84a8f9667f6cc4c027eaa`](https://github.com/g4klx/ircDDBGateway/commit/f5ab9f7c93c6f28147b84a8f9667f6cc4c027eaa) | `ref/ircDDBGateway/` (gitignored) |
| `LX3JL/xlxd` | [`bf5d0148dbdf2534af129ca3cc034c5051dcfc8d`](https://github.com/LX3JL/xlxd/commit/bf5d0148dbdf2534af129ca3cc034c5051dcfc8d) | `ref/xlxd/` (gitignored) |

Both are GPL-2.0. `dstar-gateway` is also licensed under GPL-2.0-or-later
for this reason.

## File-level references, by reference project

### g4klx/ircDDBGateway

All paths below are relative to the project root (e.g.
`Common/DPlusProtocolHandler.cpp` lives at
`ref/ircDDBGateway/Common/DPlusProtocolHandler.cpp`).

#### DPlus (REF port 20001)

| File | Lines | What we use | Consumed in |
|------|-------|-------------|------------|
| `Common/DPlusProtocolHandler.cpp` | — | Inbound packet length-table dispatch (the 13 / 15 / 17 / 29 / 32 / 56 / 58 byte table); this is the foundation of `dstar-gateway-core::codec::dplus::decode` | `dstar-gateway-core/src/codec/dplus/decode.rs` |
| `Common/DPlusProtocolHandler.cpp` | 64-68 | NAK packet layout | `dstar-gateway-core/src/codec/dplus/consts.rs:45-48` |
| `Common/DPlusHandler.cpp` | 57 | `DPLUS_POLL_INTERVAL = 1` second | `dstar-gateway-core/src/codec/dplus/consts.rs:16-17` |
| `Common/DPlusHandler.cpp` | 58 | `DPLUS_INACTIVITY_TIMEOUT = 30` seconds | `dstar-gateway-core/src/codec/dplus/consts.rs:26` |
| `Common/DPlusHandler.cpp` | 481-482 | Outbound keepalive packet | `dstar-gateway-core/src/codec/dplus/consts.rs:51` |
| `Common/DPlusAuthenticator.cpp` | 62-200 | Full TCP auth flow (connect → send auth packet → read host list → close) | `dstar-gateway-core/src/codec/dplus/auth.rs`, `dstar-gateway/src/auth/client.rs` |
| `Common/DPlusAuthenticator.cpp` | 111-143 | The 56-byte auth packet layout (version + callsign + magic bytes) | `dstar-gateway/src/auth/client.rs:104-140, 206-230` |
| `Common/DPlusAuthenticator.cpp` | 151-192 | Host-list response parsing | `dstar-gateway-core/src/codec/dplus/auth.rs` |
| `Common/ConnectData.cpp` | 251-259 | LINK/UNLINK packet layouts (Phase 1) | `dstar-gateway-core/src/codec/dplus/consts.rs:92-95` |
| `Common/ConnectData.cpp` | 441-447 | DPlus LINK2 packet (Phase 2) | `dstar-gateway-core/src/codec/dplus/consts.rs:60-63` |
| `Common/ConnectData.cpp` | 449-473 | DPlus LINK1 packet (Phase 1) | `dstar-gateway-core/src/codec/dplus/encode.rs:89-133`, `dstar-gateway-core/src/codec/dplus/consts.rs:111` |
| `Common/ConnectData.cpp` | 475-481 | DPlus OKRW ack packet | `dstar-gateway-core/src/codec/dplus/consts.rs:68` |
| `Common/HeaderData.cpp` | 515-529 | D-STAR embedded header (DCS variant) | `dstar-gateway-core/src/codec/dcs/mod.rs:16` |
| `Common/HeaderData.cpp` | 590-635 | DExtra voice header (56 bytes) | `dstar-gateway-core/src/codec/dextra/encode.rs:139` |
| `Common/HeaderData.cpp` | 619-623 | Raw `memcpy` callsign copy (RX lenient parsing) | `dstar-gateway-core/src/types/callsign.rs:10-12, 67-71, 153-156` |
| `Common/HeaderData.cpp` | 637-684 | `getDPlusData` (full DPlus voice header serialization) | `dstar-gateway-core/src/header.rs:27-30, 101-105, 116-122`, `dstar-gateway-core/src/codec/dplus/encode.rs:169-247` |
| `Common/CCITTChecksum.cpp` | — | CRC-CCITT reference | `dstar-gateway-core/src/header.rs:28` |
| `Common/AMBEData.cpp` | 317-345 | DExtra voice data + EOT frames | `dstar-gateway-core/src/codec/dextra/encode.rs:211-280` |
| `Common/AMBEData.cpp` | 347-388 | DPlus voice data frame | `dstar-gateway-core/src/codec/dplus/encode.rs:249` |
| `Common/AMBEData.cpp` | 380-388 | DPlus EOT marker inside voice frame | `dstar-gateway-core/src/codec/dplus/encode.rs:291` |
| `Common/AMBEData.cpp` | 391-431 | DCS 100-byte voice frame layout | `dstar-gateway-core/src/codec/dcs/mod.rs:15, dstar-gateway-core/src/codec/dcs/consts.rs:57-63` |
| `Common/DStarDefines.h` | 34 | `DSTAR_FRAME_SIZE_BYTES = 9` (AMBE frame size) | `dstar-gateway-core/src/codec/dplus/consts.rs:137` |
| `Common/DStarDefines.h` | 85-92 | Slow-data header block size and bit pattern | `dstar-gateway-core/src/slowdata/block.rs:8`, `dstar-gateway-core/src/slowdata/mod.rs:15-16` |
| `Common/DStarDefines.h` | 111-113 | Slow-data scrambler constants | `dstar-gateway-core/src/slowdata/mod.rs:16`, `dstar-gateway-core/src/slowdata/scrambler.rs:3` |
| `Common/DStarDefines.h` | 115-117 | Reflector port assignments (DPlus=20001, DEXTRA=30001, DCS=30051) | `dstar-gateway-core/src/codec/dplus/consts.rs:10`, `dstar-gateway-core/src/codec/dextra/consts.rs:10`, `dstar-gateway-core/src/codec/dcs/consts.rs:10`, `dstar-gateway-core/src/types/protocol_kind.rs:25` |
| `Common/DStarDefines.h` | 122 | Magic bytes table | `dstar-gateway-core/src/codec/dplus/consts.rs:33` |
| `Common/DStarDefines.h` | — | `Module` letter space ("A"–"Z") | `dstar-gateway-core/src/types/module.rs:8` |
| `Common/DPRSHandler.cpp` | 120-260 | DPRS position decoder | `dstar-gateway-core/src/dprs/mod.rs:7` |
| `Common/DPRSHandler.cpp` | — | `calcCRC` function for DPRS checksum | `dstar-gateway-core/src/dprs/crc.rs:5` |
| `Common/SlowDataEncoder.cpp` | — | Slow-data assembler reference | `dstar-gateway-core/src/slowdata/assembler.rs:78`, `dstar-gateway-core/src/slowdata/mod.rs:15` |

#### DExtra (XRF/XLX port 30001)

| File | Lines | What we use | Consumed in |
|------|-------|-------------|------------|
| `Common/DExtraProtocolHandler.cpp` | — | Inbound packet length-table dispatch | `dstar-gateway-core/src/codec/dextra/decode.rs` (mirror reference) |
| `Common/DExtraHandler.cpp` | 51 | `DEXTRA_POLL_INTERVAL` | `dstar-gateway-core/src/codec/dextra/consts.rs:15-17` |
| `Common/DExtraHandler.cpp` | 52 | `DEXTRA_INACTIVITY_TIMEOUT` | `dstar-gateway-core/src/codec/dextra/consts.rs:21-23` |
| `Common/ConnectData.cpp` | 278-321 | DExtra LINK / UNLINK / ACK / NAK layouts | `dstar-gateway-core/src/codec/dextra/mod.rs:7-11`, `dstar-gateway-core/src/codec/dextra/encode.rs:18-100` |
| `Common/ConnectData.cpp` | 283-300 | LINK 11-byte payload | `dstar-gateway-core/src/codec/dextra/consts.rs:45-47` |
| `Common/ConnectData.cpp` | 302-316 | ACK 14-byte payload | `dstar-gateway-core/src/codec/dextra/consts.rs:50-52`, `dstar-gateway-core/src/codec/dextra/encode.rs:60-100` |
| `Common/ConnectData.cpp` | 304-307 | ACK "ACK" marker bytes | `dstar-gateway-core/src/codec/dextra/consts.rs:73-76` |
| `Common/ConnectData.cpp` | 312-315 | NAK "NAK" marker bytes | `dstar-gateway-core/src/codec/dextra/consts.rs:79-81` |
| `Common/PollData.cpp` | 155-168 | DExtra keepalive (9 bytes) | `dstar-gateway-core/src/codec/dextra/consts.rs:55-57`, `dstar-gateway-core/src/codec/dextra/encode.rs:102-137` |

#### DCS (port 30051)

| File | Lines | What we use | Consumed in |
|------|-------|-------------|------------|
| `Common/DCSHandler.cpp` | 54 | `DCS_POLL_INTERVAL` | `dstar-gateway-core/src/codec/dcs/consts.rs:15-17` |
| `Common/DCSHandler.cpp` | 55 | `DCS_INACTIVITY_TIMEOUT` | `dstar-gateway-core/src/codec/dcs/consts.rs:21-23` |
| `Common/ConnectData.cpp` | 323-393 | DCS LINK / UNLINK / ACK / NAK layouts | `dstar-gateway-core/src/codec/dcs/mod.rs:14`, `dstar-gateway-core/src/codec/dcs/encode.rs:20-120` |
| `Common/ConnectData.cpp` | 337-363 | DCS LINK 519-byte packet | `dstar-gateway-core/src/codec/dcs/encode.rs:20` |
| `Common/ConnectData.cpp` | 344-358 | DCS LINK/NAK accept list (lenient) | `dstar-gateway-core/src/codec/dcs/consts.rs:89-95` |
| `Common/ConnectData.cpp` | 345-357 | DCS LINK marker enumeration | `dstar-gateway-core/src/codec/dcs/packet.rs:118` |
| `Common/ConnectData.cpp` | 364 | DCS LINK size (519U) | `dstar-gateway-core/src/codec/dcs/consts.rs:33` |
| `Common/ConnectData.cpp` | 366-372 | DCS UNLINK (19U) | `dstar-gateway-core/src/codec/dcs/encode.rs:79`, `dstar-gateway-core/src/codec/dcs/consts.rs:38` |
| `Common/ConnectData.cpp` | 377-379 | DCS ACK tag offset | `dstar-gateway-core/src/codec/dcs/consts.rs:67-70` |
| `Common/ConnectData.cpp` | 380, 388 | DCS ACK / NAK sizes (14U) | `dstar-gateway-core/src/codec/dcs/consts.rs:43` |
| `Common/ConnectData.cpp` | 384-386 | DCS NAK tag offset | `dstar-gateway-core/src/codec/dcs/consts.rs:78` |
| `Common/AMBEData.cpp` | 391-431 | DCS 100-byte voice frame | `dstar-gateway-core/src/codec/dcs/mod.rs:15` |
| `Common/AMBEData.cpp` | 398-401 | DCS voice frame header bytes | `dstar-gateway-core/src/codec/dcs/consts.rs:62` |
| `Common/AMBEData.cpp` | 410-414 | DCS voice frame trailer | `dstar-gateway-core/src/codec/dcs/consts.rs:83` |
| `Common/AMBEData.cpp` | 430 | DCS AMBE frame size (100U) | `dstar-gateway-core/src/codec/dcs/consts.rs:57` |
| `Common/PollData.cpp` | 170-204 | DCS keepalive (17U) | `dstar-gateway-core/src/codec/dcs/mod.rs:17`, `dstar-gateway-core/src/codec/dcs/consts.rs:48` |

### LX3JL/xlxd

Used as the "mirror" reference — we cross-check every ircDDBGateway
finding against xlxd to catch places where the two agree (confirming
it's on the wire) and places where they diverge (where we have to
pick the more widely deployed behavior).

| File | Lines | What we use | Consumed in |
|------|-------|-------------|------------|
| `src/main.h` | 93 | `#define DPLUS_PORT 20001` cross-check | `dstar-gateway-core/src/codec/dplus/consts.rs:11` |
| `src/main.h` | 94 | `DPLUS_KEEPALIVE_PERIOD = 1` cross-check | `dstar-gateway-core/src/codec/dplus/consts.rs:18` |
| `src/cdplusprotocol.cpp` | 430 | DPlus LINK1 cadence cross-check | `dstar-gateway-core/src/codec/dplus/consts.rs:62` |
| `src/cdplusprotocol.cpp` | 447-451 | DPlus OKRW tag | `dstar-gateway-core/src/codec/dplus/consts.rs:70` |
| `src/cdplusprotocol.cpp` | 529-533 | DPlus header retransmit cadence | `dstar-gateway-core/src/codec/dplus/consts.rs:76` |
| `src/cdplusprotocol.cpp` | 535-544 | DPlus keepalive body | `dstar-gateway-core/src/codec/dplus/consts.rs:94`, `dstar-gateway-core/src/codec/dplus/encode.rs:139` |
| `src/cdplusprotocol.cpp` | 541-545 | DPlus NAK keepalive response | `dstar-gateway-core/src/codec/dplus/consts.rs:101` |
| `src/cdextraprotocol.cpp` | — | DExtra wire format mirror | `dstar-gateway-core/src/codec/dextra/mod.rs:11` |
| `src/cdcsprotocol.cpp` | — | DCS wire format mirror | `dstar-gateway-core/src/codec/dcs/mod.rs:19` |
| `src/cdcsprotocol.cpp` | 411 | DCS keepalive accept list | `dstar-gateway-core/src/codec/dcs/consts.rs:51-52` |
| `src/cdcsprotocol.cpp` | — | DCS trailer byte variant | `dstar-gateway-core/src/validator/diagnostic.rs:96` |

## How to update this file

1. Re-sync the local clones:
   ```bash
   git -C ref/ircDDBGateway pull
   git -C ref/xlxd pull
   git -C ref/ircDDBGateway rev-parse HEAD
   git -C ref/xlxd rev-parse HEAD
   ```
2. Update the **Pinned versions** table with the new hashes.
3. For any new reference that was added to the code since the last
   update, grep for `ircDDBGateway` and `xlxd` in the three crates
   and add a row for every new citation:
   ```bash
   rg -n 'ircDDBGateway|xlxd' dstar-gateway-core/src dstar-gateway/src dstar-gateway-server/src
   ```
4. Open a PR that touches only this file. The changelog CI check
   will **not** require a CHANGELOG entry (`.md` files don't match
   the Rust-code glob).
