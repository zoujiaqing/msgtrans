#!/usr/bin/env bash
# Compile the generated Swift bindings on the macOS host and run them against
# the test server over pinned QUIC.
set -euo pipefail
cd "$(dirname "$0")/.."

scripts/gen-bindings.sh >/dev/null
cargo build --release --example test_server

OUT=target/swift-smoke
mkdir -p "$OUT"
swiftc -parse-as-library -O \
  -I bindings/swift \
  -Xcc -fmodule-map-file=bindings/swift/msgtrans_ffiFFI.modulemap \
  -L target/release -lmsgtrans_ffi \
  bindings/swift/MsgtransFFI.swift swift-smoke/main.swift \
  -o "$OUT/smoke"

LOG=$(mktemp)
target/release/examples/test_server >"$LOG" 2>&1 &
SERVER=$!
trap 'kill $SERVER 2>/dev/null || true' EXIT
for _ in $(seq 1 50); do grep -q '^READY' "$LOG" && break; sleep 0.1; done
read -r QUIC PIN < <(sed -nE 's/^READY tcp_tls=[0-9]+ quic=([0-9]+) pin=(.*)$/\1 \2/p' "$LOG")

DYLD_LIBRARY_PATH=target/release "$OUT/smoke" "$QUIC" "$PIN"
echo "--- server log ---"
cat "$LOG"
grep -q '^SERVER_GOT client-answers' "$LOG" || { echo "server got no answer" >&2; exit 1; }
