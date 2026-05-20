//! Unit tests for the typed `KissCommand` and `KissPort` wrappers.

use proptest as _;
use thiserror as _;

use kiss_tnc::{KissCommand, KissPort};

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
    assert!(KissCommand::Return.is_return());
    assert!(!KissCommand::Data.is_return());
    assert!(!KissCommand::SetHardware.is_return());
}

#[test]
fn kiss_port_new_validates_the_nibble_range() {
    assert_eq!(KissPort::new(0).map(KissPort::get), Some(0));
    assert_eq!(KissPort::new(15).map(KissPort::get), Some(15));
    assert_eq!(KissPort::new(16), None);
    assert_eq!(KissPort::new(255), None);
}

#[test]
fn kiss_port_max_and_th_d75_constants() {
    assert_eq!(KissPort::MAX.get(), 15);
    assert_eq!(KissPort::TH_D75.get(), 0);
    assert_eq!(KissPort::default(), KissPort::TH_D75);
}

#[test]
fn kiss_port_from_type_byte_extracts_the_high_nibble() {
    assert_eq!(KissPort::from_type_byte(0x00).get(), 0);
    assert_eq!(KissPort::from_type_byte(0x5A).get(), 5);
    assert_eq!(KissPort::from_type_byte(0xF3).get(), 15);
}
