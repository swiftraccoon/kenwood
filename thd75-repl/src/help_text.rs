//! Per-command detailed help text.
//!
//! `for_command(cmd)` returns a multi-line detailed help block for a
//! specific command, or `None` if the command name is unknown.
//!
//! `ALL_COMMANDS` lists every command we have detailed help for. The
//! lint test in this module iterates the list and runs the
//! accessibility lint over every entry.
//!
//! This module also holds the three multi-line help blobs that list
//! commands for each REPL mode: CAT, APRS, and D-STAR. These were
//! previously `println!`-driven functions in `commands.rs`; moving
//! them here lets the lint test exercise them uniformly with the
//! per-command entries.

/// The names of every command that has a detailed help entry. Used by
/// `help all` to iterate and by the lint test to assert coverage.
pub const ALL_COMMANDS: &[&str] = &[
    "id",
    "battery",
    "clock",
    "freq",
    "mode",
    "squelch",
    "power",
    "att",
    "meter",
    "lock",
    "dualband",
    "bt",
    "vox",
    "fm",
    "vfo",
    "up",
    "down",
    "tune",
    "recall",
    "channel",
    "channels",
    "step",
    "gps",
    "urcall",
    "cq",
    "reflector",
    "unreflector",
    "aprs",
    "dstar",
    "last",
    "status",
    "check",
    "verbose",
    "quiet",
    "confirm",
    "help",
    "quit",
];

/// Return detailed help text for a single command, or `None` if the
/// command name is not recognized.
#[must_use]
pub fn for_command(cmd: &str) -> Option<&'static str> {
    match cmd {
        "id" | "identify" => Some(ID_HELP),
        "battery" | "bat" => Some(BATTERY_HELP),
        "clock" | "time" => Some(CLOCK_HELP),
        "freq" | "frequency" => Some(FREQ_HELP),
        "mode" => Some(MODE_HELP),
        "squelch" | "sq" => Some(SQUELCH_HELP),
        "power" | "pwr" => Some(POWER_HELP),
        "att" | "attenuator" => Some(ATT_HELP),
        "meter" | "smeter" => Some(METER_HELP),
        "lock" => Some(LOCK_HELP),
        "dualband" | "dual" => Some(DUALBAND_HELP),
        "bt" | "bluetooth" => Some(BT_HELP),
        "vox" => Some(VOX_HELP),
        "fm" => Some(FM_HELP),
        "vfo" => Some(VFO_HELP),
        "up" => Some(UP_HELP),
        "down" => Some(DOWN_HELP),
        "tune" => Some(TUNE_HELP),
        "recall" => Some(RECALL_HELP),
        "ch" | "channel" => Some(CHANNEL_HELP),
        "channels" => Some(CHANNELS_HELP),
        "step" => Some(STEP_HELP),
        "gps" => Some(GPS_HELP),
        "urcall" | "ur" => Some(URCALL_HELP),
        "cq" => Some(CQ_HELP),
        "reflector" | "ref" => Some(REFLECTOR_HELP),
        "unreflector" | "unlink" | "unref" => Some(UNREFLECTOR_HELP),
        "aprs" => Some(APRS_HELP),
        "dstar" => Some(DSTAR_HELP),
        "last" | "repeat" => Some(LAST_HELP),
        "status" => Some(STATUS_HELP),
        "check" => Some(CHECK_HELP),
        "verbose" | "quiet" => Some(VERBOSE_HELP),
        "confirm" => Some(CONFIRM_HELP),
        "help" | "?" => Some(HELP_HELP),
        "quit" | "exit" | "q" => Some(QUIT_HELP),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Mode help text (multi-line blobs listing all commands per mode).
// These replace the old `commands::help()`, `commands::aprs_help()`,
// and `commands::dstar_help()` functions that called `println!`
// repeatedly. Returning them as constants lets the lint test in this
// module check every line.
// ---------------------------------------------------------------------------

/// CAT mode help text listing all available commands.
pub const CAT_MODE_HELP: &str = "-- Information --
help: Show this help text
id: Radio model and identification
battery: Battery charge level
clock: Radio real-time clock
quit: Exit the program
-- Reading radio state --
freq: Frequency on band A (or: freq b)
mode: Operating mode on band A (or: mode b)
squelch: Squelch level on band A (or: squelch b)
power: Transmit power on band A (or: power b)
att (attenuator): Attenuator on or off (or: att b)
meter: Signal strength meter, S0 to S9 (or: meter b)
lock: Key lock on or off
dualband: Dual band display on or off
bt (bluetooth): Bluetooth on or off
vox: Voice-operated transmit on or off
fm: FM broadcast radio on or off
vfo: Full variable frequency oscillator state (or: vfo b)
-- Changing settings --
mode a fm: Set mode. Options: fm, nfm, am, dv, lsb, usb, cw
squelch a 3: Set squelch level, 0 through 5
power a high: Set power. Options: high, medium, low, extra-low
att a on: Set attenuator on or off
lock on: Set key lock on or off
dualband on: Set dual band on or off
bt on: Set Bluetooth on or off
vox on: Set voice-operated transmit on or off
vox gain 5: Set voice-operated transmit gain, 0 through 9
vox delay 3: Set voice-operated transmit delay, 0 through 6
fm on: Set FM broadcast radio on or off
step a 5: Set frequency step size by index, 0 through 11
-- Tuning --
up: Step frequency up on band A (or: up b)
down: Step frequency down on band A (or: down b)
tune a 146.520: Tune band A to a frequency in megahertz
recall a 5: Recall memory channel 5 on band A
-- Memory channels --
ch 5: Read memory channel 5
channels: List programmed channels, default 0 through 19
channels 0 100: List programmed channels 0 through 99
-- D-STAR digital voice --
urcall: Read destination callsign (D-STAR your-call field)
urcall W1AW: Set destination callsign
cq: Set destination to CQ CQ CQ (general call)
reflector REF030 C: Connect to reflector, module C
unreflector: Disconnect from reflector
-- GPS --
gps on on: Set GPS receiver on, PC output on
gps off off: Set GPS receiver off, PC output off
-- APRS packet radio mode --
aprs start MYCALL 7: Enter APRS mode with callsign and SSID
-- D-STAR gateway mode --
dstar start MYCALL XRF030C: Enter D-STAR gateway with reflector";

/// APRS mode help text listing APRS-specific commands.
pub const APRS_MODE_HELP: &str = "You are in APRS packet radio mode.
-- APRS commands --
monitor: Continuously listen for APRS events (Ctrl-C to stop)
msg W1AW Hello there: Send an APRS message to a station
position 35.30 -82.46 Portable: Send a position beacon
beacon: Send a status beacon
stations: List recently heard stations
igate r/35.30/-82.46/100: Bridge RF to APRS-IS with a filter
aprs stop: Leave APRS mode, return to normal radio control
quit: Exit the program";

/// D-STAR gateway mode help text.
pub const DSTAR_MODE_HELP: &str = "You are in D-STAR digital voice gateway mode.
-- D-STAR gateway commands --
monitor: Start listening for voice and reflector events (Ctrl-C to stop)
link REF030C: Connect to a reflector and start monitoring
  Supported prefixes: REF (DPlus), XRF/XLX (DExtra), DCS
unlink: Disconnect from the reflector
echo: Test your audio. Records your transmission, plays it back
text Hello: Set a text message to embed in your next voice transmission
text clear: Remove the outgoing text message
heard: List recently heard stations
status: Modem and reflector connection status
dstar stop: Leave D-STAR mode, return to normal radio control
quit: Exit the program";

// ---------------------------------------------------------------------------
// Per-command detailed help entries.
// ---------------------------------------------------------------------------

const ID_HELP: &str = "id: Read the radio model and identification.
Syntax: id
Example: id
Related commands: status, clock";

const BATTERY_HELP: &str = "battery: Read the current battery charge level.
Syntax: battery
Output is one of: empty, one third, two thirds, full, charging.
Example: battery
Related commands: status";

const CLOCK_HELP: &str = "clock: Read the radio's real-time clock.
Syntax: clock
Example: clock
Related commands: status";

const FREQ_HELP: &str = "freq: Read the current frequency on a band.
Syntax: freq [a or b]
Default band is A.
Example: freq
Example: freq b
Related commands: tune, up, down, vfo, channel";

const MODE_HELP: &str = "mode: Read or set the operating mode on a band.
Syntax: mode [a or b] [mode_name]
Valid modes: fm, nfm, am, dv, lsb, usb, cw, dr, wfm.
Example: mode a
Example: mode a fm
Example: mode b dv
Related commands: freq, vfo";

const SQUELCH_HELP: &str = "squelch: Read or set the squelch level on a band.
Syntax: squelch [a or b] [0 through 5]
Example: squelch
Example: squelch a 3
Related commands: meter";

const POWER_HELP: &str = "power: Read or set the transmit power on a band.
Syntax: power [a or b] [level]
Valid levels: high, medium, low, extra-low.
Example: power a
Example: power a high
Related commands: vox";

const ATT_HELP: &str = "att: Read or set the attenuator on a band.
Syntax: att [a or b] [on or off]
Example: att a
Example: att a on
Related commands: freq";

const METER_HELP: &str = "meter: Read the signal strength meter on a band.
Syntax: meter [a or b]
Output is a letter-number pair from S0 to S9.
Example: meter
Example: meter b
Related commands: squelch, status";

const LOCK_HELP: &str = "lock: Read or set the key lock state.
Syntax: lock [on or off]
Example: lock
Example: lock on
Related commands: status";

const DUALBAND_HELP: &str = "dualband: Read or set the dual-band display mode.
Syntax: dualband [on or off]
Example: dualband
Example: dualband on";

const BT_HELP: &str = "bt: Read or set the Bluetooth state.
Syntax: bt [on or off]
Example: bt
Example: bt off";

const VOX_HELP: &str = "vox: Read or set voice-operated transmit.
Syntax: vox [on or off]
Syntax: vox gain [0 through 9]
Syntax: vox delay [0 through 6]
Example: vox on
Example: vox gain 5
Example: vox delay 3";

const FM_HELP: &str = "fm: Read or set the FM broadcast radio receiver.
Syntax: fm [on or off]
Example: fm on";

const VFO_HELP: &str = "vfo: Read the full variable frequency oscillator state.
Syntax: vfo [a or b]
Reports frequency, step size, transmit offset, and mode.
Example: vfo
Example: vfo b";

const UP_HELP: &str = "up: Step the frequency up by one increment.
Syntax: up [a or b]
Example: up
Example: up b
Related commands: down, step, tune";

const DOWN_HELP: &str = "down: Step the frequency down by one increment.
Syntax: down [a or b]
Example: down
Example: down b
Related commands: up, step, tune";

const TUNE_HELP: &str = "tune: Tune a band to a specific frequency in megahertz.
Syntax: tune [a or b] [frequency in megahertz]
Example: tune a 146.520
Example: tune b 446.000
Related commands: freq, up, down";

const RECALL_HELP: &str = "recall: Recall a memory channel on a band.
Syntax: recall [a or b] [channel number]
Example: recall a 5
Related commands: channel, channels";

const CHANNEL_HELP: &str = "channel: Read a single memory channel by number.
Syntax: ch [channel number]
Example: ch 5
Related commands: channels, recall";

const CHANNELS_HELP: &str = "channels: List programmed memory channels in a range.
Syntax: channels [start] [end]
Default range is 0 through 19.
Example: channels
Example: channels 0 100
Related commands: channel, recall";

const STEP_HELP: &str = "step: Read or set the frequency step size.
Syntax: step [a or b] [index 0 through 11]
Index maps: 0=5, 1=6.25, 2=8.33, 3=9, 4=10, 5=12.5, 6=15 kilohertz.
Index maps: 7=20, 8=25, 9=30, 10=50, 11=100 kilohertz.
Example: step a 5
Related commands: up, down";

const GPS_HELP: &str = "gps: Set the GPS receiver and PC output configuration.
Syntax: gps [on or off] [on or off]
First argument controls the GPS receiver.
Second argument controls PC serial output of NMEA sentences.
Example: gps on on
Example: gps off off";

const URCALL_HELP: &str = "urcall: Read or set the D-STAR destination callsign.
Syntax: urcall [callsign] [suffix]
Example: urcall
Example: urcall W1AW
Example: urcall W1AW P
Related commands: cq, reflector";

const CQ_HELP: &str = "cq: Set the D-STAR destination to CQ CQ CQ.
Syntax: cq
Sets the urcall field to the general call destination.
Example: cq
Related commands: urcall";

const REFLECTOR_HELP: &str = "reflector: Connect to a D-STAR reflector.
Syntax: reflector [name] [module]
Example: reflector REF030 C
Related commands: unreflector, dstar";

const UNREFLECTOR_HELP: &str = "unreflector: Disconnect from the linked reflector.
Syntax: unreflector
Example: unreflector
Related commands: reflector";

const APRS_HELP: &str = "aprs: Enter or leave APRS packet radio mode.
Syntax: aprs start [callsign] [ssid]
Syntax: aprs stop
Example: aprs start W1AW 7
Related commands: beacon, msg, position";

const DSTAR_HELP: &str = "dstar: Enter or leave D-STAR gateway mode.
Syntax: dstar start [callsign] [reflector]
Syntax: dstar stop
Example: dstar start W1AW XRF030C
Related commands: link, unlink, echo";

const LAST_HELP: &str = "last: Re-announce previous output lines.
Syntax: last
Syntax: last [number]
Syntax: last all
Example: last
Example: last 5
Example: last all";

const STATUS_HELP: &str = "status: Dump the full radio state in one block.
Syntax: status
Reads model, firmware, battery, clock, lock, both bands' settings.
Takes 2 to 3 seconds because it issues many CAT commands.
Example: status";

const CHECK_HELP: &str = "check: Run the accessibility compliance self-check.
Syntax: check
Exercises every formatter, runs the lint, prints a report.
Example: check";

const VERBOSE_HELP: &str = "verbose: Toggle verbose monitor output.
Syntax: verbose
Syntax: verbose [on or off]
Syntax: quiet (same as verbose off)
Example: verbose off";

const CONFIRM_HELP: &str = "confirm: Toggle transmit-command confirmation.
Syntax: confirm [on or off]
When on, beacon, position, msg, cq, echo, and link prompt first.
Default is on. Disabling lasts for the current session.
Example: confirm off
Example: confirm on";

const HELP_HELP: &str = "help: Show help text.
Syntax: help
Syntax: help [command]
Syntax: help all
Example: help
Example: help freq";

const QUIT_HELP: &str = "quit: Exit the program.
Syntax: quit
Aliases: exit, q
Example: quit";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lint;

    #[test]
    fn all_commands_have_help() {
        for cmd in ALL_COMMANDS {
            assert!(
                for_command(cmd).is_some(),
                "command {cmd:?} is listed in ALL_COMMANDS but has no help entry",
            );
        }
    }

    #[test]
    fn every_help_text_passes_lint() {
        for cmd in ALL_COMMANDS {
            let text = for_command(cmd).expect("entry exists");
            if let Err(violations) = lint::check_output(text) {
                panic!(
                    "help for {cmd:?} violates accessibility rules: {violations:?}\ntext:\n{text}"
                );
            }
        }
    }

    #[test]
    fn unknown_command_returns_none() {
        assert_eq!(for_command("nosuchcommand"), None);
    }

    #[test]
    fn freq_help_includes_example() {
        let text = for_command("freq").expect("freq has help");
        assert!(text.contains("Example:"));
        assert!(text.contains("Syntax:"));
    }

    #[test]
    fn mode_help_texts_pass_lint() {
        lint::check_output(CAT_MODE_HELP).expect("CAT_MODE_HELP lints clean");
        lint::check_output(APRS_MODE_HELP).expect("APRS_MODE_HELP lints clean");
        lint::check_output(DSTAR_MODE_HELP).expect("DSTAR_MODE_HELP lints clean");
    }
}
