//! Compare two memory dumps to identify which bytes changed.
//!
//! This is the primary tool for mapping radio settings to memory offsets.
//! The workflow is:
//!
//! 1. Dump memory -> save as `tests/fixtures/memory_dump_a.bin`
//! 2. Change **one** setting on the radio (via menu or CAT)
//! 3. Dump memory -> save as `tests/fixtures/memory_dump_b.bin`
//! 4. Run this diff to find the changed bytes
//! 5. Document the offset in `src/memory/settings.rs`
//!
//! Run:
//! ```text
//! cargo test --test memory_diff -- --nocapture
//! ```
//!
//! The diff test works entirely on saved files -- no hardware connection
//! is needed.

use kenwood_thd75::protocol::programming;

/// A single byte difference between two memory dumps.
#[derive(Debug)]
struct ByteDiff {
    /// Absolute byte offset within the memory image.
    offset: usize,
    /// MCP page number (offset / 256).
    page: usize,
    /// Byte position within the page (offset % 256).
    byte_in_page: usize,
    /// Value in dump A (before).
    old: u8,
    /// Value in dump B (after).
    new: u8,
}

/// Classify an offset into a known memory region name.
fn region_for_offset(offset: usize) -> &'static str {
    match offset {
        0x0000..0x2000 => "System settings",
        0x2000..0x3300 => "Channel flags",
        0x3300..0x4000 => "Padding/reserved",
        0x4000..0x10000 => "Channel data",
        0x10000..0x14B00 => "Channel names",
        0x14B00..0x15100 => "Group names / gap",
        0x15100..0x15200 => "APRS status header",
        0x15200..0x2A100 => "APRS messages/settings",
        0x2A100..0x4D100 => "D-STAR repeater/callsign",
        0x4D100.. => "Bluetooth + tail config",
    }
}

/// Compare two memory dumps and report every byte that differs.
///
/// The output groups changes by memory region and shows both the hex
/// and ASCII values (when printable) to aid manual analysis.
#[test]
fn diff_memory_dumps() {
    let path_a = "tests/fixtures/memory_dump_a.bin";
    let path_b = "tests/fixtures/memory_dump_b.bin";

    let dump_a = match std::fs::read(path_a) {
        Ok(data) => data,
        Err(e) => {
            eprintln!("Cannot read {path_a}: {e}");
            eprintln!(
                "To use this test, create two dumps:\n\
                 1. Rename memory_dump.bin -> memory_dump_a.bin\n\
                 2. Change a setting, dump again\n\
                 3. Rename memory_dump.bin -> memory_dump_b.bin"
            );
            return;
        }
    };
    let dump_b = match std::fs::read(path_b) {
        Ok(data) => data,
        Err(e) => {
            eprintln!("Cannot read {path_b}: {e}");
            return;
        }
    };

    assert_eq!(
        dump_a.len(),
        dump_b.len(),
        "Dumps have different sizes ({} vs {})",
        dump_a.len(),
        dump_b.len(),
    );
    assert_eq!(
        dump_a.len(),
        programming::TOTAL_SIZE,
        "Dump size {} does not match expected {} bytes",
        dump_a.len(),
        programming::TOTAL_SIZE,
    );

    // Collect all differing bytes.
    let diffs: Vec<ByteDiff> = dump_a
        .iter()
        .zip(dump_b.iter())
        .enumerate()
        .filter_map(|(i, (&a, &b))| {
            if a != b {
                Some(ByteDiff {
                    offset: i,
                    page: i / programming::PAGE_SIZE,
                    byte_in_page: i % programming::PAGE_SIZE,
                    old: a,
                    new: b,
                })
            } else {
                None
            }
        })
        .collect();

    eprintln!("Compared {} bytes", dump_a.len());
    eprintln!("Found {} changed bytes:", diffs.len());
    eprintln!();

    if diffs.is_empty() {
        eprintln!("The two dumps are identical.");
        return;
    }

    // Group by region for readability.
    let mut current_region = "";
    for d in &diffs {
        let region = region_for_offset(d.offset);
        if region != current_region {
            eprintln!("--- {region} ---");
            current_region = region;
        }
        let old_ch = displayable_char(d.old);
        let new_ch = displayable_char(d.new);
        eprintln!(
            "  0x{:05X} (page 0x{:04X} + 0x{:02X}): \
             0x{:02X} -> 0x{:02X}  ({old_ch} -> {new_ch})",
            d.offset, d.page, d.byte_in_page, d.old, d.new,
        );
    }

    // Print a hex context dump around each changed region.
    eprintln!();
    eprintln!("=== Hex context (16 bytes around each change) ===");
    let mut shown_ranges: Vec<(usize, usize)> = Vec::new();
    for d in &diffs {
        let ctx_start = d.offset.saturating_sub(8) & !0xF; // align to 16
        let ctx_end = (d.offset + 8).min(dump_a.len());
        // Skip if we already showed a range covering this offset.
        if shown_ranges
            .iter()
            .any(|&(s, e)| ctx_start >= s && ctx_end <= e)
        {
            continue;
        }
        shown_ranges.push((ctx_start, ctx_end));
        eprintln!();
        eprintln!(
            "  Offset 0x{:05X} (page 0x{:04X} + 0x{:02X}):",
            d.offset, d.page, d.byte_in_page,
        );
        print_hex_diff(&dump_a, &dump_b, ctx_start, ctx_end);
    }

    // Summary.
    eprintln!();
    eprintln!("=== Summary ===");
    let offset_min = diffs.first().map_or(0, |d| d.offset);
    let offset_max = diffs.last().map_or(0, |d| d.offset);
    eprintln!("  Range: 0x{offset_min:05X} - 0x{offset_max:05X}");
    eprintln!("  Span : {} bytes", offset_max - offset_min + 1);
    eprintln!("  Count: {} changed bytes", diffs.len());
    eprintln!("  Region(s): {}", region_for_offset(offset_min),);
    if region_for_offset(offset_min) != region_for_offset(offset_max) {
        eprintln!("           + {}", region_for_offset(offset_max),);
    }
}

/// Print aligned hex dump showing A and B with markers on changed bytes.
fn print_hex_diff(a: &[u8], b: &[u8], start: usize, end: usize) {
    let aligned_start = start & !0xF;
    let aligned_end = ((end + 15) & !0xF).min(a.len());

    for row_start in (aligned_start..aligned_end).step_by(16) {
        let row_end = (row_start + 16).min(a.len());

        // Dump A line.
        eprint!("    A 0x{row_start:05X}: ");
        for i in row_start..row_end {
            eprint!("{:02X} ", a[i]);
        }
        eprint!(" |");
        for i in row_start..row_end {
            eprint!("{}", displayable_char(a[i]));
        }
        eprintln!("|");

        // Dump B line.
        eprint!("    B 0x{row_start:05X}: ");
        for i in row_start..row_end {
            eprint!("{:02X} ", b[i]);
        }
        eprint!(" |");
        for i in row_start..row_end {
            eprint!("{}", displayable_char(b[i]));
        }
        eprintln!("|");

        // Marker line showing which bytes differ.
        eprint!("               ");
        let mut any_diff = false;
        for i in row_start..row_end {
            if a[i] != b[i] {
                eprint!("^^ ");
                any_diff = true;
            } else {
                eprint!("   ");
            }
        }
        if any_diff {
            eprintln!();
        } else {
            eprintln!("(same)");
        }
    }
}

/// Format a byte as a printable ASCII character, or `.` if not printable.
fn displayable_char(b: u8) -> char {
    if b.is_ascii_graphic() || b == b' ' {
        b as char
    } else {
        '.'
    }
}

/// Hex-dump a specific region from a single dump file.
///
/// Useful for inspecting a region of interest without a diff.
///
/// Usage: set `DUMP_PATH`, `DUMP_OFFSET`, and `DUMP_LEN` environment
/// variables, or just edit the constants below for quick inspection.
#[test]
fn hex_dump_region() {
    let path =
        std::env::var("DUMP_PATH").unwrap_or_else(|_| "tests/fixtures/memory_dump.bin".to_string());
    let offset: usize = std::env::var("DUMP_OFFSET")
        .ok()
        .and_then(|s| {
            s.strip_prefix("0x")
                .or(Some(&s))
                .and_then(|h| usize::from_str_radix(h, 16).ok())
        })
        .unwrap_or(0x11C0); // default: power-on message
    let len: usize = std::env::var("DUMP_LEN")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(64);

    let data = match std::fs::read(&path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("Cannot read {path}: {e}");
            eprintln!("Run memory_dump test first to create the file.");
            return;
        }
    };

    let end = (offset + len).min(data.len());
    let region = region_for_offset(offset);

    eprintln!("Hex dump of {path} at 0x{offset:05X} ({len} bytes) [{region}]");
    eprintln!("  Page 0x{:04X} + 0x{:02X}", offset / 256, offset % 256);
    eprintln!();

    let aligned_start = offset & !0xF;
    for row_start in (aligned_start..end).step_by(16) {
        let row_end = (row_start + 16).min(data.len());
        eprint!("  0x{row_start:05X}: ");
        for i in row_start..row_end {
            eprint!("{:02X} ", data[i]);
        }
        // Pad short rows.
        for _ in row_end..row_start + 16 {
            eprint!("   ");
        }
        eprint!(" |");
        for i in row_start..row_end {
            eprint!("{}", displayable_char(data[i]));
        }
        eprintln!("|");
    }
}
