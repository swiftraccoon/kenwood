//! Accessible SSB/CW/AM demodulator for the TH-D75 IF-over-USB stream.
//!
//! Contains the CAT setup, 12 kHz `ADC stream IN` capture, `if-dsp`
//! demodulation, playback, and accessible prompt pipeline. Startup and the
//! `tune` command land Band B on the requested frequency through the
//! library's read-back-checked UP/DW stepping retune; off-step or distant targets
//! are refused with guidance.
//!
//! Design notes:
//! - No `tokio::signal`: Ctrl-C arrives as a rustyline interrupt and
//!   takes the quit path, leaving process SIGINT semantics untouched.
//! - Audio flows callback -> bounded channel -> DSP thread -> bounded
//!   channel -> callback; volume and signal level cross threads as
//!   `AtomicU32` float bits; overruns/underruns are counters plus a
//!   once-per-session announcement.
//! - Radio state discipline lives in the library's IF-tap session:
//!   `enter_if_tap` saves every touched setting (including the tuning step)
//!   and checks engagement through CAT readback; `restore_if_tap` restores
//!   the saved tuning step before walking the original frequency, and reports
//!   per-step failures.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use if_dsp::{Bandwidth, Channelizer, ChannelizerConfig, DemodMode, DspError};
use kenwood_thd75::transport::SerialTransport;
use kenwood_thd75::types::{Frequency, OperatingMode, StepSize, UsbAudioOutput};
use kenwood_thd75::{IfTapConfig, IfTapRestoreReport, IfTapSavedState, Radio};
use thd75_listen::output;
use thd75_listen::parser::{Command, FilterWidth, ListenerFrequency, SignalLevel, Volume};
#[cfg(test)]
use thd75_repl as _;

/// Default startup frequency in hertz.
const DEFAULT_FREQ_HZ: u32 = 435_640_000;

/// Default volume percentage at startup.
const DEFAULT_VOLUME_PERCENT: u8 = 60;

/// Bound on each audio channel (chunks, not samples): deep enough to
/// ride out scheduling hiccups, shallow enough to cap latency.
const CHANNEL_DEPTH: usize = 8;

/// Commands from the prompt thread to the DSP thread.
enum DspCmd {
    /// Switch demodulation mode.
    SetMode(DemodMode),
    /// Change the audio passband width in hertz.
    SetFilter(Bandwidth),
}

/// State shared with the real-time audio callbacks via atomics.
#[derive(Debug)]
struct SharedAudio {
    /// Output gain as `f32` bits.
    volume_bits: AtomicU32,
    /// Latest signal estimate in decibels (relative scale) as `f32` bits.
    level_db_bits: AtomicU32,
    /// Input chunks dropped because the DSP thread fell behind.
    overruns: AtomicUsize,
    /// Output callbacks that ran short of samples.
    underruns: AtomicUsize,
    /// Input callbacks delivered (stream liveness).
    in_blocks: AtomicUsize,
    /// Output callbacks served (stream liveness).
    out_blocks: AtomicUsize,
    /// Set by the output callback on underrun; cleared when announced.
    underrun_flag: AtomicBool,
}

/// Logarithmic volume taper: 0 is mute, 100 is unity, each step below
/// 100 removes half a decibel.
fn taper(volume: Volume) -> f32 {
    let percent = volume.percent();
    if percent == 0 {
        0.0
    } else {
        10.0_f32.powf((f32::from(percent) - 100.0) / 40.0)
    }
}

/// The DSP thread: demodulate incoming IF chunks, publish a signal
/// level, and hand audio chunks to the output callback. Exits when
/// either channel closes.
///
/// # Errors
///
/// Returns a checked DSP reconfiguration failure before processing another block.
fn dsp_thread(
    mut channelizer: Channelizer,
    rx_in: &Receiver<Vec<f32>>,
    tx_out: &SyncSender<Vec<f32>>,
    rx_cmd: &Receiver<DspCmd>,
    shared: &SharedAudio,
) -> Result<(), DspError> {
    let mut audio = Vec::new();
    let mut level_rms = 0.0_f32;
    while let Ok(block) = rx_in.recv() {
        while let Ok(cmd) = rx_cmd.try_recv() {
            match cmd {
                DspCmd::SetMode(mode) => channelizer.set_mode(mode)?,
                DspCmd::SetFilter(width) => channelizer.set_filter_width(Some(width))?,
            }
        }
        channelizer.process(&block, &mut audio);
        // Signal level: exponentially smoothed RMS of the demodulated
        // audio, published as decibels relative to full scale.
        #[expect(
            clippy::cast_precision_loss,
            reason = "RMS normalization rounds the positive sample count to f32; this affects only the displayed signal estimate"
        )]
        let block_len = audio.len().max(1) as f32;
        let sum_sq: f32 = audio.iter().map(|s| s * s).sum();
        let rms = (sum_sq / block_len).sqrt();
        level_rms = 0.2_f32.mul_add(rms, 0.8 * level_rms);
        let db = if level_rms > 1e-6 {
            20.0 * level_rms.log10()
        } else {
            -120.0
        };
        shared.level_db_bits.store(db.to_bits(), Ordering::Relaxed);
        let outgoing = std::mem::take(&mut audio);
        if tx_out.send(outgoing).is_err() {
            return Ok(());
        }
    }
    Ok(())
}

/// Locate the radio's audio input device ("ADC stream IN") and the
/// default output device.
fn find_audio_devices() -> Result<(cpal::Device, cpal::Device), String> {
    let host = cpal::default_host();
    let mut input = None;
    let devices = host
        .input_devices()
        .map_err(|e| format!("listing audio inputs: {e}"))?;
    for device in devices {
        if let Ok(name) = device.name()
            && name.contains("ADC stream IN")
        {
            input = Some(device);
            break;
        }
    }
    let Some(input) = input else {
        return Err(output::no_device_guidance().to_owned());
    };
    let Some(out) = host.default_output_device() else {
        return Err("no default audio output device".to_owned());
    };
    Ok((input, out))
}

/// Resolve the serial port: `--port` override or USB discovery.
fn find_serial_port(port_arg: Option<String>) -> Result<String, String> {
    if let Some(port) = port_arg {
        return Ok(port);
    }
    let ports = SerialTransport::discover_usb().map_err(|e| format!("USB discovery: {e}"))?;
    ports
        .first()
        .map(|info| info.port_name.clone())
        .ok_or_else(|| output::no_serial_guidance().to_owned())
}

struct CliArgs {
    port: Option<String>,
    frequency: ListenerFrequency,
}

fn parse_args() -> Result<CliArgs, String> {
    let mut port = None;
    let mut frequency = ListenerFrequency::new(Frequency::new(DEFAULT_FREQ_HZ))
        .map_err(|error| error.to_string())?;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--port" => {
                port = Some(args.next().ok_or("--port needs a device path")?);
            }
            "--freq" => {
                let raw = args.next().ok_or("--freq needs megahertz")?;
                match format!("tune {raw}").parse::<Command>() {
                    Ok(Command::Tune(freq)) => frequency = freq,
                    Ok(_) | Err(_) => return Err(format!("invalid --freq value {raw:?}")),
                }
            }
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    Ok(CliArgs { port, frequency })
}

fn main() -> std::process::ExitCode {
    println!("{}", output::banner(env!("CARGO_PKG_VERSION")));
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            println!("{}", output::error(e));
            std::process::ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let args = parse_args()?;
    let config = IfTapConfig::new(OperatingMode::Usb)
        .map_err(|error| format!("configuring the radio for listening: {error}"))?
        .with_step(StepSize::Hz5000);

    // Audio first: if the IF interface is absent there is no point in
    // touching (and having to restore) the radio at all.
    let (input_dev, output_dev) = find_audio_devices()?;

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("runtime: {e}"))?;

    // Serial discovery and open must run inside the runtime context: the
    // serial stream registers with the tokio reactor. Without this guard
    // the library refuses with a typed error naming the missing runtime.
    let transport = {
        let _reactor = rt.enter();
        let port = find_serial_port(args.port)?;
        SerialTransport::open(&port).map_err(|e| format!("opening {port}: {e}"))?
    };
    let mut radio = rt
        .block_on(Radio::connect_with_tnc_exit(transport))
        .map_err(|e| format!("connecting: {e}"))?;
    let _info = rt
        .block_on(radio.identify())
        .map_err(|e| format!("the radio did not identify over CAT: {e}"))?;

    // Engage the IF tap. The library saves every touched setting (band,
    // band mode, output, squelch, operating mode, and the tuning step),
    // applies the listening configuration, and proves IF output engaged by
    // readback; a mid-configure failure rolls the radio back first.
    let saved: IfTapSavedState = match rt.block_on(radio.enter_if_tap(config)) {
        Ok(session) => session.into_saved_state(),
        Err(enter_error) => {
            print_restore_report(&enter_error.rollback);
            drop(rt.block_on(radio.disconnect()));
            return Err(format!(
                "configuring the radio for listening: {enter_error}"
            ));
        }
    };

    // Report restoration failures or an unavailable CAT recovery path before return.
    let result = run_session(
        &rt,
        &mut radio,
        &saved,
        &input_dev,
        &output_dev,
        args.frequency,
    );
    let recovery_error = if radio.cat_recovery_required() {
        rt.block_on(radio.recover_cat()).err()
    } else {
        None
    };

    if recovery_error.is_none() {
        let report = rt.block_on(radio.restore_if_tap(saved));
        if report.is_complete() {
            println!("{}", output::goodbye());
        } else {
            print_restore_report(&report);
        }
    } else {
        println!(
            "{}",
            output::restore_warning("all radio settings because CAT recovery failed")
        );
    }
    drop(rt.block_on(radio.disconnect()));
    match (result, recovery_error) {
        (Ok(()), None) => Ok(()),
        (Err(error), None) => Err(error),
        (Ok(()), Some(recovery)) => Err(format!(
            "CAT recovery failed before radio-state restoration: {recovery}"
        )),
        (Err(error), Some(recovery)) => Err(format!(
            "{error}; CAT recovery also failed before radio-state restoration: {recovery}"
        )),
    }
}

/// Configure the radio, start the audio pipeline, and run the prompt
/// loop until quit. Stream handles live in this scope, so returning
/// stops the audio before the caller restores the radio.
#[expect(
    clippy::too_many_lines,
    reason = "linear startup sequence plus a flat command dispatcher; \
              splitting would thread a dozen locals through helpers \
              without reducing the real complexity"
)]
fn run_session(
    rt: &tokio::runtime::Runtime,
    radio: &mut Radio<SerialTransport>,
    saved: &IfTapSavedState,
    input_dev: &cpal::Device,
    output_dev: &cpal::Device,
    frequency: ListenerFrequency,
) -> Result<(), String> {
    let default_volume = Volume::new(DEFAULT_VOLUME_PERCENT).map_err(|error| error.to_string())?;
    let channelizer = Channelizer::new(ChannelizerConfig::default())
        .map_err(|error| format!("configuring audio processing: {error}"))?;
    // Land Band B on the requested frequency. When it already matches this
    // is a pure verification; otherwise the library steps there with the
    // verified UP/DW commands, dropping the IF output during the walk
    // (frequency writes are rejected while the tap is engaged) and
    // re-engaging it afterwards with a readback proof.
    rt.block_on(retune(radio, saved, frequency.frequency()))?;

    let shared = Arc::new(SharedAudio {
        volume_bits: AtomicU32::new(taper(default_volume).to_bits()),
        level_db_bits: AtomicU32::new((-120.0_f32).to_bits()),
        overruns: AtomicUsize::new(0),
        underruns: AtomicUsize::new(0),
        in_blocks: AtomicUsize::new(0),
        out_blocks: AtomicUsize::new(0),
        underrun_flag: AtomicBool::new(false),
    });

    let (tx_in, rx_in) = sync_channel::<Vec<f32>>(CHANNEL_DEPTH);
    let (tx_out, rx_out) = sync_channel::<Vec<f32>>(CHANNEL_DEPTH);
    let (tx_cmd, rx_cmd) = std::sync::mpsc::channel::<DspCmd>();

    let in_config = input_dev
        .default_input_config()
        .map_err(|e| format!("input config: {e}"))?;
    if in_config.sample_rate().0 != 48_000 {
        return Err(format!(
            "the radio's audio interface reports {} hertz; expected 48000",
            in_config.sample_rate().0
        ));
    }
    if in_config.sample_format() != cpal::SampleFormat::F32 {
        return Err(format!(
            "unsupported input sample format {:?}; expected 32 bit float",
            in_config.sample_format()
        ));
    }
    let in_channels = usize::from(in_config.channels());
    let in_shared = Arc::clone(&shared);
    let input_stream = input_dev
        .build_input_stream(
            &in_config.into(),
            move |data: &[f32], _info: &cpal::InputCallbackInfo| {
                let _ = in_shared.in_blocks.fetch_add(1, Ordering::Relaxed);
                // Mono IF: keep the first channel of each frame.
                let chunk: Vec<f32> = data
                    .chunks(in_channels.max(1))
                    .filter_map(|frame| frame.first().copied())
                    .collect();
                if tx_in.try_send(chunk).is_err() {
                    let _ = in_shared.overruns.fetch_add(1, Ordering::Relaxed);
                }
            },
            |e| tracing_error(&e),
            None,
        )
        .map_err(|e| format!("opening the radio audio stream: {e}"))?;

    let out_config = output_dev
        .default_output_config()
        .map_err(|e| format!("output config: {e}"))?;
    if out_config.sample_format() != cpal::SampleFormat::F32 {
        return Err(format!(
            "unsupported output sample format {:?}; expected 32 bit float",
            out_config.sample_format()
        ));
    }
    let out_channels = usize::from(out_config.channels());
    let out_rate = out_config.sample_rate().0;
    if out_rate != 48_000 {
        return Err(format!(
            "the output device runs at {out_rate} hertz; set it to 48000 in Audio MIDI Setup"
        ));
    }
    let out_shared = Arc::clone(&shared);
    let mut pending: VecDeque<f32> = VecDeque::new();
    let output_stream = output_dev
        .build_output_stream(
            &out_config.into(),
            move |data: &mut [f32], _info: &cpal::OutputCallbackInfo| {
                let _ = out_shared.out_blocks.fetch_add(1, Ordering::Relaxed);
                while let Ok(chunk) = rx_out.try_recv() {
                    pending.extend(chunk);
                }
                let gain = f32::from_bits(out_shared.volume_bits.load(Ordering::Relaxed));
                let mut missed = false;
                for frame in data.chunks_mut(out_channels.max(1)) {
                    let sample = pending.pop_front().map_or_else(
                        || {
                            missed = true;
                            0.0
                        },
                        |s| s * gain,
                    );
                    for slot in frame.iter_mut() {
                        *slot = sample;
                    }
                }
                if missed {
                    let _ = out_shared.underruns.fetch_add(1, Ordering::Relaxed);
                    out_shared.underrun_flag.store(true, Ordering::Relaxed);
                }
                // Bound memory if the DSP races ahead of a stalled device.
                if pending.len() > 96_000 {
                    drop(pending.drain(..pending.len() - 48_000));
                }
            },
            |e| tracing_error(&e),
            None,
        )
        .map_err(|e| format!("opening the speaker stream: {e}"))?;

    let mut rl = rustyline::DefaultEditor::new().map_err(|e| format!("terminal: {e}"))?;
    let mut current_filter =
        FilterWidth::new(DemodMode::Usb.default_filter_hz()).map_err(|error| error.to_string())?;

    input_stream
        .play()
        .map_err(|e| format!("starting the radio audio stream: {e}"))?;
    output_stream
        .play()
        .map_err(|e| format!("starting the speaker stream: {e}"))?;

    let dsp_shared = Arc::clone(&shared);
    let dsp = std::thread::Builder::new()
        .name("if-dsp".to_owned())
        .spawn(move || dsp_thread(channelizer, &rx_in, &tx_out, &rx_cmd, &dsp_shared))
        .map_err(|error| format!("starting audio processing: {error}"))?;

    let mut current_frequency = frequency;
    let mut current_mode = DemodMode::Usb;
    let mut current_volume = default_volume;
    let mut underrun_announced = false;
    println!("{}", output::ready(current_frequency, current_mode));

    loop {
        let line = match rl.readline("listen> ") {
            Ok(line) => line,
            Err(
                rustyline::error::ReadlineError::Interrupted | rustyline::error::ReadlineError::Eof,
            ) => break,
            Err(e) => {
                println!("{}", output::error(format_args!("input failure: {e}")));
                break;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        drop(rl.add_history_entry(&line));
        let command = match line.parse::<Command>() {
            Ok(c) => c,
            Err(msg) => {
                println!("{}", output::error(msg));
                continue;
            }
        };
        match command {
            Command::Tune(freq) => match rt.block_on(retune(radio, saved, freq.frequency())) {
                Ok(()) => {
                    current_frequency = freq;
                    println!("{}", output::tuned(freq));
                }
                Err(e) => println!("{}", output::error(e)),
            },
            Command::Mode(mode) => {
                let width = match FilterWidth::new(mode.default_filter_hz()) {
                    Ok(width) => width,
                    Err(error) => {
                        println!("{}", output::error(error));
                        continue;
                    }
                };
                if tx_cmd.send(DspCmd::SetMode(mode)).is_err() {
                    println!("{}", output::error("audio engine stopped"));
                    break;
                }
                current_mode = mode;
                current_filter = width;
                println!("{}", output::mode_set(mode));
            }
            Command::Filter(filter) => {
                let checked = Bandwidth::new(filter.hz()).and_then(|width| {
                    ChannelizerConfig::new(current_mode).with_filter_width(width)
                });
                let width = match checked {
                    Ok(config) => config.filter_width(),
                    Err(error) => {
                        println!("{}", output::error(error));
                        continue;
                    }
                };
                if tx_cmd.send(DspCmd::SetFilter(width)).is_err() {
                    println!("{}", output::error("audio engine stopped"));
                    break;
                }
                current_filter = filter;
                println!("{}", output::filter_set(filter));
            }
            Command::Volume(pct) => {
                current_volume = pct;
                shared
                    .volume_bits
                    .store(taper(pct).to_bits(), Ordering::Relaxed);
                println!("{}", output::volume_set(pct));
            }
            Command::Signal => {
                let db = f32::from_bits(shared.level_db_bits.load(Ordering::Relaxed));
                match SignalLevel::new(db) {
                    Ok(level) => println!("{}", output::signal(level)),
                    Err(error) => println!("{}", output::error(error)),
                }
            }
            Command::Status => {
                let health = output::StreamHealth {
                    input_blocks: shared.in_blocks.load(Ordering::Relaxed),
                    output_blocks: shared.out_blocks.load(Ordering::Relaxed),
                    overruns: shared.overruns.load(Ordering::Relaxed),
                    underruns: shared.underruns.load(Ordering::Relaxed),
                };
                println!(
                    "{}",
                    output::status(
                        current_frequency,
                        current_mode,
                        current_filter,
                        current_volume,
                        &health,
                    )
                );
            }
            Command::Help => println!("{}", output::help()),
            Command::Quit => break,
        }
        if !underrun_announced && shared.underrun_flag.swap(false, Ordering::Relaxed) {
            underrun_announced = true;
            println!("{}", output::underrun_notice());
        }
    }

    // Stop audio before the caller restores the radio: dropping the
    // streams ends the callbacks; closing the input channel ends the
    // DSP thread.
    drop(input_stream);
    drop(output_stream);
    drop(tx_cmd);
    dsp.join()
        .map_err(|_| "audio engine stopped".to_owned())?
        .map_err(|error| format!("audio processing: {error}"))
}

/// Land Band B on `target` under the engaged IF tap.
///
/// Delegates to the library's verified UP/DW stepping retune: an equal
/// frequency verifies without touching the radio; otherwise the walk is
/// bounded, verified by readback, and the IF output is re-engaged with a
/// readback proof.
async fn retune(
    radio: &mut Radio<SerialTransport>,
    saved: &IfTapSavedState,
    target: Frequency,
) -> Result<(), String> {
    radio
        .retune_if_tap(saved, target, UsbAudioOutput::IntermediateFrequency)
        .await
        .map(|_landed| ())
        .map_err(|e| format!("retuning Band B: {e}"))
}

/// Speak one warning line per restore step that failed or was skipped.
fn print_restore_report(report: &IfTapRestoreReport) {
    for (step, _error) in report.failures() {
        println!("{}", output::restore_warning(&step.to_string()));
    }
    for step in report.not_attempted() {
        println!("{}", output::restore_warning(&step.to_string()));
    }
}

/// Ignore the callback error without allocating, locking or performing I/O.
///
/// Callback counters and underruns remain available through `status`; this
/// hook does not record a separate device-error count.
const fn tracing_error(e: &cpal::StreamError) {
    let _ = e;
}
