//! Test native Bluetooth RFCOMM transport.

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
