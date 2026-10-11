#!/usr/bin/env bash
# Generate Kotlin and Swift bindings from the compiled library's metadata.
#
#   scripts/gen-bindings.sh [out-dir]
set -euo pipefail
cd "$(dirname "$0")/.."

OUT="${1:-bindings}"
cargo build --release
case "$(uname -s)" in
  Darwin) LIB=target/release/libmsgtrans_ffi.dylib ;;
  *)      LIB=target/release/libmsgtrans_ffi.so ;;
esac

for lang in kotlin swift; do
  cargo run --quiet --manifest-path uniffi-bindgen/Cargo.toml -- \
    generate --library "$LIB" --language "$lang" --no-format --out-dir "$OUT/$lang"
done
find "$OUT" -type f | sort
