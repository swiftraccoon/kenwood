//! Serial candidates are ordered TM-D750, JVCKENWOOD, then others.

use kenwood_transport as _;
use mcp_d75_extract as _;
use thiserror as _;
use tokio as _;
use tokio_serial as _;
use tracing as _;

use kenwood_tmd750::transport::{
    KENWOOD_VID, SerialCandidate, TMD750_MAIN_PID, TMD750_PANEL_PID, prioritize,
};

#[test]
fn tmd750_usb_endpoints_are_classified_by_their_exact_product_ids() {
    for pid in [TMD750_MAIN_PID, TMD750_PANEL_PID] {
        let candidate = SerialCandidate {
            path: format!("/dev/cu.usbmodem-{pid:04x}"),
            vid: Some(KENWOOD_VID),
            pid: Some(pid),
        };
        assert!(candidate.is_tmd750(), "PID {pid:#06x} was not recognized");
    }

    let unrelated = SerialCandidate {
        path: "/dev/cu.usbmodem-unrelated".to_owned(),
        vid: Some(KENWOOD_VID),
        pid: Some(0x9031),
    };
    assert!(!unrelated.is_tmd750());
}

#[test]
fn tmd750_ports_come_first_and_order_is_otherwise_stable() {
    let candidates = vec![
        SerialCandidate {
            path: "/dev/cu.usbmodem1".to_owned(),
            vid: Some(0x1234),
            pid: Some(0x0001),
        },
        SerialCandidate {
            path: "/dev/cu.usbmodem2".to_owned(),
            vid: Some(KENWOOD_VID),
            pid: Some(0x9999),
        },
        SerialCandidate {
            path: "/dev/cu.Bluetooth-Incoming-Port".to_owned(),
            vid: None,
            pid: None,
        },
        SerialCandidate {
            path: "/dev/cu.usbmodem3".to_owned(),
            vid: Some(KENWOOD_VID),
            pid: Some(TMD750_PANEL_PID),
        },
        SerialCandidate {
            path: "/dev/cu.usbmodem4".to_owned(),
            vid: Some(KENWOOD_VID),
            pid: Some(TMD750_MAIN_PID),
        },
    ];
    let prioritized = prioritize(candidates);
    let ordered: Vec<&str> = prioritized
        .iter()
        .map(|candidate| candidate.path.as_str())
        .collect();
    assert_eq!(
        ordered,
        vec![
            "/dev/cu.usbmodem3",
            "/dev/cu.usbmodem4",
            "/dev/cu.usbmodem2",
            "/dev/cu.usbmodem1",
            "/dev/cu.Bluetooth-Incoming-Port"
        ]
    );
}
