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
#
# test-autocrlf is a further stage, outside the seven above and never run
# unless named explicitly (--stages=test-autocrlf): it re-checks out the tree
# under a throwaway GIT_CONFIG_GLOBAL with core.autocrlf=true — matching Git
# for Windows' installer default, which is what a Windows checkout of this
# repo converts tracked text files with — then runs the test stage against
# the result, and checks the tree back out under the ordinary config
# afterward either way. It refuses on a dirty tree rather than running: the
# checkout it needs discards uncommitted changes with no record of what they
# were, where every other stage here only reads the tree.

set -euo pipefail

ALL_STAGES=(check windows test clippy doc fmt drift)
EXTRA_STAGES=(test-autocrlf)
KNOWN_STAGES=("${ALL_STAGES[@]}" "${EXTRA_STAGES[@]}")

join_by_comma() {
    local IFS=,
    echo "$*"
}

usage() {
    printf 'Usage: %s [--stages=%s]\n' "$0" "$(join_by_comma "${ALL_STAGES[@]}")" >&2
    printf '  also valid, not run by default: %s\n' "$(join_by_comma "${EXTRA_STAGES[@]}")" >&2
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
    # The checkout below discards uncommitted changes with no way to get
    # them back, so this stage insists on a clean tree rather than risk
    # being the thing that did that.
    if [ -n "$(git status --porcelain)" ]; then
        printf 'test-autocrlf refuses to run: working tree is not clean (git status --porcelain is non-empty). Commit or stash first — this stage re-checks out the whole tree and a dirty one would be discarded.\n' >&2
        exit 1
    fi
    # GIT_CONFIG_GLOBAL, not `git config --global`, so a developer's own
    # ~/.gitconfig is never touched — and scoped to this one checkout, not
    # exported for cargo test's own run: a test that sets its own HOME to
    # sandbox a git identity or an autocrlf matrix of its own (several do)
    # reads GIT_CONFIG_GLOBAL instead of that HOME's .gitconfig if it is set
    # in the environment, identity included, which is a confound this stage
    # must not hand every test regardless of what it is testing.
    autocrlf_config="$(mktemp)"
    printf '[core]\n\tautocrlf = true\n' > "$autocrlf_config"
    restore_autocrlf_checkout() {
        git checkout -- .
        rm -f "$autocrlf_config"
    }
    # The trap is the path back on the `set -e` exit a failing test
    # triggers; the explicit call after a passing run is what lets a later
    # stage, if one was named alongside this one, see the tree back at its
    # ordinary LF checkout instead of the CRLF one this stage needs.
    trap restore_autocrlf_checkout EXIT
    GIT_CONFIG_GLOBAL="$autocrlf_config" git checkout -- .
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

ran_stages=()
for s in "${KNOWN_STAGES[@]}"; do
    if run_stage "$s"; then
        ran_stages+=("$s")
    fi
done

if [ "${#ran_stages[@]}" -eq "${#ALL_STAGES[@]}" ]; then
    printf '\nAll checks passed.\n'
else
    printf '\nAll checks passed (stages: %s).\n' "$(join_by_comma "${ran_stages[@]}")"
fi
