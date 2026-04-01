//! Full MCP memory dump -- saves radio configuration to a file for analysis.
//!
//! This test reads the complete 500 KB memory image from a connected TH-D75
//! and writes it to `tests/fixtures/memory_dump.bin`. The raw image can then
//! be loaded into a [`MemoryImage`] for offline analysis, or compared against
//! a second dump with the `memory_diff` test to map settings to offsets.
//!
//! **WARNING:** Takes approximately 55 seconds at 9600 baud. The USB
//! connection drops when exiting programming mode -- reconnect the radio
//! before running any further hardware tests.
//!
//! Run:
//! ```text
//! cargo test --test memory_dump -- --ignored --nocapture --test-threads=1
//! ```

use kenwood_thd75::memory::MemoryImage;
use kenwood_thd75::radio::Radio;
use kenwood_thd75::transport::SerialTransport;

/// Connect to the first discovered TH-D75 via USB.
async fn connect() -> Radio<SerialTransport> {
    let ports = SerialTransport::discover_usb().expect("USB discovery failed");
    assert!(!ports.is_empty(), "No TH-D75 found -- connect radio via USB");
    let transport = SerialTransport::open(
        &ports[0].port_name,
        SerialTransport::DEFAULT_BAUD,
    )
    .expect("Failed to open serial port");
    Radio::connect(transport).await.expect("Failed to connect")
}

/// Dump the full 500 KB radio memory to `tests/fixtures/memory_dump.bin`.
///
/// Also parses the dump with [`MemoryImage`] and prints a summary of
/// populated channels, the power-on message, and the model name.
#[tokio::test]
#[ignore = "requires TH-D75 connected via USB; takes ~55 seconds"]
async fn dump_full_memory() {
    let mut radio = connect().await;

    eprintln!("Starting full memory dump (1955 pages at 9600 baud)...");

    let image = radio
        .read_memory_image_with_progress(|current, total| {
            if current % 100 == 0 || current == total - 1 {
                eprintln!("  Progress: {current}/{total} pages");
            }
        })
        .await
        .expect("Memory dump failed");

    // Save raw binary.
    let raw_path = "tests/fixtures/memory_dump.bin";
    std::fs::write(raw_path, &image).expect("Failed to write dump file");
    eprintln!("Saved {} bytes to {raw_path}", image.len());

    // Parse and summarise.
    let mem = MemoryImage::from_raw(image).expect("Invalid memory image");

    let settings = mem.settings();
    eprintln!("Power-on message : {:?}", settings.power_on_message());
    eprintln!("Model name       : {:?}", settings.model_name());

    let channels = mem.channels();
    let populated = channels.count();
    eprintln!("Populated channels: {populated}");

    for i in 0..20u16 {
        if channels.is_used(i) {
            let name = channels.name(i);
            if let Some(ch) = channels.get(i) {
                eprintln!(
                    "  CH {:03}: {:>10.4} MHz -- {}",
                    i,
                    ch.flash.rx_frequency.as_mhz(),
                    name,
                );
            }
        }
    }

    // Print summary of known memory regions.
    eprintln!();
    eprintln!("--- Region overview ---");
    let raw = mem.as_raw();
    print_region_summary("Settings    ", raw, 0x0000, 0x2000);
    print_region_summary("Ch Flags    ", raw, 0x2000, 0x3300);
    print_region_summary("Ch Data     ", raw, 0x4000, 0x10000);
    print_region_summary("Ch Names    ", raw, 0x10000, 0x14B00);
    print_region_summary("APRS        ", raw, 0x15100, 0x2A100);
    print_region_summary("D-STAR      ", raw, 0x2A100, 0x4D100);
    print_region_summary("BT + Tail   ", raw, 0x4D100, raw.len());

    // USB connection drops after programming mode -- disconnect is best-effort.
    let _ = radio.disconnect().await;
}

/// Print a summary line for a memory region: how many bytes are non-0xFF.
fn print_region_summary(label: &str, image: &[u8], start: usize, end: usize) {
    let end = end.min(image.len());
    if start >= end {
        return;
    }
    let region = &image[start..end];
    let non_ff = region.iter().filter(|&&b| b != 0xFF).count();
    let total = region.len();
    eprintln!(
        "  {label} 0x{start:05X}-0x{end:05X} ({total:>6} bytes): \
         {non_ff:>6} non-FF ({:.1}%)",
        if total > 0 {
            non_ff as f64 / total as f64 * 100.0
        } else {
            0.0
        },
    );
}
