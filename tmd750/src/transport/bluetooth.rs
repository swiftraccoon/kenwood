//! TM-D750 selection among the host's paired Bluetooth devices.
//!
//! [`kenwood_transport`] lists the host's paired devices and opens an RFCOMM
//! endpoint behind its `native-bluetooth` feature. This module holds the model
//! policy that picks the radio from that list; it is generic over the address
//! type, so it needs no Bluetooth dependency.

use std::fmt;

/// Names a paired TM-D750 is listed under.
///
/// On firmware 1.02 the radio advertises `stm32mp1-ex5240`, the name of its
/// system-on-chip, and a host may list it under the alias `TM-D750`. A name
/// nominates a candidate only; identify the radio over CAT before using it.
pub const BLUETOOTH_NAMES: [&str; 2] = ["TM-D750", "stm32mp1-ex5240"];

/// Select the one paired device that [`BLUETOOTH_NAMES`] names exactly.
///
/// `devices` are the host's `(address, name)` records; names compare
/// byte-for-byte, case and spaces included.
///
/// # Errors
///
/// Returns [`BluetoothSelectionError::NoCandidate`] when no record carries a
/// listed name, and [`BluetoothSelectionError::Ambiguous`] when more than one
/// does or the matching address appears in another record.
pub fn select_paired_device<A, N>(
    devices: impl IntoIterator<Item = (A, N)>,
) -> Result<A, BluetoothSelectionError>
where
    A: Clone + PartialEq + fmt::Display,
    N: AsRef<str>,
{
    let devices: Vec<(A, N)> = devices.into_iter().collect();
    let listed = |name: &N| BLUETOOTH_NAMES.contains(&name.as_ref());
    let mut matching = devices.iter().filter(|(_, name)| listed(name));
    let Some((address, _)) = matching.next() else {
        return Err(BluetoothSelectionError::NoCandidate);
    };
    let records = devices
        .iter()
        .filter(|(candidate, _)| candidate == address)
        .count();
    if matching.next().is_some() || records != 1 {
        let candidates = devices
            .iter()
            .filter(|(candidate, name)| listed(name) || candidate == address)
            .map(|(candidate, name)| format!("{:?} ({candidate})", name.as_ref()))
            .collect();
        return Err(BluetoothSelectionError::Ambiguous { candidates });
    }
    Ok(address.clone())
}

/// No single paired device can be selected as the TM-D750.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum BluetoothSelectionError {
    /// No paired device carries one of [`BLUETOOTH_NAMES`].
    #[error("no paired device is named TM-D750 or stm32mp1-ex5240")]
    NoCandidate,
    /// Several records nominate a TM-D750, or its address is listed twice.
    #[error("paired TM-D750 selection is ambiguous: {}", candidates.join(", "))]
    Ambiguous {
        /// Each conflicting record as `"name" (address)`.
        candidates: Vec<String>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn one_exactly_named_record_is_selected() -> TestResult {
        for name in BLUETOOTH_NAMES {
            let selected = select_paired_device([
                ("01:23:45:67:89:AC", "TH-D75"),
                ("01:23:45:67:89:AB", name),
            ])?;
            assert_eq!(selected, "01:23:45:67:89:AB");
        }
        for name in [
            "TH-D75",
            "tm-d750",
            "TM-D750 ",
            "TM-D750-2",
            "",
            "stm32mp1",
            "stm32mp1-ex5241",
            "STM32MP1-EX5240",
            "stm32mp1-ex5240 ",
        ] {
            assert_eq!(
                select_paired_device([("01:23:45:67:89:AB", name)]),
                Err(BluetoothSelectionError::NoCandidate),
                "{name:?}"
            );
        }
        assert_eq!(
            select_paired_device(Vec::<(&str, &str)>::new()),
            Err(BluetoothSelectionError::NoCandidate)
        );
        Ok(())
    }

    #[test]
    fn several_candidates_or_a_repeated_address_are_ambiguous() {
        let first = "01:23:45:67:89:AB";
        let second = "01:23:45:67:89:AC";
        for records in [
            [(first, "stm32mp1-ex5240"), (second, "TM-D750")],
            [(first, "TM-D750"), (first, "TM-D750")],
            [(first, "TM-D750"), (first, "other name")],
        ] {
            let result = select_paired_device(records);
            assert!(
                matches!(&result, Err(BluetoothSelectionError::Ambiguous { candidates })
                    if candidates.len() == 2),
                "{records:?}: {result:?}"
            );
        }
        let message = select_paired_device([(first, "stm32mp1-ex5240"), (second, "TM-D750")])
            .err()
            .map(|error| error.to_string())
            .unwrap_or_default();
        for expected in ["ambiguous", "stm32mp1-ex5240", "TM-D750", first, second] {
            assert!(message.contains(expected), "{message}");
        }
    }
}
