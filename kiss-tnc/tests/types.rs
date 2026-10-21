//! Unit tests for the typed `KissCommand` and `KissPort` wrappers.

use proptest as _;
use thiserror as _;

use kiss_tnc::{KissCommand, KissDecoder, KissError, KissFrame, KissPort};

#[test]
fn kiss_command_byte_roundtrips_for_every_variant() {
    for command in [
        KissCommand::Data,
        KissCommand::TxDelay,
        KissCommand::Persistence,
        KissCommand::SlotTime,
        KissCommand::TxTail,
        KissCommand::FullDuplex,
        KissCommand::SetHardware,
        KissCommand::Return,
    ] {
        assert_eq!(KissCommand::from_byte(command.as_byte()), Some(command));
        assert_eq!(KissCommand::try_from(u8::from(command)), Ok(command));
    }
}

#[test]
fn kiss_command_from_byte_rejects_unassigned_values() {
    // 0x07..=0x0E are unassigned low nibbles; 0x42 is an arbitrary miss.
    assert_eq!(KissCommand::from_byte(0x07), None);
    assert_eq!(KissCommand::from_byte(0x0E), None);
    assert_eq!(KissCommand::from_byte(0x42), None);
}

#[test]
fn kiss_command_is_return_only_for_return() {
    assert!(
        KissCommand::Return.is_return(),
        "return uses the entire type byte"
    );
    assert!(
        !KissCommand::Data.is_return(),
        "data uses port and command nibbles"
    );
    assert!(
        !KissCommand::SetHardware.is_return(),
        "hardware commands use port and command nibbles",
    );
}

#[test]
fn kiss_port_new_validates_the_nibble_range() {
    assert_eq!(KissPort::new(0).map(KissPort::get), Some(0));
    assert_eq!(KissPort::new(15).map(KissPort::get), Some(15));
    assert_eq!(KissPort::new(16), None);
    assert_eq!(KissPort::new(255), None);
}

#[test]
fn kiss_port_max_and_zero_constants() {
    assert_eq!(KissPort::MAX.get(), 15);
    assert_eq!(u8::from(KissPort::ZERO), 0);
    assert_eq!(KissPort::default(), KissPort::ZERO);
}

#[test]
fn kiss_command_try_from_preserves_rejected_byte() {
    for byte in [0x07, 0x0F, 0x42, 0xF0] {
        assert_eq!(
            KissCommand::try_from(byte),
            Err(KissError::UnknownCommand(byte)),
            "raw command conversion must not mask the input byte",
        );
    }
}

#[test]
fn kiss_port_try_from_validates_both_boundaries() {
    assert_eq!(KissPort::try_from(0), Ok(KissPort::ZERO));
    assert_eq!(KissPort::try_from(15), Ok(KissPort::MAX));
    assert_eq!(KissPort::try_from(16), Err(KissError::InvalidPort(16)));
    assert_eq!(KissPort::try_from(255), Err(KissError::InvalidPort(255)));
}

#[test]
fn public_types_support_cross_thread_ownership_and_borrowing() {
    fn assert_send_sync<T: Send + Sync>() {}

    assert_send_sync::<KissCommand>();
    assert_send_sync::<KissDecoder>();
    assert_send_sync::<KissError>();
    assert_send_sync::<KissFrame>();
    assert_send_sync::<KissPort>();
}

#[test]
fn error_diagnostics_are_copyable_values() {
    fn assert_copy<T: Copy>() {}

    assert_copy::<KissError>();
}

#[test]
fn kiss_port_from_type_byte_extracts_the_high_nibble() {
    assert_eq!(KissPort::from_type_byte(0x00).get(), 0);
    assert_eq!(KissPort::from_type_byte(0x5A).get(), 5);
    assert_eq!(KissPort::from_type_byte(0xF3).get(), 15);
}
