//! `DCS` protocol (`DCS` reflectors, UDP port 30051).
//!
//! `DCS` uses 100-byte voice packets that embed the full D-STAR header
//! in every frame (unlike DExtra/DPlus which only send the header once).
//! Connection uses a 519-byte packet. Keepalive interval is 2 seconds.
//!
//! # Packet formats (per `g4klx/ircDDBGateway` and `LX3JL/xlxd`)
//!
//! | Packet       | Size | Format |
//! |--------------|------|--------|
//! | Connect      | 519  | callsign\[8\] + modules + 0x0B + zeros |
//! | Disconnect   | 19   | callsign\[8\] + module + 0x20 0x00 + name\[8\] |
//! | Poll         | 17   | callsign\[8\] + 0x00 + name\[8\] |
//! | Voice        | 100  | "0001" + flags + header fields + AMBE + seq |

// TODO: implement DCS packet builders, parser, and async client.
// The packet formats are documented above and verified in
// ref/ircDDBGateway/Common/DCSProtocolHandler.cpp.
