//! `DCS` reflector wire codec (UDP port 30051).
//!
//! `DCS` is the most complex of the three protocols:
//! - 519-byte LINK with embedded HTML payload
//! - 19-byte UNLINK
//! - 14-byte ACK/NAK reply
//! - 17-byte poll (request and reply identical)
//! - 100-byte voice frame with embedded D-STAR header
//!
//! See [`packet`] for the canonical packet enums, [`encode`] for
//! TX-side encoders, and [`decode`] for RX-side decoders.
//!
//! Reference implementations:
//! - `ircDDBGateway/Common/ConnectData.cpp:323-393` (LINK/UNLINK/ACK/NAK)
//! - `ircDDBGateway/Common/AMBEData.cpp:391-431` (voice frame)
//! - `ircDDBGateway/Common/HeaderData.cpp:515-529` (embedded header)
//! - `ircDDBGateway/Common/PollData.cpp:170-204` (keepalive)
//! - `ircDDBGateway/Common/DCSHandler.cpp:54-55` (timer constants)
//! - `xlxd/src/cdcsprotocol.cpp` (mirror reference)

pub mod consts;
pub mod decode;
pub mod encode;
pub mod error;
pub mod packet;

pub use decode::{decode_client_to_server, decode_server_to_client};
pub use encode::{
    encode_connect_ack, encode_connect_link, encode_connect_nak, encode_connect_unlink,
    encode_poll_reply, encode_poll_request, encode_voice,
};
pub use error::DcsError;
pub use packet::{ClientPacket, ConnectResult, GatewayType, ServerPacket};
