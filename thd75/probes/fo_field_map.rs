//! Probe: read ME for stored memory channels and correlate with MCP binary.
//!
//! Run: `cargo run --manifest-path thd75/Cargo.toml --bin fo_field_map`
//!
//! Reads ME for several stored channels, then enters MCP mode and reads
//! the raw binary for those same channels. Byte-by-byte correlation
//! reveals exactly which struct byte maps to which CAT field.

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

async fn cmd_matched(
    t: &mut EitherTransport,
    codec: &mut Codec,
    command: &str,
    expect_prefix: &str,
) -> Option<String> {
    let _ = t.write(format!("{command}\r").as_bytes()).await;
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    let mut buf = [0u8; 4096];
    let mut matched = None;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(std::time::Duration::from_millis(500), t.read(&mut buf)).await {
            Ok(Ok(n)) if n > 0 => {
                codec.feed(&buf[..n]);
                while let Some(frame) = codec.next_frame() {
                    let s = String::from_utf8_lossy(&frame).to_string();
                    if s.starts_with(expect_prefix) {
                        matched = Some(s);
                    }
                }
                if matched.is_some() {
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    if let Ok(Ok(n)) = tokio::time::timeout(
                        std::time::Duration::from_millis(200),
                        t.read(&mut buf),
                    )
                    .await
                    {
                        if n > 0 {
                            codec.feed(&buf[..n]);
                        }
                        while codec.next_frame().is_some() {}
                    }
                    return matched;
                }
            }
            _ => {}
        }
    }
    matched
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

        // Drain
        let mut buf = [0u8; 4096];
        let _ = tokio::time::timeout(
            std::time::Duration::from_millis(500),
            transport.read(&mut buf),
        )
        .await;

        println!("\n=== ME FIELD MAPPING PROBE ===\n");

        // Read ME for channels with unusual byte[8], byte[9], or byte[14]
        println!("--- Reading stored memory channels ---");
        let targets: Vec<u16> = vec![
            // byte[14] = 0x3C (cross-tone channels)
            31, 33, 34, 75, 88,
            // 1100+ range: non-zero byte[8] and byte[9] lower nibble
            1101, 1102, 1103, 1104, 1105, 1106, 1107, 1108, 1109, 1110,
            1131, 1132, 1133, 1134, 1135, 1136,
            // byte[8] non-zero, byte[9] = 0x43/0x75/0x79
            1152, 1158, 1159,
        ];
        for ch in targets {
            let cmd = format!("ME {ch:03}");
            if let Some(r) = cmd_matched(&mut transport, &mut codec, &cmd, "ME ").await {
                // Only print populated channels (not "*" empty)
                if !r.contains(",*") && r.len() > 10 {
                    println!("\n  {r}");
                    if let Some(payload) = r.trim().strip_prefix("ME ") {
                        let fields: Vec<&str> = payload.split(',').collect();
                        println!("  {} fields:", fields.len());
                        for (i, val) in fields.iter().enumerate() {
                            println!("    [{i:2}] = {val}");
                        }
                    }
                }
            }
        }

        // Also read FO for both bands for comparison
        println!("\n--- FO for both bands ---");
        for band in [0u8, 1] {
            if let Some(r) =
                cmd_matched(&mut transport, &mut codec, &format!("FO {band}"), "FO ").await
            {
                println!("  {r}");
            }
        }

        // Read SH, TN for context
        println!("\n--- SH and TN ---");
        for band in [0u8, 1] {
            if let Some(r) =
                cmd_matched(&mut transport, &mut codec, &format!("SH {band}"), "SH ").await
            {
                println!("  {r}");
            }
        }
    });
}
