#!/usr/bin/env bash
# Single source of truth for CI checks.
# Used by .github/workflows/ci.yml, release.yml's pre-tag gate, .githooks/pre-push,
# and contributors running checks locally. Anything that diverges between local
# and CI belongs here so it stays in sync.
#
# Usage: scripts/ci-local.sh [--stages=check,windows,test,clippy,doc,fmt,drift,test-autocrlf,windows-wine]
#
# With no --stages, runs all stages in the order above but the last — this is
# what CI, the release gate and the pre-push hook invoke. windows-wine runs
# only when named: it is scripts/windows-wine.sh, which downloads several GB
# on first use and reports reds that belong to Wine rather than to the code.
# --stages selects a subset, in that same fixed order, regardless of the order
# named on the command line; re-gating an integration tip between landings
# wants --stages=drift alone, without paying for a full build first. A subset
# run's terminal line names the stages it ran, so a log naming less than the
# seven default stages is never mistaken for a full gate.
#
# Exits non-zero on the first failure. The windows stage is the one
# exception: ci-checks.yml's windows-check job already owns Windows compile
# and lint truth authoritatively, so a contributor without the target
# installed gets a loud skip there, not a red gate for a check CI runs
# regardless.
#
# test-autocrlf is a further stage, outside the seven above and never run
# unless named explicitly (--stages=test-autocrlf): it runs the test stage
# under a throwaway GIT_CONFIG_SYSTEM with core.autocrlf=true — the tier Git
# for Windows' installer sets it at, ahead of anyone's identity, which is why
# every git repo a test creates at runtime inherits it there regardless of
# what HOME that test points at. First it empties the index and resets the
# tree to HEAD under the same setting, rewriting every tracked text file to
# CRLF the way a Windows checkout of this repo would (paths .gitattributes
# pins to eol=lf stay LF), then does the same under the ordinary config
# afterward either way, leaving every tracked text file LF and
# `git status --porcelain` empty. It refuses on a dirty tree rather than
# running: the reset it needs discards uncommitted changes with no record of
# what they were, where every other stage here only reads the tree.

set -euo pipefail

ALL_STAGES=(check windows test clippy doc fmt drift)
OPT_IN_STAGES=(test-autocrlf windows-wine)
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
    cargo test --release --no-fail-fast
fi

if run_stage test-autocrlf; then
    header "cargo test --release --no-fail-fast (core.autocrlf=true, simulating a Windows checkout)"
    # The reset below discards uncommitted changes with no way to get
    # them back, so this stage insists on a clean tree rather than risk
    # being the thing that did that.
    if [ -n "$(git status --porcelain)" ]; then
        printf 'test-autocrlf refuses to run: working tree is not clean (git status --porcelain is non-empty). Commit or stash first — this stage re-checks out the whole tree and a dirty one would be discarded.\n' >&2
        exit 1
    fi
    # GIT_CONFIG_SYSTEM, not GIT_CONFIG_GLOBAL: the system tier is what Git
    # for Windows' installer sets core.autocrlf=true at, and it sits below
    # anyone's identity rather than replacing it, so a test that sandboxes
    # its own HOME to set up a git identity or an autocrlf matrix of its own
    # (several do) keeps that identity exactly as it would on the real
    # ambient environment this stage otherwise matches — only the system
    # tier changes, system config having nothing in it here to lose.
    autocrlf_config="$(mktemp)"
    printf '[core]\n\tautocrlf = true\n' > "$autocrlf_config"
    # `git checkout -- .` would do nothing here: a clean tree is stat-clean
    # against the index, so no file is rewritten and the line endings stay as
    # they were. Emptying the index first forces every tracked file out again.
    recheckout_tree() {
        git read-tree --empty
        git reset --hard -q
    }
    restore_autocrlf_checkout() {
        unset GIT_CONFIG_SYSTEM
        recheckout_tree
        rm -f "$autocrlf_config"
    }
    # The trap is the path back on the `set -e` exit a failing test
    # triggers; the explicit call after a passing run is what lets a later
    # stage, if one was named alongside this one, see the tree back at its
    # ordinary LF checkout instead of the CRLF one this stage needs.
    trap restore_autocrlf_checkout EXIT
    export GIT_CONFIG_SYSTEM="$autocrlf_config"
    recheckout_tree
    cargo test --release --no-fail-fast
    restore_autocrlf_checkout
    trap - EXIT
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
