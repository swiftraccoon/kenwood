// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

//! Audio I/O — mic capture / speaker playback / AMBE codec plumbing.
//!
//! Runs on a dedicated `std::thread` so the tokio runtime never owns
//! a `cpal::Stream` (streams are `!Send` on some platforms). The
//! thread opens the default input and output devices at whatever
//! native config cpal reports, sets up lock-free ring buffers for
//! each direction, and processes audio in its main loop:
//!
//! - RX: pull incoming [`VoiceFrame`]s from the command channel,
//!   decode to 160-sample PCM, sinc-resample to HW rate, push to
//!   the speaker ringbuffer which the cpal output callback drains.
//! - TX: while PTT is active, drain 20 ms of HW-rate mic samples,
//!   sinc-resample to 8 kHz, feed through [`AmbeEncoder`], wrap the
//!   resulting 9-byte AMBE in a [`VoiceFrame`], and push into the
//!   session command channel via [`SessionCommand::TxFrame`].
//!
//! Sample-rate conversion uses `rubato`'s windowed-sinc resampler,
//! which bandlimits as it resamples — no separate anti-alias filter
//! is needed. The input / output devices and the recording /
//! playback paths are all driven from here.

use std::sync::mpsc as std_mpsc;
use std::thread::JoinHandle;
use std::time::Duration;

use cpal::SampleFormat;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use dstar_gateway_core::dprs::{DprsReport, Latitude, Longitude, encode_dprs};
use dstar_gateway_core::slowdata::{encode_text_message, scramble};
use dstar_gateway_core::types::Callsign;
use dstar_gateway_core::voice::{DSTAR_NULL_SLOW_DATA_BYTES, DSTAR_SYNC_BYTES, VoiceFrame};
use mbelib_rs::{AmbeDecoder, AmbeEncoder};
use ringbuf::HeapRb;
use ringbuf::traits::{Consumer, Observer, Producer, Split};
use rubato::{
    Resampler as _, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction,
};
use tokio::sync::mpsc as tokio_mpsc;
use tracing::{error, info, warn};

use crate::geo::TxPosition;
use crate::session::SessionCommand;

/// AMBE native sample rate.
const AMBE_SAMPLE_RATE: u32 = 8000;
/// Samples per AMBE frame (20 ms at 8 kHz).
const AMBE_FRAME_SAMPLES: usize = 160;

/// Frames of resampled RX audio accumulated before the first push to
/// the speaker ring buffer (~60 ms). The headroom rides through the
/// stream, absorbing network jitter that would otherwise underrun.
const RX_PRIME_FRAMES: usize = 3;

/// Samples of raised-cosine ramp at 8 kHz (10 ms) applied to each
/// stream's first frame (fade-in) and final frame (fade-out).
const RX_FADE_SAMPLES: usize = 80;

/// Handle the GUI (and session task) holds; forwards user intent
/// and RX frames to the audio worker thread.
///
/// Cloneable so the session task can route RX voice frames directly
/// to the worker — bypassing the egui redraw cadence, which would
/// otherwise add ~50 ms of jitter to every frame and cause the 50 fps
/// audio to arrive in bursts that underflow the speaker ring buffer.
#[derive(Debug, Clone)]
pub(crate) struct AudioHandle {
    cmd_tx: std_mpsc::Sender<AudioCommand>,
    _worker: std::sync::Arc<JoinHandle<()>>,
}

/// Commands emitted by the GUI and consumed by the audio worker.
#[derive(Debug)]
pub(crate) enum AudioCommand {
    /// Begin mic capture — worker also wires a `StartTx` through the
    /// session command channel so the session task emits the header
    /// before the first voice frame arrives.
    StartTx {
        /// Callsign to embed in `my_call`.
        my_call: String,
    },
    /// Stop mic capture and tell the session task to emit EOT.
    StopTx,
    /// A new RX voice stream is starting. The audio worker resets the
    /// [`AmbeDecoder`] — one decoder per voice stream — so stale
    /// synthesiser state from the prior stream doesn't leak into the
    /// first frames of the new one.
    RxStart,
    /// One voice frame arrived from the reflector — decode + play.
    RxFrame(VoiceFrame),
    /// A voice frame was lost upstream (UDP sequence gap) —
    /// synthesize one concealment frame so the hole plays as a
    /// parameter-repeat instead of a 20 ms silence gap.
    RxLost,
    /// The RX stream ended (EOT, inactivity, or link loss) — fade
    /// out and flush the held-back tail frame, then reset playback
    /// state for the next stream.
    RxEnd,
    /// Set the operator's slow-data text and/or GPS beacon. Either
    /// field may be `None`. Takes effect on the next TX frame.
    SetSlowData {
        /// Slow-data text message (≤20 chars; longer is truncated).
        text: Option<String>,
        /// Manual GPS position to beacon.
        gps: Option<TxPosition>,
    },
    /// Re-enumerate audio devices; the worker replies via
    /// `AudioStatus::DeviceList`.
    EnumerateDevices,
    /// Rebuild audio I/O on the named devices (`None` = host default).
    SelectDevices {
        /// Input device name.
        input: Option<String>,
        /// Output device name.
        output: Option<String>,
    },
    /// Start recording received audio to a WAV file.
    StartRecording,
    /// Stop the active recording.
    StopRecording,
    /// Play a WAV file to the speakers locally (not transmitted).
    PlayFile {
        /// Path to a WAV file.
        path: std::path::PathBuf,
    },
    /// Transmit a WAV file as an outgoing voice stream.
    TransmitFile {
        /// Path to a WAV file.
        path: std::path::PathBuf,
    },
}

/// Status pushed from the audio worker to the GUI. Drained every
/// frame by `App::drain_audio_status`.
#[derive(Debug, Clone)]
pub(crate) enum AudioStatus {
    /// Enumerated device names (input list, output list).
    DeviceList {
        /// Input device names.
        inputs: Vec<String>,
        /// Output device names.
        outputs: Vec<String>,
    },
    /// Live peak levels in `0.0..=1.0` (TX mic, RX decoded).
    Levels {
        /// TX mic peak this tick.
        tx_peak: f32,
        /// RX decoded-audio peak this tick.
        rx_peak: f32,
    },
    /// Recording started (`true`) or stopped (`false`).
    Recording(bool),
}

/// Direction of a raised-cosine amplitude ramp.
#[derive(Debug, Clone, Copy)]
enum FadeDirection {
    /// Ramp the first `RX_FADE_SAMPLES` from 0 → 1.
    In,
    /// Ramp the last `RX_FADE_SAMPLES` from 1 → 0.
    Out,
}

/// Apply a 10 ms raised-cosine ramp in place to one 8 kHz frame.
fn apply_fade(pcm: &mut [i16; AMBE_FRAME_SAMPLES], direction: FadeDirection) {
    #[expect(
        clippy::cast_precision_loss,
        reason = "RX_FADE_SAMPLES is 80 — exact in f32"
    )]
    let ramp_len = RX_FADE_SAMPLES as f32;
    for i in 0..RX_FADE_SAMPLES {
        #[expect(clippy::cast_precision_loss, reason = "i < 80 — exact in f32")]
        let rising = 0.5 * (1.0 - (std::f32::consts::PI * i as f32 / ramp_len).cos());
        let (idx, gain) = match direction {
            FadeDirection::In => (i, rising),
            FadeDirection::Out => (AMBE_FRAME_SAMPLES - RX_FADE_SAMPLES + i, 1.0 - rising),
        };
        if let Some(s) = pcm.get_mut(idx) {
            #[expect(
                clippy::cast_possible_truncation,
                reason = "gain ≤ 1.0 keeps the product inside i16 range"
            )]
            {
                *s = (f32::from(*s) * gain) as i16;
            }
        }
    }
}

/// One-frame holdback for RX playback.
///
/// The most recent decoded frame waits here and is released only
/// when its successor arrives, so end-of-stream can fade the final
/// frame's tail before it reaches the speaker. Costs one frame
/// (20 ms) of RX latency — inaudible for reflector listening.
#[derive(Debug, Default)]
struct RxPlayback {
    /// Held-back most recent frame (8 kHz, pre-resample).
    hold: Option<[i16; AMBE_FRAME_SAMPLES]>,
    /// Frames released from holdback so far this stream.
    emitted: usize,
}

impl RxPlayback {
    /// Reset for a new stream, discarding any unflushed holdback
    /// (the previous stream ended without EOT — its tail is stale).
    fn reset(&mut self) {
        *self = Self::default();
    }

    /// Feed one decoded/concealed frame; returns the previous frame,
    /// now due for emission. The stream's first frame is faded in.
    fn push(&mut self, mut pcm: [i16; AMBE_FRAME_SAMPLES]) -> Option<[i16; AMBE_FRAME_SAMPLES]> {
        if self.hold.is_none() && self.emitted == 0 {
            apply_fade(&mut pcm, FadeDirection::In);
        }
        let due = self.hold.replace(pcm);
        if due.is_some() {
            self.emitted = self.emitted.saturating_add(1);
        }
        due
    }

    /// End of stream: returns the held final frame with its tail
    /// faded out, or `None` when nothing is held.
    fn finish(&mut self) -> Option<[i16; AMBE_FRAME_SAMPLES]> {
        let mut last = self.hold.take()?;
        apply_fade(&mut last, FadeDirection::Out);
        self.emitted = self.emitted.saturating_add(1);
        Some(last)
    }
}

/// Cyclic slow-data fragment scheduler for the TX path.
///
/// Holds the scrambled 3-byte fragments for the operator's current
/// text message and/or GPS beacon. `pump_tx` pulls one fragment per
/// non-sync superframe slot, cycling so the message + position repeat
/// for the duration of the transmission — the cadence a receiving
/// station expects.
#[derive(Debug, Default)]
struct TxSlowData {
    /// Scrambled fragments; empty means "send null filler".
    fragments: Vec<[u8; 3]>,
    /// Next index into `fragments`.
    cursor: usize,
}

impl TxSlowData {
    /// Rebuild the fragment sequence from the operator's current text
    /// and position. `my_call` names the DPRS sentence's station.
    fn set(&mut self, text: Option<&str>, gps: Option<&TxPosition>, my_call: &str) {
        let mut fragments = Vec::new();
        if let Some(t) = text
            && !t.is_empty()
        {
            fragments.extend(encode_text_message(t));
        }
        if let Some(pos) = gps.and_then(TxPosition::validated) {
            fragments.extend(encode_gps_fragments(pos, my_call));
        }
        self.fragments = fragments;
        self.cursor = 0;
    }

    /// Next fragment for a non-sync frame, or `None` when nothing is
    /// scheduled (caller then sends the null-filler pattern).
    fn next_fragment(&mut self) -> Option<[u8; 3]> {
        let frag = self.fragments.get(self.cursor).copied()?;
        self.cursor = (self.cursor + 1) % self.fragments.len();
        Some(frag)
    }
}

/// Fragment a DPRS sentence for `pos` into scrambled `0x3X` slow-data
/// blocks (Kenwood layout: type byte + 5 payload bytes per 6-byte
/// block, two 3-byte halves). Returns an empty vec if the position or
/// callsign can't be encoded — lenient, never panics.
fn encode_gps_fragments(pos: &TxPosition, my_call: &str) -> Vec<[u8; 3]> {
    let Ok(callsign) = Callsign::try_from_str(my_call) else {
        return Vec::new();
    };
    let (Ok(latitude), Ok(longitude)) = (
        Latitude::try_new(pos.latitude),
        Longitude::try_new(pos.longitude),
    ) else {
        return Vec::new();
    };
    let report = DprsReport {
        callsign,
        latitude,
        longitude,
        symbol: pos.symbol,
        comment: if pos.comment.is_empty() {
            None
        } else {
            Some(pos.comment.clone())
        },
    };
    let mut sentence = String::new();
    if encode_dprs(&report, &mut sentence).is_err() {
        return Vec::new();
    }
    // DPRS sentences terminate with CR — the RX assembler scans for it.
    sentence.push('\r');

    let mut out = Vec::new();
    for chunk in sentence.as_bytes().chunks(5) {
        let mut block = [0u8; 6];
        block[0] = 0x30; // GPS NMEA-passthrough type nibble.
        for (slot, &b) in block.iter_mut().skip(1).zip(chunk) {
            *slot = b;
        }
        for half in block.chunks(3) {
            let mut frag = [0u8; 3];
            frag.copy_from_slice(half);
            out.push(scramble(frag));
        }
    }
    out
}

impl AudioHandle {
    /// Spawn the audio worker thread.
    ///
    /// `session_tx` is used to push `StartTx` / `TxFrame` / `EndTx`
    /// commands at the session task; those are distinct from the
    /// `AudioCommand`s the GUI sends to the worker itself.
    pub(crate) fn start(
        session_tx: tokio_mpsc::Sender<SessionCommand>,
    ) -> (Self, std_mpsc::Receiver<AudioStatus>) {
        let (cmd_tx, cmd_rx) = std_mpsc::channel();
        let (status_tx, status_rx) = std_mpsc::channel();
        #[expect(
            clippy::expect_used,
            reason = "Thread spawn can only fail from OS resource exhaustion (PTHREAD_CREATE \
                      ENOMEM/EAGAIN), which is unrecoverable inside an egui constructor — \
                      the audio subsystem is mandatory for the GUI's purpose. Panicking \
                      with a named message here produces a clearer crash report than \
                      propagating the error through the GUI init path."
        )]
        let worker = std::thread::Builder::new()
            .name("sextant-audio".into())
            .spawn(move || run_audio_worker(cmd_rx, session_tx, status_tx))
            .expect("spawn audio thread");
        (
            Self {
                cmd_tx,
                _worker: std::sync::Arc::new(worker),
            },
            status_rx,
        )
    }

    /// Send a command to the audio worker. Drops silently if the
    /// worker has exited.
    pub(crate) fn send(&self, cmd: AudioCommand) {
        let _unused = self.cmd_tx.send(cmd);
    }
}

fn run_audio_worker(
    cmd_rx: std_mpsc::Receiver<AudioCommand>,
    session_tx: tokio_mpsc::Sender<SessionCommand>,
    status_tx: std_mpsc::Sender<AudioStatus>,
) {
    // Init failure is no longer fatal — the worker keeps running with
    // no device so the operator can pick a working one from the audio
    // panel. The error is still surfaced to the GUI.
    let audio = match AudioIo::init() {
        Ok(a) => {
            info!(
                in_rate = a.input_rate,
                in_chs = a.input_channels,
                out_rate = a.output_rate,
                out_chs = a.output_channels,
                "audio initialised"
            );
            Some(a)
        }
        Err(e) => {
            error!(error = %e, "audio init failed — TX/RX disabled until a device is selected");
            // Surface to the GUI via the session task so the user sees
            // a real error banner. `try_send` because the session task
            // may not yet be polling — the bounded channel queues it.
            if let Err(send_err) = session_tx.try_send(SessionCommand::AudioInitError(e)) {
                error!(error = %send_err, "could not surface audio init error to GUI");
            }
            None
        }
    };

    let mut worker = AudioWorker {
        audio,
        // 40 ms-lookahead pitch tracker matches OP25's reference
        // (`pitch_est.cc` 2-frame DP) and recovers ~10-15 % more
        // cross-correlation on real voice vs the zero-latency
        // single-frame fallback. The latency cost is one-way,
        // hidden inside the codec (the resampler/jitter buffer
        // between mic and AMBE encoder is already several frames
        // worth), so end-users perceive no extra delay.
        encoder: AmbeEncoder::new_with_lookahead(),
        decoder: AmbeDecoder::new(),
        tx_active: false,
        mic_scratch: Vec::with_capacity(65_536),
        resampled_in: Vec::with_capacity(AMBE_FRAME_SAMPLES),
        resampled_out: Vec::with_capacity(65_536),
        cmd_rx,
        session_tx,
        status_tx,
        tx_stats: TxStats::default(),
        tx_superframe_idx: 0,
        tx_slow_data: TxSlowData::default(),
        tx_my_call: String::new(),
        tx_peak: 0.0,
        rx_peak: 0.0,
        level_tick: 0,
        recorder: None,
        tx_file: None,
        rx_playback: RxPlayback::default(),
        rx_prime: Vec::with_capacity(65_536),
        rx_prime_frames: 0,
        rx_primed: false,
    };
    // Enumerate devices once at startup so the GUI's pickers populate
    // without waiting for an explicit EnumerateDevices command.
    let (inputs, outputs) = enumerate_devices();
    let _unused = worker
        .status_tx
        .send(AudioStatus::DeviceList { inputs, outputs });
    worker.run();
    info!("audio worker shutting down");
}

struct AudioWorker {
    /// `None` when no audio device is open (init failed, or a device
    /// switch is mid-flight). TX/RX silently no-op until a device
    /// is (re)selected.
    audio: Option<AudioIo>,
    encoder: AmbeEncoder,
    decoder: AmbeDecoder,
    tx_active: bool,
    mic_scratch: Vec<f32>,
    resampled_in: Vec<f32>,
    resampled_out: Vec<f32>,
    cmd_rx: std_mpsc::Receiver<AudioCommand>,
    session_tx: tokio_mpsc::Sender<SessionCommand>,
    /// Channel for pushing device lists, level meters, and recording
    /// state to the GUI.
    status_tx: std_mpsc::Sender<AudioStatus>,
    /// Mic-level statistics for the current TX session, reset on
    /// `StartTx` and logged on `StopTx`.  A summary at TX end tells
    /// us unambiguously whether the mic is producing meaningful
    /// audio (`peak_max` ≥ ~0.1, roughly -20 dBFS, typical for spoken
    /// voice) or just floor noise / zeros (common when macOS denies
    /// permission without prompting).
    tx_stats: TxStats,
    /// Frame index inside the current 21-frame superframe, cycled
    /// `0 → 20 → 0 → ...` across the TX stream. Used to pick the
    /// slow-data payload: sync pattern at frame 0, scrambled null
    /// filler otherwise. Reset on `StartTx` so the very first voice
    /// frame after the header carries the sync pattern, which is what
    /// the receiving MMDVM modem locks onto to start audio decode.
    tx_superframe_idx: u8,
    /// Operator's TX slow-data schedule (text + GPS beacon).
    tx_slow_data: TxSlowData,
    /// Operator callsign captured on `StartTx`, used for the DPRS
    /// sentence in `tx_slow_data`.
    tx_my_call: String,
    /// Rolling TX mic peak (`0.0..=1.0`) since the last level push.
    tx_peak: f32,
    /// Rolling RX decoded-audio peak since the last level push.
    rx_peak: f32,
    /// `run`-loop ticks since the last `AudioStatus::Levels` push.
    level_tick: u8,
    /// Active WAV recording writer, `Some` while recording RX audio.
    recorder: Option<hound::WavWriter<std::io::BufWriter<std::fs::File>>>,
    /// 8 kHz mono samples of a file being transmitted, `Some` while a
    /// transmit-from-file is in progress. `pump_tx` pulls frames from
    /// here instead of the mic.
    tx_file: Option<std::vec::IntoIter<f32>>,
    /// RX one-frame holdback + fade bookkeeping.
    rx_playback: RxPlayback,
    /// Resampled PCM accumulated during stream-start priming.
    rx_prime: Vec<f32>,
    /// Frames accumulated into `rx_prime` so far this stream.
    rx_prime_frames: usize,
    /// True once priming has flushed — frames then push straight
    /// through to the speaker.
    rx_primed: bool,
}

#[derive(Debug, Default)]
struct TxStats {
    frames: u32,
    peak_max: f32,
    peak_sum: f64,
    /// Count of frames whose peak was below the typical floor-noise
    /// threshold of -46 dBFS (0.005 linear).  A high ratio here
    /// with low `peak_max` is the "macOS denied mic" smoking gun.
    silent_frames: u32,
    /// True iff the first-frame peak has been reported.
    logged_first: bool,
}

impl TxStats {
    fn reset(&mut self) {
        *self = Self::default();
    }

    fn record(&mut self, peak: f32) -> FirstFrameInfo {
        self.frames = self.frames.saturating_add(1);
        if peak > self.peak_max {
            self.peak_max = peak;
        }
        self.peak_sum += f64::from(peak);
        if peak < 0.005 {
            self.silent_frames = self.silent_frames.saturating_add(1);
        }
        if self.logged_first {
            FirstFrameInfo { peak: f32::NAN }
        } else {
            self.logged_first = true;
            FirstFrameInfo { peak }
        }
    }

    fn mean_peak(&self) -> f64 {
        if self.frames == 0 {
            0.0
        } else {
            self.peak_sum / f64::from(self.frames)
        }
    }
}

struct FirstFrameInfo {
    /// Peak of the first frame; `NaN` if not the first.
    peak: f32,
}

impl AudioWorker {
    fn run(&mut self) {
        loop {
            let mut shutdown = false;
            loop {
                match self.cmd_rx.try_recv() {
                    Ok(cmd) => self.handle_command(cmd),
                    Err(std_mpsc::TryRecvError::Empty) => break,
                    Err(std_mpsc::TryRecvError::Disconnected) => {
                        shutdown = true;
                        break;
                    }
                }
            }
            if shutdown {
                break;
            }
            self.pump_tx();
            // Push level meters to the GUI ~every 50 ms (10 ticks).
            self.level_tick += 1;
            if self.level_tick >= 10 {
                self.level_tick = 0;
                let _unused = self.status_tx.send(AudioStatus::Levels {
                    tx_peak: self.tx_peak,
                    rx_peak: self.rx_peak,
                });
                self.tx_peak = 0.0;
                self.rx_peak = 0.0;
            }
            // ~5 ms tick keeps CPU low while the 20 ms TX cadence
            // stays responsive. Output-path jitter is absorbed by
            // the speaker ringbuf (~1 s of headroom at HW rate).
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn handle_command(&mut self, cmd: AudioCommand) {
        match cmd {
            AudioCommand::StartTx { my_call } => {
                // Purge any stale mic samples so each keying starts
                // fresh. `AudioIo::drain_mic` is non-blocking.
                if let Some(audio) = self.audio.as_mut() {
                    audio.drain_mic();
                }
                self.tx_active = true;
                self.tx_stats.reset();
                // First voice frame after the header is frame 0 of the
                // superframe (sync). The receiving MMDVM modem locks
                // its slow-data scrambler/descrambler to this 21-frame
                // boundary; without sync at the right cadence the radio
                // gets the header but never enters audio decode.
                self.tx_superframe_idx = 0;
                // Match the constructor in `start_audio_worker` —
                // lookahead encoder for OP25-parity voice quality.
                self.encoder = AmbeEncoder::new_with_lookahead();
                // Remember the callsign for the DPRS slow-data sentence.
                self.tx_my_call.clone_from(&my_call);
                if let Err(e) = self.session_tx.try_send(SessionCommand::StartTx {
                    my_call: my_call.clone(),
                }) {
                    warn!(error = %e, "session StartTx enqueue failed");
                }
                tracing::info!(my_call, "TX path enabled — mic capture active");
            }
            AudioCommand::SetSlowData { text, gps } => {
                self.tx_slow_data
                    .set(text.as_deref(), gps.as_ref(), &self.tx_my_call);
            }
            AudioCommand::EnumerateDevices => {
                let (inputs, outputs) = enumerate_devices();
                let _unused = self
                    .status_tx
                    .send(AudioStatus::DeviceList { inputs, outputs });
            }
            AudioCommand::SelectDevices { input, output } => {
                self.select_devices(input.as_deref(), output.as_deref());
            }
            AudioCommand::StartRecording => self.start_recording(),
            AudioCommand::StopRecording => self.stop_recording(),
            AudioCommand::PlayFile { path } => self.play_file(&path),
            AudioCommand::TransmitFile { path } => self.transmit_file(&path),
            AudioCommand::StopTx => self.stop_tx_capture(),
            AudioCommand::RxStart => {
                tracing::info!("RX stream starting — decoder + playback reset");
                self.decoder = AmbeDecoder::new();
                self.rx_playback.reset();
                self.rx_prime.clear();
                self.rx_prime_frames = 0;
                self.rx_primed = false;
            }
            AudioCommand::RxEnd => self.finish_rx_stream(),
            AudioCommand::RxFrame(frame) => {
                tracing::trace!(
                    ambe = format_args!("{:02x?}", frame.ambe),
                    "RX frame received by audio worker"
                );
                // Decode the 9-byte AMBE into 160 i16 samples @ 8 kHz.
                let pcm_i16 = self.decoder.decode_frame(&frame.ambe);
                self.handle_rx_pcm(&pcm_i16);
            }
            AudioCommand::RxLost => {
                let pcm_i16 = self.decoder.conceal_frame();
                self.handle_rx_pcm(&pcm_i16);
            }
        }
    }

    /// Route one decoded (or concealed) 8 kHz frame into the
    /// holdback; emit whichever frame the holdback releases.
    fn handle_rx_pcm(&mut self, pcm_i16: &[i16; AMBE_FRAME_SAMPLES]) {
        if let Some(due) = self.rx_playback.push(*pcm_i16) {
            self.emit_rx_frame(&due);
        }
    }

    /// Stream end: flush the faded tail and any un-flushed priming
    /// buffer (streams shorter than the priming depth), then reset.
    fn finish_rx_stream(&mut self) {
        if let Some(last) = self.rx_playback.finish() {
            self.emit_rx_frame(&last);
        }
        self.flush_rx_prime();
        self.rx_playback.reset();
        self.rx_prime_frames = 0;
        self.rx_primed = false;
    }

    /// Emit one frame down the RX output path: recorder tee, level
    /// meter, resample, then prime-or-push to the speaker.
    fn emit_rx_frame(&mut self, pcm_i16: &[i16; AMBE_FRAME_SAMPLES]) {
        // While recording, tee the decoded 8 kHz PCM straight
        // to the WAV (the codec's native rate — no resampling).
        if let Some(writer) = self.recorder.as_mut() {
            for &s in pcm_i16 {
                let _unused = writer.write_sample(s);
            }
        }
        // Convert to f32 for resampling.
        self.resampled_in.clear();
        self.resampled_in
            .extend(pcm_i16.iter().map(|&s| f32::from(s) / 32768.0));
        // Track the RX peak for the level meter.
        let rx_pk = self
            .resampled_in
            .iter()
            .fold(0.0_f32, |acc, &s| acc.max(s.abs()));
        self.rx_peak = self.rx_peak.max(rx_pk);
        // Resample to HW output rate.
        if let Some(audio) = self.audio.as_mut() {
            self.resampled_out = audio.rx_resampler.process(&self.resampled_in);
        } else {
            return;
        }
        if self.rx_primed {
            if let Some(audio) = self.audio.as_mut() {
                audio.push_speaker(&self.resampled_out);
            }
        } else {
            // Accumulate the stream's first frames so playback opens
            // with jitter headroom instead of racing the network.
            self.rx_prime.extend_from_slice(&self.resampled_out);
            self.rx_prime_frames = self.rx_prime_frames.saturating_add(1);
            if self.rx_prime_frames >= RX_PRIME_FRAMES {
                self.flush_rx_prime();
            }
        }
    }

    /// Push the accumulated priming buffer to the speaker and switch
    /// to pass-through.
    fn flush_rx_prime(&mut self) {
        if !self.rx_prime.is_empty()
            && let Some(audio) = self.audio.as_mut()
        {
            audio.push_speaker(&self.rx_prime);
        }
        self.rx_prime.clear();
        self.rx_primed = true;
    }

    /// Rebuild audio I/O on the named devices, surfacing any failure
    /// to the GUI. Tears the old streams down first.
    fn select_devices(&mut self, input: Option<&str>, output: Option<&str>) {
        // Some hosts allow only one stream per device — drop the old
        // `AudioIo` before opening the new one.
        self.audio = None;
        match AudioIo::init_with(input, output) {
            Ok(io) => self.audio = Some(io),
            Err(e) => {
                error!(error = %e, "device switch failed");
                if let Err(send_err) = self.session_tx.try_send(SessionCommand::AudioInitError(e)) {
                    error!(error = %send_err, "could not surface device error");
                }
            }
        }
    }

    /// Open a WAV recording, surfacing any failure to the GUI.
    fn start_recording(&mut self) {
        match open_recording_writer() {
            Ok(w) => {
                self.recorder = Some(w);
                let _unused = self.status_tx.send(AudioStatus::Recording(true));
            }
            Err(e) => {
                error!(error = %e, "could not start recording");
                if let Err(se) = self.session_tx.try_send(SessionCommand::AudioInitError(e)) {
                    error!(error = %se, "recording error not surfaced");
                }
            }
        }
    }

    /// Finalize and close the active recording, if any.
    fn stop_recording(&mut self) {
        if let Some(w) = self.recorder.take() {
            if let Err(e) = w.finalize() {
                error!(error = %e, "finalize recording");
            }
            let _unused = self.status_tx.send(AudioStatus::Recording(false));
        }
    }

    /// Stop a live mic transmission and log a one-line mic-health
    /// verdict so the operator can tell whether the mic was alive.
    fn stop_tx_capture(&mut self) {
        if !self.tx_active {
            return;
        }
        self.tx_active = false;
        let stats = &self.tx_stats;
        let mean = stats.mean_peak();
        let silence_ratio = if stats.frames == 0 {
            0.0
        } else {
            f64::from(stats.silent_frames) / f64::from(stats.frames)
        };
        let diagnosis = if stats.peak_max < 0.001 {
            "MIC LIKELY DENIED — peak never exceeded -60 dBFS; \
             cpal is receiving zeros. macOS permission not granted. \
             Run via `open target/Sextant.app` (rebuild the bundle first)."
        } else if stats.peak_max < 0.02 {
            "MIC VERY QUIET — peak never exceeded -34 dBFS. Either \
             the mic is muted, the input device is wrong, or the \
             gain is set very low. The AMBE encoder will treat \
             this as silence."
        } else if silence_ratio > 0.7 {
            "MIC MOSTLY SILENT — <30% of frames had usable audio. \
             Speak louder/closer to the mic."
        } else {
            "MIC OK — producing signal above the floor-noise threshold."
        };
        tracing::info!(
            frames = stats.frames,
            peak_max = format_args!("{:.4}", stats.peak_max),
            peak_mean = format_args!("{mean:.4}"),
            silent_frames = stats.silent_frames,
            silence_ratio = format_args!("{:.1}%", silence_ratio * 100.0),
            "TX mic-level summary: {diagnosis}"
        );
        if let Err(e) = self.session_tx.try_send(SessionCommand::EndTx) {
            warn!(error = %e, "session EndTx enqueue failed");
        }
        tracing::info!("TX path disabled — mic capture stopped");
    }

    /// Play a WAV file to the speakers locally (not transmitted).
    fn play_file(&mut self, path: &std::path::Path) {
        let Some(audio) = self.audio.as_mut() else {
            return;
        };
        match load_wav_resampled(path, audio.output_rate) {
            Ok(samples) => audio.push_speaker(&samples),
            Err(e) => {
                error!(error = %e, "play-file: load failed");
                if let Err(se) = self.session_tx.try_send(SessionCommand::AudioInitError(e)) {
                    error!(error = %se, "file error not surfaced");
                }
            }
        }
    }

    /// Begin transmitting a WAV file as an outgoing voice stream.
    /// Refused while a live-mic TX is already active.
    fn transmit_file(&mut self, path: &std::path::Path) {
        if self.tx_active {
            warn!("transmit-from-file ignored — already transmitting");
            return;
        }
        let samples = match load_wav_resampled(path, AMBE_SAMPLE_RATE) {
            Ok(s) => s,
            Err(e) => {
                error!(error = %e, "transmit-from-file: load failed");
                if let Err(se) = self.session_tx.try_send(SessionCommand::AudioInitError(e)) {
                    error!(error = %se, "file error not surfaced");
                }
                return;
            }
        };
        self.tx_superframe_idx = 0;
        self.encoder = AmbeEncoder::new_with_lookahead();
        self.tx_file = Some(samples.into_iter());
        self.tx_active = true;
        if let Err(e) = self.session_tx.try_send(SessionCommand::StartTx {
            my_call: self.tx_my_call.clone(),
        }) {
            warn!(error = %e, "file-transmit StartTx enqueue failed");
        }
    }

    /// Pull the next 160-sample frame from a transmit-from-file, or
    /// `None` when the file is exhausted (which ends the stream).
    fn next_file_frame(&mut self) -> Option<Vec<f32>> {
        let chunk: Vec<f32> = self
            .tx_file
            .as_mut()
            .map_or_else(Vec::new, |f| f.by_ref().take(AMBE_FRAME_SAMPLES).collect());
        if chunk.is_empty() {
            // File exhausted — end the outgoing stream.
            self.tx_file = None;
            self.tx_active = false;
            if let Err(e) = self.session_tx.try_send(SessionCommand::EndTx) {
                warn!(error = %e, "file-transmit EndTx enqueue failed");
            }
            return None;
        }
        Some(chunk)
    }

    /// Pull one 20-ms mic frame, resampled to the 8 kHz codec rate, or
    /// `None` when the mic ringbuf hasn't a full frame ready yet.
    fn next_mic_frame(&mut self) -> Option<Vec<f32>> {
        let audio = self.audio.as_mut()?;
        let hw_per_frame = hw_samples_per_frame(audio.input_rate);
        self.mic_scratch.clear();
        if !audio.pop_mic(hw_per_frame, &mut self.mic_scratch) {
            return None;
        }
        // Raw HW-rate mic peak — feeds the mic-health verdict.
        let mic_peak = self
            .mic_scratch
            .iter()
            .fold(0.0_f32, |acc, &s| acc.max(s.abs()));
        let first_frame = self.tx_stats.record(mic_peak);
        if !first_frame.peak.is_nan() {
            tracing::info!(
                first_peak = format_args!("{:.4}", first_frame.peak),
                "TX: first mic-chunk peak captured"
            );
        }
        Some(audio.tx_resampler.process(&self.mic_scratch))
    }

    /// Source 20-ms voice frames (mic or file), encode each, attach
    /// the slow-data fragment, and forward to the session task.
    fn pump_tx(&mut self) {
        if !self.tx_active {
            // Avoid letting the mic ringbuf bloat while not transmitting.
            if let Some(audio) = self.audio.as_mut() {
                audio.drain_mic();
            }
            return;
        }
        loop {
            let from_file = self.tx_file.is_some();
            let Some(mut codec_frame) = (if from_file {
                self.next_file_frame()
            } else {
                self.next_mic_frame()
            }) else {
                break;
            };

            // Pad / truncate to exactly one codec frame.
            if codec_frame.len() < AMBE_FRAME_SAMPLES {
                codec_frame.resize(AMBE_FRAME_SAMPLES, 0.0);
            } else if codec_frame.len() > AMBE_FRAME_SAMPLES {
                codec_frame.truncate(AMBE_FRAME_SAMPLES);
            }
            let frame_peak = codec_frame.iter().fold(0.0_f32, |acc, &s| acc.max(s.abs()));
            self.tx_peak = self.tx_peak.max(frame_peak);

            let ambe = self.encoder.encode_frame(&codec_frame);
            // Slow-data superframe pattern: sync at frame 0, then the
            // operator's scheduled slow-data fragment (text / GPS) at
            // frames 1-20, or the scrambled null filler otherwise.
            let slow_data = if self.tx_superframe_idx == 0 {
                DSTAR_SYNC_BYTES
            } else {
                self.tx_slow_data
                    .next_fragment()
                    .unwrap_or(DSTAR_NULL_SLOW_DATA_BYTES)
            };
            self.tx_superframe_idx = (self.tx_superframe_idx + 1) % 21;
            let frame = VoiceFrame { ambe, slow_data };
            if let Err(e) = self.session_tx.try_send(SessionCommand::TxFrame(frame)) {
                warn!(error = %e, "TxFrame enqueue dropped");
            }
        }
    }
}

/// Owns the cpal streams and ringbufs. One input (mic) + one output
/// (speaker). Mono samples on both ends — stereo HW is folded to
/// mono in the callbacks.
struct AudioIo {
    _input_stream: cpal::Stream,
    _output_stream: cpal::Stream,
    mic_rx: ringbuf::HeapCons<f32>,
    speaker_tx: ringbuf::HeapProd<f32>,
    input_rate: u32,
    input_channels: u16,
    output_rate: u32,
    output_channels: u16,
    /// HW-rate mic frame → 8 kHz codec rate.
    tx_resampler: Resampler,
    /// 8 kHz codec rate → HW-rate speaker.
    rx_resampler: Resampler,
}

/// Resample a whole buffer with a fixed-chunk sinc resampler. The
/// final chunk is zero-padded. Empty on resampler-build failure.
fn resample_buffer(samples: &[f32], in_rate: u32, out_rate: u32) -> Vec<f32> {
    const CHUNK: usize = 1024;
    if in_rate == out_rate || samples.is_empty() {
        return samples.to_vec();
    }
    let Some(mut rs) = Resampler::new(in_rate, out_rate, CHUNK) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for window in samples.chunks(CHUNK) {
        let mut frame = window.to_vec();
        frame.resize(CHUNK, 0.0);
        out.extend(rs.process(&frame));
    }
    out
}

/// Read a WAV file as mono `f32` resampled to `target_rate`.
///
/// 16-bit PCM only — samples that don't decode as `i16` are dropped
/// (a non-16-bit file therefore plays as silence rather than an
/// error). Multi-channel input is folded to mono.
fn load_wav_resampled(path: &std::path::Path, target_rate: u32) -> Result<Vec<f32>, String> {
    let mut reader = hound::WavReader::open(path).map_err(|e| format!("open wav: {e}"))?;
    let spec = reader.spec();
    let channels = usize::from(spec.channels.max(1));
    let raw: Vec<i16> = reader.samples::<i16>().filter_map(Result::ok).collect();
    let mono: Vec<f32> = raw
        .chunks(channels)
        .map(|frame| {
            let sum: f32 = frame.iter().map(|&s| f32::from(s) / 32768.0).sum();
            #[expect(
                clippy::cast_precision_loss,
                reason = "channel count is a tiny integer (1..=8); exact in f32."
            )]
            let n = frame.len().max(1) as f32;
            sum / n
        })
        .collect();
    Ok(resample_buffer(&mono, spec.sample_rate, target_rate))
}

/// Open a new timestamped 8 kHz mono 16-bit WAV recording writer in
/// `<config dir>/sextant/recordings/`.
fn open_recording_writer() -> Result<hound::WavWriter<std::io::BufWriter<std::fs::File>>, String> {
    let mut dir = dirs_next::config_dir().ok_or("no config dir")?;
    dir.push("sextant");
    dir.push("recordings");
    std::fs::create_dir_all(&dir).map_err(|e| format!("create recordings dir: {e}"))?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let path = dir.join(format!("rx-{stamp}.wav"));
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: AMBE_SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    hound::WavWriter::create(&path, spec).map_err(|e| format!("create wav: {e}"))
}

/// Enumerate cpal device names. Returns `(input names, output names)`.
/// Devices whose name can't be read are skipped.
fn enumerate_devices() -> (Vec<String>, Vec<String>) {
    let host = cpal::default_host();
    let inputs = host
        .input_devices()
        .map(|devs| devs.filter_map(|d| d.name().ok()).collect())
        .unwrap_or_default();
    let outputs = host
        .output_devices()
        .map(|devs| devs.filter_map(|d| d.name().ok()).collect())
        .unwrap_or_default();
    (inputs, outputs)
}

/// Find a device by name, or the host default. `is_input` selects
/// which device list / default to consult.
fn pick_device(host: &cpal::Host, name: Option<&str>, is_input: bool) -> Option<cpal::Device> {
    if let Some(want) = name {
        let devices = if is_input {
            host.input_devices().ok()
        } else {
            host.output_devices().ok()
        };
        if let Some(found) =
            devices.and_then(|mut d| d.find(|dev| dev.name().is_ok_and(|n| n == want)))
        {
            return Some(found);
        }
    }
    if is_input {
        host.default_input_device()
    } else {
        host.default_output_device()
    }
}

impl AudioIo {
    fn init() -> Result<Self, String> {
        Self::init_with(None, None)
    }

    /// Open the named input / output devices, falling back to the host
    /// default when a name is `None` or not found.
    fn init_with(input_name: Option<&str>, output_name: Option<&str>) -> Result<Self, String> {
        let host = cpal::default_host();
        let input_device = pick_device(&host, input_name, true).ok_or("no audio input device")?;
        let output_device =
            pick_device(&host, output_name, false).ok_or("no audio output device")?;

        let input_cfg = input_device
            .default_input_config()
            .map_err(|e| format!("default input config: {e}"))?;
        let output_cfg = output_device
            .default_output_config()
            .map_err(|e| format!("default output config: {e}"))?;

        let input_channels = input_cfg.channels();
        let output_channels = output_cfg.channels();
        let input_rate = input_cfg.sample_rate().0;
        let output_rate = output_cfg.sample_rate().0;

        // ~1 s of buffer at HW rate — plenty of headroom for GC /
        // scheduler hiccups without burning memory.
        let mic_cap = input_rate as usize;
        let speaker_cap = output_rate as usize;
        let (mic_prod, mic_rx) = HeapRb::<f32>::new(mic_cap).split();
        let (speaker_tx, speaker_cons) = HeapRb::<f32>::new(speaker_cap).split();

        let input_stream = build_input_stream(&input_device, &input_cfg, input_channels, mic_prod)?;
        let output_stream =
            build_output_stream(&output_device, &output_cfg, output_channels, speaker_cons)?;

        input_stream
            .play()
            .map_err(|e| format!("start input stream: {e}"))?;
        output_stream
            .play()
            .map_err(|e| format!("start output stream: {e}"))?;

        // Per-direction sinc resamplers. The TX resampler consumes one
        // 20 ms HW-rate mic frame; the RX resampler consumes one 160-
        // sample (8 kHz) decoded voice frame.
        let tx_resampler = Resampler::new(
            input_rate,
            AMBE_SAMPLE_RATE,
            hw_samples_per_frame(input_rate),
        )
        .ok_or("build TX resampler")?;
        let rx_resampler = Resampler::new(AMBE_SAMPLE_RATE, output_rate, AMBE_FRAME_SAMPLES)
            .ok_or("build RX resampler")?;

        Ok(Self {
            _input_stream: input_stream,
            _output_stream: output_stream,
            mic_rx,
            speaker_tx,
            input_rate,
            input_channels,
            output_rate,
            output_channels,
            tx_resampler,
            rx_resampler,
        })
    }

    fn pop_mic(&mut self, n: usize, out: &mut Vec<f32>) -> bool {
        if self.mic_rx.occupied_len() < n {
            return false;
        }
        out.reserve(n);
        for _ in 0..n {
            match self.mic_rx.try_pop() {
                Some(s) => out.push(s),
                None => return false,
            }
        }
        true
    }

    fn drain_mic(&mut self) {
        while self.mic_rx.try_pop().is_some() {}
    }

    fn push_speaker(&mut self, samples: &[f32]) {
        for &s in samples {
            if self.speaker_tx.try_push(s).is_err() {
                // Speaker ringbuf full — drop the rest. Implies
                // output is back-pressuring (shouldn't happen with
                // 1s of buffer unless the speaker stream stalled).
                return;
            }
        }
    }
}

/// Sinc resampler for one mono direction, wrapping `rubato::SincFixedIn`.
///
/// `SincFixedIn` consumes a fixed input chunk and bandlimits with its
/// own windowed-sinc filter, so it both resamples and anti-aliases in
/// one step — replacing the previous hand-rolled linear-interpolation
/// and FIR cascade. Both sextant directions resample fixed-size
/// frames (160 samples at 8 kHz on RX, one HW-rate frame on TX),
/// which is exactly the fixed-input contract `SincFixedIn` wants.
struct Resampler {
    inner: SincFixedIn<f32>,
}

impl Resampler {
    /// Build a resampler from `in_rate` to `out_rate` for fixed input
    /// chunks of `chunk` samples. `None` if rubato rejects the params.
    fn new(in_rate: u32, out_rate: u32, chunk: usize) -> Option<Self> {
        let params = SincInterpolationParameters {
            sinc_len: 128,
            f_cutoff: 0.95,
            interpolation: SincInterpolationType::Linear,
            oversampling_factor: 128,
            window: WindowFunction::BlackmanHarris2,
        };
        let ratio = f64::from(out_rate) / f64::from(in_rate);
        let inner = SincFixedIn::new(ratio, 1.0, params, chunk, 1).ok()?;
        Some(Self { inner })
    }

    /// Resample one fixed-size chunk. `input.len()` must equal the
    /// `chunk` the resampler was built with; a mismatch yields an
    /// empty result rather than a panic.
    fn process(&mut self, input: &[f32]) -> Vec<f32> {
        self.inner.process(&[input], None).map_or_else(
            |_| Vec::new(),
            |out| out.into_iter().next().unwrap_or_default(),
        )
    }
}

/// HW samples per 20 ms at the given rate.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "rate is a u32 audio sample rate (typically 8000..=192000); multiplied \
              by 0.020 and rounded the result is a small non-negative integer that \
              fits a usize on every platform we target."
)]
fn hw_samples_per_frame(rate: u32) -> usize {
    (f64::from(rate) * 0.020).round() as usize
}

fn build_input_stream(
    device: &cpal::Device,
    cfg: &cpal::SupportedStreamConfig,
    channels: u16,
    mut mic_prod: ringbuf::HeapProd<f32>,
) -> Result<cpal::Stream, String> {
    let err_fn = |e| error!(error = %e, "cpal input callback error");
    let stream_cfg = cfg.config();
    match cfg.sample_format() {
        SampleFormat::F32 => device
            .build_input_stream(
                &stream_cfg,
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    write_mono(data, channels, &mut mic_prod);
                },
                err_fn,
                None,
            )
            .map_err(|e| format!("build input stream (f32): {e}")),
        SampleFormat::I16 => device
            .build_input_stream(
                &stream_cfg,
                move |data: &[i16], _: &cpal::InputCallbackInfo| {
                    for chunk in data.chunks(channels as usize) {
                        let sum: f32 = chunk.iter().map(|&s| f32::from(s) / 32768.0).sum();
                        #[expect(
                            clippy::cast_precision_loss,
                            reason = "chunk.len() is a small channel count (typically \
                                      1..=8); f32 represents it exactly. Division to \
                                      average channels is audio-averaging where f32 \
                                      precision is the standard."
                        )]
                        let avg = sum / chunk.len() as f32;
                        let _unused = mic_prod.try_push(avg);
                    }
                },
                err_fn,
                None,
            )
            .map_err(|e| format!("build input stream (i16): {e}")),
        SampleFormat::U16 => device
            .build_input_stream(
                &stream_cfg,
                move |data: &[u16], _: &cpal::InputCallbackInfo| {
                    for chunk in data.chunks(channels as usize) {
                        let sum: f32 = chunk
                            .iter()
                            .map(|&s| (f32::from(s) - 32768.0) / 32768.0)
                            .sum();
                        #[expect(
                            clippy::cast_precision_loss,
                            reason = "chunk.len() is a small channel count (typically \
                                      1..=8); f32 represents it exactly. Division to \
                                      average channels is audio-averaging where f32 \
                                      precision is the standard."
                        )]
                        let avg = sum / chunk.len() as f32;
                        let _unused = mic_prod.try_push(avg);
                    }
                },
                err_fn,
                None,
            )
            .map_err(|e| format!("build input stream (u16): {e}")),
        other => Err(format!("unsupported input sample format: {other:?}")),
    }
}

fn build_output_stream(
    device: &cpal::Device,
    cfg: &cpal::SupportedStreamConfig,
    channels: u16,
    mut speaker_cons: ringbuf::HeapCons<f32>,
) -> Result<cpal::Stream, String> {
    let err_fn = |e| error!(error = %e, "cpal output callback error");
    let stream_cfg = cfg.config();
    match cfg.sample_format() {
        SampleFormat::F32 => device
            .build_output_stream(
                &stream_cfg,
                move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    fill_stereo_from_mono_f32(data, channels, &mut speaker_cons);
                },
                err_fn,
                None,
            )
            .map_err(|e| format!("build output stream (f32): {e}")),
        SampleFormat::I16 => device
            .build_output_stream(
                &stream_cfg,
                move |data: &mut [i16], _: &cpal::OutputCallbackInfo| {
                    for chunk in data.chunks_mut(channels as usize) {
                        let s = speaker_cons.try_pop().unwrap_or(0.0);
                        #[expect(
                            clippy::cast_possible_truncation,
                            reason = "s is clamped to -1.0..=1.0 then multiplied by \
                                      32767.0, yielding -32767.0..=32767.0 — all \
                                      representable in i16 with no truncation."
                        )]
                        let v = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
                        for slot in chunk.iter_mut() {
                            *slot = v;
                        }
                    }
                },
                err_fn,
                None,
            )
            .map_err(|e| format!("build output stream (i16): {e}")),
        SampleFormat::U16 => device
            .build_output_stream(
                &stream_cfg,
                move |data: &mut [u16], _: &cpal::OutputCallbackInfo| {
                    for chunk in data.chunks_mut(channels as usize) {
                        let s = speaker_cons.try_pop().unwrap_or(0.0);
                        #[expect(
                            clippy::cast_possible_truncation,
                            clippy::cast_sign_loss,
                            reason = "s is clamped to -1.0..=1.0, so (s + 1.0) is \
                                      0.0..=2.0, times 32767.5 is 0.0..=65535.0 — \
                                      non-negative and fits in u16 with no truncation."
                        )]
                        let v = ((s.clamp(-1.0, 1.0) + 1.0) * 32767.5) as u16;
                        for slot in chunk.iter_mut() {
                            *slot = v;
                        }
                    }
                },
                err_fn,
                None,
            )
            .map_err(|e| format!("build output stream (u16): {e}")),
        other => Err(format!("unsupported output sample format: {other:?}")),
    }
}

fn write_mono(data: &[f32], channels: u16, mic_prod: &mut ringbuf::HeapProd<f32>) {
    for chunk in data.chunks(channels as usize) {
        let sum: f32 = chunk.iter().sum();
        #[expect(
            clippy::cast_precision_loss,
            reason = "chunk.len() is a small channel count (typically 1..=8); f32 \
                      represents it exactly. Division to average channels is \
                      audio-averaging where f32 precision is the standard."
        )]
        let avg = sum / chunk.len() as f32;
        let _unused = mic_prod.try_push(avg);
    }
}

fn fill_stereo_from_mono_f32(
    data: &mut [f32],
    channels: u16,
    speaker_cons: &mut ringbuf::HeapCons<f32>,
) {
    for chunk in data.chunks_mut(channels as usize) {
        let s = speaker_cons.try_pop().unwrap_or(0.0);
        for slot in chunk.iter_mut() {
            *slot = s;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Resampler;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// rubato's sinc resampler must not alias: a 6500 Hz tone
    /// resampled 48 kHz → 8 kHz must not produce 1500 Hz content
    /// (6500 folds to 8000 − 6500 = 1500 without an anti-alias
    /// filter). rubato's windowed-sinc kernel bandlimits before
    /// downsampling — this guards the property the old hand-rolled
    /// FIR provided; losing it brings sextant↔sextant "garble noise"
    /// back.
    #[test]
    fn resampler_does_not_alias_6500hz_tone() -> TestResult {
        let chunk = 960_usize; // one 20 ms frame at 48 kHz
        let mut rs = Resampler::new(48_000, 8_000, chunk).ok_or("build resampler")?;

        // 10 chunks of a 6500 Hz tone at 48 kHz, peak amplitude 0.5.
        let mut down: Vec<f32> = Vec::new();
        for c in 0..10_usize {
            let mut frame = Vec::with_capacity(chunk);
            for i in 0..chunk {
                #[expect(
                    clippy::cast_precision_loss,
                    reason = "test: sample index bounded by 9600, exact in f32."
                )]
                let t = (c * chunk + i) as f32 / 48_000.0;
                frame.push(0.5 * (t * 2.0 * std::f32::consts::PI * 6500.0).sin());
            }
            down.extend(rs.process(&frame));
        }

        // Single-bin DFT at 1500 Hz (the would-be alias frequency).
        let mut re = 0.0_f32;
        let mut im = 0.0_f32;
        for (i, &s) in down.iter().enumerate() {
            #[expect(
                clippy::cast_precision_loss,
                reason = "test: i bounded by down.len() (~1600), exact in f32."
            )]
            let t = i as f32 / 8_000.0;
            let phase = 2.0 * std::f32::consts::PI * 1500.0 * t;
            re += s * phase.cos();
            im += s * phase.sin();
        }
        #[expect(
            clippy::cast_precision_loss,
            reason = "test: down.len() small, exact in f32."
        )]
        let n_down = down.len().max(1) as f32;
        let alias_mag = re.hypot(im) / n_down;
        assert!(
            alias_mag < 0.05,
            "1500 Hz alias amplitude {alias_mag:.4} exceeds 0.05 — the \
             resampler is leaking high-frequency content into the speech \
             band (sextant↔sextant garble noise)."
        );
        Ok(())
    }

    /// Full sextant↔sextant simulation: generate a voice-like signal,
    /// resample 48 → 8 kHz (TX), AMBE-encode, AMBE-decode, resample
    /// 8 → 48 kHz (RX), and verify the output is audible — not the
    /// "garble noise" / "no voice" symptom. If this passes, the codec
    /// and resampler pipeline is internally consistent; any remaining
    /// sextant↔sextant failure is in cpal I/O, the network, or
    /// operator configuration.
    #[test]
    fn end_to_end_voice_pipeline_produces_audible_output() -> TestResult {
        use super::{AMBE_FRAME_SAMPLES, AMBE_SAMPLE_RATE};
        use mbelib_rs::{AmbeDecoder, AmbeEncoder};

        let hw_rate = 48_000_u32;
        let frame_hw = 960_usize; // 20 ms at 48 kHz
        let mut tx_rs =
            Resampler::new(hw_rate, AMBE_SAMPLE_RATE, frame_hw).ok_or("build TX resampler")?;
        let mut rx_rs = Resampler::new(AMBE_SAMPLE_RATE, hw_rate, AMBE_FRAME_SAMPLES)
            .ok_or("build RX resampler")?;

        // 1. ~2 s of voice-like signal at 48 kHz: F0 ≈ 130 Hz with
        //    vibrato + 8 decaying harmonics and a syllabic envelope.
        let n_frames = 100_usize;
        let total_hw = n_frames * frame_hw;
        let mut hw_input = Vec::with_capacity(total_hw);
        for i in 0..total_hw {
            #[expect(
                clippy::cast_precision_loss,
                reason = "test: i bounded by 96000, exact in f32."
            )]
            let t = i as f32 / hw_rate as f32;
            let f0 = 25.0_f32.mul_add((t * 2.0 * std::f32::consts::PI * 5.0).sin(), 130.0);
            let mut s = 0.0_f32;
            for (k, amp) in [1.0_f32, 0.7, 0.5, 0.4, 0.3, 0.25, 0.2, 0.15]
                .iter()
                .enumerate()
            {
                #[expect(
                    clippy::cast_precision_loss,
                    reason = "test: k bounded by 8, exact in f32."
                )]
                let kf = (k + 1) as f32;
                s += amp * (t * 2.0 * std::f32::consts::PI * f0 * kf).sin();
            }
            let env = 0.3_f32.mul_add((t * 2.0 * std::f32::consts::PI * 3.0).sin().abs(), 0.4);
            hw_input.push(0.4 * env * s);
        }

        // 2. TX: resample 48 → 8 kHz, AMBE-encode.
        let mut enc = AmbeEncoder::new_with_lookahead();
        let mut wire: Vec<[u8; 9]> = Vec::with_capacity(n_frames);
        for f in 0..n_frames {
            let start = f * frame_hw;
            let chunk = hw_input.get(start..start + frame_hw).unwrap_or(&[]);
            let mut resampled = tx_rs.process(chunk);
            // Pad / truncate to exactly one codec frame.
            if resampled.len() < AMBE_FRAME_SAMPLES {
                resampled.resize(AMBE_FRAME_SAMPLES, 0.0);
            } else if resampled.len() > AMBE_FRAME_SAMPLES {
                resampled.truncate(AMBE_FRAME_SAMPLES);
            }
            wire.push(enc.encode_frame(&resampled));
        }

        // 3. RX: AMBE-decode, resample 8 → 48 kHz.
        let mut dec = AmbeDecoder::new();
        let mut hw_output: Vec<f32> = Vec::with_capacity(total_hw);
        for ambe in &wire {
            let pcm = dec.decode_frame(ambe);
            let norm: Vec<f32> = pcm.iter().map(|&s| f32::from(s) / 32768.0).collect();
            hw_output.extend(rx_rs.process(&norm));
        }

        // 4. Skip warmup — encoder, decoder, and both resamplers take
        //    a few frames to converge.
        let warmup = 8 * frame_hw;
        let body = hw_output.get(warmup..).unwrap_or(&[]);
        assert!(!body.is_empty(), "no decoded output past warmup");

        // 5. RMS / peak. The parametric codec doesn't preserve phase,
        //    but loudness should land within ~6 dB of the input.
        let sum_sq: f32 = body.iter().map(|&v| v * v).sum();
        #[expect(
            clippy::cast_precision_loss,
            reason = "test: body.len() bounded by ~96000, exact in f32 mantissa."
        )]
        let rms = (sum_sq / body.len() as f32).sqrt();
        let peak = body.iter().map(|&v| v.abs()).fold(0.0_f32, f32::max);
        assert!(
            rms > 0.02,
            "decoded RMS {rms:.4} below 0.02 — pipeline producing near-silent output."
        );
        assert!(
            peak > 0.05,
            "decoded peak {peak:.4} below 0.05 — pipeline producing near-silent output."
        );
        Ok(())
    }

    #[test]
    fn tx_slow_data_text_roundtrips_through_collector() {
        use dstar_gateway_core::slowdata::SlowDataTextCollector;

        let mut sched = super::TxSlowData::default();
        sched.set(Some("CQ TEST"), None, "W1AW");
        // `encode_text_message` yields exactly 8 fragments for non-empty
        // text — pull them and feed the RX collector at seq 1..=8.
        let mut collector = SlowDataTextCollector::new();
        for seq in 1u8..=8 {
            let Some(frag) = sched.next_fragment() else {
                break;
            };
            collector.push(frag, seq);
        }
        let msg = collector.take_message();
        assert!(
            matches!(&msg, Some(m) if m.starts_with(b"CQ TEST")),
            "TX-scheduled text must decode back through the RX collector, got {msg:?}"
        );
    }

    #[test]
    fn tx_slow_data_empty_yields_no_fragments() {
        let mut sched = super::TxSlowData::default();
        sched.set(None, None, "W1AW");
        assert!(sched.next_fragment().is_none());
    }

    /// Fade-in silences the very first sample and leaves everything
    /// past the ramp untouched.
    #[test]
    fn fade_in_silences_frame_start() -> TestResult {
        use super::{AMBE_FRAME_SAMPLES, FadeDirection, RX_FADE_SAMPLES, apply_fade};
        let mut pcm = [10_000_i16; AMBE_FRAME_SAMPLES];
        apply_fade(&mut pcm, FadeDirection::In);
        assert_eq!(pcm.first().copied(), Some(0), "first sample must be 0");
        let past_ramp = pcm.get(RX_FADE_SAMPLES..).ok_or("ramp within frame")?;
        assert!(
            past_ramp.iter().all(|&s| s == 10_000),
            "samples past the ramp must be untouched"
        );
        Ok(())
    }

    /// Fade-out takes the final sample to (near) zero and leaves the
    /// frame's start untouched.
    #[test]
    fn fade_out_silences_frame_end() -> TestResult {
        use super::{AMBE_FRAME_SAMPLES, FadeDirection, RX_FADE_SAMPLES, apply_fade};
        let mut pcm = [100_i16; AMBE_FRAME_SAMPLES];
        apply_fade(&mut pcm, FadeDirection::Out);
        let last = pcm.last().copied().ok_or("non-empty frame")?;
        assert!(last.abs() <= 1, "final sample must be ~0, got {last}");
        let before_ramp = pcm
            .get(..AMBE_FRAME_SAMPLES - RX_FADE_SAMPLES)
            .ok_or("ramp within frame")?;
        assert!(
            before_ramp.iter().all(|&s| s == 100),
            "samples before the ramp must be untouched"
        );
        Ok(())
    }

    /// The holdback delays every frame by exactly one slot and
    /// fades the stream's first frame in.
    #[test]
    fn holdback_delays_by_one_frame() -> TestResult {
        use super::{AMBE_FRAME_SAMPLES, RxPlayback};
        let mut rx = RxPlayback::default();
        assert!(
            rx.push([100; AMBE_FRAME_SAMPLES]).is_none(),
            "first frame is held"
        );
        let due = rx
            .push([200; AMBE_FRAME_SAMPLES])
            .ok_or("second push releases the first frame")?;
        assert_eq!(
            due.first().copied(),
            Some(0),
            "released first frame is faded in"
        );
        assert_eq!(
            due.last().copied(),
            Some(100),
            "released frame is the first one pushed"
        );
        Ok(())
    }

    /// Finish releases the held tail faded out; a second finish has
    /// nothing left.
    #[test]
    fn finish_fades_out_the_held_tail() -> TestResult {
        use super::{AMBE_FRAME_SAMPLES, RxPlayback};
        let mut rx = RxPlayback::default();
        let _held = rx.push([100; AMBE_FRAME_SAMPLES]);
        let last = rx.finish().ok_or("held frame flushed on finish")?;
        let tail = last.last().copied().ok_or("non-empty frame")?;
        assert!(tail.abs() <= 1, "tail must be faded to ~0, got {tail}");
        assert!(rx.finish().is_none(), "nothing left after finish");
        Ok(())
    }
}
