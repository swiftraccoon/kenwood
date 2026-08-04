//! Firmware collision guard shared by legacy raw probes.

/// Exact firmware identity whose automation ABI repurposes bare `GM` and `GW`.
pub(crate) const AZIMUTH_AUTOMATION_FIRMWARE: &str = "1.03.AZM";

/// Exact stock identities whose bare `GM` and `GW` meanings are established.
pub(crate) const STOCK_FIRMWARE_IDENTITIES: [&str; 2] = ["1.03", "1.03.000"];

/// Extract an exact firmware version from one CAT `FV` response frame.
///
/// A valid frame is `FV ` followed by one non-empty token. Whitespace or an
/// unexpected prefix leaves the firmware undetermined and therefore cannot
/// authorize a colliding stock probe.
pub(crate) fn parse_fv_frame(frame: &str) -> Option<&str> {
    let version = frame.strip_prefix("FV ")?;
    if version.is_empty() || version.bytes().any(|byte| byte.is_ascii_whitespace()) {
        None
    } else {
        Some(version)
    }
}

/// Authorize one stock bare `GM`/`GW` probe only after exact FV discovery.
///
/// Only exact established stock identities are accepted. An automation,
/// unknown, future, or undetermined identity produces a diagnostic suitable
/// for printing directly by a probe.
pub(crate) fn require_stock_bare_probe(
    mnemonic: &str,
    firmware_version: Option<&str>,
) -> Result<(), String> {
    match firmware_version {
        Some(version) if STOCK_FIRMWARE_IDENTITIES.contains(&version) => Ok(()),
        Some(AZIMUTH_AUTOMATION_FIRMWARE) => Err(format!(
            "SKIPPED stock bare {mnemonic}: FV {AZIMUTH_AUTOMATION_FIRMWARE} repurposes {mnemonic} for the Azimuth automation ABI"
        )),
        Some(version) => Err(format!(
            "REFUSED stock bare {mnemonic}: FV {version} is not an established stock identity"
        )),
        None => Err(format!(
            "REFUSED stock bare {mnemonic}: exact FV could not be determined"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_azimuth_identity_is_refused() {
        assert!(require_stock_bare_probe("GM", Some(AZIMUTH_AUTOMATION_FIRMWARE)).is_err());
        assert!(require_stock_bare_probe("GW", Some(AZIMUTH_AUTOMATION_FIRMWARE)).is_err());
    }

    #[test]
    fn only_exact_stock_identities_are_authorized() {
        for version in STOCK_FIRMWARE_IDENTITIES {
            assert!(
                require_stock_bare_probe("GM", Some(version)).is_ok(),
                "established stock identity {version:?} must be authorized"
            );
        }
        for version in ["1.03.AZM2", "1.03.azm", "V1.03.AZM", "1.04"] {
            assert!(
                require_stock_bare_probe("GM", Some(version)).is_err(),
                "unknown identity {version:?} must fail closed"
            );
        }
    }

    #[test]
    fn undetermined_firmware_is_refused() {
        assert!(require_stock_bare_probe("GM", None).is_err());
    }

    #[test]
    fn fv_frame_parser_is_exact() {
        assert_eq!(parse_fv_frame("FV 1.03.AZM"), Some("1.03.AZM"));
        assert_eq!(parse_fv_frame("FV 1.03.AZM2"), Some("1.03.AZM2"));
        assert_eq!(parse_fv_frame("FV"), None);
        assert_eq!(parse_fv_frame("FV 1.03.AZM "), None);
        assert_eq!(parse_fv_frame("ID 1.03.AZM"), None);
    }
}
