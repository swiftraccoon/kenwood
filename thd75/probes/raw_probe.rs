//! Raw byte probe — captures exact wire responses for debugging.
//! Run: cargo test --test raw_probe -- --ignored --nocapture --test-threads=1

use kenwood_thd75::protocol::Codec;
use kenwood_thd75::transport::{SerialTransport, Transport};

async fn raw_exchange(cmd: &[u8]) {
    let ports = SerialTransport::discover_usb().unwrap();
    let mut transport =
        SerialTransport::open(&ports[0].port_name, SerialTransport::DEFAULT_BAUD).unwrap();

    let cmd_str = String::from_utf8_lossy(&cmd[..cmd.len() - 1]);
    let _ = transport.write(cmd).await;

    let mut codec = Codec::new();
    let mut buf = [0u8; 4096];

    let timeout = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let n = transport.read(&mut buf).await.unwrap();
            codec.feed(&buf[..n]);
            if let Some(frame) = codec.next_frame() {
                return frame;
            }
        }
    })
    .await;

    match timeout {
        Ok(frame) => {
            let text = String::from_utf8_lossy(&frame);
            let fields: Vec<&str> = text.split(',').collect();
            println!("CMD: {cmd_str}");
            println!("  RAW: {text}");
            println!("  FIELDS ({}):", fields.len());
            for (i, f) in fields.iter().enumerate() {
                println!("    [{i:2}] = {f:?}");
            }
            println!();
        }
        Err(_) => {
            println!("CMD: {cmd_str}");
            println!("  TIMEOUT (no response in 3s)\n");
        }
    }

    let _ = transport.close().await;
}

#[tokio::test]
#[ignore]
async fn probe_me_raw() {
    raw_exchange(b"ME 000\r").await;
    raw_exchange(b"ME 001\r").await;
}

#[tokio::test]
#[ignore]
async fn probe_dc_raw() {
    raw_exchange(b"DC 0\r").await;
    raw_exchange(b"DC 1\r").await;
}

#[tokio::test]
#[ignore]
async fn probe_fq_raw() {
    raw_exchange(b"FQ 0\r").await;
    raw_exchange(b"FQ 1\r").await;
}

#[tokio::test]
#[ignore]
async fn probe_be_raw() {
    raw_exchange(b"BE\r").await;
}
