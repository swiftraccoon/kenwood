//! Endpoint policy tests use only local metadata, never discovery or helpers.

use std::cell::Cell;
use std::path::PathBuf;

use kenwood_tmd750::transport::{KENWOOD_VID, TMD750_PANEL_PID};

use super::*;

type TestResult = AppResult<()>;

fn usb(path: &str, pid: u16) -> SerialCandidate {
    SerialCandidate {
        path: path.to_owned(),
        vid: Some(KENWOOD_VID),
        pid: Some(pid),
    }
}

#[test]
fn default_requires_one_exact_paired_model_name() -> TestResult {
    let expected: BluetoothAddress = "01:23:45:67:89:AB".parse()?;
    let other: BluetoothAddress = "01:23:45:67:89:AC".parse()?;
    let selected = select_bluetooth(None, [(&other, "TH-D75"), (&expected, "TM-D750")])?;
    assert_eq!(selected.address, expected);
    assert!(selected.helper.is_none());
    for name in ["TH-D75", "tm-d750", "TM-D750 ", "TM-D750-2", ""] {
        assert!(select_bluetooth(None, [(&expected, name)]).is_err());
    }
    assert!(select_bluetooth(None, std::iter::empty()).is_err());
    Ok(())
}

#[test]
fn observed_remote_board_name_is_a_candidate_not_a_model_or_address_override() -> TestResult {
    let radio: BluetoothAddress = "01:23:45:67:89:AB".parse()?;
    let other: BluetoothAddress = "01:23:45:67:89:AC".parse()?;
    let selected = select_bluetooth(None, [(&radio, "stm32mp1-ex5240"), (&other, "TH-D75")])?;
    assert_eq!(selected.address, radio);
    assert!(select_bluetooth(None, [(&radio, "stm32mp1-ex5240"), (&other, "TM-D750")]).is_err());
    for name in [
        "stm32mp1",
        "stm32mp1-ex5241",
        "STM32MP1-EX5240",
        "stm32mp1-ex5240 ",
    ] {
        assert!(select_bluetooth(None, [(&radio, name)]).is_err());
    }
    Ok(())
}

#[test]
fn duplicate_names_or_conflicting_address_records_are_not_selected() -> TestResult {
    let first: BluetoothAddress = "01:23:45:67:89:AB".parse()?;
    let second: BluetoothAddress = "01:23:45:67:89:AC".parse()?;
    for records in [
        [(&first, "TM-D750"), (&second, "TM-D750")],
        [(&first, "TM-D750"), (&first, "TM-D750")],
        [(&first, "TM-D750"), (&first, "other name")],
    ] {
        assert!(select_bluetooth(None, records).is_err());
    }
    Ok(())
}

#[test]
fn explicit_bluetooth_preserves_helper_and_does_not_consume_inventory() -> TestResult {
    let address: BluetoothAddress = "01:23:45:67:89:AB".parse()?;
    let supplied = Endpoint {
        address: address.clone(),
        helper: Some(PathBuf::from("/absolute/helper with spaces")),
    };
    let inspected = Cell::new(0);
    let records = std::iter::once((&address, "unrelated")).inspect(|_| {
        inspected.set(inspected.get() + 1);
    });
    let selected = select_bluetooth(Some(supplied.clone()), records)?;
    assert_eq!(selected.address, supplied.address);
    assert_eq!(selected.helper, supplied.helper);
    assert_eq!(inspected.get(), 0);
    Ok(())
}

#[test]
fn one_callout_is_selected_with_or_without_its_matching_dialin_alias() -> TestResult {
    for pid in [TMD750_MAIN_PID, TMD750_PANEL_PID] {
        let callout = usb("/dev/cu.usbmodem1", pid);
        let dialin = usb("/dev/tty.usbmodem1", pid);
        for candidates in [vec![callout.clone()], vec![dialin, callout.clone()]] {
            assert_eq!(select_control(None, &candidates)?, callout);
        }
    }
    Ok(())
}

#[test]
fn default_prefers_main_only_when_both_connectors_are_unambiguous() -> TestResult {
    let main = usb("/dev/cu.usbmodem1", TMD750_MAIN_PID);
    let panel = usb("/dev/cu.usbmodem2", TMD750_PANEL_PID);
    let candidates = [
        panel.clone(),
        usb("/dev/tty.usbmodem1", TMD750_MAIN_PID),
        main.clone(),
        usb("/dev/tty.usbmodem2", TMD750_PANEL_PID),
    ];
    assert_eq!(select_control(None, &candidates)?, main);
    assert_eq!(select_control(Some(&panel.path), &candidates)?, panel);
    let mut reversed = candidates;
    reversed.reverse();
    assert_eq!(select_control(None, &reversed)?, main);
    Ok(())
}

#[test]
fn no_callout_or_only_unqualified_ports_refuses_default_selection() {
    for candidates in [
        Vec::new(),
        vec![usb("/dev/tty.usbmodem1", TMD750_MAIN_PID)],
        vec![usb("COM7", TMD750_MAIN_PID)],
        vec![SerialCandidate {
            path: "/dev/cu.TM-D750".to_owned(),
            vid: None,
            pid: None,
        }],
        vec![usb("/dev/cu.other", 0x9000)],
    ] {
        assert!(select_control(None, &candidates).is_err());
    }
}

#[test]
fn explicit_usb_preserves_exact_path_and_never_substitutes_an_alias() -> TestResult {
    let callout = usb("/dev/cu.usbmodem1", TMD750_MAIN_PID);
    let dialin = usb("/dev/tty.usbmodem1", TMD750_MAIN_PID);
    let candidates = [callout.clone(), dialin.clone()];
    assert_eq!(select_control(Some(&dialin.path), &candidates)?, dialin);
    assert!(select_control(Some(&dialin.path), &[callout]).is_err());
    for requested in ["", " ", "/dev/cu.usbmodem2", "/dev/cu.TM-D750"] {
        assert!(select_control(Some(requested), &candidates).is_err());
    }
    Ok(())
}

#[test]
fn multiple_services_of_either_connector_block_automatic_selection() {
    let main = usb("/dev/cu.usbmodem1", TMD750_MAIN_PID);
    let panel = usb("/dev/cu.usbmodem2", TMD750_PANEL_PID);
    for duplicate in [
        usb("/dev/cu.usbmodem3", TMD750_MAIN_PID),
        usb("/dev/tty.usbmodem3", TMD750_MAIN_PID),
        usb("/dev/cu.usbmodem3", TMD750_PANEL_PID),
        usb("/dev/tty.usbmodem3", TMD750_PANEL_PID),
    ] {
        assert!(select_control(None, &[main.clone(), panel.clone(), duplicate]).is_err());
    }
    assert!(
        select_control(
            Some(&main.path),
            &[main.clone(), usb("/dev/cu.usbmodem3", TMD750_MAIN_PID)],
        )
        .is_err()
    );
}

#[test]
fn duplicate_selected_paths_and_conflicting_aliases_are_rejected() {
    let main = usb("/dev/cu.usbmodem1", TMD750_MAIN_PID);
    let mut changed_vendor = usb("/dev/tty.usbmodem1", TMD750_MAIN_PID);
    changed_vendor.vid = Some(0x1234);
    for candidates in [
        vec![main.clone(), main.clone()],
        vec![main.clone(), changed_vendor],
        vec![main.clone(), usb("/dev/tty.usbmodem1", TMD750_PANEL_PID)],
        vec![main.clone(), usb(&main.path, TMD750_PANEL_PID)],
    ] {
        assert!(select_control(None, &candidates).is_err());
        assert!(select_control(Some(&main.path), &candidates).is_err());
    }
}

#[test]
fn unrelated_ports_do_not_change_a_unique_control_selection() -> TestResult {
    let main = usb("/dev/cu.usbmodem1", TMD750_MAIN_PID);
    let candidates = [
        SerialCandidate {
            path: "/dev/cu.TM-D750".to_owned(),
            vid: None,
            pid: None,
        },
        usb("/dev/cu.other", 0x9000),
        main.clone(),
    ];
    assert_eq!(select_control(None, &candidates)?, main);
    assert!(select_control(Some("/dev/cu.TM-D750"), &candidates).is_err());
    Ok(())
}

#[test]
fn sticky_cancellation_refuses_endpoint_admission() {
    let cancelled = AtomicBool::new(false);
    assert!(check_cancelled(&cancelled).is_ok());
    cancelled.store(true, Ordering::Relaxed);
    let error = check_cancelled(&cancelled);
    assert!(matches!(error, Err(error) if error.kind() == io::ErrorKind::Interrupted));
}
