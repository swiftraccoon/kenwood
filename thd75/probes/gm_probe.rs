//! Quick probe: does GM have a read response?
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
        ).await;

        // Test GM bare read
        let _ = transport.write(b"GM\r").await;
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            async {
                loop {
                    match transport.read(&mut buf).await {
                        Ok(n) if n > 0 => {
                            codec.feed(&buf[..n]);
                            if let Some(frame) = codec.next_frame() {
                                return String::from_utf8_lossy(&frame).to_string();
                            }
                        }
                        _ => tokio::time::sleep(std::time::Duration::from_millis(50)).await,
                    }
                }
            },
        ).await;
        match result {
            Ok(resp) => println!("GM read response: {resp}"),
            Err(_) => println!("GM read: timeout (no response)"),
        }

        // Test MS bare read
        let _ = transport.write(b"MS\r").await;
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            async {
                loop {
                    match transport.read(&mut buf).await {
                        Ok(n) if n > 0 => {
                            codec.feed(&buf[..n]);
                            if let Some(frame) = codec.next_frame() {
                                return String::from_utf8_lossy(&frame).to_string();
                            }
                        }
                        _ => tokio::time::sleep(std::time::Duration::from_millis(50)).await,
                    }
                }
            },
        ).await;
        match result {
            Ok(resp) => println!("MS read response: {resp}"),
            Err(_) => println!("MS read: timeout (no response)"),
        }

        // Test BS 0 read
        let _ = transport.write(b"BS 0\r").await;
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            async {
                loop {
                    match transport.read(&mut buf).await {
                        Ok(n) if n > 0 => {
                            codec.feed(&buf[..n]);
                            if let Some(frame) = codec.next_frame() {
                                return String::from_utf8_lossy(&frame).to_string();
                            }
                        }
                        _ => tokio::time::sleep(std::time::Duration::from_millis(50)).await,
                    }
                }
            },
        ).await;
        match result {
            Ok(resp) => println!("BS 0 read response: {resp}"),
            Err(_) => println!("BS 0 read: timeout (no response)"),
        }

        // Test BL read (already works, just verify format)
        let _ = transport.write(b"BL\r").await;
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            async {
                loop {
                    match transport.read(&mut buf).await {
                        Ok(n) if n > 0 => {
                            codec.feed(&buf[..n]);
                            if let Some(frame) = codec.next_frame() {
                                return String::from_utf8_lossy(&frame).to_string();
                            }
                        }
                        _ => tokio::time::sleep(std::time::Duration::from_millis(50)).await,
                    }
                }
            },
        ).await;
        match result {
            Ok(resp) => println!("BL read response: {resp}"),
            Err(_) => println!("BL read: timeout (no response)"),
        }
    });
}
