//! Test native Bluetooth RFCOMM transport.

// Deps visible to every kenwood-thd75 example target but unused here.
// Acknowledged so `unused_crate_dependencies` stays silent without
// weakening the lint.
use aprs as _;
use aprs_is as _;
use ax25_codec as _;
use dstar_gateway_core as _;
use kiss_tnc as _;
use mmdvm as _;
use mmdvm_core as _;
use proptest as _;
use serde_json as _;
use thiserror as _;
use tokio_serial as _;
use tracing as _;

// Used only by the macOS `main` below; on other targets the stub `main` leaves
// them unused, so acknowledge them there to keep `unused_crate_dependencies` quiet.
#[cfg(not(target_os = "macos"))]
use {kenwood_thd75 as _, tokio as _};

#[cfg(target_os = "macos")]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    use kenwood_thd75::BluetoothTransport;
    use kenwood_thd75::Radio;

    for i in 1..=3 {
        println!("=== Attempt {i} ===");
        match BluetoothTransport::open(None) {
            Ok(transport) => {
                println!("  transport opened");
                let mut radio = Radio::connect(transport).await?;
                match radio.identify().await {
                    Ok(info) => println!("  identify: {}", info.model),
                    Err(e) => println!("  identify failed: {e}"),
                }
                radio.disconnect().await?;
                println!("  disconnected");
            }
            Err(e) => println!("  open failed: {e}"),
        }
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("macOS only");
}
