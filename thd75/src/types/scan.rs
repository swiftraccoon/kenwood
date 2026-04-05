//! Scan and band scope types.
//!
//! The TH-D75 supports multiple scan modes including VFO band scan,
//! memory scan, program scan, MHz scan, group link scan, priority scan,
//! call scan, and band scope (spectrum display).
//!
//! Scan resume behavior (how the radio continues scanning after stopping
//! on a signal) is configured via the SR command. The scan range for
//! program scan is configured via the SF command.
//!
//! Per User Manual Chapter 9:
//!
//! - Adjust squelch before scanning; too-low squelch causes immediate stops.
//! - While using CTCSS or DCS, scan stops on any signal but immediately
//!   resumes if the signal lacks the matching tone/code.
//! - Pressing and holding `[PTT]` temporarily stops scan on a non-TX band.
//! - The 1 MHz decimal point blinks on the display during active scanning.
//! - Resume methods have separate settings for analog (Menu No. 130) and
//!   digital DV/DR modes (Menu No. 131). Default: Time for analog, Seek
//!   for digital.
//! - Time-operate restart time: Menu No. 132, 1-10 seconds, default 5.
//! - Carrier-operate restart time: Menu No. 133, 1-10 seconds, default 2.
//!
//! See TH-D75 User Manual, Chapter 9: Scan.

use super::Frequency;

/// Scan resume method — controls how the radio resumes scanning after
/// stopping on an active signal.
///
/// Configured via Menu No. 130 (analog) or Menu No. 131 (digital DV/DR)
/// on the radio, and via the SR CAT command. Default: Time for analog,
/// Seek for digital.
///
/// # Safety warning
/// The SR command with value 0 has been observed to reboot the radio
/// on some firmware versions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScanResumeMethod {
    /// Time-operated: resume scanning after a configurable delay even
    /// if the signal is still present. Default hold time is 5 seconds
    /// (Menu No. 132, range 1-10 seconds).
    TimeOperated,
    /// Carrier-operated: resume scanning when the received signal
    /// drops below the squelch threshold and stays closed for the
    /// configured restart time. Default restart time is 2 seconds
    /// (Menu No. 133, range 1-10 seconds).
    CarrierOperated,
    /// Seek: stop on the first active signal and remain there.
    /// The user must manually resume scanning.
    Seek,
}

impl ScanResumeMethod {
    /// Convert from the SR command's numeric value.
    ///
    /// Returns `None` for unrecognized values.
    #[must_use]
    pub const fn from_raw(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::TimeOperated),
            1 => Some(Self::CarrierOperated),
            2 => Some(Self::Seek),
            _ => None,
        }
    }

    /// Convert to the SR command's numeric value.
    #[must_use]
    pub const fn to_raw(self) -> u8 {
        match self {
            Self::TimeOperated => 0,
            Self::CarrierOperated => 1,
            Self::Seek => 2,
        }
    }
}

impl TryFrom<u8> for ScanResumeMethod {
    type Error = crate::error::ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::from_raw(value).ok_or(crate::error::ValidationError::SettingOutOfRange {
            name: "scan resume method",
            value,
            detail: "must be 0-2",
        })
    }
}

/// Scan type — the different scanning modes available on the TH-D75.
///
/// The active scan type depends on the current radio mode (VFO vs Memory)
/// and the key sequence used to start the scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScanType {
    /// VFO band scan: scans the frequency range stored in Menu No. 100
    /// (Programmable VFO) using the current step size. Started by
    /// pressing and holding `[VFO]` in VFO mode.
    BandScan,
    /// Memory channel scan: scans through all stored memory channels that
    /// are not locked out. Started by pressing and holding `[MR]`.
    /// At least 2 non-locked-out memory channels must contain data.
    /// Program scan memory and the priority channel are excluded.
    /// If the recall method (Menu No. 202) is set to `Current Band`,
    /// only channels in the same frequency band are scanned.
    MemoryScan,
    /// Program scan: scans between two user-defined frequency limits.
    /// There are 50 program scan memories (L0/U0 through L49/U49).
    /// Started by pressing and holding `[VFO]` when the VFO frequency
    /// is within a registered program scan range.
    ProgramScan,
    /// MHz scan: scans within a 1 MHz range around the current frequency.
    /// For example, if tuned to 145.400 MHz, scans 145.000-145.995 MHz.
    /// Started by pressing and holding `[MHz]`.
    MhzScan,
    /// Group link scan: scans memory channels within linked memory groups.
    /// Groups are linked via Menu No. 203. Up to 30 groups can be linked.
    /// Started by pressing and holding `[MHz]` in memory mode.
    /// If no groups are linked, memory scan is executed instead.
    GroupLinkScan,
    /// Priority scan: checks the frequency registered in the priority
    /// channel `[Pri]` every 3 seconds. When that channel is busy, the
    /// radio switches to it. Menu No. 134 enables/disables this.
    /// The priority channel must be on Band B. Not available in
    /// single-band-A mode or when FM radio mode is active.
    PriorityScan,
    /// Call scan: alternates between the current VFO frequency (or memory
    /// channel) and the call channel. Started by pressing and holding
    /// `[CALL]`. The selected memory channel is scanned even if locked out.
    CallScan,
}

/// Visual scan range for the band scope display.
///
/// The band scope shows signal activity around the current center frequency.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VisualScanRange {
    /// Center frequency of the visual scan.
    pub center: Frequency,
    /// Display range around the center.
    pub range: VisualRange,
}

/// Band scope display width.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VisualRange {
    /// +/- 1 MHz around center.
    Narrow,
    /// +/- 2.5 MHz around center.
    Medium,
    /// +/- 5 MHz around center.
    Wide,
}

/// Program scan edges — lower and upper frequency limits for program scan.
///
/// These correspond to the Programmable VFO settings (Menu No. 100),
/// which define the lower and upper tunable frequency boundaries.
/// Program scan sweeps between these two limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProgramScanEdge {
    /// Lower frequency limit.
    pub start: Frequency,
    /// Upper frequency limit.
    pub end: Frequency,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_resume_roundtrip() {
        for raw in 0..=2 {
            let method = ScanResumeMethod::from_raw(raw).unwrap();
            assert_eq!(method.to_raw(), raw);
        }
    }

    #[test]
    fn scan_resume_invalid() {
        assert!(ScanResumeMethod::from_raw(3).is_none());
        assert!(ScanResumeMethod::from_raw(255).is_none());
    }

    #[test]
    fn scan_resume_values() {
        assert_eq!(ScanResumeMethod::TimeOperated.to_raw(), 0);
        assert_eq!(ScanResumeMethod::CarrierOperated.to_raw(), 1);
        assert_eq!(ScanResumeMethod::Seek.to_raw(), 2);
    }

    #[test]
    fn visual_scan_range_construction() {
        let range = VisualScanRange {
            center: Frequency::new(145_000_000),
            range: VisualRange::Medium,
        };
        assert_eq!(range.center.as_hz(), 145_000_000);
        assert_eq!(range.range, VisualRange::Medium);
    }

    #[test]
    fn program_scan_edge_construction() {
        let edge = ProgramScanEdge {
            start: Frequency::new(144_000_000),
            end: Frequency::new(148_000_000),
        };
        assert_eq!(edge.start.as_hz(), 144_000_000);
        assert_eq!(edge.end.as_hz(), 148_000_000);
    }

    #[test]
    fn scan_type_debug() {
        // Ensure all variants are representable and Debug works
        let types = [
            ScanType::BandScan,
            ScanType::MemoryScan,
            ScanType::ProgramScan,
            ScanType::MhzScan,
            ScanType::GroupLinkScan,
            ScanType::PriorityScan,
            ScanType::CallScan,
        ];
        for t in types {
            let _ = format!("{t:?}");
        }
    }
}
