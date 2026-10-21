// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

//! macOS application-bundle creation before opening GUI, network or audio state.

use std::error::Error;
use std::ffi::OsString;
use std::fmt;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

use crate::persistence::{SaveError, save_atomic, save_with};

/// Command-line rejection or bundle publication failure.
#[derive(Debug)]
pub(crate) enum BundleError {
    Argument(OsString),
    Platform,
    Io(io::Error),
    Publish(SaveError),
}

impl fmt::Display for BundleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Argument(argument) => write!(
                f,
                "unexpected argument {}; use --help",
                argument.to_string_lossy()
            ),
            Self::Platform => f.write_str("--bundle requires macOS"),
            Self::Io(error) => write!(f, "read executable for bundle: {error}"),
            Self::Publish(error) => error.fmt(f),
        }
    }
}

impl Error for BundleError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Publish(error) => Some(error),
            Self::Argument(_) | Self::Platform => None,
        }
    }
}

/// Handle `--help` or `--bundle [destination.app]` before starting application I/O.
/// An empty argument list selects the GUI. Other arguments are rejected.
pub(crate) fn run_if_requested(
    mut arguments: impl Iterator<Item = OsString>,
) -> Result<bool, BundleError> {
    let Some(argument) = arguments.next() else {
        return Ok(false);
    };
    if argument == "--help" {
        if let Some(extra) = arguments.next() {
            return Err(BundleError::Argument(extra));
        }
        println!("Usage: sextant [--bundle [destination.app] | --help]");
        return Ok(true);
    }
    if argument != "--bundle" {
        return Err(BundleError::Argument(argument));
    }
    let destination = arguments.next().map_or_else(
        || Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/Sextant.app"),
        PathBuf::from,
    );
    if let Some(extra) = arguments.next() {
        return Err(BundleError::Argument(extra));
    }
    if !cfg!(target_os = "macos") {
        return Err(BundleError::Platform);
    }
    create_bundle(
        &std::env::current_exe().map_err(BundleError::Io)?,
        &destination,
    )?;
    println!("Created {}", destination.display());
    Ok(true)
}

/// Publish the running executable and purpose metadata as individually synced files.
/// Existing bundle resources remain in place; each failed file write retains its
/// preceding contents. Success confirms both publications, without granting media
/// authorization or signing the enclosing application bundle.
fn create_bundle(binary: &Path, destination: &Path) -> Result<(), BundleError> {
    let mut source = File::open(binary).map_err(BundleError::Io)?;
    let permissions = source.metadata().map_err(BundleError::Io)?.permissions();
    let executable = destination.join("Contents/MacOS/sextant");
    let _executable = save_with(&executable, |file| {
        let _copied = io::copy(&mut source, file)?;
        file.set_permissions(permissions)
    })
    .map_err(BundleError::Publish)?;
    let metadata = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
    <key>CFBundleExecutable</key><string>sextant</string>
    <key>CFBundleIdentifier</key><string>dev.swiftraccoon.sextant</string>
    <key>CFBundleName</key><string>Sextant</string>
    <key>CFBundleDisplayName</key><string>Sextant</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleVersion</key><string>{version}</string>
    <key>CFBundleShortVersionString</key><string>{version}</string>
    <key>NSMicrophoneUsageDescription</key>
    <string>Sextant captures microphone audio for D-STAR voice transmission.</string>
    <key>NSHighResolutionCapable</key><true/>
</dict></plist>
"#,
        version = env!("CARGO_PKG_VERSION")
    );
    let _metadata = save_atomic(
        &destination.join("Contents/Info.plist"),
        metadata.as_bytes(),
    )
    .map_err(BundleError::Publish)?;
    Ok(())
}
