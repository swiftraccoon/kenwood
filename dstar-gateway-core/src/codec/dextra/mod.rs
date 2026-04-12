//! `DExtra` (XRF/XLX reflectors, UDP port 30001) wire codec.
//!
//! See [`packet`] for the canonical packet enums. See [`encode`] for
//! TX-side encoders and [`decode`] for RX-side decoders.
//!
//! Reference implementations:
//! - `ircDDBGateway/Common/DExtraProtocolHandler.cpp` (parser dispatch)
//! - `ircDDBGateway/Common/ConnectData.cpp:278-321` (connect codec)
//! - `ircDDBGateway/Common/HeaderData.cpp:590-635` (voice header)
//! - `ircDDBGateway/Common/AMBEData.cpp:318-345` (voice data + EOT)
//! - `xlxd/src/cdextraprotocol.cpp` (mirror reference)

pub mod consts;
pub mod decode;
pub mod encode;
pub mod error;
pub mod packet;

pub use decode::{decode_client_to_server, decode_server_to_client};
pub use encode::{
    encode_connect_ack, encode_connect_link, encode_connect_nak, encode_poll, encode_poll_echo,
    encode_unlink, encode_voice_data, encode_voice_eot, encode_voice_header,
};
pub use error::DExtraError;
pub use packet::{ClientPacket, ConnectResult, ServerPacket};
