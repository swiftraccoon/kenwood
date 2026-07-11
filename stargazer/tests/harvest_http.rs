// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later

//! End-to-end harvest against a canned HTTP server: dry-run plans
//! without writing, a real run downloads and records provenance, and
//! a re-run is idempotent (retrying only past failures).

use std::collections::HashMap;
use std::io::{Read as _, Write as _};
use std::net::TcpListener;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use stargazer::harvest::{HarvestOptions, Harvester, ManifestRecord};

// Compilation-unit dep acknowledgements (unused_crate_dependencies):
use chrono as _;
use clap as _;
use dstar_gateway as _;
use dstar_gateway_core as _;
use mbelib_rs as _;
use reqwest as _;
use serde as _;
use thiserror as _;
use toml as _;
use tracing as _;
use tracing_subscriber as _;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const DATE_PATH: &str = "/streams/2026/07/11/";
const STEM_A: &str = "REF030-C--20260711-175407.594--MM3TWA--e658";
const STEM_B: &str = "REF030-C--20260711-180144.540--WB3JSW--9485";
const STEM_C: &str = "REF030-C--20260711-181000.000--N0CALL--0c0c";

fn listing_html() -> String {
    use std::fmt::Write as _;
    let mut rows = String::from("<html><body><h1>Index of /streams/2026/07/11</h1><table>");
    rows.push_str("<tr><td><a href=\"/streams/2026/07/\">Parent Directory</a></td></tr>");
    for stem in [STEM_A, STEM_B] {
        for ext in ["txt", "mp3", "dvrec"] {
            let _unused = write!(
                rows,
                "<tr><td><a href=\"{stem}.{ext}\">{stem}.{ext}</a></td></tr>"
            );
        }
    }
    // Listed but deleted server-side before we fetch it (retention
    // race): the file route below intentionally 404s.
    let _unused = write!(
        rows,
        "<tr><td><a href=\"{STEM_C}.mp3\">{STEM_C}.mp3</a></td></tr>"
    );
    rows.push_str("</table></body></html>");
    rows
}

/// Canned routes: path → (status, body).
fn routes() -> HashMap<String, (u16, Vec<u8>)> {
    let mut map = HashMap::new();
    let _unused = map.insert(DATE_PATH.to_string(), (200, listing_html().into_bytes()));
    for (stem, tag) in [(STEM_A, "A"), (STEM_B, "B")] {
        for ext in ["txt", "mp3", "dvrec"] {
            let _unused = map.insert(
                format!("{DATE_PATH}{stem}.{ext}"),
                (200, format!("{tag}-{ext}-body").into_bytes()),
            );
        }
    }
    // STEM_C.mp3 deliberately absent → 404.
    map
}

/// Minimal blocking HTTP/1.1 responder on a detached thread.
fn spawn_server() -> Result<String, Box<dyn std::error::Error>> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let addr = listener.local_addr()?;
    let table = Arc::new(routes());
    let _unused = std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut sock) = stream else { return };
            let table = Arc::clone(&table);
            let mut buf = Vec::new();
            let mut chunk = [0u8; 1024];
            loop {
                match sock.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        buf.extend_from_slice(chunk.get(..n).unwrap_or_default());
                        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                            break;
                        }
                    }
                }
            }
            let text = String::from_utf8_lossy(&buf);
            let path = text.split_whitespace().nth(1).unwrap_or("/").to_string();
            let (status, body) = table
                .get(&path)
                .cloned()
                .unwrap_or_else(|| (404, b"not found".to_vec()));
            let reason = if status == 200 { "OK" } else { "Not Found" };
            let head = format!(
                "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _unused = sock.write_all(head.as_bytes());
            let _unused = sock.write_all(&body);
        }
    });
    Ok(format!("http://{addr}/"))
}

/// A real-shaped local sidecar matching `STEM_A` (gap-free).
fn write_local_recording(date_dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(date_dir)?;
    std::fs::write(
        date_dir.join("20260711T175407Z_MM3TWA_E658.json"),
        r#"{
          "schema": "stargazer-recording/1",
          "stream_id": "E658",
          "started_at": "2026-07-11T17:54:07.608Z",
          "frames": { "received": 3975, "expected": 3975, "gaps": 0 },
          "header": { "my_callsign": "MM3TWA" }
        }"#,
    )
}

fn read_manifest(published: &Path) -> Result<Vec<ManifestRecord>, Box<dyn std::error::Error>> {
    let text = std::fs::read_to_string(published.join("harvest.jsonl"))?;
    let mut out = Vec::new();
    for line in text.lines() {
        out.push(serde_json::from_str(line)?);
    }
    Ok(out)
}

fn harvester(base_url: &str, dry_run: bool) -> Result<Harvester, Box<dyn std::error::Error>> {
    Ok(Harvester::new(HarvestOptions {
        base_url: Some(base_url.to_string()),
        limit: None,
        dry_run,
        fetch_gap: Duration::ZERO,
    })?)
}

#[tokio::test]
async fn harvest_end_to_end_dry_run_real_run_and_idempotent_rerun() -> TestResult {
    let base_url = spawn_server()?;
    let recordings = tempfile::tempdir()?;
    let date_dir = recordings.path().join("REF030-C").join("2026-07-11");
    write_local_recording(&date_dir)?;
    let date = chrono::NaiveDate::from_ymd_opt(2026, 7, 11).ok_or("date")?;
    let published = date_dir.join("published");

    // -- Phase 1: dry run plans but writes nothing --
    let rec = harvester(&base_url, true)?
        .harvest_dir(recordings.path(), "REF030-C", date)
        .await?;
    assert_eq!(rec.http_status, Some(200));
    assert_eq!(rec.published_tx, 3);
    assert_eq!(rec.local_recordings, 1);
    assert_eq!(rec.matched, 1);
    assert_eq!(rec.published_only, 2);
    assert_eq!(rec.local_only, 0);
    // A: mp3+txt (paired, gap-free) + B: all three (salvage) + C: mp3.
    assert_eq!(rec.planned, 6);
    assert_eq!(rec.downloaded, 0);
    assert!(rec.dry_run);
    assert!(!published.exists(), "dry run must not create published/");

    // -- Phase 2: real run downloads and records provenance --
    let rec = harvester(&base_url, false)?
        .harvest_dir(recordings.path(), "REF030-C", date)
        .await?;
    assert_eq!(rec.planned, 6);
    assert_eq!(rec.downloaded, 5, "{rec:?}");
    assert_eq!(rec.failed, 1, "the 404'd salvage mp3");
    assert_eq!(rec.skipped_existing, 0);

    let mp3 = std::fs::read(published.join(format!("{STEM_A}.mp3")))?;
    assert_eq!(mp3, b"A-mp3-body");
    for ext in ["txt", "mp3", "dvrec"] {
        assert!(
            published.join(format!("{STEM_B}.{ext}")).exists(),
            "salvage fetches everything published"
        );
    }
    assert!(
        !published.join(format!("{STEM_A}.dvrec")).exists(),
        "gap-free pair skips the dvrec"
    );
    assert!(
        !published.join(format!("{STEM_C}.mp3")).exists(),
        "failed download leaves no file"
    );
    // No temp litter.
    let litter: Vec<String> = std::fs::read_dir(&published)?
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| Path::new(n).extension() == Some("tmp".as_ref()))
        .collect();
    assert!(litter.is_empty(), "{litter:?}");

    let manifest = read_manifest(&published)?;
    let files = manifest
        .iter()
        .filter(|r| matches!(r, ManifestRecord::File(_)))
        .count();
    let runs = manifest
        .iter()
        .filter(|r| matches!(r, ManifestRecord::Run(_)))
        .count();
    assert_eq!((files, runs), (6, 1), "6 attempts + 1 run summary");
    let failed_records: Vec<&ManifestRecord> = manifest
        .iter()
        .filter(|r| matches!(r, ManifestRecord::File(f) if f.error.is_some()))
        .collect();
    assert_eq!(failed_records.len(), 1);
    assert!(
        matches!(
            failed_records.first(),
            Some(ManifestRecord::File(f)) if f.file_name.starts_with(STEM_C)
        ),
        "{failed_records:?}"
    );
    let paired = manifest
        .iter()
        .find(|r| matches!(r, ManifestRecord::File(f) if f.file_name == format!("{STEM_A}.mp3")));
    assert!(
        matches!(
            paired,
            Some(ManifestRecord::File(f))
                if f.local_stem.as_deref() == Some("20260711T175407Z_MM3TWA_E658")
        ),
        "paired download carries the local stem"
    );

    // -- Phase 3: re-run is idempotent, retrying only the failure --
    let rec = harvester(&base_url, false)?
        .harvest_dir(recordings.path(), "REF030-C", date)
        .await?;
    assert_eq!(rec.skipped_existing, 5);
    assert_eq!(rec.planned, 1, "only the previously failed file");
    assert_eq!(rec.downloaded, 0);
    assert_eq!(rec.failed, 1);
    let manifest = read_manifest(&published)?;
    assert_eq!(manifest.len(), 7 + 2, "one file attempt + one run appended");
    Ok(())
}

#[tokio::test]
async fn harvest_records_listing_absence_without_failing() -> TestResult {
    let base_url = spawn_server()?;
    let recordings = tempfile::tempdir()?;
    std::fs::create_dir_all(recordings.path().join("REF030-C"))?;
    // A date the server has nothing for → 404 listing, zero coverage.
    let date = chrono::NaiveDate::from_ymd_opt(2026, 7, 10).ok_or("date")?;
    let rec = harvester(&base_url, false)?
        .harvest_dir(recordings.path(), "REF030-C", date)
        .await?;
    assert_eq!(rec.http_status, Some(404));
    assert_eq!(rec.published_tx, 0);
    assert_eq!(rec.planned, 0);
    assert!(rec.error.is_none(), "nothing-published is not an error");
    assert!(
        !recordings
            .path()
            .join("REF030-C")
            .join("2026-07-10")
            .exists(),
        "a nothing-day must not create directory litter"
    );
    Ok(())
}

#[tokio::test]
async fn harvest_respects_download_limit() -> TestResult {
    let base_url = spawn_server()?;
    let recordings = tempfile::tempdir()?;
    let date_dir = recordings.path().join("REF030-C").join("2026-07-11");
    write_local_recording(&date_dir)?;
    let date = chrono::NaiveDate::from_ymd_opt(2026, 7, 11).ok_or("date")?;

    let harvester = Harvester::new(HarvestOptions {
        base_url: Some(base_url),
        limit: Some(2),
        dry_run: false,
        fetch_gap: Duration::ZERO,
    })?;
    let rec = harvester
        .harvest_dir(recordings.path(), "REF030-C", date)
        .await?;
    assert_eq!(rec.downloaded + rec.failed, 2, "{rec:?}");
    assert!(rec.truncated);
    Ok(())
}
