//! Hardware probe: Bluetooth RFCOMM reopen after a real link drop.
//!
//! NOT a cargo target (nothing in `probes/` compiles); move to
//! `examples/` or register it before running. IOBluetooth needs the
//! real main thread, which is why this is a standalone `fn main`
//! program rather than a `#[test]`.
//!
//! Procedure:
//! 1. Pair the radio via Menu 934 and run this program.
//! 2. When prompted, power-cycle the radio (or walk it out of range
//!    and back) to drop the RFCOMM channel.
//! 3. The probe drives `RadioSupervisor::heal` through a
//!    `MainThreadBroker` pumped on the main thread and prints every
//!    `LinkEvent`. Success is a `Restored` event followed by a
//!    working `FV` query.

use kenwood_thd75::radio::Radio;
use kenwood_thd75::session::{RadioSupervisor, ReconnectPolicy};
use kenwood_thd75::transport::{BluetoothTransport, MainThreadBroker};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Current-thread runtime so async blocks interleave with broker
    // pumping on the one (main) thread IOBluetooth requires.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    let mut broker = MainThreadBroker::new();
    let handle = broker.handle();

    rt.block_on(async {
        println!("Opening native Bluetooth RFCOMM (pair via Menu 934 first)...");
        let transport = BluetoothTransport::open(None)?.with_broker(handle);
        let radio = Radio::connect(transport).await?;
        let mut sup = RadioSupervisor::new(radio, ReconnectPolicy::default(), 8);
        let mut events = sup.events();

        let fw = sup.radio().get_firmware_version().await?;
        println!("Connected. Firmware: {fw}");
        println!("\n>>> POWER-CYCLE THE RADIO NOW <<<");
        println!("Polling FV until the link drops...\n");

        // Poll until a command fails: that is the link drop.
        loop {
            match sup.radio().get_firmware_version().await {
                Ok(_) => tokio::time::sleep(std::time::Duration::from_secs(1)).await,
                Err(e) => {
                    println!("Link dropped: {e}");
                    break;
                }
            }
        }

        println!("Healing (watch the events)...");
        let heal = sup.heal();
        tokio::pin!(heal);
        // Interleave healing with broker pumping: reopen jobs submitted
        // by the Bluetooth transport run here, on the main thread.
        let result = loop {
            tokio::select! {
                r = &mut heal => break r,
                () = tokio::time::sleep(std::time::Duration::from_millis(50)) => {
                    let _ = broker.pump();
                }
            }
        };

        while let Ok(ev) = events.try_recv() {
            println!("event: {ev:?}");
        }
        result?;

        let fw = sup.radio().get_firmware_version().await?;
        println!("\nRestored. Firmware: {fw}");
        Ok(())
    })
}
