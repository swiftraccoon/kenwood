#!/usr/bin/env bash
# Persistent TX-reach attempt: beacon periodically and stop at first proof
# that the world heard us.
#
# Every cycle: connect, log battery, beacon twice (uncompressed + Mic-E,
# WIDE1-1,WIDE2-1 path), listen briefly for our own digipeated echo, exit
# cleanly, release the radio, sleep. A continuously reconnecting APRS-IS
# witness watches for any igate gating our callsign. Success (either a
# witness line from us, or hearing our own call back from a digi) writes
# a marker file and ends the loop.
#
# Environment:
#   APRS_CALL               operator callsign, no SSID (required)
#   APRS_LAT / APRS_LON     beacon position, decimal degrees (required)
#   APRS_TX_INTERVAL_SECS   pause between cycles (default 600)
#   APRS_TX_MAX_CYCLES      give up after this many cycles (default 36)
#
# Usage: tx-persist.sh   (transmits every cycle; launching it IS the
# authorization, same policy as run.sh)

set -euo pipefail

call="${APRS_CALL:?set APRS_CALL to your callsign}"
lat="${APRS_LAT:?set APRS_LAT in decimal degrees}"
lon="${APRS_LON:?set APRS_LON in decimal degrees}"
interval="${APRS_TX_INTERVAL_SECS:-600}"
max_cycles="${APRS_TX_MAX_CYCLES:-36}"

repo_root="$(cd "$(dirname "$0")/../../.." && pwd)"
cd "$repo_root"
log_dir="$repo_root/.aprs-validation-logs"
mkdir -p "$log_dir"
marker="$log_dir/tx-reached.marker"
witness_log="$log_dir/tx-persist-witness.log"
cycle_log="$log_dir/tx-persist.log"
rm -f "$marker"

note() { printf '[%s] %s\n' "$(date -u +%H:%M:%S)" "$1" >> "$cycle_log"; }

cargo build -q -p thd75-repl
repl_bin="$repo_root/target/debug/thd75-repl"

# Continuously reconnecting witness. `cargo run -p` resolves the
# example-name collision with the thd75 crate; nothing else builds
# during the loop, so the binary stays correct.
witness_loop() {
  while [ ! -f "$marker" ]; do
    cargo run -q -p aprs-is --example monitor -- "$call" >> "$witness_log" 2>&1 || true
    sleep 10
  done
}
witness_loop &
witness_pid=$!
cleanup() { kill "$witness_pid" 2>/dev/null || true; }
trap cleanup EXIT

beacon_cycle() {
  n="$1"
  fifo="$(mktemp -u)"
  mkfifo "$fifo"
  "$repl_bin" --yes --timestamps < "$fifo" >> "$cycle_log" 2>&1 &
  rpid=$!
  # Watchdog: no cycle may hold the radio longer than 150 s.
  ( sleep 150 && kill -TERM "$rpid" 2>/dev/null ) &
  watchdog=$!
  exec 3>"$fifo"
  say() { printf '%s\n' "$1" >&3; }
  say "battery"
  say "step a 0"
  say "tune a 144.390"
  say "aprs start $call a 7"
  sleep 2
  say "position $lat $lon reach test $n"
  sleep 3
  say "mice $lat $lon 0 0 reach test $n"
  # Brief listen: if a digi repeats us we hear our own callsign back.
  say "monitor"
  sleep 45
  kill -INT "$rpid" 2>/dev/null || true
  sleep 2
  say "aprs stop"
  say "quit"
  exec 3>&-
  wait "$rpid" 2>/dev/null || true
  kill "$watchdog" 2>/dev/null || true
  rm -f "$fifo"
}

note "tx-persist starting: $max_cycles cycles, ${interval}s apart, as $call-7"
for n in $(seq 1 "$max_cycles"); do
  if grep -qE "^\[[0-9:]+\] $call-7>" "$witness_log" 2>/dev/null; then
    echo "igated: $(grep -E "^\[[0-9:]+\] $call-7>" "$witness_log" | head -1)" > "$marker"
    note "SUCCESS before cycle $n: an igate heard us"
    break
  fi
  note "cycle $n: beaconing"
  beacon_cycle "$n"
  if grep -q "station heard: $call" "$cycle_log"; then
    echo "digi echo: heard our own call back from a digipeater" > "$marker"
    note "SUCCESS at cycle $n: digi echo received"
    break
  fi
  sleep "$interval"
done

if [ ! -f "$marker" ]; then
  note "gave up after $max_cycles cycles with no network reach"
fi
