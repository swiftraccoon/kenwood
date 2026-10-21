// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later

//! End-to-end harvest against a canned HTTP server: dry-run plans
//! without writing, a real run downloads and records provenance, and
//! a re-run is idempotent (retrying only past failures).

use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use stargazer::harvest::{FetchStage, HarvestFailure, HarvestOptions, Harvester, ManifestRecord};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpListener;

// Compilation-unit dep acknowledgements (unused_crate_dependencies):
use chrono as _;
use clap as _;
use dstar_gateway as _;
use dstar_gateway_core::RoutingCallsign;
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

#[derive(Debug)]
struct TestServer {
    base_url: String,
    requests: Arc<Mutex<Vec<(String, tokio::time::Instant)>>>,
    task: tokio::task::JoinHandle<std::io::Result<()>>,
}

impl TestServer {
    fn url(&self) -> &str {
        &self.base_url
    }
    fn requests(&self) -> Result<Vec<(String, tokio::time::Instant)>, Box<dyn std::error::Error>> {
        Ok(self
            .requests
            .lock()
            .map_err(|error| std::io::Error::other(error.to_string()))?
            .clone())
    }
    async fn stop(mut self) -> TestResult {
        self.task.abort();
        match (&mut self.task).await {
            Ok(result) => Ok(result?),
            Err(error) if error.is_cancelled() => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn spawn_server(
    table: HashMap<String, (u16, Vec<u8>)>,
) -> Result<TestServer, Box<dyn std::error::Error>> {
    let wire = table
        .into_iter()
        .map(|(path, (status, body))| {
            let mut response = format!(
                "HTTP/1.1 {status} Response\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .into_bytes();
            response.extend_from_slice(&body);
            (path, response)
        })
        .collect();
    spawn_wire_server(wire).await
}

async fn spawn_wire_server(
    table: HashMap<String, Vec<u8>>,
) -> Result<TestServer, Box<dyn std::error::Error>> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let requests = Arc::new(Mutex::new(Vec::new()));
    let observed = Arc::clone(&requests);
    let task = tokio::spawn(async move {
        loop {
            let (mut sock, _peer) = listener.accept().await?;
            let mut buf = Vec::new();
            let mut chunk = [0u8; 1024];
            loop {
                match sock.read(&mut chunk).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        buf.extend_from_slice(chunk.get(..n).unwrap_or_default());
                        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                            break;
                        }
                        if buf.len() > 16_384 {
                            return Err(std::io::Error::other("fixture request too long"));
                        }
                    }
                }
            }
            let text = String::from_utf8_lossy(&buf);
            let path = text.split_whitespace().nth(1).unwrap_or("/").to_string();
            observed
                .lock()
                .map_err(|error| std::io::Error::other(error.to_string()))?
                .push((path.clone(), tokio::time::Instant::now()));
            let response = table.get(&path).map_or(
                b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    .as_slice(),
                Vec::as_slice,
            );
            let _written = sock.write_all(response).await;
        }
    });
    Ok(TestServer {
        base_url: format!("http://{addr}/"),
        requests,
        task,
    })
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
    Ok(Harvester::new(
        HarvestOptions::new(RoutingCallsign::new("N0CALL")?)
            .with_base_url(base_url.parse()?)
            .with_dry_run(dry_run),
    )?)
}

#[tokio::test]
async fn harvest_end_to_end_dry_run_real_run_and_idempotent_rerun() -> TestResult {
    let server = spawn_server(routes()).await?;
    let base_url = server.url();
    let recordings = tempfile::tempdir()?;
    let date_dir = recordings.path().join("REF030-C").join("2026-07-11");
    write_local_recording(&date_dir)?;
    let date = chrono::NaiveDate::from_ymd_opt(2026, 7, 11).ok_or("date")?;
    let published = date_dir.join("published");

    // -- Phase 1: dry run plans but writes nothing --
    let rec = harvester(base_url, true)?
        .harvest_dir(recordings.path(), &"REF030-C".parse()?, date)
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
    let rec = harvester(base_url, false)?
        .harvest_dir(recordings.path(), &"REF030-C".parse()?, date)
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
    let mut litter = Vec::new();
    for entry in std::fs::read_dir(&published)? {
        let name = entry?.file_name().to_string_lossy().into_owned();
        if Path::new(&name).extension() == Some("tmp".as_ref()) {
            litter.push(name);
        }
    }
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
    let rec = harvester(base_url, false)?
        .harvest_dir(recordings.path(), &"REF030-C".parse()?, date)
        .await?;
    assert_eq!(rec.skipped_existing, 5);
    assert_eq!(rec.planned, 1, "only the previously failed file");
    assert_eq!(rec.downloaded, 0);
    assert_eq!(rec.failed, 1);
    let manifest = read_manifest(&published)?;
    assert_eq!(manifest.len(), 7 + 2, "one file attempt + one run appended");
    server.stop().await?;
    Ok(())
}

#[tokio::test]
async fn harvest_records_listing_absence_without_failing() -> TestResult {
    let server = spawn_server(routes()).await?;
    let base_url = server.url();
    let recordings = tempfile::tempdir()?;
    std::fs::create_dir_all(recordings.path().join("REF030-C"))?;
    // A date the server has nothing for → 404 listing, zero coverage.
    let date = chrono::NaiveDate::from_ymd_opt(2026, 7, 10).ok_or("date")?;
    let rec = harvester(base_url, false)?
        .harvest_dir(recordings.path(), &"REF030-C".parse()?, date)
        .await?;
    assert_eq!(rec.http_status, Some(404));
    assert_eq!(rec.published_tx, 0);
    assert_eq!(rec.planned, 0);
    assert!(rec.error().is_none(), "nothing-published is not an error");
    assert!(
        !recordings
            .path()
            .join("REF030-C")
            .join("2026-07-10")
            .exists(),
        "a nothing-day must not create directory litter"
    );
    server.stop().await?;
    Ok(())
}

#[tokio::test]
async fn harvest_respects_download_limit() -> TestResult {
    let server = spawn_server(routes()).await?;
    let base_url = server.url();
    let recordings = tempfile::tempdir()?;
    let date_dir = recordings.path().join("REF030-C").join("2026-07-11");
    write_local_recording(&date_dir)?;
    let date = chrono::NaiveDate::from_ymd_opt(2026, 7, 11).ok_or("date")?;

    let mut harvester = Harvester::new(
        HarvestOptions::new(RoutingCallsign::new("N0CALL")?)
            .with_base_url(base_url.parse()?)
            .with_limit(NonZeroUsize::new(2).ok_or("limit")?),
    )?;
    let rec = harvester
        .harvest_dir(recordings.path(), &"REF030-C".parse()?, date)
        .await?;
    assert_eq!(rec.downloaded + rec.failed, 2, "{rec:?}");
    assert!(rec.truncated);
    let downloads: Vec<_> = server
        .requests()?
        .into_iter()
        .filter(|(path, _)| path.starts_with(DATE_PATH) && path != DATE_PATH)
        .collect();
    let first = downloads.first().ok_or("first download")?;
    let second = downloads.get(1).ok_or("second download")?;
    assert!(second.1.duration_since(first.1) >= Duration::from_secs(2));
    server.stop().await?;
    Ok(())
}

#[tokio::test]
async fn robots_disallow_stops_the_run_before_the_listing() -> TestResult {
    let mut table = routes();
    let _unused = table.insert(
        "/robots.txt".to_string(),
        (200, b"User-agent: *\nDisallow: /streams/\n".to_vec()),
    );
    let server = spawn_server(table).await?;
    let base_url = server.url();
    let recordings = tempfile::tempdir()?;
    let date_dir = recordings.path().join("REF030-C").join("2026-07-11");
    write_local_recording(&date_dir)?;
    let date = chrono::NaiveDate::from_ymd_opt(2026, 7, 11).ok_or("date")?;

    let rec = harvester(base_url, false)?
        .harvest_dir(recordings.path(), &"REF030-C".parse()?, date)
        .await?;
    assert!(rec.error().is_some_and(|e| e.contains("robots")), "{rec:?}");
    assert_eq!(rec.published_tx, 0, "listing never fetched");
    assert_eq!(rec.planned, 0);
    assert_eq!(rec.downloaded, 0);
    assert!(
        !date_dir.join("published").exists(),
        "a robots-blocked run leaves no trace"
    );
    assert_eq!(
        server
            .requests()?
            .iter()
            .map(|(path, _)| path.as_str())
            .collect::<Vec<_>>(),
        vec!["/robots.txt"]
    );
    server.stop().await?;
    Ok(())
}

#[tokio::test]
async fn consecutive_failures_trip_the_breaker() -> TestResult {
    // Listing advertises four salvage transmissions whose files have
    // all been purged server-side (every download 404s): the breaker
    // must stop after three straight failures instead of marching on.
    use std::fmt::Write as _;
    let mut html = String::from("<html><body><table>");
    for sid in ["aa01", "aa02", "aa03", "aa04"] {
        let _unused = write!(
            html,
            "<tr><td><a href=\"REF030-C--20260711-190000.000--K0GNE--{sid}.mp3\">x</a></td></tr>"
        );
    }
    html.push_str("</table></body></html>");
    let mut table = HashMap::new();
    let _unused = table.insert(DATE_PATH.to_string(), (200, html.into_bytes()));
    let server = spawn_server(table).await?;
    let base_url = server.url();

    let recordings = tempfile::tempdir()?;
    std::fs::create_dir_all(recordings.path().join("REF030-C"))?;
    let date = chrono::NaiveDate::from_ymd_opt(2026, 7, 11).ok_or("date")?;
    let rec = harvester(base_url, false)?
        .harvest_dir(recordings.path(), &"REF030-C".parse()?, date)
        .await?;
    assert_eq!(rec.planned, 4);
    assert_eq!(
        rec.failed, 3,
        "stopped at the breaker, not the plan: {rec:?}"
    );
    assert_eq!(rec.downloaded, 0);
    assert!(
        rec.error().is_some_and(|e| e.contains("consecutive")),
        "{rec:?}"
    );
    server.stop().await?;
    Ok(())
}

#[tokio::test]
async fn robots_server_failure_prevents_listing_and_downloads() -> TestResult {
    let mut table = routes();
    let _previous = table.insert("/robots.txt".to_owned(), (503, b"unavailable".to_vec()));
    let server = spawn_server(table).await?;
    let directory = tempfile::tempdir()?;
    let date = chrono::NaiveDate::from_ymd_opt(2026, 7, 11).ok_or("date")?;
    let record = harvester(server.url(), false)?
        .harvest_dir(directory.path(), &"REF030-C".parse()?, date)
        .await?;
    assert!(
        matches!(record.failure(), Some(HarvestFailure::HttpStatus { stage: FetchStage::Robots, status }) if *status == reqwest::StatusCode::SERVICE_UNAVAILABLE),
        "{record:?}"
    );
    assert_eq!(
        server
            .requests()?
            .iter()
            .map(|(path, _)| path.as_str())
            .collect::<Vec<_>>(),
        vec!["/robots.txt"]
    );
    assert_eq!(std::fs::read_dir(directory.path())?.count(), 0);
    server.stop().await?;
    Ok(())
}

#[tokio::test]
async fn robots_wildcards_and_allow_exceptions_filter_individual_artifacts() -> TestResult {
    let mut table = routes();
    let robots = format!(
        "User-agent: stargazer-harvest\nDisallow: /*.mp3$\nAllow: {DATE_PATH}{STEM_A}.mp3$\n"
    );
    let _previous = table.insert("/robots.txt".to_owned(), (200, robots.into_bytes()));
    let server = spawn_server(table).await?;
    let directory = tempfile::tempdir()?;
    let date = chrono::NaiveDate::from_ymd_opt(2026, 7, 11).ok_or("date")?;
    let record = harvester(server.url(), true)?
        .harvest_dir(directory.path(), &"REF030-C".parse()?, date)
        .await?;
    assert_eq!(record.blocked_by_robots, 2);
    assert_eq!(record.planned, 5);
    assert_eq!(server.requests()?.len(), 2);
    server.stop().await?;
    Ok(())
}

#[tokio::test]
async fn declared_and_chunked_responses_enforce_the_selected_bound() -> TestResult {
    for response in [
        b"HTTP/1.1 200 OK\r\nContent-Length: 64\r\nConnection: close\r\n\r\n0123456789012345678901234567890123456789012345678901234567890123".to_vec(),
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n40\r\n0123456789012345678901234567890123456789012345678901234567890123\r\n0\r\n\r\n".to_vec(),
    ] {
        let server = spawn_wire_server(HashMap::from([(DATE_PATH.to_owned(), response)])).await?;
        let directory = tempfile::tempdir()?;
        let maximum = NonZeroUsize::new(32).ok_or("limit")?;
        let options = HarvestOptions::new(RoutingCallsign::new("N0CALL")?).with_base_url(server.url().parse()?).with_response_limit(maximum).with_dry_run(true);
        let date = chrono::NaiveDate::from_ymd_opt(2026, 7, 11).ok_or("date")?;
        let record = Harvester::new(options)?.harvest_dir(directory.path(), &"REF030-C".parse()?, date).await?;
        assert!(matches!(record.failure(), Some(HarvestFailure::Fetch { stage: FetchStage::Listing, source: stargazer::FetchError::TooLarge { maximum: actual } }) if *actual == maximum), "{record:?}");
        assert_eq!(record.planned, 0);
        server.stop().await?;
    }
    Ok(())
}

#[tokio::test]
async fn redirect_chain_stops_at_the_client_limit() -> TestResult {
    let mut table = HashMap::new();
    for index in 0..7 {
        let path = if index == 0 {
            DATE_PATH.to_owned()
        } else {
            format!("/redirect{index}")
        };
        let _previous = table.insert(path, format!("HTTP/1.1 302 Found\r\nLocation: /redirect{}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n", index + 1).into_bytes());
    }
    let server = spawn_wire_server(table).await?;
    let directory = tempfile::tempdir()?;
    let date = chrono::NaiveDate::from_ymd_opt(2026, 7, 11).ok_or("date")?;
    let record = harvester(server.url(), true)?
        .harvest_dir(directory.path(), &"REF030-C".parse()?, date)
        .await?;
    assert!(
        matches!(record.failure(), Some(HarvestFailure::Fetch { stage: FetchStage::Listing, source: stargazer::FetchError::Http(error) }) if error.is_redirect()),
        "{record:?}"
    );
    assert!(server.requests()?.len() <= 7);
    server.stop().await?;
    Ok(())
}
