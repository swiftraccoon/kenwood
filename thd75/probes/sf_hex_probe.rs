//! Probe: does SF use hex or decimal for step values >= 10?
//! Sets step to 50kHz (index 10), reads back, checks if response is "A" or "10".
use kenwood_thd75::protocol::Codec;
use kenwood_thd75::transport::{EitherTransport, SerialTransport, Transport};

fn open_transport() -> (String, EitherTransport) {
    if let Ok(ports) = SerialTransport::discover_usb() {
        if let Some(info) = ports.first() {
            let t = SerialTransport::open(&info.port_name, SerialTransport::DEFAULT_BAUD)
                .expect("USB open failed");
            return (info.port_name.clone(), EitherTransport::Serial(t));
        }
    }
    #[cfg(target_os = "macos")]
    {
        let bt = kenwood_thd75::BluetoothTransport::open(None).expect("BT connect failed");
        ("bluetooth:TH-D75".into(), EitherTransport::Bluetooth(bt))
    }
    #[cfg(not(target_os = "macos"))]
    panic!("No device found");
}

async fn send_and_read(
    transport: &mut EitherTransport,
    codec: &mut Codec,
    cmd: &str,
    buf: &mut [u8],
) -> String {
    let cmd_bytes = format!("{cmd}\r");
    let _ = transport.write(cmd_bytes.as_bytes()).await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    match tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            match transport.read(buf).await {
                Ok(n) if n > 0 => {
                    codec.feed(&buf[..n]);
                    if let Some(frame) = codec.next_frame() {
                        return String::from_utf8_lossy(&frame).to_string();
                    }
                }
                _ => tokio::time::sleep(std::time::Duration::from_millis(50)).await,
            }
        }
    })
    .await
    {
        Ok(resp) => resp,
        Err(_) => "TIMEOUT".to_string(),
    }
}

fn main() {
    let (path, mut transport) = open_transport();
    eprintln!("Connected via: {path}");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let mut codec = Codec::new();
        let mut buf = [0u8; 4096];
        let _ = tokio::time::timeout(
            std::time::Duration::from_millis(500),
            transport.read(&mut buf),
        )
        .await;

        // Read current step to restore later
        let original = send_and_read(&mut transport, &mut codec, "SF 0", &mut buf).await;
        println!("Original SF 0: {original}");

        // Try setting step to 50kHz (index 10) using decimal
        println!("Setting SF 0,10 (decimal): {}", send_and_read(&mut transport, &mut codec, "SF 0,10", &mut buf).await);
        println!("Read back SF 0: {}", send_and_read(&mut transport, &mut codec, "SF 0", &mut buf).await);

        // Try setting step to 50kHz (index 10) using hex A
        println!("Setting SF 0,A (hex): {}", send_and_read(&mut transport, &mut codec, "SF 0,A", &mut buf).await);
        println!("Read back SF 0: {}", send_and_read(&mut transport, &mut codec, "SF 0", &mut buf).await);

        // Try setting step to 100kHz (index 11) using decimal
        println!("Setting SF 0,11 (decimal): {}", send_and_read(&mut transport, &mut codec, "SF 0,11", &mut buf).await);
        println!("Read back SF 0: {}", send_and_read(&mut transport, &mut codec, "SF 0", &mut buf).await);

        // Try setting step to 100kHz (index 11) using hex B
        println!("Setting SF 0,B (hex): {}", send_and_read(&mut transport, &mut codec, "SF 0,B", &mut buf).await);
        println!("Read back SF 0: {}", send_and_read(&mut transport, &mut codec, "SF 0", &mut buf).await);

        // Restore original
        if let Some(orig_val) = original.strip_prefix("SF ") {
            println!("Restoring: SF {orig_val}");
            let _ = send_and_read(&mut transport, &mut codec, &format!("SF {orig_val}"), &mut buf).await;
        }

        println!("\nDone. Check if responses show A/B (hex) or 10/11 (decimal).");
    });
}
