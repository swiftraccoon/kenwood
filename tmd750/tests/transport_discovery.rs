//! Serial candidates are ordered JVCKENWOOD first, otherwise stable.

use kenwood_thd75 as _;
use mcp_d75_extract as _;
use thiserror as _;
use tokio as _;
use tokio_serial as _;
use tracing as _;

use kenwood_tmd750::transport::{KENWOOD_VID, SerialCandidate, prioritize};

#[test]
fn kenwood_ports_come_first_and_order_is_otherwise_stable() {
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
            pid: Some(0x9023),
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
            "/dev/cu.usbmodem2",
            "/dev/cu.usbmodem3",
            "/dev/cu.usbmodem1",
            "/dev/cu.Bluetooth-Incoming-Port"
        ]
    );
}
