use std::time::Duration;

use kenwood_thd75::Radio;
use kenwood_thd75::transport::EitherTransport;
use kenwood_thd75::transport::SerialTransport;
use kenwood_thd75::types::Band;
use tokio::sync::mpsc;

use crate::app::{BandState, Message, RadioState};

/// Poll interval for reading radio state.
/// ~10 commands per cycle (FQ, SQ, MD, PC, RA, FS per band + globals).
/// SM and BY are NOT polled — they use AI push notifications instead.
const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Reconnect poll interval after disconnect.
const RECONNECT_INTERVAL: Duration = Duration::from_secs(1);

/// Open a transport on the calling thread (must be main for BT).
///
/// This is synchronous — call from main before starting tokio.
/// On macOS, Bluetooth RFCOMM callbacks require the main thread's
/// `CFRunLoop`, so transport discovery must happen before the tokio
/// runtime is spawned on a dedicated thread.
///
/// # Arguments
/// - `port`: explicit serial port path (e.g. `/dev/cu.usbmodem*`), or
///   `None` for auto-detection of USB CDC or Bluetooth SPP.
/// - `baud`: baud rate (115200 for USB CDC, 9600 for BT SPP).
///
/// # Returns
/// `(port_path, transport)` on success, or an error string.
pub fn discover_and_open_transport(
    port: Option<&str>,
    baud: u32,
) -> Result<(String, EitherTransport), String> {
    discover_and_open(port, baud)
}

/// Spawn the radio communication task with a pre-opened transport.
///
/// Performs initial handshake (identify, enable AI mode, read firmware
/// version and radio type), then spawns a tokio task that:
/// 1. Polls band state (FQ, SQ, MD, PC, RA, FS) and global state on a timer
/// 2. Processes AI-pushed BY notifications as a gate for SM reads
/// 3. Handles user commands (tune, set squelch, MCP write, etc.)
///
/// S-meter and busy state are event-driven via AI mode, not polled.
/// This avoids spurious firmware spikes on Band B that occur with
/// direct SM/BY polling.
///
/// # Arguments
/// - `mcp_speed`: `"fast"` for `McpSpeed::Fast` (risky), anything else for Safe.
/// - `tx`: channel for sending state updates and errors to the TUI.
/// - `bt_req_tx` / `bt_resp_rx`: channels for requesting BT reconnect from
///   the main thread (`IOBluetooth` RFCOMM must be opened on main).
/// - `cmd_rx`: channel for receiving user commands from the TUI.
#[allow(clippy::similar_names)]
pub async fn spawn_with_transport(
    path: String,
    transport: EitherTransport,
    mcp_speed: String,
    tx: mpsc::UnboundedSender<Message>,
    mut cmd_rx: mpsc::UnboundedReceiver<crate::event::RadioCommand>,
    bt_req_tx: std::sync::mpsc::Sender<(Option<String>, u32)>,
    bt_resp_rx: std::sync::mpsc::Receiver<Result<(String, EitherTransport), String>>,
) -> Result<String, String> {
    let baud = SerialTransport::DEFAULT_BAUD;
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

    // Enable AI (Auto Information) mode — radio pushes BY/FQ/MD notifications
    // instead of requiring polling. This is critical for reliable S-meter:
    // AI-pushed BY notifications go through the radio's internal squelch
    // debouncing, while polled BY reads raw hardware state with spurious spikes.
    radio
        .set_auto_info(true)
        .await
        .map_err(|e| format!("AI mode failed: {e}"))?;

    // Subscribe to unsolicited notifications from AI mode
    let mut notifications = radio.subscribe();

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
        // AI notification state — these fields are updated by push notifications
        // from the radio (AI mode) rather than polling. This reduces USB traffic,
        // provides instant updates, and avoids firmware quirks (e.g., spurious
        // SM/BY spikes on Band B when polled directly).
        let mut s_meter_a: u8 = 0;
        let mut s_meter_b: u8 = 0;
        let mut busy_a = false;
        let mut busy_b = false;

        // Main loop: poll + handle commands + process AI notifications
        'outer: loop {
            loop {
                tokio::select! {
                    () = tokio::time::sleep(POLL_INTERVAL) => {
                        match poll_once(&mut radio, s_meter_a, s_meter_b, busy_a, busy_b).await {
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
                    Ok(notification) = notifications.recv() => {
                        // Process AI-pushed notifications. The radio sends these
                        // automatically when state changes (AI 1 mode). This is
                        // faster than polling and avoids firmware quirks.
                        use kenwood_thd75::protocol::Response;
                        // Other AI notifications (FQ, MD, SQ, VM, etc.) are
                        // handled implicitly — the next poll cycle will read
                        // the updated values. AI mode ensures we don't miss
                        // rapid changes between poll cycles.
                        if let Response::Busy { band, busy } = notification {
                            // BY gate: squelch open → poll SM; closed → zero meter
                            if busy {
                                match radio.get_smeter(band).await {
                                    Ok(level) => match band {
                                        Band::A => { s_meter_a = level; busy_a = true; }
                                        Band::B => { s_meter_b = level; busy_b = true; }
                                        _ => {}
                                    },
                                    Err(e) => {
                                        tracing::warn!(?band, "SM read failed on BY: {e}");
                                        // Still mark busy even though SM read failed
                                        match band {
                                            Band::A => busy_a = true,
                                            Band::B => busy_b = true,
                                            _ => {}
                                        }
                                    }
                                }
                            } else {
                                match band {
                                    Band::A => { s_meter_a = 0; busy_a = false; }
                                    Band::B => { s_meter_b = 0; busy_b = false; }
                                    _ => {}
                                }
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
                                if let Err(e) = radio.frequency_up(band).await {
                                    let _ = tx.send(Message::RadioError(format!("Freq up: {e}")));
                                }
                            }
                            crate::event::RadioCommand::FreqDown(band) => {
                                // DW exists as a blind step-down, but we use the manual
                                // read-subtract-tune path for precision: DW doesn't confirm
                                // the resulting frequency, and we need the exact value for
                                // the TUI display update.
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
                                // Enters MCP mode, modifies one byte, exits. USB/BT drops.
                                let page = offset / 256;
                                let byte_idx = (offset % 256) as usize;
                                let _ = tx.send(Message::RadioError(format!("Writing MCP 0x{offset:04X}...")));
                                match radio.modify_memory_page(page, |data| {
                                    data[byte_idx] = value;
                                }).await {
                                    Ok(()) => {
                                        // Update the in-memory MCP cache so the TUI
                                        // stays in sync without requiring a full re-read.
                                        let _ = tx.send(Message::McpByteWritten { offset, value });
                                        let _ = tx.send(Message::RadioError(format!("MCP 0x{offset:04X} = {value} — reconnecting...")));
                                    }
                                    Err(e) => {
                                        let _ = tx.send(Message::McpError(format!("MCP write 0x{offset:04X}: {e}")));
                                    }
                                }
                                // USB/BT drops after MCP exit
                                let _ = tx.send(Message::Disconnected);
                                break; // Go to reconnect loop
                            }
                        }
                    }
                }
            }

            // Reconnect loop.
            //
            // After MCP programming mode, the D75's USB stack resets and the
            // device may re-enumerate with a different path (e.g., the
            // usbmodem suffix changes on macOS). We first try the original
            // path, then auto-discover USB. If both fail (e.g., BT connection),
            // we request the main thread to open a new BT transport (IOBluetooth
            // RFCOMM must be opened on the main thread for CFRunLoop callbacks).
            // Close the old transport BEFORE opening a new connection. Critical
            // for Bluetooth: do_rfcomm_open() calls [device closeConnection] on
            // the shared IOBluetoothDevice, which would corrupt the old
            // RfcommContext's channel pointer if it's still alive.
            if let Err(e) = radio.close_transport().await {
                tracing::warn!("failed to close transport before reconnect: {e}");
            }
            tokio::time::sleep(Duration::from_secs(3)).await;
            let mut attempts = 0u32;
            loop {
                attempts += 1;
                let _ = tx.send(Message::RadioError(format!(
                    "Reconnect attempt {attempts}..."
                )));

                // For BT paths, skip discover_and_open entirely — IOBluetooth
                // RFCOMM must be opened on the main thread (CFRunLoop). Calling
                // BluetoothTransport::open from a tokio thread is undefined
                // behavior in Objective-C.
                let connect_result = if SerialTransport::is_bluetooth_port(&path_clone) {
                    Err("BT requires main thread".to_string())
                } else {
                    discover_and_open(Some(&path_clone), baud)
                        .or_else(|_| discover_and_open(None, baud))
                }
                .or_else(|_| {
                    bt_req_tx
                        .send((Some(path_clone.clone()), baud))
                        .map_err(|e| e.to_string())?;
                    bt_resp_rx
                        .recv_timeout(Duration::from_secs(10))
                        .map_err(|e| e.to_string())?
                });
                if let Ok((_p, transport)) = connect_result {
                    if let Ok(mut new_radio) = Radio::connect(transport).await {
                        if new_radio.identify().await.is_ok() {
                            if let Err(e) = new_radio.set_auto_info(true).await {
                                tracing::error!("AI mode failed after reconnect: {e}");
                                let _ = tx.send(Message::RadioError(format!(
                                    "AI mode failed: {e} — S-meter may not update"
                                )));
                            }
                            notifications = new_radio.subscribe();
                            s_meter_a = 0;
                            s_meter_b = 0;
                            busy_a = false;
                            busy_b = false;
                            let _ = tx.send(Message::Reconnected);
                            radio = new_radio;
                            continue 'outer;
                        }
                    }
                }
                // Exponential backoff: 1s, 2s, 3s, ... up to 10s
                let delay = RECONNECT_INTERVAL * attempts.min(10);
                tokio::time::sleep(delay).await;
            }
        }
    });

    Ok(path)
}

/// Distinguishes transport errors (connection lost) from protocol errors
/// (parse failures). Transport errors break out of the poll loop to the
/// reconnect path. Protocol errors are non-fatal — the current poll cycle
/// is skipped but the connection stays alive.
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

async fn poll_once(
    radio: &mut Radio<EitherTransport>,
    s_meter_a: u8,
    s_meter_b: u8,
    busy_a: bool,
    busy_b: bool,
) -> Result<RadioState, PollError> {
    let mut band_a = poll_band(radio, Band::A).await?;
    let mut band_b = poll_band(radio, Band::B).await?;

    // S-meter and busy are driven by AI-pushed BY notifications (not polling).
    // This avoids spurious firmware spikes on Band B.
    band_a.s_meter = s_meter_a;
    band_a.busy = busy_a;
    band_b.s_meter = s_meter_b;
    band_b.busy = busy_b;

    // Global state reads — all confirmed safe (no side effects)
    let battery_level = radio.get_battery_level().await.unwrap_or(0);
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
        battery_level,
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

/// Poll per-band state: frequency, squelch setting, mode, power, attenuator, step size.
///
/// **SM and BY are intentionally omitted.** The D75 firmware returns spurious
/// SM=5 / BY=1 spikes when Band B is polled directly. Instead, S-meter and
/// busy state are driven by AI-pushed BY notifications in the main loop,
/// which go through the radio's internal squelch debouncing and match the
/// radio's own display behavior. The `s_meter` and `busy` fields are set to
/// zero here and overwritten by `poll_once` with the AI-driven values.
async fn poll_band(radio: &mut Radio<EitherTransport>, band: Band) -> Result<BandState, PollError> {
    let channel = radio
        .get_frequency(band)
        .await
        .map_err(|e| classify_error(&format!("FQ {band:?}"), &e))?;

    // SM and BY are NOT polled — they are driven by AI-pushed BY notifications.
    // Polling SM/BY causes spurious readings on Band B due to firmware behavior.
    // The AI push path goes through the radio's internal squelch debouncing.

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

    let attenuator = radio.get_attenuator(band).await.unwrap_or(false);
    // FS returns N (not available) in some modes — gracefully default
    let step_size = radio.get_frequency_step(band).await.ok();

    Ok(BandState {
        frequency: channel.rx_frequency,
        mode,
        s_meter: 0, // Set by AI notification handler
        squelch,
        power_level,
        busy: false, // Set by AI notification handler
        attenuator,
        step_size,
    })
}

/// Step frequency down by reading current freq + step size, subtracting, and tuning.
/// The TH-D75 CAT protocol has no native down-step command (only UP exists).
async fn freq_down(
    radio: &mut Radio<EitherTransport>,
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

fn discover_and_open(port: Option<&str>, baud: u32) -> Result<(String, EitherTransport), String> {
    // Explicit port
    if let Some(path) = port {
        if path != "auto" {
            if SerialTransport::is_bluetooth_port(path) {
                // Use native IOBluetooth RFCOMM for BT (bypasses broken serial driver)
                #[cfg(target_os = "macos")]
                {
                    let bt = kenwood_thd75::BluetoothTransport::open(None)
                        .map_err(|e| format!("BT connect failed: {e}"))?;
                    return Ok((path.to_string(), EitherTransport::Bluetooth(bt)));
                }
                #[cfg(not(target_os = "macos"))]
                {
                    let transport = SerialTransport::open(path, baud)
                        .map_err(|e| format!("Failed to open {path}: {e}"))?;
                    return Ok((path.to_string(), EitherTransport::Serial(transport)));
                }
            }
            let transport = SerialTransport::open(path, baud)
                .map_err(|e| format!("Failed to open {path}: {e}"))?;
            return Ok((path.to_string(), EitherTransport::Serial(transport)));
        }
    }

    // Auto-discover: try USB first
    let usb_ports =
        SerialTransport::discover_usb().map_err(|e| format!("USB discovery failed: {e}"))?;

    if let Some(info) = usb_ports.first() {
        let path = info.port_name.clone();
        let transport = SerialTransport::open(&path, baud)
            .map_err(|e| format!("Failed to open {path}: {e}"))?;
        return Ok((path, EitherTransport::Serial(transport)));
    }

    // No USB — try native Bluetooth RFCOMM
    #[cfg(target_os = "macos")]
    {
        if let Ok(bt) = kenwood_thd75::BluetoothTransport::open(None) {
            return Ok(("bluetooth:TH-D75".into(), EitherTransport::Bluetooth(bt)));
        }
    }

    Err("No TH-D75 found on USB or Bluetooth".to_string())
}
