//! Hardware probe: Bluetooth RFCOMM reopen after a real link drop.
//!
//! NOT a cargo target (nothing in `probes/` compiles); move to
//! `examples/` or register it before running.
//!
//! Procedure:
//! 1. Pair the radio via Menu 934 and run this program.
//! 2. When prompted, power-cycle the radio (or walk it out of range
//!    and back) to drop the RFCOMM channel.
//! 3. The probe drives `RadioLinkRecovery::recover` and prints every `LinkEvent`.
//!    Success is a `Restored` event followed by a working `FV` query.

use kenwood_thd75::radio::Radio;
use kenwood_thd75::session::{RadioLinkRecovery, ReconnectAttemptLimit, ReconnectPolicy};
use kenwood_thd75::transport::BluetoothTransport;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    rt.block_on(async {
        println!("Opening native Bluetooth RFCOMM (pair via Menu 934 first)...");
        let transport = BluetoothTransport::open(None)?;
        let radio = Radio::new(transport);
        let attempt_limit = ReconnectAttemptLimit::new(8)?;
        let mut recovery = RadioLinkRecovery::new(radio, ReconnectPolicy::default(), attempt_limit);
        let mut events = recovery.events();

        let fw = recovery.radio().get_firmware_version().await?;
        println!("Connected. Firmware: {fw}");
        println!("\n>>> POWER-CYCLE THE RADIO NOW <<<");
        println!("Polling FV until the link drops...\n");

        // Poll until a command fails: that is the link drop.
        loop {
            match recovery.radio().get_firmware_version().await {
                Ok(_) => tokio::time::sleep(std::time::Duration::from_secs(1)).await,
                Err(e) => {
                    println!("Link dropped: {e}");
                    break;
                }
            }
        }

        println!("Healing (watch the events)...");
        let result = recovery.recover().await;

        while let Ok(ev) = events.try_recv() {
            println!("event: {ev:?}");
        }
        result?;

        let fw = recovery.radio().get_firmware_version().await?;
        println!("\nRestored. Firmware: {fw}");
        Ok(())
    })
}
