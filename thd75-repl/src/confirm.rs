//! Transmit confirmation with explicit refusal, EOF and I/O outcomes.
//!
//! Confirmation policy is process-wide. Noninteractive sessions refuse a
//! required prompt without consuming a queued script command.

use std::error::Error;
use std::fmt;
use std::io::{self, BufRead, Read as _};
use std::sync::{Mutex, MutexGuard, OnceLock};

/// Result of a completed transmit-confirmation decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Confirmation {
    /// Confirmation is disabled or the response is y/yes.
    Approved,
    /// The response declined transmission.
    Declined,
    /// Input ended before any response.
    EndOfInput,
    /// Required confirmation cannot be answered in a script.
    ScriptRefused,
}

impl Confirmation {
    /// Whether the caller may start the requested transmission.
    #[must_use]
    pub const fn is_approved(self) -> bool {
        matches!(self, Self::Approved)
    }
}

/// Confirmation failure before a decision was made.
#[derive(Debug)]
#[non_exhaustive]
pub enum ConfirmationError {
    /// The response could not be read.
    Input(io::Error),
    /// The prompt could not be written or flushed.
    Output(io::Error),
    /// A response exceeded 64 UTF-8 bytes including its line terminator.
    ResponseTooLong,
}

impl fmt::Display for ConfirmationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Input(error) => write!(f, "reading transmit confirmation: {error}"),
            Self::Output(error) => write!(f, "writing transmit confirmation: {error}"),
            Self::ResponseTooLong => f.write_str("transmit confirmation exceeds 64 bytes"),
        }
    }
}

impl Error for ConfirmationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Input(error) | Self::Output(error) => Some(error),
            Self::ResponseTooLong => None,
        }
    }
}

#[derive(Debug)]
struct Configuration {
    required: bool,
    script: bool,
}

impl Default for Configuration {
    fn default() -> Self {
        Self {
            required: true,
            script: false,
        }
    }
}

fn configuration() -> MutexGuard<'static, Configuration> {
    static CONFIGURATION: OnceLock<Mutex<Configuration>> = OnceLock::new();
    CONFIGURATION
        .get_or_init(|| Mutex::new(Configuration::default()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Read whether transmit confirmation is required.
#[must_use]
pub fn is_required() -> bool {
    configuration().required
}

/// Enable or disable transmit confirmation.
pub fn set_required(required: bool) {
    configuration().required = required;
}

/// Select noninteractive policy without exposing mutable configuration storage.
pub fn set_script_mode(enabled: bool) {
    configuration().script = enabled;
}

/// Prompt before transmission and return its exact decision.
///
/// Disabled confirmation approves immediately. A required script prompt returns
/// `Confirmation::ScriptRefused` without reading stdin. Interactive EOF, negative answers and
/// successful answers remain distinct. Prompt/status messages use shared
/// escaped stdout/history output. Responses over 64 bytes are drained through
/// their line ending and rejected.
///
/// # Errors
///
/// Returns Input for read/drain failures, Output for prompt write/flush failure,
/// or `ConfirmationError::ResponseTooLong`. No transmission is performed.
pub fn tx_confirm() -> Result<Confirmation, ConfirmationError> {
    let (required, script) = {
        let config = configuration();
        (config.required, config.script)
    };
    if !required {
        return Ok(Confirmation::Approved);
    }
    if script {
        crate::aprintln!("Error: transmit commands require --yes in script mode.");
        return Ok(Confirmation::ScriptRefused);
    }
    crate::aprintln!("Confirm transmit? Type yes or y to proceed; anything else cancels.");
    if let Some(error) = crate::take_output_error() {
        return Err(ConfirmationError::Output(error));
    }
    let result = read_answer(&mut io::stdin().lock())?;
    match result {
        Confirmation::Declined => crate::aprintln!("Transmission cancelled."),
        Confirmation::EndOfInput => crate::aprintln!("Input ended; transmission cancelled."),
        Confirmation::Approved | Confirmation::ScriptRefused => {}
    }
    Ok(result)
}

fn read_answer(reader: &mut impl BufRead) -> Result<Confirmation, ConfirmationError> {
    let mut response = String::new();
    let read = reader
        .by_ref()
        .take(65)
        .read_line(&mut response)
        .map_err(ConfirmationError::Input)?;
    if read == 0 {
        return Ok(Confirmation::EndOfInput);
    }
    if read > 64 {
        if !response.ends_with('\n') {
            let _drained = reader.skip_until(b'\n').map_err(ConfirmationError::Input)?;
        }
        return Err(ConfirmationError::ResponseTooLong);
    }
    let response = response.trim();
    Ok(
        if response.eq_ignore_ascii_case("y") || response.eq_ignore_ascii_case("yes") {
            Confirmation::Approved
        } else {
            Confirmation::Declined
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn Error>>;

    #[test]
    fn approval_decline_and_eof_are_distinct() -> TestResult {
        assert_eq!(read_answer(&mut &b"YES\n"[..])?, Confirmation::Approved);
        assert_eq!(read_answer(&mut &b"n\n"[..])?, Confirmation::Declined);
        assert_eq!(read_answer(&mut &b""[..])?, Confirmation::EndOfInput);
        assert_eq!(read_answer(&mut &b"\n"[..])?, Confirmation::Declined);
        Ok(())
    }

    #[test]
    fn long_response_drains_its_line_but_keeps_the_next_command() -> TestResult {
        let input = format!("{}\nnext command\n", "y".repeat(100));
        let mut reader = io::Cursor::new(input);
        let result = read_answer(&mut reader);
        assert!(
            matches!(result, Err(ConfirmationError::ResponseTooLong)),
            "{result:?}"
        );
        let mut remaining = String::new();
        let _read = reader.read_line(&mut remaining)?;
        assert_eq!(remaining, "next command\n");
        Ok(())
    }

    #[test]
    fn input_failure_retains_its_io_source() {
        #[derive(Debug)]
        struct FailedReader;

        impl io::Read for FailedReader {
            fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::new(io::ErrorKind::BrokenPipe, "lost input"))
            }
        }

        let mut reader = io::BufReader::new(FailedReader);
        let result = read_answer(&mut reader);
        assert!(
            matches!(result, Err(ConfirmationError::Input(ref error))
            if error.kind() == io::ErrorKind::BrokenPipe),
            "{result:?}"
        );
    }
}
