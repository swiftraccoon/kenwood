#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Swift Raccoon
# SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later
#
# Build lodestar-core for all Apple targets and produce
# LodestarKit.xcframework at lodestar/LodestarKit.xcframework.
#
# Slices:
#   - ios-arm64                      (iPhone / iPad device)
#   - ios-arm64_x86_64-simulator     (iPhone / iPad simulator)
#   - macos-arm64_x86_64             (native macOS app)
#
# Prerequisites:
#   rustup target add aarch64-apple-ios aarch64-apple-ios-sim \
#     x86_64-apple-ios aarch64-apple-darwin x86_64-apple-darwin
#
# Note: Mac Catalyst slices (aarch64-apple-ios-macabi, x86_64-apple-ios-macabi)
# are intentionally omitted. IOBluetoothDevice is unavailable on Catalyst,
# so the Mac build uses a native macOS target instead.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CRATE_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
WORKSPACE_DIR="$(cd "${CRATE_DIR}/.." && pwd)"
OUTPUT_DIR="${WORKSPACE_DIR}/lodestar/LodestarKit.xcframework"
STAGING_DIR="${CRATE_DIR}/target/xcframework-staging"
LIB_NAME="liblodestar_core.a"

echo "==> Building Rust static libraries for Apple targets"
cd "${WORKSPACE_DIR}"

TARGETS=(
    aarch64-apple-ios
    aarch64-apple-ios-sim
    x86_64-apple-ios
    aarch64-apple-darwin
    x86_64-apple-darwin
)

for target in "${TARGETS[@]}"; do
    echo "--> cargo build --release --target ${target}"
    cargo build -p lodestar-core --release --target "${target}"
done

echo "==> Staging fat libraries"
rm -rf "${STAGING_DIR}"
mkdir -p "${STAGING_DIR}/ios-device" "${STAGING_DIR}/ios-sim" "${STAGING_DIR}/macos"

cp "${WORKSPACE_DIR}/target/aarch64-apple-ios/release/${LIB_NAME}" \
   "${STAGING_DIR}/ios-device/${LIB_NAME}"

lipo -create \
    "${WORKSPACE_DIR}/target/aarch64-apple-ios-sim/release/${LIB_NAME}" \
    "${WORKSPACE_DIR}/target/x86_64-apple-ios/release/${LIB_NAME}" \
    -output "${STAGING_DIR}/ios-sim/${LIB_NAME}"

lipo -create \
    "${WORKSPACE_DIR}/target/aarch64-apple-darwin/release/${LIB_NAME}" \
    "${WORKSPACE_DIR}/target/x86_64-apple-darwin/release/${LIB_NAME}" \
    -output "${STAGING_DIR}/macos/${LIB_NAME}"

echo "==> Generating Swift bindings"
HEADERS_DIR="${STAGING_DIR}/headers"
mkdir -p "${HEADERS_DIR}"

# Library mode: reads the metadata embedded in the compiled static lib,
# which combines UDL-derived items (via include_scaffolding!) and
# proc-macro-derived items (#[uniffi::export], #[derive(uniffi::Object)],
# etc.). This is required for mixed UDL + proc-macro crates like ours.
cargo run --manifest-path "${CRATE_DIR}/Cargo.toml" \
    --bin uniffi-bindgen -- generate \
    --library "${STAGING_DIR}/ios-device/${LIB_NAME}" \
    --config "${CRATE_DIR}/uniffi.toml" \
    --language swift \
    --out-dir "${HEADERS_DIR}"

# Move generated .h into each slice's Headers dir and rename the
# modulemap to the conventional `module.modulemap` so Xcode auto-discovers
# the C module without a custom `MODULEMAP_FILE` build setting.
for slice in ios-device ios-sim macos; do
    mkdir -p "${STAGING_DIR}/${slice}/Headers"
    cp "${HEADERS_DIR}"/*.h "${STAGING_DIR}/${slice}/Headers/"
    for mm in "${HEADERS_DIR}"/*.modulemap; do
        cp "${mm}" "${STAGING_DIR}/${slice}/Headers/module.modulemap"
    done
done

echo "==> Creating xcframework"
rm -rf "${OUTPUT_DIR}"
mkdir -p "$(dirname "${OUTPUT_DIR}")"
xcodebuild -create-xcframework \
    -library "${STAGING_DIR}/ios-device/${LIB_NAME}" -headers "${STAGING_DIR}/ios-device/Headers" \
    -library "${STAGING_DIR}/ios-sim/${LIB_NAME}" -headers "${STAGING_DIR}/ios-sim/Headers" \
    -library "${STAGING_DIR}/macos/${LIB_NAME}" -headers "${STAGING_DIR}/macos/Headers" \
    -output "${OUTPUT_DIR}"

echo "==> Copying generated Swift sources into lodestar/Generated/"
SWIFT_SOURCES_DIR="${WORKSPACE_DIR}/lodestar/Generated"
mkdir -p "${SWIFT_SOURCES_DIR}"
cp "${HEADERS_DIR}"/*.swift "${SWIFT_SOURCES_DIR}/"

echo "==> Done: ${OUTPUT_DIR}"
