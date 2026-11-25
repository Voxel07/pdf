#!/usr/bin/env bash
# Runs cargo inside a Rust container (useful when no C toolchain is installed locally).
# Usage: scripts/docker-cargo.sh build --release
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CACHE="${PDFSIGN_CARGO_CACHE:-$HOME/.cache/pdfsign-cargo}"
# renovate: datasource=docker depName=rust
RUST_IMAGE="rust:1.100.0-slim-trixie"
mkdir -p "$CACHE"
exec docker run --rm -i \
  -u "$(id -u):$(id -g)" \
  -e CARGO_HOME=/cargo -e CARGO_TARGET_DIR=/src/server/target-docker \
  -v "$CACHE:/cargo:z" -v "$ROOT:/src:z" -w /src/server \
  --network host \
  "$RUST_IMAGE" cargo "$@"
