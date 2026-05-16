#!/usr/bin/env bash
# Single source of truth for CI checks.
# Used by .github/workflows/ci.yml, release.yml's pre-tag gate, and contributors
# running checks locally. Anything that diverges between local and CI belongs
# here so it stays in sync.
#
# Usage: scripts/ci-local.sh
#
# Exits non-zero on the first failure.

set -euo pipefail

header() {
    printf '\n==> %s\n' "$1"
}

header "cargo check"
cargo check

header "cargo test --release"
cargo test --release

header "cargo clippy --all-targets -- -D warnings"
cargo clippy --all-targets -- -D warnings

header "cargo fmt --all -- --check"
cargo fmt --all -- --check

printf '\nAll checks passed.\n'
