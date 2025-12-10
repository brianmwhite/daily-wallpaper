#!/usr/bin/env bash
set -euo pipefail

# Helper to run the Bing wallpaper CLI from source without installing.
# Prefers uv for isolated environments; falls back to system Python.

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$ROOT_DIR"

if command -v uv >/dev/null 2>&1; then
  exec uv run --package "$ROOT_DIR" python -m bing_wallpaper.cli "$@"
else
  export PYTHONPATH="$ROOT_DIR/src${PYTHONPATH:+:$PYTHONPATH}"
  exec python -m bing_wallpaper.cli "$@"
fi
