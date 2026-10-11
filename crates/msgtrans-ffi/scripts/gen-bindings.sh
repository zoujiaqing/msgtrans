#!/usr/bin/env bash
# Generate Kotlin and Swift bindings from the compiled library's metadata.
#
#   scripts/gen-bindings.sh [out-dir]
source "$(dirname "$0")/common.sh"

OUT="${1:-bindings}"
# The default release profile: unstripped, so the UniFFI metadata the
# generator reads is intact.
cargo build -p msgtrans-ffi --release

for lang in kotlin swift; do
  cargo run --quiet --manifest-path uniffi-bindgen/Cargo.toml -- \
    generate --library "$HOST_LIB" --language "$lang" --no-format --out-dir "$OUT/$lang"
done
find "$OUT" -type f | sort
