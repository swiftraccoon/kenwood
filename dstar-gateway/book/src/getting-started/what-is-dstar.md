# What is D-STAR?

**D-STAR** (Digital Smart Technologies for Amateur Radio) is a
digital voice and data protocol for amateur radio, developed by
the Japan Amateur Radio League (JARL) in the early 2000s and
adopted most prominently by ICOM in their commercial radios. It
competes in roughly the same ecological niche as DMR and Yaesu
System Fusion: a radio-over-IP overlay that lets hams on opposite
sides of the world talk to each other by way of the Internet.

This book assumes some familiarity with amateur radio basics but
nothing specific to D-STAR. This chapter is a quick primer for
software engineers who came to the library via "someone asked me
to write a D-STAR client".

## The layer cake

A D-STAR QSO (conversation) between two operators in different
countries goes through several layers:

```text
[Operator A's radio]
        |
        v
[Operator A's local hotspot / repeater]   <-- MMDVM / ThumbDV / DVMega
        |
        v
[Reflector gateway]                       <-- ircDDBGateway, dstar-gateway
        |
        v  (DPlus / DExtra / DCS over UDP)
[Reflector server]                        <-- xlxd, ref_reflector
        |
        v  (fan-out)
[Other reflector gateways on the same module]
        |
        v
[Other operators' hotspots / repeaters]
        |
        v
[Other operators' radios]
```

`dstar-gateway` implements the **reflector gateway** layer and,
optionally, the **reflector server** layer. The radio-to-hotspot
leg is handled by a different family of protocols (MMDVM /
USRP / `thd75` CAT) and is out of scope for this library.

## Reflectors and modules

A **reflector** is a server that sits on the Internet and
broadcasts voice traffic to a group of connected clients. Each
reflector hosts up to 26 **modules**, named A through Z. A client
links to exactly one module at a time; clients on the same module
hear each other. Clients on different modules of the same
reflector do not.

Reflector naming follows a convention:
- `REFxxx` names refer to **DPlus** reflectors (e.g. REF030,
  REF001, REF077). Port 20001.
- `XRFxxx` names refer to **DExtra** reflectors (e.g. XRF757,
  XRF223). Port 30001.
- `XLXxxx` names refer to **XLX** reflectors, which speak a
  superset of the DExtra protocol (e.g. XLX307). Port 30001.
- `DCSxxx` names refer to **DCS** reflectors (e.g. DCS001,
  DCS002). Port 30051.

From the gateway's perspective, DExtra and XLX are the same wire
format — XLX just has more features. DPlus is different enough to
need a separate codec but shares the voice-frame structure. DCS
is different again and has a notably larger (100-byte) voice
frame.

## What does "linking" mean?

When a client "links" to a reflector, it sends a **LINK** packet
to the reflector's well-known port. The reflector either
acknowledges the link (ACK/OKRW) or rejects it (NAK/BUSY). Once
linked, the client and reflector exchange **keepalive** packets
at a protocol-specific cadence (1 second for DPlus, 1 second for
DExtra, 1 second for DCS) and the client sends voice frames
whenever the local operator keys up.

Disconnection happens by sending an **UNLINK** packet and
optionally waiting for the reflector's ACK. Most real-world
clients don't wait — they send UNLINK and drop the socket.
`dstar-gateway` supports both patterns: `AsyncSession::disconnect`
sends UNLINK and awaits the ACK, while `Drop` on the session
just severs the connection.

## Voice, headers, and streams

A **voice transmission** in D-STAR has three parts:
1. A **header** (56 or 58 bytes depending on the protocol),
   sent once at the start, containing the callsign, the
   destination (usually `CQCQCQ`), and the routing info.
2. A series of **voice frames**, each carrying 20 ms of AMBE-
   encoded audio plus a sync pattern and optional slow-data.
   DPlus/DExtra frames are 29 bytes; DCS frames are 100 bytes
   including an embedded copy of the header.
3. An **end-of-transmission** marker, which is a voice frame
   with a special `End` byte set.

A transmission is identified by a **stream id**: a non-zero
16-bit integer chosen by the sender. Every frame in the same
transmission carries the same stream id; overlapping
transmissions on the same reflector are distinguished by their
distinct stream ids. `dstar-gateway` enforces non-zero stream
ids via the `StreamId` newtype.

This is the protocol that `dstar-gateway` implements. The rest
of this book shows how to drive it from Rust without becoming an
expert in the underlying bits.
