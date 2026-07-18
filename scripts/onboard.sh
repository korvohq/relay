#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd -- "$SCRIPT_DIR/.." && pwd)"

if ! command -v cargo >/dev/null 2>&1; then
  echo "error: Rust/Cargo is required; install it from https://rustup.rs" >&2
  exit 1
fi

cd "$PROJECT_DIR"
echo "Building Relay locally..."
cargo build --release --locked

echo
exec "$PROJECT_DIR/target/release/relay" onboard "$@"
