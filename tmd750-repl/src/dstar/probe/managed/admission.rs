//! Endpoint-role checks over enumeration metadata, before any connection.

use kenwood_tmd750::memory::TerminalGatewayRoute;
use kenwood_tmd750::transport::{SerialCandidate, TMD750_MAIN_PID, TMD750_PANEL_PID};

use crate::CommandError;
use crate::mcp::reconnect::endpoint_is_unambiguous;

/// The two USB endpoints a managed probe uses.
///
/// `control` carries CAT and MCP; `modem` carries the diagnostic connection.
/// Matching metadata can still come from two radios.
#[derive(Debug)]
pub(super) struct Endpoints {
    pub(super) control: SerialCandidate,
    pub(super) modem: SerialCandidate,
}

impl Endpoints {
    /// Select both roles from `candidates`.
    ///
    /// Returns `CommandError` unless both paths enumerate unambiguously as
    /// TM-D750 connectors, the two paths and PIDs differ, and the modem PID
    /// matches `route`; a Bluetooth route is rejected outright.
    pub(super) fn admit(
        modem: &SerialCandidate,
        control_path: &str,
        route: TerminalGatewayRoute,
        candidates: &[SerialCandidate],
    ) -> Result<Self, CommandError> {
        let control = super::super::select_endpoint(control_path, candidates.to_vec())?;
        let selected = super::super::select_endpoint(&modem.path, candidates.to_vec())?;
        let route_pid = match route {
            TerminalGatewayRoute::MainUnit => TMD750_MAIN_PID,
            TerminalGatewayRoute::ControlPanel => TMD750_PANEL_PID,
            TerminalGatewayRoute::Bluetooth => {
                return Err(CommandError(
                    "managed diagnostics require a captured USB gateway route; routing is never changed".to_owned(),
                ));
            }
        };
        if selected != *modem
            || selected.path == control.path
            || selected.pid == control.pid
            || selected.pid != Some(route_pid)
            || [&selected, &control]
                .iter()
                .any(|endpoint| !endpoint_is_unambiguous(endpoint, candidates))
        {
            return Err(CommandError(
                "managed diagnostics require two distinct, unambiguous USB connectors with the modem matching the captured gateway route; no alias or substitute is accepted".to_owned(),
            ));
        }
        Ok(Self {
            control,
            modem: selected,
        })
    }
}

#[cfg(test)]
mod tests {
    use kenwood_tmd750::transport::KENWOOD_VID;

    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn candidate(path: &str, pid: u16) -> SerialCandidate {
        SerialCandidate {
            path: path.to_owned(),
            vid: Some(KENWOOD_VID),
            pid: Some(pid),
        }
    }

    #[test]
    fn admission_requires_two_exact_unique_connector_roles() {
        let panel = candidate("/dev/cu.panel", TMD750_PANEL_PID);
        let main = candidate("/dev/cu.main", TMD750_MAIN_PID);
        let route = TerminalGatewayRoute::ControlPanel;
        assert!(
            Endpoints::admit(&panel, &main.path, route, &[panel.clone(), main.clone()]).is_ok()
        );
        assert!(
            Endpoints::admit(&panel, &panel.path, route, std::slice::from_ref(&panel)).is_err()
        );
        assert!(Endpoints::admit(&panel, &main.path, route, std::slice::from_ref(&panel)).is_err());
        assert!(
            Endpoints::admit(
                &panel,
                "/dev/tty.main",
                route,
                &[panel.clone(), main.clone()]
            )
            .is_err()
        );
        let alias = candidate("/dev/tty.main", TMD750_MAIN_PID);
        assert!(
            Endpoints::admit(
                &panel,
                &main.path,
                route,
                &[panel.clone(), main.clone(), alias.clone()]
            )
            .is_ok()
        );
        assert!(Endpoints::admit(&panel, &main.path, route, &[panel.clone(), alias]).is_err());
        let second_main = candidate("/dev/cu.second-main", TMD750_MAIN_PID);
        assert!(
            Endpoints::admit(
                &panel,
                &main.path,
                route,
                &[panel.clone(), main.clone(), second_main]
            )
            .is_err()
        );
        assert!(
            Endpoints::admit(
                &panel,
                &main.path,
                TerminalGatewayRoute::MainUnit,
                &[panel.clone(), main.clone()]
            )
            .is_err()
        );
        assert!(
            Endpoints::admit(
                &panel,
                &main.path,
                TerminalGatewayRoute::Bluetooth,
                &[panel.clone(), main.clone()]
            )
            .is_err()
        );
    }

    #[test]
    fn either_usb_route_retains_exact_roles_with_both_alias_pairs() -> TestResult {
        let panel = candidate("/dev/cu.panel", TMD750_PANEL_PID);
        let main = candidate("/dev/cu.main", TMD750_MAIN_PID);
        let candidates = [
            panel.clone(),
            candidate("/dev/tty.panel", TMD750_PANEL_PID),
            main.clone(),
            candidate("/dev/tty.main", TMD750_MAIN_PID),
        ];
        for (modem, control, route) in [
            (&panel, &main, TerminalGatewayRoute::ControlPanel),
            (&main, &panel, TerminalGatewayRoute::MainUnit),
        ] {
            let admitted = Endpoints::admit(modem, &control.path, route, &candidates)?;
            assert_eq!(admitted.modem, *modem);
            assert_eq!(admitted.control, *control);
        }
        Ok(())
    }

    #[test]
    fn conflicting_alias_metadata_and_repeated_exact_paths_are_rejected() {
        let panel = candidate("/dev/cu.panel", TMD750_PANEL_PID);
        let main = candidate("/dev/cu.main", TMD750_MAIN_PID);
        let mut changed_vendor = main.clone();
        changed_vendor.vid = Some(0xFFFF);
        let snapshots = [
            vec![panel.clone(), main.clone(), main.clone()],
            vec![panel.clone(), main.clone(), panel.clone()],
            vec![panel.clone(), main.clone(), changed_vendor],
            vec![
                panel.clone(),
                main.clone(),
                candidate("/dev/tty.main", TMD750_PANEL_PID),
            ],
            vec![
                panel.clone(),
                main.clone(),
                candidate("/dev/tty.panel", TMD750_MAIN_PID),
            ],
        ];
        for snapshot in snapshots {
            assert!(
                Endpoints::admit(
                    &panel,
                    &main.path,
                    TerminalGatewayRoute::ControlPanel,
                    &snapshot,
                )
                .is_err(),
                "conflicting enumeration admitted: {snapshot:?}"
            );
        }
    }

    #[test]
    fn alias_names_cannot_supply_distinct_control_and_modem_roles() {
        let modem = candidate("/dev/cu.same-service", TMD750_PANEL_PID);
        let control = candidate("/dev/tty.same-service", TMD750_MAIN_PID);
        assert!(
            Endpoints::admit(
                &modem,
                &control.path,
                TerminalGatewayRoute::ControlPanel,
                &[modem.clone(), control.clone()],
            )
            .is_err()
        );
    }

    #[test]
    fn stale_modem_metadata_and_unselected_replacement_paths_are_rejected() {
        let panel = candidate("/dev/cu.panel", TMD750_PANEL_PID);
        let main = candidate("/dev/cu.main", TMD750_MAIN_PID);
        for observed in [
            candidate(&panel.path, TMD750_MAIN_PID),
            candidate("/dev/cu.replacement", TMD750_PANEL_PID),
        ] {
            assert!(
                Endpoints::admit(
                    &panel,
                    &main.path,
                    TerminalGatewayRoute::ControlPanel,
                    &[observed, main.clone()],
                )
                .is_err()
            );
        }
    }
}
