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
use std::collections::btree_map;
use std::iter::FusedIterator;

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
    /// A captured window extends beyond the six-digit offset domain.
    #[error("snapshot window at {offset} with {length} bytes exceeds the readable offset domain")]
    WindowOutOfRange {
        /// First byte's offset.
        offset: MemoryReadOffset,
        /// Number of captured bytes.
        length: usize,
    },
    /// A mapping cannot retain its meaning in the persisted text grammar.
    #[error(transparent)]
    InvalidMapping(#[from] OffsetMappingError),
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
    ///
    /// # Errors
    ///
    /// Returns [`VerifyError::WindowOutOfRange`] when any complete window
    /// extends beyond [`MemoryReadOffset::MAX`], including unchanged bytes.
    pub fn from_windows(windows: Vec<(MemoryReadOffset, Vec<u8>)>) -> Result<Self, VerifyError> {
        for (offset, bytes) in &windows {
            if bytes.len()
                > usize::try_from(MemoryReadOffset::MAX - offset.as_raw() + 1).map_err(|_| {
                    VerifyError::WindowOutOfRange {
                        offset: *offset,
                        length: bytes.len(),
                    }
                })?
            {
                return Err(VerifyError::WindowOutOfRange {
                    offset: *offset,
                    length: bytes.len(),
                });
            }
        }
        Ok(Self { windows })
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

/// Why a field-to-offset mapping cannot be persisted losslessly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum OffsetMappingError {
    /// The field name is empty.
    #[error("offset-map field name must not be empty")]
    EmptyField,
    /// A field byte is a text-format delimiter or control byte.
    #[error("offset-map field contains control byte {byte:#04X} at index {index}")]
    FieldControlByte {
        /// Rejected byte.
        byte: u8,
        /// Zero-based byte index.
        index: usize,
    },
    /// The field has no observed offsets.
    #[error("offset-map entry must contain at least one observed offset")]
    EmptyOffsets,
}

/// One nonempty field name and its nonempty ordered observed offsets.
///
/// Names contain no ASCII controls, so every value round-trips through
/// [`RuntimeOffsetMap::to_text`] and [`RuntimeOffsetMap::from_text`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OffsetMapping {
    field: String,
    offsets: Vec<MemoryReadOffset>,
}

impl OffsetMapping {
    /// Retain an owned name and offset vector after validating text geometry.
    ///
    /// # Errors
    ///
    /// Returns [`OffsetMappingError`] for an empty name, an ASCII control byte
    /// in the name or an empty offset vector.
    pub fn new(
        field: impl Into<String>,
        offsets: Vec<MemoryReadOffset>,
    ) -> Result<Self, OffsetMappingError> {
        let field = field.into();
        if field.is_empty() {
            return Err(OffsetMappingError::EmptyField);
        }
        if let Some((index, byte)) = field
            .bytes()
            .enumerate()
            .find(|(_, byte)| byte.is_ascii_control())
        {
            return Err(OffsetMappingError::FieldControlByte { byte, index });
        }
        if offsets.is_empty() {
            return Err(OffsetMappingError::EmptyOffsets);
        }
        Ok(Self { field, offsets })
    }

    /// The persisted field name.
    #[must_use]
    pub fn field(&self) -> &str {
        &self.field
    }

    /// The observed offsets in their recorded order.
    #[must_use]
    pub fn offsets(&self) -> &[MemoryReadOffset] {
        &self.offsets
    }

    /// Recover the owned name and offsets without cloning their allocations.
    #[must_use]
    pub fn into_parts(self) -> (String, Vec<MemoryReadOffset>) {
        (self.field, self.offsets)
    }
}

impl TryFrom<(String, Vec<MemoryReadOffset>)> for OffsetMapping {
    type Error = OffsetMappingError;
    fn try_from((field, offsets): (String, Vec<MemoryReadOffset>)) -> Result<Self, Self::Error> {
        Self::new(field, offsets)
    }
}

impl From<OffsetMapping> for (String, Vec<MemoryReadOffset>) {
    fn from(entry: OffsetMapping) -> Self {
        entry.into_parts()
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
    pub fn record(&mut self, entry: OffsetMapping) {
        // Replacing an existing record is intentional and the displaced value
        // is not needed. Bound explicitly because `unused_results` is denied.
        let (field, offsets) = entry.into_parts();
        let _previous = self.entries.insert(field, offsets);
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

    /// Iterate over fields in lexical order and their borrowed offset slices.
    #[must_use]
    pub fn iter(&self) -> OffsetMapIter<'_> {
        OffsetMapIter {
            inner: self.entries.iter(),
        }
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
            map.record(OffsetMapping::new(field, offsets)?);
        }
        Ok(map)
    }
}

impl Extend<OffsetMapping> for RuntimeOffsetMap {
    fn extend<I: IntoIterator<Item = OffsetMapping>>(&mut self, entries: I) {
        for entry in entries {
            self.record(entry);
        }
    }
}

impl FromIterator<OffsetMapping> for RuntimeOffsetMap {
    fn from_iter<I: IntoIterator<Item = OffsetMapping>>(entries: I) -> Self {
        let mut map = Self::default();
        map.extend(entries);
        map
    }
}

/// Borrowed field/offset entries in lexical field-name order.
#[derive(Debug, Clone)]
pub struct OffsetMapIter<'a> {
    inner: btree_map::Iter<'a, String, Vec<MemoryReadOffset>>,
}

impl<'a> Iterator for OffsetMapIter<'a> {
    type Item = (&'a str, &'a [MemoryReadOffset]);
    fn next(&mut self) -> Option<Self::Item> {
        self.inner
            .next()
            .map(|(field, offsets)| (field.as_str(), offsets.as_slice()))
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}
impl DoubleEndedIterator for OffsetMapIter<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        self.inner
            .next_back()
            .map(|(field, offsets)| (field.as_str(), offsets.as_slice()))
    }
}
impl ExactSizeIterator for OffsetMapIter<'_> {}
impl FusedIterator for OffsetMapIter<'_> {}

impl<'a> IntoIterator for &'a RuntimeOffsetMap {
    type Item = (&'a str, &'a [MemoryReadOffset]);
    type IntoIter = OffsetMapIter<'a>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// Owned checked entries in lexical field-name order.
#[derive(Debug)]
pub struct OffsetMapIntoIter {
    inner: btree_map::IntoIter<String, Vec<MemoryReadOffset>>,
}

impl Iterator for OffsetMapIntoIter {
    type Item = OffsetMapping;
    fn next(&mut self) -> Option<Self::Item> {
        self.inner
            .next()
            .map(|(field, offsets)| OffsetMapping { field, offsets })
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}
impl DoubleEndedIterator for OffsetMapIntoIter {
    fn next_back(&mut self) -> Option<Self::Item> {
        self.inner
            .next_back()
            .map(|(field, offsets)| OffsetMapping { field, offsets })
    }
}
impl ExactSizeIterator for OffsetMapIntoIter {}
impl FusedIterator for OffsetMapIntoIter {}

impl IntoIterator for RuntimeOffsetMap {
    type Item = OffsetMapping;
    type IntoIter = OffsetMapIntoIter;
    fn into_iter(self) -> Self::IntoIter {
        OffsetMapIntoIter {
            inner: self.entries.into_iter(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{OffsetMapping, OffsetMappingError, RuntimeOffsetMap, StateSnapshot, VerifyError};
    use crate::types::MemoryReadOffset;

    type TestResult = Result<(), Box<dyn std::error::Error>>;
    type BoxErr = Box<dyn std::error::Error>;

    fn snap(offset: u32, bytes: &[u8]) -> Result<StateSnapshot, BoxErr> {
        Ok(StateSnapshot::from_windows(vec![(
            MemoryReadOffset::new(offset)?,
            bytes.to_vec(),
        )])?)
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
        ])?;
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
        map.record(OffsetMapping::new(
            "radio.BacklightControl",
            vec![MemoryReadOffset::new(0x12_3456)?],
        )?);
        map.record(OffsetMapping::new(
            "radio.UsbFunction",
            vec![MemoryReadOffset::new(0x10)?, MemoryReadOffset::new(0x20)?],
        )?);

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

    #[test]
    fn complete_snapshot_windows_are_checked_even_when_bytes_do_not_change() -> TestResult {
        let offset = MemoryReadOffset::new(MemoryReadOffset::MAX)?;
        let result = StateSnapshot::from_windows(vec![(offset, vec![0, 0])]);
        assert!(
            matches!(result, Err(VerifyError::WindowOutOfRange { length: 2, .. })),
            "unchanged overrun bytes must not form a snapshot: {result:?}"
        );
        let valid = StateSnapshot::from_windows(vec![(offset, vec![0])])?;
        assert!(valid.diff(&valid)?.is_empty());
        Ok(())
    }

    #[test]
    fn offset_mappings_reject_text_delimiters_and_empty_observations() {
        for field in ["", "a\tb", "a\nb", "a\rb", "a\0b"] {
            assert!(
                OffsetMapping::new(field, vec![MemoryReadOffset::ZERO]).is_err(),
                "invalid persisted name {field:?}"
            );
        }
        assert_eq!(
            OffsetMapping::new("field", Vec::new()),
            Err(OffsetMappingError::EmptyOffsets)
        );
    }

    #[test]
    fn map_collection_roundtrip_retains_owned_inputs_and_checked_entries() -> TestResult {
        let field = "radio.Field".to_owned();
        let allocation = field.as_ptr();
        let offsets = vec![MemoryReadOffset::ZERO];
        let offset_allocation = offsets.as_ptr();
        let map: RuntimeOffsetMap = std::iter::once(OffsetMapping::new(field, offsets)?).collect();
        let (field, offsets) = map.iter().next().ok_or("recorded entry")?;
        assert_eq!(
            field.as_ptr(),
            allocation,
            "owned field must retain its allocation"
        );
        assert_eq!(
            offsets.as_ptr(),
            offset_allocation,
            "owned offsets must retain their allocation"
        );
        assert_eq!(RuntimeOffsetMap::from_text(&map.to_text())?, map);
        let rebuilt: RuntimeOffsetMap = map.clone().into_iter().collect();
        assert_eq!(rebuilt, map);
        assert_eq!((&map).into_iter().len(), 1);
        Ok(())
    }
}
