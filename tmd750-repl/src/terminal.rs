//! Terminal guidance bound to observed CAT state and the selected USB endpoint.

use kenwood_tmd750::DvGatewayMode;
use kenwood_tmd750::memory::MCP_D750_SCHEMA_FIRMWARE;
use kenwood_tmd750::transport::{
    KENWOOD_VID, SerialCandidate, TMD750_MAIN_PID, TMD750_PANEL_PID, discover_serial,
};

/// Physical USB connection used by the selected serial endpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum UsbConnection {
    /// USB-C on the main radio unit.
    MainUnit,
    /// USB-C on the control panel.
    Panel,
    /// Platform enumeration did not identify the selected connection.
    Unknown,
}

impl UsbConnection {
    const fn from_candidate(candidate: &SerialCandidate) -> Self {
        match (candidate.vid, candidate.pid) {
            (Some(KENWOOD_VID), Some(TMD750_MAIN_PID)) => Self::MainUnit,
            (Some(KENWOOD_VID), Some(TMD750_PANEL_PID)) => Self::Panel,
            _ => Self::Unknown,
        }
    }

    const fn routing(self) -> &'static str {
        match self {
            Self::MainUnit => "Menu 986: USB (Main Unit), matching the selected endpoint.",
            Self::Panel => "Menu 986: USB (Panel), matching the selected endpoint.",
            Self::Unknown => concat!(
                "Menu 986: match the physical USB connection used by this command.\n",
                "Select USB (Main Unit) for the main unit, USB (Panel) for the panel."
            ),
        }
    }
}

/// Identify the USB connector behind `path` from enumeration metadata.
///
/// Returns `UsbConnection::Unknown` when enumeration fails or reports no
/// matching path. No serial port is opened or probed.
pub(super) fn connection_for_path(path: &str) -> UsbConnection {
    match discover_serial() {
        Ok(candidates) => candidates
            .iter()
            .find(|candidate| candidate.path == path)
            .map_or(UsbConnection::Unknown, UsbConnection::from_candidate),
        Err(error) => {
            tracing::warn!(%error, "USB routing metadata unavailable; use physical cable location");
            UsbConnection::Unknown
        }
    }
}

/// Return the manual Terminal Mode setup sequence for this USB connection.
pub(super) fn instructions(connection: UsbConnection) -> String {
    format!(
        "Automatic Terminal Mode entry is disabled: this build writes no\n\
         Terminal or routing setting.\n\
         The current MCP schema label is {MCP_D750_SCHEMA_FIRMWARE}, the label\n\
         the registry was extracted against, not a vendor version limit.\n\
         Configure the radio manually, in this order:\n\
         Menu 980: COM+AF In/Out.\n\
         {}\n\
         Menu 651: enter and select your DV Gateway callsign.\n\
         DV Gateway uses Band A; the Menu 610 callsign does not apply.\n\
         Menu 670: Reflector TERM Mode.\n\
         Menus 671 and 672: leave both at DIRECT.\n\
         Menu 650: Terminal Mode last. Wait for the TERM indicator.\n\
         The assigned gateway interface does not accept CAT while active.\n\
         Then rerun the same dstar start command.\n\
         A complete MMDVM GET_VERSION reply is required before gateway setup.",
        connection.routing()
    )
}

/// Build startup guidance for the Gateway state observed over CAT.
///
/// `gateway` is `None` when the GW query failed. The text describes only the
/// endpoint that answered CAT; no other endpoint is opened or probed.
pub(super) fn cat_startup_guidance(
    connection: UsbConnection,
    gateway: Option<DvGatewayMode>,
) -> String {
    match gateway {
        Some(DvGatewayMode::Off) => instructions(connection),
        Some(DvGatewayMode::Terminal) => concat!(
            "Terminal Mode is already selected (GW 2).\n",
            "This endpoint answered CAT, so it is not the active gateway.\n",
            "If another endpoint carries the DV Gateway, select it with --port.\n",
            "No other endpoint was opened and no setting was changed."
        )
        .to_owned(),
        Some(DvGatewayMode::Unqualified(_)) | None => concat!(
            "The GW query failed or returned an unrecognized value.\n",
            "Check the DV Gateway setting on the radio, then rerun this command.\n",
            "No other endpoint was opened and no setting was changed."
        )
        .to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_endpoints_select_only_their_own_routing() {
        for (pid, expected, forbidden) in [
            (TMD750_MAIN_PID, UsbConnection::MainUnit, "USB (Panel)"),
            (TMD750_PANEL_PID, UsbConnection::Panel, "USB (Main Unit)"),
        ] {
            let candidate = SerialCandidate {
                path: "selected-port".to_owned(),
                vid: Some(KENWOOD_VID),
                pid: Some(pid),
            };
            let connection = UsbConnection::from_candidate(&candidate);
            assert_eq!(connection, expected);
            let guidance = instructions(connection);
            assert!(guidance.contains(connection.routing()));
            assert!(!guidance.contains(forbidden));
            assert!(guidance.lines().all(|line| line.chars().count() <= 80));
        }
    }

    #[test]
    fn unknown_usb_identity_does_not_guess_routing() {
        let candidate = SerialCandidate {
            path: "unidentified-port".to_owned(),
            vid: None,
            pid: Some(TMD750_MAIN_PID),
        };
        let connection = UsbConnection::from_candidate(&candidate);
        assert_eq!(connection, UsbConnection::Unknown);
        let guidance = instructions(connection);
        assert!(guidance.contains("match the physical USB connection"));
        assert!(guidance.contains("USB (Main Unit)"));
        assert!(guidance.contains("USB (Panel)"));
    }

    #[test]
    fn gateway_guidance_distinguishes_schema_label_from_vendor_limit_and_cat_recovery() {
        let guidance = instructions(UsbConnection::MainUnit);
        assert!(
            guidance.contains("not a vendor version limit"),
            "declared schema provenance must not become a vendor restriction"
        );
        assert!(
            guidance.contains("this build writes no"),
            "operators must be told that no Terminal or routing setting is written"
        );
        assert!(
            guidance.contains("does not accept CAT while active"),
            "operators must not expect the gateway route to keep answering CAT"
        );
        assert!(
            !guidance.contains("Observed radio:"),
            "static instructions cannot invent a current firmware observation"
        );
    }

    #[test]
    fn observed_terminal_and_unknown_states_never_repeat_manual_setup() {
        for connection in [
            UsbConnection::MainUnit,
            UsbConnection::Panel,
            UsbConnection::Unknown,
        ] {
            for gateway in [
                Some(DvGatewayMode::Terminal),
                Some(DvGatewayMode::Unqualified(1)),
                None,
            ] {
                let guidance = cat_startup_guidance(connection, gateway);
                assert!(
                    guidance.lines().all(|line| line.chars().count() <= 80),
                    "state-aware guidance must remain readable without wide lines"
                );
                for forbidden in [
                    "Configure the radio manually",
                    "Menu 650:",
                    "TERM indicator",
                    "Menu 986:",
                    "USB (Panel)",
                    "USB (Main Unit)",
                ] {
                    assert!(
                        !guidance.contains(forbidden),
                        "observed active or unknown state must not infer {forbidden}"
                    );
                }
            }
            assert_eq!(
                cat_startup_guidance(connection, Some(DvGatewayMode::Off)),
                instructions(connection),
                "observed Off retains the existing endpoint-specific instructions"
            );
        }
    }
}
