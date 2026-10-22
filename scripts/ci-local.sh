#!/usr/bin/env bash
# Single source of truth for CI checks.
# Used by .github/workflows/ci.yml, release.yml's pre-tag gate, .githooks/pre-push,
# and contributors running checks locally. Anything that diverges between local
# and CI belongs here so it stays in sync.
#
# Usage: scripts/ci-local.sh [--stages=check,windows,test,test-ambient-autocrlf,clippy,doc,fmt,drift,windows-wine]
#
# With no --stages, runs all stages in the order above but the last — this is
# what CI, the release gate and the pre-push hook invoke. windows-wine runs
# only when named: it is scripts/windows-wine.sh, which downloads several GB
# on first use and reports reds that belong to Wine rather than to the code.
# --stages selects a subset, in that same fixed order, regardless of the order
# named on the command line; re-gating an integration tip between landings
# wants --stages=drift alone, without paying for a full build first. A subset
# run's terminal line names the stages it ran, so a log naming less than every
# default stage is never mistaken for a full gate.
#
# Both test stages run the suite with TMPDIR behind a symlink, the geometry
# macOS's temp root has, on every host that can make one.
#
# test-ambient-autocrlf runs the suite a second time under a system-tier
# core.autocrlf=true, which Git for Windows' installer sets, so every git a
# test runs converts line endings on checkout unless its fixture pins its own
# config. Which fixtures that reddens turns on the filesystem — whether it
# clones, links, or neither — so the stage runs on every host rather than on
# one. This repository's own files are not re-checked out for it:
# .gitattributes keeps them LF under any config, and
# tests/repo_line_endings_test.rs holds that.
#
# Exits non-zero on the first failure. The windows stage is the one
# exception: ci-checks.yml's windows-check job already owns Windows compile
# and lint truth authoritatively, so a contributor without the target
# installed gets a loud skip there, not a red gate for a check CI runs
# regardless.

set -euo pipefail

ALL_STAGES=(check windows test test-ambient-autocrlf clippy doc fmt drift)
OPT_IN_STAGES=(windows-wine)
KNOWN_STAGES=("${ALL_STAGES[@]}" "${OPT_IN_STAGES[@]}")

join_by_comma() {
    local IFS=,
    echo "$*"
}

usage() {
    printf 'Usage: %s [--stages=%s]\n' "$0" "$(join_by_comma "${ALL_STAGES[@]}")" >&2
    printf '  also valid, not run by default: %s\n' "$(join_by_comma "${OPT_IN_STAGES[@]}")" >&2
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
    for known_stage in "${KNOWN_STAGES[@]}"; do
        [ "$s" = "$known_stage" ] && known=1 && break
    done
    if [ "$known" -ne 1 ]; then
        printf 'unknown stage: %s (valid: %s)\n' "$s" "$(join_by_comma "${KNOWN_STAGES[@]}")" >&2
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

# Runs a command with TMPDIR behind a symlink, made inside the TMPDIR this
# script was handed so fixtures stay on that filesystem. rwv records and
# prints resolved paths, and macOS's TMPDIR is under /var, a symlink to
# /private/var: a test comparing a path rwv reports with one spelled from the
# raw temp root is red there and green wherever the temp dir is a real
# directory. Under the link every host has that geometry, and link/ and real/
# share no substring, so a `contains` cannot collapse the two spellings the way
# /var inside /private/var does. Git Bash's `ln -s` copies rather than links
# unless MSYS asks for native symlinks and Windows grants the right to make
# one, so a host that made no link runs without it and says so.
with_symlinked_tmpdir() {
    local base gate_tmp status=0
    base="${TMPDIR:-/tmp}"
    gate_tmp="$(mktemp -d "${base%/}/rwv-gate-tmp.XXXXXX")"
    mkdir "$gate_tmp/real"
    if ln -s "$gate_tmp/real" "$gate_tmp/link" 2>/dev/null && [ -L "$gate_tmp/link" ]; then
        printf 'TMPDIR=%s (a symlink to %s)\n' "$gate_tmp/link" "$gate_tmp/real"
        TMPDIR="$gate_tmp/link" "$@" || status=$?
    else
        printf 'NOTICE: this host made no symlink, so TMPDIR is not behind one: a test comparing a path rwv resolved with one spelled from the raw temp root is unchecked here\n' >&2
        "$@" || status=$?
    fi
    chmod -R u+rwx "$gate_tmp" 2>/dev/null || true
    rm -rf "$gate_tmp" || printf 'warning: could not remove %s\n' "$gate_tmp" >&2
    return "$status"
}

if run_stage check; then
    header "cargo check"
    cargo check
fi

if run_stage windows; then
    header "cargo check --locked --all-targets --target x86_64-pc-windows-msvc"
    if command -v rustup >/dev/null 2>&1 && rustup target list --installed 2>/dev/null | grep -qx x86_64-pc-windows-msvc; then
        cargo check --locked --all-targets --target x86_64-pc-windows-msvc
        # Host clippy never sees a cfg(windows) item, so clippy.toml's bans
        # reach those only through a run for the Windows target. Linting
        # needs no linker.
        header "cargo clippy --locked --all-targets --target x86_64-pc-windows-msvc -- -D warnings"
        cargo clippy --locked --all-targets --target x86_64-pc-windows-msvc -- -D warnings
    else
        printf 'windows cross-check skipped: x86_64-pc-windows-msvc not installed — rustup target add x86_64-pc-windows-msvc\n'
    fi
fi

if run_stage test; then
    header "cargo test --release --no-fail-fast"
    # --no-fail-fast: without it cargo stops at the first failing binary and
    # every later one goes unrun rather than green — this is what makes the
    # red list a list.
    with_symlinked_tmpdir cargo test --release --no-fail-fast
fi

if run_stage test-ambient-autocrlf; then
    header "cargo test --release --no-fail-fast (ambient core.autocrlf=true, as Git for Windows installs it)"
    # GIT_CONFIG_SYSTEM, not GIT_CONFIG_GLOBAL: the system tier is where Git
    # for Windows' installer sets it, below anyone's identity rather than in
    # place of it, so a test that sandboxes its own HOME for an identity or a
    # line-ending matrix of its own keeps both, as it would on Windows.
    ambient_config="$(mktemp)"
    printf '[core]\n\tautocrlf = true\n' > "$ambient_config"
    ambient_status=0
    with_symlinked_tmpdir env GIT_CONFIG_SYSTEM="$ambient_config" cargo test --release --no-fail-fast || ambient_status=$?
    rm -f "$ambient_config"
    [ "$ambient_status" -eq 0 ] || exit "$ambient_status"
fi

if run_stage clippy; then
    header "cargo clippy --all-targets -- -D warnings"
    cargo clippy --all-targets -- -D warnings
fi

if run_stage doc; then
    header "cargo doc --no-deps --document-private-items (rustdoc warnings deny)"
    RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --document-private-items
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
    # A pass says only that the three directories below match what the
    # generator currently produces from current inputs — nothing about a
    # hand edit anywhere else, and nothing about one made inside them, since
    # regeneration already overwrote it before this diff ran.
    cargo run --quiet --bin generate-explain
    git diff --exit-code -- docs/reference/explain/ docs/reference/schemas/ docs/reference/prime/ || {
        status=$?
        printf 'explain artifacts changed by regeneration — commit them (this check compares the regenerated docs/reference/explain, docs/reference/schemas, and docs/reference/prime against the index; it does not inspect files the generator does not write)\n' >&2
        exit "$status"
    }
fi

if run_stage windows-wine; then
    header "test suite under Wine as x86_64-pc-windows-gnu binaries"
    "$(dirname "$0")/windows-wine.sh"
fi

ran_stages=()
for s in "${KNOWN_STAGES[@]}"; do
    if run_stage "$s"; then
        ran_stages+=("$s")
    fi
done

if [ "${ran_stages[*]}" = "${ALL_STAGES[*]}" ]; then
    printf '\nAll checks passed.\n'
else
    printf '\nAll checks passed (stages: %s).\n' "$(join_by_comma "${ran_stages[@]}")"
fi
