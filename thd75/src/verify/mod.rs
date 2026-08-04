//! Before-and-after verification of live radio state.
//!
//! A [`StateSnapshot`] holds byte windows read from the radio at one point in
//! time. Diffing two snapshots across a setting change proves what actually
//! changed on the running radio, which a flash-level read-back cannot do: an
//! MCP page write proves bytes reached configuration flash, not that the
//! running radio applied them.
//!
//! [`RuntimeOffsetMap`] accumulates the field-to-offset mappings that
//! differential snapshots reveal, so that knowledge survives across sessions.
//!
//! This module is wire-format agnostic and transport agnostic. It consumes
//! windows a caller already obtained, so it has no async and no I/O and is
//! fully testable without a radio.

use std::collections::BTreeMap;

use crate::error::ValidationError;
use crate::types::MemoryReadOffset;

/// A single byte that differed between two snapshots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteChange {
    /// Offset of the byte that changed.
    pub offset: MemoryReadOffset,
    /// Value in the earlier snapshot.
    pub before: u8,
    /// Value in the later snapshot.
    pub after: u8,
}

/// Errors produced while comparing snapshots or parsing persisted data.
#[derive(Debug, thiserror::Error)]
pub enum VerifyError {
    /// The two snapshots do not cover the same windows, so a byte-for-byte
    /// comparison would be meaningless.
    #[error("snapshot window layouts differ: {detail}")]
    WindowLayoutMismatch {
        /// What specifically differed.
        detail: String,
    },

    /// A persisted offset map line could not be parsed.
    #[error("malformed offset map on line {line}: {detail}")]
    MalformedOffsetMap {
        /// One-based line number.
        line: usize,
        /// What specifically was wrong.
        detail: String,
    },

    /// An offset was outside the addressable window.
    #[error("invalid offset: {0}")]
    InvalidOffset(#[from] ValidationError),
}

/// Byte windows captured from the radio at one point in time.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StateSnapshot {
    windows: Vec<(MemoryReadOffset, Vec<u8>)>,
}

impl StateSnapshot {
    /// Builds a snapshot from windows a caller already read.
    #[must_use]
    pub const fn from_windows(windows: Vec<(MemoryReadOffset, Vec<u8>)>) -> Self {
        Self { windows }
    }

    /// Returns the captured windows.
    #[must_use]
    pub fn windows(&self) -> &[(MemoryReadOffset, Vec<u8>)] {
        &self.windows
    }

    /// Reports every byte that differs between this snapshot and `other`.
    ///
    /// Both snapshots must cover the same offsets with the same lengths in the
    /// same order. Comparing mismatched layouts is treated as an error rather
    /// than silently comparing whatever happens to line up, because a partial
    /// comparison would produce a confident-looking but meaningless answer.
    ///
    /// # Errors
    ///
    /// Returns [`VerifyError::WindowLayoutMismatch`] if the layouts differ, and
    /// [`VerifyError::InvalidOffset`] if a window extends past the addressable
    /// window.
    pub fn diff(&self, other: &Self) -> Result<Vec<ByteChange>, VerifyError> {
        if self.windows.len() != other.windows.len() {
            return Err(VerifyError::WindowLayoutMismatch {
                detail: format!(
                    "{} windows versus {}",
                    self.windows.len(),
                    other.windows.len()
                ),
            });
        }

        let mut changes = Vec::new();
        for (mine, theirs) in self.windows.iter().zip(other.windows.iter()) {
            let (my_offset, my_bytes) = mine;
            let (their_offset, their_bytes) = theirs;
            if my_offset != their_offset || my_bytes.len() != their_bytes.len() {
                return Err(VerifyError::WindowLayoutMismatch {
                    detail: format!(
                        "window at {my_offset} len {} versus {their_offset} len {}",
                        my_bytes.len(),
                        their_bytes.len()
                    ),
                });
            }
            for (index, (before, after)) in my_bytes.iter().zip(their_bytes.iter()).enumerate() {
                if before == after {
                    continue;
                }
                let index_u32 =
                    u32::try_from(index).map_err(|_| VerifyError::WindowLayoutMismatch {
                        detail: format!("window at {my_offset} is too long to index"),
                    })?;
                let raw = my_offset.as_raw().checked_add(index_u32).ok_or_else(|| {
                    VerifyError::WindowLayoutMismatch {
                        detail: format!("window at {my_offset} overflows the address space"),
                    }
                })?;
                changes.push(ByteChange {
                    offset: MemoryReadOffset::new(raw)?,
                    before: *before,
                    after: *after,
                });
            }
        }
        Ok(changes)
    }
}

/// Field-to-offset mappings discovered by differential snapshots.
///
/// Persists as one line per field: the field name, a tab, then the offsets as
/// six-digit uppercase hexadecimal separated by commas. A line-oriented text
/// format rather than JSON because `serde_json` is a development dependency
/// only and this crate keeps its runtime dependencies minimal.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RuntimeOffsetMap {
    entries: BTreeMap<String, Vec<MemoryReadOffset>>,
}

impl RuntimeOffsetMap {
    /// Records the offsets a field was observed to occupy, replacing any
    /// previous record for that field.
    pub fn record(&mut self, field: &str, offsets: &[MemoryReadOffset]) {
        // Replacing an existing record is intentional and the displaced value
        // is not needed. Bound explicitly because `unused_results` is denied.
        let _previous = self.entries.insert(field.to_owned(), offsets.to_vec());
    }

    /// Returns the offsets recorded for a field, if any.
    #[must_use]
    pub fn get(&self, field: &str) -> Option<&[MemoryReadOffset]> {
        self.entries.get(field).map(Vec::as_slice)
    }

    /// Returns the number of recorded fields.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns `true` if no fields have been recorded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Serializes the map to its line-oriented text form.
    #[must_use]
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        for (field, offsets) in &self.entries {
            out.push_str(field);
            out.push('\t');
            for (index, offset) in offsets.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(&offset.to_string());
            }
            out.push('\n');
        }
        out
    }

    /// Parses the line-oriented text form. Blank lines are ignored.
    ///
    /// # Errors
    ///
    /// Returns [`VerifyError::MalformedOffsetMap`] for a line with no tab or an
    /// unparseable offset, and [`VerifyError::InvalidOffset`] if an offset is
    /// outside the addressable window.
    pub fn from_text(text: &str) -> Result<Self, VerifyError> {
        let mut map = Self::default();
        for (index, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let line_number = index + 1;
            let (field, rest) =
                line.split_once('\t')
                    .ok_or_else(|| VerifyError::MalformedOffsetMap {
                        line: line_number,
                        detail: "expected a tab between field and offsets".to_owned(),
                    })?;
            let mut offsets = Vec::new();
            for token in rest.split(',') {
                let raw = u32::from_str_radix(token.trim(), 16).map_err(|e| {
                    VerifyError::MalformedOffsetMap {
                        line: line_number,
                        detail: format!("offset {token:?}: {e}"),
                    }
                })?;
                offsets.push(MemoryReadOffset::new(raw)?);
            }
            map.record(field, &offsets);
        }
        Ok(map)
    }
}

#[cfg(test)]
mod tests {
    use super::{RuntimeOffsetMap, StateSnapshot};
    use crate::types::MemoryReadOffset;

    type TestResult = Result<(), Box<dyn std::error::Error>>;
    type BoxErr = Box<dyn std::error::Error>;

    fn snap(offset: u32, bytes: &[u8]) -> Result<StateSnapshot, BoxErr> {
        Ok(StateSnapshot::from_windows(vec![(
            MemoryReadOffset::new(offset)?,
            bytes.to_vec(),
        )]))
    }

    #[test]
    fn identical_snapshots_have_no_changes() -> TestResult {
        let before = snap(0x1000, &[1, 2, 3, 4])?;
        let after = snap(0x1000, &[1, 2, 3, 4])?;
        assert!(before.diff(&after)?.is_empty());
        Ok(())
    }

    #[test]
    fn diff_reports_offset_before_and_after() -> TestResult {
        let before = snap(0x1000, &[1, 2, 3, 4])?;
        let after = snap(0x1000, &[1, 2, 9, 4])?;
        let changes = before.diff(&after)?;
        assert_eq!(changes.len(), 1);
        let change = changes.first().ok_or("missing change")?;
        assert_eq!(change.offset.as_raw(), 0x1002);
        assert_eq!(change.before, 3);
        assert_eq!(change.after, 9);
        Ok(())
    }

    #[test]
    fn diff_reports_every_changed_byte() -> TestResult {
        let before = snap(0x20, &[0, 0, 0])?;
        let after = snap(0x20, &[1, 0, 2])?;
        let changes = before.diff(&after)?;
        assert_eq!(changes.len(), 2);
        assert_eq!(changes.first().ok_or("missing")?.offset.as_raw(), 0x20);
        assert_eq!(changes.get(1).ok_or("missing")?.offset.as_raw(), 0x22);
        Ok(())
    }

    #[test]
    fn diff_rejects_mismatched_offsets() -> TestResult {
        let before = snap(0x1000, &[1, 2, 3, 4])?;
        let after = snap(0x2000, &[1, 2, 3, 4])?;
        let result = before.diff(&after);
        assert!(
            result.is_err(),
            "differing window offsets must be an error, got {result:?}"
        );
        Ok(())
    }

    #[test]
    fn diff_rejects_differing_window_length() -> TestResult {
        let before = snap(0x1000, &[1, 2, 3, 4])?;
        let after = snap(0x1000, &[1, 2, 3])?;
        let result = before.diff(&after);
        assert!(
            result.is_err(),
            "differing window lengths must be an error, got {result:?}"
        );
        Ok(())
    }

    #[test]
    fn diff_rejects_differing_window_count() -> TestResult {
        let before = StateSnapshot::from_windows(vec![
            (MemoryReadOffset::new(0x10)?, vec![0]),
            (MemoryReadOffset::new(0x20)?, vec![0]),
        ]);
        let after = snap(0x10, &[0])?;
        let result = before.diff(&after);
        assert!(
            result.is_err(),
            "differing window counts must be an error, got {result:?}"
        );
        Ok(())
    }

    #[test]
    fn offset_map_round_trips_through_text() -> TestResult {
        let mut map = RuntimeOffsetMap::default();
        map.record(
            "radio.BacklightControl",
            &[MemoryReadOffset::new(0x12_3456)?],
        );
        map.record(
            "radio.UsbFunction",
            &[MemoryReadOffset::new(0x10)?, MemoryReadOffset::new(0x20)?],
        );

        let text = map.to_text();
        let parsed = RuntimeOffsetMap::from_text(&text)?;

        assert_eq!(parsed.len(), 2);
        let usb = parsed
            .get("radio.UsbFunction")
            .ok_or("UsbFunction missing after round trip")?;
        assert_eq!(usb.len(), 2);
        assert_eq!(usb.first().ok_or("empty")?.as_raw(), 0x10);
        assert_eq!(usb.get(1).ok_or("short")?.as_raw(), 0x20);
        assert_eq!(
            parsed
                .get("radio.BacklightControl")
                .ok_or("BacklightControl missing")?
                .len(),
            1
        );
        Ok(())
    }

    #[test]
    fn offset_map_ignores_blank_lines() -> TestResult {
        let parsed = RuntimeOffsetMap::from_text("\nfield.a\t000010\n\n")?;
        assert_eq!(parsed.len(), 1);
        Ok(())
    }

    #[test]
    fn offset_map_rejects_line_without_tab() {
        let result = RuntimeOffsetMap::from_text("field.a 000010\n");
        assert!(result.is_err(), "missing tab must fail, got {result:?}");
    }

    #[test]
    fn offset_map_rejects_bad_hex() {
        let result = RuntimeOffsetMap::from_text("field.a\tZZZZZZ\n");
        assert!(result.is_err(), "bad hex must fail, got {result:?}");
    }

    #[test]
    fn offset_map_rejects_out_of_range_offset() {
        let result = RuntimeOffsetMap::from_text("field.a\t1000000\n");
        assert!(
            result.is_err(),
            "offset above the window must fail, got {result:?}"
        );
    }

    #[test]
    fn empty_map_reports_empty() {
        let map = RuntimeOffsetMap::default();
        assert!(map.is_empty());
        assert_eq!(map.len(), 0);
        assert_eq!(map.to_text(), "");
    }
}
