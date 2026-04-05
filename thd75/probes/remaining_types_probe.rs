//! Hardware probe for remaining untyped CAT command parameters.
//!
//! Tests SH, MS, SF, BL, US, TY to determine actual wire formats and value ranges.
//! Run with radio connected via USB: cargo run --bin remaining_types_probe
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
        // Drain stale data
        let _ = tokio::time::timeout(
            std::time::Duration::from_millis(500),
            transport.read(&mut buf),
        )
        .await;

        // === SH (Filter Width) ===
        println!("=== SH (Filter Width) ===");
        println!("SH bare: {}", send_and_read(&mut transport, &mut codec, "SH", &mut buf).await);
        for mode in 0..=2 {
            println!("SH {mode}: {}", send_and_read(&mut transport, &mut codec, &format!("SH {mode}"), &mut buf).await);
        }
        // Try write format
        println!("SH 0,0: {}", send_and_read(&mut transport, &mut codec, "SH 0,0", &mut buf).await);

        // === MS (Position Source) ===
        println!("\n=== MS (Position Source) ===");
        println!("MS bare: {}", send_and_read(&mut transport, &mut codec, "MS", &mut buf).await);

        // === SF (Scan Range) ===
        println!("\n=== SF (Scan Range) ===");
        println!("SF 0: {}", send_and_read(&mut transport, &mut codec, "SF 0", &mut buf).await);
        println!("SF 1: {}", send_and_read(&mut transport, &mut codec, "SF 1", &mut buf).await);
        println!("SF bare: {}", send_and_read(&mut transport, &mut codec, "SF", &mut buf).await);

        // === BL (Battery Level) ===
        println!("\n=== BL (Battery Level) ===");
        println!("BL bare: {}", send_and_read(&mut transport, &mut codec, "BL", &mut buf).await);

        // === IO (verify) ===
        println!("\n=== IO (verify) ===");
        println!("IO bare: {}", send_and_read(&mut transport, &mut codec, "IO", &mut buf).await);

        // === US (User Settings) ===
        println!("\n=== US (User Settings) ===");
        println!("US bare: {}", send_and_read(&mut transport, &mut codec, "US", &mut buf).await);

        // === TY (Radio Type) ===
        println!("\n=== TY (Radio Type) ===");
        println!("TY bare: {}", send_and_read(&mut transport, &mut codec, "TY", &mut buf).await);

        println!("\n=== Probe complete ===");
    });
}
