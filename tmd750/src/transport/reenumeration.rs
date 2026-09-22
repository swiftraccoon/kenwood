//! Selecting a pinned serial endpoint again from a fresh enumeration.
//!
//! Nothing here opens a port. A matching path and USB vendor/product ids
//! identify the serial service, not the physical radio behind it; the caller
//! proves the radio with `ID` after opening.

use std::collections::BTreeMap;

use super::SerialCandidate;

/// Whether the pinned endpoint may be reopened from one enumeration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reenumeration {
    /// The exact original path carries its original USB ids, without ambiguity.
    Ready(SerialCandidate),
    /// No serial service carries the original USB ids; a later enumeration may.
    Absent,
    /// The enumeration cannot safely select the pinned endpoint, and polling
    /// again does not change that.
    Rejected(ReenumerationRejection),
}

/// A terminal selection failure, distinct from temporary absence.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ReenumerationRejection {
    /// The pinned endpoint has an empty path or is not a TM-D750 USB endpoint.
    #[error("the pinned endpoint is not a recognized TM-D750 USB endpoint")]
    UnqualifiedOriginal,
    /// One serial service was enumerated with contradictory USB metadata.
    #[error("USB metadata conflicts for serial service {path}")]
    ConflictingMetadata {
        /// A path participating in the conflicting observations.
        path: String,
    },
    /// The pinned service, or its alias, is present with different USB ids.
    #[error("USB metadata changed for the pinned serial service {path}")]
    SelectedEndpointChanged {
        /// The observed path or alias carrying the changed metadata.
        path: String,
    },
    /// More than one distinct serial service carries the original USB ids.
    #[error("multiple serial services carry the original USB vendor/product ids: {paths:?}")]
    AmbiguousEndpoints {
        /// Distinct service paths, sorted.
        paths: Vec<String>,
    },
    /// The original USB ids are present only on a path that was not pinned.
    #[error("the pinned path is absent; its USB vendor/product ids are present at {path}")]
    DifferentPath {
        /// The unpinned path; it is never opened in place of the pinned one.
        path: String,
    },
}

/// Classify one fresh enumeration against the pinned endpoint.
///
/// Only temporary absence returns [`Reenumeration::Absent`], so only it
/// permits further polling. Changed USB metadata, conflicting aliases, extra
/// same-role services and replacement paths are rejected. A macOS dial-in and
/// callout pair counts as one service, but neither alias stands in for the
/// pinned path.
///
/// Conflict checks cover the pinned service and every service carrying its
/// USB ids; inconsistent metadata on unrelated serial services is ignored.
#[must_use]
pub fn select_pinned(original: &SerialCandidate, candidates: &[SerialCandidate]) -> Reenumeration {
    if original.path.is_empty() || !original.is_tmd750() {
        return Reenumeration::Rejected(ReenumerationRejection::UnqualifiedOriginal);
    }
    if let Some(path) = conflicting_metadata(original, candidates) {
        return Reenumeration::Rejected(ReenumerationRejection::ConflictingMetadata {
            path: path.to_owned(),
        });
    }
    let selected_service = service_name(&original.path);
    if let Some(observed) = candidates.iter().find(|candidate| {
        service_name(&candidate.path) == selected_service && !same_usb_ids(original, candidate)
    }) {
        return Reenumeration::Rejected(ReenumerationRejection::SelectedEndpointChanged {
            path: observed.path.clone(),
        });
    }
    let selected = candidates
        .iter()
        .find(|candidate| candidate.path == original.path);
    let matching = deduplicate_aliases(
        candidates
            .iter()
            .filter(|candidate| same_usb_ids(original, candidate))
            .cloned(),
    );
    match matching.as_slice() {
        [] => Reenumeration::Absent,
        [candidate] => selected.map_or_else(
            || {
                Reenumeration::Rejected(ReenumerationRejection::DifferentPath {
                    path: candidate.path.clone(),
                })
            },
            |selected| Reenumeration::Ready(selected.clone()),
        ),
        multiple => {
            let mut paths = multiple
                .iter()
                .map(|candidate| candidate.path.clone())
                .collect::<Vec<_>>();
            paths.sort_unstable();
            Reenumeration::Rejected(ReenumerationRejection::AmbiguousEndpoints { paths })
        }
    }
}

/// True when `candidates` selects exactly `endpoint` without ambiguity.
#[must_use]
pub fn is_unambiguous(endpoint: &SerialCandidate, candidates: &[SerialCandidate]) -> bool {
    matches!(select_pinned(endpoint, candidates), Reenumeration::Ready(selected) if selected == *endpoint)
}

/// The macOS serial service name shared by a `/dev/cu.` callout path and its
/// `/dev/tty.` dial-in alias, or `None` for any other path.
#[must_use]
pub fn macos_service_name(path: &str) -> Option<&str> {
    path.strip_prefix("/dev/cu.")
        .or_else(|| path.strip_prefix("/dev/tty."))
}

/// Whether two candidates name the same serial service: the same path, or a
/// macOS callout and dial-in pair with equal USB ids.
#[must_use]
pub fn are_aliases(first: &SerialCandidate, second: &SerialCandidate) -> bool {
    if first.path == second.path {
        return true;
    }
    same_usb_ids(first, second)
        && macos_service_name(&first.path)
            .zip(macos_service_name(&second.path))
            .is_some_and(|(first_service, second_service)| first_service == second_service)
}

/// Keep one candidate per serial service, preferring the `/dev/cu.` callout
/// path over its dial-in alias; order otherwise follows the input.
#[must_use]
pub fn deduplicate_aliases(
    candidates: impl IntoIterator<Item = SerialCandidate>,
) -> Vec<SerialCandidate> {
    let mut unique: Vec<SerialCandidate> = Vec::new();
    for candidate in candidates {
        if let Some(existing) = unique
            .iter_mut()
            .find(|existing| are_aliases(existing, &candidate))
        {
            if candidate.path.starts_with("/dev/cu.") && !existing.path.starts_with("/dev/cu.") {
                *existing = candidate;
            }
        } else {
            unique.push(candidate);
        }
    }
    unique
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
    macos_service_name(path).map_or(ServiceName::Other(path), ServiceName::MacOs)
}

fn same_usb_ids(first: &SerialCandidate, second: &SerialCandidate) -> bool {
    first.vid == second.vid && first.pid == second.pid
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::{KENWOOD_VID, TMD750_MAIN_PID, TMD750_PANEL_PID};

    fn endpoint(path: &str, pid: u16) -> SerialCandidate {
        SerialCandidate {
            path: path.to_owned(),
            vid: Some(KENWOOD_VID),
            pid: Some(pid),
        }
    }

    #[test]
    fn exact_pinned_endpoint_is_ready_without_consuming_snapshots() {
        let original = endpoint("/dev/cu.usbmodem101", TMD750_MAIN_PID);
        let candidates = vec![original.clone()];
        assert_eq!(
            select_pinned(&original, &candidates),
            Reenumeration::Ready(original.clone())
        );
        assert!(is_unambiguous(&original, &candidates));
        assert_eq!(candidates, [original]);
    }

    #[test]
    fn missing_target_waits_without_selecting_another_usb_role() {
        let original = endpoint("/dev/cu.usbmodem101", TMD750_MAIN_PID);
        let panel = endpoint("/dev/cu.usbmodem201", TMD750_PANEL_PID);
        let mut foreign = endpoint("/dev/cu.foreign", TMD750_MAIN_PID);
        foreign.vid = Some(0x1234);
        for candidates in [vec![], vec![panel], vec![foreign]] {
            assert_eq!(select_pinned(&original, &candidates), Reenumeration::Absent);
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
                select_pinned(&original, &candidates),
                Reenumeration::Ready(original)
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
                select_pinned(&original, &candidates),
                Reenumeration::Rejected(ReenumerationRejection::DifferentPath {
                    path: replacement.to_owned(),
                })
            );
            assert!(!is_unambiguous(&original, &candidates));
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
                    select_pinned(original, &candidates),
                    Reenumeration::Ready(original.clone())
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
        let expected = Reenumeration::Rejected(ReenumerationRejection::AmbiguousEndpoints {
            paths: vec![
                "/dev/cu.usbmodem101".to_owned(),
                "/dev/cu.usbmodem202".to_owned(),
            ],
        });
        assert_eq!(select_pinned(&original, &candidates), expected);
        candidates.reverse();
        assert_eq!(select_pinned(&original, &candidates), expected);
        let absent_original = endpoint("/dev/cu.absent", TMD750_MAIN_PID);
        assert_eq!(select_pinned(&absent_original, &candidates), expected);
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
                select_pinned(&original, &[observed]),
                Reenumeration::Rejected(ReenumerationRejection::SelectedEndpointChanged {
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
                    select_pinned(&original, &[observed]),
                    Reenumeration::Rejected(ReenumerationRejection::SelectedEndpointChanged {
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
                    select_pinned(&original, &candidates),
                    Reenumeration::Rejected(ReenumerationRejection::ConflictingMetadata { .. })
                ));
                candidates.reverse();
                assert!(matches!(
                    select_pinned(&original, &candidates),
                    Reenumeration::Rejected(ReenumerationRejection::ConflictingMetadata { .. })
                ));
            }
        }
    }

    #[test]
    fn unrelated_inconsistent_services_do_not_block_the_pinned_endpoint() {
        let original = endpoint("/dev/cu.selected", TMD750_MAIN_PID);
        let panel = endpoint("/dev/cu.other-role", TMD750_PANEL_PID);
        let unknown_alias = SerialCandidate {
            path: "/dev/tty.other-role".to_owned(),
            vid: None,
            pid: None,
        };
        let mut candidates = vec![panel, unknown_alias];
        assert_eq!(select_pinned(&original, &candidates), Reenumeration::Absent);
        candidates.push(original.clone());
        assert_eq!(
            select_pinned(&original, &candidates),
            Reenumeration::Ready(original)
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
                    select_pinned(&original, &candidates),
                    Reenumeration::Rejected(ReenumerationRejection::ConflictingMetadata { .. })
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
                select_pinned(&original, std::slice::from_ref(&original)),
                Reenumeration::Rejected(ReenumerationRejection::UnqualifiedOriginal)
            );
        }
    }

    #[test]
    fn alias_deduplication_prefers_the_callout_path_and_keeps_input_order() {
        let dialin = endpoint("/dev/tty.usbmodem101", TMD750_MAIN_PID);
        let callout = endpoint("/dev/cu.usbmodem101", TMD750_MAIN_PID);
        let panel = endpoint("/dev/cu.usbmodem201", TMD750_PANEL_PID);
        let unique = deduplicate_aliases(vec![dialin.clone(), panel.clone(), callout.clone()]);
        assert_eq!(unique, [callout.clone(), panel]);
        assert!(are_aliases(&dialin, &callout));
        assert!(!are_aliases(
            &dialin,
            &endpoint("/dev/cu.usbmodem101", TMD750_PANEL_PID)
        ));
        assert_eq!(
            macos_service_name("/dev/tty.usbmodem101"),
            Some("usbmodem101")
        );
        assert_eq!(macos_service_name("COM7"), None);
    }
}
