//! Validated D-STAR transmit indices within a 21-frame superframe.

use super::TypeError;

/// A transmit frame's index in the inclusive range 0 through 20.
///
/// This value contains no header or termination flags. Encoders add the
/// protocol's EOT marker separately. Received packets retain their original
/// sequence byte, including unknown bits, until a caller explicitly validates
/// an index for a new transmission.
///
/// The transmitter layout follows xlxd's `CDextraProtocol::EncodeDvFramePacket`
/// and `EncodeDvLastFramePacket`, which use a modulo-21 index and add `0x40`
/// separately for termination.
///
/// ```
/// use dstar_gateway_core::FrameSequence;
/// let final_index = FrameSequence::new(20).ok_or("valid final index")?;
/// assert_eq!(final_index.next(), FrameSequence::ZERO);
/// assert!(FrameSequence::new(0x40).is_none());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FrameSequence(u8);

impl FrameSequence {
    /// The synchronization frame at the beginning of a superframe.
    pub const ZERO: Self = Self(0);

    /// Construct an index, rejecting values outside 0 through 20.
    #[must_use]
    pub const fn new(index: u8) -> Option<Self> {
        if index <= 20 { Some(Self(index)) } else { None }
    }

    /// Return the index without any termination or header flag bits.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }

    /// Return the following index, wrapping from 20 to 0.
    #[must_use]
    pub const fn next(self) -> Self {
        if self.0 == 20 {
            Self::ZERO
        } else {
            Self(self.0 + 1)
        }
    }
}

impl TryFrom<u8> for FrameSequence {
    type Error = TypeError;

    fn try_from(index: u8) -> Result<Self, Self::Error> {
        Self::new(index).ok_or(TypeError::InvalidFrameSequence { got: index })
    }
}

impl From<FrameSequence> for u8 {
    fn from(index: FrameSequence) -> Self {
        index.get()
    }
}

impl std::fmt::Display for FrameSequence {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, formatter)
    }
}

impl std::fmt::Binary for FrameSequence {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Binary::fmt(&self.0, formatter)
    }
}

impl std::fmt::Octal for FrameSequence {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Octal::fmt(&self.0, formatter)
    }
}

impl std::fmt::LowerHex for FrameSequence {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::LowerHex::fmt(&self.0, formatter)
    }
}

impl std::fmt::UpperHex for FrameSequence {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::UpperHex::fmt(&self.0, formatter)
    }
}

#[cfg(test)]
mod tests {
    use super::FrameSequence;
    use crate::codec::dextra::{
        ClientPacket, decode_client_to_server, encode_voice_data, encode_voice_eot,
    };
    use crate::{NullSink, StreamId, VoiceFrame};

    const STREAM: StreamId = StreamId::new(1).unwrap();

    #[test]
    fn transmit_indices_keep_data_and_termination_distinct()
    -> Result<(), Box<dyn std::error::Error>> {
        for raw in [21, 31, 0x40, 0x80, u8::MAX] {
            assert!(
                FrameSequence::new(raw).is_none(),
                "flags and out-of-domain indices cannot enter a transmit operation: {raw}"
            );
        }
        let mut sequence = FrameSequence::ZERO;
        for expected in 0..21 {
            let mut bytes = [0_u8; 32];
            let length = encode_voice_data(&mut bytes, STREAM, sequence, &VoiceFrame::silence())?;
            let packet = decode_client_to_server(
                bytes.get(..length).ok_or("data packet length")?,
                &mut NullSink,
            )?;
            assert!(
                matches!(packet, ClientPacket::VoiceData { seq, .. } if seq == expected),
                "a data index must decode as data with its unchanged index: {packet:?}"
            );
            let length = encode_voice_eot(&mut bytes, STREAM, sequence)?;
            let packet = decode_client_to_server(
                bytes.get(..length).ok_or("EOT packet length")?,
                &mut NullSink,
            )?;
            assert!(
                matches!(packet, ClientPacket::VoiceEot { seq, .. } if seq == (expected | 0x40)),
                "termination adds only its independent end bit: {packet:?}"
            );
            sequence = sequence.next();
        }
        assert_eq!(
            sequence,
            FrameSequence::ZERO,
            "a complete superframe wraps to synchronization"
        );
        Ok(())
    }
}
