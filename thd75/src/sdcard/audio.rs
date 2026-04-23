//! Parser for WAV audio recording files.
//!
//! The TH-D75 records TX/RX audio to standard RIFF WAV files.
//! Per User Manual Chapter 20 and Operating Tips §5.14:
//!
//! - Format: 16 kHz sample rate, 16-bit signed PCM, mono.
//! - Maximum file size: 2 GB (approximately 18 hours of audio).
//!   Recording continues in a new file if the limit is exceeded.
//! - Recording band selectable: A or B (Menu No. 302).
//! - Recording starts/stops via Menu No. 301.
//!
//! # Location
//!
//! `/KENWOOD/TH-D75/AUDIO_REC/*.wav` — maximum 255 files per directory.
//!
//! # Details
//!
//! This parser validates the RIFF/WAV header and extracts metadata
//! (sample rate, bit depth, channels, data length, duration).
//! It does **not** decode PCM sample data.

use super::{SdCardError, read_u16_le, read_u32_le};

/// Expected sample rate for TH-D75 audio recordings (Hz).
const EXPECTED_SAMPLE_RATE: u32 = 16_000;

/// Expected bits per sample for TH-D75 audio recordings.
const EXPECTED_BITS_PER_SAMPLE: u16 = 16;

/// Expected channel count for TH-D75 audio recordings (mono).
const EXPECTED_CHANNELS: u16 = 1;

/// WAV audio format code for PCM.
const WAV_FORMAT_PCM: u16 = 1;

/// Minimum WAV file size: 44 bytes (RIFF header + fmt chunk + data chunk header).
const MIN_WAV_SIZE: usize = 44;

/// Metadata extracted from a TH-D75 audio recording WAV file.
///
/// Contains only the header information — PCM sample data is not
/// loaded or decoded.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioRecording {
    /// Sample rate in Hz. Expected: 16000 for TH-D75.
    pub sample_rate: u32,
    /// Bits per sample. Expected: 16 for TH-D75.
    pub bits_per_sample: u16,
    /// Number of audio channels. Expected: 1 (mono) for TH-D75.
    pub channels: u16,
    /// Size of the raw PCM data section in bytes.
    pub data_length: u32,
    /// Calculated recording duration in seconds.
    ///
    /// Computed as `data_length / (sample_rate * channels * bits_per_sample / 8)`.
    pub duration_secs: f64,
}

/// Parse a WAV audio recording file from raw bytes.
///
/// Validates the RIFF/WAV header structure and verifies the audio
/// format matches the TH-D75 specification (16 kHz, 16-bit, mono PCM).
///
/// # Errors
///
/// Returns [`SdCardError::FileTooSmall`] if the data is shorter than
/// the minimum WAV header size (44 bytes).
///
/// Returns [`SdCardError::InvalidWavHeader`] if the RIFF magic,
/// WAVE format tag, fmt chunk ID, or audio format code is invalid.
///
/// Returns [`SdCardError::UnexpectedAudioFormat`] if the sample rate,
/// bit depth, or channel count does not match the expected TH-D75
/// format.
pub fn parse(data: &[u8]) -> Result<AudioRecording, SdCardError> {
    if data.len() < MIN_WAV_SIZE {
        return Err(SdCardError::FileTooSmall {
            expected: MIN_WAV_SIZE,
            actual: data.len(),
        });
    }

    // Validate RIFF header: bytes 0-3 = "RIFF"
    if data.get(..4) != Some(b"RIFF") {
        return Err(SdCardError::InvalidWavHeader {
            detail: "missing RIFF magic bytes".to_owned(),
        });
    }

    // Validate WAVE format: bytes 8-11 = "WAVE"
    if data.get(8..12) != Some(b"WAVE") {
        return Err(SdCardError::InvalidWavHeader {
            detail: "missing WAVE format identifier".to_owned(),
        });
    }

    // Find the "fmt " sub-chunk. It usually starts at offset 12, but
    // we search to handle files with extra chunks before fmt.
    let fmt_offset = find_chunk(data, *b"fmt ").ok_or_else(|| SdCardError::InvalidWavHeader {
        detail: "fmt chunk not found".to_owned(),
    })?;

    // fmt chunk needs at least 16 bytes of data (after 8-byte chunk header).
    if data.len() < fmt_offset + 24 {
        return Err(SdCardError::FileTooSmall {
            expected: fmt_offset + 24,
            actual: data.len(),
        });
    }

    let audio_format = read_u16_le(data, fmt_offset + 8);
    if audio_format != WAV_FORMAT_PCM {
        return Err(SdCardError::InvalidWavHeader {
            detail: format!(
                "unsupported audio format code {audio_format} (expected {WAV_FORMAT_PCM} for PCM)"
            ),
        });
    }

    let channels = read_u16_le(data, fmt_offset + 10);
    let sample_rate = read_u32_le(data, fmt_offset + 12);
    let bits_per_sample = read_u16_le(data, fmt_offset + 22);

    // Validate TH-D75 expected format.
    if sample_rate != EXPECTED_SAMPLE_RATE
        || bits_per_sample != EXPECTED_BITS_PER_SAMPLE
        || channels != EXPECTED_CHANNELS
    {
        return Err(SdCardError::UnexpectedAudioFormat {
            sample_rate,
            bits_per_sample,
            channels,
        });
    }

    // Find the "data" sub-chunk.
    let data_offset = find_chunk(data, *b"data").ok_or_else(|| SdCardError::InvalidWavHeader {
        detail: "data chunk not found".to_owned(),
    })?;

    if data.len() < data_offset + 8 {
        return Err(SdCardError::FileTooSmall {
            expected: data_offset + 8,
            actual: data.len(),
        });
    }

    let data_length = read_u32_le(data, data_offset + 4);

    let bytes_per_sample_frame =
        f64::from(sample_rate) * f64::from(channels) * f64::from(bits_per_sample) / 8.0;
    let duration_secs = f64::from(data_length) / bytes_per_sample_frame;

    Ok(AudioRecording {
        sample_rate,
        bits_per_sample,
        channels,
        data_length,
        duration_secs,
    })
}

/// Search for a RIFF chunk by its 4-byte ID, starting after the
/// 12-byte RIFF header. Returns the offset of the chunk header.
fn find_chunk(data: &[u8], id: [u8; 4]) -> Option<usize> {
    let mut offset = 12; // Skip RIFF header (4 + 4 + 4)

    while offset + 8 <= data.len() {
        if data.get(offset..offset + 4) == Some(id.as_slice()) {
            return Some(offset);
        }

        // Chunk size is at offset+4, little-endian u32.
        let chunk_size = read_u32_le(data, offset + 4) as usize;
        // Chunks are word-aligned (padded to even size).
        let padded = (chunk_size + 1) & !1;
        offset += 8 + padded;
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal valid WAV file with the given parameters and PCM data length.
    fn build_wav(sample_rate: u32, bits_per_sample: u16, channels: u16, pcm_len: u32) -> Vec<u8> {
        let mut buf = Vec::new();

        // RIFF header
        buf.extend_from_slice(b"RIFF");
        let file_size = 36 + pcm_len;
        buf.extend_from_slice(&file_size.to_le_bytes());
        buf.extend_from_slice(b"WAVE");

        // fmt chunk
        buf.extend_from_slice(b"fmt ");
        buf.extend_from_slice(&16u32.to_le_bytes()); // chunk size
        buf.extend_from_slice(&WAV_FORMAT_PCM.to_le_bytes()); // audio format
        buf.extend_from_slice(&channels.to_le_bytes());
        buf.extend_from_slice(&sample_rate.to_le_bytes());
        let byte_rate = sample_rate * u32::from(channels) * u32::from(bits_per_sample) / 8;
        buf.extend_from_slice(&byte_rate.to_le_bytes());
        let block_align = channels * bits_per_sample / 8;
        buf.extend_from_slice(&block_align.to_le_bytes());
        buf.extend_from_slice(&bits_per_sample.to_le_bytes());

        // data chunk
        buf.extend_from_slice(b"data");
        buf.extend_from_slice(&pcm_len.to_le_bytes());

        // Append zero-filled PCM data (enough for header parsing).
        let fill = pcm_len.min(256);
        buf.resize(buf.len() + fill as usize, 0);

        buf
    }

    type TestResult = Result<(), Box<dyn std::error::Error>>;
    type BoxErr = Box<dyn std::error::Error>;

    fn write_slice(image: &mut [u8], offset: usize, data: &[u8]) -> Result<(), BoxErr> {
        let end = offset + data.len();
        let img_len = image.len();
        image
            .get_mut(offset..end)
            .ok_or_else(|| {
                format!("write_slice: range {offset}..{end} out of bounds (len={img_len})")
            })?
            .copy_from_slice(data);
        Ok(())
    }

    #[test]
    fn parse_valid_d75_wav() -> TestResult {
        // 1 second of 16 kHz / 16-bit / mono = 32000 bytes
        let pcm_len: u32 = 32_000;
        let wav = build_wav(16_000, 16, 1, pcm_len);
        let rec = parse(&wav)?;

        assert_eq!(rec.sample_rate, 16_000);
        assert_eq!(rec.bits_per_sample, 16);
        assert_eq!(rec.channels, 1);
        assert_eq!(rec.data_length, pcm_len);
        assert!((rec.duration_secs - 1.0).abs() < 0.001);
        Ok(())
    }

    #[test]
    fn parse_duration_calculation() -> TestResult {
        // 5 minutes = 300 seconds → 300 * 32000 = 9_600_000 bytes
        let pcm_len: u32 = 9_600_000;
        let wav = build_wav(16_000, 16, 1, pcm_len);
        let rec = parse(&wav)?;

        assert!((rec.duration_secs - 300.0).abs() < 0.001);
        Ok(())
    }

    #[test]
    fn too_short_returns_error() -> TestResult {
        let data = b"RIFF";
        let err = parse(data)
            .err()
            .ok_or("expected FileTooSmall but got Ok")?;
        assert!(
            matches!(err, SdCardError::FileTooSmall { .. }),
            "expected FileTooSmall, got {err:?}"
        );
        Ok(())
    }

    #[test]
    fn empty_returns_error() -> TestResult {
        let err = parse(b"").err().ok_or("expected FileTooSmall but got Ok")?;
        assert!(
            matches!(err, SdCardError::FileTooSmall { .. }),
            "expected FileTooSmall, got {err:?}"
        );
        Ok(())
    }

    #[test]
    fn wrong_riff_magic() -> TestResult {
        let mut wav = build_wav(16_000, 16, 1, 32_000);
        write_slice(&mut wav, 0, b"XXXX")?;
        let err = parse(&wav)
            .err()
            .ok_or("expected InvalidWavHeader but got Ok")?;
        assert!(
            matches!(err, SdCardError::InvalidWavHeader { .. }),
            "expected InvalidWavHeader, got {err:?}"
        );
        Ok(())
    }

    #[test]
    fn wrong_wave_format() -> TestResult {
        let mut wav = build_wav(16_000, 16, 1, 32_000);
        write_slice(&mut wav, 8, b"AVI ")?;
        let err = parse(&wav)
            .err()
            .ok_or("expected InvalidWavHeader but got Ok")?;
        assert!(
            matches!(err, SdCardError::InvalidWavHeader { .. }),
            "expected InvalidWavHeader, got {err:?}"
        );
        Ok(())
    }

    #[test]
    fn non_pcm_format_rejected() -> TestResult {
        let mut wav = build_wav(16_000, 16, 1, 32_000);
        // Set audio format to 3 (IEEE float) at fmt+8 = offset 20
        write_slice(&mut wav, 20, &3u16.to_le_bytes())?;
        let err = parse(&wav)
            .err()
            .ok_or("expected InvalidWavHeader but got Ok")?;
        assert!(
            matches!(err, SdCardError::InvalidWavHeader { .. }),
            "expected InvalidWavHeader, got {err:?}"
        );
        Ok(())
    }

    #[test]
    fn wrong_sample_rate_rejected() -> TestResult {
        let wav = build_wav(44_100, 16, 1, 88_200);
        let err = parse(&wav)
            .err()
            .ok_or("expected UnexpectedAudioFormat but got Ok")?;
        assert!(
            matches!(err, SdCardError::UnexpectedAudioFormat { .. }),
            "expected UnexpectedAudioFormat, got {err:?}"
        );
        Ok(())
    }

    #[test]
    fn wrong_bit_depth_rejected() -> TestResult {
        let wav = build_wav(16_000, 8, 1, 16_000);
        let err = parse(&wav)
            .err()
            .ok_or("expected UnexpectedAudioFormat but got Ok")?;
        assert!(
            matches!(err, SdCardError::UnexpectedAudioFormat { .. }),
            "expected UnexpectedAudioFormat, got {err:?}"
        );
        Ok(())
    }

    #[test]
    fn stereo_rejected() -> TestResult {
        let wav = build_wav(16_000, 16, 2, 64_000);
        let err = parse(&wav)
            .err()
            .ok_or("expected UnexpectedAudioFormat but got Ok")?;
        assert!(
            matches!(err, SdCardError::UnexpectedAudioFormat { .. }),
            "expected UnexpectedAudioFormat, got {err:?}"
        );
        Ok(())
    }
}
