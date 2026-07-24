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
SWIFT_SOURCES_DIR="${WORKSPACE_DIR}/lodestar/Generated"
CACHE_DIR="${CRATE_DIR}/target/xcframework-cache"
CACHE_KEY_FILE="${CACHE_DIR}/key.sha256"
CACHE_MANIFEST_FILE="${CACHE_DIR}/outputs.sha256"
BINDGEN_TARGET_DIR="${WORKSPACE_DIR}/target/native-bindgen/lodestar"
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

archive_path() {
    local target=$1
    printf '%s/target/%s/release/%s\n' \
        "${WORKSPACE_DIR}" "${target}" "${LIB_NAME}"
}

file_digest() {
    shasum -a 256 "$1" | awk '{print $1}'
}

emit_file_input() {
    local label=$1
    local path=$2

    if [[ -f "${path}" ]]; then
        printf 'file:%s:%s\n' "${label}" "$(file_digest "${path}")"
    else
        printf 'file:%s:MISSING\n' "${label}"
    fi
}

compute_cache_key() {
    local sdk target
    local input_files=(
        "${WORKSPACE_DIR}/Cargo.toml"
        "${WORKSPACE_DIR}/Cargo.lock"
        "${WORKSPACE_DIR}/rust-toolchain.toml"
        "${WORKSPACE_DIR}/.cargo/config"
        "${WORKSPACE_DIR}/.cargo/config.toml"
        "${CRATE_DIR}/Cargo.toml"
        "${CRATE_DIR}/build.rs"
        "${CRATE_DIR}/src/bin/uniffi-bindgen.rs"
        "${CRATE_DIR}/src/lodestar.udl"
        "${CRATE_DIR}/uniffi.toml"
        "${BASH_SOURCE[0]}"
    )

    {
        printf 'lodestar-xcframework-cache-v1\n'
        printf 'library:%s\n' "${LIB_NAME}"
        printf 'bindgen-features:default\n'

        for target in "${TARGETS[@]}"; do
            printf 'target:%s\n' "${target}"
            emit_file_input \
                "archive-${target}" \
                "$(archive_path "${target}")"
        done

        for target in "${input_files[@]}"; do
            emit_file_input "${target#"${WORKSPACE_DIR}/"}" "${target}"
        done

        printf '%s\n' 'tool:rustc'
        rustc -vV
        printf '%s\n' 'tool:cargo'
        cargo -Vv
        printf '%s\n' 'tool:xcodebuild'
        xcodebuild -version
        printf 'developer-dir:%s\n' "$(xcode-select -p)"
        printf 'lipo:%s\n' "$(xcrun --find lipo)"
        for sdk in iphoneos iphonesimulator macosx; do
            printf 'sdk:%s:path:%s\n' \
                "${sdk}" "$(xcrun --sdk "${sdk}" --show-sdk-path)"
            printf 'sdk:%s:build:%s\n' \
                "${sdk}" \
                "$(xcrun --sdk "${sdk}" --show-sdk-build-version)"
        done

        env | LC_ALL=C sort | grep -E \
            '^(AR|CC|CFLAGS|CXX|CXXFLAGS|DEVELOPER_DIR|IPHONEOS_DEPLOYMENT_TARGET|MACOSX_DEPLOYMENT_TARGET|RANLIB|RUSTC|RUSTC_WRAPPER|RUSTC_WORKSPACE_WRAPPER|RUSTFLAGS|CARGO_ENCODED_RUSTFLAGS|CARGO_PROFILE_[A-Z0-9_]+|CARGO_TARGET_[A-Z0-9_]+_RUSTFLAGS|SDKROOT)=' \
            || true
    } | shasum -a 256 | awk '{print $1}'
}

emit_tree_manifest() {
    local root=$1
    local label=$2
    local file relative

    while IFS= read -r file; do
        relative=${file#"${root}/"}
        printf '%s:%s:%s\n' \
            "${label}" "${relative}" "$(file_digest "${file}")"
    done < <(find "${root}" -type f ! -name '.DS_Store' -print | LC_ALL=C sort)
}

write_output_manifest() {
    printf 'lodestar-xcframework-outputs-v1\n'
    emit_tree_manifest "${OUTPUT_DIR}" xcframework
    emit_tree_manifest "${SWIFT_SOURCES_DIR}" swift
}

cache_matches() {
    local expected_key=$1
    local cached_key verify_manifest

    [[ -f "${CACHE_KEY_FILE}" ]] || return 1
    [[ -f "${CACHE_MANIFEST_FILE}" ]] || return 1
    [[ -d "${OUTPUT_DIR}" ]] || return 1
    [[ -d "${SWIFT_SOURCES_DIR}" ]] || return 1
    IFS= read -r cached_key < "${CACHE_KEY_FILE}"
    [[ "${cached_key}" == "${expected_key}" ]] || return 1

    verify_manifest=$(mktemp "${CACHE_DIR}/verify.XXXXXX")
    if ! write_output_manifest > "${verify_manifest}"; then
        rm -f "${verify_manifest}"
        return 1
    fi
    if cmp -s "${verify_manifest}" "${CACHE_MANIFEST_FILE}"; then
        rm -f "${verify_manifest}"
        return 0
    fi
    rm -f "${verify_manifest}"
    return 1
}

update_cache() {
    local cache_key=$1
    local manifest_tmp key_tmp

    manifest_tmp=$(mktemp "${CACHE_DIR}/outputs.XXXXXX")
    key_tmp=$(mktemp "${CACHE_DIR}/key.XXXXXX")
    if ! write_output_manifest > "${manifest_tmp}"; then
        rm -f "${manifest_tmp}" "${key_tmp}"
        return 1
    fi
    printf '%s\n' "${cache_key}" > "${key_tmp}"

    # The key is the commit marker, so replace it only after the matching
    # output manifest is durable at its final path.
    mv -f "${manifest_tmp}" "${CACHE_MANIFEST_FILE}"
    mv -f "${key_tmp}" "${CACHE_KEY_FILE}"
}

for target in "${TARGETS[@]}"; do
    echo "--> cargo build --release --target ${target}"
    cargo build -p lodestar-core --release --target "${target}"
done

mkdir -p "${CACHE_DIR}"
CACHE_KEY=$(compute_cache_key)
if cache_matches "${CACHE_KEY}"; then
    echo "==> Reusing cached LodestarKit.xcframework"
    exit 0
fi

mkdir -p "${CRATE_DIR}/target"
BUILD_DIR=$(mktemp -d "${CRATE_DIR}/target/xcframework-build.XXXXXX")
STAGING_DIR="${BUILD_DIR}/staging"
TEMP_OUTPUT_DIR="${BUILD_DIR}/LodestarKit.xcframework"
TEMP_SWIFT_SOURCES_DIR="${BUILD_DIR}/Generated"

cleanup() {
    rm -rf "${BUILD_DIR}"
}
trap cleanup EXIT

echo "==> Staging fat libraries"
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
env CARGO_TARGET_DIR="${BINDGEN_TARGET_DIR}" \
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
xcodebuild -create-xcframework \
    -library "${STAGING_DIR}/ios-device/${LIB_NAME}" -headers "${STAGING_DIR}/ios-device/Headers" \
    -library "${STAGING_DIR}/ios-sim/${LIB_NAME}" -headers "${STAGING_DIR}/ios-sim/Headers" \
    -library "${STAGING_DIR}/macos/${LIB_NAME}" -headers "${STAGING_DIR}/macos/Headers" \
    -output "${TEMP_OUTPUT_DIR}"

echo "==> Copying generated Swift sources into lodestar/Generated/"
mkdir -p "${TEMP_SWIFT_SOURCES_DIR}"
cp "${HEADERS_DIR}"/*.swift "${TEMP_SWIFT_SOURCES_DIR}/"

# Both trees are complete before either live output is replaced. The cache
# key is updated only after both replacements succeed.
rm -rf "${OUTPUT_DIR}" "${SWIFT_SOURCES_DIR}"
mv "${TEMP_OUTPUT_DIR}" "${OUTPUT_DIR}"
mv "${TEMP_SWIFT_SOURCES_DIR}" "${SWIFT_SOURCES_DIR}"
update_cache "${CACHE_KEY}"

echo "==> Done: ${OUTPUT_DIR}"
