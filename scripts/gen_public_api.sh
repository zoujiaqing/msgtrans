#!/usr/bin/env bash
# Regenerate the committed public-API baseline (public-api.txt).
#
# The baseline is the frozen 2.0 crate-root surface plus `msgtrans::spi`. CI
# diffs the live surface against it (see .github/workflows/ci.yml, job
# `public-api`) and fails on any unrecorded change, so widening or breaking the
# public API is impossible without updating this file in the same commit.
#
# Requires the PINNED nightly (rustdoc JSON format changes between nightlies,
# so a floating toolchain would diff for reasons unrelated to the API) and
# cargo-public-api at the version CI pins (.github/workflows/ci.yml):
#   rustup toolchain install nightly-2026-07-23
#   cargo install cargo-public-api --locked --version 0.52.0
#
# Bump NIGHTLY here and in .github/workflows/ci.yml together, regenerating the
# baseline in the same commit.
#
# Run from the crate root:
#   ./scripts/gen_public_api.sh
set -euo pipefail

cd "$(dirname "$0")/.."

NIGHTLY="${MSGTRANS_PUBLIC_API_NIGHTLY:-nightly-2026-07-23}"

if ! rustup toolchain list | grep -q "^${NIGHTLY}"; then
  echo "error: toolchain ${NIGHTLY} is not installed." >&2
  echo "       rustup toolchain install ${NIGHTLY}" >&2
  exit 1
fi

# All protocols enabled: the frozen surface is documented for the full build.
# `-s` omits blanket impls (e.g. `impl<T> Any for T`) to keep the baseline
# stable across rustc versions while still tracking every real API item.
cargo "+${NIGHTLY}" public-api --all-features -s > public-api.txt
echo "Wrote public-api.txt (toolchain ${NIGHTLY})"
