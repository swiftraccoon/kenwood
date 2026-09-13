//! Complete JSON-line writes, failure boundaries, and an offline capture benchmark.

use std::collections::VecDeque;
use std::time::Duration;

use serde::ser::{Error as _, SerializeStruct};

use super::*;

type TestResult = Result<(), Box<dyn StdError + Send + Sync>>;

struct Counted<W> {
    inner: W,
    writes: u64,
    flushes: u64,
}

impl<W> Counted<W> {
    const fn new(inner: W) -> Self {
        Self {
            inner,
            writes: 0,
            flushes: 0,
        }
    }
}

impl<W: Write> Write for Counted<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.writes += 1;
        self.inner.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flushes += 1;
        self.inner.flush()
    }
}

fn fixed_record() -> Record<Event<'static>> {
    Record {
        sequence: 7,
        utc_unix_nanoseconds: "0".to_owned(),
        elapsed_microseconds: 9,
        event: Event::ReadCompleted {
            bytes: &[0, 10, 34, 255],
        },
    }
}

#[test]
fn accepting_writer_receives_one_complete_json_line_then_one_flush() -> TestResult {
    let mut recorder = Recorder::new(Counted::new(Vec::new()), Arc::new(AtomicBool::new(false)));
    recorder.write_record(&fixed_record())?;
    assert_eq!(recorder.writer.writes, 1);
    assert_eq!(recorder.writer.flushes, 1);
    assert_eq!(recorder.writer.inner, b"{\"sequence\":7,\"utc_unix_nanoseconds\":\"0\",\"elapsed_microseconds\":9,\"event\":{\"kind\":\"read_completed\",\"bytes\":[0,10,34,255]}}\n");
    Ok(())
}

struct FailedSerialization;

impl Serialize for FailedSerialization {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut state = serializer.serialize_struct("Partial", 2)?;
        state.serialize_field("prefix", "serialized before failure")?;
        Err(S::Error::custom("intentional serialization failure"))
    }
}

#[test]
fn serialization_failure_writes_no_prefix_and_blocks_later_records() -> TestResult {
    let cancelled = Arc::new(AtomicBool::new(false));
    let mut recorder = Recorder::new(Counted::new(Vec::new()), Arc::clone(&cancelled));
    recorder.record(FailedSerialization);
    recorder.record(Event::CloseCompleted);
    assert_eq!(recorder.writer.writes, 0);
    assert_eq!(recorder.writer.flushes, 0);
    assert!(recorder.writer.inner.is_empty());
    assert_eq!(recorder.summary().events, 0);
    assert!(!recorder.summary().complete);
    assert!(
        recorder
            .summary()
            .error
            .ok_or("serialization error missing")?
            .message
            .contains("intentional serialization failure")
    );
    assert!(cancelled.load(Ordering::Relaxed));
    Ok(())
}

enum Step {
    Accept(usize),
    Interrupted,
    Failed,
}

#[derive(Default)]
struct PartialWriter {
    steps: VecDeque<Step>,
    bytes: Vec<u8>,
    flushes: u64,
    fail_flush: bool,
}

impl Write for PartialWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        match self.steps.pop_front() {
            Some(Step::Interrupted) => Err(io::ErrorKind::Interrupted.into()),
            Some(Step::Failed) => Err(io::Error::other("intentional write failure")),
            action => {
                let count = match action {
                    Some(Step::Accept(limit)) => limit.min(bytes.len()),
                    _ => bytes.len(),
                };
                self.bytes.extend_from_slice(
                    bytes
                        .get(..count)
                        .ok_or_else(|| io::Error::other("invalid test write count"))?,
                );
                Ok(count)
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flushes += 1;
        if self.fail_flush {
            Err(io::Error::other("intentional flush failure"))
        } else {
            Ok(())
        }
    }
}

#[test]
fn interrupted_and_short_writes_finish_the_same_complete_line() -> TestResult {
    let writer = PartialWriter {
        steps: VecDeque::from([Step::Interrupted, Step::Accept(7), Step::Accept(3)]),
        ..PartialWriter::default()
    };
    let mut recorder = Recorder::new(writer, Arc::new(AtomicBool::new(false)));
    recorder.write_record(&fixed_record())?;
    let mut expected = serde_json::to_vec(&fixed_record())?;
    expected.push(b'\n');
    assert_eq!(recorder.writer.bytes, expected);
    assert_eq!(recorder.writer.flushes, 1);
    Ok(())
}

#[test]
fn partial_write_and_write_zero_leave_failed_records_uncounted() -> TestResult {
    for step in [Step::Failed, Step::Accept(0)] {
        let cancelled = Arc::new(AtomicBool::new(false));
        let writer = PartialWriter {
            steps: VecDeque::from([Step::Accept(7), step]),
            ..PartialWriter::default()
        };
        let mut recorder = Recorder::new(Counted::new(writer), Arc::clone(&cancelled));
        recorder.record(Event::CloseRequested);
        let calls = recorder.writer.writes;
        let first = recorder
            .summary()
            .error
            .ok_or("original write error missing")?
            .message;
        recorder.record(Event::CloseCompleted);
        recorder.fail(&io::Error::other("later failure"));
        assert_eq!(recorder.writer.writes, calls);
        assert_eq!(recorder.writer.inner.bytes.len(), 7);
        assert_eq!(recorder.writer.flushes, 0);
        assert_eq!(recorder.summary().events, 0);
        assert!(!recorder.summary().complete);
        assert_eq!(
            recorder.summary().error.ok_or("write error lost")?.message,
            first
        );
        assert!(cancelled.load(Ordering::Relaxed));
    }
    Ok(())
}

#[test]
fn flush_failure_does_not_count_or_retry_the_written_line() -> TestResult {
    let cancelled = Arc::new(AtomicBool::new(false));
    let writer = PartialWriter {
        fail_flush: true,
        ..PartialWriter::default()
    };
    let mut recorder = Recorder::new(Counted::new(writer), Arc::clone(&cancelled));
    recorder.record(Event::CloseCompleted);
    let calls = recorder.writer.writes;
    recorder.record(Event::CloseRequested);
    assert_eq!(recorder.writer.writes, calls);
    assert_eq!(recorder.writer.flushes, 1);
    assert_eq!(recorder.writer.inner.bytes.last(), Some(&b'\n'));
    let parsed: serde_json::Value = serde_json::from_slice(&recorder.writer.inner.bytes)?;
    assert_eq!(parsed.get("sequence"), Some(&serde_json::Value::from(0)));
    assert_eq!(recorder.summary().events, 0);
    assert!(!recorder.summary().complete);
    assert_eq!(
        recorder
            .summary()
            .error
            .ok_or("flush error missing")?
            .message,
        "intentional flush failure"
    );
    assert!(cancelled.load(Ordering::Relaxed));
    Ok(())
}

#[test]
fn cancellation_does_not_skip_cleanup_capture_or_sequence_numbers() -> TestResult {
    let cancelled = Arc::new(AtomicBool::new(true));
    let mut recorder = Recorder::new(Counted::new(Vec::new()), Arc::clone(&cancelled));
    recorder.record(Event::CloseRequested);
    recorder.record(Event::CloseCompleted);
    assert_eq!(recorder.writer.writes, 2);
    assert_eq!(recorder.writer.flushes, 2);
    assert_eq!(recorder.summary().events, 2);
    assert!(recorder.summary().complete);
    let text = String::from_utf8(recorder.writer.inner)?;
    for (index, line) in text.lines().enumerate() {
        let row: serde_json::Value = serde_json::from_str(line)?;
        assert_eq!(row.get("sequence"), Some(&serde_json::Value::from(index)));
    }
    assert!(cancelled.load(Ordering::Relaxed));
    Ok(())
}

#[derive(Serialize, serde::Deserialize)]
struct Backup {
    kind: String,
    model: String,
    firmware: String,
    radio_type: String,
    pages: Vec<(u32, Vec<u8>)>,
}

fn backup_fixture() -> Backup {
    Backup {
        kind: "backup".to_owned(),
        model: "TM-D750".to_owned(),
        firmware: "1.02".to_owned(),
        radio_type: "K,2,1".to_owned(),
        pages: kenwood_tmd750::protocol::mcp::regions::menu_regions()
            .into_iter()
            .flat_map(kenwood_tmd750::Region::pages)
            .map(|page| (page.address().as_u32(), vec![0xA5; page.len()]))
            .collect(),
    }
}

fn benchmark_fixture() -> Result<Backup, Box<dyn StdError + Send + Sync>> {
    #[derive(serde::Deserialize)]
    struct Envelope {
        event: Backup,
    }
    let Some(path) = std::env::var_os("TMD750_CAPTURE_BENCH_INPUT") else {
        return Ok(backup_fixture());
    };
    let captured = fs::read_to_string(path)?;
    let first = captured.lines().next().ok_or("backup capture is empty")?;
    let envelope: Envelope = serde_json::from_str(first)?;
    assert_eq!(envelope.event.kind, "backup");
    Ok(envelope.event)
}

#[test]
#[ignore = "manual offline file-I/O timing; no hardware or fixed latency assertion"]
fn benchmark_full_configuration_record() -> TestResult {
    let backup = benchmark_fixture()?;
    assert_eq!(backup.pages.len(), 1_138);
    assert_eq!(
        backup
            .pages
            .iter()
            .map(|(_, bytes)| bytes.len())
            .sum::<usize>(),
        289_962
    );
    let directory = tempfile::tempdir()?;
    let mut elapsed = Vec::new();
    for sample in 0..5 {
        let path = directory.path().join(format!("backup-{sample}.jsonl"));
        let writer = Counted::new(create_private_file(&path)?);
        let mut recorder = Recorder::new(writer, Arc::new(AtomicBool::new(false)));
        let started = Instant::now();
        recorder.record(&backup);
        let record_time = started.elapsed();
        recorder.ensure_complete()?;
        recorder.writer.flush()?;
        let syncing = Instant::now();
        recorder.writer.inner.sync_all()?;
        let sync_time = syncing.elapsed();
        let total = started.elapsed();
        assert_eq!(recorder.summary().events, 1);
        let captured: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
        assert_eq!(captured.get("event"), Some(&serde_json::to_value(&backup)?));
        eprintln!(
            "backup sample={sample} pages=1138 raw_bytes=289962 json_bytes={} writes={} flushes={} record_us={} sync_us={} total_us={}",
            fs::metadata(&path)?.len(),
            recorder.writer.writes,
            recorder.writer.flushes,
            record_time.as_micros(),
            sync_time.as_micros(),
            total.as_micros()
        );
        elapsed.push(total);
    }
    elapsed.sort_unstable();
    eprintln!(
        "backup total_us min={} median={} max={}",
        elapsed.first().map_or(0, Duration::as_micros),
        elapsed.get(2).map_or(0, Duration::as_micros),
        elapsed.last().map_or(0, Duration::as_micros)
    );
    Ok(())
}
