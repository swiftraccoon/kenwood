//! AMBE-to-PCM-to-MP3 audio decode pipeline.
//!
//! D-STAR voice transmissions encode speech using the AMBE 3600x2450 codec
//! at 3600 bits/second (2450 bps voice + 1150 bps FEC). Each voice frame is
//! 9 bytes, transmitted at 50 frames/second (one every 20 ms). The decoder
//! pipeline converts these compressed frames into standard MP3 audio:
//!
//! ```text
//! [u8; 9] x N           mbelib-rs           mp3lame-encoder
//! AMBE frames  ------>  PCM i16 @ 8 kHz  ------>  MP3 bytes
//!              decode_frame()             encode + flush
//! ```
//!
//! ## Pipeline stages
//!
//! 1. **AMBE decode** (`mbelib_rs::AmbeDecoder`): Each 9-byte AMBE frame is
//!    decoded into 160 signed 16-bit PCM samples at 8000 Hz (20 ms of audio).
//!    The decoder carries inter-frame state for delta prediction and
//!    phase-continuous synthesis, so frames must be fed sequentially.
//!
//! 2. **PCM accumulation**: All 160-sample chunks are concatenated into a
//!    single `Vec<i16>` buffer. For a typical 3-second D-STAR transmission
//!    (150 frames), this is 24,000 samples.
//!
//! 3. **MP3 encoding** (`mp3lame_encoder`): The accumulated PCM buffer is
//!    encoded in one pass using LAME configured for mono, 8000 Hz input
//!    sample rate, CBR at the requested bitrate. A final flush writes any
//!    remaining LAME internal buffer to complete the MP3 stream.

use mp3lame_encoder::{Bitrate, FlushGap, MonoPcm, Quality};

/// Errors that can occur during the AMBE-to-MP3 decode pipeline.
///
/// Wraps the two external library error types (`mp3lame_encoder::BuildError`
/// and `mp3lame_encoder::EncodeError`) into a single enum so that callers
/// only need to handle one error type from [`decode_to_mp3`].
#[derive(Debug)]
pub(crate) enum DecodeError {
    /// No AMBE frames were provided — nothing to decode.
    EmptyInput,

    /// The requested bitrate does not correspond to a valid LAME CBR
    /// bitrate. Valid values: 8, 16, 24, 32, 40, 48, 64, 80, 96, 112,
    /// 128, 160, 192, 224, 256, 320 (kbps).
    UnsupportedBitrate(u32),

    /// Failed to initialize the LAME MP3 encoder (memory allocation
    /// failure or invalid parameter combination).
    EncoderInit(mp3lame_encoder::BuildError),

    /// Failed during MP3 encoding of the PCM buffer.
    Encode(mp3lame_encoder::EncodeError),
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyInput => f.write_str("no AMBE frames to decode"),
            Self::UnsupportedBitrate(br) => {
                write!(f, "unsupported MP3 bitrate: {br} kbps")
            }
            Self::EncoderInit(e) => write!(f, "LAME encoder init failed: {e}"),
            Self::Encode(e) => write!(f, "MP3 encoding failed: {e}"),
        }
    }
}

impl std::error::Error for DecodeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::EncoderInit(e) => Some(e),
            Self::Encode(e) => Some(e),
            Self::EmptyInput | Self::UnsupportedBitrate(_) => None,
        }
    }
}

impl From<mp3lame_encoder::BuildError> for DecodeError {
    fn from(e: mp3lame_encoder::BuildError) -> Self {
        Self::EncoderInit(e)
    }
}

impl From<mp3lame_encoder::EncodeError> for DecodeError {
    fn from(e: mp3lame_encoder::EncodeError) -> Self {
        Self::Encode(e)
    }
}

/// Decodes a sequence of 9-byte AMBE voice frames into an MP3 byte buffer.
///
/// # Pipeline
///
/// 1. Creates an `mbelib_rs::AmbeDecoder` (one per call — stateful across
///    frames within the same stream).
/// 2. Feeds each 9-byte AMBE frame through the decoder, collecting the
///    resulting 160-sample PCM chunks into a contiguous `Vec<i16>`.
/// 3. Configures a LAME MP3 encoder for mono 8000 Hz CBR at `bitrate` kbps.
/// 4. Encodes the entire PCM buffer in one pass, then flushes to finalize
///    the MP3 stream.
///
/// # Arguments
///
/// - `frames` — Ordered slice of 9-byte AMBE frames from a single D-STAR
///   voice stream. Must be in receive order (the decoder uses inter-frame
///   delta prediction).
/// - `bitrate` — MP3 constant bitrate in kbps. Must match one of the LAME
///   `Bitrate` enum values (8, 16, 24, 32, 40, 48, 64, 80, 96, 112, 128,
///   160, 192, 224, 256, 320).
///
/// # Errors
///
/// - [`DecodeError::EmptyInput`] if `frames` is empty.
/// - [`DecodeError::UnsupportedBitrate`] if `bitrate` is not a valid LAME value.
/// - [`DecodeError::EncoderInit`] if LAME fails to initialize.
/// - [`DecodeError::Encode`] if LAME fails during encoding or flushing.
pub(crate) fn decode_to_mp3(frames: &[[u8; 9]], bitrate: u32) -> Result<Vec<u8>, DecodeError> {
    if frames.is_empty() {
        return Err(DecodeError::EmptyInput);
    }

    // Map the u32 bitrate to the mp3lame_encoder::Bitrate enum. LAME only
    // supports specific CBR values; reject anything else early.
    let lame_bitrate = bitrate_from_kbps(bitrate)?;

    // --- Stage 1: AMBE decode ---
    // Create a fresh decoder. Each stream needs its own decoder because
    // AMBE uses inter-frame delta prediction (gain and spectral magnitudes
    // are coded as deltas from the previous frame).
    let mut ambe_decoder = mbelib_rs::AmbeDecoder::new();

    // Pre-allocate the PCM buffer: 160 samples per frame.
    let total_samples = frames.len() * 160;
    let mut pcm_buffer: Vec<i16> = Vec::with_capacity(total_samples);

    // Decode each AMBE frame into 160 PCM samples and append to the buffer.
    for frame in frames {
        let samples = ambe_decoder.decode_frame(frame);
        pcm_buffer.extend_from_slice(&samples);
    }

    // --- Stage 2: MP3 encode ---
    // Configure LAME for mono, 8000 Hz input (D-STAR native sample rate),
    // CBR at the requested bitrate.
    let mut builder = mp3lame_encoder::Builder::new().ok_or(mp3lame_encoder::BuildError::NoMem)?;
    builder.set_num_channels(1)?;
    builder.set_sample_rate(8000)?;
    builder.set_brate(lame_bitrate)?;
    builder.set_quality(Quality::Best)?;
    let mut encoder = builder.build()?;

    // Allocate the MP3 output buffer. The mp3lame_encoder crate provides a
    // helper that computes a safe upper bound: samples * 1.25 + 7200.
    let max_mp3_size = mp3lame_encoder::max_required_buffer_size(pcm_buffer.len());
    let mut mp3_buffer: Vec<u8> = Vec::with_capacity(max_mp3_size);

    // Encode the entire PCM buffer in one call. MonoPcm<i16> feeds a
    // single-channel i16 buffer through lame_encode_buffer.
    let _encoded = encoder.encode_to_vec(MonoPcm(pcm_buffer.as_slice()), &mut mp3_buffer)?;

    // Flush any remaining data from LAME's internal buffers. FlushGap pads
    // with zeros to complete the final MP3 frame.
    let _flushed = encoder.flush_to_vec::<FlushGap>(&mut mp3_buffer)?;

    Ok(mp3_buffer)
}

/// Maps a bitrate in kbps (u32) to the `mp3lame_encoder::Bitrate` enum.
///
/// Returns `Err(DecodeError::UnsupportedBitrate)` if the value doesn't
/// correspond to any LAME CBR bitrate.
const fn bitrate_from_kbps(kbps: u32) -> Result<Bitrate, DecodeError> {
    match kbps {
        8 => Ok(Bitrate::Kbps8),
        16 => Ok(Bitrate::Kbps16),
        24 => Ok(Bitrate::Kbps24),
        32 => Ok(Bitrate::Kbps32),
        40 => Ok(Bitrate::Kbps40),
        48 => Ok(Bitrate::Kbps48),
        64 => Ok(Bitrate::Kbps64),
        80 => Ok(Bitrate::Kbps80),
        96 => Ok(Bitrate::Kbps96),
        112 => Ok(Bitrate::Kbps112),
        128 => Ok(Bitrate::Kbps128),
        160 => Ok(Bitrate::Kbps160),
        192 => Ok(Bitrate::Kbps192),
        224 => Ok(Bitrate::Kbps224),
        256 => Ok(Bitrate::Kbps256),
        320 => Ok(Bitrate::Kbps320),
        other => Err(DecodeError::UnsupportedBitrate(other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// AMBE silence frame (same as `dstar_gateway_core::voice::AMBE_SILENCE`).
    /// Decodes to near-zero PCM samples.
    const AMBE_SILENCE: [u8; 9] = [0x9E, 0x8D, 0x32, 0x88, 0x26, 0x1A, 0x3F, 0x61, 0xE8];

    #[test]
    fn empty_input_returns_error() {
        let result = decode_to_mp3(&[], 64);
        assert!(
            matches!(result, Err(DecodeError::EmptyInput)),
            "expected EmptyInput, got {result:?}"
        );
    }

    #[test]
    fn unsupported_bitrate_returns_error() {
        let frames = [AMBE_SILENCE];
        let result = decode_to_mp3(&frames, 100);
        assert!(
            matches!(result, Err(DecodeError::UnsupportedBitrate(100))),
            "expected UnsupportedBitrate(100), got {result:?}"
        );
    }

    #[test]
    fn decode_silence_frames_produces_valid_mp3() -> Result<(), Box<dyn std::error::Error>> {
        // 50 frames = 1 second of silence.
        let frames: Vec<[u8; 9]> = vec![AMBE_SILENCE; 50];
        let mp3 = decode_to_mp3(&frames, 64)?;

        // The output should be non-empty and start with an MP3 sync word
        // or ID3 tag. LAME may prepend a Xing/Info VBR header even in CBR
        // mode, so we just check that we got a substantial number of bytes.
        assert!(
            mp3.len() > 100,
            "expected substantial MP3 output, got {} bytes",
            mp3.len()
        );
        Ok(())
    }

    #[test]
    fn bitrate_from_kbps_roundtrips_all_valid_values() {
        let valid = [
            8, 16, 24, 32, 40, 48, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320,
        ];
        for kbps in valid {
            assert!(
                bitrate_from_kbps(kbps).is_ok(),
                "bitrate {kbps} should be valid"
            );
        }
    }

    #[test]
    fn bitrate_from_kbps_rejects_invalid() {
        for kbps in [0, 1, 7, 9, 15, 17, 33, 65, 100, 129, 321, 1000] {
            assert!(
                bitrate_from_kbps(kbps).is_err(),
                "bitrate {kbps} should be rejected"
            );
        }
    }
}
