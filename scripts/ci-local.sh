#!/usr/bin/env bash
# Single source of truth for CI checks.
# Used by .github/workflows/ci.yml, release.yml's pre-tag gate, .githooks/pre-push,
# and contributors running checks locally. Anything that diverges between local
# and CI belongs here so it stays in sync.
#
# Usage: scripts/ci-local.sh [--stages=check,windows,test,clippy,doc,fmt,drift]
#
# With no --stages, runs all stages in the order above — this is what CI, the
# release gate and the pre-push hook invoke. --stages selects a subset, in
# that same fixed order, regardless of the order named on the command line;
# re-gating an integration tip between landings wants --stages=drift alone,
# without paying for a full build first. A subset run's
# terminal line names the stages it ran, so a log naming less than all seven
# is never mistaken for a full gate.
#
# Exits non-zero on the first failure. The windows stage is the one
# exception: ci-checks.yml's windows-check job already owns Windows compile
# truth authoritatively, so a contributor without the target installed gets a
# loud skip there, not a red gate for a check CI runs regardless.

set -euo pipefail

ALL_STAGES=(check windows test clippy doc fmt drift)

join_by_comma() {
    local IFS=,
    echo "$*"
}

usage() {
    printf 'Usage: %s [--stages=%s]\n' "$0" "$(join_by_comma "${ALL_STAGES[@]}")" >&2
}

stages_csv="$(join_by_comma "${ALL_STAGES[@]}")"
for arg in "$@"; do
    case "$arg" in
        --stages=*)
            stages_csv="${arg#--stages=}"
            ;;
        *)
            printf 'unknown argument: %s\n' "$arg" >&2
            usage
            exit 1
            ;;
    esac
done

IFS=',' read -ra requested <<< "$stages_csv"
for s in "${requested[@]}"; do
    known=0
    for known_stage in "${ALL_STAGES[@]}"; do
        [ "$s" = "$known_stage" ] && known=1 && break
    done
    if [ "$known" -ne 1 ]; then
        printf 'unknown stage: %s (valid: %s)\n' "$s" "$(join_by_comma "${ALL_STAGES[@]}")" >&2
        exit 1
    fi
done

run_stage() {
    local want="$1"
    for s in "${requested[@]}"; do
        [ "$s" = "$want" ] && return 0
    done
    return 1
}

header() {
    printf '\n==> %s\n' "$1"
}

if run_stage check; then
    header "cargo check"
    cargo check
fi

if run_stage windows; then
    header "cargo check --locked --all-targets --target x86_64-pc-windows-msvc"
    if command -v rustup >/dev/null 2>&1 && rustup target list --installed 2>/dev/null | grep -qx x86_64-pc-windows-msvc; then
        cargo check --locked --all-targets --target x86_64-pc-windows-msvc
    else
        printf 'windows cross-check skipped: x86_64-pc-windows-msvc not installed — rustup target add x86_64-pc-windows-msvc\n'
    fi
fi

if run_stage test; then
    header "cargo test --release"
    cargo test --release
fi

if run_stage clippy; then
    header "cargo clippy --all-targets -- -D warnings"
    cargo clippy --all-targets -- -D warnings
fi

if run_stage doc; then
    header "cargo doc --no-deps (rustdoc warnings deny)"
    RUSTDOCFLAGS="-D warnings" cargo doc --no-deps
fi

if run_stage fmt; then
    header "cargo fmt --all -- --check"
    cargo fmt --all -- --check
fi

if run_stage drift; then
    header "explain artifacts up to date (no drift after regeneration)"
    # Regenerate explain markdown + JSON Schema artifacts from templates + Rust
    # types. If anyone changed a `--json`-backing type or a template without
    # committing the regenerated output, this fails.
    # Also regenerates docs/reference/prime/overview.md from its template.
    cargo run --quiet --bin generate-explain
    git diff --exit-code -- docs/reference/explain/ docs/reference/schemas/ docs/reference/prime/ || {
        status=$?
        printf 'explain artifacts changed by regeneration — commit them (this check diffs the working tree against the index; it cannot pass with uncommitted regen)\n' >&2
        exit "$status"
    }
fi

ran_stages=()
for s in "${ALL_STAGES[@]}"; do
    if run_stage "$s"; then
        ran_stages+=("$s")
    fi
done

if [ "${#ran_stages[@]}" -eq "${#ALL_STAGES[@]}" ]; then
    printf '\nAll checks passed.\n'
else
    printf '\nAll checks passed (stages: %s).\n' "$(join_by_comma "${ran_stages[@]}")"
fi
