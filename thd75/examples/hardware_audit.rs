//! Allowlisted raw-CAT hardware qualification for the TH-D75.
//!
//! This runner deliberately bypasses the typed command serializers and
//! parsers being audited. It has no arbitrary-command mode.
//!
//! Read-only baseline:
//!
//! ```text
//! cargo run -p kenwood-thd75 --example hardware_audit -- \
//!   baseline --port /dev/cu.usbmodem101 \
//!   --capture-root /private/path/thd75-audit
//! ```
//!
//! On macOS, the same fixed read-only allowlist can use the native RFCOMM
//! transport and can machine-check its on-wire preflight without reading stdin:
//!
//! ```text
//! cargo run -p kenwood-thd75 --example hardware_audit -- \
//!   baseline --bluetooth TH-D75 --machine-checked-read-only \
//!   --capture-root /private/path/thd75-audit
//! ```
//!
//! Exact V1.03.AZM automation-firmware baseline (machine-checked only):
//!
//! ```text
//! cargo run -p kenwood-thd75 --example hardware_audit -- \
//!   baseline --automation --bluetooth TH-D75 \
//!   --machine-checked-read-only --capture-root /private/path/thd75-audit
//! ```
//!
//! This profile first proves the complete V1.03.AZM image and ABI through
//! `Radio::qualify_automation`, reopens the same transport identity, and
//! uses that fresh connection for the fixed CAT audit. The CAT allowlist is
//! the stock 61-frame baseline minus only bare `GM` and bare `GW`, whose
//! handlers V1.03.AZM deliberately repurposes.
//!
//! The compiled containment implementation is deliberately disabled for bench
//! execution until a private MCP pre-image can exist before its first write.

// Dependencies visible to every kenwood-thd75 example target but unused here.
// Acknowledged so `unused_crate_dependencies` stays silent without weakening
// the lint configuration.
use aprs as _;
use aprs_is as _;
use ax25_codec as _;
use dstar_gateway_core as _;
use kiss_tnc as _;
use mmdvm as _;
use mmdvm_core as _;
use proptest as _;
use thiserror as _;
use tokio_serial as _;
use tracing as _;

use std::error::Error;
use std::fs::{DirBuilder, File, OpenOptions};
use std::io::{self, BufWriter, Write};
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use kenwood_thd75::error::TransportError;
use kenwood_thd75::protocol::Codec;
use kenwood_thd75::radio::automation::AutomationAbi;
#[cfg(target_os = "macos")]
use kenwood_thd75::transport::BluetoothTransport;
use kenwood_thd75::transport::{EitherTransport, SerialTransport, Transport};
use kenwood_thd75::{Error as RadioError, Radio};
use serde_json::{Map, Value, json};

type AuditError = Box<dyn Error + Send + Sync>;
type AuditResult<T> = Result<T, AuditError>;

const CAT_BAUD: u32 = 115_200;
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(2);
const DRAIN_WINDOW: Duration = Duration::from_millis(30);
const DRAIN_TOTAL_TIMEOUT: Duration = Duration::from_millis(250);
const MAX_DRAIN_BYTES: usize = 64 * 1024;
const CLOSE_TIMEOUT: Duration = Duration::from_secs(2);
const EXPECTED_TY: &[u8] = b"TY K,2";
const STATIC_RE_IMAGE_SHA256: &str =
    "193963ca4b7a38392815686893858eec20292b629fe999f10b93a22a3a8e4001";
const OPERATOR_ASSERTIONS: &[&str] = &[
    "packet/TNC mode is Off",
    "APRS Beacon TX Method is Manual",
    "VOX is Off",
    "TX Inhibit is On",
    "scanning is stopped",
    "the lowest RF power is selected",
    "APRS MyCallsign was inspected and corrected if needed",
    "the intended antenna path is selected",
    "headphones and amplified speakers are disconnected",
    "the TUI, REPL, and every other process that could own or use the port are closed",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Baseline,
    MakeSafe,
}

impl Mode {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Baseline => "baseline",
            Self::MakeSafe => "make-safe",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BaselineProfile {
    StockDefault,
    Automation,
}

impl BaselineProfile {
    const fn as_str(self) -> &'static str {
        match self {
            Self::StockDefault => "stock-default",
            Self::Automation => "automation",
        }
    }

    fn includes_rest_spec(self, spec: &CommandSpec) -> bool {
        self == Self::StockDefault || (spec.wire != b"GM\r" && spec.wire != b"GW\r")
    }

    fn case_count(self) -> usize {
        IDENTITY_READS.len() + BASELINE_PREFLIGHT.len() + baseline_rest_specs(self).count()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Endpoint {
    Usb(String),
    Bluetooth(String),
}

fn insert_evidence(record: &mut Map<String, Value>, key: &str, value: Value) {
    drop(record.insert(key.to_owned(), value));
}

impl Endpoint {
    const fn transport_name(&self) -> &'static str {
        match self {
            Self::Usb(_) => "usb-cdc",
            Self::Bluetooth(_) => "bluetooth-rfcomm",
        }
    }

    fn value(&self) -> &str {
        match self {
            Self::Usb(port) | Self::Bluetooth(port) => port,
        }
    }

    fn add_evidence_fields(&self, record: &mut Map<String, Value>, privacy: EvidencePrivacy) {
        insert_evidence(record, "transport", json!(self.transport_name()));
        let endpoint = privacy.endpoint_value(self.value());
        insert_evidence(record, "endpoint", endpoint.clone());
        if let Self::Usb(_) = self {
            insert_evidence(record, "port", endpoint);
            insert_evidence(record, "baud", json!(CAT_BAUD));
            insert_evidence(record, "usb_vid", json!("2166"));
            insert_evidence(record, "usb_pid", json!("9023"));
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum EvidencePrivacy {
    Private,
    Sanitized,
}

impl EvidencePrivacy {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Private => "private",
            Self::Sanitized => "sanitized",
        }
    }

    fn endpoint_value(self, endpoint: &str) -> Value {
        match self {
            Self::Private => json!(endpoint),
            Self::Sanitized => json!({
                "$redacted": "endpoint",
                "byte_len": endpoint.len(),
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Config {
    mode: Mode,
    profile: BaselineProfile,
    endpoint: Endpoint,
    capture_root: PathBuf,
    machine_checked_read_only: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AutomationAttestation {
    abi: AutomationAbi,
    transport_reopened_before_raw_audit: bool,
}

#[derive(Debug)]
struct CaptureFiles {
    capture_id: String,
    session_dir: PathBuf,
    private: BufWriter<File>,
    sanitized: BufWriter<File>,
}

#[derive(Debug, Clone, Copy)]
struct CommandSpec {
    probe_id: &'static str,
    wire: &'static [u8],
    sensitive: bool,
    response_prefix: Option<&'static [u8]>,
    state_change: bool,
}

impl CommandSpec {
    const fn public(probe_id: &'static str, wire: &'static [u8]) -> Self {
        Self {
            probe_id,
            wire,
            sensitive: false,
            response_prefix: None,
            state_change: false,
        }
    }

    const fn sensitive(probe_id: &'static str, wire: &'static [u8]) -> Self {
        Self {
            probe_id,
            wire,
            sensitive: true,
            response_prefix: None,
            state_change: false,
        }
    }

    const fn indexed_public(
        probe_id: &'static str,
        wire: &'static [u8],
        response_prefix: &'static [u8],
    ) -> Self {
        Self {
            probe_id,
            wire,
            sensitive: false,
            response_prefix: Some(response_prefix),
            state_change: false,
        }
    }

    const fn indexed_sensitive(
        probe_id: &'static str,
        wire: &'static [u8],
        response_prefix: &'static [u8],
    ) -> Self {
        Self {
            probe_id,
            wire,
            sensitive: true,
            response_prefix: Some(response_prefix),
            state_change: false,
        }
    }

    const fn exact_echo(
        probe_id: &'static str,
        wire: &'static [u8],
        response: &'static [u8],
    ) -> Self {
        Self {
            probe_id,
            wire,
            sensitive: false,
            response_prefix: Some(response),
            state_change: true,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct ContainmentTarget {
    name: &'static str,
    write: CommandSpec,
    read: CommandSpec,
    expected: &'static [u8],
}

#[derive(Debug)]
struct Exchange {
    drained: Vec<u8>,
    drain_truncated: bool,
    received: Vec<u8>,
    unsolicited: Vec<Vec<u8>>,
    terminal: ExchangeTerminal,
}

#[derive(Debug)]
enum ExchangeTerminal {
    Response(Vec<u8>),
    Failure { code: &'static str, detail: String },
}

impl Exchange {
    fn response(&self) -> Option<&[u8]> {
        match &self.terminal {
            ExchangeTerminal::Response(response) => Some(response),
            ExchangeTerminal::Failure { .. } => None,
        }
    }

    const fn terminal_code(&self) -> &'static str {
        match &self.terminal {
            ExchangeTerminal::Response(response) => match response.as_slice() {
                b"N" => "radio-n",
                b"?" => "radio-question",
                _ => "response",
            },
            ExchangeTerminal::Failure { code, .. } => code,
        }
    }

    fn failure_detail(&self) -> Option<&str> {
        match &self.terminal {
            ExchangeTerminal::Response(_) => None,
            ExchangeTerminal::Failure { detail, .. } => Some(detail),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CaseStatus {
    Pass,
    Inconclusive,
    Fail,
}

impl CaseStatus {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Inconclusive => "inconclusive",
            Self::Fail => "fail",
        }
    }
}

#[derive(Debug, Default)]
struct CaseSummary {
    passed: usize,
    inconclusive: usize,
    failed: usize,
}

impl CaseSummary {
    const fn add(&mut self, status: CaseStatus) {
        match status {
            CaseStatus::Pass => self.passed = self.passed.saturating_add(1),
            CaseStatus::Inconclusive => {
                self.inconclusive = self.inconclusive.saturating_add(1);
            }
            CaseStatus::Fail => self.failed = self.failed.saturating_add(1),
        }
    }

    const fn is_all_pass(&self) -> bool {
        self.inconclusive == 0 && self.failed == 0
    }
}

const IDENTITY_READS: &[CommandSpec] = &[
    CommandSpec::public("A1-P0-ID-READ", b"ID\r"),
    CommandSpec::public("A1-P0-FV-READ", b"FV\r"),
    CommandSpec::public("A1-P0-TY-READ", b"TY\r"),
];

const BASELINE_PREFLIGHT: &[CommandSpec] = &[
    CommandSpec::public("A1-P0-AI-READ", b"AI\r"),
    CommandSpec::public("A1-P0-TN-READ", b"TN\r"),
    CommandSpec::public("A1-P0-PT-READ", b"PT\r"),
    CommandSpec::public("A1-P0-VX-READ", b"VX\r"),
    CommandSpec::public("A1-P0-IO-READ", b"IO\r"),
];

const BASELINE_REST: &[CommandSpec] = &[
    CommandSpec::sensitive("A1-P0-CS-READ", b"CS\r"),
    CommandSpec::sensitive("A1-P0-AE-READ", b"AE\r"),
    CommandSpec::public("A1-P0-AG-READ", b"AG\r"),
    CommandSpec::public("A1-P0-BC-READ", b"BC\r"),
    CommandSpec::public("A1-P0-DL-READ", b"DL\r"),
    CommandSpec::public("A1-P0-PS-READ", b"PS\r"),
    CommandSpec::public("A1-P0-BT-READ", b"BT\r"),
    CommandSpec::public("A1-P0-SD-READ", b"SD\r"),
    CommandSpec::public("A1-P0-FR-READ", b"FR\r"),
    CommandSpec::public("A1-P0-BL-READ", b"BL\r"),
    CommandSpec::indexed_sensitive("A1-P0-FQ-0-READ", b"FQ 0\r", b"FQ 0,"),
    CommandSpec::indexed_sensitive("A1-P0-FQ-1-READ", b"FQ 1\r", b"FQ 1,"),
    CommandSpec::indexed_sensitive("A1-P0-FO-0-READ", b"FO 0\r", b"FO 0,"),
    CommandSpec::indexed_sensitive("A1-P0-FO-1-READ", b"FO 1\r", b"FO 1,"),
    CommandSpec::indexed_public("A1-P0-BY-0-READ", b"BY 0\r", b"BY 0,"),
    CommandSpec::indexed_public("A1-P0-BY-1-READ", b"BY 1\r", b"BY 1,"),
    CommandSpec::indexed_public("A1-P0-SM-0-READ", b"SM 0\r", b"SM 0,"),
    CommandSpec::indexed_public("A1-P0-SM-1-READ", b"SM 1\r", b"SM 1,"),
    CommandSpec::indexed_public("A1-P0-SQ-0-READ", b"SQ 0\r", b"SQ 0,"),
    CommandSpec::indexed_public("A1-P0-SQ-1-READ", b"SQ 1\r", b"SQ 1,"),
    CommandSpec::indexed_public("A1-P0-MD-0-READ", b"MD 0\r", b"MD 0,"),
    CommandSpec::indexed_public("A1-P0-MD-1-READ", b"MD 1\r", b"MD 1,"),
    CommandSpec::indexed_public("A1-P0-PC-0-READ", b"PC 0\r", b"PC 0,"),
    CommandSpec::indexed_public("A1-P0-PC-1-READ", b"PC 1\r", b"PC 1,"),
    CommandSpec::indexed_public("A1-P0-RA-0-READ", b"RA 0\r", b"RA 0,"),
    CommandSpec::indexed_public("A1-P0-RA-1-READ", b"RA 1\r", b"RA 1,"),
    CommandSpec::indexed_public("A1-P0-VM-0-READ", b"VM 0\r", b"VM 0,"),
    CommandSpec::indexed_public("A1-P0-VM-1-READ", b"VM 1\r", b"VM 1,"),
    CommandSpec::indexed_public("A1-P0-SF-0-READ", b"SF 0\r", b"SF 0,"),
    CommandSpec::indexed_public("A1-P0-SF-1-READ", b"SF 1\r", b"SF 1,"),
    CommandSpec::indexed_public("A1-P0-SH-0-READ", b"SH 0\r", b"SH 0,"),
    CommandSpec::indexed_public("A1-P0-SH-1-READ", b"SH 1\r", b"SH 1,"),
    CommandSpec::indexed_public("A1-P0-SH-2-READ", b"SH 2\r", b"SH 2,"),
    CommandSpec::public("A1-P0-GP-READ", b"GP\r"),
    CommandSpec::public("A1-P0-GM-READ", b"GM\r"),
    CommandSpec::public("A1-P0-FS-READ", b"FS\r"),
    CommandSpec::public("A1-P0-FT-READ", b"FT\r"),
    CommandSpec::public("A1-P0-VD-READ", b"VD\r"),
    CommandSpec::public("A1-P0-VG-READ", b"VG\r"),
    CommandSpec::public("A1-P0-BS-READ", b"BS\r"),
    CommandSpec::public("A1-P0-LC-READ", b"LC\r"),
    CommandSpec::public("A1-P0-GS-READ", b"GS\r"),
    CommandSpec::public("A1-P0-MS-READ", b"MS\r"),
    CommandSpec::public("A1-P0-AS-READ", b"AS\r"),
    CommandSpec::indexed_sensitive("A1-P0-DC-1-READ", b"DC 1\r", b"DC 1,"),
    CommandSpec::indexed_sensitive("A1-P0-DC-2-READ", b"DC 2\r", b"DC 2,"),
    CommandSpec::indexed_sensitive("A1-P0-DC-3-READ", b"DC 3\r", b"DC 3,"),
    CommandSpec::indexed_sensitive("A1-P0-DC-4-READ", b"DC 4\r", b"DC 4,"),
    CommandSpec::indexed_sensitive("A1-P0-DC-5-READ", b"DC 5\r", b"DC 5,"),
    CommandSpec::indexed_sensitive("A1-P0-DC-6-READ", b"DC 6\r", b"DC 6,"),
    CommandSpec::public("A1-P0-DS-READ", b"DS\r"),
    CommandSpec::public("A1-P0-RT-READ", b"RT\r"),
    CommandSpec::public("A1-P0-GW-READ", b"GW\r"),
];

fn baseline_rest_specs(profile: BaselineProfile) -> impl Iterator<Item = &'static CommandSpec> {
    BASELINE_REST
        .iter()
        .filter(move |spec| profile.includes_rest_spec(spec))
}

const SAFETY_READS: &[CommandSpec] = &[
    CommandSpec::public("A1-P0-AI-READ", b"AI\r"),
    CommandSpec::public("A1-P0-TN-READ", b"TN\r"),
    CommandSpec::public("A1-P0-PT-READ", b"PT\r"),
    CommandSpec::public("A1-P0-VX-READ", b"VX\r"),
    CommandSpec::public("A1-P0-IO-READ", b"IO\r"),
];

const CONTAINMENT_TARGETS: &[ContainmentTarget] = &[
    ContainmentTarget {
        name: "tnc-off",
        write: CommandSpec::exact_echo("A1-CONTAIN-TN-OFF", b"TN 0,0\r", b"TN 0,0"),
        read: CommandSpec::public("A1-P0-TN-READ", b"TN\r"),
        expected: b"TN 0,0",
    },
    ContainmentTarget {
        name: "beacon-manual",
        write: CommandSpec::exact_echo("A1-CONTAIN-PT-MANUAL", b"PT 0\r", b"PT 0"),
        read: CommandSpec::public("A1-P0-PT-READ", b"PT\r"),
        expected: b"PT 0",
    },
    ContainmentTarget {
        name: "vox-off",
        write: CommandSpec::exact_echo("A1-CONTAIN-VX-OFF", b"VX 0\r", b"VX 0"),
        read: CommandSpec::public("A1-P0-VX-READ", b"VX\r"),
        expected: b"VX 0",
    },
    ContainmentTarget {
        name: "io-af",
        write: CommandSpec::exact_echo("A1-CONTAIN-IO-AF", b"IO 0\r", b"IO 0"),
        read: CommandSpec::public("A1-P0-IO-READ", b"IO\r"),
        expected: b"IO 0",
    },
    ContainmentTarget {
        name: "auto-info-off",
        write: CommandSpec::exact_echo("A1-CONTAIN-AI-OFF", b"AI 0\r", b"AI 0"),
        read: CommandSpec::public("A1-P0-AI-READ", b"AI\r"),
        expected: b"AI 0",
    },
];

#[tokio::main(flavor = "current_thread")]
async fn main() -> AuditResult<()> {
    let config = parse_args()?;
    if config.mode == Mode::MakeSafe {
        return Err(invalid_input(
            "make-safe is disabled by the backup-before-first-write rule; establish containment \
             through the radio UI and run the read-only baseline",
        ));
    }
    validate_endpoint(&config.endpoint)?;

    let mut captures = create_capture_files(&config.capture_root)?;
    write_headers(
        &mut captures.private,
        &mut captures.sanitized,
        &config,
        &captures.capture_id,
    )?;
    if !config.machine_checked_read_only {
        confirm_ui_checked(&config.endpoint)?;
    }
    write_preflight_observation(
        &mut captures.private,
        &mut captures.sanitized,
        &captures.capture_id,
        &config.endpoint,
        config.profile,
        config.machine_checked_read_only,
    )?;
    if config.mode == Mode::MakeSafe {
        confirm_make_safe(&config.endpoint)?;
    }

    let mut automation_attestation = None;
    let result = run_selected_profile(
        &config,
        &mut captures.private,
        &mut captures.sanitized,
        &captures.capture_id,
        &captures.session_dir,
        &mut automation_attestation,
    )
    .await;
    let session_status = if result.is_ok() {
        "complete"
    } else {
        "aborted"
    };
    let end_result = write_session_end(
        &mut captures.private,
        &mut captures.sanitized,
        &captures.capture_id,
        session_status,
    );
    durable_flush(&mut captures.private)?;
    durable_flush(&mut captures.sanitized)?;
    end_result?;
    write_capture_manifest(
        &captures,
        session_status,
        &config,
        automation_attestation.as_ref(),
    )?;
    eprintln!(
        "Audit capture records and manifest written with status {session_status}. Capture ID: {}",
        captures.capture_id
    );

    result?;
    println!(
        "Audit capture complete; transport closed. Capture ID: {}",
        captures.capture_id
    );
    Ok(())
}

fn parse_args() -> AuditResult<Config> {
    parse_args_from(std::env::args().skip(1))
}

fn parse_args_from<I, S>(args: I) -> AuditResult<Config>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let mut args = args.into_iter().map(Into::into).peekable();
    let mode = match args.peek().map(String::as_str) {
        Some("baseline") => {
            drop(args.next());
            Mode::Baseline
        }
        Some("make-safe") => {
            drop(args.next());
            Mode::MakeSafe
        }
        Some(flag) if flag.starts_with("--") => Mode::Baseline,
        _ => return Err(invalid_input(usage())),
    };

    let mut port = None;
    let mut bluetooth = None;
    let mut capture_root = None;
    let mut machine_checked_read_only = false;
    let mut automation = false;
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--port" if port.is_none() => port = Some(required_value(&mut args, &flag)?),
            "--bluetooth" if bluetooth.is_none() => {
                bluetooth = Some(required_value(&mut args, &flag)?);
            }
            "--capture-root" if capture_root.is_none() => {
                capture_root = Some(PathBuf::from(required_value(&mut args, &flag)?));
            }
            "--machine-checked-read-only" if !machine_checked_read_only => {
                machine_checked_read_only = true;
            }
            "--automation" if !automation => automation = true,
            "--port"
            | "--bluetooth"
            | "--capture-root"
            | "--machine-checked-read-only"
            | "--automation" => {
                return Err(invalid_input(format!("duplicate argument: {flag}")));
            }
            _ => return Err(invalid_input(format!("unknown argument: {flag}"))),
        }
    }

    let endpoint = match (port, bluetooth) {
        (Some(port), None) => Endpoint::Usb(port),
        (None, Some(device_name)) => Endpoint::Bluetooth(device_name),
        _ => {
            return Err(invalid_input(
                "exactly one of --port PATH or --bluetooth NAME is required",
            ));
        }
    };
    if machine_checked_read_only && mode != Mode::Baseline {
        return Err(invalid_input(
            "--machine-checked-read-only is valid only for the read-only baseline",
        ));
    }
    if automation && mode != Mode::Baseline {
        return Err(invalid_input(
            "--automation is valid only for the read-only baseline",
        ));
    }
    if automation && !machine_checked_read_only {
        return Err(invalid_input(
            "--automation requires --machine-checked-read-only",
        ));
    }

    Ok(Config {
        mode,
        profile: if automation {
            BaselineProfile::Automation
        } else {
            BaselineProfile::StockDefault
        },
        endpoint,
        capture_root: capture_root.ok_or_else(|| invalid_input("--capture-root is required"))?,
        machine_checked_read_only,
    })
}

fn required_value<I>(args: &mut std::iter::Peekable<I>, flag: &str) -> AuditResult<String>
where
    I: Iterator<Item = String>,
{
    let value = args
        .next()
        .ok_or_else(|| invalid_input(format!("missing value for {flag}")))?;
    if value.is_empty() || value.starts_with("--") {
        return Err(invalid_input(format!("missing value for {flag}")));
    }
    Ok(value)
}

const fn usage() -> &'static str {
    "usage: hardware_audit [baseline|make-safe] (--port PATH | --bluetooth NAME) \
     --capture-root DIR [--machine-checked-read-only] [--automation]"
}

fn invalid_input(message: impl Into<String>) -> AuditError {
    Box::new(io::Error::new(io::ErrorKind::InvalidInput, message.into()))
}

fn create_capture(path: &Path) -> AuditResult<BufWriter<File>> {
    let mut options = OpenOptions::new();
    let configured = options.write(true).create_new(true);
    #[cfg(unix)]
    let configured = configured.mode(0o600);
    Ok(BufWriter::new(configured.open(path)?))
}

#[derive(Debug)]
struct ContainmentJournal {
    writer: BufWriter<File>,
    sequence: u64,
}

impl ContainmentJournal {
    fn create(session_dir: &Path, original: &[(String, Vec<u8>)]) -> AuditResult<Self> {
        let mut journal = Self {
            writer: create_capture(&session_dir.join("containment-journal.jsonl"))?,
            sequence: 0,
        };
        let original_values: Vec<Value> = original
            .iter()
            .map(|(command, response)| {
                json!({
                    "command": command,
                    "response_hex": hex_with_cr(response),
                })
            })
            .collect();
        journal.append(
            "prepared",
            &json!({
                "original": original_values,
                "targets": CONTAINMENT_TARGETS
                    .iter()
                    .map(|target| {
                        json!({
                            "name": target.name,
                            "write_hex": hex(target.write.wire),
                            "expected_hex": hex_with_cr(target.expected),
                        })
                    })
                    .collect::<Vec<_>>(),
            }),
        )?;
        Ok(journal)
    }

    fn append(&mut self, status: &str, data: &Value) -> AuditResult<()> {
        let timestamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
        write_json_line(
            &mut self.writer,
            &json!({
                "schema_version": 1,
                "sequence": self.sequence,
                "timestamp_unix_ms": timestamp,
                "status": status,
                "data": data,
            }),
        )?;
        self.sequence = self.sequence.saturating_add(1);
        durable_flush(&mut self.writer)
    }
}

fn create_capture_files(root: &Path) -> AuditResult<CaptureFiles> {
    if !root.is_absolute() {
        return Err(invalid_input("--capture-root must be an absolute path"));
    }
    let created_root = match std::fs::symlink_metadata(root) {
        Ok(metadata) if metadata.file_type().is_dir() => false,
        Ok(_) => {
            return Err(invalid_input(
                "--capture-root must be a real directory, not a file or symlink",
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let mut builder = DirBuilder::new();
            let configured = builder.recursive(true);
            #[cfg(unix)]
            let configured = configured.mode(0o700);
            configured.create(root)?;
            true
        }
        Err(error) => return Err(Box::new(error)),
    };
    #[cfg(unix)]
    if created_root && std::fs::metadata(root)?.mode() & 0o777 != 0o700 {
        return Err(invalid_input(
            "a newly created capture root must have mode 0700",
        ));
    }
    #[cfg(not(unix))]
    let _ = created_root;
    let resolved_root = root.canonicalize()?;
    validate_capture_root_privacy(&resolved_root)?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
    let capture_id = format!("{now}-{}", std::process::id());
    let session_dir = resolved_root.join(&capture_id);
    #[cfg(unix)]
    let mut builder = DirBuilder::new();
    #[cfg(not(unix))]
    let builder = DirBuilder::new();
    #[cfg(unix)]
    let builder = builder.mode(0o700);
    builder.create(&session_dir)?;

    let private = create_capture(&session_dir.join("private.jsonl"))?;
    let sanitized = create_capture(&session_dir.join("sanitized.jsonl"))?;
    Ok(CaptureFiles {
        capture_id,
        session_dir,
        private,
        sanitized,
    })
}

fn validate_capture_root_privacy(root: &Path) -> AuditResult<()> {
    let discovery = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .arg("rev-parse")
        .arg("--show-toplevel")
        .output()?;
    if !discovery.status.success() {
        return Ok(());
    }
    let worktree_text = std::str::from_utf8(&discovery.stdout)?.trim();
    let worktree = Path::new(worktree_text).canonicalize()?;
    if !root.starts_with(&worktree) {
        return Err(invalid_input(
            "git reported a worktree that does not contain the capture root",
        ));
    }
    let ignored = std::process::Command::new("git")
        .arg("-C")
        .arg(&worktree)
        .arg("check-ignore")
        .arg("--quiet")
        .arg("--")
        .arg(root)
        .status()?;
    if ignored.success() {
        Ok(())
    } else {
        Err(invalid_input(
            "an in-worktree capture root must be covered by .gitignore",
        ))
    }
}

fn validate_endpoint(endpoint: &Endpoint) -> AuditResult<()> {
    match endpoint {
        Endpoint::Usb(port) => validate_usb_port(port),
        Endpoint::Bluetooth(_) => {
            #[cfg(target_os = "macos")]
            {
                Ok(())
            }
            #[cfg(not(target_os = "macos"))]
            {
                Err(invalid_input(
                    "--bluetooth uses native RFCOMM and is supported only on macOS",
                ))
            }
        }
    }
}

fn validate_usb_port(port: &str) -> AuditResult<()> {
    if SerialTransport::is_bluetooth_port(port) {
        return Err(invalid_input(
            "--port requires the USB CDC endpoint; use --bluetooth NAME for native RFCOMM",
        ));
    }
    let matches = SerialTransport::discover_usb()?
        .into_iter()
        .filter(|candidate| candidate.port_name == port)
        .count();
    if matches == 1 {
        Ok(())
    } else {
        Err(invalid_input(format!(
            "the exact port must enumerate once as USB VID:PID 2166:9023: {port}"
        )))
    }
}

fn open_transport(endpoint: &Endpoint) -> AuditResult<EitherTransport> {
    match endpoint {
        Endpoint::Usb(port) => Ok(EitherTransport::Serial(SerialTransport::open(
            port, CAT_BAUD,
        )?)),
        Endpoint::Bluetooth(device_name) => open_bluetooth_transport(device_name),
    }
}

#[cfg(target_os = "macos")]
fn open_bluetooth_transport(device_name: &str) -> AuditResult<EitherTransport> {
    Ok(EitherTransport::Bluetooth(BluetoothTransport::open(Some(
        device_name,
    ))?))
}

#[cfg(not(target_os = "macos"))]
fn open_bluetooth_transport(_device_name: &str) -> AuditResult<EitherTransport> {
    Err(invalid_input(
        "--bluetooth uses native RFCOMM and is supported only on macOS",
    ))
}

async fn close_transport<T: Transport>(transport: &mut T) -> AuditResult<()> {
    tokio::time::timeout(CLOSE_TIMEOUT, transport.close())
        .await
        .map_or_else(
            |_| Err(invalid_input("transport close timed out")),
            |inner| inner.map_err(Into::into),
        )
}

struct RadioRawTransport<'a, T: Transport>(&'a mut Radio<T>);

fn map_radio_write_error(error: RadioError) -> TransportError {
    match error {
        RadioError::Transport(error) => error,
        error => TransportError::Write(io::Error::other(error)),
    }
}

fn map_radio_read_error(error: RadioError) -> TransportError {
    match error {
        RadioError::Transport(error) => error,
        error => TransportError::Read(io::Error::other(error)),
    }
}

fn map_radio_connection_error(error: RadioError) -> TransportError {
    match error {
        RadioError::Transport(error) => error,
        error => TransportError::Disconnected(io::Error::other(error)),
    }
}

impl<T: Transport> Transport for RadioRawTransport<'_, T> {
    async fn write(&mut self, data: &[u8]) -> Result<(), TransportError> {
        self.0
            .transport_write(data)
            .await
            .map_err(map_radio_write_error)
    }

    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, TransportError> {
        self.0
            .transport_read(buffer)
            .await
            .map_err(map_radio_read_error)
    }

    async fn close(&mut self) -> Result<(), TransportError> {
        self.0
            .close_transport()
            .await
            .map_err(map_radio_connection_error)
    }

    async fn reopen(&mut self) -> Result<(), TransportError> {
        self.0.reconnect().await.map_err(map_radio_connection_error)
    }
}

async fn qualify_automation_on_endpoint(
    endpoint: &Endpoint,
) -> AuditResult<(Radio<EitherTransport>, AutomationAbi)> {
    let transport = open_transport(endpoint)?;
    let mut radio = Radio::connect(transport).await?;
    let qualification = radio
        .qualify_automation()
        .await
        .map(|session| session.abi());
    match qualification {
        Ok(abi) => Ok((radio, abi)),
        Err(error) => {
            drop(tokio::time::timeout(CLOSE_TIMEOUT, radio.disconnect()).await);
            Err(error.into())
        }
    }
}

async fn run_configured_audit<T: Transport>(
    config: &Config,
    transport: &mut T,
    private: &mut BufWriter<File>,
    sanitized: &mut BufWriter<File>,
    session_dir: &Path,
) -> AuditResult<()> {
    let mut codec = Codec::new();
    match config.mode {
        Mode::Baseline => {
            run_baseline(transport, &mut codec, private, sanitized, config.profile).await
        }
        Mode::MakeSafe => {
            run_make_safe(transport, &mut codec, private, sanitized, session_dir).await
        }
    }
}

async fn run_selected_profile(
    config: &Config,
    private: &mut BufWriter<File>,
    sanitized: &mut BufWriter<File>,
    capture_id: &str,
    session_dir: &Path,
    automation_attestation: &mut Option<AutomationAttestation>,
) -> AuditResult<()> {
    if config.profile == BaselineProfile::Automation {
        println!("Exact-attesting V1.03.AZM before the raw CAT audit...");
        let (mut radio, abi) = match qualify_automation_on_endpoint(&config.endpoint).await {
            Ok(qualified) => qualified,
            Err(error) => {
                write_automation_attestation(
                    private,
                    sanitized,
                    capture_id,
                    &config.endpoint,
                    None,
                    Some(&error.to_string()),
                )?;
                return Err(error);
            }
        };
        let mut evidence = AutomationAttestation {
            abi,
            transport_reopened_before_raw_audit: false,
        };
        *automation_attestation = Some(evidence);
        if let Err(error) = radio.reconnect().await {
            let error = AuditError::from(error);
            let evidence_result = write_automation_attestation(
                private,
                sanitized,
                capture_id,
                &config.endpoint,
                Some(&evidence),
                Some(&error.to_string()),
            );
            drop(tokio::time::timeout(CLOSE_TIMEOUT, radio.disconnect()).await);
            evidence_result?;
            return Err(error);
        }
        evidence.transport_reopened_before_raw_audit = true;
        *automation_attestation = Some(evidence);
        if let Err(error) = write_automation_attestation(
            private,
            sanitized,
            capture_id,
            &config.endpoint,
            Some(&evidence),
            None,
        ) {
            drop(tokio::time::timeout(CLOSE_TIMEOUT, radio.disconnect()).await);
            return Err(error);
        }
        let result = {
            let mut transport = RadioRawTransport(&mut radio);
            run_configured_audit(config, &mut transport, private, sanitized, session_dir).await
        };
        let close = tokio::time::timeout(CLOSE_TIMEOUT, radio.disconnect())
            .await
            .map_or_else(
                |_| Err(invalid_input("transport close timed out")),
                |inner| inner.map_err(Into::into),
            );
        result?;
        return close;
    }

    let mut transport = open_transport(&config.endpoint)?;
    let result =
        run_configured_audit(config, &mut transport, private, sanitized, session_dir).await;
    let close = close_transport(&mut transport).await;
    result?;
    close
}

fn write_headers(
    private: &mut BufWriter<File>,
    sanitized: &mut BufWriter<File>,
    config: &Config,
    capture_id: &str,
) -> AuditResult<()> {
    let started = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
    write_json_line(
        private,
        &session_start_record(config, capture_id, started, EvidencePrivacy::Private),
    )?;
    write_json_line(
        sanitized,
        &session_start_record(config, capture_id, started, EvidencePrivacy::Sanitized),
    )?;
    durable_flush(private)?;
    durable_flush(sanitized)?;
    Ok(())
}

fn session_start_record(
    config: &Config,
    capture_id: &str,
    started: u128,
    privacy: EvidencePrivacy,
) -> Value {
    let preflight_evidence_basis = if config.machine_checked_read_only {
        "machine-checked-read-only"
    } else {
        "operator-ui-attestation"
    };
    with_endpoint_evidence(
        json!({
            "type": "session_start",
            "schema_version": 1,
            "privacy": privacy.as_str(),
            "capture_id": capture_id,
            "mode": config.mode.as_str(),
            "profile": config.profile.as_str(),
            "fixed_cat_case_count": config.profile.case_count(),
            "automation_attestation_required": config.profile == BaselineProfile::Automation,
            "started_unix_ms": started,
            "preflight_evidence_basis": preflight_evidence_basis,
            "machine_checked_read_only": config.machine_checked_read_only,
            "expected_model": "TH-D75",
            "expected_firmware": "1.03",
            "expected_region": "K",
            "expected_variant": "2",
        }),
        &config.endpoint,
        privacy,
    )
}

fn with_endpoint_evidence(
    mut record: Value,
    endpoint: &Endpoint,
    privacy: EvidencePrivacy,
) -> Value {
    if let Value::Object(fields) = &mut record {
        endpoint.add_evidence_fields(fields, privacy);
    }
    record
}

fn durable_flush(writer: &mut BufWriter<File>) -> AuditResult<()> {
    writer.flush()?;
    writer.get_ref().sync_data()?;
    Ok(())
}

fn write_capture_manifest(
    captures: &CaptureFiles,
    session_status: &str,
    config: &Config,
    automation_attestation: Option<&AutomationAttestation>,
) -> AuditResult<()> {
    let private_path = captures.session_dir.join("private.jsonl");
    let sanitized_path = captures.session_dir.join("sanitized.jsonl");
    let source_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/hardware_audit.rs");
    let executable_path = std::env::current_exe()?;
    let timestamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
    let manifest = json!({
        "type": "capture_manifest",
        "schema_version": 1,
        "capture_id": captures.capture_id,
        "created_unix_ms": timestamp,
        "session_status": session_status,
        "profile": config.profile.as_str(),
        "identity_target": {
            "model": "TH-D75",
            "firmware": "1.03",
            "region": "K",
            "variant": "2",
            "validation": if session_status == "complete" {
                "exact-raw-match"
            } else {
                "inspect-terminal-transcript"
            },
        },
        "allowlist": {
            "risk": "R0",
            "profile": config.profile.as_str(),
            "case_count": config.profile.case_count(),
            "arbitrary_command_mode": false,
        },
        "automation_attestation": automation_attestation_manifest(config.profile, automation_attestation),
        "static_re_target": {
            "firmware_image_sha256": STATIC_RE_IMAGE_SHA256,
            "basis": "pinned-offline-image-not-an-observed-radio-hash",
        },
        "private_transcript": {
            "file": "private.jsonl",
            "bytes": std::fs::metadata(&private_path)?.len(),
            "sha256": sha256_file(&private_path)?,
        },
        "sanitized_transcript": {
            "file": "sanitized.jsonl",
            "bytes": std::fs::metadata(&sanitized_path)?.len(),
            "sha256": sha256_file(&sanitized_path)?,
        },
        "runner_provenance": {
            "source_sha256": sha256_file(&source_path)?,
            "executable_sha256": sha256_file(&executable_path)?,
            "git_head": git_head(&source_path),
        },
    });

    let manifest_path = captures.session_dir.join("manifest.json");
    let mut options = OpenOptions::new();
    let configured = options.write(true).create_new(true);
    #[cfg(unix)]
    let configured = configured.mode(0o600);
    let mut file = BufWriter::new(configured.open(manifest_path)?);
    serde_json::to_writer_pretty(&mut file, &manifest)?;
    file.write_all(b"\n")?;
    durable_flush(&mut file)
}

fn automation_attestation_manifest(
    profile: BaselineProfile,
    attestation: Option<&AutomationAttestation>,
) -> Value {
    if profile == BaselineProfile::StockDefault {
        return json!({
            "required": false,
            "status": "not-applicable",
            "qualifier": "Radio::qualify_automation",
        });
    }
    attestation.map_or_else(
        || {
            json!({
                "required": true,
                "status": "not-proved",
                "qualifier": "Radio::qualify_automation",
                "automation_qualified": false,
                "transport_reopened_before_raw_audit": false,
            })
        },
        |evidence| {
            json!({
                "required": true,
                "status": if evidence.transport_reopened_before_raw_audit {
                    "passed"
                } else {
                    "qualified-reopen-not-proved"
                },
                "qualifier": "Radio::qualify_automation",
                "automation_qualified": true,
                "transport_reopened_before_raw_audit": evidence.transport_reopened_before_raw_audit,
                "abi": {
                    "version": evidence.abi.version,
                    "features": evidence.abi.features,
                    "max_key": evidence.abi.max_key,
                    "max_phase": evidence.abi.max_phase,
                },
            })
        },
    )
}

fn sha256_file(path: &Path) -> AuditResult<String> {
    let output = std::process::Command::new("shasum")
        .arg("-a")
        .arg("256")
        .arg("--")
        .arg(path)
        .output()?;
    if !output.status.success() {
        return Err(invalid_input(format!(
            "shasum failed while hashing an audit artifact: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let digest = std::str::from_utf8(&output.stdout)?
        .split_ascii_whitespace()
        .next()
        .ok_or_else(|| invalid_input("shasum returned no digest"))?;
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(invalid_input("shasum returned an invalid SHA-256 digest"));
    }
    Ok(digest.to_ascii_lowercase())
}

fn git_head(path: &Path) -> Option<String> {
    let directory = path.parent()?;
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(directory)
        .arg("rev-parse")
        .arg("HEAD")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let head = std::str::from_utf8(&output.stdout).ok()?.trim();
    if head.len() == 40 && head.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Some(head.to_ascii_lowercase())
    } else {
        None
    }
}

fn write_session_end(
    private: &mut BufWriter<File>,
    sanitized: &mut BufWriter<File>,
    capture_id: &str,
    status: &str,
) -> AuditResult<()> {
    let timestamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
    write_json_line(
        private,
        &json!({
            "type": "session_end",
            "schema_version": 1,
            "privacy": "private",
            "capture_id": capture_id,
            "timestamp_unix_ms": timestamp,
            "status": status,
        }),
    )?;
    write_json_line(
        sanitized,
        &json!({
            "type": "session_end",
            "schema_version": 1,
            "privacy": "sanitized",
            "capture_id": capture_id,
            "timestamp_unix_ms": timestamp,
            "status": status,
        }),
    )?;
    durable_flush(private)?;
    durable_flush(sanitized)
}

fn write_preflight_observation(
    private: &mut BufWriter<File>,
    sanitized: &mut BufWriter<File>,
    capture_id: &str,
    endpoint: &Endpoint,
    profile: BaselineProfile,
    machine_checked: bool,
) -> AuditResult<()> {
    let timestamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
    write_json_line(
        private,
        &preflight_observation_record(
            capture_id,
            endpoint,
            profile,
            timestamp,
            EvidencePrivacy::Private,
            machine_checked,
        ),
    )?;
    write_json_line(
        sanitized,
        &preflight_observation_record(
            capture_id,
            endpoint,
            profile,
            timestamp,
            EvidencePrivacy::Sanitized,
            machine_checked,
        ),
    )?;
    durable_flush(private)?;
    durable_flush(sanitized)
}

fn preflight_observation_record(
    capture_id: &str,
    endpoint: &Endpoint,
    profile: BaselineProfile,
    timestamp: u128,
    privacy: EvidencePrivacy,
    machine_checked: bool,
) -> Value {
    let (action_code, result_code, evidence_basis, assertions) = if machine_checked {
        let mut assertions = vec![
            "exact identity must pass before non-identity reads",
            "CAT containment must pass before the remaining read allowlist",
            "the runner exposes no arbitrary-command mode",
        ];
        if profile == BaselineProfile::Automation {
            assertions.extend([
                "Radio::qualify_automation must exact-attest V1.03.AZM before raw probing",
                "the qualified transport must close and a fresh transport must open before the 59-case CAT audit",
            ]);
        }
        (
            "preflight-read-only-policy",
            "machine-check-required",
            "machine-checked-read-only",
            json!(assertions),
        )
    } else {
        (
            "preflight-ui-checklist",
            "operator-attested",
            "operator-attestation",
            json!(OPERATOR_ASSERTIONS),
        )
    };
    with_endpoint_evidence(
        json!({
            "type": "observation",
            "schema_version": 1,
            "privacy": privacy.as_str(),
            "capture_id": capture_id,
            "timestamp_unix_ms": timestamp,
            "profile": profile.as_str(),
            "fixed_cat_case_count": profile.case_count(),
            "action_code": action_code,
            "result_code": result_code,
            "evidence_basis": evidence_basis,
            "assertions": assertions,
            "manual_ui_attestation_bypassed": machine_checked,
            "radio_state_verified": false,
        }),
        endpoint,
        privacy,
    )
}

fn write_automation_attestation(
    private: &mut BufWriter<File>,
    sanitized: &mut BufWriter<File>,
    capture_id: &str,
    endpoint: &Endpoint,
    attestation: Option<&AutomationAttestation>,
    error: Option<&str>,
) -> AuditResult<()> {
    let timestamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
    write_json_line(
        private,
        &automation_attestation_record(
            capture_id,
            endpoint,
            timestamp,
            EvidencePrivacy::Private,
            attestation,
            error,
        ),
    )?;
    write_json_line(
        sanitized,
        &automation_attestation_record(
            capture_id,
            endpoint,
            timestamp,
            EvidencePrivacy::Sanitized,
            attestation,
            error,
        ),
    )?;
    durable_flush(private)?;
    durable_flush(sanitized)
}

fn automation_attestation_record(
    capture_id: &str,
    endpoint: &Endpoint,
    timestamp: u128,
    privacy: EvidencePrivacy,
    attestation: Option<&AutomationAttestation>,
    error: Option<&str>,
) -> Value {
    let automation_qualified = attestation.is_some();
    let transport_reopened =
        attestation.is_some_and(|evidence| evidence.transport_reopened_before_raw_audit);
    let passed = automation_qualified && transport_reopened && error.is_none();
    let result_code = if passed {
        "exact-automation-qualified-transport-reopened"
    } else if automation_qualified {
        "exact-automation-qualified-transport-reopen-failed"
    } else {
        "exact-automation-not-proved"
    };
    let abi = attestation.map_or(Value::Null, |evidence| {
        json!({
            "version": evidence.abi.version,
            "features": evidence.abi.features,
            "max_key": evidence.abi.max_key,
            "max_phase": evidence.abi.max_phase,
        })
    });
    let error_detail = error.map_or(Value::Null, |detail| match privacy {
        EvidencePrivacy::Private => json!(detail),
        EvidencePrivacy::Sanitized => json!({
            "$redacted": "automation-attestation-error",
            "byte_len": detail.len(),
        }),
    });
    with_endpoint_evidence(
        json!({
            "type": "observation",
            "schema_version": 1,
            "privacy": privacy.as_str(),
            "capture_id": capture_id,
            "timestamp_unix_ms": timestamp,
            "profile": BaselineProfile::Automation.as_str(),
            "fixed_cat_case_count": BaselineProfile::Automation.case_count(),
            "action_code": "qualify-automation",
            "result_code": result_code,
            "evidence_basis": "Radio::qualify_automation",
            "automation_qualified": automation_qualified,
            "transport_reopened_before_raw_audit": transport_reopened,
            "abi": abi,
            "error_detail": error_detail,
        }),
        endpoint,
        privacy,
    )
}

async fn run_baseline<T: Transport>(
    transport: &mut T,
    codec: &mut Codec,
    private: &mut BufWriter<File>,
    sanitized: &mut BufWriter<File>,
    profile: BaselineProfile,
) -> AuditResult<()> {
    println!("Running raw read-only preflight...");
    let identity = run_specs(transport, codec, private, sanitized, 0, IDENTITY_READS).await?;
    validate_identity(&identity)?;
    let (ai_spec, remaining_preflight) = BASELINE_PREFLIGHT
        .split_first()
        .ok_or_else(|| invalid_input("baseline preflight allowlist is empty"))?;
    let ai = run_specs(
        transport,
        codec,
        private,
        sanitized,
        IDENTITY_READS.len(),
        std::slice::from_ref(ai_spec),
    )
    .await?;
    expect_response(&ai, "AI", b"AI 0")?;
    let mut preflight = ai;
    preflight.extend(
        run_specs(
            transport,
            codec,
            private,
            sanitized,
            IDENTITY_READS.len() + 1,
            remaining_preflight,
        )
        .await?,
    );
    validate_cat_containment(&preflight)?;

    println!("CAT containment subset passed; running the remaining read-only allowlist...");
    let summary = run_specs_collect_all(
        transport,
        codec,
        private,
        sanitized,
        IDENTITY_READS.len() + BASELINE_PREFLIGHT.len(),
        baseline_rest_specs(profile),
    )
    .await?;
    println!(
        "Remaining allowlist summary: {} pass, {} inconclusive, {} fail.",
        summary.passed, summary.inconclusive, summary.failed
    );
    if summary.is_all_pass() {
        Ok(())
    } else {
        Err(invalid_input(format!(
            "read-only baseline completed every remaining case but was not all-pass: {} \
             inconclusive, {} fail",
            summary.inconclusive, summary.failed
        )))
    }
}

async fn run_make_safe<T: Transport>(
    transport: &mut T,
    codec: &mut Codec,
    private: &mut BufWriter<File>,
    sanitized: &mut BufWriter<File>,
    session_dir: &Path,
) -> AuditResult<()> {
    let mut sequence = 0;
    println!("Verifying exact radio identity before containment...");
    let identity = run_specs_interruptible(
        transport,
        codec,
        private,
        sanitized,
        sequence,
        IDENTITY_READS,
    )
    .await?;
    sequence += IDENTITY_READS.len();
    validate_identity(&identity)?;

    println!("Capturing original safety state...");
    let original =
        run_specs_interruptible(transport, codec, private, sanitized, sequence, SAFETY_READS)
            .await?;
    sequence += SAFETY_READS.len();
    validate_safety_read_grammar(&original)?;

    let mut journal = ContainmentJournal::create(session_dir, &original)?;
    println!("Applying journaled containment one setting at a time...");
    let apply_result = apply_containment(
        transport,
        codec,
        private,
        sanitized,
        &mut journal,
        &mut sequence,
    )
    .await;

    if let Err(primary) = apply_result {
        let recovery_start = journal.append(
            "recovery-start",
            &json!({"reason": "interrupted-or-step-failed"}),
        );
        let recovery = recover_containment(
            transport,
            codec,
            private,
            sanitized,
            &mut journal,
            &mut sequence,
        )
        .await;
        match (recovery_start, recovery) {
            (Ok(()), Ok(())) => {
                journal.append("recovered", &json!({"contained": true}))?;
                return Err(invalid_input(format!(
                    "containment encountered an error but recovery verified every safe target: \
                     {primary}"
                )));
            }
            (journal_result, recovery_result) => {
                let journal_status = if journal_result.is_ok() {
                    "recorded"
                } else {
                    "failed"
                };
                let recovery_status = if recovery_result.is_ok() {
                    "verified"
                } else {
                    "failed"
                };
                return Err(invalid_input(format!(
                    "containment is unresolved (journal={journal_status}, \
                     recovery={recovery_status}); use the radio UI immediately and inspect the \
                     private containment journal"
                )));
            }
        }
    }

    println!("Running final CAT containment readback...");
    let verified =
        run_specs_interruptible(transport, codec, private, sanitized, sequence, SAFETY_READS)
            .await?;
    validate_cat_containment(&verified)?;
    journal.append("complete", &json!({"contained": true}))?;
    Ok(())
}

async fn run_specs_interruptible<T: Transport>(
    transport: &mut T,
    codec: &mut Codec,
    private: &mut BufWriter<File>,
    sanitized: &mut BufWriter<File>,
    sequence_base: usize,
    specs: &[CommandSpec],
) -> AuditResult<Vec<(String, Vec<u8>)>> {
    run_specs(transport, codec, private, sanitized, sequence_base, specs).await
}

async fn apply_containment<T: Transport>(
    transport: &mut T,
    codec: &mut Codec,
    private: &mut BufWriter<File>,
    sanitized: &mut BufWriter<File>,
    journal: &mut ContainmentJournal,
    sequence: &mut usize,
) -> AuditResult<()> {
    for target in CONTAINMENT_TARGETS {
        let write_sequence = *sequence;
        *sequence = sequence.saturating_add(1);
        journal.append(
            "write-intent",
            &json!({
                "target": target.name,
                "tx_hex": hex(target.write.wire),
                "sequence": write_sequence,
            }),
        )?;
        let written = run_specs_interruptible(
            transport,
            codec,
            private,
            sanitized,
            write_sequence,
            std::slice::from_ref(&target.write),
        )
        .await?;
        expect_response(
            &written,
            command_text(target.write.wire)?.as_str(),
            target.expected,
        )?;
        journal.append(
            "write-acknowledged",
            &json!({"target": target.name, "response_hex": hex_with_cr(target.expected)}),
        )?;

        let read_sequence = *sequence;
        *sequence = sequence.saturating_add(1);
        let readback = run_specs_interruptible(
            transport,
            codec,
            private,
            sanitized,
            read_sequence,
            std::slice::from_ref(&target.read),
        )
        .await?;
        expect_response(
            &readback,
            command_text(target.read.wire)?.as_str(),
            target.expected,
        )?;
        journal.append(
            "target-verified",
            &json!({"target": target.name, "response_hex": hex_with_cr(target.expected)}),
        )?;
    }
    Ok(())
}

async fn recover_containment<T: Transport>(
    transport: &mut T,
    codec: &mut Codec,
    private: &mut BufWriter<File>,
    sanitized: &mut BufWriter<File>,
    journal: &mut ContainmentJournal,
    sequence: &mut usize,
) -> AuditResult<()> {
    let mut failed = Vec::new();
    let mut evidence_failures = 0_u64;
    for target in CONTAINMENT_TARGETS {
        let write_sequence = *sequence;
        *sequence = sequence.saturating_add(1);
        if journal
            .append(
                "recovery-write-intent",
                &json!({
                    "target": target.name,
                    "tx_hex": hex(target.write.wire),
                    "sequence": write_sequence,
                }),
            )
            .is_err()
        {
            evidence_failures = evidence_failures.saturating_add(1);
        }
        let write_ok = run_specs(
            transport,
            codec,
            private,
            sanitized,
            write_sequence,
            std::slice::from_ref(&target.write),
        )
        .await
        .map_or_else(
            |_| {
                evidence_failures = evidence_failures.saturating_add(1);
                false
            },
            |observed| {
                command_text(target.write.wire).is_ok_and(|command| {
                    expect_response(&observed, &command, target.expected).is_ok()
                })
            },
        );

        let read_sequence = *sequence;
        *sequence = sequence.saturating_add(1);
        let read_ok = run_specs(
            transport,
            codec,
            private,
            sanitized,
            read_sequence,
            std::slice::from_ref(&target.read),
        )
        .await
        .map_or_else(
            |_| {
                evidence_failures = evidence_failures.saturating_add(1);
                false
            },
            |observed| {
                command_text(target.read.wire).is_ok_and(|command| {
                    expect_response(&observed, &command, target.expected).is_ok()
                })
            },
        );
        if journal
            .append(
                "recovery-target-result",
                &json!({
                    "target": target.name,
                    "write_ok": write_ok,
                    "readback_ok": read_ok,
                }),
            )
            .is_err()
        {
            evidence_failures = evidence_failures.saturating_add(1);
        }
        if !(write_ok && read_ok) {
            failed.push(target.name);
        }
    }

    if failed.is_empty() && evidence_failures == 0 {
        Ok(())
    } else {
        Err(invalid_input(format!(
            "containment recovery evidence incomplete: failed targets [{}], evidence failures \
             {evidence_failures}",
            failed.join(", ")
        )))
    }
}

async fn run_specs<T: Transport>(
    transport: &mut T,
    codec: &mut Codec,
    private: &mut BufWriter<File>,
    sanitized: &mut BufWriter<File>,
    sequence_base: usize,
    specs: &[CommandSpec],
) -> AuditResult<Vec<(String, Vec<u8>)>> {
    let mut observed = Vec::with_capacity(specs.len());
    for (offset, spec) in specs.iter().enumerate() {
        let command = command_text(spec.wire)?;
        let sequence = sequence_base + offset;
        write_attempt(private, sanitized, sequence, spec)?;
        let exchange = exchange(transport, codec, spec).await;
        let grammar_ok = exchange
            .response()
            .map(|response| validate_response_grammar(&command, response));
        write_exchange(private, sanitized, sequence, spec, &exchange, grammar_ok)?;
        let (status, reason) = classify_case(&command, &exchange, grammar_ok);
        write_case_result(private, sanitized, sequence, spec, status, reason)?;
        let printable = if spec.sensitive {
            "<redacted>".to_owned()
        } else {
            exchange.response().map_or_else(
                || format!("<{}>", exchange.terminal_code()),
                |response| String::from_utf8_lossy(response).into_owned(),
            )
        };
        println!("  {command:<10} -> {printable}");
        if status != CaseStatus::Pass {
            return Err(case_error(&command, &exchange, status, reason));
        }
        let response = exchange
            .response()
            .ok_or_else(|| invalid_input("passing case has no response"))?;
        observed.push((command, response.to_vec()));
        intercommand_pause().await?;
    }
    Ok(observed)
}

async fn run_specs_collect_all<'a, T, I>(
    transport: &mut T,
    codec: &mut Codec,
    private: &mut BufWriter<File>,
    sanitized: &mut BufWriter<File>,
    sequence_base: usize,
    specs: I,
) -> AuditResult<CaseSummary>
where
    T: Transport,
    I: IntoIterator<Item = &'a CommandSpec>,
{
    let mut summary = CaseSummary::default();
    for (offset, spec) in specs.into_iter().enumerate() {
        let command = command_text(spec.wire)?;
        let sequence = sequence_base + offset;
        write_attempt(private, sanitized, sequence, spec)?;
        let exchange = exchange(transport, codec, spec).await;
        let grammar_ok = exchange
            .response()
            .map(|response| validate_response_grammar(&command, response));
        write_exchange(private, sanitized, sequence, spec, &exchange, grammar_ok)?;
        let (status, reason) = classify_case(&command, &exchange, grammar_ok);
        write_case_result(private, sanitized, sequence, spec, status, reason)?;
        summary.add(status);

        let printable = if spec.sensitive {
            "<redacted>".to_owned()
        } else {
            exchange.response().map_or_else(
                || format!("<{}>", exchange.terminal_code()),
                |response| String::from_utf8_lossy(response).into_owned(),
            )
        };
        println!("  {command:<10} -> {printable} [{}]", status.as_str());

        if matches!(exchange.terminal, ExchangeTerminal::Failure { .. }) {
            return Err(case_error(&command, &exchange, status, reason));
        }
        intercommand_pause().await?;
    }
    Ok(summary)
}

fn classify_case(
    command: &str,
    exchange: &Exchange,
    grammar_ok: Option<bool>,
) -> (CaseStatus, &'static str) {
    match exchange.response() {
        Some(b"N") => (CaseStatus::Inconclusive, "radio-runtime-unavailable"),
        Some(b"?") => (CaseStatus::Inconclusive, "radio-rejected-form"),
        Some(b"RT ------------") if command == "RT" => {
            (CaseStatus::Inconclusive, "radio-clock-unavailable")
        }
        Some(_) if grammar_ok == Some(true) => (CaseStatus::Pass, "strict-grammar-match"),
        Some(_) => (CaseStatus::Fail, "strict-grammar-mismatch"),
        None => (CaseStatus::Inconclusive, exchange.terminal_code()),
    }
}

fn case_error(command: &str, exchange: &Exchange, status: CaseStatus, reason: &str) -> AuditError {
    let detail = exchange
        .failure_detail()
        .map_or_else(String::new, |value| format!(": {value}"));
    invalid_input(format!(
        "{command} ended {} ({reason}){detail}",
        status.as_str()
    ))
}

async fn intercommand_pause() -> AuditResult<()> {
    tokio::select! {
        biased;
        interrupt = tokio::signal::ctrl_c() => {
            interrupt?;
            Err(Box::new(io::Error::new(
                io::ErrorKind::Interrupted,
                "operator interrupted between CAT cases",
            )))
        }
        () = tokio::time::sleep(Duration::from_millis(10)) => Ok(()),
    }
}

async fn exchange<T: Transport>(
    transport: &mut T,
    codec: &mut Codec,
    spec: &CommandSpec,
) -> Exchange {
    let Some(expected) = spec.wire.get(..2) else {
        return Exchange {
            drained: Vec::new(),
            drain_truncated: false,
            received: Vec::new(),
            unsolicited: Vec::new(),
            terminal: ExchangeTerminal::Failure {
                code: "invalid-allowlist",
                detail: "allowlisted command is shorter than two bytes".to_owned(),
            },
        };
    };
    let mut drain = drain_stale(transport, codec).await;
    if let Some((code, detail)) = drain.failure.take() {
        return exchange_before_response(drain, code, detail);
    }
    if drain.truncated {
        return exchange_before_response(
            drain,
            "drain-not-quiescent",
            "stale input did not become quiet before the bounded drain limit; no command byte \
             was written"
                .to_owned(),
        );
    }
    codec.clear();

    let write_result = tokio::select! {
        biased;
        interrupt = tokio::signal::ctrl_c() => {
            return exchange_before_response(
                drain,
                "operator-interrupt",
                interrupt.map_or_else(
                    |error| format!("Ctrl-C monitor failed before CAT write: {error}"),
                    |()| "operator interrupted before CAT write".to_owned(),
                ),
            );
        }
        result = tokio::time::timeout(RESPONSE_TIMEOUT, transport.write(spec.wire)) => result,
    };
    match write_result {
        Err(_) => {
            return exchange_before_response(
                drain,
                "write-timeout",
                "CAT write did not complete before the bounded timeout".to_owned(),
            );
        }
        Ok(Err(error)) => {
            return exchange_before_response(drain, "write-error", error.to_string());
        }
        Ok(Ok(())) => {}
    }

    let mut unsolicited = drain.frames;
    let mut received = Vec::new();
    let terminal = tokio::time::timeout(
        RESPONSE_TIMEOUT,
        await_response(
            transport,
            codec,
            spec,
            expected,
            &mut received,
            &mut unsolicited,
        ),
    )
    .await
    .unwrap_or_else(|_| ExchangeTerminal::Failure {
        code: "response-timeout",
        detail: "CAT response did not complete before the bounded timeout".to_owned(),
    });

    Exchange {
        drained: drain.bytes,
        drain_truncated: false,
        received,
        unsolicited,
        terminal,
    }
}

fn exchange_before_response(drain: Drain, code: &'static str, detail: String) -> Exchange {
    Exchange {
        drained: drain.bytes,
        drain_truncated: drain.truncated,
        received: Vec::new(),
        unsolicited: drain.frames,
        terminal: ExchangeTerminal::Failure { code, detail },
    }
}

async fn await_response<T: Transport>(
    transport: &mut T,
    codec: &mut Codec,
    spec: &CommandSpec,
    expected: &[u8],
    received: &mut Vec<u8>,
    unsolicited: &mut Vec<Vec<u8>>,
) -> ExchangeTerminal {
    let mut buffer = [0u8; 4096];
    loop {
        let read_result = tokio::select! {
            biased;
            interrupt = tokio::signal::ctrl_c() => {
                return ExchangeTerminal::Failure {
                    code: "operator-interrupt",
                    detail: interrupt.map_or_else(
                        |error| format!("Ctrl-C monitor failed during CAT read: {error}"),
                        |()| "operator interrupted while awaiting the CAT response".to_owned(),
                    ),
                };
            }
            result = transport.read(&mut buffer) => result,
        };
        let count = match read_result {
            Ok(count) => count,
            Err(error) => {
                return ExchangeTerminal::Failure {
                    code: "read-error",
                    detail: error.to_string(),
                };
            }
        };
        if count == 0 {
            return ExchangeTerminal::Failure {
                code: "unexpected-eof",
                detail: "radio disconnected while awaiting the CAT response".to_owned(),
            };
        }
        let Some(chunk) = buffer.get(..count) else {
            return ExchangeTerminal::Failure {
                code: "invalid-read-count",
                detail: format!(
                    "transport reported {count} bytes for a {}-byte buffer",
                    buffer.len()
                ),
            };
        };
        received.extend_from_slice(chunk);
        codec.feed(chunk);
        let mut matched = None;
        while let Some(frame) = codec.next_frame() {
            if matched.is_none()
                && (frame == b"?"
                    || frame == b"N"
                    || spec.response_prefix.map_or_else(
                        || mnemonic_matches(&frame, expected),
                        |prefix| frame.starts_with(prefix),
                    ))
            {
                matched = Some(frame);
            } else {
                unsolicited.push(frame);
            }
        }
        if let Some(frame) = matched {
            return ExchangeTerminal::Response(frame);
        }
    }
}

#[derive(Debug)]
struct Drain {
    bytes: Vec<u8>,
    frames: Vec<Vec<u8>>,
    truncated: bool,
    failure: Option<(&'static str, String)>,
}

async fn drain_stale<T: Transport>(transport: &mut T, codec: &mut Codec) -> Drain {
    let mut bytes = Vec::new();
    let mut frames = Vec::new();
    let mut buffer = [0u8; 4096];
    let started = Instant::now();
    let mut truncated = false;
    let mut failure = None;
    loop {
        let Some(remaining) = DRAIN_TOTAL_TIMEOUT.checked_sub(started.elapsed()) else {
            truncated = true;
            break;
        };
        let wait = DRAIN_WINDOW.min(remaining);
        let read_result = tokio::select! {
            biased;
            interrupt = tokio::signal::ctrl_c() => {
                failure = Some((
                    "operator-interrupt",
                    interrupt.map_or_else(
                        |error| format!("Ctrl-C monitor failed during stale-input drain: {error}"),
                        |()| "operator interrupted during stale-input drain".to_owned(),
                    ),
                ));
                break;
            }
            result = tokio::time::timeout(wait, transport.read(&mut buffer)) => result,
        };
        match read_result {
            Ok(Ok(0)) => break,
            Ok(Ok(count)) => {
                let Some(chunk) = buffer.get(..count) else {
                    failure = Some((
                        "invalid-read-count",
                        format!(
                            "transport reported {count} bytes for a {}-byte drain buffer",
                            buffer.len()
                        ),
                    ));
                    break;
                };
                bytes.extend_from_slice(chunk);
                codec.feed(chunk);
                while let Some(frame) = codec.next_frame() {
                    frames.push(frame);
                }
                if bytes.len() >= MAX_DRAIN_BYTES {
                    truncated = true;
                    break;
                }
            }
            Ok(Err(error)) => {
                failure = Some(("drain-error", error.to_string()));
                break;
            }
            Err(_) if wait == DRAIN_WINDOW => break,
            Err(_) => {
                truncated = true;
                break;
            }
        }
    }
    Drain {
        bytes,
        frames,
        truncated,
        failure,
    }
}

fn mnemonic_matches(frame: &[u8], expected: &[u8]) -> bool {
    frame.get(..2) == Some(expected)
        && (frame.len() == 2 || frame.get(2).is_some_and(|byte| *byte == b' '))
}

fn command_text(wire: &[u8]) -> AuditResult<String> {
    let command = wire
        .strip_suffix(b"\r")
        .ok_or_else(|| invalid_input("allowlisted command is not CR terminated"))?;
    Ok(std::str::from_utf8(command)?.to_owned())
}

fn write_attempt(
    private: &mut BufWriter<File>,
    sanitized: &mut BufWriter<File>,
    sequence: usize,
    spec: &CommandSpec,
) -> AuditResult<()> {
    let command = command_text(spec.wire)?;
    let timestamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
    let record = json!({
        "type": "tx_intent",
        "timestamp_unix_ms": timestamp,
        "sequence": sequence,
        "probe_id": spec.probe_id,
        "risk": if spec.state_change { "R1" } else { "R0" },
        "command": command,
        "tx_hex": hex(spec.wire),
        "state_change": spec.state_change,
        "durability": "flushed-before-io",
    });
    write_json_line(private, &record)?;
    write_json_line(sanitized, &record)?;
    durable_flush(private)?;
    durable_flush(sanitized)
}

fn write_exchange(
    private: &mut BufWriter<File>,
    sanitized: &mut BufWriter<File>,
    sequence: usize,
    spec: &CommandSpec,
    exchange: &Exchange,
    grammar_ok: Option<bool>,
) -> AuditResult<()> {
    let command = command_text(spec.wire)?;
    let unsolicited_hex: Vec<String> = exchange
        .unsolicited
        .iter()
        .map(|frame| hex_with_cr(frame))
        .collect();
    let (private_response_hex, sanitized_response_hex, response_redactions) =
        exchange.response().map_or_else(
            || (None, None, Vec::new()),
            |response| {
                let (sanitized_hex, redactions) =
                    sanitize_response_hex(&command, spec.sensitive, response);
                (Some(hex_with_cr(response)), Some(sanitized_hex), redactions)
            },
        );
    let sanitized_drained_hex = mask_unknown_bytes(&exchange.drained);
    let sanitized_received_hex = mask_unknown_bytes(&exchange.received);
    let sanitized_unsolicited_hex: Vec<String> = exchange
        .unsolicited
        .iter()
        .map(|frame| {
            let mut terminated = frame.clone();
            terminated.push(b'\r');
            mask_unknown_bytes(&terminated)
        })
        .collect();
    let timestamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
    let outcome = exchange.terminal_code();
    let private_parsed = parsed_response(&command, false, exchange.response(), grammar_ok);
    let sanitized_parsed =
        parsed_response(&command, spec.sensitive, exchange.response(), grammar_ok);

    write_json_line(
        private,
        &json!({
            "type": "exchange",
            "timestamp_unix_ms": timestamp,
            "sequence": sequence,
            "probe_id": spec.probe_id,
            "risk": if spec.state_change { "R1" } else { "R0" },
            "command": command,
            "tx_hex": hex(spec.wire),
            "drained_hex": hex(&exchange.drained),
            "drain_truncated": exchange.drain_truncated,
            "rx_stream_hex": hex(&exchange.received),
            "unsolicited_hex": unsolicited_hex,
            "response_hex": private_response_hex,
            "response_redactions": response_redactions,
            "outcome": outcome,
            "grammar_ok": grammar_ok,
            "parsed_response": private_parsed,
            "error_detail": exchange.failure_detail(),
        }),
    )?;

    write_json_line(
        sanitized,
        &json!({
            "type": "exchange",
            "timestamp_unix_ms": timestamp,
            "sequence": sequence,
            "probe_id": spec.probe_id,
            "risk": if spec.state_change { "R1" } else { "R0" },
            "command": command,
            "tx_hex": hex(spec.wire),
            "drained_bytes": exchange.drained.len(),
            "drained_hex": sanitized_drained_hex,
            "drain_truncated": exchange.drain_truncated,
            "rx_stream_hex": sanitized_received_hex,
            "unsolicited_frames": exchange.unsolicited.len(),
            "unsolicited_hex": sanitized_unsolicited_hex,
            "response_bytes": exchange.response().map_or(0, |response| response.len() + 1),
            "response_hex": sanitized_response_hex,
            "response_redactions": response_redactions,
            "outcome": outcome,
            "grammar_ok": grammar_ok,
            "parsed_response": sanitized_parsed,
            "error_detail": exchange.failure_detail().map(|detail| json!({
                "$redacted": "transport-error-detail",
                "byte_len": detail.len(),
            })),
        }),
    )?;
    durable_flush(private)?;
    durable_flush(sanitized)?;
    Ok(())
}

fn write_case_result(
    private: &mut BufWriter<File>,
    sanitized: &mut BufWriter<File>,
    sequence: usize,
    spec: &CommandSpec,
    status: CaseStatus,
    reason: &str,
) -> AuditResult<()> {
    let command = command_text(spec.wire)?;
    let timestamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
    let record = json!({
        "type": "case_result",
        "timestamp_unix_ms": timestamp,
        "sequence": sequence,
        "probe_id": spec.probe_id,
        "risk": if spec.state_change { "R1" } else { "R0" },
        "command": command,
        "status": status.as_str(),
        "reason_code": reason,
        "oracle": "raw-cat-bytes-and-strict-response-grammar",
        "claim_scope": "wire-response-shape",
        "restore_result": if spec.state_change { "not-recorded-here" } else { "not-needed-read-only" },
    });
    write_json_line(private, &record)?;
    write_json_line(sanitized, &record)?;
    durable_flush(private)?;
    durable_flush(sanitized)
}

fn parsed_response(
    command: &str,
    redact_payload: bool,
    response: Option<&[u8]>,
    grammar_ok: Option<bool>,
) -> Value {
    let Some(response) = response else {
        return Value::Null;
    };
    if response == b"N" {
        return json!({"status": "runtime-unavailable", "grammar_ok": grammar_ok});
    }
    if response == b"?" {
        return json!({"status": "form-rejected", "grammar_ok": grammar_ok});
    }
    let payload = response
        .iter()
        .position(|byte| *byte == b' ')
        .and_then(|space| response.get(space.saturating_add(1)..))
        .unwrap_or(&[]);
    if redact_payload {
        json!({
            "status": "response",
            "mnemonic": command.get(..2),
            "payload": {
                "$redacted": "sensitive-response-payload",
                "byte_len": payload.len(),
                "field_count": payload.split(|byte| *byte == b',').count(),
            },
            "grammar_ok": grammar_ok,
        })
    } else {
        let fields: Vec<String> = payload
            .split(|byte| *byte == b',')
            .map(|field| String::from_utf8_lossy(field).into_owned())
            .collect();
        json!({
            "status": "response",
            "mnemonic": command.get(..2),
            "fields": fields,
            "grammar_ok": grammar_ok,
        })
    }
}

fn sanitize_response_hex(command: &str, sensitive: bool, frame: &[u8]) -> (String, Vec<Value>) {
    let mut terminated = frame.to_vec();
    terminated.push(b'\r');
    if !sensitive {
        return (hex(&terminated), Vec::new());
    }

    let redaction = match command {
        "AE" if frame.starts_with(b"AE ")
            && frame.len() == 15
            && frame.get(11).is_some_and(|byte| *byte == b',') =>
        {
            Some(("serial-number", 3, 8))
        }
        "CS" if frame.starts_with(b"CS ") => Some(("callsign", 3, frame.len().saturating_sub(3))),
        command if command.starts_with("DC ") && frame.starts_with(command.as_bytes()) => {
            let start = command.len().saturating_add(1);
            frame
                .get(command.len())
                .filter(|byte| **byte == b',')
                .map(|_| ("callsign", start, frame.len().saturating_sub(start)))
        }
        command
            if (command.starts_with("FO ") || command.starts_with("FQ "))
                && frame.starts_with(command.as_bytes()) =>
        {
            let start = command.len().saturating_add(1);
            frame
                .get(command.len())
                .filter(|byte| **byte == b',')
                .map(|_| ("frequency", start, frame.len().saturating_sub(start)))
        }
        _ => None,
    };

    redaction.map_or_else(
        || {
            let length = frame.len();
            (
                mask_range(&terminated, 0, length),
                vec![json!({
                    "target": "response_hex",
                    "class": "unknown-frame",
                    "start_byte": 0,
                    "length_bytes": length,
                })],
            )
        },
        |(class, start, length)| {
            (
                mask_range(&terminated, start, length),
                vec![json!({
                    "target": "response_hex",
                    "class": class,
                    "start_byte": start,
                    "length_bytes": length,
                })],
            )
        },
    )
}

fn mask_unknown_bytes(bytes: &[u8]) -> String {
    let mut result = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        if *byte == b'\r' || *byte == b'\n' {
            result.push_str(&hex(std::slice::from_ref(byte)));
        } else {
            result.push_str("??");
        }
    }
    result
}

fn mask_range(bytes: &[u8], start: usize, length: usize) -> String {
    let end = start.saturating_add(length);
    let mut result = String::with_capacity(bytes.len().saturating_mul(2));
    for (index, byte) in bytes.iter().enumerate() {
        if index >= start && index < end {
            result.push_str("??");
        } else {
            result.push_str(&hex(std::slice::from_ref(byte)));
        }
    }
    result
}

fn hex_with_cr(frame: &[u8]) -> String {
    let mut bytes = frame.to_vec();
    bytes.push(b'\r');
    hex(&bytes)
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789ABCDEF";
    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        if let (Some(high), Some(low)) = (
            DIGITS.get(usize::from(*byte >> 4)),
            DIGITS.get(usize::from(*byte & 0x0F)),
        ) {
            result.push(char::from(*high));
            result.push(char::from(*low));
        }
    }
    result
}

fn write_json_line(writer: &mut BufWriter<File>, value: &Value) -> AuditResult<()> {
    serde_json::to_writer(&mut *writer, value)?;
    writer.write_all(b"\n")?;
    Ok(())
}

fn validate_response_grammar(command: &str, response: &[u8]) -> bool {
    match command {
        "ID" => response == b"ID TH-D75",
        "FV" => response == b"FV 1.03",
        "TY" => validate_ty_shape(response),
        "AE" => validate_ae(response),
        "AI" => decimal_field(response, b"AI ", 1, 0, 1),
        "TN" => ranged_csv(response, b"TN ", &[(0, 3), (0, 1)]),
        "PT" => decimal_field(response, b"PT ", 1, 0, 3),
        "CS" => validate_callsign(response),
        "VX" => decimal_field(response, b"VX ", 1, 0, 1),
        "IO" => decimal_field(response, b"IO ", 1, 0, 2),
        "AG" => decimal_field(response, b"AG ", 3, 0, 200),
        "BC" => decimal_field(response, b"BC ", 1, 0, 1),
        "DL" => decimal_field(response, b"DL ", 1, 0, 1),
        "PS" => response == b"PS 1",
        "BT" => decimal_field(response, b"BT ", 1, 0, 1),
        "SD" => decimal_field(response, b"SD ", 1, 0, 1),
        "FR" => decimal_field(response, b"FR ", 1, 0, 1),
        "BL" => decimal_field(response, b"BL ", 1, 0, 5),
        "FQ 0" => decimal_field(response, b"FQ 0,", 10, 0, u64::MAX),
        "FQ 1" => decimal_field(response, b"FQ 1,", 10, 0, u64::MAX),
        "FO 0" => validate_fo(response, b"FO 0"),
        "FO 1" => validate_fo(response, b"FO 1"),
        "BY 0" => decimal_field(response, b"BY 0,", 1, 0, 1),
        "BY 1" => decimal_field(response, b"BY 1,", 1, 0, 1),
        "SM 0" => hex_field(response, b"SM 0,", 0x0F),
        "SM 1" => hex_field(response, b"SM 1,", 0x0F),
        "SQ 0" => decimal_field(response, b"SQ 0,", 1, 0, 6),
        "SQ 1" => decimal_field(response, b"SQ 1,", 1, 0, 6),
        "MD 0" => decimal_field(response, b"MD 0,", 1, 0, 7),
        "MD 1" => decimal_field(response, b"MD 1,", 1, 0, 7),
        "PC 0" => decimal_field(response, b"PC 0,", 1, 0, 3),
        "PC 1" => decimal_field(response, b"PC 1,", 1, 0, 3),
        "RA 0" => decimal_field(response, b"RA 0,", 1, 0, 1),
        "RA 1" => decimal_field(response, b"RA 1,", 1, 0, 1),
        "VM 0" => decimal_field(response, b"VM 0,", 1, 0, 3),
        "VM 1" => decimal_field(response, b"VM 1,", 1, 0, 3),
        "SF 0" => hex_field(response, b"SF 0,", 0x0B),
        "SF 1" => hex_field(response, b"SF 1,", 0x0B),
        "SH 0" => decimal_field(response, b"SH 0,", 1, 0, 4),
        "SH 1" => decimal_field(response, b"SH 1,", 1, 0, 4),
        "SH 2" => decimal_field(response, b"SH 2,", 1, 0, 3),
        "GP" => ranged_csv(response, b"GP ", &[(0, 1), (0, 1)]),
        "GM" => decimal_field(response, b"GM ", 1, 0, 1),
        "FS" => decimal_field(response, b"FS ", 1, 0, 3),
        "FT" => decimal_field(response, b"FT ", 1, 0, 1),
        "VD" => decimal_field(response, b"VD ", 1, 0, 6),
        "VG" => decimal_field(response, b"VG ", 1, 0, 9),
        "BS" => decimal_field(response, b"BS ", 1, 0, 1),
        "LC" => decimal_field(response, b"LC ", 1, 0, 3),
        "GS" => ranged_csv(
            response,
            b"GS ",
            &[(0, 1), (0, 1), (0, 1), (0, 1), (0, 1), (0, 1)],
        ),
        "MS" => decimal_field(response, b"MS ", 1, 0, 5),
        "AS" => decimal_field(response, b"AS ", 1, 0, 1),
        "DC 1" => validate_dc(response, b"DC 1,"),
        "DC 2" => validate_dc(response, b"DC 2,"),
        "DC 3" => validate_dc(response, b"DC 3,"),
        "DC 4" => validate_dc(response, b"DC 4,"),
        "DC 5" => validate_dc(response, b"DC 5,"),
        "DC 6" => validate_dc(response, b"DC 6,"),
        "DS" => decimal_field(response, b"DS ", 1, 1, 6),
        "RT" => response == b"RT ------------" || validate_rt(response),
        "GW" => decimal_field(response, b"GW ", 1, 0, 1),
        "TN 0,0" => response == b"TN 0,0",
        "PT 0" => response == b"PT 0",
        "VX 0" => response == b"VX 0",
        "IO 0" => response == b"IO 0",
        "AI 0" => response == b"AI 0",
        _ => false,
    }
}

fn validate_ty_shape(response: &[u8]) -> bool {
    matches!(
        response,
        [
            b'T',
            b'Y',
            b' ',
            b'E' | b'J' | b'K' | b'0',
            b',',
            b'0'..=b'9' | b'A'..=b'F',
        ]
    )
}

fn decimal_field(response: &[u8], prefix: &[u8], width: usize, min: u64, max: u64) -> bool {
    response.strip_prefix(prefix).is_some_and(|field| {
        field.len() == width
            && parse_decimal(field).is_some_and(|value| value >= min && value <= max)
    })
}

fn parse_decimal(field: &[u8]) -> Option<u64> {
    if field.is_empty() {
        return None;
    }
    field.iter().try_fold(0_u64, |value, byte| {
        let digit = byte.checked_sub(b'0')?;
        if digit > 9 {
            return None;
        }
        value.checked_mul(10)?.checked_add(u64::from(digit))
    })
}

fn hex_field(response: &[u8], prefix: &[u8], max: u8) -> bool {
    response.strip_prefix(prefix).is_some_and(|field| {
        matches!(field, [value] if uppercase_hex_value(*value).is_some_and(|raw| raw <= max))
    })
}

fn uppercase_hex_value(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => value.checked_sub(b'0'),
        b'A'..=b'F' => value.checked_sub(b'A').and_then(|raw| raw.checked_add(10)),
        _ => None,
    }
}

fn ranged_csv(response: &[u8], prefix: &[u8], ranges: &[(u64, u64)]) -> bool {
    let Some(payload) = response.strip_prefix(prefix) else {
        return false;
    };
    let mut fields = payload.split(|byte| *byte == b',');
    for (min, max) in ranges {
        let Some(field) = fields.next() else {
            return false;
        };
        if !parse_decimal(field).is_some_and(|value| value >= *min && value <= *max) {
            return false;
        }
    }
    fields.next().is_none()
}

fn validate_ae(response: &[u8]) -> bool {
    let Some(payload) = response.strip_prefix(b"AE ") else {
        return false;
    };
    let mut fields = payload.split(|byte| *byte == b',');
    let Some(serial) = fields.next() else {
        return false;
    };
    let Some(model) = fields.next() else {
        return false;
    };
    serial.len() == 8
        && serial.iter().all(u8::is_ascii_alphanumeric)
        && model.len() == 3
        && model.iter().all(u8::is_ascii_alphanumeric)
        && fields.next().is_none()
}

fn validate_callsign(response: &[u8]) -> bool {
    response.strip_prefix(b"CS ").is_some_and(|payload| {
        payload.len() <= 9
            && payload
                .iter()
                .all(|byte| byte.is_ascii_graphic() || *byte == b' ')
    })
}

fn validate_fo(response: &[u8], expected_first: &[u8]) -> bool {
    let mut fields = response.split(|byte| *byte == b',');
    let Some(first) = fields.next() else {
        return false;
    };
    let Some(frequency) = fields.next() else {
        return false;
    };
    let Some(offset) = fields.next() else {
        return false;
    };
    first == expected_first
        && frequency.len() == 10
        && parse_decimal(frequency).is_some()
        && offset.len() == 10
        && parse_decimal(offset).is_some()
        && fields.clone().count() == 18
        && fields.all(|field| {
            field
                .iter()
                .all(|byte| byte.is_ascii_graphic() || *byte == b' ')
        })
}

fn validate_dc(response: &[u8], prefix: &[u8]) -> bool {
    let Some(payload) = response.strip_prefix(prefix) else {
        return false;
    };
    let mut fields = payload.split(|byte| *byte == b',');
    let Some(callsign) = fields.next() else {
        return false;
    };
    let Some(suffix) = fields.next() else {
        return false;
    };
    callsign.len() <= 8
        && suffix.len() <= 4
        && callsign
            .iter()
            .chain(suffix)
            .all(|byte| byte.is_ascii_graphic() || *byte == b' ')
        && fields.next().is_none()
}

fn validate_rt(response: &[u8]) -> bool {
    let Some(payload) = response.strip_prefix(b"RT ") else {
        return false;
    };
    if payload.len() != 12 || parse_decimal(payload).is_none() {
        return false;
    }
    let mut pairs = payload.chunks_exact(2);
    let Some(year) = pairs.next().and_then(parse_decimal) else {
        return false;
    };
    let Some(month) = pairs.next().and_then(parse_decimal) else {
        return false;
    };
    let Some(day) = pairs.next().and_then(parse_decimal) else {
        return false;
    };
    let Some(hour) = pairs.next().and_then(parse_decimal) else {
        return false;
    };
    let Some(minute) = pairs.next().and_then(parse_decimal) else {
        return false;
    };
    let Some(second) = pairs.next().and_then(parse_decimal) else {
        return false;
    };
    if !pairs.remainder().is_empty()
        || !(1..=12).contains(&month)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return false;
    }
    let full_year = 2000_u64.saturating_add(year);
    let leap = full_year.is_multiple_of(4)
        && (!full_year.is_multiple_of(100) || full_year.is_multiple_of(400));
    let max_day = match month {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    (1..=max_day).contains(&day)
}

fn validate_identity(observed: &[(String, Vec<u8>)]) -> AuditResult<()> {
    expect_response(observed, "ID", b"ID TH-D75")?;
    expect_response(observed, "FV", b"FV 1.03")?;
    expect_response(observed, "TY", EXPECTED_TY)
}

fn validate_safety_read_grammar(observed: &[(String, Vec<u8>)]) -> AuditResult<()> {
    let ai = response_for(observed, "AI")?;
    let tn = response_for(observed, "TN")?;
    let pt = response_for(observed, "PT")?;
    let vx = response_for(observed, "VX")?;
    let io = response_for(observed, "IO")?;
    if !matches!(ai, [b'A', b'I', b' ', b'0' | b'1'])
        || !matches!(tn, [b'T', b'N', b' ', b'0'..=b'9', b',', b'0'..=b'9'])
        || !matches!(pt, [b'P', b'T', b' ', b'0'..=b'3'])
        || !matches!(vx, [b'V', b'X', b' ', b'0' | b'1'])
        || !matches!(io, [b'I', b'O', b' ', b'0'..=b'2'])
    {
        return Err(invalid_input(
            "one or more containment pre-read responses failed strict grammar",
        ));
    }
    Ok(())
}

fn validate_cat_containment(observed: &[(String, Vec<u8>)]) -> AuditResult<()> {
    let exact = [
        ("AI", b"AI 0".as_slice()),
        ("PT", b"PT 0".as_slice()),
        ("VX", b"VX 0".as_slice()),
        ("IO", b"IO 0".as_slice()),
    ];
    let mut mismatches = Vec::new();
    for (command, expected) in exact {
        let response = response_for(observed, command)?;
        if response != expected {
            mismatches.push(format!("{command}={}", String::from_utf8_lossy(response)));
        }
    }
    let tn = response_for(observed, "TN")?;
    if tn != b"TN 0,0" && tn != b"TN 0,1" {
        mismatches.push(format!("TN={}", String::from_utf8_lossy(tn)));
    }
    if mismatches.is_empty() {
        Ok(())
    } else {
        Err(invalid_input(format!(
            "CAT containment subset failed: {}; the physical safety checklist remains \
             independently required",
            mismatches.join(", ")
        )))
    }
}

fn expect_response(
    observed: &[(String, Vec<u8>)],
    command: &str,
    expected: &[u8],
) -> AuditResult<()> {
    let response = response_for(observed, command)?;
    if response == expected {
        Ok(())
    } else {
        Err(invalid_input(format!(
            "{command} identity mismatch: {}",
            String::from_utf8_lossy(response)
        )))
    }
}

fn response_for<'a>(observed: &'a [(String, Vec<u8>)], command: &str) -> AuditResult<&'a [u8]> {
    observed
        .iter()
        .find(|(candidate, _)| candidate == command)
        .map(|(_, response)| response.as_slice())
        .ok_or_else(|| invalid_input(format!("missing response for {command}")))
}

fn confirm_ui_checked(endpoint: &Endpoint) -> AuditResult<()> {
    use std::io::IsTerminal;

    if !io::stdin().is_terminal() {
        return Err(invalid_input(
            "the radio UI checklist attestation requires an interactive terminal",
        ));
    }
    let required = format!("RADIO UI CHECKED {}", endpoint.value());
    println!("Before the endpoint is opened, attest that all of these are currently true:");
    for assertion in OPERATOR_ASSERTIONS {
        println!("  - {assertion}");
    }
    println!("This records an operator attestation, not independent verification.");
    println!("Type exactly: {required}");
    io::stdout().flush()?;
    let entered = read_confirmation_line()?;
    if entered != required {
        return Err(invalid_input(
            "UI checklist attestation did not match; no port was opened",
        ));
    }
    Ok(())
}

fn read_confirmation_line() -> AuditResult<String> {
    let mut entered = String::new();
    if io::stdin().read_line(&mut entered)? == 0 {
        return Err(invalid_input(
            "confirmation input closed before a phrase was entered",
        ));
    }
    entered
        .strip_suffix("\r\n")
        .or_else(|| entered.strip_suffix('\n'))
        .map(str::to_owned)
        .ok_or_else(|| invalid_input("confirmation must end with Enter"))
}

fn confirm_make_safe(endpoint: &Endpoint) -> AuditResult<()> {
    use std::io::IsTerminal;

    if !io::stdin().is_terminal() {
        return Err(invalid_input(
            "make-safe confirmation requires an interactive terminal",
        ));
    }
    let required = format!("MAKE SAFE {}", endpoint.value());
    println!("This will leave TNC Off, PT Manual, VOX Off, IO AF, and AI Off.");
    println!("Type exactly: {required}");
    io::stdout().flush()?;
    let entered = read_confirmation_line()?;
    if entered != required {
        return Err(invalid_input(
            "confirmation did not match; no port was opened",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    use kenwood_thd75::error::TransportError;
    use kenwood_thd75::transport::MockTransport;

    type TestResult = AuditResult<()>;

    fn test_config(args: &[&str]) -> AuditResult<Config> {
        parse_args_from(args.iter().copied())
    }

    #[test]
    fn cli_requires_exactly_one_endpoint() {
        let missing = test_config(&["baseline", "--capture-root", "/tmp/audit"])
            .expect_err("an endpoint is required");
        assert!(missing.to_string().contains("exactly one"));

        let both = test_config(&[
            "baseline",
            "--port",
            "/dev/cu.usbmodem101",
            "--bluetooth",
            "TH-D75",
            "--capture-root",
            "/tmp/audit",
        ])
        .expect_err("USB and Bluetooth are mutually exclusive");
        assert!(both.to_string().contains("exactly one"));
    }

    #[test]
    fn cli_preserves_interactive_usb_baseline() -> TestResult {
        let config = test_config(&[
            "baseline",
            "--port",
            "/dev/cu.usbmodem101",
            "--capture-root",
            "/tmp/audit",
        ])?;
        assert_eq!(
            config.endpoint,
            Endpoint::Usb("/dev/cu.usbmodem101".to_owned())
        );
        assert_eq!(config.mode, Mode::Baseline);
        assert_eq!(config.profile, BaselineProfile::StockDefault);
        assert!(!config.machine_checked_read_only);
        Ok(())
    }

    #[test]
    fn cli_accepts_noninteractive_bluetooth_baseline() -> TestResult {
        let config = test_config(&[
            "--bluetooth",
            "TH-D75",
            "--machine-checked-read-only",
            "--capture-root",
            "/tmp/audit",
        ])?;
        assert_eq!(config.endpoint, Endpoint::Bluetooth("TH-D75".to_owned()));
        assert_eq!(config.mode, Mode::Baseline);
        assert_eq!(config.profile, BaselineProfile::StockDefault);
        assert!(config.machine_checked_read_only);
        Ok(())
    }

    #[test]
    fn automation_profile_requires_explicit_machine_checked_baseline() -> TestResult {
        let config = test_config(&[
            "baseline",
            "--automation",
            "--bluetooth",
            "TH-D75",
            "--machine-checked-read-only",
            "--capture-root",
            "/tmp/audit",
        ])?;
        assert_eq!(config.profile, BaselineProfile::Automation);
        assert_eq!(config.mode, Mode::Baseline);
        assert!(config.machine_checked_read_only);
        assert_eq!(config.profile.case_count(), 59);

        let interactive = test_config(&[
            "baseline",
            "--automation",
            "--bluetooth",
            "TH-D75",
            "--capture-root",
            "/tmp/audit",
        ])
        .expect_err("V1.03.AZM unexpectedly accepted an operator-only preflight");
        assert!(
            interactive
                .to_string()
                .contains("requires --machine-checked-read-only")
        );

        let write_capable = test_config(&[
            "make-safe",
            "--automation",
            "--bluetooth",
            "TH-D75",
            "--capture-root",
            "/tmp/audit",
        ])
        .expect_err("V1.03.AZM unexpectedly enabled a write-capable mode");
        assert!(write_capable.to_string().contains("read-only baseline"));
        Ok(())
    }

    #[test]
    fn machine_checked_mode_cannot_enable_make_safe() {
        let error = test_config(&[
            "make-safe",
            "--bluetooth",
            "TH-D75",
            "--machine-checked-read-only",
            "--capture-root",
            "/tmp/audit",
        ])
        .expect_err("machine checking is read-only");
        assert!(
            error
                .to_string()
                .contains("only for the read-only baseline")
        );
    }

    #[test]
    fn evidence_names_transport_and_redacts_endpoint() -> TestResult {
        let config = test_config(&[
            "baseline",
            "--bluetooth",
            "TH-D75",
            "--machine-checked-read-only",
            "--capture-root",
            "/tmp/audit",
        ])?;
        let private = session_start_record(&config, "capture", 123, EvidencePrivacy::Private);
        let sanitized = session_start_record(&config, "capture", 123, EvidencePrivacy::Sanitized);

        assert_eq!(
            private.get("transport").and_then(Value::as_str),
            Some("bluetooth-rfcomm")
        );
        assert_eq!(
            private.get("endpoint").and_then(Value::as_str),
            Some("TH-D75")
        );
        assert_eq!(
            private
                .get("preflight_evidence_basis")
                .and_then(Value::as_str),
            Some("machine-checked-read-only")
        );
        assert_eq!(
            private.get("profile").and_then(Value::as_str),
            Some("stock-default")
        );
        assert_eq!(
            private.get("fixed_cat_case_count").and_then(Value::as_u64),
            Some(61)
        );
        assert_eq!(
            sanitized
                .get("endpoint")
                .and_then(|value| value.get("$redacted"))
                .and_then(Value::as_str),
            Some("endpoint")
        );
        assert_eq!(
            sanitized
                .get("endpoint")
                .and_then(|value| value.get("byte_len"))
                .and_then(Value::as_u64),
            Some(6)
        );
        Ok(())
    }

    #[test]
    fn every_allowlisted_frame_is_cr_terminated() -> TestResult {
        for spec in IDENTITY_READS
            .iter()
            .chain(BASELINE_PREFLIGHT)
            .chain(BASELINE_REST)
            .chain(SAFETY_READS)
        {
            let _command = command_text(spec.wire)?;
            assert!(
                spec.wire.len() >= 3,
                "every CAT frame needs a two-byte mnemonic and CR"
            );
        }
        for target in CONTAINMENT_TARGETS {
            let _write = command_text(target.write.wire)?;
            let _read = command_text(target.read.wire)?;
        }
        Ok(())
    }

    #[test]
    fn baseline_is_the_exact_61_frame_read_allowlist() -> TestResult {
        let specs: Vec<&CommandSpec> = IDENTITY_READS
            .iter()
            .chain(BASELINE_PREFLIGHT)
            .chain(BASELINE_REST)
            .collect();
        let frames: Vec<&[u8]> = specs.iter().map(|spec| spec.wire).collect();
        assert_eq!(frames.len(), 61);
        let unique: BTreeSet<&[u8]> = frames.iter().copied().collect();
        assert_eq!(unique.len(), frames.len());
        let probe_ids: BTreeSet<&str> = specs.iter().map(|spec| spec.probe_id).collect();
        assert_eq!(probe_ids.len(), specs.len());
        assert!(probe_ids.contains("A1-P0-BL-READ"));
        assert!(
            probe_ids
                .iter()
                .all(|probe_id| probe_id.starts_with("A1-P0-"))
        );

        let prohibited = [b"0G".as_slice(), b"0E", b"BE", b"DW", b"UP", b"TX", b"0M"];
        for frame in frames {
            let mnemonic = frame
                .get(..2)
                .ok_or_else(|| invalid_input("allowlisted frame has no mnemonic"))?;
            assert!(!prohibited.contains(&mnemonic));
        }
        Ok(())
    }

    #[test]
    fn automation_fixed_cat_profile_excludes_only_bare_gm_and_gw() -> TestResult {
        let stock: Vec<&CommandSpec> = IDENTITY_READS
            .iter()
            .chain(BASELINE_PREFLIGHT)
            .chain(baseline_rest_specs(BaselineProfile::StockDefault))
            .collect();
        let automation: Vec<&CommandSpec> = IDENTITY_READS
            .iter()
            .chain(BASELINE_PREFLIGHT)
            .chain(baseline_rest_specs(BaselineProfile::Automation))
            .collect();
        assert_eq!(stock.len(), 61);
        assert_eq!(automation.len(), 59);
        assert_eq!(BaselineProfile::StockDefault.case_count(), stock.len());
        assert_eq!(BaselineProfile::Automation.case_count(), automation.len());

        let automation_wires: BTreeSet<&[u8]> = automation.iter().map(|spec| spec.wire).collect();
        let removed: Vec<&[u8]> = stock
            .iter()
            .map(|spec| spec.wire)
            .filter(|wire| !automation_wires.contains(wire))
            .collect();
        assert_eq!(removed, vec![b"GM\r".as_slice(), b"GW\r".as_slice()]);
        assert!(!automation_wires.contains(b"GM\r".as_slice()));
        assert!(!automation_wires.contains(b"GW\r".as_slice()));
        assert!(automation.iter().all(|spec| !spec.state_change));
        Ok(())
    }

    #[test]
    fn automation_evidence_records_profile_attestation_reopen_and_case_count() -> TestResult {
        let config = test_config(&[
            "baseline",
            "--automation",
            "--bluetooth",
            "TH-D75",
            "--machine-checked-read-only",
            "--capture-root",
            "/tmp/audit",
        ])?;
        let start = session_start_record(&config, "capture", 123, EvidencePrivacy::Private);
        assert_eq!(
            start.get("profile").and_then(Value::as_str),
            Some("automation")
        );
        assert_eq!(
            start.get("fixed_cat_case_count").and_then(Value::as_u64),
            Some(59)
        );

        let attestation = AutomationAttestation {
            abi: AutomationAbi {
                version: 1,
                features: 0x1F,
                max_key: 0x18,
                max_phase: 2,
            },
            transport_reopened_before_raw_audit: true,
        };
        let record = automation_attestation_record(
            "capture",
            &config.endpoint,
            124,
            EvidencePrivacy::Private,
            Some(&attestation),
            None,
        );
        assert_eq!(
            record.get("result_code").and_then(Value::as_str),
            Some("exact-automation-qualified-transport-reopened")
        );
        assert_eq!(
            record.get("automation_qualified").and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            record
                .get("transport_reopened_before_raw_audit")
                .and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            record.get("fixed_cat_case_count").and_then(Value::as_u64),
            Some(59)
        );
        assert_eq!(
            record
                .get("abi")
                .and_then(|abi| abi.get("features"))
                .and_then(Value::as_u64),
            Some(0x1F)
        );

        let manifest = automation_attestation_manifest(config.profile, Some(&attestation));
        assert_eq!(
            manifest.get("status").and_then(Value::as_str),
            Some("passed")
        );
        assert_eq!(
            manifest
                .get("transport_reopened_before_raw_audit")
                .and_then(Value::as_bool),
            Some(true)
        );
        Ok(())
    }

    #[test]
    fn response_matching_requires_a_mnemonic_boundary() {
        assert!(mnemonic_matches(b"FQ 0,0145200000", b"FQ"));
        assert!(mnemonic_matches(b"DW", b"DW"));
        assert!(!mnemonic_matches(b"FQX", b"FQ"));
        assert!(!mnemonic_matches(b"N", b"FQ"));
    }

    #[test]
    fn sensitive_response_hex_preserves_length_and_masks_payload() {
        let response = b"CS EXAMPLE";
        let (rendered, redactions) = sanitize_response_hex("CS", true, response);
        assert_eq!(rendered, "435320??????????????0D");
        assert_eq!(rendered.len(), (response.len() + 1) * 2);
        assert_eq!(redactions.len(), 1);
    }

    #[test]
    fn safe_preconditions_reject_automatic_beaconing() {
        let observed = vec![
            ("AI".to_owned(), b"AI 0".to_vec()),
            ("TN".to_owned(), b"TN 0,0".to_vec()),
            ("PT".to_owned(), b"PT 2".to_vec()),
            ("VX".to_owned(), b"VX 0".to_vec()),
            ("IO".to_owned(), b"IO 0".to_vec()),
        ];
        assert!(validate_cat_containment(&observed).is_err());
    }

    #[test]
    fn strict_grammar_covers_firmware_boundaries() {
        assert!(validate_response_grammar("BL", b"BL 5"));
        assert!(validate_response_grammar("VD", b"VD 6"));
        assert!(validate_response_grammar("MS", b"MS 5"));
        assert!(validate_response_grammar("PT", b"PT 3"));
        assert!(validate_response_grammar("SF 1", b"SF 1,B"));
        assert!(validate_ty_shape(b"TY E,A"));
        assert!(!validate_ty_shape(b"TY X,2"));
        assert!(!validate_ty_shape(b"TY K,a"));
        assert!(!validate_response_grammar("BL", b"BL 6"));
        assert!(!validate_response_grammar("VD", b"VD 7"));
        assert!(!validate_response_grammar("SF 1", b"SF 1,C"));
    }

    #[test]
    fn rt_grammar_rejects_impossible_dates() {
        assert!(validate_response_grammar("RT", b"RT 240229235959"));
        assert!(validate_response_grammar("RT", b"RT ------------"));
        assert!(!validate_response_grammar("RT", b"RT 230229235959"));
        assert!(!validate_response_grammar("RT", b"RT 241332000000"));
    }

    #[tokio::test(start_paused = true)]
    async fn precommand_resync_discards_a_stale_partial_frame() -> TestResult {
        let mut transport = MockTransport::new();
        transport.pend_when_empty();
        transport.expect_reads(b"ID\r", &[b"ID TH-D75\rSTALE"]);
        transport.expect(b"FV\r", b"FV 1.03\r");
        let mut codec = Codec::new();

        let first = exchange(
            &mut transport,
            &mut codec,
            &CommandSpec::public("A1-P0-ID-READ", b"ID\r"),
        )
        .await;
        assert_eq!(first.response(), Some(b"ID TH-D75".as_slice()));

        let second = exchange(
            &mut transport,
            &mut codec,
            &CommandSpec::public("A1-P0-FV-READ", b"FV\r"),
        )
        .await;
        assert_eq!(second.response(), Some(b"FV 1.03".as_slice()));
        transport.assert_complete();
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn indexed_exchange_skips_wrong_selector_and_preserves_other_frames() -> TestResult {
        let mut transport = MockTransport::new();
        transport.pend_when_empty();
        transport.expect_reads(
            b"FQ 0\r",
            &[b"$GPGGA,private\rFQ 1,0435000000\rFQ 0,0145200000\rAI 0\r"],
        );
        let mut codec = Codec::new();
        let spec = CommandSpec::indexed_sensitive("A1-P0-FQ-0-READ", b"FQ 0\r", b"FQ 0,");

        let result = exchange(&mut transport, &mut codec, &spec).await;
        assert_eq!(result.response(), Some(b"FQ 0,0145200000".as_slice()));
        assert_eq!(result.unsolicited.len(), 3);
        transport.assert_complete();
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn response_timeout_is_bounded() {
        let mut transport = MockTransport::new();
        transport.pend_when_empty();
        transport.expect_hang(b"ID\r");
        let mut codec = Codec::new();
        let result = exchange(
            &mut transport,
            &mut codec,
            &CommandSpec::public("A1-P0-ID-READ", b"ID\r"),
        )
        .await;
        assert_eq!(result.terminal_code(), "response-timeout");
        transport.assert_complete();
    }

    #[tokio::test(start_paused = true)]
    async fn collect_all_keeps_complete_n_and_bad_grammar_cases() -> TestResult {
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let root = std::env::temp_dir().join(format!(
            "kenwood-hardware-audit-collect-test-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir(&root)?;
        let mut captures = create_capture_files(&root)?;

        let specs = [
            CommandSpec::public("A1-P0-AG-READ", b"AG\r"),
            CommandSpec::public("A1-P0-BC-READ", b"BC\r"),
            CommandSpec::public("A1-P0-DL-READ", b"DL\r"),
        ];
        let mut transport = MockTransport::new();
        transport.pend_when_empty();
        transport.expect(b"AG\r", b"N\r");
        transport.expect(b"BC\r", b"BC 9\r");
        transport.expect(b"DL\r", b"DL 1\r");
        let mut codec = Codec::new();

        let summary = run_specs_collect_all(
            &mut transport,
            &mut codec,
            &mut captures.private,
            &mut captures.sanitized,
            0,
            &specs,
        )
        .await?;
        assert_eq!(summary.passed, 1);
        assert_eq!(summary.inconclusive, 1);
        assert_eq!(summary.failed, 1);
        transport.assert_complete();
        drop(captures);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[tokio::test]
    async fn continuously_busy_stale_input_is_bounded() -> TestResult {
        #[derive(Debug)]
        struct BusyTransport {
            writes: usize,
        }

        impl Transport for BusyTransport {
            async fn write(&mut self, _data: &[u8]) -> Result<(), TransportError> {
                self.writes = self.writes.saturating_add(1);
                Ok(())
            }

            async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, TransportError> {
                let frame = b"AI 0\r";
                let target = buffer.get_mut(..frame.len()).ok_or_else(|| {
                    TransportError::Read(io::Error::other("test buffer too short"))
                })?;
                target.copy_from_slice(frame);
                Ok(frame.len())
            }

            async fn close(&mut self) -> Result<(), TransportError> {
                Ok(())
            }
        }

        let mut transport = BusyTransport { writes: 0 };
        let mut codec = Codec::new();
        let drain = drain_stale(&mut transport, &mut codec).await;
        assert!(drain.truncated);
        assert!(drain.bytes.len() >= MAX_DRAIN_BYTES);
        assert_eq!(transport.writes, 0);

        let result = exchange(
            &mut transport,
            &mut codec,
            &CommandSpec::public("A1-P0-ID-READ", b"ID\r"),
        )
        .await;
        assert_eq!(result.terminal_code(), "drain-not-quiescent");
        assert_eq!(transport.writes, 0);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn capture_creation_preserves_parent_mode_and_uses_owner_only_children() -> TestResult {
        use std::os::unix::fs::PermissionsExt;

        let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let root = std::env::temp_dir().join(format!(
            "kenwood-hardware-audit-test-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir(&root)?;
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755))?;

        let captures = create_capture_files(&root)?;
        let root_mode = std::fs::metadata(&root)?.permissions().mode() & 0o777;
        let session_mode = std::fs::metadata(&captures.session_dir)?
            .permissions()
            .mode()
            & 0o777;
        let private_mode = std::fs::metadata(captures.session_dir.join("private.jsonl"))?
            .permissions()
            .mode()
            & 0o777;
        let sanitized_mode = std::fs::metadata(captures.session_dir.join("sanitized.jsonl"))?
            .permissions()
            .mode()
            & 0o777;
        let config = test_config(&[
            "baseline",
            "--port",
            "/dev/cu.usbmodem101",
            "--capture-root",
            root.to_str()
                .ok_or_else(|| invalid_input("temporary capture root is not UTF-8"))?,
        ])?;
        write_capture_manifest(&captures, "aborted", &config, None)?;
        let manifest_mode = std::fs::metadata(captures.session_dir.join("manifest.json"))?
            .permissions()
            .mode()
            & 0o777;
        let manifest: Value =
            serde_json::from_slice(&std::fs::read(captures.session_dir.join("manifest.json"))?)?;

        assert_eq!(root_mode, 0o755);
        assert_eq!(session_mode, 0o700);
        assert_eq!(private_mode, 0o600);
        assert_eq!(sanitized_mode, 0o600);
        assert_eq!(manifest_mode, 0o600);
        assert_eq!(
            manifest.get("session_status").and_then(Value::as_str),
            Some("aborted")
        );
        assert_eq!(
            manifest.get("profile").and_then(Value::as_str),
            Some("stock-default")
        );
        assert_eq!(
            manifest
                .get("allowlist")
                .and_then(|allowlist| allowlist.get("case_count"))
                .and_then(Value::as_u64),
            Some(61)
        );
        assert_eq!(
            manifest
                .get("automation_attestation")
                .and_then(|attestation| attestation.get("status"))
                .and_then(Value::as_str),
            Some("not-applicable")
        );
        assert_eq!(
            manifest
                .get("private_transcript")
                .and_then(|value| value.get("sha256"))
                .and_then(Value::as_str)
                .map(str::len),
            Some(64)
        );
        drop(captures);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
}
