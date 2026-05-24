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

header "explain artifacts up to date (no drift after regeneration)"
# Regenerate explain markdown + JSON Schema artifacts from templates + Rust
# types. If anyone changed a `--json`-backing type or a template without
# committing the regenerated output, this fails. See fo-tn9uk.2.
cargo run --quiet --bin generate-explain
git diff --exit-code -- docs/reference/explain/ docs/reference/schemas/

printf '\nAll checks passed.\n'
