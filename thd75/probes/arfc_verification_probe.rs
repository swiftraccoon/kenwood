//! Hardware probe to verify ARFC-D75 decompilation findings.
//!
//! Tests: power level range, memory channel modes, unknown ARFC commands
//! (MK, QA, SC, TC, UE, WI, XK, YM, ZO), D7/0B/0L commands, and
//! ToneMode CrossTone (value 3).
//!
//! Run with radio connected: cargo run --bin arfc_verification_probe

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
        // Drain
        let _ = tokio::time::timeout(
            std::time::Duration::from_millis(500),
            transport.read(&mut buf),
        )
        .await;

        // === Power Level Range ===
        println!("=== Power Level (PC) ===");
        println!("PC 0 (read A): {}", send_and_read(&mut transport, &mut codec, "PC 0", &mut buf).await);
        println!("PC 1 (read B): {}", send_and_read(&mut transport, &mut codec, "PC 1", &mut buf).await);
        // Try writing values 4-6 to see if the radio accepts them
        for val in 4..=6 {
            println!("PC 0,{val} (write): {}", send_and_read(&mut transport, &mut codec, &format!("PC 0,{val}"), &mut buf).await);
        }
        // Read back to see what happened
        println!("PC 0 (after): {}", send_and_read(&mut transport, &mut codec, "PC 0", &mut buf).await);

        // === D7 Command ===
        println!("\n=== D7 (D75-specific) ===");
        println!("D7 bare: {}", send_and_read(&mut transport, &mut codec, "D7", &mut buf).await);

        // === 0B and 0L Commands ===
        println!("\n=== 0B / 0L (status commands) ===");
        println!("0B bare: {}", send_and_read(&mut transport, &mut codec, "0B", &mut buf).await);
        println!("0L bare: {}", send_and_read(&mut transport, &mut codec, "0L", &mut buf).await);

        // === Unknown ARFC Commands ===
        println!("\n=== Unknown ARFC Commands ===");
        for cmd in &["MK", "QA", "SC", "TC", "TC 1", "UE", "WI", "XK", "YM", "ZO"] {
            println!("{cmd}: {}", send_and_read(&mut transport, &mut codec, cmd, &mut buf).await);
        }

        // === ToneMode CrossTone ===
        println!("\n=== TN (TNC/Tone mode) ===");
        println!("TN bare: {}", send_and_read(&mut transport, &mut codec, "TN", &mut buf).await);

        // === Memory Channel Mode ===
        println!("\n=== VM (VFO/Memory mode) ===");
        println!("VM 0: {}", send_and_read(&mut transport, &mut codec, "VM 0", &mut buf).await);
        println!("VM 1: {}", send_and_read(&mut transport, &mut codec, "VM 1", &mut buf).await);

        // === DV Gateway Mode ===
        println!("\n=== GW (Gateway mode) ===");
        println!("GW bare: {}", send_and_read(&mut transport, &mut codec, "GW", &mut buf).await);

        println!("\n=== Probe complete ===");
    });
}
