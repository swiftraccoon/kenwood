//! MCP byte offsets used by the qualified `set_*_via_mcp` setters.
//!
//! Every offset here is hardware-verified, and each must stay equal to the
//! generated registry entry named beside it: the registry-pin test below
//! checks every pair, so a regenerated registry cannot silently diverge
//! from these setters (the same discipline as the Menu 650 offset in
//! `terminal_mode`).

use crate::protocol::programming;

/// Key beep enable, `radio.Beep`.
pub(crate) const BEEP: usize = 0x1071;
/// Key beep volume, `radio.BeepVolume`.
pub(crate) const BEEP_VOLUME: usize = 0x1072;
/// VOX enable, `radio.Vox`.
pub(crate) const VOX: usize = 0x101B;
/// Bluetooth on/off, `radio.BluetoothOnOff`.
pub(crate) const BLUETOOTH: usize = 0x1078;
/// FM broadcast radio mode, `radio.FmRadioMode`.
pub(crate) const FM_RADIO_MODE: usize = 0x1040;
/// Analog scan resume method, `radio.ScanResumeAnalog`.
pub(crate) const SCAN_RESUME_ANALOG: usize = 0x100C;
/// Digital scan resume method, `radio.ScanResumeDigital`.
pub(crate) const SCAN_RESUME_DIGITAL: usize = 0x100D;

/// Page index for a pinned setter offset.
#[expect(
    clippy::cast_possible_truncation,
    reason = "every pinned offset sits inside the radio's 500 KB image, so the page index fits \
              u16 trivially; the registry-pin test keeps the offsets inside that image"
)]
pub(crate) const fn page(offset: usize) -> u16 {
    (offset / programming::PAGE_SIZE) as u16
}

/// Byte index within the page for a pinned setter offset.
pub(crate) const fn byte_index(offset: usize) -> usize {
    offset % programming::PAGE_SIZE
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::menu_fields::menu_field;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// Every pinned pair: (generated registry field name, MCP offset).
    const PINNED: &[(&str, usize)] = &[
        ("radio.Beep", BEEP),
        ("radio.BeepVolume", BEEP_VOLUME),
        ("radio.Vox", VOX),
        ("radio.BluetoothOnOff", BLUETOOTH),
        ("radio.FmRadioMode", FM_RADIO_MODE),
        ("radio.ScanResumeAnalog", SCAN_RESUME_ANALOG),
        ("radio.ScanResumeDigital", SCAN_RESUME_DIGITAL),
    ];

    #[test]
    fn registry_pins_every_setter_offset() -> TestResult {
        for (name, offset) in PINNED {
            let field =
                menu_field(name).ok_or_else(|| format!("registry entry missing: {name}"))?;
            assert_eq!(
                field.descriptor.offset, *offset,
                "{name} moved in the generated registry"
            );
        }
        Ok(())
    }

    #[test]
    fn page_and_byte_split_round_trips() {
        for (name, offset) in PINNED {
            let rebuilt = usize::from(page(*offset)) * programming::PAGE_SIZE + byte_index(*offset);
            assert_eq!(rebuilt, *offset, "{name} split drifted");
        }
    }
}
