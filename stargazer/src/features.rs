// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later

//! Lossless AMBE container inspection and streaming vocoder-parameter export.

use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};

use mbelib_rs::{AmbeParamExtractor, FrameParams, PARAM_BANDS};

use crate::capture::FrameRecord;

/// Features per codec frame: f0, harmonic count, then amplitude, voicing and mask planes.
pub const FEATURE_DIM: usize = 2 + PARAM_BANDS * 3;

/// A validated borrowed version-1 AMBE container, including its reserved header bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FrameArchive<'a> {
    bytes: &'a [u8],
}

/// AMBE container framing errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ContainerError {
    /// The 16-byte header has another magic, version or record length.
    #[error("invalid STGZAMBE version-1 header")]
    BadHeader,
    /// A final frame is shorter than 13 bytes.
    #[error("truncated AMBE record: {0} trailing bytes")]
    Truncated(usize),
}

impl<'a> FrameArchive<'a> {
    /// Validate framing without allocating or discarding raw bytes.
    ///
    /// The four reserved header bytes and all 256 sequence values are retained.
    ///
    /// # Errors
    ///
    /// Returns [`ContainerError`] for an invalid header or incomplete final record.
    pub fn from_bytes(bytes: &'a [u8]) -> Result<Self, ContainerError> {
        validate_header(bytes.get(..16).ok_or(ContainerError::BadHeader)?)?;
        let trailing = (bytes.len() - 16) % 13;
        if trailing != 0 {
            return Err(ContainerError::Truncated(trailing));
        }
        Ok(Self { bytes })
    }
    /// Borrow every original byte, including the complete header.
    #[must_use]
    pub const fn as_bytes(self) -> &'a [u8] {
        self.bytes
    }
    /// Iterate copied fixed-width records in arrival order.
    #[must_use]
    pub fn iter(self) -> FrameRecords<'a> {
        FrameRecords {
            chunks: self.bytes.get(16..).unwrap_or_default().chunks_exact(13),
        }
    }
}

impl<'a> TryFrom<&'a [u8]> for FrameArchive<'a> {
    type Error = ContainerError;
    fn try_from(bytes: &'a [u8]) -> Result<Self, Self::Error> {
        Self::from_bytes(bytes)
    }
}
impl AsRef<[u8]> for FrameArchive<'_> {
    fn as_ref(&self) -> &[u8] {
        self.bytes
    }
}
impl<'a> IntoIterator for FrameArchive<'a> {
    type Item = FrameRecord;
    type IntoIter = FrameRecords<'a>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}
impl<'a> IntoIterator for &FrameArchive<'a> {
    type Item = FrameRecord;
    type IntoIter = FrameRecords<'a>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// Borrowed, exact-size iterator over validated 13-byte frame records.
#[derive(Debug, Clone)]
pub struct FrameRecords<'a> {
    chunks: std::slice::ChunksExact<'a, u8>,
}
impl Iterator for FrameRecords<'_> {
    type Item = FrameRecord;
    fn next(&mut self) -> Option<Self::Item> {
        record(self.chunks.next()?)
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.chunks.size_hint()
    }
}
impl ExactSizeIterator for FrameRecords<'_> {}
impl std::iter::FusedIterator for FrameRecords<'_> {}
impl DoubleEndedIterator for FrameRecords<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        record(self.chunks.next_back()?)
    }
}

fn validate_header(bytes: &[u8]) -> Result<(), ContainerError> {
    if bytes.len() != 16
        || bytes.get(..8) != Some(b"STGZAMBE".as_slice())
        || bytes.get(8..12) != Some(&[1, 0, 13, 0])
    {
        return Err(ContainerError::BadHeader);
    }
    Ok(())
}

fn record(bytes: &[u8]) -> Option<FrameRecord> {
    Some(FrameRecord {
        seq: *bytes.first()?,
        ambe: bytes.get(1..10)?.try_into().ok()?,
        slow_data: bytes.get(10..13)?.try_into().ok()?,
    })
}

/// Flatten one checked vocoder parameter record into a feature row.
#[must_use]
pub fn feature_row(parameters: &FrameParams) -> [f32; FEATURE_DIM] {
    let mut row = [0.0; FEATURE_DIM];
    if let Some(slot) = row.first_mut() {
        *slot = parameters.f0_hz();
    }
    if let Some(slot) = row.get_mut(1) {
        #[expect(
            clippy::cast_precision_loss,
            reason = "validated harmonic count is at most 56 and exactly representable in f32"
        )]
        {
            *slot = parameters.harmonics() as f32;
        }
    }
    for band in 0..parameters.harmonics() {
        if let Some(slot) = row.get_mut(2 + band) {
            *slot = parameters
                .amplitudes()
                .get(band)
                .copied()
                .unwrap_or_default();
        }
        if let Some(slot) = row.get_mut(2 + PARAM_BANDS + band) {
            *slot = f32::from(u8::from(
                parameters.voiced().get(band).copied().unwrap_or_default(),
            ));
        }
        if let Some(slot) = row.get_mut(2 + 2 * PARAM_BANDS + band) {
            *slot = 1.0;
        }
    }
    row
}

/// Feature extraction or file publication failure.
#[derive(Debug, thiserror::Error)]
pub enum FeatureError {
    /// Input framing is invalid.
    #[error(transparent)]
    Container(#[from] ContainerError),
    /// The result cannot fit the platform's address space.
    #[error("feature rows exceed addressable capacity")]
    TooLarge,
    /// Reserving the complete row buffer failed.
    #[error("feature allocation: {0}")]
    Allocation(#[source] std::collections::TryReserveError),
    /// Input or temporary-output I/O failed.
    #[error("feature file {path}: {source}")]
    Io {
        /// File being read, written or installed.
        path: PathBuf,
        /// Underlying filesystem error.
        #[source]
        source: std::io::Error,
    },
}

/// Extract codec-time rows, concealing sequence gaps with a fresh parameter extractor.
///
/// # Errors
///
/// Returns a capacity/allocation error before extraction when the complete result
/// cannot be reserved. Use [`export_file`] to avoid accumulating complete files.
pub fn extract_features(records: &[FrameRecord]) -> Result<Vec<[f32; FEATURE_DIM]>, FeatureError> {
    let count = crate::audio::expected_frames(records)
        .and_then(|count| usize::try_from(count).ok())
        .ok_or(FeatureError::TooLarge)?;
    let mut rows = Vec::new();
    rows.try_reserve_exact(count)
        .map_err(FeatureError::Allocation)?;
    let mut extractor = AmbeParamExtractor::new();
    let mut previous = None;
    for frame in records {
        if let Some(previous) = previous {
            for _ in 0..crate::capture::seq_gap(previous, frame.seq) {
                rows.push(feature_row(&extractor.conceal()));
            }
        }
        previous = Some(frame.seq);
        rows.push(feature_row(&extractor.extract(&frame.ambe)));
    }
    Ok(rows)
}

fn io_error(path: &Path) -> impl FnOnce(std::io::Error) -> FeatureError {
    let path = path.to_path_buf();
    move |source| FeatureError::Io { path, source }
}

/// Stream a container into little-endian f32 rows without replacing an existing file.
///
/// Memory holds one archive record, one feature row and one extractor. The output
/// is synced in an exclusive temporary file and installed only after complete
/// parsing. Errors may leave that temporary file; the destination is never replaced.
///
/// # Errors
///
/// Returns typed input/framing, count overflow, filesystem or existing-output errors.
pub fn export_file(input: &Path, output: &Path) -> Result<usize, FeatureError> {
    let mut source = std::fs::File::open(input).map_err(io_error(input))?;
    let mut header = [0; 16];
    source.read_exact(&mut header).map_err(io_error(input))?;
    validate_header(&header)?;
    let mut temporary = output.as_os_str().to_owned();
    temporary.push(".tmp");
    let temporary = PathBuf::from(temporary);
    let mut destination = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(io_error(&temporary))?;
    let result = export_records(&mut source, &mut destination, input, &temporary);
    drop(destination);
    match result {
        Ok(count) => {
            if let Err(source) = std::fs::hard_link(&temporary, output) {
                let _cleanup = std::fs::remove_file(&temporary);
                return Err(io_error(output)(source));
            }
            std::fs::remove_file(&temporary).map_err(io_error(&temporary))?;
            Ok(count)
        }
        Err(error) => {
            let _cleanup = std::fs::remove_file(&temporary);
            Err(error)
        }
    }
}

fn export_records(
    source: &mut std::fs::File,
    destination: &mut std::fs::File,
    input: &Path,
    output: &Path,
) -> Result<usize, FeatureError> {
    let mut extractor = AmbeParamExtractor::new();
    let mut previous = None;
    let mut count = 0_usize;
    loop {
        let mut bytes = [0; 13];
        let mut filled = 0;
        while filled < bytes.len() {
            let size = source
                .read(bytes.get_mut(filled..).ok_or(FeatureError::TooLarge)?)
                .map_err(io_error(input))?;
            if size == 0 {
                break;
            }
            filled += size;
        }
        if filled == 0 {
            break;
        }
        if filled != bytes.len() {
            return Err(ContainerError::Truncated(filled).into());
        }
        let frame = record(&bytes).ok_or(ContainerError::Truncated(filled))?;
        if let Some(previous) = previous {
            for _ in 0..crate::capture::seq_gap(previous, frame.seq) {
                write_row(destination, &feature_row(&extractor.conceal()))
                    .map_err(io_error(output))?;
                count = count.checked_add(1).ok_or(FeatureError::TooLarge)?;
            }
        }
        previous = Some(frame.seq);
        write_row(destination, &feature_row(&extractor.extract(&frame.ambe)))
            .map_err(io_error(output))?;
        count = count.checked_add(1).ok_or(FeatureError::TooLarge)?;
    }
    destination.sync_all().map_err(io_error(output))?;
    Ok(count)
}

fn write_row(file: &mut std::fs::File, row: &[f32; FEATURE_DIM]) -> std::io::Result<()> {
    for value in row {
        file.write_all(&value.to_le_bytes())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;
    const SILENCE: [u8; 9] = [0x9E, 0x8D, 0x32, 0x88, 0x26, 0x1A, 0x3F, 0x61, 0xE8];
    fn frame(seq: u8) -> FrameRecord {
        FrameRecord {
            seq,
            ambe: SILENCE,
            slow_data: [1, 2, 3],
        }
    }

    #[test]
    fn round_trips_the_writers_container() -> TestResult {
        let frames = [
            frame(0),
            FrameRecord {
                seq: 255,
                ambe: [0x11; 9],
                slow_data: [4, 5, 6],
            },
        ];
        let bytes = crate::writer::container_bytes(&frames)?;
        let archive = FrameArchive::from_bytes(&bytes)?;
        assert_eq!(archive.iter().len(), 2);
        assert_eq!(archive.iter().collect::<Vec<_>>(), frames);
        assert_eq!(archive.as_bytes(), bytes);
        Ok(())
    }

    #[test]
    fn rejects_foreign_bytes_and_preserves_reserved_bytes() -> TestResult {
        assert!(matches!(
            FrameArchive::from_bytes(b"NOTOURSXxxxxxxxx"),
            Err(ContainerError::BadHeader)
        ));
        let mut bytes = crate::writer::container_bytes(&[])?;
        if let Some(reserved) = bytes.get_mut(12..16) {
            reserved.copy_from_slice(&[1, 2, 3, 4]);
        }
        assert_eq!(FrameArchive::from_bytes(&bytes)?.as_bytes(), bytes);
        bytes.extend_from_slice(&[0; 5]);
        assert!(matches!(
            FrameArchive::from_bytes(&bytes),
            Err(ContainerError::Truncated(5))
        ));
        Ok(())
    }

    #[test]
    fn features_conceal_gaps_and_survive_wild_seq() -> TestResult {
        assert_eq!(extract_features(&[frame(0), frame(1), frame(4)])?.len(), 5);
        assert_eq!(
            extract_features(&[frame(3), frame(200), frame(4)])?.len(),
            3
        );
        Ok(())
    }

    #[test]
    fn rows_have_voice_shaped_content() -> TestResult {
        let rows = extract_features(&[frame(0)])?;
        let row = rows.first().ok_or("feature row")?;
        let f0 = *row.first().ok_or("f0")?;
        let harmonics = *row.get(1).ok_or("harmonics")?;
        assert!(f0 > 0.0 && f0 < 500.0, "f0 {f0}");
        assert!((9.0..=56.0).contains(&harmonics), "harmonics {harmonics}");
        let mask: f32 = row.get(2 + 2 * PARAM_BANDS..).ok_or("mask")?.iter().sum();
        assert!((mask - harmonics).abs() < f32::EPSILON);
        Ok(())
    }

    #[test]
    fn streaming_export_matches_rows_and_never_overwrites() -> TestResult {
        let directory = tempfile::tempdir()?;
        let input = directory.path().join("input.ambe");
        let output = directory.path().join("output.f32");
        let frames = [frame(0), frame(3), frame(255)];
        std::fs::write(&input, crate::writer::container_bytes(&frames)?)?;
        let rows = extract_features(&frames)?;
        assert_eq!(export_file(&input, &output)?, rows.len());
        let expected: Vec<u8> = rows
            .iter()
            .flatten()
            .flat_map(|value| value.to_le_bytes())
            .collect();
        assert_eq!(std::fs::read(&output)?, expected);
        assert!(export_file(&input, &output).is_err());
        assert_eq!(std::fs::read(&output)?, expected);
        std::fs::write(
            &input,
            [crate::writer::container_bytes(&frames)?, vec![0; 5]].concat(),
        )?;
        let incomplete = directory.path().join("incomplete.f32");
        assert!(matches!(
            export_file(&input, &incomplete),
            Err(FeatureError::Container(ContainerError::Truncated(5)))
        ));
        assert!(!incomplete.exists());
        Ok(())
    }
}
