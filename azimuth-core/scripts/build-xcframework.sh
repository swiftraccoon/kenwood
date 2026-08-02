#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-2.0-or-later
#
# Build AzimuthCore.xcframework for iPadOS devices, iPadOS simulators, and
# native macOS. Install targets with:
# rustup target add aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios \
#   aarch64-apple-darwin x86_64-apple-darwin

set -euo pipefail

# Match the Azimuth app targets so native Objective-C shim objects do not
# inherit the newer host SDK version when this script runs on a beta Xcode.
export MACOSX_DEPLOYMENT_TARGET=26.0
export IPHONEOS_DEPLOYMENT_TARGET=26.0

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CRATE_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
WORKSPACE_DIR="$(cd "${CRATE_DIR}/.." && pwd)"
OUTPUT_DIR="${WORKSPACE_DIR}/azimuth/AzimuthCore.xcframework"
SWIFT_SOURCES_DIR="${WORKSPACE_DIR}/azimuth/Generated"
CACHE_DIR="${CRATE_DIR}/target/xcframework-cache"
CACHE_KEY_FILE="${CACHE_DIR}/key.sha256"
CACHE_MANIFEST_FILE="${CACHE_DIR}/outputs.sha256"
BINDGEN_TARGET_DIR="${WORKSPACE_DIR}/target/native-bindgen/azimuth"
LIB_NAME="libazimuth_core.a"

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
        "${CRATE_DIR}/src/bin/uniffi-bindgen.rs"
        "${CRATE_DIR}/uniffi.toml"
        "${BASH_SOURCE[0]}"
    )

    {
        printf 'azimuth-xcframework-cache-v1\n'
        printf 'library:%s\n' "${LIB_NAME}"
        printf 'macos-deployment:%s\n' "${MACOSX_DEPLOYMENT_TARGET}"
        printf 'ios-deployment:%s\n' "${IPHONEOS_DEPLOYMENT_TARGET}"
        printf 'bindgen-features:bindgen-cli\n'

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
            '^(AR|CC|CFLAGS|CXX|CXXFLAGS|DEVELOPER_DIR|RANLIB|RUSTC|RUSTC_WRAPPER|RUSTC_WORKSPACE_WRAPPER|RUSTFLAGS|CARGO_ENCODED_RUSTFLAGS|CARGO_PROFILE_[A-Z0-9_]+|CARGO_TARGET_[A-Z0-9_]+_RUSTFLAGS|SDKROOT)=' \
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
    printf 'azimuth-xcframework-outputs-v1\n'
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

echo "==> Building Azimuth core for Apple targets"
cd "${WORKSPACE_DIR}"
for target in "${TARGETS[@]}"; do
    echo "--> ${target}"
    cargo build -p azimuth-core --lib --release --target "${target}"
done

mkdir -p "${CACHE_DIR}"
CACHE_KEY=$(compute_cache_key)
if cache_matches "${CACHE_KEY}"; then
    echo "==> Reusing cached AzimuthCore.xcframework"
    exit 0
fi

mkdir -p "${CRATE_DIR}/target"
BUILD_DIR=$(mktemp -d "${CRATE_DIR}/target/xcframework-build.XXXXXX")
STAGING_DIR="${BUILD_DIR}/staging"
TEMP_OUTPUT_DIR="${BUILD_DIR}/AzimuthCore.xcframework"
TEMP_SWIFT_SOURCES_DIR="${BUILD_DIR}/Generated"

cleanup() {
    rm -rf "${BUILD_DIR}"
}
trap cleanup EXIT

echo "==> Staging Apple libraries"
mkdir -p \
    "${STAGING_DIR}/ios-device/Headers" \
    "${STAGING_DIR}/ios-simulator/Headers" \
    "${STAGING_DIR}/macos/Headers" \
    "${STAGING_DIR}/generated"

cp "${WORKSPACE_DIR}/target/aarch64-apple-ios/release/${LIB_NAME}" \
    "${STAGING_DIR}/ios-device/${LIB_NAME}"
lipo -create \
    "${WORKSPACE_DIR}/target/aarch64-apple-ios-sim/release/${LIB_NAME}" \
    "${WORKSPACE_DIR}/target/x86_64-apple-ios/release/${LIB_NAME}" \
    -output "${STAGING_DIR}/ios-simulator/${LIB_NAME}"
lipo -create \
    "${WORKSPACE_DIR}/target/aarch64-apple-darwin/release/${LIB_NAME}" \
    "${WORKSPACE_DIR}/target/x86_64-apple-darwin/release/${LIB_NAME}" \
    -output "${STAGING_DIR}/macos/${LIB_NAME}"

echo "==> Generating Swift bindings from compiled UniFFI metadata"
env CARGO_TARGET_DIR="${BINDGEN_TARGET_DIR}" \
    cargo run --manifest-path "${CRATE_DIR}/Cargo.toml" \
    --features bindgen-cli --bin azimuth-uniffi-bindgen -- generate \
    --library "${STAGING_DIR}/ios-device/${LIB_NAME}" \
    --config "${CRATE_DIR}/uniffi.toml" \
    --language swift \
    --out-dir "${STAGING_DIR}/generated"

for slice in ios-device ios-simulator macos; do
    cp "${STAGING_DIR}/generated"/*.h "${STAGING_DIR}/${slice}/Headers/"
    for module_map in "${STAGING_DIR}/generated"/*.modulemap; do
        cp "${module_map}" "${STAGING_DIR}/${slice}/Headers/module.modulemap"
    done
done

echo "==> Creating AzimuthCore.xcframework"
xcodebuild -create-xcframework \
    -library "${STAGING_DIR}/ios-device/${LIB_NAME}" \
    -headers "${STAGING_DIR}/ios-device/Headers" \
    -library "${STAGING_DIR}/ios-simulator/${LIB_NAME}" \
    -headers "${STAGING_DIR}/ios-simulator/Headers" \
    -library "${STAGING_DIR}/macos/${LIB_NAME}" \
    -headers "${STAGING_DIR}/macos/Headers" \
    -output "${TEMP_OUTPUT_DIR}"

echo "==> Copying generated Swift API"
mkdir -p "${TEMP_SWIFT_SOURCES_DIR}"
cp "${STAGING_DIR}/generated"/*.swift "${TEMP_SWIFT_SOURCES_DIR}/"

# Both trees are complete before either live output is replaced. The cache
# key is updated only after both replacements succeed.
rm -rf "${OUTPUT_DIR}" "${SWIFT_SOURCES_DIR}"
mv "${TEMP_OUTPUT_DIR}" "${OUTPUT_DIR}"
mv "${TEMP_SWIFT_SOURCES_DIR}" "${SWIFT_SOURCES_DIR}"
update_cache "${CACHE_KEY}"

echo "==> Built ${OUTPUT_DIR}"
