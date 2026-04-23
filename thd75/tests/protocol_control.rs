//! Integration tests for the 13 control protocol commands:
//! AI, BY, DL, DW, BE, RX, TX, LC, IO, BL, VD, VG, VX.

use kenwood_thd75::protocol::{self, Command, Response};
use kenwood_thd75::types::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

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
fn parse_ai_response_on() -> TestResult {
    let r = protocol::parse(b"AI 1")?;
    let Response::AutoInfo { enabled } = r else {
        return Err(format!("expected AutoInfo, got {r:?}").into());
    };
    assert!(enabled);
    Ok(())
}

#[test]
fn parse_ai_response_off() -> TestResult {
    let r = protocol::parse(b"AI 0")?;
    let Response::AutoInfo { enabled } = r else {
        return Err(format!("expected AutoInfo, got {r:?}").into());
    };
    assert!(!enabled);
    Ok(())
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
fn parse_by_busy() -> TestResult {
    let r = protocol::parse(b"BY 0,1")?;
    let Response::Busy { band, busy } = r else {
        return Err(format!("expected Busy, got {r:?}").into());
    };
    assert_eq!(band, Band::A);
    assert!(busy);
    Ok(())
}

#[test]
fn parse_by_not_busy() -> TestResult {
    let r = protocol::parse(b"BY 1,0")?;
    let Response::Busy { band, busy } = r else {
        return Err(format!("expected Busy, got {r:?}").into());
    };
    assert_eq!(band, Band::B);
    assert!(!busy);
    Ok(())
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
fn parse_dl_enabled() -> TestResult {
    let r = protocol::parse(b"DL 1")?;
    let Response::DualBand { enabled } = r else {
        return Err(format!("expected DualBand, got {r:?}").into());
    };
    assert!(enabled);
    Ok(())
}

#[test]
fn parse_dl_disabled() -> TestResult {
    let r = protocol::parse(b"DL 0")?;
    let Response::DualBand { enabled } = r else {
        return Err(format!("expected DualBand, got {r:?}").into());
    };
    assert!(!enabled);
    Ok(())
}

// ============================================================================
// DW -- Frequency Down (step frequency down, counterpart to UP)
// ============================================================================

#[test]
fn serialize_dw_band_a() {
    assert_eq!(
        protocol::serialize(&Command::FrequencyDown { band: Band::A }),
        b"DW 0\r"
    );
}

#[test]
fn serialize_dw_band_b() {
    assert_eq!(
        protocol::serialize(&Command::FrequencyDown { band: Band::B }),
        b"DW 1\r"
    );
}

#[test]
fn parse_dw_response() -> TestResult {
    let r = protocol::parse(b"DW 0")?;
    let Response::FrequencyDown = r else {
        return Err(format!("expected FrequencyDown, got {r:?}").into());
    };
    Ok(())
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
fn parse_be_enabled() -> TestResult {
    let r = protocol::parse(b"BE 1")?;
    let Response::Beep { enabled } = r else {
        return Err(format!("expected Beep, got {r:?}").into());
    };
    assert!(enabled);
    Ok(())
}

#[test]
fn parse_be_disabled() -> TestResult {
    let r = protocol::parse(b"BE 0")?;
    let Response::Beep { enabled } = r else {
        return Err(format!("expected Beep, got {r:?}").into());
    };
    assert!(!enabled);
    Ok(())
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
fn parse_lc_locked() -> TestResult {
    let r = protocol::parse(b"LC 1")?;
    let Response::Lock { locked } = r else {
        return Err(format!("expected Lock, got {r:?}").into());
    };
    assert!(locked);
    Ok(())
}

#[test]
fn parse_lc_unlocked() -> TestResult {
    let r = protocol::parse(b"LC 0")?;
    let Response::Lock { locked } = r else {
        return Err(format!("expected Lock, got {r:?}").into());
    };
    assert!(!locked);
    Ok(())
}

#[test]
fn serialize_lc_full() -> TestResult {
    assert_eq!(
        protocol::serialize(&Command::SetLockFull {
            locked: true,
            lock_type: KeyLockType::try_from(2)?,
            lock_a: true,
            lock_b: false,
            lock_c: true,
            lock_ptt: false,
        }),
        b"LC 1,2,1,0,1,0\r"
    );
    Ok(())
}

// ============================================================================
// IO -- I/O port (read-only, u8 value)
// ============================================================================

#[test]
fn serialize_io_read() {
    assert_eq!(protocol::serialize(&Command::GetIoPort), b"IO\r");
}

#[test]
fn parse_io_response() -> TestResult {
    let r = protocol::parse(b"IO 0")?;
    let Response::IoPort { value } = r else {
        return Err(format!("expected IoPort, got {r:?}").into());
    };
    assert_eq!(value, DetectOutputMode::Af);
    Ok(())
}

// ============================================================================
// BL -- Battery Level (read-only: 0=Empty, 1=1/3, 2=2/3, 3=Full)
// ============================================================================

#[test]
fn serialize_bl_read() {
    assert_eq!(protocol::serialize(&Command::GetBatteryLevel), b"BL\r");
}

#[test]
fn parse_bl_response() -> TestResult {
    let r = protocol::parse(b"BL 3")?;
    let Response::BatteryLevel { level } = r else {
        return Err(format!("expected BatteryLevel, got {r:?}").into());
    };
    assert_eq!(level, BatteryLevel::Full);
    Ok(())
}

#[test]
fn parse_bl_empty() -> TestResult {
    let r = protocol::parse(b"BL 0")?;
    let Response::BatteryLevel { level } = r else {
        return Err(format!("expected BatteryLevel, got {r:?}").into());
    };
    assert_eq!(level, BatteryLevel::Empty);
    Ok(())
}

#[test]
fn parse_bl_charging() -> TestResult {
    let r = protocol::parse(b"BL 4")?;
    let Response::BatteryLevel { level } = r else {
        return Err(format!("expected BatteryLevel, got {r:?}").into());
    };
    assert_eq!(level, BatteryLevel::Charging);
    Ok(())
}

// ============================================================================
// VD -- VOX delay (numeric)
// ============================================================================

#[test]
fn serialize_vd_read() {
    assert_eq!(protocol::serialize(&Command::GetVoxDelay), b"VD\r");
}

#[test]
fn serialize_vd_write() -> TestResult {
    assert_eq!(
        protocol::serialize(&Command::SetVoxDelay {
            delay: VoxDelay::new(10)?
        }),
        b"VD 10\r"
    );
    Ok(())
}

#[test]
fn parse_vd_response() -> TestResult {
    let r = protocol::parse(b"VD 7")?;
    let Response::VoxDelay { delay } = r else {
        return Err(format!("expected VoxDelay, got {r:?}").into());
    };
    assert_eq!(delay, VoxDelay::new(7)?);
    Ok(())
}

// ============================================================================
// VG -- VOX gain (numeric)
// ============================================================================

#[test]
fn serialize_vg_read() {
    assert_eq!(protocol::serialize(&Command::GetVoxGain), b"VG\r");
}

#[test]
fn serialize_vg_write() -> TestResult {
    assert_eq!(
        protocol::serialize(&Command::SetVoxGain {
            gain: VoxGain::new(4)?
        }),
        b"VG 4\r"
    );
    Ok(())
}

#[test]
fn parse_vg_response() -> TestResult {
    let r = protocol::parse(b"VG 9")?;
    let Response::VoxGain { gain } = r else {
        return Err(format!("expected VoxGain, got {r:?}").into());
    };
    assert_eq!(gain, VoxGain::new(9)?);
    Ok(())
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
fn parse_vx_enabled() -> TestResult {
    let r = protocol::parse(b"VX 1")?;
    let Response::Vox { enabled } = r else {
        return Err(format!("expected Vox, got {r:?}").into());
    };
    assert!(enabled);
    Ok(())
}

#[test]
fn parse_vx_disabled() -> TestResult {
    let r = protocol::parse(b"VX 0")?;
    let Response::Vox { enabled } = r else {
        return Err(format!("expected Vox, got {r:?}").into());
    };
    assert!(!enabled);
    Ok(())
}
