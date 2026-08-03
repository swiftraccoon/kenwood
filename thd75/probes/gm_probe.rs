//! Quick probe: does GM have a read response?
mod firmware_guard;

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
    buf: &mut [u8],
    command: &str,
) -> Option<String> {
    let wire = format!("{command}\r");
    if transport.write(wire.as_bytes()).await.is_err() {
        return None;
    }
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
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
    .ok()
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

        let fv_response = send_and_read(&mut transport, &mut codec, &mut buf, "FV").await;
        match &fv_response {
            Some(response) => println!("Firmware identity: {response}"),
            None => println!("Firmware identity: timeout (stock GM will be refused)"),
        }
        let firmware_version = fv_response
            .as_deref()
            .and_then(firmware_guard::parse_fv_frame)
            .map(str::to_owned);

        // Test GM bare read
        match firmware_guard::require_stock_bare_probe("GM", firmware_version.as_deref()) {
            Ok(()) => match send_and_read(&mut transport, &mut codec, &mut buf, "GM").await {
                Some(response) => println!("GM read response: {response}"),
                None => println!("GM read: timeout (no response)"),
            },
            Err(diagnostic) => println!("{diagnostic}"),
        }

        // Test MS bare read
        match send_and_read(&mut transport, &mut codec, &mut buf, "MS").await {
            Some(response) => println!("MS read response: {response}"),
            None => println!("MS read: timeout (no response)"),
        }

        // Test BS 0 read
        match send_and_read(&mut transport, &mut codec, &mut buf, "BS 0").await {
            Some(response) => println!("BS 0 read response: {response}"),
            None => println!("BS 0 read: timeout (no response)"),
        }

        // Test BL read (already works, just verify format)
        match send_and_read(&mut transport, &mut codec, &mut buf, "BL").await {
            Some(response) => println!("BL read response: {response}"),
            None => println!("BL read: timeout (no response)"),
        }
    });
}
