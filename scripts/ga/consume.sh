#!/usr/bin/env bash
# Install the archive in a fresh consumer directory; no checkout dependency.
set -euo pipefail
if [ "$#" -lt 3 ] || [[ "$3" != beta && "$3" != soak ]]; then
  echo "usage: $0 <msgtrans-2.0.0.crate> <new output directory> beta|soak [seconds]" >&2
  exit 2
fi
archive=$1
out=$2
mode=$3
src="$(cd "$(dirname "$0")" && pwd)"
mkdir "$out"
out="$(cd "$out" && pwd -P)"
tar -xf "$archive" -C "$out"
cd "$out"
mkdir -p consumer/src
cp "$src/$mode.rs" consumer/src/main.rs
cat > consumer/Cargo.toml <<'TOML'
[package]
name = "msgtrans-ga-consumer"
version = "0.1.0"
edition = "2021"
publish = false

[dependencies]
msgtrans = { path = "../msgtrans-2.0.0", version = "=2.0.0" }
tokio = { version = "1", features = ["full"] }
async-trait = "0.1"
rcgen = "0.14"
TOML
if [ "$mode" = beta ]; then
  echo 'beta = { package = "msgtrans", version = "=2.0.0-beta.3", default-features = false, features = ["quic"] }' >> consumer/Cargo.toml
fi
cd consumer
cargo tree -i msgtrans@2.0.0
cargo run --release -- "${4:-3600}"
