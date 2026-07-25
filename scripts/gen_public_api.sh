#!/usr/bin/env bash
# Regenerate the committed public-API baseline (public-api.txt).
#
# The baseline is the frozen 2.0 crate-root surface plus `msgtrans::spi`. CI
# diffs the live surface against it (see .github/workflows/ci.yml, job
# `public-api`) and fails on any unrecorded change, so widening or breaking the
# public API is impossible without updating this file in the same commit.
#
# Requires a nightly toolchain (for rustdoc JSON) and cargo-public-api at the
# version CI pins (.github/workflows/ci.yml) so the format matches the baseline:
#   rustup toolchain install nightly
#   cargo install cargo-public-api --locked --version 0.52.0
#
# Run from the crate root:
#   ./scripts/gen_public_api.sh
set -euo pipefail

cd "$(dirname "$0")/.."

# All protocols enabled: the frozen surface is documented for the full build.
# `-s` omits blanket impls (e.g. `impl<T> Any for T`) to keep the baseline
# stable across rustc versions while still tracking every real API item.
cargo +nightly public-api --all-features -s > public-api.txt
echo "Wrote public-api.txt"
