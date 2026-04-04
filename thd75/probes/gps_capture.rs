//! Capture live NMEA sentences from the radio's GPS receiver.
//!
//! Enables GP PC output, captures for 30 seconds, saves to a `.nme` file,
//! then disables PC output. To run, temporarily add to Cargo.toml:
//!
//! ```toml
//! [[bin]]
//! name = "gps_capture"
//! path = "probes/gps_capture.rs"
//! ```
//!
//! Then: `cargo run --manifest-path thd75/Cargo.toml --bin gps_capture`

use kenwood_thd75::protocol::Codec;
use kenwood_thd75::transport::{EitherTransport, SerialTransport, Transport};
use std::io::Write;

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

        // Drain pending data
        let _ = tokio::time::timeout(
            std::time::Duration::from_millis(500),
            transport.read(&mut buf),
        )
        .await;

        // Enable GPS + PC output: GP 1,1
        eprintln!("Enabling GPS PC output (GP 1,1)...");
        let _ = transport.write(b"GP 1,1\r").await;
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        // Enable RMC + GGA sentences: GS 1,0,0,0,1,0
        eprintln!("Enabling GGA+RMC sentences (GS 1,0,0,0,1,0)...");
        let _ = transport.write(b"GS 1,0,0,0,1,0\r").await;
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        // Drain command responses
        while let Ok(Ok(n)) = tokio::time::timeout(
            std::time::Duration::from_millis(300),
            transport.read(&mut buf),
        )
        .await
        {
            if n > 0 {
                codec.feed(&buf[..n]);
                while codec.next_frame().is_some() {}
            }
        }

        // Capture NMEA for 30 seconds
        let duration = std::time::Duration::from_secs(30);
        let out_path = "gps_capture.nme";
        let mut file = std::fs::File::create(out_path).expect("cannot create output file");
        let mut nmea_count = 0u32;
        let mut cat_count = 0u32;

        eprintln!("Capturing NMEA for {duration:?} → {out_path}");

        let deadline = tokio::time::Instant::now() + duration;
        let mut raw_buf = Vec::new();

        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(
                std::time::Duration::from_millis(1000),
                transport.read(&mut buf),
            )
            .await
            {
                Ok(Ok(n)) if n > 0 => {
                    raw_buf.extend_from_slice(&buf[..n]);

                    // Process complete lines
                    while let Some(nl_pos) = raw_buf.iter().position(|&b| b == b'\n') {
                        let line: Vec<u8> = raw_buf.drain(..=nl_pos).collect();
                        let text = String::from_utf8_lossy(&line);
                        let trimmed = text.trim();

                        if trimmed.starts_with("$GP") || trimmed.starts_with("$GN") {
                            writeln!(file, "{trimmed}").expect("write failed");
                            nmea_count += 1;
                            if nmea_count <= 5 {
                                eprintln!("  NMEA: {trimmed}");
                            }
                        } else if !trimmed.is_empty() {
                            cat_count += 1;
                        }
                    }
                }
                _ => {}
            }
        }

        eprintln!("\nCaptured {nmea_count} NMEA sentences, {cat_count} CAT frames");
        eprintln!("Saved to {out_path}");

        // Disable PC output: GP 1,0
        eprintln!("Disabling GPS PC output (GP 1,0)...");
        let _ = transport.write(b"GP 1,0\r").await;
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    });
}
