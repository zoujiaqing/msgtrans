#!/usr/bin/env bash
# Build the Android shared library into a jniLibs/ layout.
#
#   ANDROID_NDK_HOME=/path/to/ndk scripts/build-android.sh [out-dir]
#
# arm64-v8a only, matching the apps' abiFilters (32-bit ARM devices are out of
# support since 2026-09-08). API level 24 matches their minSdk.
set -euo pipefail
cd "$(dirname "$0")/.."

: "${ANDROID_NDK_HOME:?set ANDROID_NDK_HOME to an installed NDK}"
OUT="${1:-dist/android/jniLibs}"
ABIS="${MSGTRANS_FFI_ABIS:-arm64-v8a}"

args=()
for abi in $ABIS; do args+=(-t "$abi"); done

cargo ndk --platform 24 "${args[@]}" -o "$OUT" build --release
find "$OUT" -name '*.so' -exec ls -lh {} \;
