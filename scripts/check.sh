#!/usr/bin/env bash
# Everything that should pass before a commit: formatting, lints (warnings are errors), tests.
# The lint policy lives in Cargo.toml ([lints]); formatting in rustfmt.toml.
set -euo pipefail
cd "$(dirname "$0")/.."

cargo fmt --check || { echo "→ run: cargo fmt" >&2; exit 1; }
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
