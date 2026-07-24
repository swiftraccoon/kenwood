#!/bin/sh
# macOS assesses each newly linked test executable with Gatekeeper and
# XProtect before dyld reaches main. Nextest normally starts two list
# queries per binary across every CPU, which can create dozens of
# simultaneous first-launch assessments and stall discovery.
#
# Hashing by binary path gives each binary's normal and --ignored query
# the same lock while allowing up to eight different binaries through
# at once. Eight keeps Gatekeeper busy on larger Macs without recreating
# nextest's default 18-binary first-launch storm.
# This wraps listing only; nextest's actual test execution keeps its full
# concurrency. lockf releases its kernel lock on every exit, including
# interruption, so stale slots cannot accumulate.
set -eu

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH='' cd -- "$script_dir/.." && pwd)
lock_dir="$repo_root/target/nextest/list-locks"
mkdir -p "$lock_dir"

checksum=$(printf '%s' "$1" | cksum)
checksum=${checksum%% *}
slot=$((checksum % 8))

exec /usr/bin/lockf -k "$lock_dir/slot-$slot.lock" "$@"
