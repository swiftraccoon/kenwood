// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later

//! Vocoder-parameter feature export from `.ambe` containers.
//!
//! Converts an archived frame container into the 170-dimension
//! per-frame feature rows that vocoder-parameter ASR consumes:
//! `[f0, L, sa[56], v_uv[56], mask[56]]`, one row per 20 ms frame.
//! Sequence gaps are filled with the extractor's concealment
//! parameters so the row grid stays uniform, and out-of-alphabet
//! seq bytes (corrupted frames) never drive the gap math — the same
//! guards the capture core applies.

use std::path::Path;

use mbelib_rs::{AmbeParamExtractor, FrameParams, PARAM_BANDS};

/// D-STAR voice seq values cycle 0..=20.
const SEQ_MODULUS: u16 = 21;

/// Features per frame: f0, L, then three `PARAM_BANDS`-wide planes
/// (spectral amplitude, voiced flag, validity mask).
pub const FEATURE_DIM: usize = 2 + PARAM_BANDS * 3;

/// One archived frame record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContainerRecord {
    /// D-STAR frame sequence byte as received.
    pub seq: u8,
    /// 9-byte AMBE payload.
    pub ambe: [u8; 9],
    /// 3-byte slow-data payload.
    pub slow_data: [u8; 3],
}

/// Container parsing failures.
#[derive(Debug, thiserror::Error)]
pub enum ContainerError {
    /// The magic or version did not match.
    #[error("not a stargazer AMBE container (bad magic or version)")]
    BadHeader,
    /// The byte length does not divide into whole records.
    #[error("truncated container: {0} trailing bytes")]
    Truncated(usize),
}

/// Parse a `.ambe` container's bytes into frame records.
///
/// # Errors
///
/// [`ContainerError`] when the header or record framing is invalid.
pub fn read_container(bytes: &[u8]) -> Result<Vec<ContainerRecord>, ContainerError> {
    let header = bytes.get(..16).ok_or(ContainerError::BadHeader)?;
    let magic_ok = header.get(..8) == Some(b"STGZAMBE".as_slice());
    let version_ok = header.get(8..10) == Some(&[1, 0]);
    let record_len_ok = header.get(10..12) == Some(&[13, 0]);
    if !(magic_ok && version_ok && record_len_ok) {
        return Err(ContainerError::BadHeader);
    }
    let body = bytes.get(16..).unwrap_or(&[]);
    let trailing = body.len() % 13;
    if trailing != 0 {
        return Err(ContainerError::Truncated(trailing));
    }
    Ok(body
        .chunks_exact(13)
        .map(|rec| {
            let mut ambe = [0u8; 9];
            let mut slow_data = [0u8; 3];
            if let Some(src) = rec.get(1..10) {
                ambe.copy_from_slice(src);
            }
            if let Some(src) = rec.get(10..13) {
                slow_data.copy_from_slice(src);
            }
            ContainerRecord {
                seq: rec.first().copied().unwrap_or(0),
                ambe,
                slow_data,
            }
        })
        .collect())
}

/// Flatten one frame's parameters into a feature row.
#[must_use]
pub fn feature_row(p: &FrameParams) -> [f32; FEATURE_DIM] {
    let mut row = [0.0f32; FEATURE_DIM];
    if let Some(slot) = row.first_mut() {
        *slot = p.f0_hz;
    }
    if let Some(slot) = row.get_mut(1) {
        #[expect(
            clippy::cast_precision_loss,
            reason = "harmonic count is at most 56; exact in f32"
        )]
        {
            *slot = p.harmonics as f32;
        }
    }
    for band in 0..p.harmonics.min(PARAM_BANDS) {
        if let Some(sa) = row.get_mut(2 + band) {
            *sa = p.amplitudes.get(band).copied().unwrap_or(0.0);
        }
        if let Some(vuv) = row.get_mut(2 + PARAM_BANDS + band) {
            *vuv = f32::from(u8::from(p.voiced.get(band).copied().unwrap_or(false)));
        }
        if let Some(mask) = row.get_mut(2 + 2 * PARAM_BANDS + band) {
            *mask = 1.0;
        }
    }
    row
}

/// Extract feature rows from archived records, concealing seq gaps
/// so the 20 ms grid stays uniform.
#[must_use]
pub fn extract_features(records: &[ContainerRecord]) -> Vec<[f32; FEATURE_DIM]> {
    let mut extractor = AmbeParamExtractor::new();
    let mut rows = Vec::with_capacity(records.len());
    let mut prev_seq: Option<u8> = None;
    for rec in records {
        if let Some(prev) = prev_seq
            && u16::from(rec.seq) < SEQ_MODULUS
            && u16::from(prev) < SEQ_MODULUS
        {
            let distance = (u16::from(rec.seq) + SEQ_MODULUS - u16::from(prev)) % SEQ_MODULUS;
            for _ in 1..distance {
                rows.push(feature_row(&extractor.conceal()));
            }
        }
        prev_seq = Some(rec.seq);
        rows.push(feature_row(&extractor.extract(&rec.ambe)));
    }
    rows
}

/// Read a container file and write its features as little-endian
/// f32 rows (`FEATURE_DIM` floats per frame).
///
/// # Errors
///
/// I/O or container-format errors, stringified for CLI reporting.
pub fn export_file(input: &Path, out: &Path) -> Result<usize, String> {
    let bytes = std::fs::read(input).map_err(|e| format!("read {}: {e}", input.display()))?;
    let records = read_container(&bytes).map_err(|e| format!("{}: {e}", input.display()))?;
    let rows = extract_features(&records);
    let mut buf = Vec::with_capacity(rows.len() * FEATURE_DIM * 4);
    for row in &rows {
        for value in row {
            buf.extend_from_slice(&value.to_le_bytes());
        }
    }
    std::fs::write(out, &buf).map_err(|e| format!("write {}: {e}", out.display()))?;
    Ok(rows.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::{CompletedRecording, EndReason, FrameRecord, StreamOrigin};
    use chrono::{DateTime, Utc};
    use dstar_gateway_core::{Module, StreamId};
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// The D-STAR AMBE silence frame — a valid DVSI voice frame.
    const SILENCE: [u8; 9] = [0x9E, 0x8D, 0x32, 0x88, 0x26, 0x1A, 0x3F, 0x61, 0xE8];

    fn recording(frames: Vec<FrameRecord>) -> Option<CompletedRecording> {
        Some(CompletedRecording {
            origin: StreamOrigin {
                reflector: "REF001".to_string(),
                module: Module::C,
                protocol: "dplus",
                host: "h".to_string(),
                port: 20001,
                peer: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 20001),
            },
            stream_id: StreamId::new(1)?,
            header: None,
            header_diagnostics: Vec::new(),
            started_at: DateTime::<Utc>::UNIX_EPOCH,
            ended_at: DateTime::<Utc>::UNIX_EPOCH,
            end_reason: EndReason::Eot,
            frames,
            gaps: 0,
            text: None,
            text_bytes: None,
            dprs: Vec::new(),
        })
    }

    /// The reader must accept exactly what the writer produces.
    #[test]
    fn round_trips_the_writers_container() -> TestResult {
        let dir = tempfile::tempdir()?;
        let writer = crate::writer::Writer::new(dir.path().to_path_buf(), false);
        let frames = vec![
            FrameRecord {
                seq: 0,
                ambe: SILENCE,
                slow_data: [1, 2, 3],
            },
            FrameRecord {
                seq: 1,
                ambe: [0x11; 9],
                slow_data: [4, 5, 6],
            },
        ];
        let rec = recording(frames).ok_or("fixture")?;
        let json_path = writer.write(&rec)?;
        let bytes = std::fs::read(json_path.with_extension("ambe"))?;
        let records = read_container(&bytes)?;
        assert_eq!(records.len(), 2);
        let first = records.first().ok_or("rec 0")?;
        assert_eq!(first.seq, 0);
        assert_eq!(first.ambe, SILENCE);
        assert_eq!(first.slow_data, [1, 2, 3]);
        let second = records.get(1).ok_or("rec 1")?;
        assert_eq!(second.seq, 1);
        Ok(())
    }

    #[test]
    fn rejects_foreign_bytes() {
        assert!(matches!(
            read_container(b"NOTOURSXxxxxxxxx"),
            Err(ContainerError::BadHeader)
        ));
        let mut bad = Vec::from(*b"STGZAMBE");
        bad.extend_from_slice(&[1, 0, 13, 0, 0, 0, 0, 0]);
        bad.extend_from_slice(&[0u8; 5]); // not a whole record
        assert!(matches!(
            read_container(&bad),
            Err(ContainerError::Truncated(5))
        ));
    }

    #[test]
    fn features_conceal_gaps_and_survive_wild_seq() {
        let rec = |seq: u8| ContainerRecord {
            seq,
            ambe: SILENCE,
            slow_data: [0; 3],
        };
        // 0,1,[2,3 missing],4 → 3 received + 2 concealed rows.
        let rows = extract_features(&[rec(0), rec(1), rec(4)]);
        assert_eq!(rows.len(), 5);
        // A corrupted seq byte must not underflow or conceal.
        let rows = extract_features(&[rec(3), rec(200), rec(4)]);
        assert_eq!(rows.len(), 3);
    }

    #[test]
    fn rows_have_voice_shaped_content() {
        let records = [ContainerRecord {
            seq: 0,
            ambe: SILENCE,
            slow_data: [0; 3],
        }];
        let rows = extract_features(&records);
        let row = rows.first().copied().unwrap_or([0.0; FEATURE_DIM]);
        let f0 = row.first().copied().unwrap_or(0.0);
        let harmonics = row.get(1).copied().unwrap_or(0.0);
        assert!(f0 > 0.0 && f0 < 500.0, "f0 {f0}");
        assert!((9.0..=56.0).contains(&harmonics), "L {harmonics}");
        // Mask plane marks exactly L bands valid.
        let mask_sum: f32 = row
            .get(2 + 2 * PARAM_BANDS..)
            .map_or(0.0, |m| m.iter().sum());
        assert!((mask_sum - harmonics).abs() < f32::EPSILON);
    }
}
