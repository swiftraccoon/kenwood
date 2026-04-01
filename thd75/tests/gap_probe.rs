//! Targeted probe to investigate data we can't read yet.
//! STRICTLY READ-ONLY. No writes.
//!
//! Run: cargo test --test gap_probe -- --ignored --nocapture --test-threads=1

use kenwood_thd75::protocol::Codec;
use kenwood_thd75::transport::{SerialTransport, Transport};

async fn raw_cmd(transport: &mut SerialTransport, cmd: &str) -> Option<String> {
    let wire = format!("{cmd}\r");
    if transport.write(wire.as_bytes()).await.is_err() {
        return None;
    }
    let mut codec = Codec::new();
    let mut buf = [0u8; 4096];
    let result = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            match transport.read(&mut buf).await {
                Ok(0) => return None,
                Ok(n) => {
                    codec.feed(&buf[..n]);
                    if let Some(frame) = codec.next_frame() {
                        return Some(String::from_utf8_lossy(&frame).to_string());
                    }
                }
                Err(_) => return None,
            }
        }
    })
    .await;
    match result {
        Ok(Some(s)) => Some(s),
        _ => None,
    }
}

/// Investigation 1: TN tone settings.
/// TN returns ? — maybe it needs a specific format.
/// The D74 docs say TN returns TNC mode, not tone.
/// Try: bare TN, TN 0, TN 1
/// Also check: what mode are the bands in? If DV mode, tone may not apply.
#[tokio::test]
#[ignore]
async fn investigate_tn() {
    let ports = SerialTransport::discover_usb().unwrap();
    let mut transport =
        SerialTransport::open(&ports[0].port_name, SerialTransport::DEFAULT_BAUD).unwrap();

    println!("\n=== INVESTIGATE TN ===");

    // First check what mode we're in
    let md0 = raw_cmd(&mut transport, "MD 0").await;
    let md1 = raw_cmd(&mut transport, "MD 1").await;
    println!("  Current modes: Band A={:?}, Band B={:?}", md0, md1);

    // Try TN in various formats (all reads)
    let cmds = ["TN", "TN 0", "TN 1", "TN 0,0", "TN 0,1"];
    for cmd in &cmds {
        // SAFETY: TN on D74 is TNC mode set. But TN with just a read
        // parameter should be safe. TN mode,band on D74 is a write.
        // Skip TN 0,0 and TN 0,1 if they might be writes.
        if cmd.contains(',') {
            println!("  {cmd:<25} -> SKIPPED (may be write: mode,band format)");
            continue;
        }
        let resp = raw_cmd(&mut transport, cmd).await;
        println!("  {cmd:<25} -> {:?}", resp);
    }

    let _ = transport.close().await;
}

/// Investigation 2: CS callsign slots.
/// Bare CS returns callsign. CS 0-10 echo index.
/// Maybe CS with a number selects the slot (write!) and bare CS reads it.
/// Let's just try bare CS multiple times and see if it's consistent.
#[tokio::test]
#[ignore]
async fn investigate_cs() {
    let ports = SerialTransport::discover_usb().unwrap();
    let mut transport =
        SerialTransport::open(&ports[0].port_name, SerialTransport::DEFAULT_BAUD).unwrap();

    println!("\n=== INVESTIGATE CS ===");

    // Read bare CS (confirmed safe)
    println!("  Bare CS reads:");
    for i in 0..3 {
        let resp = raw_cmd(&mut transport, "CS").await;
        println!("    CS attempt {i}: {:?}", resp);
    }

    // DC slots gave us callsign data. Let's dump all DC slots fully.
    println!("\n  DC slot details (confirmed safe reads):");
    for slot in 0..=8u8 {
        let cmd = format!("DC {slot}");
        let resp = raw_cmd(&mut transport, &cmd).await;
        match &resp {
            Some(r) if r == "?" || r == "N" => {
                println!("    {cmd}: {}", r);
                if slot > 6 {
                    break;
                }
            }
            Some(r) => {
                let fields: Vec<&str> = r.split(',').collect();
                println!("    {cmd}: {} fields = {:?}", fields.len(), fields);
            }
            None => {
                println!("    {cmd}: TIMEOUT");
                break;
            }
        }
    }

    let _ = transport.close().await;
}

/// Investigation 3: Channel names.
/// ME field 19 is URCALL (always CQCQCQ). The display name isn't there.
/// Let's look at the ME response more carefully — dump ALL 23 fields
/// for a channel we know has a name ("ForestCityPD" on ch 018).
/// Also check if there are fields beyond 22 that we truncated.
#[tokio::test]
#[ignore]
async fn investigate_channel_names() {
    let ports = SerialTransport::discover_usb().unwrap();
    let mut transport =
        SerialTransport::open(&ports[0].port_name, SerialTransport::DEFAULT_BAUD).unwrap();

    println!("\n=== INVESTIGATE CHANNEL NAMES ===");

    // Full raw ME 018 (should be 155.190 "ForestCityPD")
    println!("  Full ME 018 (expecting ForestCityPD):");
    if let Some(resp) = raw_cmd(&mut transport, "ME 018").await {
        let fields: Vec<&str> = resp.split(',').collect();
        println!("    Total fields: {}", fields.len());
        for (i, f) in fields.iter().enumerate() {
            let hex: String = f
                .bytes()
                .map(|b| format!("{b:02X}"))
                .collect::<Vec<_>>()
                .join(" ");
            println!("    [{i:2}] = {f:<20} hex: {hex}");
        }
    }

    // Compare with a VFO read — FO has the name field too
    println!("\n  FO 0 for comparison:");
    if let Some(resp) = raw_cmd(&mut transport, "FO 0").await {
        let fields: Vec<&str> = resp.split(',').collect();
        println!("    Total fields: {}", fields.len());
        for (i, f) in fields.iter().enumerate() {
            let hex: String = f
                .bytes()
                .map(|b| format!("{b:02X}"))
                .collect::<Vec<_>>()
                .join(" ");
            println!("    [{i:2}] = {f:<20} hex: {hex}");
        }
    }

    // Try MN command — some Kenwood radios have MN for memory name
    println!("\n  Trying MN (memory name) command:");
    let resp = raw_cmd(&mut transport, "MN 018").await;
    println!("    MN 018: {:?}", resp);
    let resp = raw_cmd(&mut transport, "MN").await;
    println!("    MN (bare): {:?}", resp);

    // Try MNA — another possible memory name command
    println!("\n  Trying MNA (memory name alt) command:");
    let resp = raw_cmd(&mut transport, "MNA 018").await;
    println!("    MNA 018: {:?}", resp);

    // Try NB — name block?
    println!("\n  Trying NB:");
    let resp = raw_cmd(&mut transport, "NB").await;
    println!("    NB: {:?}", resp);

    // Try NA — name?
    let resp = raw_cmd(&mut transport, "NA").await;
    println!("    NA: {:?}", resp);

    // Try CN — channel name?
    let resp = raw_cmd(&mut transport, "CN 018").await;
    println!("    CN 018: {:?}", resp);

    let _ = transport.close().await;
}

/// Investigation 4: US (user settings).
/// Bare US returns ?. Try US with sub-parameter indices.
/// On some Kenwood radios, US takes a 2-digit settings index.
#[tokio::test]
#[ignore]
async fn investigate_us() {
    let ports = SerialTransport::discover_usb().unwrap();
    let mut transport =
        SerialTransport::open(&ports[0].port_name, SerialTransport::DEFAULT_BAUD).unwrap();

    println!("\n=== INVESTIGATE US ===");

    // Try bare US
    let resp = raw_cmd(&mut transport, "US").await;
    println!("  US (bare): {:?}", resp);

    // Try US with 2-digit indices (read format on some radios)
    // These should be reads if they follow the Kenwood convention
    // of "mnemonic index" = read, "mnemonic index,value" = write
    for i in [0, 1, 2, 3, 4, 5, 10, 20, 30, 40, 50] {
        let cmd = format!("US {i:02}");
        let resp = raw_cmd(&mut transport, &cmd).await;
        match &resp {
            Some(r) if r == "?" || r == "N" => {
                println!("  {cmd}: {r}");
            }
            Some(r) => {
                println!("  {cmd}: {r}");
            }
            None => {
                println!("  {cmd}: TIMEOUT");
                break;
            }
        }
    }

    let _ = transport.close().await;
}

/// Investigation 5: Full AG exploration.
/// AG returns 091 (3-digit). Is there a per-band version?
/// Try AG 0 and AG 1 to see if they're reads or writes on D75.
/// CAUTION: AG with a number MAY set gain. Only try if we can verify.
/// Actually — our deep probe already sent AG bare and got 091.
/// Let's NOT send AG with parameters (risk of muting speaker).
/// Just document what we know.
#[tokio::test]
#[ignore]
async fn investigate_ag() {
    let ports = SerialTransport::discover_usb().unwrap();
    let mut transport =
        SerialTransport::open(&ports[0].port_name, SerialTransport::DEFAULT_BAUD).unwrap();

    println!("\n=== INVESTIGATE AG ===");
    // Safe: bare AG read
    let resp = raw_cmd(&mut transport, "AG").await;
    println!("  AG (bare): {:?}", resp);
    println!("  NOTE: AG with number parameter is a WRITE (sets gain level).");
    println!("  NOT sending AG 0 or AG 1 — would change radio volume.");

    let _ = transport.close().await;
}

/// Investigation 6: BE (beacon) behavior.
/// On our radio it returned N. Is it truly beacon or beep?
/// Check if the radio is in a mode where beep status could be queried.
/// DO NOT send BE — it may transmit a beacon.
#[tokio::test]
#[ignore]
async fn investigate_be() {
    let ports = SerialTransport::discover_usb().unwrap();
    let mut transport =
        SerialTransport::open(&ports[0].port_name, SerialTransport::DEFAULT_BAUD).unwrap();

    println!("\n=== INVESTIGATE BE ===");
    println!("  BE is classified as BEACON SEND (transmit action) on D74.");
    println!("  On our D75, it returned N (not available).");
    println!("  NOT sending BE — risk of unintended transmission.");
    println!("  There is no known safe way to query beep setting via CAT.");

    let _ = transport.close().await;
}
