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
//! `/KENWOOD/TH-D75/REC/*.wav`, maximum 255 files per directory.
//!
//! # Details
//!
//! This parser validates the RIFF/WAV header and extracts metadata
//! (sample rate, bit depth, channels, data length, duration).
//! It does **not** decode PCM sample data.

use std::time::Duration;

use super::{SdCardError, read_u16_le, read_u32_le};

/// TH-D75 recording sample rate in hertz.
pub const AUDIO_SAMPLE_RATE_HZ: u32 = 16_000;

/// TH-D75 recording bit depth.
pub const AUDIO_BITS_PER_SAMPLE: u16 = 16;

/// TH-D75 recording channel count (mono).
pub const AUDIO_CHANNELS: u16 = 1;

/// Bytes in one complete mono 16-bit PCM sample frame.
const AUDIO_BYTES_PER_SAMPLE_FRAME: u32 = 2;

/// Nanoseconds represented by one 16 kHz sample frame.
const AUDIO_NANOSECONDS_PER_SAMPLE_FRAME: u64 = 62_500;

/// WAV audio format code for PCM.
const WAV_FORMAT_PCM: u16 = 1;

/// Minimum WAV file size: 44 bytes (RIFF header + fmt chunk + data chunk header).
const MIN_WAV_SIZE: usize = 44;

/// Validated metadata from a TH-D75 audio recording WAV file.
///
/// The sample rate, bit depth, and channel count are invariants of this type.
/// PCM sample data is not loaded or decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AudioRecordingInfo {
    data_length_bytes: u32,
}

impl AudioRecordingInfo {
    /// Return the fixed TH-D75 recording sample rate in hertz.
    #[must_use]
    pub const fn sample_rate_hz(self) -> u32 {
        AUDIO_SAMPLE_RATE_HZ
    }

    /// Return the fixed TH-D75 recording bit depth.
    #[must_use]
    pub const fn bits_per_sample(self) -> u16 {
        AUDIO_BITS_PER_SAMPLE
    }

    /// Return the fixed TH-D75 recording channel count.
    #[must_use]
    pub const fn channels(self) -> u16 {
        AUDIO_CHANNELS
    }

    /// Return the raw PCM data-chunk length in bytes.
    #[must_use]
    pub const fn data_length_bytes(self) -> u32 {
        self.data_length_bytes
    }

    /// Return the number of complete mono PCM sample frames.
    #[must_use]
    pub const fn sample_frames(self) -> u32 {
        self.data_length_bytes / AUDIO_BYTES_PER_SAMPLE_FRAME
    }

    /// Return the exact recording duration represented by the sample frames.
    #[must_use]
    pub fn duration(self) -> Duration {
        Duration::from_nanos(u64::from(self.sample_frames()) * AUDIO_NANOSECONDS_PER_SAMPLE_FRAME)
    }

    /// Return the recording duration in fractional seconds.
    #[must_use]
    pub fn duration_secs_f64(self) -> f64 {
        self.duration().as_secs_f64()
    }
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
/// Returns [`SdCardError::InvalidWavHeader`] if the RIFF size does not match
/// the input, a chunk header, payload, or padding byte is incomplete, required
/// chunks are missing or duplicated, the `fmt` PCM metadata is internally
/// inconsistent, or the `data` payload ends between sample frames.
///
/// Returns [`SdCardError::UnexpectedAudioFormat`] if the sample rate,
/// bit depth, or channel count does not match the expected TH-D75
/// format.
pub fn parse(data: &[u8]) -> Result<AudioRecordingInfo, SdCardError> {
    let (fmt_chunk, data_chunk) = parse_required_chunks(data)?;
    let format = PcmFormat::parse(fmt_chunk)?;
    format.validate_data_chunk(data_chunk)?;

    Ok(AudioRecordingInfo {
        data_length_bytes: data_chunk.size,
    })
}

fn parse_required_chunks(data: &[u8]) -> Result<(RiffChunk<'_>, RiffChunk<'_>), SdCardError> {
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

    let riff_size = usize::try_from(read_u32_le(data, 4)?).map_err(|_| {
        invalid_wav("declared RIFF size does not fit this platform's address space")
    })?;
    let declared_file_size = riff_size
        .checked_add(8)
        .ok_or_else(|| invalid_wav("declared RIFF file size overflows address space"))?;
    if declared_file_size != data.len() {
        return Err(invalid_wav(format!(
            "RIFF size field declares {declared_file_size} total bytes, but input contains {}",
            data.len()
        )));
    }

    let mut fmt_chunk = None;
    let mut data_chunk = None;
    for chunk in RiffChunks::new(data, declared_file_size) {
        let chunk = chunk?;
        match chunk.id {
            id if id == *b"fmt " => {
                if fmt_chunk.replace(chunk).is_some() {
                    return Err(invalid_wav(format!(
                        "duplicate fmt chunk at offset {}",
                        chunk.offset
                    )));
                }
            }
            id if id == *b"data" => {
                if data_chunk.replace(chunk).is_some() {
                    return Err(invalid_wav(format!(
                        "duplicate data chunk at offset {}",
                        chunk.offset
                    )));
                }
            }
            _ => {}
        }
    }

    Ok((
        fmt_chunk.ok_or_else(|| invalid_wav("fmt chunk not found"))?,
        data_chunk.ok_or_else(|| invalid_wav("data chunk not found"))?,
    ))
}

fn invalid_wav(detail: impl Into<String>) -> SdCardError {
    SdCardError::InvalidWavHeader {
        detail: detail.into(),
    }
}

/// One completely bounded RIFF chunk.
#[derive(Debug, Clone, Copy)]
struct RiffChunk<'a> {
    id: [u8; 4],
    offset: usize,
    size: u32,
    payload: &'a [u8],
}

/// Validated PCM metadata from the `fmt` chunk.
#[derive(Debug, Clone, Copy)]
struct PcmFormat {
    sample_rate: u32,
    byte_rate: u32,
    block_align: u16,
    bits_per_sample: u16,
    channels: u16,
}

impl PcmFormat {
    fn parse(fmt_chunk: RiffChunk<'_>) -> Result<Self, SdCardError> {
        if fmt_chunk.payload.len() < 16 {
            return Err(invalid_wav(format!(
                "fmt chunk at offset {} has a {}-byte payload; PCM requires at least 16 bytes",
                fmt_chunk.offset,
                fmt_chunk.payload.len()
            )));
        }

        let audio_format = read_u16_le(fmt_chunk.payload, 0)?;
        if audio_format != WAV_FORMAT_PCM {
            return Err(invalid_wav(format!(
                "unsupported audio format code {audio_format} \
                 (expected {WAV_FORMAT_PCM} for PCM)"
            )));
        }

        let format = Self {
            channels: read_u16_le(fmt_chunk.payload, 2)?,
            sample_rate: read_u32_le(fmt_chunk.payload, 4)?,
            byte_rate: read_u32_le(fmt_chunk.payload, 8)?,
            block_align: read_u16_le(fmt_chunk.payload, 12)?,
            bits_per_sample: read_u16_le(fmt_chunk.payload, 14)?,
        };
        format.validate_radio_format()?;
        format.validate_internal_consistency()?;
        Ok(format)
    }

    const fn validate_radio_format(self) -> Result<(), SdCardError> {
        if self.sample_rate != AUDIO_SAMPLE_RATE_HZ
            || self.bits_per_sample != AUDIO_BITS_PER_SAMPLE
            || self.channels != AUDIO_CHANNELS
        {
            return Err(SdCardError::UnexpectedAudioFormat {
                sample_rate: self.sample_rate,
                bits_per_sample: self.bits_per_sample,
                channels: self.channels,
            });
        }
        Ok(())
    }

    fn validate_internal_consistency(self) -> Result<(), SdCardError> {
        let bits_per_frame = u32::from(self.channels)
            .checked_mul(u32::from(self.bits_per_sample))
            .ok_or_else(|| invalid_wav("PCM bits per sample frame overflow u32"))?;
        if !bits_per_frame.is_multiple_of(8) {
            return Err(invalid_wav(format!(
                "channels ({}) * bits per sample ({}) is not byte-aligned",
                self.channels, self.bits_per_sample
            )));
        }
        let expected_block_align = u16::try_from(bits_per_frame / 8)
            .map_err(|_| invalid_wav("PCM block alignment does not fit u16"))?;
        if self.block_align != expected_block_align {
            return Err(invalid_wav(format!(
                "fmt block alignment is {}, expected {expected_block_align} for {} channel(s) \
                 at {} bits",
                self.block_align, self.channels, self.bits_per_sample
            )));
        }

        let expected_byte_rate = self
            .sample_rate
            .checked_mul(u32::from(self.block_align))
            .ok_or_else(|| invalid_wav("PCM byte rate overflows u32"))?;
        if self.byte_rate != expected_byte_rate {
            return Err(invalid_wav(format!(
                "fmt byte rate is {}, expected {expected_byte_rate} \
                 ({} Hz * {}-byte block alignment)",
                self.byte_rate, self.sample_rate, self.block_align
            )));
        }
        Ok(())
    }

    fn validate_data_chunk(self, data_chunk: RiffChunk<'_>) -> Result<(), SdCardError> {
        if !data_chunk
            .payload
            .len()
            .is_multiple_of(usize::from(self.block_align))
        {
            return Err(invalid_wav(format!(
                "data chunk length {} is not divisible by the {}-byte block alignment",
                data_chunk.payload.len(),
                self.block_align
            )));
        }
        Ok(())
    }
}

/// Checked iterator over chunks inside a declared RIFF container.
struct RiffChunks<'a> {
    data: &'a [u8],
    next_offset: usize,
    riff_end: usize,
    failed: bool,
}

impl<'a> RiffChunks<'a> {
    const fn new(data: &'a [u8], riff_end: usize) -> Self {
        Self {
            data,
            next_offset: 12,
            riff_end,
            failed: false,
        }
    }

    fn next_chunk(&mut self) -> Result<Option<RiffChunk<'a>>, SdCardError> {
        if self.next_offset == self.riff_end {
            return Ok(None);
        }

        let remaining = self.riff_end.saturating_sub(self.next_offset);
        if remaining < 8 {
            return Err(invalid_wav(format!(
                "RIFF chunk header at offset {} is truncated: expected 8 bytes, found {remaining}",
                self.next_offset
            )));
        }

        let header_end = self
            .next_offset
            .checked_add(8)
            .ok_or_else(|| invalid_wav("RIFF chunk header offset overflows address space"))?;
        let header = self
            .data
            .get(self.next_offset..header_end)
            .ok_or_else(|| invalid_wav("RIFF chunk header exceeds the input buffer"))?;
        let (id, size_bytes) = header
            .split_first_chunk::<4>()
            .unwrap_or_else(|| unreachable!("a checked RIFF chunk header contains an ID"));
        let size_bytes = size_bytes
            .first_chunk::<4>()
            .copied()
            .unwrap_or_else(|| unreachable!("a checked RIFF chunk header contains a size"));
        let size = u32::from_le_bytes(size_bytes);
        let payload_size = usize::try_from(size)
            .map_err(|_| invalid_wav("RIFF chunk payload size does not fit this platform"))?;
        let payload_end = header_end
            .checked_add(payload_size)
            .ok_or_else(|| invalid_wav("RIFF chunk payload end overflows address space"))?;
        if payload_end > self.riff_end {
            return Err(invalid_wav(format!(
                "RIFF chunk {id:02X?} at offset {} declares {size} payload bytes ending at \
                 {payload_end}, beyond RIFF end {}",
                self.next_offset, self.riff_end
            )));
        }

        let padding = usize::from(!size.is_multiple_of(2));
        let next_offset = payload_end
            .checked_add(padding)
            .ok_or_else(|| invalid_wav("RIFF chunk padding end overflows address space"))?;
        if next_offset > self.riff_end {
            return Err(invalid_wav(format!(
                "RIFF chunk {id:02X?} at offset {} has odd payload length {size}, but its \
                 required padding byte is missing",
                self.next_offset
            )));
        }

        let payload = self
            .data
            .get(header_end..payload_end)
            .ok_or_else(|| invalid_wav("RIFF chunk payload exceeds the input buffer"))?;
        let chunk = RiffChunk {
            id: *id,
            offset: self.next_offset,
            size,
            payload,
        };
        self.next_offset = next_offset;
        Ok(Some(chunk))
    }
}

impl<'a> Iterator for RiffChunks<'a> {
    type Item = Result<RiffChunk<'a>, SdCardError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed {
            return None;
        }
        match self.next_chunk() {
            Ok(Some(chunk)) => Some(Ok(chunk)),
            Ok(None) => None,
            Err(error) => {
                self.failed = true;
                Some(Err(error))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;
    type BoxErr = Box<dyn std::error::Error>;

    /// Build a minimal valid WAV file with the given parameters and PCM data length.
    fn build_wav(
        sample_rate: u32,
        bits_per_sample: u16,
        channels: u16,
        pcm_len: u32,
    ) -> Result<Vec<u8>, BoxErr> {
        let mut buf = Vec::new();

        // RIFF header
        buf.extend_from_slice(b"RIFF");
        let padding = pcm_len % 2;
        let riff_size = 36u32
            .checked_add(pcm_len)
            .and_then(|size| size.checked_add(padding))
            .ok_or("test WAV size overflowed u32")?;
        buf.extend_from_slice(&riff_size.to_le_bytes());
        buf.extend_from_slice(b"WAVE");

        // fmt chunk
        buf.extend_from_slice(b"fmt ");
        buf.extend_from_slice(&16u32.to_le_bytes()); // chunk size
        buf.extend_from_slice(&WAV_FORMAT_PCM.to_le_bytes()); // audio format
        buf.extend_from_slice(&channels.to_le_bytes());
        buf.extend_from_slice(&sample_rate.to_le_bytes());
        let bits_per_frame = u32::from(channels)
            .checked_mul(u32::from(bits_per_sample))
            .ok_or("test WAV bits per frame overflowed u32")?;
        let block_align = u16::try_from(bits_per_frame / 8)?;
        let byte_rate = sample_rate
            .checked_mul(u32::from(block_align))
            .ok_or("test WAV byte rate overflowed u32")?;
        buf.extend_from_slice(&byte_rate.to_le_bytes());
        buf.extend_from_slice(&block_align.to_le_bytes());
        buf.extend_from_slice(&bits_per_sample.to_le_bytes());

        // data chunk
        buf.extend_from_slice(b"data");
        buf.extend_from_slice(&pcm_len.to_le_bytes());

        // Append the complete declared PCM payload and any RIFF word padding.
        let payload_and_padding = usize::try_from(
            pcm_len
                .checked_add(padding)
                .ok_or("test WAV payload plus padding overflowed u32")?,
        )?;
        let final_size = buf
            .len()
            .checked_add(payload_and_padding)
            .ok_or("test WAV allocation size overflowed usize")?;
        buf.resize(final_size, 0);

        Ok(buf)
    }

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

    fn set_declared_riff_size_to_actual(wav: &mut [u8]) -> Result<(), BoxErr> {
        let riff_size = wav
            .len()
            .checked_sub(8)
            .ok_or("test WAV is shorter than its RIFF prefix")?;
        let riff_size = u32::try_from(riff_size)?;
        write_slice(wav, 4, &riff_size.to_le_bytes())
    }

    fn assert_invalid_wav_contains(wav: &[u8], expected: &str) -> TestResult {
        match parse(wav) {
            Err(SdCardError::InvalidWavHeader { detail }) => {
                assert!(
                    detail.contains(expected),
                    "expected WAV error containing {expected:?}, got {detail:?}"
                );
                Ok(())
            }
            Err(other) => Err(format!("expected InvalidWavHeader, got {other:?}").into()),
            Ok(recording) => Err(format!("invalid WAV was accepted: {recording:?}").into()),
        }
    }

    #[test]
    fn parse_valid_d75_wav() -> TestResult {
        // 1 second of 16 kHz / 16-bit / mono = 32000 bytes
        let pcm_len: u32 = 32_000;
        let wav = build_wav(16_000, 16, 1, pcm_len)?;
        let rec = parse(&wav)?;

        assert_eq!(rec.sample_rate_hz(), 16_000);
        assert_eq!(rec.bits_per_sample(), 16);
        assert_eq!(rec.channels(), 1);
        assert_eq!(rec.data_length_bytes(), pcm_len);
        assert!(
            (rec.duration_secs_f64() - 1.0).abs() < 0.001,
            "one second of PCM produced duration {}",
            rec.duration_secs_f64()
        );
        Ok(())
    }

    #[test]
    fn parse_duration_calculation() -> TestResult {
        // 5 minutes = 300 seconds → 300 * 32000 = 9_600_000 bytes
        let pcm_len: u32 = 9_600_000;
        let wav = build_wav(16_000, 16, 1, pcm_len)?;
        let rec = parse(&wav)?;

        assert!(
            (rec.duration_secs_f64() - 300.0).abs() < 0.001,
            "five minutes of PCM produced duration {}",
            rec.duration_secs_f64()
        );
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
        let mut wav = build_wav(16_000, 16, 1, 32_000)?;
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
        let mut wav = build_wav(16_000, 16, 1, 32_000)?;
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
        let mut wav = build_wav(16_000, 16, 1, 32_000)?;
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
        let wav = build_wav(44_100, 16, 1, 88_200)?;
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
        let wav = build_wav(16_000, 8, 1, 16_000)?;
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
        let wav = build_wav(16_000, 16, 2, 64_000)?;
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
    fn declared_riff_size_must_equal_input_size() -> TestResult {
        let mut wav = build_wav(16_000, 16, 1, 4)?;
        wav.push(0);
        assert_invalid_wav_contains(
            &wav,
            "RIFF size field declares 48 total bytes, but input contains 49",
        )
    }

    #[test]
    fn truncated_chunk_header_is_rejected() -> TestResult {
        let mut wav = build_wav(16_000, 16, 1, 0)?;
        wav.extend_from_slice(b"JUNK");
        set_declared_riff_size_to_actual(&mut wav)?;
        assert_invalid_wav_contains(
            &wav,
            "RIFF chunk header at offset 44 is truncated: expected 8 bytes, found 4",
        )
    }

    #[test]
    fn truncated_data_payload_is_rejected() -> TestResult {
        let mut wav = build_wav(16_000, 16, 1, 4)?;
        let _removed = wav.pop().ok_or("test WAV had no byte to truncate")?;
        set_declared_riff_size_to_actual(&mut wav)?;
        assert_invalid_wav_contains(
            &wav,
            "declares 4 payload bytes ending at 48, beyond RIFF end 47",
        )
    }

    #[test]
    fn odd_chunk_requires_its_padding_byte() -> TestResult {
        let mut wav = build_wav(16_000, 16, 1, 0)?;
        wav.extend_from_slice(b"JUNK");
        wav.extend_from_slice(&1u32.to_le_bytes());
        wav.push(0xA5);
        set_declared_riff_size_to_actual(&mut wav)?;
        assert_invalid_wav_contains(&wav, "required padding byte is missing")
    }

    #[test]
    fn complete_odd_unknown_chunk_is_accepted() -> TestResult {
        let mut wav = build_wav(16_000, 16, 1, 4)?;
        wav.extend_from_slice(b"JUNK");
        wav.extend_from_slice(&1u32.to_le_bytes());
        wav.extend_from_slice(&[0xA5, 0x00]);
        set_declared_riff_size_to_actual(&mut wav)?;

        let recording = parse(&wav)?;
        assert_eq!(recording.data_length_bytes(), 4);
        Ok(())
    }

    #[test]
    fn fmt_payload_must_contain_complete_pcm_metadata() -> TestResult {
        let mut wav = build_wav(16_000, 16, 1, 0)?;
        write_slice(&mut wav, 16, &15u32.to_le_bytes())?;
        assert_invalid_wav_contains(&wav, "15-byte payload; PCM requires at least 16 bytes")
    }

    #[test]
    fn block_alignment_must_match_channel_and_sample_width() -> TestResult {
        let mut wav = build_wav(16_000, 16, 1, 4)?;
        write_slice(&mut wav, 32, &4u16.to_le_bytes())?;
        assert_invalid_wav_contains(&wav, "fmt block alignment is 4, expected 2")
    }

    #[test]
    fn byte_rate_must_match_sample_rate_and_block_alignment() -> TestResult {
        let mut wav = build_wav(16_000, 16, 1, 4)?;
        write_slice(&mut wav, 28, &31_999u32.to_le_bytes())?;
        assert_invalid_wav_contains(&wav, "fmt byte rate is 31999, expected 32000")
    }

    #[test]
    fn data_length_must_contain_whole_sample_frames() -> TestResult {
        let mut wav = build_wav(16_000, 16, 1, 4)?;
        write_slice(&mut wav, 40, &3u32.to_le_bytes())?;
        assert_invalid_wav_contains(
            &wav,
            "data chunk length 3 is not divisible by the 2-byte block alignment",
        )
    }

    #[test]
    fn forged_chunk_size_cannot_escape_riff_container() -> TestResult {
        let mut wav = build_wav(16_000, 16, 1, 4)?;
        write_slice(&mut wav, 40, &u32::MAX.to_le_bytes())?;
        assert_invalid_wav_contains(&wav, "beyond RIFF end 48")
    }

    #[test]
    fn duplicate_fmt_chunk_is_rejected() -> TestResult {
        let mut wav = build_wav(16_000, 16, 1, 0)?;
        let duplicate = wav
            .get(12..36)
            .ok_or("test WAV fmt chunk missing")?
            .to_vec();
        wav.extend_from_slice(&duplicate);
        set_declared_riff_size_to_actual(&mut wav)?;
        assert_invalid_wav_contains(&wav, "duplicate fmt chunk at offset 44")
    }
}
