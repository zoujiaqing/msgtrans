#!/usr/bin/env bash
# Build static libraries for iOS device and Apple-silicon simulator and wrap
# them, with the generated C header and module map, in an XCFramework.
#
#   scripts/build-ios.sh            -> dist/ios/MsgtransFFI.xcframework
#
# Pair it with bindings/swift/MsgtransFFI.swift (scripts/gen-bindings.sh).
set -euo pipefail
cd "$(dirname "$0")/.."

TARGETS=(aarch64-apple-ios aarch64-apple-ios-sim)
for t in "${TARGETS[@]}"; do
  cargo build --release --target "$t"
done

scripts/gen-bindings.sh >/dev/null

HEADERS=dist/ios/headers
rm -rf "$HEADERS" dist/ios/MsgtransFFI.xcframework
mkdir -p "$HEADERS"
cp bindings/swift/msgtrans_ffiFFI.h "$HEADERS/"
cp bindings/swift/msgtrans_ffiFFI.modulemap "$HEADERS/module.modulemap"

args=()
for t in "${TARGETS[@]}"; do
  args+=(-library "target/$t/release/libmsgtrans_ffi.a" -headers "$HEADERS")
done
xcodebuild -create-xcframework "${args[@]}" -output dist/ios/MsgtransFFI.xcframework
find dist/ios/MsgtransFFI.xcframework -name '*.a' -exec ls -lh {} \;
