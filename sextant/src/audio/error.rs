// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

//! Typed failures carried from audio operations to the presentation boundary.

use std::error::Error;
use std::fmt;
use std::io;

use super::wav::WavError;

/// Audio-device direction named by an operation or failure.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Direction {
    Input,
    Output,
}

/// A device, format or stream-construction failure.
#[derive(Debug)]
pub(crate) enum DeviceError {
    Missing(Direction),
    NamedMissing { direction: Direction, name: String },
    Enumerate(cpal::DevicesError),
    Name(cpal::DeviceNameError),
    Configuration(cpal::DefaultStreamConfigError),
    Format(Direction, cpal::SampleFormat),
    Geometry { channels: u16, rate: u32 },
    Build(cpal::BuildStreamError),
    Play(cpal::PlayStreamError),
    Resampler(rubato::ResamplerConstructionError),
}

impl fmt::Display for DeviceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing(direction) => write!(f, "no {direction:?} audio device; select a device"),
            Self::NamedMissing { direction, name } => write!(
                f,
                "selected {direction:?} device {name} is unavailable; select another device"
            ),
            Self::Enumerate(error) => write!(f, "enumerate audio devices: {error}"),
            Self::Name(error) => write!(f, "read audio device name: {error}"),
            Self::Configuration(error) => write!(f, "read audio configuration: {error}"),
            Self::Format(direction, format) => {
                write!(f, "unsupported {direction:?} format {format:?}")
            }
            Self::Geometry { channels, rate } => write!(
                f,
                "unsupported audio configuration: {channels} channels at {rate} Hz"
            ),
            Self::Build(error) => write!(f, "build audio stream: {error}"),
            Self::Play(error) => write!(f, "start audio stream: {error}"),
            Self::Resampler(error) => write!(f, "build audio resampler: {error}"),
        }
    }
}

impl Error for DeviceError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Enumerate(error) => Some(error),
            Self::Name(error) => Some(error),
            Self::Configuration(error) => Some(error),
            Self::Build(error) => Some(error),
            Self::Play(error) => Some(error),
            Self::Resampler(error) => Some(error),
            Self::Missing(_)
            | Self::NamedMissing { .. }
            | Self::Format(..)
            | Self::Geometry { .. } => None,
        }
    }
}

/// An audio operation rejected or completed with a concrete failure.
#[derive(Debug)]
pub(crate) enum AudioError {
    /// The bounded worker queue cannot accept another operation.
    QueueFull,
    /// The worker has exited or shutdown has been requested.
    WorkerStopped,
    /// A transmission requires an established reflector connection.
    NotConnected,
    /// A preceding stream termination is still waiting for the session queue.
    TxEnding,
    /// The bounded session queue cannot accept a new transmission or frame.
    SessionBusy,
    /// A file/device operation requires an open output device.
    NoOutput,
    /// A live transmission already occupies the outgoing stream.
    AlreadyTransmitting,
    /// A decoded live audio block could not enter the speaker buffer.
    SpeakerOverflow(usize),
    /// Hardware capture samples exceeded the bounded microphone ring.
    CaptureOverflow(u64),
    /// The device backend reported a running-stream failure.
    Stream(cpal::StreamError),
    /// More errors arrived than the bounded presentation queue could retain.
    DiagnosticOverflow(u64),
    /// A file could not be opened or decoded.
    Wav(WavError),
    /// A live input chunk could not be resampled.
    Resample(rubato::ResampleError),
    /// Captured samples violate the encoder's finite unit-scale contract.
    Pcm(mbelib_rs::PcmError),
    /// A device stream could not be configured.
    Device(DeviceError),
    /// A recording sample or WAV finalization failed.
    Recording(hound::Error),
    /// Preparing or syncing a recording file failed.
    Io(io::Error),
    /// The platform has no application configuration directory.
    NoConfigurationDirectory,
    /// A transmitted DPRS source or report could not be encoded.
    Gps(dstar_gateway_core::dprs::DprsError),
}

impl fmt::Display for AudioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::QueueFull => f.write_str("audio queue full; retry the operation"),
            Self::WorkerStopped => f.write_str("audio worker stopped; restart the application"),
            Self::NotConnected => {
                f.write_str("reflector disconnected; connect before transmitting")
            }
            Self::TxEnding => f.write_str("transmission is ending; wait before starting another"),
            Self::SessionBusy => f.write_str("session queue full; outgoing transmission stopped"),
            Self::NoOutput => f.write_str("no output device; select an audio device"),
            Self::AlreadyTransmitting => f.write_str("transmission already active; stop it first"),
            Self::SpeakerOverflow(samples) => {
                write!(f, "speaker buffer full; {samples} samples lost")
            }
            Self::CaptureOverflow(samples) => {
                write!(f, "microphone buffer full; {samples} samples lost")
            }
            Self::Stream(error) => write!(f, "audio device stream failed: {error}"),
            Self::DiagnosticOverflow(count) => write!(
                f,
                "{count} earlier audio failures exceeded the diagnostic queue"
            ),
            Self::Wav(error) => error.fmt(f),
            Self::Resample(error) => write!(f, "resample live audio: {error}"),
            Self::Pcm(error) => write!(f, "captured audio rejected: {error}"),
            Self::Device(error) => error.fmt(f),
            Self::Recording(error) => write!(f, "recording failed: {error}"),
            Self::Io(error) => write!(f, "recording file operation failed: {error}"),
            Self::NoConfigurationDirectory => {
                f.write_str("configuration directory unavailable; recording cannot start")
            }
            Self::Gps(error) => write!(f, "GPS beacon rejected: {error}"),
        }
    }
}

impl Error for AudioError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Stream(error) => Some(error),
            Self::Wav(error) => Some(error),
            Self::Resample(error) => Some(error),
            Self::Pcm(error) => Some(error),
            Self::Device(error) => Some(error),
            Self::Recording(error) => Some(error),
            Self::Io(error) => Some(error),
            Self::Gps(error) => Some(error),
            _ => None,
        }
    }
}
