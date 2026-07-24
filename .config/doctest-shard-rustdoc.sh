#!/bin/sh
# Cargo has no package-level doctest partitioning. Every shard therefore
# resolves the full workspace (preserving Cargo's exact cross-package
# feature unification), while this RUSTDOC wrapper executes each selected
# crate's `--test` command in exactly one stable hash shard.
set -eu

real_rustdoc=${KENWOOD_REAL_RUSTDOC:?KENWOOD_REAL_RUSTDOC is required}
shard_index=${KENWOOD_DOCTEST_SHARD_INDEX:?KENWOOD_DOCTEST_SHARD_INDEX is required}
shard_count=${KENWOOD_DOCTEST_SHARD_COUNT:?KENWOOD_DOCTEST_SHARD_COUNT is required}

case "$shard_index" in
    ''|*[!0-9]*) echo "invalid doctest shard '$shard_index/$shard_count'" >&2; exit 2 ;;
esac
case "$shard_count" in
    ''|*[!0-9]*) echo "invalid doctest shard '$shard_index/$shard_count'" >&2; exit 2 ;;
esac
if [ "$shard_count" -lt 1 ] || [ "$shard_index" -ge "$shard_count" ]; then
    echo "invalid doctest shard '$shard_index/$shard_count'" >&2
    exit 2
fi

is_test=0
crate_name=""
previous=""
for argument in "$@"; do
    if [ "$previous" = "--crate-name" ]; then
        crate_name=$argument
    fi
    case "$argument" in
        --test) is_test=1 ;;
        --crate-name=*) crate_name=${argument#--crate-name=} ;;
    esac
    previous=$argument
done

# Version probes and any future non-doctest rustdoc calls must remain
# transparent. Only Cargo's actual `rustdoc --test` units are sharded.
if [ "$is_test" -eq 0 ]; then
    exec "$real_rustdoc" "$@"
fi
if [ -z "$crate_name" ]; then
    echo "doctest shard wrapper received --test without --crate-name" >&2
    exit 2
fi

checksum=$(printf '%s' "$crate_name" | cksum)
checksum=${checksum%% *}
if [ $((checksum % shard_count)) -ne "$shard_index" ]; then
    exit 0
fi

exec "$real_rustdoc" "$@"
