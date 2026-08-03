//! Make a guarded, read-only backup of the complete TH-D75 MCP image.
//!
//! The backup is intentionally restricted to the supported USB radio and
//! firmware used by this bench. Before entering programming mode it reads the
//! raw CAT identity and checks five CAT-readable safety settings. By default,
//! the operator must separately attest the UI and peripheral conditions before
//! the port is opened. `--machine-checked-read-only` explicitly records that
//! this manual attestation was skipped and proceeds only when the limited CAT
//! subset is safe for a read-only MCP operation. In either mode, the operator
//! must confirm the exact port and output path at a terminal.
//!
//! ```text
//! cargo run -p kenwood-thd75 --example config_backup -- \
//!   --port /dev/cu.usbmodem101 \
//!   --output /absolute/private/directory/thd75-backup.bin
//! ```
//!
//! Add `--machine-checked-read-only` to skip the manual UI checklist without
//! claiming those unobservable conditions were verified.
//!
//! The existing output parent must be a canonical, owner-private 0700
//! directory, and the final output name must not exist. A new empty
//! same-directory 0600 staging file proves ownership and write access before
//! the port is opened. Backup bytes are not written to it until the MCP
//! operation has completed, the transport has been closed, and a fresh
//! connection to the same explicit USB path returns the identical raw
//! `ID`/`FV`/`TY` identity. The staged bytes are synced, read back exactly,
//! SHA-256 checked, then published under the final name with an atomic
//! no-clobber hard link.

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
use serde_json as _;
use thiserror as _;
use tokio_serial as _;
use tracing as _;

use std::error::Error as StdError;
use std::fs::{File, OpenOptions};
use std::io::{self, IsTerminal, Read, Seek, SeekFrom, Write};
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use kenwood_thd75::Radio;
use kenwood_thd75::error::Error as RadioError;
use kenwood_thd75::protocol::{Codec, programming};
use kenwood_thd75::radio::programming::McpSpeed;
use kenwood_thd75::transport::{SerialTransport, Transport};

type BackupError = Box<dyn StdError + Send + Sync>;
type BackupResult<T> = Result<T, BackupError>;

const CAT_BAUD: u32 = SerialTransport::DEFAULT_BAUD;
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(2);
const DRAIN_QUIET_WINDOW: Duration = Duration::from_millis(30);
const DRAIN_TOTAL_TIMEOUT: Duration = Duration::from_millis(250);
const MAX_UNSOLICITED_BYTES: usize = 64 * 1024;
const CLOSE_TIMEOUT: Duration = Duration::from_secs(2);
const RECOVERY_TIMEOUT: Duration = Duration::from_secs(30);
const TEMP_NAME_ATTEMPTS: u32 = 128;
const SHA256_ROUND_CONSTANTS: [u32; 64] = [
    0x428a_2f98,
    0x7137_4491,
    0xb5c0_fbcf,
    0xe9b5_dba5,
    0x3956_c25b,
    0x59f1_11f1,
    0x923f_82a4,
    0xab1c_5ed5,
    0xd807_aa98,
    0x1283_5b01,
    0x2431_85be,
    0x550c_7dc3,
    0x72be_5d74,
    0x80de_b1fe,
    0x9bdc_06a7,
    0xc19b_f174,
    0xe49b_69c1,
    0xefbe_4786,
    0x0fc1_9dc6,
    0x240c_a1cc,
    0x2de9_2c6f,
    0x4a74_84aa,
    0x5cb0_a9dc,
    0x76f9_88da,
    0x983e_5152,
    0xa831_c66d,
    0xb003_27c8,
    0xbf59_7fc7,
    0xc6e0_0bf3,
    0xd5a7_9147,
    0x06ca_6351,
    0x1429_2967,
    0x27b7_0a85,
    0x2e1b_2138,
    0x4d2c_6dfc,
    0x5338_0d13,
    0x650a_7354,
    0x766a_0abb,
    0x81c2_c92e,
    0x9272_2c85,
    0xa2bf_e8a1,
    0xa81a_664b,
    0xc24b_8b70,
    0xc76c_51a3,
    0xd192_e819,
    0xd699_0624,
    0xf40e_3585,
    0x106a_a070,
    0x19a4_c116,
    0x1e37_6c08,
    0x2748_774c,
    0x34b0_bcb5,
    0x391c_0cb3,
    0x4ed8_aa4a,
    0x5b9c_ca4f,
    0x682e_6ff3,
    0x748f_82ee,
    0x78a5_636f,
    0x84c8_7814,
    0x8cc7_0208,
    0x90be_fffa,
    0xa450_6ceb,
    0xbef9_a3f7,
    0xc671_78f2,
];
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
const CAT_SAFETY_SCOPE: &str = "The CAT subset checks only Auto Information, packet/TNC mode, \
    APRS Beacon TX Method, VOX, and the AF/IF output selection. It does not verify TX Inhibit, \
    scan state, RF power, callsign correctness, antenna routing, or attached audio hardware.";

const ID_COMMANDS: &[&[u8]] = &[b"ID\r", b"FV\r", b"TY\r"];
const CAT_SAFETY_COMMANDS: &[&[u8]] = &[b"AI\r", b"TN\r", b"PT\r", b"VX\r", b"IO\r"];

#[derive(Debug, PartialEq, Eq)]
struct Config {
    port: String,
    output: PathBuf,
    machine_checked_read_only: bool,
}

#[derive(Debug, PartialEq, Eq)]
struct RawIdentity {
    id: Vec<u8>,
    firmware: Vec<u8>,
    radio_type: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CatSafetySubset {
    auto_info: Vec<u8>,
    tnc: Vec<u8>,
    beacon: Vec<u8>,
    vox: Vec<u8>,
    io_port: Vec<u8>,
}

#[derive(Debug)]
struct OutputTarget {
    parent: PathBuf,
    final_path: PathBuf,
    directory: File,
}

#[derive(Debug)]
struct StagedOutput {
    target: OutputTarget,
    temporary_path: PathBuf,
    file: File,
    final_link_created: bool,
    committed: bool,
}

impl OutputTarget {
    fn prepare(final_path: &Path) -> BackupResult<Self> {
        if !final_path.is_absolute() {
            return Err(invalid_input("the backup output path must be absolute"));
        }
        let _file_name = final_path
            .file_name()
            .ok_or_else(|| invalid_input("the backup output path needs a file name"))?;
        let requested_parent = output_parent(final_path)?;
        let parent = requested_parent.canonicalize().map_err(|error| {
            invalid_input(format!(
                "the backup parent must already exist as a 0700 directory: {error}"
            ))
        })?;
        if requested_parent != parent {
            return Err(invalid_input(
                "the backup parent path must already be canonical and contain no symlink, '.' or \
                 '..' components",
            ));
        }
        reject_symlinked_ancestors(&parent)?;
        ensure_private_parent(&parent)?;
        validate_git_privacy(&parent)?;
        ensure_output_absent(final_path)?;

        let directory = File::open(&parent)?;
        let target = Self {
            parent,
            final_path: final_path.to_path_buf(),
            directory,
        };
        target.ensure_unchanged()?;
        Ok(target)
    }

    fn ensure_unchanged(&self) -> BackupResult<()> {
        reject_symlinked_ancestors(&self.parent)?;
        let current_parent = self.parent.canonicalize()?;
        if current_parent != self.parent {
            return Err(invalid_input(
                "the backup parent path changed or became an alias",
            ));
        }
        let path_metadata = std::fs::symlink_metadata(&self.parent)?;
        let handle_metadata = self.directory.metadata()?;
        validate_private_parent_metadata(&self.parent, &path_metadata)?;
        if !same_file(&path_metadata, &handle_metadata) {
            return Err(invalid_input(
                "the backup parent directory changed after validation",
            ));
        }
        Ok(())
    }

    fn stage(self) -> BackupResult<StagedOutput> {
        self.ensure_unchanged()?;
        ensure_output_absent(&self.final_path)?;
        let final_name = self
            .final_path
            .file_name()
            .ok_or_else(|| invalid_input("the backup output path needs a file name"))?;

        for attempt in 0..TEMP_NAME_ATTEMPTS {
            let mut temporary_name = std::ffi::OsString::from(".");
            temporary_name.push(final_name);
            temporary_name.push(format!(".partial.{}.{}", std::process::id(), attempt));
            let temporary_path = self.parent.join(temporary_name);

            let mut options = OpenOptions::new();
            let configured = options.read(true).write(true).create_new(true);
            #[cfg(unix)]
            let configured = configured.mode(0o600);
            match configured.open(&temporary_path) {
                Ok(file) => {
                    let staged = StagedOutput {
                        target: self,
                        temporary_path,
                        file,
                        final_link_created: false,
                        committed: false,
                    };
                    staged.validate_staged_file()?;
                    staged.target.ensure_unchanged()?;
                    ensure_output_absent(&staged.target.final_path)?;
                    return Ok(staged);
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(Box::new(error)),
            }
        }

        Err(invalid_input(format!(
            "could not reserve a private same-directory staging file after {TEMP_NAME_ATTEMPTS} \
             attempts"
        )))
    }
}

#[derive(Debug, PartialEq, Eq)]
struct PublishedOutput {
    path: PathBuf,
    sha256: String,
}

impl StagedOutput {
    fn validate_staged_file(&self) -> BackupResult<()> {
        let metadata = self.file.metadata()?;
        if !metadata.file_type().is_file() {
            return Err(invalid_input("the staging output is not a regular file"));
        }
        #[cfg(unix)]
        {
            let directory_metadata = self.target.directory.metadata()?;
            if metadata.mode() & 0o777 != 0o600
                || metadata.nlink() != 1
                || metadata.uid() != directory_metadata.uid()
            {
                return Err(invalid_input(
                    "the staging output must be a new owner-matched 0600 file with one link",
                ));
            }
        }
        Ok(())
    }

    fn publish(mut self, image: &[u8]) -> BackupResult<PublishedOutput> {
        let source_sha256 = sha256_bytes(image)?;
        self.file.write_all(image)?;
        self.file.sync_all()?;
        self.validate_staged_file()?;

        let stored_len = usize::try_from(self.file.metadata()?.len())
            .map_err(|_| invalid_input("stored backup length does not fit usize"))?;
        if stored_len != programming::TOTAL_SIZE || stored_len != image.len() {
            return Err(invalid_input(format!(
                "staged backup has {stored_len} bytes; expected {}",
                programming::TOTAL_SIZE
            )));
        }

        let readback_position = self.file.seek(SeekFrom::Start(0))?;
        if readback_position != 0 {
            return Err(invalid_input(
                "could not seek the staged backup to its first byte",
            ));
        }
        let mut readback = Vec::with_capacity(stored_len);
        let readback_len = self.file.read_to_end(&mut readback)?;
        if readback_len != stored_len {
            return Err(invalid_input(
                "the staged backup readback ended at an unexpected length",
            ));
        }
        let readback_sha256 = sha256_bytes(&readback)?;
        if readback_sha256 != source_sha256 {
            return Err(invalid_input(format!(
                "staged backup SHA-256 mismatch: memory={source_sha256}, file={readback_sha256}"
            )));
        }
        if readback != image {
            return Err(invalid_input(
                "staged backup bytes differ from the in-memory MCP image despite equal length",
            ));
        }

        self.target.ensure_unchanged()?;
        ensure_output_absent(&self.target.final_path)?;
        match std::fs::hard_link(&self.temporary_path, &self.target.final_path) {
            Ok(()) => self.final_link_created = true,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                return Err(invalid_input(
                    "the final backup name appeared before publication; it was not overwritten",
                ));
            }
            Err(error) => return Err(Box::new(error)),
        }
        self.target.directory.sync_all()?;

        validate_same_regular_file(
            &self.file,
            &self.target.final_path,
            programming::TOTAL_SIZE,
            2,
        )?;
        std::fs::remove_file(&self.temporary_path)?;
        self.target.directory.sync_all()?;
        self.target.ensure_unchanged()?;
        validate_same_regular_file(
            &self.file,
            &self.target.final_path,
            programming::TOTAL_SIZE,
            1,
        )?;

        let mut published_file = File::open(&self.target.final_path)?;
        validate_same_regular_file(
            &self.file,
            &self.target.final_path,
            programming::TOTAL_SIZE,
            1,
        )?;
        let mut published_readback = Vec::with_capacity(stored_len);
        let published_readback_len = published_file.read_to_end(&mut published_readback)?;
        if published_readback_len != stored_len {
            return Err(invalid_input(
                "the final-name backup readback ended at an unexpected length",
            ));
        }
        let published_sha256 = sha256_bytes(&published_readback)?;
        if published_sha256 != source_sha256 || published_readback != image {
            return Err(invalid_input(
                "the atomically published backup failed final-name readback verification",
            ));
        }

        self.committed = true;
        Ok(PublishedOutput {
            path: self.target.final_path.clone(),
            sha256: source_sha256,
        })
    }
}

impl Drop for StagedOutput {
    fn drop(&mut self) {
        remove_if_same_file(&self.temporary_path, &self.file);
        if self.final_link_created && !self.committed {
            remove_if_same_file(&self.target.final_path, &self.file);
        }
        drop(self.target.directory.sync_all());
    }
}

#[tokio::main]
#[expect(
    clippy::too_many_lines,
    reason = "The fail-closed backup sequence stays linear so each close, recovery, postflight, \
              and publication gate remains visibly ordered"
)]
async fn main() -> BackupResult<()> {
    let config = parse_args()?;
    validate_usb_port(&config.port)?;
    let staged_output = OutputTarget::prepare(&config.output)?.stage()?;
    if config.machine_checked_read_only {
        println!(
            "Manual UI state was not attested. Continuing only with exact identity and the \
             machine-readable CAT safety subset."
        );
        println!(
            "Machine-only policy requires packet/TNC Off and a documented beacon method; it does \
             not claim the beacon method is Manual."
        );
        println!("{CAT_SAFETY_SCOPE}");
    } else {
        confirm_ui_checked(&config.port)?;
    }

    let mut transport = SerialTransport::open(&config.port, CAT_BAUD)?;
    let mut codec = Codec::new();
    let preflight_result = run_preflight(&mut transport, &mut codec).await;
    let (identity, cat_safety) = match preflight_result {
        Ok(preflight) => preflight,
        Err(error) => {
            return Err(with_close_result(
                error,
                close_transport(&mut transport).await,
            ));
        }
    };

    if let Err(error) = validate_expected_identity(&identity)
        .and_then(|()| validate_cat_safety_subset(&cat_safety, config.machine_checked_read_only))
    {
        return Err(with_close_result(
            error,
            close_transport(&mut transport).await,
        ));
    }

    if let Err(error) = confirm_backup(&config) {
        return Err(with_close_result(
            error,
            close_transport(&mut transport).await,
        ));
    }

    let confirmation_preflight = run_preflight(&mut transport, &mut codec).await;
    let (confirmed_identity, confirmed_cat_safety) = match confirmation_preflight {
        Ok(preflight) => preflight,
        Err(error) => {
            return Err(with_close_result(
                error,
                close_transport(&mut transport).await,
            ));
        }
    };
    if let Err(error) = validate_expected_identity(&confirmed_identity).and_then(|()| {
        validate_cat_safety_subset(&confirmed_cat_safety, config.machine_checked_read_only)
    }) {
        return Err(with_close_result(
            error,
            close_transport(&mut transport).await,
        ));
    }
    if confirmed_identity != identity || confirmed_cat_safety != cat_safety {
        return Err(with_close_result(
            invalid_input(
                "raw identity or CAT safety state changed during confirmation; no MCP command was \
                 sent",
            ),
            close_transport(&mut transport).await,
        ));
    }

    let mut radio = Radio::connect(transport).await?;
    radio.set_mcp_speed(McpSpeed::Safe);
    let mut termination = TerminationListener::install()?;

    println!(
        "Reading all {} bytes at 9600 baud...",
        programming::TOTAL_SIZE
    );
    let image_result = read_image_with_interrupt_recovery(&mut radio, &mut termination).await;
    let image = match image_result {
        Ok(image) => image,
        Err(error) => {
            let cleanup_unproved = error
                .downcast_ref::<RadioError>()
                .is_some_and(mcp_cleanup_unproved);
            let close_result = close_radio(&mut radio).await;
            let error = with_close_result(error, close_result);
            if cleanup_unproved {
                return Err(invalid_input(format!(
                    "{error}; MCP cleanup was not proved, so fully power-cycle the radio before \
                     sending any more commands"
                )));
            }
            return Err(error);
        }
    };

    if image.len() != programming::TOTAL_SIZE {
        let error = invalid_input(format!(
            "radio returned {} bytes; expected {}",
            image.len(),
            programming::TOTAL_SIZE
        ));
        return Err(with_close_result(error, close_radio(&mut radio).await));
    }

    let postflight: BackupResult<PublishedOutput> = tokio::select! {
        biased;
        signal = termination.recv() => {
            signal?;
            Err(invalid_input(
                "termination signal received after MCP cleanup; backup was not published",
            ))
        }
        result = async {
            disconnect_radio(radio).await?;

            validate_usb_port(&config.port)?;
            let mut post_transport = SerialTransport::open(&config.port, CAT_BAUD)?;
            let mut post_codec = Codec::new();
            let post_result = run_preflight(&mut post_transport, &mut post_codec).await;
            let post_close = close_transport(&mut post_transport).await;
            let (post_identity, post_cat_safety) = match post_result {
                Ok(preflight) => {
                    post_close?;
                    preflight
                }
                Err(error) => return Err(with_close_result(error, post_close)),
            };

            validate_expected_identity(&post_identity)?;
            validate_cat_safety_subset(&post_cat_safety, config.machine_checked_read_only)?;
            if post_identity != identity || post_cat_safety != cat_safety {
                return Err(invalid_input(
                    "post-operation raw identity or CAT safety bytes differ from preflight; backup \
                     bytes were not written",
                ));
            }

            staged_output.publish(&image)
        } => result,
    };

    let published = postflight?;
    println!(
        "Saved an exact {}-byte MCP image to {} (SHA-256 {}).",
        image.len(),
        published.path.display(),
        published.sha256
    );
    println!("Independent post-operation identity and CAT safety bytes matched the preflight.");
    Ok(())
}

fn parse_args() -> BackupResult<Config> {
    parse_args_from(std::env::args().skip(1))
}

fn parse_args_from<I>(args: I) -> BackupResult<Config>
where
    I: IntoIterator<Item = String>,
{
    let mut port = None;
    let mut output = None;
    let mut machine_checked_read_only = false;
    let mut args = args.into_iter();
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--machine-checked-read-only" if !machine_checked_read_only => {
                machine_checked_read_only = true;
            }
            "--port" if port.is_none() => {
                port = Some(
                    args.next()
                        .ok_or_else(|| invalid_input(format!("missing value for {flag}")))?,
                );
            }
            "--output" if output.is_none() => {
                output =
                    Some(PathBuf::from(args.next().ok_or_else(|| {
                        invalid_input(format!("missing value for {flag}"))
                    })?));
            }
            "--machine-checked-read-only" | "--port" | "--output" => {
                return Err(invalid_input(format!("duplicate argument: {flag}")));
            }
            _ => return Err(invalid_input(format!("unknown argument: {flag}"))),
        }
    }

    let port = port.ok_or_else(|| invalid_input(usage()))?;
    let output = output.ok_or_else(|| invalid_input(usage()))?;
    Ok(Config {
        port,
        output,
        machine_checked_read_only,
    })
}

const fn usage() -> &'static str {
    "usage: config_backup --port /dev/cu.usbmodemNNN --output /absolute/private/backup.bin \
     [--machine-checked-read-only]"
}

fn validate_usb_port(port: &str) -> BackupResult<()> {
    if !Path::new(port).is_absolute() || SerialTransport::is_bluetooth_port(port) {
        return Err(invalid_input(
            "the port must be an absolute USB CDC path, not Bluetooth",
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

fn output_parent(path: &Path) -> BackupResult<&Path> {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| invalid_input("the backup output path needs a parent directory"))
}

fn ensure_output_absent(path: &Path) -> BackupResult<()> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Ok(_) => Err(invalid_input(format!(
            "the final backup path already exists and will not be overwritten: {}",
            path.display()
        ))),
        Err(error) => Err(Box::new(error)),
    }
}

fn reject_symlinked_ancestors(path: &Path) -> BackupResult<()> {
    for ancestor in path.ancestors() {
        let metadata = std::fs::symlink_metadata(ancestor)?;
        if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() {
            return Err(invalid_input(format!(
                "every backup parent ancestor must be a real directory, not a symlink: {}",
                ancestor.display()
            )));
        }
    }
    Ok(())
}

fn validate_git_privacy(directory: &Path) -> BackupResult<()> {
    let discovery = Command::new("git")
        .arg("-C")
        .arg(directory)
        .arg("rev-parse")
        .arg("--show-toplevel")
        .output()?;
    if !discovery.status.success() {
        return Ok(());
    }
    let worktree_text = std::str::from_utf8(&discovery.stdout)?.trim();
    let worktree = Path::new(worktree_text).canonicalize()?;
    if !directory.starts_with(&worktree) {
        return Err(invalid_input(
            "git reported a worktree that does not contain the backup directory",
        ));
    }
    let ignored = Command::new("git")
        .arg("-C")
        .arg(&worktree)
        .arg("check-ignore")
        .arg("--quiet")
        .arg("--")
        .arg(directory)
        .status()?;
    if ignored.success() {
        Ok(())
    } else {
        Err(invalid_input(
            "an in-worktree backup directory must be covered by .gitignore",
        ))
    }
}

#[cfg(unix)]
fn ensure_private_parent(parent: &Path) -> BackupResult<()> {
    let metadata = std::fs::symlink_metadata(parent)?;
    validate_private_parent_metadata(parent, &metadata)
}

#[cfg(unix)]
fn validate_private_parent_metadata(
    parent: &Path,
    metadata: &std::fs::Metadata,
) -> BackupResult<()> {
    if !metadata.file_type().is_dir()
        || metadata.file_type().is_symlink()
        || metadata.mode() & 0o777 != 0o700
    {
        return Err(invalid_input(format!(
            "backup parent must be a real directory with mode 0700: {}",
            parent.display()
        )));
    }
    Ok(())
}

#[cfg(not(unix))]
fn ensure_private_parent(_parent: &Path) -> BackupResult<()> {
    Err(invalid_input(
        "this guarded backup requires Unix file modes for a 0700 parent and 0600 output",
    ))
}

#[cfg(not(unix))]
fn validate_private_parent_metadata(
    _parent: &Path,
    _metadata: &std::fs::Metadata,
) -> BackupResult<()> {
    Err(invalid_input(
        "this guarded backup requires Unix directory identity and mode checks",
    ))
}

#[cfg(unix)]
fn same_file(left: &std::fs::Metadata, right: &std::fs::Metadata) -> bool {
    left.dev() == right.dev() && left.ino() == right.ino()
}

#[cfg(not(unix))]
const fn same_file(_left: &std::fs::Metadata, _right: &std::fs::Metadata) -> bool {
    false
}

fn validate_same_regular_file(
    open_file: &File,
    path: &Path,
    expected_len: usize,
    expected_links: u64,
) -> BackupResult<()> {
    #[cfg(not(unix))]
    let _ = expected_links;
    let path_metadata = std::fs::symlink_metadata(path)?;
    let handle_metadata = open_file.metadata()?;
    let stored_len = usize::try_from(path_metadata.len())
        .map_err(|_| invalid_input("published backup length does not fit usize"))?;
    if path_metadata.file_type().is_symlink()
        || !path_metadata.file_type().is_file()
        || !same_file(&path_metadata, &handle_metadata)
        || stored_len != expected_len
    {
        return Err(invalid_input(format!(
            "published backup path does not name the verified {expected_len}-byte staging file"
        )));
    }
    #[cfg(unix)]
    if path_metadata.mode() & 0o777 != 0o600 || path_metadata.nlink() != expected_links {
        return Err(invalid_input(format!(
            "published backup must remain mode 0600 with exactly {expected_links} link(s)"
        )));
    }
    Ok(())
}

fn remove_if_same_file(path: &Path, open_file: &File) {
    let path_metadata = std::fs::symlink_metadata(path);
    let handle_metadata = open_file.metadata();
    if let (Ok(path_metadata), Ok(handle_metadata)) = (path_metadata, handle_metadata)
        && path_metadata.file_type().is_file()
        && same_file(&path_metadata, &handle_metadata)
    {
        drop(std::fs::remove_file(path));
    }
}

fn sha256_bytes(bytes: &[u8]) -> BackupResult<String> {
    let byte_length = u64::try_from(bytes.len())
        .map_err(|_| invalid_input("backup is too large for SHA-256 length encoding"))?;
    let bit_length = byte_length
        .checked_mul(8)
        .ok_or_else(|| invalid_input("backup bit length overflowed SHA-256 encoding"))?;
    let mut padded = Vec::with_capacity(bytes.len().saturating_add(128));
    padded.extend_from_slice(bytes);
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_length.to_be_bytes());

    let mut state = [
        0x6a09_e667,
        0xbb67_ae85,
        0x3c6e_f372,
        0xa54f_f53a,
        0x510e_527f,
        0x9b05_688c,
        0x1f83_d9ab,
        0x5be0_cd19,
    ];
    for block in padded.chunks_exact(64) {
        sha256_compress(&mut state, block)?;
    }

    let mut digest = String::with_capacity(64);
    for word in state {
        std::fmt::write(&mut digest, format_args!("{word:08x}"))?;
    }
    Ok(digest)
}

fn sha256_compress(state: &mut [u32; 8], block: &[u8]) -> BackupResult<()> {
    if block.len() != 64 {
        return Err(invalid_input(
            "internal SHA-256 compression block has the wrong length",
        ));
    }

    let mut schedule = [0_u32; 64];
    let words = block.chunks_exact(4);
    if !words.remainder().is_empty() {
        return Err(invalid_input(
            "internal SHA-256 word split left trailing bytes",
        ));
    }
    for (slot, word) in schedule.iter_mut().take(16).zip(words) {
        let [a, b, c, d] = word else {
            return Err(invalid_input("internal SHA-256 word has the wrong length"));
        };
        *slot = u32::from_be_bytes([*a, *b, *c, *d]);
    }

    for index in 16..64 {
        let word_15 = sha256_schedule_word(&schedule, index - 15)?;
        let word_2 = sha256_schedule_word(&schedule, index - 2)?;
        let small_sigma_0 = word_15.rotate_right(7) ^ word_15.rotate_right(18) ^ (word_15 >> 3);
        let small_sigma_1 = word_2.rotate_right(17) ^ word_2.rotate_right(19) ^ (word_2 >> 10);
        let extended = sha256_schedule_word(&schedule, index - 16)?
            .wrapping_add(small_sigma_0)
            .wrapping_add(sha256_schedule_word(&schedule, index - 7)?)
            .wrapping_add(small_sigma_1);
        let slot = schedule
            .get_mut(index)
            .ok_or_else(|| invalid_input("internal SHA-256 schedule index is invalid"))?;
        *slot = extended;
    }

    let [
        mut working_a,
        mut working_b,
        mut working_c,
        mut working_d,
        mut working_e,
        mut working_f,
        mut working_g,
        mut working_h,
    ] = *state;
    for (round_constant, schedule_word) in SHA256_ROUND_CONSTANTS.iter().zip(schedule) {
        let big_sigma_1 =
            working_e.rotate_right(6) ^ working_e.rotate_right(11) ^ working_e.rotate_right(25);
        let choice = (working_e & working_f) ^ ((!working_e) & working_g);
        let temporary_1 = working_h
            .wrapping_add(big_sigma_1)
            .wrapping_add(choice)
            .wrapping_add(*round_constant)
            .wrapping_add(schedule_word);
        let big_sigma_0 =
            working_a.rotate_right(2) ^ working_a.rotate_right(13) ^ working_a.rotate_right(22);
        let majority = (working_a & working_b) ^ (working_a & working_c) ^ (working_b & working_c);
        let temporary_2 = big_sigma_0.wrapping_add(majority);

        working_h = working_g;
        working_g = working_f;
        working_f = working_e;
        working_e = working_d.wrapping_add(temporary_1);
        working_d = working_c;
        working_c = working_b;
        working_b = working_a;
        working_a = temporary_1.wrapping_add(temporary_2);
    }

    let [
        state_a,
        state_b,
        state_c,
        state_d,
        state_e,
        state_f,
        state_g,
        state_h,
    ] = *state;
    *state = [
        state_a.wrapping_add(working_a),
        state_b.wrapping_add(working_b),
        state_c.wrapping_add(working_c),
        state_d.wrapping_add(working_d),
        state_e.wrapping_add(working_e),
        state_f.wrapping_add(working_f),
        state_g.wrapping_add(working_g),
        state_h.wrapping_add(working_h),
    ];
    Ok(())
}

fn sha256_schedule_word(schedule: &[u32; 64], index: usize) -> BackupResult<u32> {
    schedule
        .get(index)
        .copied()
        .ok_or_else(|| invalid_input("internal SHA-256 schedule read is out of range"))
}

fn confirm_ui_checked(port: &str) -> BackupResult<()> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(invalid_input(
            "the radio UI checklist attestation requires terminal stdin and stdout",
        ));
    }
    let expected = format!("RADIO UI CHECKED {port}");
    println!("Before the port is opened, attest that all of these are currently true:");
    for assertion in OPERATOR_ASSERTIONS {
        println!("  - {assertion}");
    }
    println!("{CAT_SAFETY_SCOPE}");
    println!("The checklist remains an operator attestation, not independent verification.");
    println!("Type this exact phrase, then press Enter:");
    println!("{expected}");
    print!("> ");
    io::stdout().flush()?;

    let mut entered = String::new();
    if io::stdin().read_line(&mut entered)? == 0 {
        return Err(invalid_input(
            "UI attestation input ended before a phrase was read",
        ));
    }
    if strip_line_ending(&entered) != expected {
        return Err(invalid_input(
            "UI checklist attestation did not match; no port was opened",
        ));
    }
    Ok(())
}

fn confirmation_phrase(config: &Config) -> String {
    format!(
        "BACK UP TH-D75 ON {} TO {}",
        config.port,
        config.output.display()
    )
}

fn confirm_backup(config: &Config) -> BackupResult<()> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(invalid_input(
            "interactive confirmation requires terminal stdin and stdout",
        ));
    }

    let expected = confirmation_phrase(config);
    println!("The five independently read CAT safety settings passed.");
    println!("{CAT_SAFETY_SCOPE}");
    println!("This will enter read-only MCP programming mode.");
    println!("Type this exact phrase, then press Enter:");
    println!("{expected}");
    print!("> ");
    io::stdout().flush()?;

    let mut entered = String::new();
    if io::stdin().read_line(&mut entered)? == 0 {
        return Err(invalid_input(
            "confirmation input ended before a phrase was read",
        ));
    }
    let entered = strip_line_ending(&entered);
    if entered != expected {
        return Err(invalid_input(
            "confirmation did not match exactly; no MCP command was sent",
        ));
    }
    Ok(())
}

fn strip_line_ending(value: &str) -> &str {
    let value = value.strip_suffix('\n').unwrap_or(value);
    value.strip_suffix('\r').unwrap_or(value)
}

async fn run_preflight<T: Transport>(
    transport: &mut T,
    codec: &mut Codec,
) -> BackupResult<(RawIdentity, CatSafetySubset)> {
    println!("Reading raw CAT identity and the limited CAT safety subset...");
    let identity = read_raw_identity(transport, codec).await?;
    validate_expected_identity(&identity)?;
    let cat_safety = read_cat_safety_subset(transport, codec).await?;
    Ok((identity, cat_safety))
}

async fn read_raw_identity<T: Transport>(
    transport: &mut T,
    codec: &mut Codec,
) -> BackupResult<RawIdentity> {
    let responses = query_commands(transport, codec, ID_COMMANDS).await?;
    let [id, firmware, radio_type] = responses
        .try_into()
        .map_err(|_| invalid_input("identity command count changed unexpectedly"))?;
    Ok(RawIdentity {
        id,
        firmware,
        radio_type,
    })
}

async fn read_cat_safety_subset<T: Transport>(
    transport: &mut T,
    codec: &mut Codec,
) -> BackupResult<CatSafetySubset> {
    let responses = query_commands(transport, codec, CAT_SAFETY_COMMANDS).await?;
    let [auto_info, tnc, beacon, vox, io_port] = responses
        .try_into()
        .map_err(|_| invalid_input("CAT safety command count changed unexpectedly"))?;
    Ok(CatSafetySubset {
        auto_info,
        tnc,
        beacon,
        vox,
        io_port,
    })
}

async fn query_commands<T: Transport>(
    transport: &mut T,
    codec: &mut Codec,
    commands: &[&[u8]],
) -> BackupResult<Vec<Vec<u8>>> {
    let mut responses = Vec::with_capacity(commands.len());
    for command in commands {
        responses.push(query_raw(transport, codec, command).await?);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    Ok(responses)
}

async fn query_raw<T: Transport>(
    transport: &mut T,
    codec: &mut Codec,
    command: &[u8],
) -> BackupResult<Vec<u8>> {
    drain_stale(transport, codec).await?;
    let mnemonic = command
        .get(..2)
        .ok_or_else(|| invalid_input("raw CAT command is shorter than its mnemonic"))?;
    if command.get(2) != Some(&b'\r') {
        return Err(invalid_input("raw CAT preflight accepts only bare reads"));
    }

    tokio::time::timeout(RESPONSE_TIMEOUT, transport.write(command))
        .await
        .map_err(|_| invalid_input("raw CAT write timed out"))??;

    tokio::time::timeout(RESPONSE_TIMEOUT, async {
        let mut buffer = [0_u8; 4096];
        let mut unsolicited_bytes = 0_usize;
        loop {
            let count = transport.read(&mut buffer).await?;
            if count == 0 {
                return Err(invalid_input(
                    "radio disconnected while awaiting raw CAT response",
                ));
            }
            let chunk = buffer
                .get(..count)
                .ok_or_else(|| invalid_input("transport returned an invalid byte count"))?;
            codec.feed(chunk);
            while let Some(frame) = codec.next_frame() {
                if frame == b"?" || frame == b"N" {
                    return Err(invalid_input(format!(
                        "raw CAT {} query was rejected with {}",
                        String::from_utf8_lossy(mnemonic),
                        String::from_utf8_lossy(&frame)
                    )));
                }
                if mnemonic_matches(&frame, mnemonic) {
                    return Ok(frame);
                }
                unsolicited_bytes = unsolicited_bytes.saturating_add(frame.len() + 1);
                if unsolicited_bytes > MAX_UNSOLICITED_BYTES {
                    return Err(invalid_input(
                        "too much unsolicited CAT/NMEA data while awaiting a response",
                    ));
                }
            }
        }
    })
    .await
    .map_err(|_| invalid_input("raw CAT response timed out"))?
}

async fn drain_stale<T: Transport>(transport: &mut T, codec: &mut Codec) -> BackupResult<()> {
    let mut buffer = [0_u8; 4096];
    let started = Instant::now();
    let mut total = 0_usize;
    loop {
        let Some(remaining) = DRAIN_TOTAL_TIMEOUT.checked_sub(started.elapsed()) else {
            codec.clear();
            return Err(invalid_input(
                "CAT input did not become quiet before the raw preflight query",
            ));
        };
        let wait = DRAIN_QUIET_WINDOW.min(remaining);
        match tokio::time::timeout(wait, transport.read(&mut buffer)).await {
            Ok(Ok(0)) => break,
            Ok(Ok(count)) => {
                let chunk = buffer
                    .get(..count)
                    .ok_or_else(|| invalid_input("transport returned an invalid byte count"))?;
                total = total.saturating_add(chunk.len());
                if total > MAX_UNSOLICITED_BYTES {
                    codec.clear();
                    return Err(invalid_input(
                        "too much stale CAT/NMEA data before the raw preflight query",
                    ));
                }
                codec.feed(chunk);
                while codec.next_frame().is_some() {}
            }
            Ok(Err(error)) => return Err(Box::new(error)),
            Err(_) if wait == DRAIN_QUIET_WINDOW => break,
            Err(_) => {
                codec.clear();
                return Err(invalid_input(
                    "CAT input did not become quiet before the raw preflight query",
                ));
            }
        }
    }
    codec.clear();
    Ok(())
}

fn mnemonic_matches(frame: &[u8], mnemonic: &[u8]) -> bool {
    frame.get(..2) == Some(mnemonic)
        && (frame.len() == 2 || frame.get(2).is_some_and(|byte| *byte == b' '))
}

fn validate_expected_identity(identity: &RawIdentity) -> BackupResult<()> {
    let firmware_supported = matches!(identity.firmware.as_slice(), b"FV 1.03" | b"FV 1.03.AZM");
    if identity.id == b"ID TH-D75" && firmware_supported && identity.radio_type == b"TY K,2" {
        Ok(())
    } else {
        Err(invalid_input(format!(
            "unsupported raw CAT identity: ID={}, FV={}, TY={}",
            String::from_utf8_lossy(&identity.id),
            String::from_utf8_lossy(&identity.firmware),
            String::from_utf8_lossy(&identity.radio_type)
        )))
    }
}

fn validate_cat_safety_subset(
    cat_safety: &CatSafetySubset,
    machine_checked_read_only: bool,
) -> BackupResult<()> {
    let tnc_off = cat_safety.tnc == b"TN 0,0" || cat_safety.tnc == b"TN 0,1";
    let beacon_method_known = matches!(
        cat_safety.beacon.as_slice(),
        b"PT 0" | b"PT 1" | b"PT 2" | b"PT 3"
    );
    let beacon_safe = if machine_checked_read_only {
        beacon_method_known
    } else {
        cat_safety.beacon == b"PT 0"
    };
    if cat_safety.auto_info == b"AI 0"
        && tnc_off
        && beacon_safe
        && cat_safety.vox == b"VX 0"
        && cat_safety.io_port == b"IO 0"
    {
        Ok(())
    } else {
        Err(invalid_input(format!(
            "the limited CAT safety subset was rejected: AI={}, TN={}, PT={}, VX={}, IO={}; no \
             MCP command was sent",
            String::from_utf8_lossy(&cat_safety.auto_info),
            String::from_utf8_lossy(&cat_safety.tnc),
            String::from_utf8_lossy(&cat_safety.beacon),
            String::from_utf8_lossy(&cat_safety.vox),
            String::from_utf8_lossy(&cat_safety.io_port)
        )))
    }
}

fn page_progress(page: u16, total: u16) -> (u32, u32, u32) {
    let completed = u32::from(page) + 1;
    let total = u32::from(total);
    let percent = completed.saturating_mul(100) / total.max(1);
    (completed, total, percent)
}

async fn read_image_with_interrupt_recovery<T: Transport>(
    radio: &mut Radio<T>,
    termination: &mut TerminationListener,
) -> BackupResult<Vec<u8>> {
    let interrupt_result = {
        let read = radio.read_memory_image_with_progress(|page, total| {
            if page % 100 == 0 || page == total.saturating_sub(1) {
                let (completed, total, percent) = page_progress(page, total);
                eprint!("\r  Page {completed}/{total} ({percent}%)");
            }
        });
        tokio::pin!(read);
        tokio::select! {
            biased;
            signal = termination.recv() => signal,
            result = &mut read => {
                eprintln!();
                return result.map_err(Into::into);
            }
        }
    };
    eprintln!();

    let interruption = interrupt_result.map_or_else(
        |error| {
            format!("termination listener failed ({error}); the in-progress MCP read was cancelled")
        },
        |()| "MCP read interrupted by a termination signal".to_owned(),
    );

    match tokio::time::timeout(RECOVERY_TIMEOUT, radio.recover_from_interrupted_mcp()).await {
        Ok(Ok(())) => Err(invalid_input(format!(
            "{interruption}; MCP exit and CAT recovery completed, but no backup was written"
        ))),
        Ok(Err(recovery_error)) if mcp_cleanup_unproved(&recovery_error) => {
            Err(Box::new(recovery_error))
        }
        Ok(Err(recovery_error)) => Err(invalid_input(format!(
            "{interruption}; recovery returned an error after CAT restoration: {recovery_error}; \
             no backup was written"
        ))),
        Err(_) => Err(invalid_input(format!(
            "{interruption}; MCP recovery timed out and cleanup was not proved; fully power-cycle \
             the radio before sending any more commands"
        ))),
    }
}

#[cfg(unix)]
#[derive(Debug)]
struct TerminationListener {
    interrupt: tokio::signal::unix::Signal,
    terminate: tokio::signal::unix::Signal,
    hangup: tokio::signal::unix::Signal,
}

#[cfg(unix)]
impl TerminationListener {
    fn install() -> io::Result<Self> {
        use tokio::signal::unix::{SignalKind, signal};

        Ok(Self {
            interrupt: signal(SignalKind::interrupt())?,
            terminate: signal(SignalKind::terminate())?,
            hangup: signal(SignalKind::hangup())?,
        })
    }

    async fn recv(&mut self) -> io::Result<()> {
        tokio::select! {
            _ = self.interrupt.recv() => Ok(()),
            _ = self.terminate.recv() => Ok(()),
            _ = self.hangup.recv() => Ok(()),
        }
    }
}

#[cfg(windows)]
#[derive(Debug)]
struct TerminationListener {
    interrupt: tokio::signal::windows::CtrlC,
    break_signal: tokio::signal::windows::CtrlBreak,
    close: tokio::signal::windows::CtrlClose,
    logoff: tokio::signal::windows::CtrlLogoff,
    shutdown: tokio::signal::windows::CtrlShutdown,
}

#[cfg(windows)]
impl TerminationListener {
    fn install() -> io::Result<Self> {
        use tokio::signal::windows;

        Ok(Self {
            interrupt: windows::ctrl_c()?,
            break_signal: windows::ctrl_break()?,
            close: windows::ctrl_close()?,
            logoff: windows::ctrl_logoff()?,
            shutdown: windows::ctrl_shutdown()?,
        })
    }

    async fn recv(&mut self) -> io::Result<()> {
        tokio::select! {
            _ = self.interrupt.recv() => Ok(()),
            _ = self.break_signal.recv() => Ok(()),
            _ = self.close.recv() => Ok(()),
            _ = self.logoff.recv() => Ok(()),
            _ = self.shutdown.recv() => Ok(()),
        }
    }
}

#[cfg(not(any(unix, windows)))]
#[derive(Debug)]
struct TerminationListener;

#[cfg(not(any(unix, windows)))]
impl TerminationListener {
    const fn install() -> io::Result<Self> {
        Ok(Self)
    }

    async fn recv(&mut self) -> io::Result<()> {
        tokio::signal::ctrl_c().await
    }
}

fn mcp_cleanup_unproved(error: &RadioError) -> bool {
    match error {
        RadioError::McpCleanupNotProved { .. } => true,
        RadioError::McpOperationAndCleanupFailed { cleanup, .. } => mcp_cleanup_unproved(cleanup),
        _ => false,
    }
}

async fn close_transport<T: Transport>(transport: &mut T) -> BackupResult<()> {
    tokio::time::timeout(CLOSE_TIMEOUT, transport.close())
        .await
        .map_err(|_| invalid_input("transport close timed out"))??;
    Ok(())
}

async fn close_radio<T: Transport>(radio: &mut Radio<T>) -> BackupResult<()> {
    tokio::time::timeout(CLOSE_TIMEOUT, radio.close_transport())
        .await
        .map_err(|_| invalid_input("radio transport close timed out"))??;
    Ok(())
}

async fn disconnect_radio<T: Transport>(radio: Radio<T>) -> BackupResult<()> {
    tokio::time::timeout(CLOSE_TIMEOUT, radio.disconnect())
        .await
        .map_err(|_| invalid_input("radio disconnect timed out"))??;
    Ok(())
}

fn with_close_result(primary: BackupError, close: BackupResult<()>) -> BackupError {
    match close {
        Ok(()) => primary,
        Err(close_error) => invalid_input(format!(
            "{primary}; transport cleanup also failed: {close_error}"
        )),
    }
}

fn invalid_input(message: impl Into<String>) -> BackupError {
    Box::new(io::Error::new(io::ErrorKind::InvalidInput, message.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = BackupResult<()>;

    #[cfg(unix)]
    #[derive(Debug)]
    struct PrivateTestDirectory(PathBuf);

    #[cfg(unix)]
    impl PrivateTestDirectory {
        fn create(label: &str) -> BackupResult<Self> {
            use std::os::unix::fs::DirBuilderExt;
            use std::time::{SystemTime, UNIX_EPOCH};

            let unique = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
            let path = std::env::temp_dir().canonicalize()?.join(format!(
                "kenwood-config-backup-{label}-{}-{unique}",
                std::process::id()
            ));
            let mut builder = std::fs::DirBuilder::new();
            let configured = builder.mode(0o700);
            configured.create(&path)?;
            Ok(Self(path))
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    #[cfg(unix)]
    impl Drop for PrivateTestDirectory {
        fn drop(&mut self) {
            drop(std::fs::remove_dir_all(&self.0));
        }
    }

    #[test]
    fn arguments_require_explicit_port_and_absolute_output() -> TestResult {
        let parsed = parse_args_from(
            [
                "--port",
                "/dev/cu.usbmodem101",
                "--output",
                "/private/backup.bin",
            ]
            .map(str::to_owned),
        )?;
        assert_eq!(
            parsed,
            Config {
                port: "/dev/cu.usbmodem101".to_owned(),
                output: PathBuf::from("/private/backup.bin"),
                machine_checked_read_only: false,
            }
        );
        assert!(parse_args_from(Vec::<String>::new()).is_err());
        assert!(parse_args_from(["--port", "/dev/cu.usbmodem101"].map(str::to_owned)).is_err());
        Ok(())
    }

    #[test]
    fn machine_checked_read_only_mode_is_explicit_and_order_independent() -> TestResult {
        let parsed = parse_args_from(
            [
                "--machine-checked-read-only",
                "--output",
                "/private/backup.bin",
                "--port",
                "/dev/cu.usbmodem101",
            ]
            .map(str::to_owned),
        )?;
        assert!(parsed.machine_checked_read_only);
        assert!(
            parse_args_from(
                [
                    "--machine-checked-read-only",
                    "--machine-checked-read-only",
                    "--port",
                    "/dev/cu.usbmodem101",
                    "--output",
                    "/private/backup.bin",
                ]
                .map(str::to_owned),
            )
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn confirmation_is_byte_exact_apart_from_line_ending() {
        assert_eq!(strip_line_ending("phrase\n"), "phrase");
        assert_eq!(strip_line_ending("phrase\r\n"), "phrase");
        assert_eq!(strip_line_ending("phrase "), "phrase ");
        assert_eq!(strip_line_ending("phrase\n\n"), "phrase\n");
    }

    #[test]
    fn full_image_progress_uses_a_wide_intermediate() {
        assert_eq!(page_progress(0, 1_955), (1, 1_955, 0));
        assert_eq!(page_progress(700, 1_955), (701, 1_955, 35));
        assert_eq!(page_progress(1_954, 1_955), (1_955, 1_955, 100));
    }

    #[test]
    fn identity_and_limited_cat_safety_subset_are_exact() {
        let identity = RawIdentity {
            id: b"ID TH-D75".to_vec(),
            firmware: b"FV 1.03".to_vec(),
            radio_type: b"TY K,2".to_vec(),
        };
        assert!(validate_expected_identity(&identity).is_ok());

        let azimuth_firmware = RawIdentity {
            id: b"ID TH-D75".to_vec(),
            firmware: b"FV 1.03.AZM".to_vec(),
            radio_type: b"TY K,2".to_vec(),
        };
        assert!(validate_expected_identity(&azimuth_firmware).is_ok());

        let extended_firmware = RawIdentity {
            firmware: b"FV 1.03.000".to_vec(),
            ..identity
        };
        assert!(validate_expected_identity(&extended_firmware).is_err());

        let cat_safety = CatSafetySubset {
            auto_info: b"AI 0".to_vec(),
            tnc: b"TN 0,0".to_vec(),
            beacon: b"PT 0".to_vec(),
            vox: b"VX 0".to_vec(),
            io_port: b"IO 0".to_vec(),
        };
        assert!(validate_cat_safety_subset(&cat_safety, false).is_ok());

        let automatic_beacon_with_tnc_off = CatSafetySubset {
            beacon: b"PT 2".to_vec(),
            ..cat_safety.clone()
        };
        assert!(validate_cat_safety_subset(&automatic_beacon_with_tnc_off, true).is_ok());
        assert!(validate_cat_safety_subset(&automatic_beacon_with_tnc_off, false).is_err());

        let rejected_cat_safety = CatSafetySubset {
            tnc: b"TN 2,0".to_vec(),
            ..cat_safety
        };
        assert!(validate_cat_safety_subset(&rejected_cat_safety, true).is_err());
    }

    #[test]
    fn mnemonic_matching_requires_a_token_boundary() {
        assert!(mnemonic_matches(b"ID TH-D75", b"ID"));
        assert!(mnemonic_matches(b"ID", b"ID"));
        assert!(!mnemonic_matches(b"IDENTITY", b"ID"));
        assert!(!mnemonic_matches(b"FV 1.03", b"ID"));
    }

    #[test]
    fn sha256_matches_standard_vectors() -> TestResult {
        assert_eq!(
            sha256_bytes(b"")?,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_bytes(b"abc")?,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_bytes(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq")?,
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn staging_does_not_create_the_final_name_and_cleans_up() -> TestResult {
        let directory = PrivateTestDirectory::create("stage")?;
        let final_path = directory.path().join("backup.bin");
        let staged = OutputTarget::prepare(&final_path)?.stage()?;
        let temporary_path = staged.temporary_path.clone();

        assert!(
            !final_path.try_exists()?,
            "staging must not reserve the final output name"
        );
        assert!(
            temporary_path.try_exists()?,
            "the private staging file must exist while staged"
        );
        drop(staged);
        assert!(
            !temporary_path.try_exists()?,
            "dropping an unpublished stage must remove its temporary file"
        );
        assert!(
            !final_path.try_exists()?,
            "dropping an unpublished stage must leave no final output"
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn publication_is_exact_durable_and_leaves_one_name() -> TestResult {
        let directory = PrivateTestDirectory::create("publish")?;
        let final_path = directory.path().join("backup.bin");
        let image = vec![0xa5; programming::TOTAL_SIZE];
        let published = OutputTarget::prepare(&final_path)?
            .stage()?
            .publish(&image)?;

        assert_eq!(published.path, final_path);
        assert_eq!(published.sha256.len(), 64);
        assert_eq!(std::fs::read(&published.path)?, image);
        let entries = std::fs::read_dir(directory.path())?.collect::<Result<Vec<_>, _>>()?;
        assert_eq!(
            entries.len(),
            1,
            "successful publication must remove the staging name"
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn publication_never_clobbers_a_name_created_after_staging() -> TestResult {
        let directory = PrivateTestDirectory::create("no-clobber")?;
        let final_path = directory.path().join("backup.bin");
        let staged = OutputTarget::prepare(&final_path)?.stage()?;
        let temporary_path = staged.temporary_path.clone();
        let sentinel = b"belongs to another creator";
        let mut options = OpenOptions::new();
        let mut existing = options.write(true).create_new(true).open(&final_path)?;
        existing.write_all(sentinel)?;
        existing.sync_all()?;
        drop(existing);

        let image = vec![0x5a; programming::TOTAL_SIZE];
        assert!(
            staged.publish(&image).is_err(),
            "publication must fail when the final name appears after staging"
        );
        assert_eq!(std::fs::read(&final_path)?, sentinel);
        assert!(
            !temporary_path.try_exists()?,
            "failed no-clobber publication must remove only its own staging file"
        );
        Ok(())
    }
}
