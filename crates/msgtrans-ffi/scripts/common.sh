# Sourced by the other scripts. Runs from crates/msgtrans-ffi.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

# The workspace's target directory (honours CARGO_TARGET_DIR).
TARGET_DIR="$(cargo metadata --format-version 1 --no-deps \
  | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')"

case "$(uname -s)" in
  Darwin) HOST_LIB="$TARGET_DIR/release/libmsgtrans_ffi.dylib" ;;
  *)      HOST_LIB="$TARGET_DIR/release/libmsgtrans_ffi.so" ;;
esac
