//! Accessible SSB/CW/AM demodulator for the TH-D75 IF-over-USB stream.
//!
//! Tunes the radio over CAT (the USB CDC interface) while capturing
//! the 12 kHz IF from the radio's USB audio interface (device name
//! "ADC stream IN"), demodulating it with `if-dsp`, and playing the
//! audio on the default output device.
//!
//! Design notes:
//! - No `tokio::signal`: Ctrl-C arrives as a rustyline interrupt and
//!   takes the quit path, leaving process SIGINT semantics untouched.
//! - Audio flows callback -> bounded channel -> DSP thread -> bounded
//!   channel -> callback; volume and signal level cross threads as
//!   `AtomicU32` float bits; overruns/underruns are counters plus a
//!   once-per-session announcement.
//! - Every radio setting touched is saved first and restored on every
//!   exit path (see `thd75_listen::session`).

// The accessibility-lint dev-dependency is used by the library's unit
// tests; the bin's test build links dev-deps too.
#[cfg(test)]
use thd75_repl as _;

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use if_dsp::{Channelizer, ChannelizerConfig, DemodMode};
use kenwood_thd75::Radio;
use kenwood_thd75::transport::SerialTransport;
use kenwood_thd75::types::{Band, DetectOutputMode, Frequency};
use thd75_listen::parser::{self, Command};
use thd75_listen::{output, session};

/// Default startup frequency: the 70 cm slot the IF tap was
/// hardware-verified on.
const DEFAULT_FREQ_HZ: u32 = 435_640_000;

/// Default volume percentage at startup.
const DEFAULT_VOLUME: u8 = 60;

/// Bound on each audio channel (chunks, not samples): deep enough to
/// ride out scheduling hiccups, shallow enough to cap latency.
const CHANNEL_DEPTH: usize = 8;

/// Commands from the prompt thread to the DSP thread.
enum DspCmd {
    /// Switch demodulation mode.
    SetMode(DemodMode),
    /// Change the audio passband width in hertz.
    SetFilter(f32),
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
fn taper(percent: u8) -> f32 {
    if percent == 0 {
        0.0
    } else {
        10.0_f32.powf((f32::from(percent) - 100.0) / 40.0)
    }
}

/// The DSP thread: demodulate incoming IF chunks, publish a signal
/// level, and hand audio chunks to the output callback. Exits when
/// either channel closes.
fn dsp_thread(
    rx_in: &Receiver<Vec<f32>>,
    tx_out: &SyncSender<Vec<f32>>,
    rx_cmd: &Receiver<DspCmd>,
    shared: &Arc<SharedAudio>,
) {
    let mut channelizer = Channelizer::new(ChannelizerConfig::default());
    let mut audio = Vec::new();
    let mut level_rms = 0.0_f32;
    while let Ok(block) = rx_in.recv() {
        while let Ok(cmd) = rx_cmd.try_recv() {
            match cmd {
                DspCmd::SetMode(mode) => channelizer.set_mode(mode),
                DspCmd::SetFilter(hz) => channelizer.set_filter_hz(Some(hz)),
            }
        }
        channelizer.process(&block, &mut audio);
        // Signal level: exponentially smoothed RMS of the demodulated
        // audio, published as decibels relative to full scale.
        #[expect(
            clippy::cast_precision_loss,
            reason = "audio block lengths are tiny; usize -> f32 is exact"
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
            return;
        }
    }
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
    freq_hz: u32,
}

fn parse_args() -> Result<CliArgs, String> {
    let mut port = None;
    let mut freq_hz = DEFAULT_FREQ_HZ;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--port" => {
                port = Some(args.next().ok_or("--port needs a device path")?);
            }
            "--freq" => {
                let raw = args.next().ok_or("--freq needs megahertz")?;
                match parser::parse(&format!("tune {raw}")) {
                    Ok(Command::Tune(mhz)) => {
                        #[expect(
                            clippy::cast_possible_truncation,
                            clippy::cast_sign_loss,
                            reason = "the parser bounds the value to 0.1-524 MHz, \
                                      so hertz fits u32 and is positive"
                        )]
                        {
                            freq_hz = (mhz * 1_000_000.0).round() as u32;
                        }
                    }
                    Ok(_) | Err(_) => return Err(format!("invalid --freq value {raw:?}")),
                }
            }
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    Ok(CliArgs { port, freq_hz })
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

    // Audio first: if the IF interface is absent there is no point in
    // touching (and having to restore) the radio at all.
    let (input_dev, output_dev) = find_audio_devices()?;

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("runtime: {e}"))?;

    // Serial discovery and open must run inside the runtime context:
    // the serial stream registers with the tokio reactor and PANICS
    // (rather than erroring) when none is active.
    let transport = {
        let _reactor = rt.enter();
        let port = find_serial_port(args.port)?;
        SerialTransport::open(&port, 115_200).map_err(|e| format!("opening {port}: {e}"))?
    };
    let mut radio = rt
        .block_on(Radio::connect_safe(transport))
        .map_err(|e| format!("connecting: {e}"))?;
    let _info = rt
        .block_on(radio.identify())
        .map_err(|e| format!("the radio did not identify over CAT: {e}"))?;

    let saved = rt
        .block_on(session::save_state(&mut radio))
        .map_err(|e| format!("saving radio state: {e}"))?;

    // Every path from here restores the radio before returning.
    let result = run_session(&rt, &mut radio, &input_dev, &output_dev, args.freq_hz);
    let report = rt.block_on(session::restore(&mut radio, saved));
    if report.failed.is_empty() {
        println!("{}", output::goodbye());
    } else {
        for item in report.failed {
            println!("{}", output::restore_warning(item));
        }
    }
    drop(rt.block_on(radio.disconnect()));
    result
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
    input_dev: &cpal::Device,
    output_dev: &cpal::Device,
    freq_hz: u32,
) -> Result<(), String> {
    // Tune BEFORE engaging IF output: the radio rejects FO frequency
    // writes with "not available in current mode" while IO = IF is
    // active (hardware-verified). configure_for_listening ends with
    // the IF engage, so the initial tune must precede it, and the
    // tune command toggles IF off and back on around each retune.
    rt.block_on(radio.tune_frequency(Band::B, Frequency::new(freq_hz)))
        .map_err(|e| format!("initial tune: {e}"))?;
    rt.block_on(session::configure_for_listening(radio))?;

    let shared = Arc::new(SharedAudio {
        volume_bits: AtomicU32::new(taper(DEFAULT_VOLUME).to_bits()),
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

    let dsp_shared = Arc::clone(&shared);
    let dsp = std::thread::spawn(move || dsp_thread(&rx_in, &tx_out, &rx_cmd, &dsp_shared));

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

    input_stream
        .play()
        .map_err(|e| format!("starting the radio audio stream: {e}"))?;
    output_stream
        .play()
        .map_err(|e| format!("starting the speaker stream: {e}"))?;

    let mut current_hz = freq_hz;
    let mut current_mode = DemodMode::Usb;
    let mut current_filter = current_mode.default_filter_hz();
    let mut current_volume = DEFAULT_VOLUME;
    let mut underrun_announced = false;
    println!("{}", output::ready(current_hz, current_mode));

    let mut rl = rustyline::DefaultEditor::new().map_err(|e| format!("terminal: {e}"))?;
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
        let command = match parser::parse(&line) {
            Ok(c) => c,
            Err(msg) => {
                println!("{}", output::error(msg));
                continue;
            }
        };
        match command {
            Command::Tune(mhz) => {
                #[expect(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "the parser bounds the value to 0.1-524 MHz, \
                              so hertz fits u32 and is positive"
                )]
                let hz = (mhz * 1_000_000.0).round() as u32;
                match rt.block_on(retune(radio, hz)) {
                    Ok(()) => {
                        current_hz = hz;
                        println!("{}", output::tuned(hz));
                    }
                    Err(e) => println!("{}", output::error(e)),
                }
            }
            Command::Mode(mode) => {
                current_mode = mode;
                current_filter = mode.default_filter_hz();
                if tx_cmd.send(DspCmd::SetMode(mode)).is_err() {
                    println!("{}", output::error("audio engine stopped"));
                    break;
                }
                println!("{}", output::mode_set(mode));
            }
            Command::Filter(hz) => {
                current_filter = hz;
                if tx_cmd.send(DspCmd::SetFilter(hz)).is_err() {
                    println!("{}", output::error("audio engine stopped"));
                    break;
                }
                println!("{}", output::filter_set(hz));
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
                println!("{}", output::signal(db));
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
                        current_hz,
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
    drop(dsp.join());
    Ok(())
}

/// Retune with the IF toggle.
///
/// The radio rejects FO frequency writes while `IO = IF` is engaged
/// (hardware-verified), so drop to AF, tune, re-engage IF, and verify
/// the re-engage by readback. The audio pauses for the toggle.
async fn retune(radio: &mut Radio<SerialTransport>, hz: u32) -> Result<(), String> {
    radio
        .set_io_port(DetectOutputMode::Af)
        .await
        .map_err(|e| format!("pausing IF output: {e}"))?;
    let tuned = radio.tune_frequency(Band::B, Frequency::new(hz)).await;
    let back = radio.set_io_port(DetectOutputMode::If).await;
    let verified = matches!(radio.get_io_port().await, Ok(DetectOutputMode::If));
    tuned.map_err(|e| e.to_string())?;
    back.map_err(|e| format!("resuming IF output: {e}"))?;
    if verified {
        Ok(())
    } else {
        Err("IF output did not re-engage after tuning".to_owned())
    }
}

/// Audio-callback error hook: cpal reports stream errors here from a
/// real-time context, so just count on stderr-free best effort.
const fn tracing_error(e: &cpal::StreamError) {
    // Streams report errors rarely (device unplugged); the prompt loop
    // surfaces the consequence (underruns / silence) accessibly.
    let _ = e;
}
