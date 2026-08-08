#!/usr/bin/env bash
# Pre-commit-style gate: fmt, clippy (warnings denied), full test suite.
# Usage: scripts/check.sh [--fix]
set -euo pipefail
cd "$(dirname "$0")/.."

if [[ "${1:-}" == "--fix" ]]; then
    cargo fmt
    cargo clippy --fix --allow-dirty --bin nexus-chat || true
fi

cargo fmt --check
cargo clippy --bin nexus-chat -- -D warnings -W clippy::pedantic
cargo test --bin nexus-chat
echo "check.sh: all green"
