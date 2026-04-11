//! Transmit confirmation prompt and script-mode gating.
//!
//! Every transmit command in the REPL (cq, beacon, position, msg,
//! echo, link) calls `tx_confirm` before keying the radio. This
//! guards blind operators from accidentally transmitting on air by
//! mistyping an argument or pasting the wrong line at the prompt.
//!
//! Two global atomic flags steer the prompt behaviour:
//!
//! - `CONFIRM_TX` (default `true`) - require the prompt at all.
//!   Turned off globally by `--yes` or `confirm off`.
//! - `SCRIPT_MODE` (default `false`) - the REPL is running a
//!   non-interactive script (stdin piped or `--script` passed). In
//!   script mode the prompt cannot be answered, so every transmit
//!   command aborts with a clear error unless confirmation has been
//!   explicitly disabled via `--yes`.

use std::io::BufRead as _;
use std::io::Write as _;
use std::sync::atomic::{AtomicBool, Ordering};

/// When `true` (the default) every transmit command prompts the user
/// before keying the radio. Cleared by `--yes` on the command line
/// and by the interactive `confirm off` command.
pub static CONFIRM_TX: AtomicBool = AtomicBool::new(true);

/// When `true` the REPL is running under a script or `--script`
/// file. Prompts cannot be answered, so [`tx_confirm`] prints an
/// error and returns `false` unless [`CONFIRM_TX`] is also cleared.
pub static SCRIPT_MODE: AtomicBool = AtomicBool::new(false);

/// Whether a transmit confirmation prompt is currently required.
#[must_use]
pub fn is_required() -> bool {
    CONFIRM_TX.load(Ordering::Relaxed)
}

/// Enable or disable transmit confirmation.
pub fn set_required(required: bool) {
    CONFIRM_TX.store(required, Ordering::Relaxed);
}

/// Mark the REPL as running under a non-interactive script.
pub fn set_script_mode(on: bool) {
    SCRIPT_MODE.store(on, Ordering::Relaxed);
}

/// Prompt the user to confirm a transmit action.
///
/// Returns `true` when the caller may proceed:
/// - [`CONFIRM_TX`] is false — confirmation globally disabled.
/// - The user typed `y` or `yes` (case-insensitive).
///
/// Returns `false` when the caller must abort:
/// - Script mode is active and confirmation is required.
/// - The user typed anything else, including blank lines.
/// - Reading from stdin failed.
///
/// In the abort path the function prints an explanatory line via
/// plain `println!` (so the message is *not* suppressed by the
/// history buffer or the quiet flag — confirmation is always loud).
pub fn tx_confirm() -> bool {
    if !is_required() {
        return true;
    }
    if SCRIPT_MODE.load(Ordering::Relaxed) {
        println!("Error: transmit commands require --yes in script mode.");
        return false;
    }
    println!("Confirm transmit? Type yes or y to proceed, anything else to cancel.");
    // Flush stdout so the prompt is visible before the blocking read.
    // A flush failure is fine — the prompt was already sent via
    // `println!` so there is nothing left buffered.
    let _ = std::io::stdout().flush();

    let stdin = std::io::stdin();
    let mut line = String::new();
    if stdin.lock().read_line(&mut line).is_err() {
        println!("Transmission cancelled.");
        return false;
    }
    let trimmed = line.trim().to_lowercase();
    if trimmed == "y" || trimmed == "yes" {
        true
    } else {
        println!("Transmission cancelled.");
        false
    }
}

#[cfg(test)]
mod tests {
    use super::{CONFIRM_TX, SCRIPT_MODE, is_required, set_required, set_script_mode};
    use std::sync::Mutex;
    use std::sync::atomic::Ordering;

    /// Tests mutate global atomics so they must serialise.
    static GUARD: Mutex<()> = Mutex::new(());

    fn reset() {
        CONFIRM_TX.store(true, Ordering::Relaxed);
        SCRIPT_MODE.store(false, Ordering::Relaxed);
    }

    #[test]
    fn defaults_require_confirmation() {
        let _g = GUARD
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        reset();
        assert!(is_required());
    }

    #[test]
    fn set_required_toggles_flag() {
        let _g = GUARD
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        reset();
        set_required(false);
        assert!(!is_required());
        set_required(true);
        assert!(is_required());
    }

    #[test]
    fn script_mode_persists() {
        let _g = GUARD
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        reset();
        set_script_mode(true);
        assert!(SCRIPT_MODE.load(Ordering::Relaxed));
        set_script_mode(false);
        assert!(!SCRIPT_MODE.load(Ordering::Relaxed));
    }
}
