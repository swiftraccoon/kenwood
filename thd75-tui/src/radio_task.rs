use std::time::Duration;

use kenwood_thd75::Radio;
use kenwood_thd75::transport::SerialTransport;
use kenwood_thd75::types::Band;
use tokio::sync::mpsc;

use crate::app::{BandState, Message, RadioState};

/// Poll interval for reading radio state.
/// 14 commands per cycle at 115200 baud — 500ms keeps the radio responsive.
const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Reconnect poll interval after disconnect.
const RECONNECT_INTERVAL: Duration = Duration::from_secs(1);

/// Connect to the radio and spawn the polling loop.
/// Returns the serial port path used (for display in the status bar).
pub async fn spawn(
    port: Option<String>,
    baud: u32,
    mcp_speed: String,
    tx: mpsc::UnboundedSender<Message>,
    mut cmd_rx: mpsc::UnboundedReceiver<crate::event::RadioCommand>,
) -> Result<String, String> {
    let (path, transport) = discover_and_open(port.clone(), baud)?;

    let mut radio = Radio::connect(transport)
        .await
        .map_err(|e| format!("Connect failed: {e}"))?;

    if mcp_speed == "fast" {
        radio.set_mcp_speed(kenwood_thd75::McpSpeed::Fast);
    }

    // Verify identity and read static info
    let _info = radio
        .identify()
        .await
        .map_err(|e| format!("Identify failed: {e}"))?;

    let firmware_version = radio.get_firmware_version().await.unwrap_or_default();
    let radio_type = radio
        .get_radio_type()
        .await
        .map(|(region, variant)| format!("{region} v{variant}"))
        .unwrap_or_default();

    let _ = tx.send(Message::RadioUpdate(RadioState {
        firmware_version,
        radio_type,
        ..RadioState::default()
    }));

    let path_clone = path.clone();

    tokio::spawn(async move {
        // Main loop: poll + handle commands
        'outer: loop {
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(POLL_INTERVAL) => {
                        match poll_once(&mut radio).await {
                            Ok(state) => {
                                if tx.send(Message::RadioUpdate(state)).is_err() {
                                    return;
                                }
                            }
                            Err(PollError::Transport(e)) => {
                                let _ = tx.send(Message::RadioError(e));
                                break; // Go to reconnect
                            }
                            Err(PollError::Protocol(e)) => {
                                // Parse errors are non-fatal — skip this poll cycle
                                let _ = tx.send(Message::RadioError(e));
                            }
                        }
                    }
                    Some(cmd) = cmd_rx.recv() => {
                        match cmd {
                            crate::event::RadioCommand::ReadMemory => {
                                let tx2 = tx.clone();
                                let result = radio.read_memory_image_with_progress(move |page, total| {
                                    let _ = tx2.send(Message::McpProgress { page, total });
                                }).await;
                                match result {
                                    Ok(data) => {
                                        let _ = tx.send(Message::McpReadComplete(data));
                                    }
                                    Err(e) => {
                                        let _ = tx.send(Message::McpError(format!("{e}")));
                                    }
                                }
                                // The TH-D75's USB stack always resets when exiting MCP
                                // programming mode. The connection is guaranteed to drop.
                                let _ = tx.send(Message::Disconnected);
                                break; // Go to reconnect
                            }
                            crate::event::RadioCommand::WriteMemory(data) => {
                                let tx2 = tx.clone();
                                let result = radio.write_memory_image_with_progress(&data, move |page, total| {
                                    let _ = tx2.send(Message::McpProgress { page, total });
                                }).await;
                                match result {
                                    Ok(()) => {
                                        let _ = tx.send(Message::McpWriteComplete);
                                    }
                                    Err(e) => {
                                        let _ = tx.send(Message::McpError(format!("{e}")));
                                    }
                                }
                                // The TH-D75's USB stack always resets when exiting MCP
                                // programming mode. The connection is guaranteed to drop.
                                let _ = tx.send(Message::Disconnected);
                                break; // Go to reconnect
                            }
                            crate::event::RadioCommand::TuneChannel { band, channel } => {
                                if let Err(e) = radio.tune_channel(band, channel).await {
                                    let _ = tx.send(Message::RadioError(format!("Tune failed: {e}")));
                                }
                                // Don't break — stay in poll loop, radio is still connected
                            }
                            crate::event::RadioCommand::FreqUp(band) => {
                                if let Err(e) = radio.freq_up(band).await {
                                    let _ = tx.send(Message::RadioError(format!("Freq up: {e}")));
                                }
                            }
                            crate::event::RadioCommand::FreqDown(band) => {
                                // No CAT down-step command exists, so read current
                                // freq + step, compute new freq, and tune.
                                match freq_down(&mut radio, band).await {
                                    Ok(()) => {}
                                    Err(e) => {
                                        let _ = tx.send(Message::RadioError(format!("Freq down: {e}")));
                                    }
                                }
                            }
                            crate::event::RadioCommand::TuneFreq { band, freq } => {
                                let f = kenwood_thd75::types::Frequency::new(freq);
                                if let Err(e) = radio.tune_frequency(band, f).await {
                                    let _ = tx.send(Message::RadioError(format!("Tune freq: {e}")));
                                }
                            }
                            crate::event::RadioCommand::SetSquelch { band, level } => {
                                if let Err(e) = radio.set_squelch(band, level).await {
                                    let _ = tx.send(Message::RadioError(format!("Set squelch: {e}")));
                                }
                            }
                            crate::event::RadioCommand::SetAttenuator { band, enabled } => {
                                if let Err(e) = radio.set_attenuator(band, enabled).await {
                                    let _ = tx.send(Message::RadioError(format!("Set atten: {e}")));
                                }
                            }
                            crate::event::RadioCommand::SetMode { band, mode } => {
                                if let Err(e) = radio.set_mode(band, mode).await {
                                    let _ = tx.send(Message::RadioError(format!("Set mode: {e} (may require VFO mode)")));
                                }
                            }
                            crate::event::RadioCommand::SetLock(on) => {
                                if let Err(e) = radio.set_lock(on).await {
                                    let _ = tx.send(Message::RadioError(format!("Set lock: {e}")));
                                }
                            }
                            crate::event::RadioCommand::SetDualBand(on) => {
                                if let Err(e) = radio.set_dual_band(on).await {
                                    let _ = tx.send(Message::RadioError(format!("Set dual band: {e}")));
                                }
                            }
                            crate::event::RadioCommand::SetBluetooth(on) => {
                                if let Err(e) = radio.set_bluetooth(on).await {
                                    let _ = tx.send(Message::RadioError(format!("Set bluetooth: {e}")));
                                }
                            }
                            crate::event::RadioCommand::SetVox(on) => {
                                if let Err(e) = radio.set_vox(on).await {
                                    let _ = tx.send(Message::RadioError(format!("Set VOX: {e}")));
                                }
                            }
                            crate::event::RadioCommand::SetVoxGain(level) => {
                                if let Err(e) = radio.set_vox_gain(level).await {
                                    let _ = tx.send(Message::RadioError(format!("Set VOX gain: {e}")));
                                }
                            }
                            crate::event::RadioCommand::SetVoxDelay(delay) => {
                                if let Err(e) = radio.set_vox_delay(delay).await {
                                    let _ = tx.send(Message::RadioError(format!("Set VOX delay: {e}")));
                                }
                            }
                            crate::event::RadioCommand::SetPower { band, level } => {
                                if let Err(e) = radio.set_power_level(band, level).await {
                                    let _ = tx.send(Message::RadioError(format!("Set power: {e}")));
                                }
                            }
                            crate::event::RadioCommand::SetTncBaud(rate) => {
                                if let Err(e) = radio.set_tnc_baud(rate).await {
                                    let _ = tx.send(Message::RadioError(format!("TNC baud: {e}")));
                                }
                            }
                            crate::event::RadioCommand::SetBeaconType(mode) => {
                                if let Err(e) = radio.set_beacon_type(mode).await {
                                    let _ = tx.send(Message::RadioError(format!("Beacon type: {e}")));
                                }
                            }
                            crate::event::RadioCommand::SetGpsConfig(enabled, pc_output) => {
                                if let Err(e) = radio.set_gps_config(enabled, pc_output).await {
                                    let _ = tx.send(Message::RadioError(format!("GPS config: {e}")));
                                }
                            }
                            crate::event::RadioCommand::SetFmRadio(enabled) => {
                                if let Err(e) = radio.set_fm_radio(enabled).await {
                                    let _ = tx.send(Message::RadioError(format!("FM radio: {e}")));
                                }
                            }
                            crate::event::RadioCommand::SetCallsignSlot(slot) => {
                                if let Err(e) = radio.set_active_callsign_slot(slot).await {
                                    let _ = tx.send(Message::RadioError(format!("Callsign slot: {e}")));
                                }
                            }
                            crate::event::RadioCommand::SetDstarSlot(slot) => {
                                if let Err(e) = radio.set_dstar_slot(slot).await {
                                    let _ = tx.send(Message::RadioError(format!("D-STAR slot: {e}")));
                                }
                            }
                            crate::event::RadioCommand::McpWriteByte { offset, value } => {
                                // Single-page MCP write for settings the D75 rejects via CAT.
                                // Enters MCP mode, modifies one byte, exits. USB drops.
                                let page = offset / 256;
                                let byte_idx = (offset % 256) as usize;
                                let _ = tx.send(Message::RadioError(format!("Writing MCP 0x{offset:04X}...")));
                                match radio.modify_memory_page(page, |data| {
                                    data[byte_idx] = value;
                                }).await {
                                    Ok(()) => {
                                        let _ = tx.send(Message::RadioError(format!("MCP 0x{offset:04X} = {value} — reconnecting...")));
                                    }
                                    Err(e) => {
                                        let _ = tx.send(Message::McpError(format!("MCP write 0x{offset:04X}: {e}")));
                                    }
                                }
                                // USB drops after MCP exit
                                let _ = tx.send(Message::Disconnected);
                                break; // Go to reconnect loop
                            }
                        }
                    }
                }
            }

            // Reconnect loop
            loop {
                tokio::time::sleep(RECONNECT_INTERVAL).await;
                match discover_and_open(Some(path_clone.clone()), baud) {
                    Ok((_p, transport)) => {
                        match Radio::connect(transport).await {
                            Ok(mut new_radio) => {
                                if new_radio.identify().await.is_ok() {
                                    let _ = tx.send(Message::Reconnected);
                                    radio = new_radio;
                                    continue 'outer; // Back to main loop
                                }
                            }
                            Err(_) => continue,
                        }
                    }
                    Err(_) => continue,
                }
            }
        }
    });

    Ok(path)
}

/// Distinguishes transport errors (connection lost) from protocol errors (parse failures).
enum PollError {
    Transport(String),
    Protocol(String),
}

fn classify_error(context: &str, e: &kenwood_thd75::Error) -> PollError {
    use kenwood_thd75::Error;
    match e {
        Error::Transport(_) | Error::Timeout(_) => PollError::Transport(format!("{context}: {e}")),
        _ => PollError::Protocol(format!("{context}: {e}")),
    }
}

async fn poll_once(radio: &mut Radio<SerialTransport>) -> Result<RadioState, PollError> {
    let band_a = poll_band(radio, Band::A).await?;
    let band_b = poll_band(radio, Band::B).await?;

    // Global state reads — all confirmed safe (no side effects)
    let backlight = radio.get_backlight().await.unwrap_or(0);
    let beep = radio.get_beep().await.unwrap_or(false);
    let lock = radio.get_lock().await.unwrap_or(false);
    let dual_band = radio.get_dual_band().await.unwrap_or(false);
    let bluetooth = radio.get_bluetooth().await.unwrap_or(false);
    let vox = radio.get_vox().await.unwrap_or(false);
    let vox_gain = radio.get_vox_gain().await.unwrap_or(0);
    let vox_delay = radio.get_vox_delay().await.unwrap_or(0);
    let af_gain = radio.get_af_gain().await.unwrap_or(0);
    let gps = radio.get_gps_config().await.unwrap_or((false, false));
    let beacon_type = radio.get_beacon_type().await.unwrap_or(0);
    Ok(RadioState {
        band_a,
        band_b,
        backlight,
        beep,
        lock,
        dual_band,
        bluetooth,
        vox,
        vox_gain,
        vox_delay,
        af_gain,
        firmware_version: String::new(),
        radio_type: String::new(),
        gps_enabled: gps.0,
        beacon_type,
    })
}

async fn poll_band(radio: &mut Radio<SerialTransport>, band: Band) -> Result<BandState, PollError> {
    let channel = radio
        .get_frequency(band)
        .await
        .map_err(|e| classify_error(&format!("FQ {band:?}"), &e))?;

    let s_meter = radio
        .get_smeter(band)
        .await
        .map_err(|e| classify_error(&format!("SM {band:?}"), &e))?;

    let squelch = radio
        .get_squelch(band)
        .await
        .map_err(|e| classify_error(&format!("SQ {band:?}"), &e))?;

    let mode = radio
        .get_mode(band)
        .await
        .map_err(|e| classify_error(&format!("MD {band:?}"), &e))?;

    let power_level = radio
        .get_power_level(band)
        .await
        .map_err(|e| classify_error(&format!("PC {band:?}"), &e))?;

    let busy = radio.get_busy(band).await.unwrap_or(false);
    let attenuator = radio.get_attenuator(band).await.unwrap_or(false);
    // FS returns N (not available) in some modes — gracefully default
    let step_size = radio.get_frequency_step(band).await.ok();

    Ok(BandState {
        frequency: channel.rx_frequency,
        mode,
        s_meter,
        squelch,
        power_level,
        busy,
        attenuator,
        step_size,
    })
}

/// Step frequency down by reading current freq + step size, subtracting, and tuning.
/// The TH-D75 CAT protocol has no native down-step command (only UP exists).
async fn freq_down(
    radio: &mut Radio<SerialTransport>,
    band: Band,
) -> Result<(), kenwood_thd75::Error> {
    use kenwood_thd75::types::StepSize;

    let ch = radio.get_frequency(band).await?;
    // FS may return N (not available) in some modes — default to 5 kHz
    let step = radio
        .get_frequency_step(band)
        .await
        .unwrap_or(kenwood_thd75::types::StepSize::Hz5000);
    let step_hz: u32 = match step {
        StepSize::Hz5000 => 5_000,
        StepSize::Hz6250 => 6_250,
        StepSize::Hz8330 => 8_330,
        StepSize::Hz9000 => 9_000,
        StepSize::Hz10000 => 10_000,
        StepSize::Hz12500 => 12_500,
        StepSize::Hz15000 => 15_000,
        StepSize::Hz20000 => 20_000,
        StepSize::Hz25000 => 25_000,
        StepSize::Hz30000 => 30_000,
        StepSize::Hz50000 => 50_000,
        StepSize::Hz100000 => 100_000,
    };
    let new_hz = ch.rx_frequency.as_hz().saturating_sub(step_hz);
    radio
        .tune_frequency(band, kenwood_thd75::types::Frequency::new(new_hz))
        .await
}

fn discover_and_open(port: Option<String>, baud: u32) -> Result<(String, SerialTransport), String> {
    if let Some(ref path) = port {
        if path != "auto" {
            // SerialTransport::open auto-detects BT and applies 9600/RTS-CTS
            let transport = SerialTransport::open(path, baud)
                .map_err(|e| format!("Failed to open {path}: {e}"))?;
            return Ok((path.clone(), transport));
        }
    }

    // Auto-discover: try USB first, then Bluetooth
    let usb_ports =
        SerialTransport::discover_usb().map_err(|e| format!("USB discovery failed: {e}"))?;

    if let Some(info) = usb_ports.first() {
        let path = info.port_name.clone();
        let transport = SerialTransport::open(&path, baud)
            .map_err(|e| format!("Failed to open {path}: {e}"))?;
        return Ok((path, transport));
    }

    // No USB — try Bluetooth
    let bt_ports = SerialTransport::discover_bluetooth()
        .map_err(|e| format!("BT discovery failed: {e}"))?;

    if let Some(info) = bt_ports.first() {
        let path = info.port_name.clone();
        let transport = SerialTransport::open(&path, baud)
            .map_err(|e| format!("Failed to open {path}: {e}"))?;
        return Ok((path, transport));
    }

    Err("No TH-D75 found on USB or Bluetooth".to_string())
}
