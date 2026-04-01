//! Integration tests for the 13 control protocol commands:
//! AI, BY, DL, DW, BE, RX, TX, LC, IO, BL, VD, VG, VX.

use kenwood_thd75::protocol::{self, Command, Response};
use kenwood_thd75::types::*;

// ============================================================================
// AI -- Auto-info (write-only boolean)
// ============================================================================

#[test]
fn serialize_ai_on() {
    assert_eq!(
        protocol::serialize(&Command::SetAutoInfo { enabled: true }),
        b"AI 1\r"
    );
}

#[test]
fn serialize_ai_off() {
    assert_eq!(
        protocol::serialize(&Command::SetAutoInfo { enabled: false }),
        b"AI 0\r"
    );
}

#[test]
fn parse_ai_response_on() {
    match protocol::parse(b"AI 1").unwrap() {
        Response::AutoInfo { enabled } => assert!(enabled),
        other => panic!("expected AutoInfo, got {other:?}"),
    }
}

#[test]
fn parse_ai_response_off() {
    match protocol::parse(b"AI 0").unwrap() {
        Response::AutoInfo { enabled } => assert!(!enabled),
        other => panic!("expected AutoInfo, got {other:?}"),
    }
}

// ============================================================================
// BY -- Busy (read-only, band + boolean)
// ============================================================================

#[test]
fn serialize_by_read() {
    assert_eq!(
        protocol::serialize(&Command::GetBusy { band: Band::A }),
        b"BY 0\r"
    );
}

#[test]
fn parse_by_busy() {
    match protocol::parse(b"BY 0,1").unwrap() {
        Response::Busy { band, busy } => {
            assert_eq!(band, Band::A);
            assert!(busy);
        }
        other => panic!("expected Busy, got {other:?}"),
    }
}

#[test]
fn parse_by_not_busy() {
    match protocol::parse(b"BY 1,0").unwrap() {
        Response::Busy { band, busy } => {
            assert_eq!(band, Band::B);
            assert!(!busy);
        }
        other => panic!("expected Busy, got {other:?}"),
    }
}

// ============================================================================
// DL -- Dual-band display (boolean)
// ============================================================================

#[test]
fn serialize_dl_read() {
    assert_eq!(protocol::serialize(&Command::GetDualBand), b"DL\r");
}

#[test]
fn serialize_dl_on() {
    assert_eq!(
        protocol::serialize(&Command::SetDualBand { enabled: true }),
        b"DL 1\r"
    );
}

#[test]
fn parse_dl_enabled() {
    match protocol::parse(b"DL 1").unwrap() {
        Response::DualBand { enabled } => assert!(enabled),
        other => panic!("expected DualBand, got {other:?}"),
    }
}

#[test]
fn parse_dl_disabled() {
    match protocol::parse(b"DL 0").unwrap() {
        Response::DualBand { enabled } => assert!(!enabled),
        other => panic!("expected DualBand, got {other:?}"),
    }
}

// ============================================================================
// DW -- Dual Watch (boolean)
// ============================================================================

#[test]
fn serialize_dw_read() {
    assert_eq!(protocol::serialize(&Command::GetDualWatch), b"DW\r");
}

#[test]
fn serialize_dw_on() {
    assert_eq!(
        protocol::serialize(&Command::SetDualWatch { enabled: true }),
        b"DW 1\r"
    );
}

#[test]
fn serialize_dw_off() {
    assert_eq!(
        protocol::serialize(&Command::SetDualWatch { enabled: false }),
        b"DW 0\r"
    );
}

#[test]
fn parse_dw_enabled() {
    match protocol::parse(b"DW 1").unwrap() {
        Response::DualWatch { enabled } => assert!(enabled),
        other => panic!("expected DualWatch, got {other:?}"),
    }
}

#[test]
fn parse_dw_disabled() {
    match protocol::parse(b"DW 0").unwrap() {
        Response::DualWatch { enabled } => assert!(!enabled),
        other => panic!("expected DualWatch, got {other:?}"),
    }
}

// ============================================================================
// BE -- Beep (boolean)
// ============================================================================

#[test]
fn serialize_be_read() {
    assert_eq!(protocol::serialize(&Command::GetBeep), b"BE\r");
}

#[test]
fn serialize_be_on() {
    assert_eq!(
        protocol::serialize(&Command::SetBeep { enabled: true }),
        b"BE 1\r"
    );
}

#[test]
fn serialize_be_off() {
    assert_eq!(
        protocol::serialize(&Command::SetBeep { enabled: false }),
        b"BE 0\r"
    );
}

#[test]
fn parse_be_enabled() {
    match protocol::parse(b"BE 1").unwrap() {
        Response::Beep { enabled } => assert!(enabled),
        other => panic!("expected Beep, got {other:?}"),
    }
}

#[test]
fn parse_be_disabled() {
    match protocol::parse(b"BE 0").unwrap() {
        Response::Beep { enabled } => assert!(!enabled),
        other => panic!("expected Beep, got {other:?}"),
    }
}

// ============================================================================
// RX -- Receive (action, band parameter)
// ============================================================================

#[test]
fn serialize_rx() {
    assert_eq!(
        protocol::serialize(&Command::Receive { band: Band::A }),
        b"RX 0\r"
    );
}

#[test]
fn serialize_rx_band_b() {
    assert_eq!(
        protocol::serialize(&Command::Receive { band: Band::B }),
        b"RX 1\r"
    );
}

// ============================================================================
// TX -- Transmit (action, band parameter)
// ============================================================================

#[test]
fn serialize_tx() {
    assert_eq!(
        protocol::serialize(&Command::Transmit { band: Band::A }),
        b"TX 0\r"
    );
}

#[test]
fn serialize_tx_band_b() {
    assert_eq!(
        protocol::serialize(&Command::Transmit { band: Band::B }),
        b"TX 1\r"
    );
}

// ============================================================================
// LC -- Lock control (boolean)
// ============================================================================

#[test]
fn serialize_lc_read() {
    assert_eq!(protocol::serialize(&Command::GetLock), b"LC\r");
}

#[test]
fn serialize_lc_locked() {
    assert_eq!(
        protocol::serialize(&Command::SetLock { locked: true }),
        b"LC 1\r"
    );
}

#[test]
fn serialize_lc_unlocked() {
    assert_eq!(
        protocol::serialize(&Command::SetLock { locked: false }),
        b"LC 0\r"
    );
}

#[test]
fn parse_lc_locked() {
    match protocol::parse(b"LC 1").unwrap() {
        Response::Lock { locked } => assert!(locked),
        other => panic!("expected Lock, got {other:?}"),
    }
}

#[test]
fn parse_lc_unlocked() {
    match protocol::parse(b"LC 0").unwrap() {
        Response::Lock { locked } => assert!(!locked),
        other => panic!("expected Lock, got {other:?}"),
    }
}

// ============================================================================
// IO -- I/O port (read-only, u8 value)
// ============================================================================

#[test]
fn serialize_io_read() {
    assert_eq!(protocol::serialize(&Command::GetIoPort), b"IO\r");
}

#[test]
fn parse_io_response() {
    match protocol::parse(b"IO 0").unwrap() {
        Response::IoPort { value } => assert_eq!(value, 0),
        other => panic!("expected IoPort, got {other:?}"),
    }
}

// ============================================================================
// BL -- Backlight brightness (write uses comma-separated "BL 0,level")
// ============================================================================

#[test]
fn serialize_bl_read() {
    assert_eq!(protocol::serialize(&Command::GetBacklight), b"BL\r");
}

#[test]
fn serialize_bl_write() {
    // D75 firmware handler requires "BL x,y\r" (7 bytes, comma at [4])
    assert_eq!(
        protocol::serialize(&Command::SetBacklight { level: 5 }),
        b"BL 0,5\r"
    );
}

#[test]
fn serialize_bl_write_zero() {
    assert_eq!(
        protocol::serialize(&Command::SetBacklight { level: 0 }),
        b"BL 0,0\r"
    );
}

#[test]
fn parse_bl_response() {
    // Read response is bare level (no comma format)
    match protocol::parse(b"BL 3").unwrap() {
        Response::Backlight { level } => assert_eq!(level, 3),
        other => panic!("expected Backlight, got {other:?}"),
    }
}

// ============================================================================
// VD -- VOX delay (numeric)
// ============================================================================

#[test]
fn serialize_vd_read() {
    assert_eq!(protocol::serialize(&Command::GetVoxDelay), b"VD\r");
}

#[test]
fn serialize_vd_write() {
    assert_eq!(
        protocol::serialize(&Command::SetVoxDelay { delay: 10 }),
        b"VD 10\r"
    );
}

#[test]
fn parse_vd_response() {
    match protocol::parse(b"VD 7").unwrap() {
        Response::VoxDelay { delay } => assert_eq!(delay, 7),
        other => panic!("expected VoxDelay, got {other:?}"),
    }
}

// ============================================================================
// VG -- VOX gain (numeric)
// ============================================================================

#[test]
fn serialize_vg_read() {
    assert_eq!(protocol::serialize(&Command::GetVoxGain), b"VG\r");
}

#[test]
fn serialize_vg_write() {
    assert_eq!(
        protocol::serialize(&Command::SetVoxGain { gain: 4 }),
        b"VG 4\r"
    );
}

#[test]
fn parse_vg_response() {
    match protocol::parse(b"VG 9").unwrap() {
        Response::VoxGain { gain } => assert_eq!(gain, 9),
        other => panic!("expected VoxGain, got {other:?}"),
    }
}

// ============================================================================
// VX -- VOX on/off (boolean)
// ============================================================================

#[test]
fn serialize_vx_read() {
    assert_eq!(protocol::serialize(&Command::GetVox), b"VX\r");
}

#[test]
fn serialize_vx_on() {
    assert_eq!(
        protocol::serialize(&Command::SetVox { enabled: true }),
        b"VX 1\r"
    );
}

#[test]
fn parse_vx_enabled() {
    match protocol::parse(b"VX 1").unwrap() {
        Response::Vox { enabled } => assert!(enabled),
        other => panic!("expected Vox, got {other:?}"),
    }
}

#[test]
fn parse_vx_disabled() {
    match protocol::parse(b"VX 0").unwrap() {
        Response::Vox { enabled } => assert!(!enabled),
        other => panic!("expected Vox, got {other:?}"),
    }
}
