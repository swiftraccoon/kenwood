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
//! See TH-D75 User Manual, Chapter 9: Scan.

use super::Frequency;

/// Scan resume method — controls how the radio resumes scanning after
/// stopping on an active signal.
///
/// Configured via Menu No. 200 on the radio, and via the SR CAT command.
///
/// # Safety warning
/// The SR command with value 0 has been observed to reboot the radio
/// on some firmware versions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScanResumeMethod {
    /// Time-operated: resume scanning after a configurable delay
    /// (approximately 5 seconds) even if the signal is still present.
    TimeOperated,
    /// Carrier-operated: resume scanning when the received signal
    /// drops below the squelch threshold.
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
    /// VFO band scan: scans the entire frequency range of the current band
    /// using the configured step size. Started by pressing and holding
    /// the up/down key in VFO mode.
    BandScan,
    /// Memory channel scan: scans through all stored memory channels that
    /// are not locked out. Started in memory mode.
    MemoryScan,
    /// Program scan: scans between two user-defined frequency limits
    /// (programmable VFO lower and upper bounds). Configured via
    /// Menu No. 100.
    ProgramScan,
    /// MHz scan: scans within a 1 MHz range around the current frequency.
    /// Started by pressing and holding `[MHz]`.
    MhzScan,
    /// Group link scan: scans memory channels within linked memory groups.
    /// Groups are linked via Menu No. 204.
    GroupLinkScan,
    /// Priority scan: periodically checks a designated priority channel
    /// while scanning other frequencies.
    PriorityScan,
    /// Call scan: alternates between the current frequency and the call
    /// channel.
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
