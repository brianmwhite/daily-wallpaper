#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo_root=$(cd -- "$script_dir/.." && pwd)

exec cargo run --quiet --features bump-version --bin bump_version --manifest-path "$repo_root/Cargo.toml" -- "$@"
