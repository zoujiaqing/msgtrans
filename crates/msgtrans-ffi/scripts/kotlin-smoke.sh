#!/usr/bin/env bash
# Build the host library, regenerate bindings, start the test server, and run
# the Kotlin/JVM smoke test against it.
source "$(dirname "$0")/common.sh"

scripts/gen-bindings.sh >/dev/null
cargo build -p msgtrans-ffi --release --example test_server

LOG=$(mktemp)
"$TARGET_DIR/release/examples/test_server" >"$LOG" 2>&1 &
SERVER=$!
trap 'kill $SERVER 2>/dev/null || true' EXIT

for _ in $(seq 1 50); do grep -q '^READY' "$LOG" && break; sleep 0.1; done
read -r TCP QUIC PIN < <(sed -nE 's/^READY tcp_tls=([0-9]+) quic=([0-9]+) pin=(.*)$/\1 \2 \3/p' "$LOG")

# A private Gradle home keeps this run off the shared cache locks (an
# unrelated stuck daemon can hold them); dependencies already in the shared
# cache are still reused, read-only, when there is one.
RO_CACHE="${GRADLE_RO_DEP_CACHE:-$HOME/.gradle/caches}"
[ -d "$RO_CACHE/modules-2" ] && export GRADLE_RO_DEP_CACHE="$RO_CACHE"
(cd kotlin-smoke && GRADLE_USER_HOME="${GRADLE_USER_HOME_SMOKE:-$PWD/.gradle-home}" \
  "${GRADLE:-gradle}" --quiet smoke -PsmokeArgs="$TCP $QUIC $PIN" -PjnaLibraryPath="$TARGET_DIR/release")

echo "--- server log ---"
cat "$LOG"
grep -c '^SERVER_GOT client-answers' "$LOG" | grep -qx 2 \
  || { echo "server did not receive both client answers" >&2; exit 1; }
