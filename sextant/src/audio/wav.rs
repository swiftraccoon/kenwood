// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

//! Incremental file playback with bounded decoding and resampling buffers.

use std::collections::VecDeque;
use std::error::Error;
use std::fmt;
use std::fs::File;
use std::io::{self, BufReader, Read};
use std::num::NonZeroU32;
use std::path::Path;
use std::time::Duration;

use rubato::Resampler as _;

use super::Resampler;

/// Maximum file size accepted by this application's playback paths.
const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;
/// Maximum playback duration accepted by this application, in seconds.
const MAX_DURATION_SECONDS: u64 = 300;
/// Mono input samples decoded in one bounded refill.
const INPUT_CHUNK: usize = 1024;

/// Remaining and total decoded file audio, measured at its output sample rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FileProgress {
    remaining: Duration,
    total: Duration,
}

impl FileProgress {
    pub(crate) const fn remaining(self) -> Duration {
        self.remaining
    }
    pub(crate) const fn total(self) -> Duration {
        self.total
    }
}

fn sample_duration(samples: u64, rate: NonZeroU32) -> Duration {
    let rate = u64::from(rate.get());
    Duration::from_secs(samples / rate)
        + Duration::from_nanos((samples % rate) * 1_000_000_000 / rate)
}

/// File decoding or resampling failure, retained until presentation.
#[derive(Debug)]
pub(crate) enum WavError {
    /// Opening or inspecting the input failed.
    Io(io::Error),
    /// The file exceeds the application's byte or duration limit.
    Limit,
    /// Only 16-bit integer PCM, 1..=8 channels and 8..=192 kHz are supported.
    Format(hound::WavSpec),
    /// The interleaved data ends inside a channel frame.
    PartialFrame,
    /// The WAV decoder rejected a header or sample.
    Decode(hound::Error),
    /// The sinc resampler could not be constructed.
    Resampler(rubato::ResamplerConstructionError),
    /// The sinc resampler rejected an input chunk.
    Resample(rubato::ResampleError),
}

impl fmt::Display for WavError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "read WAV: {error}"),
            Self::Limit => f.write_str("WAV exceeds 64 MiB or five minutes"),
            Self::Format(spec) => write!(f, "unsupported WAV format: {spec:?}"),
            Self::PartialFrame => f.write_str("WAV ends inside an interleaved channel frame"),
            Self::Decode(error) => write!(f, "decode WAV: {error}"),
            Self::Resampler(error) => write!(f, "construct WAV resampler: {error}"),
            Self::Resample(error) => write!(f, "resample WAV: {error}"),
        }
    }
}

impl Error for WavError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Decode(error) => Some(error),
            Self::Resampler(error) => Some(error),
            Self::Resample(error) => Some(error),
            Self::Limit | Self::Format(_) | Self::PartialFrame => None,
        }
    }
}

/// A bounded input decoder and one resampler chunk of pending mono output.
///
/// Output has `floor(input_frames * output_rate / input_rate)` samples.
/// Sinc startup delay is removed and the final filter tail is zero-padded.
pub(super) struct WavStream<R: Read = BufReader<File>> {
    samples: hound::WavIntoSamples<R, i16>,
    channels: u16,
    input_remaining: u32,
    output_remaining: u64,
    output_total: u64,
    output_rate: NonZeroU32,
    resampler: Option<Resampler>,
    startup_delay: usize,
    pending: VecDeque<f32>,
}

impl WavStream {
    /// Open a supported file without loading its sample payload.
    pub(super) fn open(path: &Path, output_rate: u32) -> Result<Self, WavError> {
        let file = File::open(path).map_err(WavError::Io)?;
        if file.metadata().map_err(WavError::Io)?.len() > MAX_FILE_BYTES {
            return Err(WavError::Limit);
        }
        Self::from_reader(
            hound::WavReader::new(BufReader::new(file)).map_err(WavError::Decode)?,
            output_rate,
        )
    }
}

impl<R: Read> WavStream<R> {
    fn from_reader(reader: hound::WavReader<R>, output_rate: u32) -> Result<Self, WavError> {
        let spec = reader.spec();
        if spec.sample_format != hound::SampleFormat::Int
            || spec.bits_per_sample != 16
            || !(1..=8).contains(&spec.channels)
            || !(8_000..=192_000).contains(&spec.sample_rate)
            || !(8_000..=192_000).contains(&output_rate)
        {
            return Err(WavError::Format(spec));
        }
        if !reader.len().is_multiple_of(u32::from(spec.channels)) {
            return Err(WavError::PartialFrame);
        }
        let duration = reader.duration();
        if u64::from(duration) > MAX_DURATION_SECONDS * u64::from(spec.sample_rate) {
            return Err(WavError::Limit);
        }
        let resampler = if spec.sample_rate == output_rate {
            None
        } else {
            Some(
                Resampler::new(spec.sample_rate, output_rate, INPUT_CHUNK)
                    .map_err(WavError::Resampler)?,
            )
        };
        let startup_delay = resampler.as_ref().map_or(0, |rs| rs.inner.output_delay());
        let output_total =
            u64::from(duration) * u64::from(output_rate) / u64::from(spec.sample_rate);
        Ok(Self {
            samples: reader.into_samples(),
            channels: spec.channels,
            input_remaining: duration,
            output_remaining: output_total,
            output_total,
            output_rate: NonZeroU32::new(output_rate).ok_or(WavError::Format(spec))?,
            resampler,
            startup_delay,
            pending: VecDeque::new(),
        })
    }

    pub(super) fn progress(&self) -> FileProgress {
        FileProgress {
            remaining: sample_duration(self.output_remaining, self.output_rate),
            total: sample_duration(self.output_total, self.output_rate),
        }
    }

    /// Decode at most one requested 20 ms output frame per call.
    ///
    /// `count` is bounded by the supported maximum rate's 20 ms frame.
    /// Returns a short final frame, then an empty vector. Dropping this value
    /// cancels playback without decoding the remainder of the file.
    pub(super) fn next_samples(&mut self, count: usize) -> Result<Vec<f32>, WavError> {
        let count = count.min(192_000 / 50);
        let mut output = Vec::with_capacity(count);
        while output.len() < count && self.output_remaining > 0 {
            if let Some(sample) = self.pending.pop_front() {
                output.push(sample);
                self.output_remaining -= 1;
            } else {
                self.refill()?;
            }
        }
        Ok(output)
    }

    /// Decode at most 1024 complete input frames and retain one output chunk.
    fn refill(&mut self) -> Result<(), WavError> {
        let mut input = [0.0; INPUT_CHUNK];
        for sample in &mut input {
            if self.input_remaining == 0 {
                break;
            }
            let mut sum = 0.0;
            for _ in 0..self.channels {
                let value = self.samples.next().ok_or(WavError::PartialFrame)?;
                sum += f32::from(value.map_err(WavError::Decode)?) / 32_768.0;
            }
            *sample = sum / f32::from(self.channels);
            self.input_remaining -= 1;
        }
        if let Some(resampler) = self.resampler.as_mut() {
            let output = resampler.process(&input).map_err(WavError::Resample)?;
            let skip = self.startup_delay.min(output.len());
            self.startup_delay -= skip;
            self.pending.extend(output.into_iter().skip(skip));
        } else {
            self.pending.extend(input);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::{WavError, WavStream};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn pcm(samples: &[i16], channels: u16, rate: u32) -> Result<Vec<u8>, hound::Error> {
        let mut bytes = Vec::new();
        let spec = hound::WavSpec {
            channels,
            sample_rate: rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::new(Cursor::new(&mut bytes), spec)?;
        for &sample in samples {
            writer.write_sample(sample)?;
        }
        writer.finalize()?;
        Ok(bytes)
    }

    #[test]
    fn bounded_stream_preserves_duration_channels_and_final_tail() -> TestResult {
        let source: Vec<i16> = (0..48_000).flat_map(|_| [8192, 16_384]).collect();
        let bytes = pcm(&source, 2, 48_000)?;
        let mut stream = WavStream::from_reader(hound::WavReader::new(Cursor::new(bytes))?, 8000)?;
        let mut output = Vec::new();
        loop {
            let frame = stream.next_samples(160)?;
            assert!(frame.len() <= 160, "one call exceeded one codec frame");
            assert!(
                stream.pending.len() < 1024,
                "decoder buffered the complete file"
            );
            if frame.is_empty() {
                break;
            }
            output.extend(frame);
        }
        assert_eq!(
            output.len(),
            8000,
            "resampling changed the logical duration"
        );
        let body = output.get(160..7840).ok_or("missing steady-state output")?;
        assert!(
            body.iter().all(|sample| (*sample - 0.375).abs() < 0.002),
            "stereo fold changed gain"
        );
        Ok(())
    }

    #[test]
    fn malformed_samples_and_unsupported_formats_are_errors() -> TestResult {
        let mut truncated = pcm(&[1; 320], 1, 8000)?;
        truncated.truncate(truncated.len() - 2);
        let mut stream =
            WavStream::from_reader(hound::WavReader::new(Cursor::new(truncated))?, 8000)?;
        assert!(
            matches!(stream.next_samples(160), Err(WavError::Decode(_))),
            "truncation was discarded"
        );
        let bytes = pcm(&[1; 320], 1, 4000)?;
        assert!(
            matches!(
                WavStream::from_reader(hound::WavReader::new(Cursor::new(bytes))?, 8000),
                Err(WavError::Format(_))
            ),
            "unsupported rate was accepted"
        );
        Ok(())
    }

    #[test]
    fn cancellation_leaves_unread_samples_and_empty_files_finish() -> TestResult {
        let bytes = pcm(&[1; 8000], 1, 8000)?;
        let mut stream = WavStream::from_reader(hound::WavReader::new(Cursor::new(bytes))?, 8000)?;
        assert_eq!(
            stream.progress().remaining(),
            std::time::Duration::from_secs(1),
            "initial progress changed duration"
        );
        assert_eq!(stream.next_samples(160)?.len(), 160, "first frame missing");
        assert_eq!(
            stream.progress().remaining(),
            std::time::Duration::from_millis(980),
            "progress did not advance by one frame"
        );
        assert_eq!(
            stream.input_remaining, 6976,
            "playback eagerly consumed the file"
        );
        drop(stream);
        let empty = pcm(&[], 1, 8000)?;
        let mut stream = WavStream::from_reader(hound::WavReader::new(Cursor::new(empty))?, 8000)?;
        assert!(
            stream.next_samples(160)?.is_empty(),
            "empty WAV produced padding"
        );
        Ok(())
    }
}
