//! Fail-closed control-and-screen evidence probe for a TH-D75 over macOS
//! Bluetooth.
//!
//! The default run proves the exact firmware/runtime ABI and captures one
//! CRC-authenticated LCD frame. `--exercise-menu` additionally performs a
//! complete `[MENU]` press/release, captures the resulting screen, presses
//! `[MENU]` again to restore the prior UI, and captures that result. Optional
//! exact OCR assertions turn the screen transition into a semantic test.
//! `--menu-navigation-key HH` inserts one bounded key tap while the menu is
//! open and captures that screen as well.
//!
//! Usage:
//! ```text
//! cargo run -p kenwood-thd75 --release --example automation_probe -- \
//!   [--device TH-D75] [--output-dir PATH] [--exercise-menu] \
//!   [--expect-baseline-text TEXT] [--expect-menu-text TEXT] \
//!   [--menu-navigation-key HH] [--expect-navigation-text TEXT] \
//!   [--restore-menu-taps N]
//! ```

// Keep every workspace example dependency represented under the workspace's
// strict unused-dependency lint.
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

#[cfg(target_os = "macos")]
mod macos {
    use std::error::Error as StdError;
    use std::fs;
    use std::io;
    use std::path::{Path, PathBuf};
    use std::time::{Instant, SystemTime, UNIX_EPOCH};

    use kenwood_thd75::Radio;
    use kenwood_thd75::radio::automation::{AutomationMetadata, AutomationSnapshot, FrontPanelKey};
    use kenwood_thd75::screen::ScreenFrame;
    use kenwood_thd75::screen::vision::{NormalizedBounds, TextObservation, require_unique_text};
    use kenwood_thd75::transport::BluetoothTransport;

    type ProbeResult<T> = Result<T, Box<dyn StdError>>;

    #[derive(Debug)]
    struct Config {
        device_name: String,
        output_dir: PathBuf,
        exercise_menu: bool,
        navigation_key: Option<FrontPanelKey>,
        restore_menu_taps: u8,
        expected_baseline_text: Option<String>,
        expected_menu_text: Option<String>,
        expected_navigation_text: Option<String>,
    }

    #[derive(Debug)]
    struct Captures {
        baseline: AutomationSnapshot,
        menu: Option<AutomationSnapshot>,
        navigated: Option<AutomationSnapshot>,
        restored: Option<AutomationSnapshot>,
        menu_key: Option<AutomationMetadata>,
        navigation_key: Option<AutomationMetadata>,
        restore_key: Option<AutomationMetadata>,
        qualification_elapsed: std::time::Duration,
        baseline_capture_elapsed: std::time::Duration,
        menu_capture_elapsed: Option<std::time::Duration>,
        navigation_capture_elapsed: Option<std::time::Duration>,
        restore_capture_elapsed: Option<std::time::Duration>,
    }

    #[expect(
        clippy::too_many_lines,
        reason = "The hardware evidence sequence is intentionally linear so every key, capture, \
                  recovery, and timing step remains visible in execution order."
    )]
    pub(super) async fn run() -> ProbeResult<()> {
        let config = parse_args()?;
        let transport = BluetoothTransport::open(Some(&config.device_name))?;
        let mut radio = Radio::connect(transport).await?;

        let qualification_started = Instant::now();
        let captures = {
            let mut session = radio.qualify_automation().await?;
            let qualification_elapsed = qualification_started.elapsed();
            let abi = session.abi();
            println!(
                "abi=version:{},features:0x{:02X},max_key:0x{:02X},max_phase:{}",
                abi.version, abi.features, abi.max_key, abi.max_phase
            );

            let baseline_started = Instant::now();
            let baseline = session.capture_screen().await?;
            let baseline_capture_elapsed = baseline_started.elapsed();

            let (
                menu,
                navigated,
                restored,
                menu_key,
                navigation_key,
                restore_key,
                menu_elapsed,
                navigation_elapsed,
                restore_elapsed,
            ) = if config.exercise_menu {
                let menu_key = session.tap_key(FrontPanelKey::Menu).await?;

                let menu_started = Instant::now();
                let menu = match session.capture_screen().await {
                    Ok(snapshot) => snapshot,
                    Err(error) => {
                        if session.is_valid() {
                            match session.tap_key(FrontPanelKey::Menu).await {
                                Ok(_recovery_metadata) => {}
                                Err(restore_error) => {
                                    eprintln!(
                                        "menu recovery failed after capture error: \
                                             {restore_error}"
                                    );
                                }
                            }
                        }
                        return Err(error.into());
                    }
                };
                let menu_elapsed = menu_started.elapsed();

                let (navigated, navigation_key, navigation_elapsed) =
                    if let Some(key) = config.navigation_key {
                        let key_metadata = session.tap_key(key).await?;
                        let navigation_started = Instant::now();
                        let snapshot = match session.capture_screen().await {
                            Ok(snapshot) => snapshot,
                            Err(error) => {
                                if session.is_valid() {
                                    match session.tap_key(FrontPanelKey::Menu).await {
                                        Ok(_recovery_metadata) => {}
                                        Err(restore_error) => {
                                            eprintln!(
                                                "menu recovery failed after navigation \
                                                     capture error: {restore_error}"
                                            );
                                        }
                                    }
                                }
                                return Err(error.into());
                            }
                        };
                        (
                            Some(snapshot),
                            Some(key_metadata),
                            Some(navigation_started.elapsed()),
                        )
                    } else {
                        (None, None, None)
                    };

                // Restore the front panel before doing OCR or filesystem
                // work, so host-side validation failures do not strand it
                // in the menu.
                let mut restore_key = None;
                for tap_index in 0..config.restore_menu_taps {
                    restore_key = Some(session.tap_key(FrontPanelKey::Menu).await?);
                    if tap_index + 1 < config.restore_menu_taps {
                        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                    }
                }
                let restore_key = restore_key
                    .ok_or_else(|| io::Error::other("restore MENU tap count cannot be zero"))?;
                let restore_started = Instant::now();
                let restored = session.capture_screen().await?;
                let restore_elapsed = restore_started.elapsed();

                (
                    Some(menu),
                    navigated,
                    Some(restored),
                    Some(menu_key),
                    navigation_key,
                    Some(restore_key),
                    Some(menu_elapsed),
                    navigation_elapsed,
                    Some(restore_elapsed),
                )
            } else {
                (None, None, None, None, None, None, None, None, None)
            };

            Captures {
                baseline,
                menu,
                navigated,
                restored,
                menu_key,
                navigation_key,
                restore_key,
                qualification_elapsed,
                baseline_capture_elapsed,
                menu_capture_elapsed: menu_elapsed,
                navigation_capture_elapsed: navigation_elapsed,
                restore_capture_elapsed: restore_elapsed,
            }
        };

        radio.disconnect().await?;
        validate_and_write(&config, &captures)?;
        println!("result=PASS");
        Ok(())
    }

    fn parse_args() -> ProbeResult<Config> {
        let mut arguments = std::env::args().skip(1);
        let mut device_name = None;
        let mut output_dir = None;
        let mut exercise_menu = false;
        let mut navigation_key = None;
        let mut restore_menu_taps = 1_u8;
        let mut expected_baseline_text = None;
        let mut expected_menu_text = None;
        let mut expected_navigation_text = None;

        while let Some(argument) = arguments.next() {
            match argument.as_str() {
                "--device" => {
                    device_name = Some(required_value(&mut arguments, "--device")?);
                }
                "--output-dir" => {
                    output_dir = Some(PathBuf::from(required_value(
                        &mut arguments,
                        "--output-dir",
                    )?));
                }
                "--exercise-menu" => exercise_menu = true,
                "--expect-baseline-text" => {
                    expected_baseline_text =
                        Some(required_value(&mut arguments, "--expect-baseline-text")?);
                }
                "--expect-menu-text" => {
                    expected_menu_text =
                        Some(required_value(&mut arguments, "--expect-menu-text")?);
                    exercise_menu = true;
                }
                "--menu-navigation-key" => {
                    let value = required_value(&mut arguments, "--menu-navigation-key")?;
                    navigation_key = Some(parse_key_id(&value)?);
                    exercise_menu = true;
                }
                "--expect-navigation-text" => {
                    expected_navigation_text =
                        Some(required_value(&mut arguments, "--expect-navigation-text")?);
                    exercise_menu = true;
                }
                "--restore-menu-taps" => {
                    let value = required_value(&mut arguments, "--restore-menu-taps")?;
                    restore_menu_taps = value.parse().map_err(|error| {
                        io::Error::new(
                            io::ErrorKind::InvalidInput,
                            format!("invalid restore MENU tap count {value:?}: {error}"),
                        )
                    })?;
                    if !(1..=8).contains(&restore_menu_taps) {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "--restore-menu-taps must be in 1..=8",
                        )
                        .into());
                    }
                    exercise_menu = true;
                }
                "--help" | "-h" => {
                    println!(
                        "automation_probe [--device NAME] [--output-dir PATH] \
                         [--exercise-menu] [--expect-baseline-text TEXT] \
                         [--expect-menu-text TEXT] [--menu-navigation-key HH] \
                         [--expect-navigation-text TEXT] [--restore-menu-taps N]"
                    );
                    std::process::exit(0);
                }
                _ if !argument.starts_with('-') && device_name.is_none() => {
                    device_name = Some(argument);
                }
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("unknown or duplicate argument: {argument}"),
                    )
                    .into());
                }
            }
        }

        let output_dir = match output_dir {
            Some(path) => path,
            None => default_output_dir()?,
        };
        if expected_navigation_text.is_some() && navigation_key.is_none() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "--expect-navigation-text requires --menu-navigation-key",
            )
            .into());
        }
        Ok(Config {
            device_name: device_name.unwrap_or_else(|| "TH-D75".to_owned()),
            output_dir,
            exercise_menu,
            navigation_key,
            restore_menu_taps,
            expected_baseline_text,
            expected_menu_text,
            expected_navigation_text,
        })
    }

    fn parse_key_id(value: &str) -> ProbeResult<FrontPanelKey> {
        let hexadecimal = value
            .strip_prefix("0x")
            .or_else(|| value.strip_prefix("0X"))
            .unwrap_or(value);
        let raw = u8::from_str_radix(hexadecimal, 16).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid hexadecimal key ID {value:?}: {error}"),
            )
        })?;
        FrontPanelKey::try_from(raw).map_err(Into::into)
    }

    fn required_value(
        arguments: &mut impl Iterator<Item = String>,
        option: &str,
    ) -> ProbeResult<String> {
        arguments.next().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{option} requires a value"),
            )
            .into()
        })
    }

    fn default_output_dir() -> ProbeResult<PathBuf> {
        let epoch_millis = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
        Ok(PathBuf::from("automation-evidence").join(epoch_millis.to_string()))
    }

    #[expect(
        clippy::too_many_lines,
        reason = "Evidence validation is kept in the same order as the hardware sequence and its \
                  emitted audit log."
    )]
    fn validate_and_write(config: &Config, captures: &Captures) -> ProbeResult<()> {
        fs::create_dir_all(&config.output_dir)?;
        write_snapshot(
            &config.output_dir,
            "baseline",
            &captures.baseline,
            captures.baseline_capture_elapsed,
        )?;
        println!(
            "qualification_ms={:.3}",
            captures.qualification_elapsed.as_secs_f64() * 1_000.0
        );

        let baseline_observations = recognize("baseline", &captures.baseline.frame)?;
        if let Some(expected) = config.expected_baseline_text.as_deref() {
            require_text("baseline", &baseline_observations, expected)?;
        }

        match (&captures.menu, &captures.restored) {
            (Some(menu), Some(restored)) => {
                let menu_key = captures
                    .menu_key
                    .as_ref()
                    .ok_or_else(|| io::Error::other("missing MENU key metadata"))?;
                let restore_key = captures
                    .restore_key
                    .as_ref()
                    .ok_or_else(|| io::Error::other("missing restore key metadata"))?;
                print_key_metadata("menu_key", menu_key);
                print_key_metadata("restore_key", restore_key);
                println!("restore_menu_taps={}", config.restore_menu_taps);
                let changed_pixels = differing_pixels(&captures.baseline.frame, &menu.frame);
                let restore_delta = differing_pixels(&captures.baseline.frame, &restored.frame);
                if changed_pixels == 0 {
                    return Err(
                        io::Error::other("MENU press produced no framebuffer change").into(),
                    );
                }

                write_snapshot(
                    &config.output_dir,
                    "menu",
                    menu,
                    captures
                        .menu_capture_elapsed
                        .ok_or_else(|| io::Error::other("missing MENU capture duration"))?,
                )?;
                write_snapshot(
                    &config.output_dir,
                    "restored",
                    restored,
                    captures
                        .restore_capture_elapsed
                        .ok_or_else(|| io::Error::other("missing restore capture duration"))?,
                )?;
                println!("menu_changed_pixels={changed_pixels}");
                println!("restored_delta_pixels={restore_delta}");

                let menu_observations = recognize("menu", &menu.frame)?;
                let restored_observations = recognize("restored", &restored.frame)?;
                if let Some(expected) = config.expected_menu_text.as_deref() {
                    require_text("menu", &menu_observations, expected)?;
                } else {
                    println!("menu_semantic_assertion=SKIPPED");
                }
                if let Some(expected) = config.expected_baseline_text.as_deref() {
                    require_text("restored", &restored_observations, expected)?;
                }

                match (&captures.navigated, config.navigation_key) {
                    (Some(navigated), Some(key)) => {
                        let key_metadata = captures
                            .navigation_key
                            .as_ref()
                            .ok_or_else(|| io::Error::other("missing navigation key metadata"))?;
                        let elapsed = captures.navigation_capture_elapsed.ok_or_else(|| {
                            io::Error::other("missing navigation capture duration")
                        })?;
                        print_key_metadata("navigation_key", key_metadata);
                        let label = format!("navigation-{:02X}", key.as_u8());
                        write_snapshot(&config.output_dir, &label, navigated, elapsed)?;
                        let menu_delta = differing_pixels(&menu.frame, &navigated.frame);
                        if menu_delta == 0 {
                            return Err(io::Error::other(format!(
                                "key 0x{:02X} produced no menu framebuffer change",
                                key.as_u8()
                            ))
                            .into());
                        }
                        println!("navigation_changed_pixels={menu_delta}");
                        let observations = recognize("navigation", &navigated.frame)?;
                        if let Some(expected) = config.expected_navigation_text.as_deref() {
                            require_navigation_text(key, &observations, expected)?;
                        } else {
                            println!("navigation_semantic_assertion=SKIPPED");
                        }
                    }
                    (None, None) => {}
                    _ => {
                        return Err(io::Error::other(
                            "internal error: incomplete navigation capture set",
                        )
                        .into());
                    }
                }
            }
            (None, None) => {}
            _ => {
                return Err(io::Error::other("internal error: incomplete MENU capture set").into());
            }
        }

        println!("evidence_dir={}", config.output_dir.display());
        Ok(())
    }

    fn write_snapshot(
        output_dir: &Path,
        label: &str,
        snapshot: &AutomationSnapshot,
        elapsed: std::time::Duration,
    ) -> ProbeResult<()> {
        let path = output_dir.join(format!("{label}.bmp"));
        fs::write(&path, snapshot.frame.to_stock_bmp())?;
        println!(
            "{label}=generation:{},crc32:{:08X},rle_bytes:{},capture_attempts:{},capture_ms:{:.3},path:{}",
            snapshot.metadata.generation,
            snapshot.metadata.crc32,
            snapshot.metadata.rle_encoded_length,
            snapshot.metadata.capture_attempts,
            elapsed.as_secs_f64() * 1_000.0,
            path.display()
        );
        Ok(())
    }

    fn print_key_metadata(label: &str, metadata: &AutomationMetadata) {
        println!(
            "{label}=command_count:{},host_sequence:{:02X},key:{:02X},phase:{},result:{}",
            metadata.command_count,
            metadata.last_host_sequence,
            metadata.last_key,
            metadata.last_phase,
            metadata.last_key_result
        );
    }

    fn recognize(label: &str, frame: &ScreenFrame) -> ProbeResult<Vec<TextObservation>> {
        let started = Instant::now();
        let observations = frame.recognize_text()?;
        println!(
            "{label}_ocr_ms={:.3}",
            started.elapsed().as_secs_f64() * 1_000.0
        );
        for observation in &observations {
            let bounds = observation.bounds();
            println!(
                "{label}_text={:?},confidence:{:.4},bounds:{:.4},{:.4},{:.4},{:.4}",
                observation.text(),
                observation.confidence(),
                bounds.x(),
                bounds.y(),
                bounds.width(),
                bounds.height()
            );
        }
        Ok(observations)
    }

    fn require_text(
        label: &str,
        observations: &[TextObservation],
        expected: &str,
    ) -> ProbeResult<()> {
        let matched = require_unique_text(
            observations,
            expected,
            0.90,
            NormalizedBounds::FULL_SCREEN,
            1.0,
        )?;
        println!(
            "{label}_asserted_text={:?},confidence:{:.4}",
            matched.text(),
            matched.confidence()
        );
        Ok(())
    }

    fn require_navigation_text(
        key: FrontPanelKey,
        observations: &[TextObservation],
        expected: &str,
    ) -> ProbeResult<()> {
        let roi = if matches!(
            key,
            FrontPanelKey::Up | FrontPanelKey::Down | FrontPanelKey::Left | FrontPanelKey::Right
        ) {
            NormalizedBounds::new(0.0, 0.72, 1.0, 0.20)?
        } else {
            NormalizedBounds::FULL_SCREEN
        };
        let matched = require_unique_text(observations, expected, 0.90, roi, 1.0)?;
        println!(
            "navigation_asserted_text={:?},confidence:{:.4}",
            matched.text(),
            matched.confidence()
        );
        Ok(())
    }

    fn differing_pixels(left: &ScreenFrame, right: &ScreenFrame) -> usize {
        left.rgb565_le()
            .chunks_exact(2)
            .zip(right.rgb565_le().chunks_exact(2))
            .filter(|(left_pixel, right_pixel)| left_pixel != right_pixel)
            .count()
    }
}

#[cfg(target_os = "macos")]
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    macos::run().await
}

#[cfg(not(target_os = "macos"))]
fn main() {
    use kenwood_thd75 as _;
    use tokio as _;

    eprintln!("automation_probe requires macOS native Bluetooth");
    std::process::exit(2);
}
