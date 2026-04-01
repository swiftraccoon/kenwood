//! Verify CAT WRITE commands work by sending through our library's serialize().
//! Tests: set value → read back → verify change → restore original.
//!
//! Run: cargo test --test cat_write_verification -- --ignored --nocapture --test-threads=1

use kenwood_thd75::radio::Radio;
use kenwood_thd75::transport::SerialTransport;
use kenwood_thd75::types::*;

async fn connect() -> Radio<SerialTransport> {
    let ports = SerialTransport::discover_usb().unwrap();
    Radio::connect(
        SerialTransport::open(&ports[0].port_name, SerialTransport::DEFAULT_BAUD).unwrap(),
    )
    .await
    .unwrap()
}

macro_rules! test_write {
    ($radio:expr, $name:expr, $read:expr, $write_new:expr, $read_after:expr, $restore:expr, $check:expr) => {{
        print!("  {:<25}", $name);
        let original = $read;
        let write_result = $write_new;
        match write_result {
            Ok(_) => {
                let readback = $read_after;
                let ok = $check(&readback);
                if ok {
                    let _ = $restore;
                    println!("OK (set → read → verify → restore)");
                } else {
                    let _ = $restore;
                    println!("FAIL (write accepted but readback didn't match)");
                }
            }
            Err(e) => println!("WRITE_FAILED: {e}"),
        }
    }};
}

#[tokio::test]
#[ignore]
async fn verify_cat_writes() {
    let mut radio = connect().await;

    println!("\n=== CAT WRITE COMMAND VERIFICATION ===\n");
    println!("  {:<25} RESULT", "COMMAND");
    println!("  {}", "-".repeat(60));

    // VOX on/off
    let orig_vox = radio.get_vox().await.unwrap();
    let r = radio.set_vox(!orig_vox).await;
    match r {
        Ok(()) => {
            let rb = radio.get_vox().await.unwrap();
            if rb == !orig_vox {
                let _ = radio.set_vox(orig_vox).await;
                println!("  {:<25} OK", "SetVox");
            } else {
                let _ = radio.set_vox(orig_vox).await;
                println!("  {:<25} FAIL (readback mismatch)", "SetVox");
            }
        }
        Err(e) => println!("  {:<25} WRITE_FAILED: {e}", "SetVox"),
    }

    // VOX gain
    let orig_vg = radio.get_vox_gain().await.unwrap();
    let new_vg = if orig_vg == 3 { 6 } else { 3 };
    match radio.set_vox_gain(new_vg).await {
        Ok(()) => {
            let rb = radio.get_vox_gain().await.unwrap();
            let _ = radio.set_vox_gain(orig_vg).await;
            if rb == new_vg { println!("  {:<25} OK", "SetVoxGain"); }
            else { println!("  {:<25} FAIL (readback: {rb})", "SetVoxGain"); }
        }
        Err(e) => println!("  {:<25} WRITE_FAILED: {e}", "SetVoxGain"),
    }

    // VOX delay
    let orig_vd = radio.get_vox_delay().await.unwrap();
    let new_vd = if orig_vd == 2 { 5 } else { 2 };
    match radio.set_vox_delay(new_vd).await {
        Ok(()) => {
            let rb = radio.get_vox_delay().await.unwrap();
            let _ = radio.set_vox_delay(orig_vd).await;
            if rb == new_vd { println!("  {:<25} OK", "SetVoxDelay"); }
            else { println!("  {:<25} FAIL (readback: {rb})", "SetVoxDelay"); }
        }
        Err(e) => println!("  {:<25} WRITE_FAILED: {e}", "SetVoxDelay"),
    }

    // Power level
    let orig_pc = radio.get_power_level(Band::A).await.unwrap();
    let new_pc = if orig_pc == PowerLevel::High { PowerLevel::Low } else { PowerLevel::High };
    match radio.set_power_level(Band::A, new_pc).await {
        Ok(()) => {
            let rb = radio.get_power_level(Band::A).await.unwrap();
            let _ = radio.set_power_level(Band::A, orig_pc).await;
            if rb == new_pc { println!("  {:<25} OK", "SetPowerLevel"); }
            else { println!("  {:<25} FAIL (readback: {rb:?})", "SetPowerLevel"); }
        }
        Err(e) => println!("  {:<25} WRITE_FAILED: {e}", "SetPowerLevel"),
    }

    // Attenuator
    let orig_ra = radio.get_attenuator(Band::A).await.unwrap();
    match radio.set_attenuator(Band::A, !orig_ra).await {
        Ok(()) => {
            let rb = radio.get_attenuator(Band::A).await.unwrap();
            let _ = radio.set_attenuator(Band::A, orig_ra).await;
            if rb == !orig_ra { println!("  {:<25} OK", "SetAttenuator"); }
            else { println!("  {:<25} FAIL", "SetAttenuator"); }
        }
        Err(e) => println!("  {:<25} WRITE_FAILED: {e}", "SetAttenuator"),
    }

    // Dual band
    let orig_dl = radio.get_dual_band().await.unwrap();
    match radio.set_dual_band(!orig_dl).await {
        Ok(()) => {
            let rb = radio.get_dual_band().await.unwrap();
            let _ = radio.set_dual_band(orig_dl).await;
            if rb == !orig_dl { println!("  {:<25} OK", "SetDualBand"); }
            else { println!("  {:<25} FAIL", "SetDualBand"); }
        }
        Err(e) => println!("  {:<25} WRITE_FAILED: {e}", "SetDualBand"),
    }

    // Lock
    let orig_lc = radio.get_lock().await.unwrap();
    match radio.set_lock(!orig_lc).await {
        Ok(()) => {
            let rb = radio.get_lock().await.unwrap();
            let _ = radio.set_lock(orig_lc).await;
            if rb == !orig_lc { println!("  {:<25} OK", "SetLock"); }
            else { println!("  {:<25} FAIL", "SetLock"); }
        }
        Err(e) => println!("  {:<25} WRITE_FAILED: {e}", "SetLock"),
    }

    // Bluetooth
    let orig_bt = radio.get_bluetooth().await.unwrap();
    match radio.set_bluetooth(!orig_bt).await {
        Ok(()) => {
            let rb = radio.get_bluetooth().await.unwrap();
            let _ = radio.set_bluetooth(orig_bt).await;
            if rb == !orig_bt { println!("  {:<25} OK", "SetBluetooth"); }
            else { println!("  {:<25} FAIL", "SetBluetooth"); }
        }
        Err(e) => println!("  {:<25} WRITE_FAILED: {e}", "SetBluetooth"),
    }

    // Squelch
    let orig_sq = radio.get_squelch(Band::A).await.unwrap();
    let new_sq = if orig_sq == 3 { 5 } else { 3 };
    match radio.set_squelch(Band::A, new_sq).await {
        Ok(()) => {
            let rb = radio.get_squelch(Band::A).await.unwrap();
            let _ = radio.set_squelch(Band::A, orig_sq).await;
            if rb == new_sq { println!("  {:<25} OK", "SetSquelch"); }
            else { println!("  {:<25} FAIL (readback: {rb})", "SetSquelch"); }
        }
        Err(e) => println!("  {:<25} WRITE_FAILED: {e}", "SetSquelch"),
    }

    // Backlight
    let orig_bl = radio.get_backlight().await.unwrap();
    let new_bl = if orig_bl == 2 { 5 } else { 2 };
    match radio.set_backlight(new_bl).await {
        Ok(()) => {
            let rb = radio.get_backlight().await.unwrap();
            let _ = radio.set_backlight(orig_bl).await;
            if rb == new_bl { println!("  {:<25} OK", "SetBacklight"); }
            else { println!("  {:<25} FAIL (readback: {rb})", "SetBacklight"); }
        }
        Err(e) => println!("  {:<25} WRITE_FAILED: {e}", "SetBacklight"),
    }

    // Mode
    let orig_md = radio.get_mode(Band::A).await.unwrap();
    let new_md = if orig_md == Mode::Fm { Mode::Nfm } else { Mode::Fm };
    match radio.set_mode(Band::A, new_md).await {
        Ok(()) => {
            let rb = radio.get_mode(Band::A).await.unwrap();
            let _ = radio.set_mode(Band::A, orig_md).await;
            if rb == new_md { println!("  {:<25} OK", "SetMode"); }
            else { println!("  {:<25} FAIL (readback: {rb:?})", "SetMode"); }
        }
        Err(e) => println!("  {:<25} WRITE_FAILED: {e}", "SetMode"),
    }

    // Dual watch
    let orig_dw = radio.get_dual_watch().await.unwrap();
    match radio.set_dual_watch(!orig_dw).await {
        Ok(()) => {
            let rb = radio.get_dual_watch().await.unwrap();
            let _ = radio.set_dual_watch(orig_dw).await;
            if rb == !orig_dw { println!("  {:<25} OK", "SetDualWatch"); }
            else { println!("  {:<25} FAIL", "SetDualWatch"); }
        }
        Err(e) => println!("  {:<25} WRITE_FAILED: {e}", "SetDualWatch"),
    }

    // Auto-info
    match radio.set_auto_info(true).await {
        Ok(()) => {
            let _ = radio.set_auto_info(false).await;
            println!("  {:<25} OK", "SetAutoInfo");
        }
        Err(e) => println!("  {:<25} WRITE_FAILED: {e}", "SetAutoInfo"),
    }

    let _ = radio.disconnect().await;
    println!("\n=== DONE ===");
}
