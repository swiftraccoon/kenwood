//! Bounded AX.25 digipeater paths.

use alloc::vec::Vec;
use core::slice;

use crate::address::RouteEntry;
use crate::error::Ax25Error;

/// Maximum number of digipeater addresses in an AX.25 frame.
///
/// Matches AX.25 v2.0 / APRS deployment convention and Linux kernel
/// `AX25_MAX_DIGIS`. AX.25 v2.2 §3.12.5 reduced this to 2 but no APRS
/// network respects that limit.
pub const MAX_DIGIPEATERS: usize = 8;

/// A validated AX.25 digipeater path.
///
/// The path always contains at most [`MAX_DIGIPEATERS`] entries. Structural
/// mutation is fallible so an [`crate::Ax25Packet`] cannot represent a frame
/// with an oversized address field.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DigipeaterPath(Vec<RouteEntry>);

impl DigipeaterPath {
    /// Create an empty digipeater path.
    #[must_use]
    pub const fn empty() -> Self {
        Self(Vec::new())
    }

    /// Create a path from route entries.
    ///
    /// # Errors
    ///
    /// Returns [`Ax25Error::TooManyDigipeaters`] if `entries` contains more
    /// than [`MAX_DIGIPEATERS`] route entries.
    pub fn new(entries: Vec<RouteEntry>) -> Result<Self, Ax25Error> {
        if entries.len() > MAX_DIGIPEATERS {
            return Err(Ax25Error::TooManyDigipeaters);
        }
        Ok(Self(entries))
    }

    /// Append an entry while preserving the protocol limit.
    ///
    /// # Errors
    ///
    /// Returns [`Ax25Error::TooManyDigipeaters`] if the path already
    /// contains [`MAX_DIGIPEATERS`] entries. The path is unchanged on error.
    pub fn try_push(&mut self, entry: RouteEntry) -> Result<(), Ax25Error> {
        if self.len() == MAX_DIGIPEATERS {
            return Err(Ax25Error::TooManyDigipeaters);
        }
        self.0.push(entry);
        Ok(())
    }

    /// Return the route entries as a slice.
    #[must_use]
    pub fn as_slice(&self) -> &[RouteEntry] {
        &self.0
    }

    /// Consume the path and return its route entries.
    #[must_use]
    pub fn into_vec(self) -> Vec<RouteEntry> {
        self.0
    }

    /// Insert an entry at `index` while preserving the protocol limit.
    ///
    /// Inserting at `self.len()` appends the entry.
    ///
    /// # Errors
    ///
    /// Returns [`Ax25Error::DigipeaterIndexOutOfBounds`] if `index` is
    /// greater than the current length, or [`Ax25Error::TooManyDigipeaters`]
    /// if the path already contains [`MAX_DIGIPEATERS`] entries. The path is
    /// unchanged on error.
    pub fn try_insert(&mut self, index: usize, entry: RouteEntry) -> Result<(), Ax25Error> {
        let len = self.len();
        if index > len {
            return Err(Ax25Error::DigipeaterIndexOutOfBounds { index, len });
        }
        if len == MAX_DIGIPEATERS {
            return Err(Ax25Error::TooManyDigipeaters);
        }
        self.0.insert(index, entry);
        Ok(())
    }

    /// Return the number of route entries.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.0.len()
    }

    /// Return `true` if the path has no route entries.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Return the entry at `index`, if present.
    #[must_use]
    pub fn get(&self, index: usize) -> Option<&RouteEntry> {
        self.0.get(index)
    }

    /// Return the mutable entry at `index`, if present.
    #[must_use]
    pub fn get_mut(&mut self, index: usize) -> Option<&mut RouteEntry> {
        self.0.get_mut(index)
    }

    /// Return the first entry, if present.
    #[must_use]
    pub fn first(&self) -> Option<&RouteEntry> {
        self.0.first()
    }

    /// Iterate over the route entries.
    pub fn iter(&self) -> slice::Iter<'_, RouteEntry> {
        self.0.iter()
    }

    /// Mutably iterate over the route entries.
    pub fn iter_mut(&mut self) -> slice::IterMut<'_, RouteEntry> {
        self.0.iter_mut()
    }
}

impl Default for DigipeaterPath {
    fn default() -> Self {
        Self::empty()
    }
}

impl TryFrom<Vec<RouteEntry>> for DigipeaterPath {
    type Error = Ax25Error;

    fn try_from(entries: Vec<RouteEntry>) -> Result<Self, Self::Error> {
        Self::new(entries)
    }
}

impl AsRef<[RouteEntry]> for DigipeaterPath {
    fn as_ref(&self) -> &[RouteEntry] {
        self.as_slice()
    }
}

impl IntoIterator for DigipeaterPath {
    type Item = RouteEntry;
    type IntoIter = alloc::vec::IntoIter<RouteEntry>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl<'a> IntoIterator for &'a DigipeaterPath {
    type Item = &'a RouteEntry;
    type IntoIter = slice::Iter<'a, RouteEntry>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<'a> IntoIterator for &'a mut DigipeaterPath {
    type Item = &'a mut RouteEntry;
    type IntoIter = slice::IterMut<'a, RouteEntry>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter_mut()
    }
}

#[cfg(test)]
mod tests {
    use alloc::boxed::Box;
    use alloc::vec::Vec;

    use super::*;

    type TestResult = Result<(), Box<dyn core::error::Error>>;

    fn entries(count: usize) -> Result<Vec<RouteEntry>, Box<dyn core::error::Error>> {
        (0..count)
            .map(|ssid| {
                let ssid = u8::try_from(ssid)?;
                Ok(RouteEntry::new("WIDE", ssid)?)
            })
            .collect()
    }

    #[test]
    fn empty_and_default_create_the_zero_entry_boundary() {
        assert!(DigipeaterPath::empty().is_empty());
        assert_eq!(DigipeaterPath::default(), DigipeaterPath::empty());
    }

    #[test]
    fn construction_accepts_exactly_eight_entries() -> TestResult {
        let path = DigipeaterPath::new(entries(MAX_DIGIPEATERS)?)?;
        assert_eq!(path.len(), MAX_DIGIPEATERS);
        Ok(())
    }

    #[test]
    fn construction_rejects_ninth_entry() -> TestResult {
        let result = DigipeaterPath::new(entries(MAX_DIGIPEATERS + 1)?);
        assert_eq!(result, Err(Ax25Error::TooManyDigipeaters));
        Ok(())
    }

    #[test]
    fn insertion_accepts_eighth_and_rejects_ninth_without_mutation() -> TestResult {
        let mut path = DigipeaterPath::new(entries(MAX_DIGIPEATERS - 1)?)?;
        path.try_insert(path.len(), RouteEntry::new("LAST", 7)?)?;
        assert_eq!(path.len(), MAX_DIGIPEATERS);

        let full_path = path.clone();
        let result = path.try_insert(path.len(), RouteEntry::new("EXTRA", 8)?);
        assert_eq!(result, Err(Ax25Error::TooManyDigipeaters));
        assert_eq!(path, full_path);
        Ok(())
    }

    #[test]
    fn push_accepts_eighth_and_rejects_ninth_without_mutation() -> TestResult {
        let mut path = DigipeaterPath::new(entries(MAX_DIGIPEATERS - 1)?)?;
        path.try_push(RouteEntry::new("LAST", 7)?)?;
        assert_eq!(path.len(), MAX_DIGIPEATERS);

        let full_path = path.clone();
        let result = path.try_push(RouteEntry::new("EXTRA", 8)?);
        assert_eq!(result, Err(Ax25Error::TooManyDigipeaters));
        assert_eq!(path, full_path);
        Ok(())
    }

    #[test]
    fn insertion_rejects_out_of_bounds_index_without_panicking() -> TestResult {
        let mut path = DigipeaterPath::empty();
        let result = path.try_insert(1, RouteEntry::new("WIDE", 1)?);
        assert_eq!(
            result,
            Err(Ax25Error::DigipeaterIndexOutOfBounds { index: 1, len: 0 })
        );
        assert!(path.is_empty());
        Ok(())
    }

    #[test]
    fn conversion_and_iteration_preserve_entries() -> TestResult {
        let original = entries(2)?;
        let path = DigipeaterPath::try_from(original.clone())?;
        assert_eq!(path.as_ref(), original.as_slice());
        assert_eq!((&path).into_iter().count(), 2);
        assert_eq!(path.into_vec(), original);
        Ok(())
    }
}
