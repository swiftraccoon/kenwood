#!/usr/bin/env bash
# APRS hardware-validation phase runner for the TH-D75 REPL.
#
# Feeds thd75-repl a paced command stream over a FIFO: sleeps between
# transmit commands and a SIGINT to end monitor/igate loops (the REPL's
# Ctrl-C handler returns to the prompt). Output is teed to a log file.
#
# Environment:
#   APRS_CALL      operator callsign, no SSID (required, all phases)
#   APRS_LAT       beacon latitude, decimal degrees (phase2/phase4igate/phase5)
#   APRS_LON       beacon longitude, decimal degrees (phase2/phase4igate/phase5)
#   APRS_WX_QUERY  WXBOT query text, e.g. a US zip code (phase3)
#
# Usage: run.sh <phase1|phase2|phase3|phase4digi|phase4igate|phase5>
#
# Every phase tunes band A to 144.390 MHz before entering APRS mode.
# Transmitting phases run the REPL with --yes because script-mode input
# cannot answer the confirmation prompt: launching a phase IS the
# transmit authorization. Start the witness in another terminal first:
#   cargo run -p aprs-is --example monitor -- "$APRS_CALL" | tee witness.log

set -euo pipefail

phase="${1:?usage: run.sh <phase1|phase2|phase3|phase4digi|phase4igate|phase5>}"
call="${APRS_CALL:?set APRS_CALL to your callsign, for example APRS_CALL=W1AW}"

repo_root="$(cd "$(dirname "$0")/../../.." && pwd)"
log_dir="$repo_root/.aprs-validation-logs"
mkdir -p "$log_dir"
log_file="$log_dir/$phase-$(date -u +%Y%m%d-%H%M%S).log"

cargo build -q -p thd75-repl
repl_bin="$repo_root/target/debug/thd75-repl"

fifo="$(mktemp -u)"
mkfifo "$fifo"
cleanup() { rm -f "$fifo"; }
trap cleanup EXIT

# Process substitution (not a pipeline) keeps the REPL the direct
# child so $! is the PID we can SIGINT.
# --trace captures a per-session packet-level log (see the REPL's log
# directory) so phase scoring can verify decode TYPES (Mic-E, weather),
# not just station callsigns.
"$repl_bin" --yes --timestamps --trace < "$fifo" > >(tee "$log_file") 2>&1 &
repl_pid=$!
exec 3>"$fifo"

say() { printf '%s\n' "$1" >&3; }
interrupt_repl() {
  kill -INT "$repl_pid"
  sleep 2
}
preamble() {
  # Set a 5 kHz step FIRST: 144.390 is on a 5 kHz grid but a coarse
  # step (e.g. 25 kHz) floors the VFO write to 144.375, putting every
  # phase 15 kHz off the APRS frequency. Verified on hardware.
  say "step a 0"
  say "tune a 144.390"
  say "freq a"
}

phase1() { # RX soak: 20 minutes of live decode, then the station list.
  preamble
  say "aprs start $call 7"
  say "monitor"
  sleep 1200
  interrupt_repl
  say "stations"
  say "aprs stop"
  say "quit"
}

phase2() { # TX beacons: every format, 30-second spacing.
  lat="${APRS_LAT:?set APRS_LAT in decimal degrees}"
  lon="${APRS_LON:?set APRS_LON in decimal degrees}"
  preamble
  say "aprs start $call 7"
  say "position $lat $lon REPL validation uncompressed"
  sleep 30
  say "compressed $lat $lon REPL validation compressed"
  sleep 30
  say "mice $lat $lon 25 90 REPL validation mice"
  sleep 30
  say "status REPL validation status"
  sleep 30
  say "object TESTOBJ $lat $lon REPL validation object"
  sleep 30
  say "aprs stop"
  say "quit"
}

phase3() { # Messaging: WXBOT round-trip plus expiry to a silent call.
  wx="${APRS_WX_QUERY:?set APRS_WX_QUERY, for example a US zip code}"
  preamble
  say "aprs start $call 7"
  say "msg WXBOT $wx"
  say "msg N0CALL expiry test"
  say "monitor"
  sleep 600
  interrupt_repl
  say "aprs stop"
  say "quit"
}

phase4digi() { # WIDE1-1 fill-in digipeater, time-boxed to 10 minutes.
  preamble
  say "aprs start $call 7 digi"
  say "monitor"
  sleep 600
  interrupt_repl
  say "aprs stop"
  say "quit"
}

phase4igate() { # IGate RF<->IS, time-boxed to 10 minutes.
  lat="${APRS_LAT:?set APRS_LAT in decimal degrees}"
  lon="${APRS_LON:?set APRS_LON in decimal degrees}"
  preamble
  say "aprs start $call 7"
  say "igate r/$lat/$lon/50"
  sleep 600
  interrupt_repl
  say "aprs stop"
  say "quit"
}

phase5() { # SmartBeaconing: first beacon, fast-rate interval, corner peg.
  lat="${APRS_LAT:?set APRS_LAT in decimal degrees}"
  lon="${APRS_LON:?set APRS_LON in decimal degrees}"
  preamble
  say "aprs start $call 7"
  # t=0: first motion sample at highway speed -> immediate first beacon.
  say "motion $lat $lon 80 0"
  # Same speed and heading: nothing due until the fast rate elapses
  # (180 seconds at or above 70 km/h with TH-D75 default settings).
  for _ in 1 2 3 4 5; do
    sleep 30
    say "motion $lat $lon 80 0"
  done
  sleep 35
  # ~215 s elapsed: past the fast rate -> time-expired beacon.
  say "motion $lat $lon 80 0"
  sleep 20
  # 90-degree turn at speed -> corner-peg beacon.
  say "motion $lat $lon 80 90"
  sleep 10
  say "aprs stop"
  say "quit"
}

case "$phase" in
  phase1) phase1 ;;
  phase2) phase2 ;;
  phase3) phase3 ;;
  phase4digi) phase4digi ;;
  phase4igate) phase4igate ;;
  phase5) phase5 ;;
  *)
    echo "unknown phase: $phase" >&2
    exit 2
    ;;
esac

exec 3>&-
wait "$repl_pid"
echo "phase $phase complete. Log: $log_file"
