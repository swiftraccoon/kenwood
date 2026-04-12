//! `DPlus` (REF reflectors, UDP port 20001) wire codec.
//!
//! See [`packet`] for the canonical packet enums. [`encode`] and
//! [`decode`] are the wire-format (en/de)coders, [`auth`] parses the
//! TCP host list response from `auth.dstargateway.org`.
//!
//! Reference implementations:
//! - `ircDDBGateway/Common/DPlusProtocolHandler.cpp` (parser dispatch)
//! - `ircDDBGateway/Common/ConnectData.cpp` (connect packet codec)
//! - `ircDDBGateway/Common/DPlusAuthenticator.cpp` (TCP auth)
//! - `ircDDBGateway/Common/HeaderData.cpp` (voice header)
//! - `ircDDBGateway/Common/AMBEData.cpp` (voice data + EOT)
//! - `xlxd/src/cdplusprotocol.cpp` (mirror reference)

pub mod auth;
pub mod consts;
pub mod decode;
pub mod encode;
pub mod error;
pub mod packet;

pub use auth::{DPlusHost, HostList, parse_auth_response};
pub use decode::{decode_client_to_server, decode_server_to_client};
pub use encode::{
    encode_link1, encode_link1_ack, encode_link2, encode_link2_reply, encode_poll,
    encode_poll_echo, encode_unlink, encode_unlink_ack, encode_voice_data, encode_voice_eot,
    encode_voice_header,
};
pub use error::DPlusError;
pub use packet::{ClientPacket, Link2Result, ServerPacket};
