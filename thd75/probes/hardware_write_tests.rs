//! Hardware write verification tests.
//!
//! Strategy: Read current value → Write a DIFFERENT known value → Read back
//! to verify the change took → Restore original. Every test proves the radio's
//! state actually changed, not just that we can echo bytes.
//!
//! SAFETY:
//! - NO transmit (TX) commands
//! - NO lock commands (could lock user out)
//! - All changes are restored to original values
//! - All changes are to non-destructive settings (volume, backlight, etc.)
//!
//! REVIEW BEFORE RUNNING: these modify real radio settings (temporarily).
//!
//! This archival probe source is not registered as a Cargo target. Before a
//! hardware run, review it against `docs/audit/probe_queue.md`, promote the
//! reviewed copy to an explicit test target, and run that target serially.

use kenwood_thd75::radio::Radio;
use kenwood_thd75::transport::SerialTransport;
use kenwood_thd75::types::*;

fn connect() -> Radio<SerialTransport> {
    let ports = SerialTransport::discover_usb().expect("USB discovery failed");
    assert!(!ports.is_empty(), "No TH-D75 found");
    let transport = SerialTransport::open(&ports[0].port_name)
        .expect("Failed to open port");
    Radio::new(transport)
}

// ============================================================
// SAFE WRITE TESTS: cosmetic / non-destructive settings
// ============================================================

/// Test: AF gain (volume) can be written and read back.
/// Safe: just changes speaker volume.
#[tokio::test]
#[ignore]
async fn write_af_gain() -> Result<(), Box<dyn std::error::Error>> {
    let mut radio = connect();
    println!("\n=== WRITE: AF GAIN ===");

    // Read original
    let original = radio.get_af_gain().await?;
    println!("  Original AF gain: {original}");

    // Pick a different value (toggle between 10 and 20)
    let new_value = if original.as_raw() == 10 {
        AfGainLevel::new(20)?
    } else {
        AfGainLevel::new(10)?
    };
    println!("  Writing AF gain: {new_value}");
    radio.set_af_gain(new_value).await?;

    // Read back; must be the new value
    let readback = radio.get_af_gain().await?;
    println!("  Readback AF gain: {readback}");
    assert_eq!(readback, new_value, "AF gain write did not take effect");

    // Restore original
    radio.set_af_gain(original).await?;
    let restored = radio.get_af_gain().await?;
    println!("  Restored AF gain: {restored}");
    assert_eq!(restored, original, "Failed to restore original AF gain");

    let _ = radio.disconnect().await;
    println!("  PASS");
    Ok(())
}

/// Test: Squelch level can be written and read back.
/// Safe: just changes squelch threshold.
#[tokio::test]
#[ignore]
async fn write_squelch() {
    let mut radio = connect();
    println!("\n=== WRITE: SQUELCH ===");

    let original = radio.get_squelch(Band::A).await.unwrap();
    println!("  Original squelch: {original}");

    let level_3 = SquelchLevel::new(3).unwrap();
    let level_5 = SquelchLevel::new(5).unwrap();
    let new_value = if original == level_3 {
        level_5
    } else {
        level_3
    };
    println!("  Writing squelch: {new_value}");
    radio.set_squelch(Band::A, new_value).await.unwrap();

    let readback = radio.get_squelch(Band::A).await.unwrap();
    println!("  Readback squelch: {readback}");
    assert_eq!(readback, new_value, "Squelch write did not take effect");

    radio.set_squelch(Band::A, original).await.unwrap();
    let restored = radio.get_squelch(Band::A).await.unwrap();
    println!("  Restored squelch: {restored}");
    assert_eq!(restored, original);

    let _ = radio.disconnect().await;
    println!("  PASS");
}

/// Test: Backlight brightness can be read (BL on TH-D75).
/// Safe: read-only.
#[tokio::test]
#[ignore]
async fn read_battery_level() {
    let mut radio = connect();
    println!("\n=== READ: BATTERY LEVEL ===");

    let level = radio.get_battery_level().await.unwrap();
    println!("  Battery level: {level} (0=Empty, 1=1/3, 2=2/3, 3=Full)");

    let _ = radio.disconnect().await;
    println!("  PASS");
}

/// Test: VOX on/off can be toggled and read back.
/// Safe: VOX only matters during TX, which we don't do.
#[tokio::test]
#[ignore]
async fn write_vox_toggle() {
    let mut radio = connect();
    println!("\n=== WRITE: VOX TOGGLE ===");

    let original = radio.get_vox().await.unwrap();
    println!("  Original VOX: {original}");

    let new_value = !original;
    println!("  Writing VOX: {new_value}");
    radio.set_vox(new_value).await.unwrap();

    let readback = radio.get_vox().await.unwrap();
    println!("  Readback VOX: {readback}");
    assert_eq!(readback, new_value, "VOX write did not take effect");

    radio.set_vox(original).await.unwrap();
    let restored = radio.get_vox().await.unwrap();
    println!("  Restored VOX: {restored}");
    assert_eq!(restored, original);

    let _ = radio.disconnect().await;
    println!("  PASS");
}

/// Test: VOX gain can be written and read back.
#[tokio::test]
#[ignore]
async fn write_vox_gain() {
    let mut radio = connect();
    println!("\n=== WRITE: VOX GAIN ===");

    let original = radio.get_vox_gain().await.unwrap();
    println!("  Original VOX gain: {original}");

    let gain_3 = VoxGain::new(3).unwrap();
    let gain_6 = VoxGain::new(6).unwrap();
    let new_value = if original == gain_3 { gain_6 } else { gain_3 };
    println!("  Writing VOX gain: {new_value}");
    radio.set_vox_gain(new_value).await.unwrap();

    let readback = radio.get_vox_gain().await.unwrap();
    println!("  Readback VOX gain: {readback}");
    assert_eq!(readback, new_value, "VOX gain write did not take effect");

    radio.set_vox_gain(original).await.unwrap();
    let restored = radio.get_vox_gain().await.unwrap();
    println!("  Restored VOX gain: {restored}");
    assert_eq!(restored, original);

    let _ = radio.disconnect().await;
    println!("  PASS");
}

/// Test: VOX delay can be written and read back.
#[tokio::test]
#[ignore]
async fn write_vox_delay() {
    let mut radio = connect();
    println!("\n=== WRITE: VOX DELAY ===");

    let original = radio.get_vox_delay().await.unwrap();
    println!("  Original VOX delay: {original}");

    let new_value = if original == VoxDelay::MS_750 {
        VoxDelay::MS_2000
    } else {
        VoxDelay::MS_750
    };
    println!("  Writing VOX delay: {new_value}");
    radio.set_vox_delay(new_value).await.unwrap();

    let readback = radio.get_vox_delay().await.unwrap();
    println!("  Readback VOX delay: {readback}");
    assert_eq!(readback, new_value, "VOX delay write did not take effect");

    radio.set_vox_delay(original).await.unwrap();
    let restored = radio.get_vox_delay().await.unwrap();
    println!("  Restored VOX delay: {restored}");
    assert_eq!(restored, original);

    let _ = radio.disconnect().await;
    println!("  PASS");
}

/// Test: Dual band display can be toggled.
/// Safe: just changes display layout.
#[tokio::test]
#[ignore]
async fn write_dual_band() {
    let mut radio = connect();
    println!("\n=== WRITE: DUAL BAND ===");

    let original = radio.get_band_mode().await.unwrap();
    println!("  Original band mode: {original}");

    let new_value = match original {
        BandMode::Dual => BandMode::Single,
        BandMode::Single => BandMode::Dual,
    };
    println!("  Writing band mode: {new_value}");
    radio.set_band_mode(new_value).await.unwrap();

    let readback = radio.get_band_mode().await.unwrap();
    println!("  Readback band mode: {readback}");
    assert_eq!(readback, new_value, "band-mode write did not take effect");

    radio.set_band_mode(original).await.unwrap();
    let restored = radio.get_band_mode().await.unwrap();
    println!("  Restored band mode: {restored}");
    assert_eq!(restored, original);

    let _ = radio.disconnect().await;
    println!("  PASS");
}

// ============================================================
// RADIO PARAMETER WRITES: mode, power, step, tone
// ============================================================

/// Test: Operating mode can be changed (FM <-> NFM).
/// Safe: doesn't transmit, just changes demodulator.
#[tokio::test]
#[ignore]
async fn write_operating_mode() {
    let mut radio = connect();
    println!("\n=== WRITE: MODE ===");

    let original = radio.get_operating_mode(Band::A).await.unwrap();
    println!("  Original mode: {original:?}");

    // Toggle between FM and NFM (both are analog, safe)
    let new_mode = match original {
        OperatingMode::Fm => OperatingMode::Nfm,
        _ => OperatingMode::Fm,
    };
    println!("  Writing mode: {new_mode:?}");
    radio
        .set_operating_mode(Band::A, new_mode)
        .await
        .unwrap();

    let readback = radio.get_operating_mode(Band::A).await.unwrap();
    println!("  Readback mode: {readback:?}");
    assert_eq!(readback, new_mode, "Mode write did not take effect");

    radio
        .set_operating_mode(Band::A, original)
        .await
        .unwrap();
    let restored = radio.get_operating_mode(Band::A).await.unwrap();
    println!("  Restored mode: {restored:?}");
    assert_eq!(restored, original);

    let _ = radio.disconnect().await;
    println!("  PASS");
}

/// Test: Power level can be changed.
/// Safe: only affects TX power which we don't use.
#[tokio::test]
#[ignore]
async fn write_power_level() {
    let mut radio = connect();
    println!("\n=== WRITE: POWER LEVEL ===");

    let original = radio.get_power_level(Band::A).await.unwrap();
    println!("  Original power: {original:?}");

    let new_level = match original {
        PowerLevel::High => PowerLevel::Low,
        PowerLevel::Low => PowerLevel::Medium,
        PowerLevel::Medium => PowerLevel::High,
        PowerLevel::ExtraLow => PowerLevel::High,
    };
    println!("  Writing power: {new_level:?}");
    radio.set_power_level(Band::A, new_level).await.unwrap();

    let readback = radio.get_power_level(Band::A).await.unwrap();
    println!("  Readback power: {readback:?}");
    assert_eq!(readback, new_level, "Power level write did not take effect");

    radio.set_power_level(Band::A, original).await.unwrap();
    let restored = radio.get_power_level(Band::A).await.unwrap();
    println!("  Restored power: {restored:?}");
    assert_eq!(restored, original);

    let _ = radio.disconnect().await;
    println!("  PASS");
}

/// Test: Attenuator can be toggled.
/// Safe: just adds/removes RF attenuation on receive.
#[tokio::test]
#[ignore]
async fn write_attenuator() {
    let mut radio = connect();
    println!("\n=== WRITE: ATTENUATOR ===");

    let original = radio.get_attenuator(Band::A).await.unwrap();
    println!("  Original attenuator: {original}");

    let new_value = !original;
    println!("  Writing attenuator: {new_value}");
    radio.set_attenuator(Band::A, new_value).await.unwrap();

    let readback = radio.get_attenuator(Band::A).await.unwrap();
    println!("  Readback attenuator: {readback}");
    assert_eq!(readback, new_value, "Attenuator write did not take effect");

    radio.set_attenuator(Band::A, original).await.unwrap();
    let restored = radio.get_attenuator(Band::A).await.unwrap();
    println!("  Restored attenuator: {restored}");
    assert_eq!(restored, original);

    let _ = radio.disconnect().await;
    println!("  PASS");
}

/// Test: Auto-info can be enabled and disabled.
/// Safe: just controls whether radio pushes notifications.
#[tokio::test]
#[ignore]
async fn write_auto_info() {
    let mut radio = connect();
    println!("\n=== WRITE: AUTO-INFO ===");

    // AI doesn't have a "get" in our API that returns bool.
    // We test by enabling it, then disabling it, and checking for no errors.
    println!("  Enabling auto-info...");
    radio.set_auto_info(true).await.unwrap();
    println!("  Auto-info enabled OK");

    println!("  Disabling auto-info...");
    radio.set_auto_info(false).await.unwrap();
    println!("  Auto-info disabled OK");

    let _ = radio.disconnect().await;
    println!("  PASS");
}

// ============================================================
// INVESTIGATE REJECTED COMMANDS
// ============================================================

/// Test TN (TNC mode) as a bare read.
///
/// Hardware-verified: bare `TN\r` returns TNC mode data.
#[tokio::test]
#[ignore]
async fn investigate_tn_tnc_mode() {
    let mut radio = connect();
    println!("\n=== INVESTIGATE: TN (TNC MODE) ===");

    println!("  Trying bare TN (TNC mode read)...");
    let result = radio.get_tnc_mode().await;
    match &result {
        Ok(resp) => println!("  TN OK: {resp:?}"),
        Err(e) => println!("  TN fails: {e}"),
    }

    let _ = radio.disconnect().await;
}

/// Investigate SF, the band-indexed scan range.
#[tokio::test]
#[ignore]
async fn investigate_scan_commands() {
    let mut radio = connect();
    println!("\n=== INVESTIGATE: SCAN COMMANDS ===");

    // Try SF with band parameter
    println!("  Trying SF 0 (step size band A)...");
    let result = radio.get_step_size(Band::A).await;
    match &result {
        Ok(resp) => println!("  SF 0 OK: {resp:?}"),
        Err(e) => println!("  SF 0 fails: {e}"),
    }

    let _ = radio.disconnect().await;
}

/// Investigate MR: memory recall takes band,channel format.
/// MR is an action command that switches the active channel.
#[tokio::test]
#[ignore]
async fn investigate_mr_command() {
    let mut radio = connect();
    println!("\n=== INVESTIGATE: MR COMMAND ===");

    // MR with channel 0
    println!("  Trying MR 000...");
    let channel = RegularChannel::new(0).unwrap();
    let result = radio.recall_channel(Band::A, channel).await;
    match &result {
        Ok(resp) => println!("  MR 000 OK: {resp:?}"),
        Err(e) => println!("  MR 000 fails: {e}"),
    }

    // The RE shows MR handler at 0xC002E694; it might take band,channel format
    // rather than just channel number.

    let _ = radio.disconnect().await;
}
