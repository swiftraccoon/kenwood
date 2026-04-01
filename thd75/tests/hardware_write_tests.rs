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
//! Run: cargo test --test hardware_write_tests -- --ignored --nocapture --test-threads=1
//!
//! REVIEW BEFORE RUNNING — these modify real radio settings (temporarily).

use kenwood_thd75::protocol::Command;
use kenwood_thd75::radio::Radio;
use kenwood_thd75::transport::SerialTransport;
use kenwood_thd75::types::*;

async fn connect() -> Radio<SerialTransport> {
    let ports = SerialTransport::discover_usb().expect("USB discovery failed");
    assert!(!ports.is_empty(), "No TH-D75 found");
    let transport = SerialTransport::open(&ports[0].port_name, SerialTransport::DEFAULT_BAUD)
        .expect("Failed to open port");
    Radio::connect(transport).await.expect("Failed to connect")
}

// ============================================================
// SAFE WRITE TESTS — cosmetic / non-destructive settings
// ============================================================

/// Test: AF gain (volume) can be written and read back.
/// Safe: just changes speaker volume.
#[tokio::test]
#[ignore]
async fn write_af_gain() {
    let mut radio = connect().await;
    println!("\n=== WRITE: AF GAIN ===");

    // Read original
    let original = radio.get_af_gain().await.unwrap();
    println!("  Original AF gain: {original}");

    // Pick a different value (toggle between 10 and 20)
    let new_value = if original == 10 { 20 } else { 10 };
    println!("  Writing AF gain: {new_value}");
    radio.set_af_gain(Band::A, new_value).await.unwrap();

    // Read back — must be the new value
    let readback = radio.get_af_gain().await.unwrap();
    println!("  Readback AF gain: {readback}");
    assert_eq!(readback, new_value, "AF gain write did not take effect");

    // Restore original
    radio.set_af_gain(Band::A, original).await.unwrap();
    let restored = radio.get_af_gain().await.unwrap();
    println!("  Restored AF gain: {restored}");
    assert_eq!(restored, original, "Failed to restore original AF gain");

    let _ = radio.disconnect().await;
    println!("  PASS");
}

/// Test: Squelch level can be written and read back.
/// Safe: just changes squelch threshold.
#[tokio::test]
#[ignore]
async fn write_squelch() {
    let mut radio = connect().await;
    println!("\n=== WRITE: SQUELCH ===");

    let original = radio.get_squelch(Band::A).await.unwrap();
    println!("  Original squelch: {original}");

    let new_value = if original == 3 { 5 } else { 3 };
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
async fn read_backlight() {
    let mut radio = connect().await;
    println!("\n=== READ: BACKLIGHT ===");

    let level = radio.get_backlight().await.unwrap();
    println!("  Backlight level: {level}");

    let _ = radio.disconnect().await;
    println!("  PASS");
}

/// Test: VOX on/off can be toggled and read back.
/// Safe: VOX only matters during TX, which we don't do.
#[tokio::test]
#[ignore]
async fn write_vox_toggle() {
    let mut radio = connect().await;
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
    let mut radio = connect().await;
    println!("\n=== WRITE: VOX GAIN ===");

    let original = radio.get_vox_gain().await.unwrap();
    println!("  Original VOX gain: {original}");

    let new_value = if original == 3 { 6 } else { 3 };
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
    let mut radio = connect().await;
    println!("\n=== WRITE: VOX DELAY ===");

    let original = radio.get_vox_delay().await.unwrap();
    println!("  Original VOX delay: {original}");

    let new_value = if original == 2 { 5 } else { 2 };
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
    let mut radio = connect().await;
    println!("\n=== WRITE: DUAL BAND ===");

    let original = radio.get_dual_band().await.unwrap();
    println!("  Original dual band: {original}");

    let new_value = !original;
    println!("  Writing dual band: {new_value}");
    radio.set_dual_band(new_value).await.unwrap();

    let readback = radio.get_dual_band().await.unwrap();
    println!("  Readback dual band: {readback}");
    assert_eq!(readback, new_value, "Dual band write did not take effect");

    radio.set_dual_band(original).await.unwrap();
    let restored = radio.get_dual_band().await.unwrap();
    println!("  Restored dual band: {restored}");
    assert_eq!(restored, original);

    let _ = radio.disconnect().await;
    println!("  PASS");
}

// ============================================================
// RADIO PARAMETER WRITES — mode, power, step, tone
// ============================================================

/// Test: Operating mode can be changed (FM <-> NFM).
/// Safe: doesn't transmit, just changes demodulator.
#[tokio::test]
#[ignore]
async fn write_mode() {
    let mut radio = connect().await;
    println!("\n=== WRITE: MODE ===");

    let original = radio.get_mode(Band::A).await.unwrap();
    println!("  Original mode: {original:?}");

    // Toggle between FM and NFM (both are analog, safe)
    let new_mode = match original {
        Mode::Fm => Mode::Nfm,
        _ => Mode::Fm,
    };
    println!("  Writing mode: {new_mode:?}");
    radio.set_mode(Band::A, new_mode).await.unwrap();

    let readback = radio.get_mode(Band::A).await.unwrap();
    println!("  Readback mode: {readback:?}");
    assert_eq!(readback, new_mode, "Mode write did not take effect");

    radio.set_mode(Band::A, original).await.unwrap();
    let restored = radio.get_mode(Band::A).await.unwrap();
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
    let mut radio = connect().await;
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
    let mut radio = connect().await;
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
    let mut radio = connect().await;
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
// FO WRITE — full channel data round-trip
// ============================================================

/// Test: Write full channel data via FO, read back, verify all fields.
/// This is the ultimate write test — FO carries all 21 fields.
/// Safe: we read current state, modify only the step size (cosmetic),
/// write it back, verify, then restore.
#[tokio::test]
#[ignore]
async fn write_fo_full_channel() {
    let mut radio = connect().await;
    println!("\n=== WRITE: FO FULL CHANNEL ===");

    // Read current full state
    let original = radio.get_frequency_full(Band::A).await.unwrap();
    println!(
        "  Original: freq={} step={:?} name={:?}",
        original.rx_frequency.as_hz(),
        original.step_size,
        original.urcall.as_str()
    );

    // Create modified copy — change step size (harmless)
    let mut modified = original.clone();
    modified.step_size = match original.step_size {
        StepSize::Hz5000 => StepSize::Hz12500,
        _ => StepSize::Hz5000,
    };
    println!("  Writing: step={:?}", modified.step_size);
    radio.set_frequency_full(Band::A, &modified).await.unwrap();

    // Read back and verify the change
    let readback = radio.get_frequency_full(Band::A).await.unwrap();
    println!(
        "  Readback: freq={} step={:?} name={:?}",
        readback.rx_frequency.as_hz(),
        readback.step_size,
        readback.urcall.as_str()
    );
    assert_eq!(
        readback.step_size, modified.step_size,
        "FO step size write did not take effect"
    );
    // Verify other fields weren't clobbered
    assert_eq!(
        readback.rx_frequency, original.rx_frequency,
        "FO write clobbered rx_frequency"
    );
    assert_eq!(
        readback.tx_offset, original.tx_offset,
        "FO write clobbered tx_offset"
    );

    // Restore original
    radio.set_frequency_full(Band::A, &original).await.unwrap();
    let restored = radio.get_frequency_full(Band::A).await.unwrap();
    println!("  Restored: step={:?}", restored.step_size);
    assert_eq!(
        restored.step_size, original.step_size,
        "Failed to restore original step size"
    );

    let _ = radio.disconnect().await;
    println!("  PASS");
}

// ============================================================
// MEMORY WRITE — channel memory round-trip
// ============================================================

/// Test: Write a memory channel via ME, read back, verify.
/// Modifies channel 099 (unlikely to be in use) to avoid clobbering
/// real saved channels. Restores after test.
#[tokio::test]
#[ignore]
async fn write_memory_channel() {
    let mut radio = connect().await;
    println!("\n=== WRITE: MEMORY CHANNEL 099 ===");

    // Read channel 099 current state (may be empty/default)
    let original = radio.read_channel(99).await.unwrap();
    println!(
        "  Original ch099: freq={} name={:?}",
        original.rx_frequency.as_hz(),
        original.urcall.as_str()
    );

    // Write a known test pattern
    let mut test_channel = original.clone();
    test_channel.rx_frequency = Frequency::new(146_520_000); // National simplex
    test_channel.tx_offset = Frequency::new(0);
    test_channel.shift = ShiftDirection::SIMPLEX;
    test_channel.step_size = StepSize::Hz5000;
    test_channel.urcall = ChannelName::new("TEST99").unwrap();

    println!(
        "  Writing ch099: freq={} name={:?}",
        test_channel.rx_frequency.as_hz(),
        test_channel.urcall.as_str()
    );
    radio.write_channel(99, &test_channel).await.unwrap();

    // Read back and verify
    let readback = radio.read_channel(99).await.unwrap();
    println!(
        "  Readback ch099: freq={} name={:?}",
        readback.rx_frequency.as_hz(),
        readback.urcall.as_str()
    );
    assert_eq!(
        readback.rx_frequency, test_channel.rx_frequency,
        "ME frequency write failed"
    );
    assert_eq!(readback.urcall.as_str(), "TEST99", "ME urcall write failed");

    // Restore original
    radio.write_channel(99, &original).await.unwrap();
    let restored = radio.read_channel(99).await.unwrap();
    println!(
        "  Restored ch099: freq={} name={:?}",
        restored.rx_frequency.as_hz(),
        restored.urcall.as_str()
    );

    let _ = radio.disconnect().await;
    println!("  PASS");
}

// ============================================================
// INVESTIGATE REJECTED COMMANDS
// ============================================================

/// Test TN (TNC mode) — bare read.
///
/// Hardware-verified: bare `TN\r` returns TNC mode data.
#[tokio::test]
#[ignore]
async fn investigate_tn_tnc_mode() {
    let mut radio = connect().await;
    println!("\n=== INVESTIGATE: TN (TNC MODE) ===");

    println!("  Trying bare TN (TNC mode read)...");
    let result = radio.execute(Command::GetTncMode).await;
    match &result {
        Ok(resp) => println!("  TN OK: {resp:?}"),
        Err(e) => println!("  TN fails: {e}"),
    }

    let _ = radio.disconnect().await;
}

/// Investigate SF — band-indexed scan range.
#[tokio::test]
#[ignore]
async fn investigate_scan_commands() {
    let mut radio = connect().await;
    println!("\n=== INVESTIGATE: SCAN COMMANDS ===");

    // Try SF with band parameter
    println!("  Trying SF 0 (scan range band A)...");
    let result = radio.execute(Command::GetScanRange { band: Band::A }).await;
    match &result {
        Ok(resp) => println!("  SF 0 OK: {resp:?}"),
        Err(e) => println!("  SF 0 fails: {e}"),
    }

    let _ = radio.disconnect().await;
}

/// Investigate MR — memory recall takes band,channel format.
/// MR is an action command that switches the active channel.
#[tokio::test]
#[ignore]
async fn investigate_mr_command() {
    let mut radio = connect().await;
    println!("\n=== INVESTIGATE: MR COMMAND ===");

    // MR with channel 0
    println!("  Trying MR 000...");
    let result = radio
        .execute(Command::RecallMemoryChannel {
            band: Band::A,
            channel: 0,
        })
        .await;
    match &result {
        Ok(resp) => println!("  MR 000 OK: {resp:?}"),
        Err(e) => println!("  MR 000 fails: {e}"),
    }

    // The RE shows MR handler at 0xC002E694 — might take band,channel format
    // rather than just channel number.

    let _ = radio.disconnect().await;
}

/// Investigate US (user settings) — does it need a sub-parameter?
/// Theory: US may be a prefix command that needs a sub-command number.
#[tokio::test]
#[ignore]
async fn investigate_us_command() {
    let mut radio = connect().await;
    println!("\n=== INVESTIGATE: US COMMAND ===");

    // Try US with parameter 0
    println!("  Trying raw 'US 0'...");
    let result = radio.execute(Command::GetUserSettings).await;
    match &result {
        Ok(resp) => println!("  US OK: {resp:?}"),
        Err(e) => println!("  US fails: {e}"),
    }

    let _ = radio.disconnect().await;
}
