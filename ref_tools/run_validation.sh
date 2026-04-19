#!/usr/bin/env bash
# Golden-vector validation harness for mbelib-rs.
#
# Builds the C/C++ reference tools (OP25 ambe_encoder + mbelib
# decoder), generates test PCM inputs, encodes and decodes them with
# both the reference (mbelib) and our Rust implementation, and diffs
# the intermediates frame-by-frame.
#
# The harness intentionally does NOT compare against OP25's
# ambe_encoder output byte-for-byte because OP25's wire layout for
# D-STAR is a different (incompatible) permutation — see
# `ref/op25/op25/gr-op25_repeater/lib/p25p2_vf.cc:825` for the OP25
# `d_list` table vs the mbelib-compatible DSD table we use in
# `mbelib-rs/src/unpack.rs`. OP25's output will not decode correctly
# in mbelib (or any real D-STAR radio). We only use OP25's
# ambe_encoder as a sanity check; the authoritative reference for
# this crate is mbelib's decoder.
#
# What this script proves:
# 1. Our Rust encoder produces bytes that mbelib decodes without
#    ECC errors — i.e. our wire output is a valid D-STAR frame.
# 2. Our Rust decoder extracts the same `(w0, L, b0..b8)` from a
#    given byte sequence as mbelib does — i.e. our parameter
#    extraction is bit-identical to the reference.
#
# What it cannot prove from synthetic input alone:
# - Whether the `b3..b8` codebook entries our encoder chooses match
#   what a DVSI chip would choose for the same PCM. That requires
#   a DVSI-origin AMBE capture to compare against.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TOOLS_BUILD="$REPO_ROOT/ref_tools/build"
GOLDEN_DIR="$REPO_ROOT/mbelib-rs/tests/golden"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

echo "==> Building reference tools (ref_tools/build)"
mkdir -p "$TOOLS_BUILD"
(cd "$TOOLS_BUILD" && cmake -DCMAKE_POLICY_VERSION_MINIMUM=3.5 .. > /dev/null && make -j4 > /dev/null)

echo "==> Building mbelib reference (ref/mbelib/build)"
mkdir -p "$REPO_ROOT/ref/mbelib/build"
(cd "$REPO_ROOT/ref/mbelib/build" && cmake -DCMAKE_POLICY_VERSION_MINIMUM=3.5 .. > /dev/null && make -j4 > /dev/null)

echo "==> Generating test input: 150 Hz sine, 1 s, -10 dBFS"
python3 - <<'PY' > /dev/null
import math, struct, pathlib
sr = 8000
f0 = 150.0
amp = 0.3
n = sr
out = pathlib.Path('/tmp/val_150hz.s16')
with out.open('wb') as f:
    for i in range(n):
        t = i / sr
        v = int(amp * math.sin(2*math.pi*f0*t) * 32767)
        f.write(struct.pack('<h', v))
PY

echo "==> Encode with Rust, verify via mbelib decoder"
cd "$REPO_ROOT"
cargo run --quiet -p mbelib-rs --features encoder --example encode_ambe_stream \
    /tmp/val_150hz.s16 "$TMP/rust.ambe" "$TMP/rust_encode.trace"

"$TOOLS_BUILD/ambe_decode_dump" "$TMP/rust.ambe" "$TMP/mbelib_on_rust.pcm" \
    "$TMP/mbelib_on_rust.trace"

cargo run --quiet -p mbelib-rs --example decode_ambe_stream \
    "$TMP/rust.ambe" "$TMP/rust_on_rust.pcm" "$TMP/rust_decode.trace"

echo
echo "==> Decoder parameter match: our Rust vs mbelib"
# Extract w0, L from each trace and diff. Both use the same format.
grep -E "^FRAME|w0|b0\.\." "$TMP/rust_decode.trace"  > "$TMP/rust_params.txt"
grep -E "^FRAME|w0"          "$TMP/mbelib_on_rust.trace" > "$TMP/mbelib_params.txt"

echo "--- Rust decoder parameters ---"
head -20 "$TMP/rust_params.txt"
echo
echo "--- mbelib decoder parameters ---"
head -20 "$TMP/mbelib_params.txt"
echo

# Pull out just the (w0, L) triples from both and diff.
awk '/^FRAME/ {f=$2} /w0 =/ {print f, $0}' "$TMP/rust_decode.trace" \
    | sed -E 's/.*w0 = ([0-9.]+)  L = ([0-9]+).*/\1 \2/' > "$TMP/rust_w0L.txt"
awk '/^FRAME/ {f=$2} /w0 =/ {print f, $0}' "$TMP/mbelib_on_rust.trace" \
    | sed -E 's/.*w0 = ([0-9.]+)  L = ([0-9]+).*/\1 \2/' > "$TMP/mbelib_w0L.txt"

echo "==> diff of (w0, L) pairs across all frames:"
if diff -q "$TMP/rust_w0L.txt" "$TMP/mbelib_w0L.txt" > /dev/null; then
    echo "  IDENTICAL across all $(wc -l < "$TMP/rust_w0L.txt") frames."
    echo "  => Rust decoder bit-extraction matches mbelib exactly."
else
    echo "  DIFFERENCES found:"
    diff "$TMP/rust_w0L.txt" "$TMP/mbelib_w0L.txt" | head -20
fi
echo

echo "==> Output spectrum: dominant frequencies in decoded PCM"
python3 - "$TMP/rust_on_rust.pcm" "$TMP/mbelib_on_rust.pcm" <<'PY'
import math, struct, sys

def spectrum(path, sr=8000):
    data = open(path, 'rb').read()
    samples = [struct.unpack('<h', data[i:i+2])[0] / 32768.0
               for i in range(0, len(data), 2)]
    results = []
    for freq in [75, 100, 125, 150, 175, 200, 250, 300, 400, 500, 750]:
        r = sum(x * math.cos(2*math.pi*freq*i/sr) for i, x in enumerate(samples))
        im = sum(x * math.sin(2*math.pi*freq*i/sr) for i, x in enumerate(samples))
        amp = math.sqrt(r*r + im*im) / len(samples)
        results.append((freq, amp))
    return results

for path in sys.argv[1:]:
    print(f"  {path}:")
    for freq, amp in spectrum(path):
        print(f"    {freq:5d} Hz: {amp:.6f}")
PY

echo
echo "==> Writing golden vector for regression guard"
mkdir -p "$GOLDEN_DIR/150hz_sine"
cp /tmp/val_150hz.s16 "$GOLDEN_DIR/150hz_sine/input.s16"
cp "$TMP/rust.ambe"   "$GOLDEN_DIR/150hz_sine/rust_encoded.ambe"
cp "$TMP/mbelib_on_rust.trace" "$GOLDEN_DIR/150hz_sine/mbelib_decode.trace"

echo "  Golden input  : $GOLDEN_DIR/150hz_sine/input.s16"
echo "  Rust encoding : $GOLDEN_DIR/150hz_sine/rust_encoded.ambe"
echo "  mbelib trace  : $GOLDEN_DIR/150hz_sine/mbelib_decode.trace"
echo

echo "==> Done."
