//! Transactional terminal setup and independently reported restoration failures.

use std::fmt;
use std::io;

use crossterm::cursor::{Hide, Show};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};

/// One terminal state transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Raw,
    Alternate,
    Hide,
    Show,
    Leave,
    Cooked,
}

/// Failed transition and its original I/O error.
#[derive(Debug)]
pub(crate) struct TerminalError {
    action: Action,
    source: io::Error,
}
impl fmt::Display for TerminalError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "terminal {:?} failed: {}",
            self.action, self.source
        )
    }
}
impl std::error::Error for TerminalError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// Terminal transitions still requiring restoration.
#[derive(Debug, Default)]
pub(crate) struct TerminalSession {
    raw: bool,
    alternate: bool,
    hidden: bool,
}
impl TerminalSession {
    /// Enter raw alternate-screen mode; roll back every completed transition on failure.
    pub(crate) fn enter(&mut self) -> Result<(), Vec<TerminalError>> {
        self.enter_with(perform)
    }
    fn enter_with(
        &mut self,
        mut apply: impl FnMut(Action) -> io::Result<()>,
    ) -> Result<(), Vec<TerminalError>> {
        for action in [Action::Raw, Action::Alternate, Action::Hide] {
            match action {
                Action::Raw => self.raw = true,
                Action::Alternate => self.alternate = true,
                Action::Hide => self.hidden = true,
                _ => unreachable!(),
            }
            if let Err(source) = apply(action) {
                let mut failures = vec![TerminalError { action, source }];
                failures.extend(self.restore_with(&mut apply));
                return Err(failures);
            }
        }
        Ok(())
    }
    /// Attempt cursor, screen and raw-mode restoration even if another step fails.
    pub(crate) fn restore(&mut self) -> Vec<TerminalError> {
        self.restore_with(perform)
    }
    fn restore_with(
        &mut self,
        mut apply: impl FnMut(Action) -> io::Result<()>,
    ) -> Vec<TerminalError> {
        let mut failures = Vec::new();
        for (pending, action) in [
            (&mut self.hidden, Action::Show),
            (&mut self.alternate, Action::Leave),
            (&mut self.raw, Action::Cooked),
        ] {
            if *pending {
                match apply(action) {
                    Ok(()) => *pending = false,
                    Err(source) => failures.push(TerminalError { action, source }),
                }
            }
        }
        failures
    }
}
impl Drop for TerminalSession {
    fn drop(&mut self) {
        let _failures = self.restore();
    }
}
fn perform(action: Action) -> io::Result<()> {
    match action {
        Action::Raw => enable_raw_mode(),
        Action::Cooked => disable_raw_mode(),
        Action::Alternate => execute!(io::stdout(), EnterAlternateScreen),
        Action::Leave => execute!(io::stdout(), LeaveAlternateScreen),
        Action::Hide => execute!(io::stdout(), Hide),
        Action::Show => execute!(io::stdout(), Show),
    }
}

#[cfg(test)]
mod tests {
    use super::{Action, TerminalSession};
    use std::mem::ManuallyDrop;
    #[test]
    fn partial_setup_rolls_back_and_cleanup_attempts_every_pending_transition() {
        let mut terminal = ManuallyDrop::new(TerminalSession::default());
        let mut actions = Vec::new();
        let result = terminal.enter_with(|action| {
            actions.push(action);
            if action == Action::Hide {
                Err(std::io::Error::other("injected hide failure"))
            } else {
                Ok(())
            }
        });
        assert!(result.is_err(), "setup failure was discarded");
        assert_eq!(
            actions,
            [
                Action::Raw,
                Action::Alternate,
                Action::Hide,
                Action::Show,
                Action::Leave,
                Action::Cooked
            ]
        );
        let mut terminal = ManuallyDrop::new(TerminalSession {
            raw: true,
            alternate: true,
            hidden: true,
        });
        actions.clear();
        let failures = terminal.restore_with(|action| {
            actions.push(action);
            Err(std::io::Error::other("injected restore failure"))
        });
        assert_eq!(actions, [Action::Show, Action::Leave, Action::Cooked]);
        assert_eq!(failures.len(), 3);
        // Clear the injected state through the same seam before fixture destruction.
        assert!(terminal.restore_with(|_action| Ok(())).is_empty());
    }
}
