#!/usr/bin/env bash
set -euo pipefail

# Helper to run the Bing wallpaper CLI from source without installing (Rust).

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$ROOT_DIR"

if ! command -v cargo >/dev/null 2>&1; then
  echo "cargo is required to run this project from source." >&2
  exit 1
fi

exec cargo run --quiet -- "$@"
