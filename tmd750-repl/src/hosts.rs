//! Local reflector configuration, with explicit precedence and file errors.

use std::io;
use std::path::{Path, PathBuf};

use dstar_gateway_core::Callsign;
use dstar_gateway_core::hosts::{HostEntry, HostFile};

const HOST_FILES: &[(&str, u16)] = &[
    ("DExtra_Hosts.txt", 30_001),
    ("DPlus_Hosts.txt", 20_001),
    ("DCS_Hosts.txt", 30_051),
    ("Local_Hosts.txt", 30_001),
];

/// A configuration failure, retaining the file path and original I/O cause.
#[derive(Debug, thiserror::Error)]
pub(super) enum HostError {
    /// The platform did not provide an application configuration directory.
    #[error("cannot determine the platform configuration directory for reflector host files")]
    NoConfigDirectory,
    /// An existing file could not be read as UTF-8 text.
    #[error("could not read reflector host file {}: {source}", path.display())]
    Read {
        /// File that failed to load.
        path: PathBuf,
        /// Original filesystem or decoding error.
        #[source]
        source: io::Error,
    },
    /// No configured file contained the requested reflector.
    #[error(
        "{name} was not found in reflector host files.\nSearched {} and {}.\nFiles: DExtra_Hosts.txt, DPlus_Hosts.txt, DCS_Hosts.txt, Local_Hosts.txt.\nAdd NAME HOSTNAME UDP_PORT, without the module letter.\nObtain the current address from the reflector operator; see the REPL README.",
        config.join("thd75-repl").display(), config.join("tmd750-repl").display()
    )]
    NotFound {
        /// Validated reflector name without its module.
        name: Callsign,
        /// Platform configuration root used for this lookup.
        config: PathBuf,
    },
}

/// Resolve a reflector from existing TH-D75 files and TM-D750 overrides.
///
/// Missing files are optional. All other file errors are returned instead of
/// silently falling back to a potentially stale address in another file.
pub(super) fn resolve(name: Callsign) -> Result<HostEntry, HostError> {
    let config = dirs_next::config_dir().ok_or(HostError::NoConfigDirectory)?;
    resolve_in(&config, name)
}

fn resolve_in(config: &Path, name: Callsign) -> Result<HostEntry, HostError> {
    let hosts = load_in(config)?;
    hosts
        .lookup(&name.as_str())
        .cloned()
        .ok_or_else(|| HostError::NotFound {
            name,
            config: config.to_owned(),
        })
}

fn load_in(config: &Path) -> Result<HostFile, HostError> {
    let mut hosts = HostFile::new();
    for application in ["thd75-repl", "tmd750-repl"] {
        for (filename, default_port) in HOST_FILES {
            let path = config.join(application).join(filename);
            match std::fs::read_to_string(&path) {
                Ok(contents) => hosts.parse(&contents, *default_port),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(source) => return Err(HostError::Read { path, source }),
            }
        }
    }
    Ok(hosts)
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;
    const REFLECTOR: Callsign = Callsign::from_wire_bytes(*b"REF030  ");

    #[test]
    fn clean_install_error_contains_paths_and_setup_instructions() -> TestResult {
        let config = tempfile::tempdir()?;
        let result = resolve_in(config.path(), REFLECTOR);
        let Err(error @ HostError::NotFound { .. }) = result else {
            return Err(format!("expected actionable missing-host error: {result:?}").into());
        };
        let message = error.to_string();
        assert!(message.contains(config.path().to_string_lossy().as_ref()));
        assert!(message.contains("DPlus_Hosts.txt"));
        assert!(message.contains("NAME HOSTNAME UDP_PORT"));
        Ok(())
    }

    #[test]
    fn application_and_local_file_overrides_have_documented_precedence() -> TestResult {
        let config = tempfile::tempdir()?;
        let legacy = config.path().join("thd75-repl");
        let current = config.path().join("tmd750-repl");
        std::fs::create_dir(&legacy)?;
        std::fs::create_dir(&current)?;
        std::fs::write(
            legacy.join("DPlus_Hosts.txt"),
            "REF030 legacy.example 20001\n",
        )?;
        assert_eq!(
            resolve_in(config.path(), REFLECTOR)?.address,
            "legacy.example"
        );
        std::fs::write(current.join("DPlus_Hosts.txt"), "REF030 current.example\n")?;
        let current_entry = resolve_in(config.path(), REFLECTOR)?;
        assert_eq!(current_entry.address, "current.example");
        assert_eq!(current_entry.port, 20_001);
        std::fs::write(
            current.join("Local_Hosts.txt"),
            "REF030 local.example 20002\n",
        )?;
        let local_entry = resolve_in(config.path(), REFLECTOR)?;
        assert_eq!(local_entry.address, "local.example");
        assert_eq!(local_entry.port, 20_002);
        Ok(())
    }

    #[test]
    fn invalid_utf8_preserves_path_and_source_instead_of_using_legacy_entry() -> TestResult {
        let config = tempfile::tempdir()?;
        let legacy = config.path().join("thd75-repl");
        let current = config.path().join("tmd750-repl");
        std::fs::create_dir(&legacy)?;
        std::fs::create_dir(&current)?;
        std::fs::write(legacy.join("DPlus_Hosts.txt"), "REF030 legacy.example\n")?;
        let invalid = current.join("DPlus_Hosts.txt");
        std::fs::write(&invalid, [0xff])?;
        let result = resolve_in(config.path(), REFLECTOR);
        assert!(matches!(
            result,
            Err(HostError::Read { path, source })
                if path == invalid && source.kind() == io::ErrorKind::InvalidData
        ));
        Ok(())
    }
}
