//! Endpoint selection for a post-exit reconnect, from enumeration alone.
//!
//! Nothing here opens a port or mutates a snapshot. A matching path and USB
//! VID/PID identify the serial service, not the physical radio behind it.

use std::collections::BTreeMap;

use kenwood_tmd750::transport::SerialCandidate;
use thiserror::Error;

/// Whether the pinned endpoint may be reopened from this enumeration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ReconnectDecision {
    /// The exact original path and USB IDs are present without ambiguity.
    Ready(SerialCandidate),
    /// No service with the original USB IDs is currently enumerated.
    AwaitingEndpoint,
    /// The snapshot cannot safely select the explicitly requested endpoint.
    Rejected(ReconnectRejection),
}

/// A terminal selection failure, distinct from temporary endpoint absence.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub(super) enum ReconnectRejection {
    /// The original snapshot did not identify a supported USB endpoint.
    #[error("the original endpoint is not a recognized TM-D750 USB endpoint")]
    UnqualifiedOriginal,
    /// One named serial service has contradictory USB metadata.
    #[error("USB metadata conflicts for serial service {path}")]
    ConflictingMetadata {
        /// A path participating in the conflicting observations.
        path: String,
    },
    /// The selected service or its alias is present with different USB IDs.
    #[error("USB metadata changed for the selected serial service {path}")]
    SelectedEndpointChanged {
        /// The observed path or alias carrying the changed metadata.
        path: String,
    },
    /// More than one distinct serial service has the original USB IDs.
    #[error("multiple serial services have the original USB VID/PID: {paths:?}")]
    AmbiguousEndpoints {
        /// Distinct service paths, sorted for stable diagnostic output.
        paths: Vec<String>,
    },
    /// Matching USB IDs are present only on a path the operator did not select.
    #[error(
        "the selected path is absent; matching USB VID/PID is present at unselected path {path}"
    )]
    DifferentPath {
        /// The unselected path; it must not be opened automatically.
        path: String,
    },
}

/// Classify one fresh enumeration against the pinned endpoint.
///
/// Only temporary absence returns `AwaitingEndpoint`, so only it permits
/// further polling. Changed USB metadata, conflicting aliases, extra same-role
/// services and replacement paths are rejected. A macOS dial-in/callout pair
/// counts as one service, but neither alias may stand in for the pinned path.
///
/// Conflict checks cover the selected service and services carrying its USB
/// IDs. Inconsistent metadata on unrelated serial services is out of scope.
pub(super) fn classify(
    original: &SerialCandidate,
    candidates: &[SerialCandidate],
) -> ReconnectDecision {
    if original.path.is_empty() || !original.is_tmd750() {
        return ReconnectDecision::Rejected(ReconnectRejection::UnqualifiedOriginal);
    }
    if let Some(path) = conflicting_metadata(original, candidates) {
        return ReconnectDecision::Rejected(ReconnectRejection::ConflictingMetadata {
            path: path.to_owned(),
        });
    }
    let selected_service = service_name(&original.path);
    if let Some(observed) = candidates.iter().find(|candidate| {
        service_name(&candidate.path) == selected_service && !same_usb_ids(original, candidate)
    }) {
        return ReconnectDecision::Rejected(ReconnectRejection::SelectedEndpointChanged {
            path: observed.path.clone(),
        });
    }
    let selected = candidates
        .iter()
        .find(|candidate| candidate.path == original.path);
    let matching = crate::deduplicate_serial_aliases(
        candidates
            .iter()
            .filter(|candidate| same_usb_ids(original, candidate))
            .cloned(),
    );
    match matching.as_slice() {
        [] => ReconnectDecision::AwaitingEndpoint,
        [candidate] => selected.map_or_else(
            || {
                ReconnectDecision::Rejected(ReconnectRejection::DifferentPath {
                    path: candidate.path.clone(),
                })
            },
            |selected| ReconnectDecision::Ready(selected.clone()),
        ),
        multiple => {
            let mut paths = multiple
                .iter()
                .map(|candidate| candidate.path.clone())
                .collect::<Vec<_>>();
            paths.sort_unstable();
            ReconnectDecision::Rejected(ReconnectRejection::AmbiguousEndpoints { paths })
        }
    }
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
enum ServiceName<'a> {
    MacOs(&'a str),
    Other(&'a str),
}

/// One serial service as first observed in an enumeration.
///
/// Later duplicate observations never replace `first`; a disagreeing one is
/// recorded in `conflicting_path`.
struct ObservedService<'a> {
    first: &'a SerialCandidate,
    conflicting_path: Option<&'a str>,
    contains_original_ids: bool,
}

fn conflicting_metadata<'a>(
    original: &SerialCandidate,
    candidates: &'a [SerialCandidate],
) -> Option<&'a str> {
    let selected_service = service_name(&original.path);
    let mut services = BTreeMap::new();
    for candidate in candidates {
        let observed = services
            .entry(service_name(&candidate.path))
            .or_insert(ObservedService {
                first: candidate,
                conflicting_path: None,
                contains_original_ids: false,
            });
        observed.contains_original_ids |= same_usb_ids(original, candidate);
        if !same_usb_ids(observed.first, candidate) {
            observed.conflicting_path = Some(&candidate.path);
        }
    }
    services.into_iter().find_map(|(name, observed)| {
        if name == selected_service || observed.contains_original_ids {
            observed.conflicting_path
        } else {
            None
        }
    })
}

fn service_name(path: &str) -> ServiceName<'_> {
    crate::macos_serial_service(path).map_or(ServiceName::Other(path), ServiceName::MacOs)
}

fn same_usb_ids(first: &SerialCandidate, second: &SerialCandidate) -> bool {
    first.vid == second.vid && first.pid == second.pid
}

#[cfg(test)]
mod tests {
    use super::*;
    use kenwood_tmd750::transport::{KENWOOD_VID, TMD750_MAIN_PID, TMD750_PANEL_PID};

    fn endpoint(path: &str, pid: u16) -> SerialCandidate {
        SerialCandidate {
            path: path.to_owned(),
            vid: Some(KENWOOD_VID),
            pid: Some(pid),
        }
    }

    #[test]
    fn exact_selected_endpoint_is_ready_without_consuming_snapshots() {
        let original = endpoint("/dev/cu.usbmodem101", TMD750_MAIN_PID);
        let candidates = vec![original.clone()];
        assert_eq!(
            classify(&original, &candidates),
            ReconnectDecision::Ready(original.clone())
        );
        assert_eq!(candidates, [original]);
    }

    #[test]
    fn missing_target_waits_without_selecting_another_usb_role() {
        let original = endpoint("/dev/cu.usbmodem101", TMD750_MAIN_PID);
        let panel = endpoint("/dev/cu.usbmodem201", TMD750_PANEL_PID);
        let mut foreign = endpoint("/dev/cu.foreign", TMD750_MAIN_PID);
        foreign.vid = Some(0x1234);
        for candidates in [vec![], vec![panel], vec![foreign]] {
            assert_eq!(
                classify(&original, &candidates),
                ReconnectDecision::AwaitingEndpoint
            );
        }
    }

    #[test]
    fn ready_selection_ignores_other_roles_and_unrelated_ports() {
        for (selected_role, other_role) in [
            (TMD750_MAIN_PID, TMD750_PANEL_PID),
            (TMD750_PANEL_PID, TMD750_MAIN_PID),
        ] {
            let original = endpoint("/dev/cu.selected", selected_role);
            let candidates = [
                endpoint("/dev/cu.other-role", other_role),
                SerialCandidate {
                    path: "unrelated".to_owned(),
                    vid: None,
                    pid: None,
                },
                original.clone(),
            ];
            assert_eq!(
                classify(&original, &candidates),
                ReconnectDecision::Ready(original)
            );
        }
    }

    #[test]
    fn replacement_path_and_alias_only_presence_are_rejected() {
        for (selected, replacement) in [
            ("/dev/cu.usbmodem101", "/dev/cu.usbmodem102"),
            ("/dev/cu.usbmodem101", "/dev/tty.usbmodem101"),
            ("/dev/tty.usbmodem101", "/dev/cu.usbmodem101"),
            ("COM7", "COM8"),
            ("COM7", "com7"),
        ] {
            let original = endpoint(selected, TMD750_MAIN_PID);
            let candidates = [endpoint(replacement, TMD750_MAIN_PID)];
            assert_eq!(
                classify(&original, &candidates),
                ReconnectDecision::Rejected(ReconnectRejection::DifferentPath {
                    path: replacement.to_owned(),
                })
            );
        }
    }

    #[test]
    fn matching_aliases_and_identical_duplicates_preserve_the_exact_selection() {
        let callout = endpoint("/dev/cu.usbmodem101", TMD750_MAIN_PID);
        let dialin = endpoint("/dev/tty.usbmodem101", TMD750_MAIN_PID);
        for original in [&callout, &dialin] {
            for candidates in [
                vec![dialin.clone(), callout.clone(), dialin.clone()],
                vec![callout.clone(), callout.clone(), dialin.clone()],
            ] {
                assert_eq!(
                    classify(original, &candidates),
                    ReconnectDecision::Ready(original.clone())
                );
            }
        }
    }

    #[test]
    fn multiple_same_role_services_are_rejected_after_alias_deduplication() {
        let original = endpoint("/dev/cu.usbmodem101", TMD750_MAIN_PID);
        let mut candidates = vec![
            endpoint("/dev/tty.usbmodem202", TMD750_MAIN_PID),
            original.clone(),
            endpoint("/dev/cu.usbmodem202", TMD750_MAIN_PID),
            endpoint("/dev/tty.usbmodem101", TMD750_MAIN_PID),
        ];
        let expected = ReconnectDecision::Rejected(ReconnectRejection::AmbiguousEndpoints {
            paths: vec![
                "/dev/cu.usbmodem101".to_owned(),
                "/dev/cu.usbmodem202".to_owned(),
            ],
        });
        assert_eq!(classify(&original, &candidates), expected);
        candidates.reverse();
        assert_eq!(classify(&original, &candidates), expected);
        let absent_original = endpoint("/dev/cu.absent", TMD750_MAIN_PID);
        assert_eq!(classify(&absent_original, &candidates), expected);
    }

    #[test]
    fn changed_or_missing_selected_usb_metadata_is_rejected() {
        let original = endpoint("/dev/cu.usbmodem101", TMD750_MAIN_PID);
        for (vid, pid) in [
            (Some(KENWOOD_VID), Some(TMD750_PANEL_PID)),
            (Some(0x1234), Some(TMD750_MAIN_PID)),
            (None, Some(TMD750_MAIN_PID)),
            (Some(KENWOOD_VID), None),
            (None, None),
        ] {
            let observed = SerialCandidate {
                path: original.path.clone(),
                vid,
                pid,
            };
            assert_eq!(
                classify(&original, &[observed]),
                ReconnectDecision::Rejected(ReconnectRejection::SelectedEndpointChanged {
                    path: original.path.clone(),
                })
            );
        }
    }

    #[test]
    fn changed_metadata_on_an_alias_is_not_treated_as_temporary_absence() {
        for (selected, alias) in [
            ("/dev/cu.usbmodem101", "/dev/tty.usbmodem101"),
            ("/dev/tty.usbmodem101", "/dev/cu.usbmodem101"),
        ] {
            let original = endpoint(selected, TMD750_MAIN_PID);
            for (vid, pid) in [
                (Some(KENWOOD_VID), Some(TMD750_PANEL_PID)),
                (Some(0x1234), Some(TMD750_MAIN_PID)),
                (None, Some(TMD750_MAIN_PID)),
                (Some(KENWOOD_VID), None),
                (None, None),
            ] {
                let observed = SerialCandidate {
                    path: alias.to_owned(),
                    vid,
                    pid,
                };
                assert_eq!(
                    classify(&original, &[observed]),
                    ReconnectDecision::Rejected(ReconnectRejection::SelectedEndpointChanged {
                        path: alias.to_owned(),
                    })
                );
            }
        }
    }

    #[test]
    fn duplicate_paths_and_aliases_cannot_hide_conflicting_metadata() {
        let original = endpoint("/dev/cu.usbmodem101", TMD750_MAIN_PID);
        for path in [original.path.as_str(), "/dev/tty.usbmodem101"] {
            for (vid, pid) in [
                (Some(KENWOOD_VID), Some(TMD750_PANEL_PID)),
                (None, Some(TMD750_MAIN_PID)),
                (Some(KENWOOD_VID), None),
            ] {
                let conflicting = SerialCandidate {
                    path: path.to_owned(),
                    vid,
                    pid,
                };
                let mut candidates = [original.clone(), conflicting];
                assert!(matches!(
                    classify(&original, &candidates),
                    ReconnectDecision::Rejected(ReconnectRejection::ConflictingMetadata { .. })
                ));
                candidates.reverse();
                assert!(matches!(
                    classify(&original, &candidates),
                    ReconnectDecision::Rejected(ReconnectRejection::ConflictingMetadata { .. })
                ));
            }
        }
    }

    #[test]
    fn unrelated_inconsistent_services_do_not_block_the_selected_endpoint() {
        let original = endpoint("/dev/cu.selected", TMD750_MAIN_PID);
        let panel = endpoint("/dev/cu.other-role", TMD750_PANEL_PID);
        let unknown_alias = SerialCandidate {
            path: "/dev/tty.other-role".to_owned(),
            vid: None,
            pid: None,
        };
        let mut candidates = vec![panel, unknown_alias];
        assert_eq!(
            classify(&original, &candidates),
            ReconnectDecision::AwaitingEndpoint
        );
        candidates.push(original.clone());
        assert_eq!(
            classify(&original, &candidates),
            ReconnectDecision::Ready(original)
        );
    }

    #[test]
    fn contradictions_on_another_same_role_service_are_not_ignored() {
        let original = endpoint("/dev/cu.selected", TMD750_MAIN_PID);
        let other = endpoint("/dev/cu.other-main", TMD750_MAIN_PID);
        let unknown_alias = SerialCandidate {
            path: "/dev/tty.other-main".to_owned(),
            vid: None,
            pid: None,
        };
        let mut candidates = vec![unknown_alias, other.clone(), other];
        for selected_present in [false, true] {
            if selected_present {
                candidates.push(original.clone());
            }
            for _ in 0..2 {
                assert!(matches!(
                    classify(&original, &candidates),
                    ReconnectDecision::Rejected(ReconnectRejection::ConflictingMetadata { .. })
                ));
                candidates.reverse();
            }
        }
    }

    #[test]
    fn unqualified_original_snapshots_are_rejected() {
        for original in [
            endpoint("", TMD750_MAIN_PID),
            endpoint("/dev/cu.other-radio", 0x9023),
            SerialCandidate {
                path: "/dev/cu.unknown".to_owned(),
                vid: None,
                pid: None,
            },
        ] {
            assert_eq!(
                classify(&original, std::slice::from_ref(&original)),
                ReconnectDecision::Rejected(ReconnectRejection::UnqualifiedOriginal)
            );
        }
    }
}
