#!/usr/bin/env bash
# Single source of truth for CI checks.
# Used by .github/workflows/ci.yml, release.yml's pre-tag gate, .githooks/pre-push,
# and contributors running checks locally. Anything that diverges between local
# and CI belongs here so it stays in sync.
#
# Usage: scripts/ci-local.sh [--tier=seat|landing|release | --stages=check,windows,apple,test,test-ambient-autocrlf,no-test-pause,clippy,doc,fmt,drift,kill-matrix,windows-wine,git-floor]
#
# With no argument, runs all stages in the order above but the last three — the
# full gate, which CI, the release gate and the pre-push hook invoke.
# kill-matrix runs only when named, or by --tier=release, which is every default
# stage and kill-matrix: it runs tests/kill_matrix's cells, each verb killed at
# each pause point it passes and then continued and aborted, which take about
# a quarter of an hour. Run it before a release and when a change touches an
# operation's steps, pause points or abort. The test stages do not run those
# cells: the binary's shape tests return at once, naming this stage, unless
# RWV_KILL_MATRIX=run, which this stage sets.
# windows-wine runs only when named: it is scripts/windows-wine.sh, which
# downloads several GB on first use and reports reds that belong to Wine
# rather than to the code. git-floor runs only when named too: it runs the
# suite against the oldest git release rwv runs on, which scripts/git-floor.sh
# downloads and builds into a cache outside the repository on first use.
#
# --tier=seat runs the default stages a change is most likely to redden and
# the ones whose output it must commit, for iterating on that change in its
# own seat. --tier=landing runs exactly the default stages seat skips: the
# whole-suite and cross-target reruns, which a landing pays once. The two
# together are the full gate, each stage once. Only the seat list is written
# down; landing is computed from it.
#
# --stages selects any subset, in the same fixed order, regardless of the
# order named on the command line; re-gating an integration tip between
# landings wants --stages=drift alone, without paying for a full build first.
# --tier and --stages are refused together.
#
# A subset run's terminal line names its tier, if any, and the stages it ran,
# so a log naming less than every default stage is never mistaken for a full
# gate.
#
# Both test stages run the suite with TMPDIR behind a symlink, the geometry
# macOS's temp root has, on every host that can make one, and with the
# test-pause feature, which lets a test hold rwv at a named point while it
# changes what rwv will read. The `rwv` an integration test spawns is the one
# cargo builds for that invocation, so the feature reaches it only by being
# passed here. no-test-pause then builds without it, as every release does,
# and holds that no pause survives there. The windows stage compiles the
# featured build, its lint the shipped one; clippy lints both. The apple stage
# does the same for aarch64-apple-darwin, so code behind a macOS cfg compiles
# and lints on a host that is not macOS before CI's macOS job runs it.
#
# test-ambient-autocrlf runs the suite a second time under a system-tier
# core.autocrlf=true, which Git for Windows' installer sets, so every git a
# test runs converts line endings on checkout unless its fixture pins its own
# config. Which fixtures that reddens turns on the filesystem — whether it
# clones, links, or neither — so the stage runs on every host rather than on
# one. This repository's own files are not re-checked out for it:
# .gitattributes keeps them LF under any config, and
# tests/source/repo_line_endings_test.rs holds that.
#
# Every stage that runs the suite runs each test binary's tests on
# RUST_TEST_THREADS threads of one process, half the host's cores and never
# fewer than four (or every core, on a host with fewer). On a 32-core host the
# suite measured no faster at 32 threads than at 16 and spent more system
# time, while 8 cost half again: half the cores is as fast for one gate and
# leaves room for another beside it. A caller's own RUST_TEST_THREADS wins.
#
# Exits non-zero on the first failure. The windows and apple stages are the
# exceptions: ci-checks.yml's windows-check job and its macOS run already own
# that compile and lint truth authoritatively, so a contributor without the
# target installed gets a loud skip there, not a red gate for a check CI runs
# regardless.

set -euo pipefail

if [ -z "${RUST_TEST_THREADS:-}" ]; then
    cores="$(getconf _NPROCESSORS_ONLN 2>/dev/null || nproc 2>/dev/null || echo 4)"
    RUST_TEST_THREADS=$((cores / 2))
    if [ "$RUST_TEST_THREADS" -lt 4 ]; then
        RUST_TEST_THREADS=$((cores < 4 ? cores : 4))
    fi
fi
export RUST_TEST_THREADS

ALL_STAGES=(check windows apple test test-ambient-autocrlf no-test-pause clippy doc fmt drift)
OPT_IN_STAGES=(kill-matrix windows-wine git-floor)
KNOWN_STAGES=("${ALL_STAGES[@]}" "${OPT_IN_STAGES[@]}")
SEAT_STAGES=(check test no-test-pause clippy doc fmt drift)
TIERS=(seat landing release)

join_by_comma() {
    local IFS=,
    echo "$*"
}

is_one_of() {
    local want="$1" candidate
    shift
    for candidate in "$@"; do
        [ "$candidate" = "$want" ] && return 0
    done
    return 1
}

LANDING_STAGES=()
for s in "${ALL_STAGES[@]}"; do
    is_one_of "$s" "${SEAT_STAGES[@]}" || LANDING_STAGES+=("$s")
done

RELEASE_STAGES=("${ALL_STAGES[@]}" kill-matrix)

usage() {
    printf 'Usage: %s [--tier=%s | --stages=%s]\n' "$0" "$(IFS='|'; echo "${TIERS[*]}")" "$(join_by_comma "${ALL_STAGES[@]}")" >&2
    printf '  seat: %s\n' "$(join_by_comma "${SEAT_STAGES[@]}")" >&2
    printf '  landing: %s\n' "$(join_by_comma "${LANDING_STAGES[@]}")" >&2
    printf '  release: %s\n' "$(join_by_comma "${RELEASE_STAGES[@]}")" >&2
    printf '  also valid in --stages, not run by default: %s\n' "$(join_by_comma "${OPT_IN_STAGES[@]}")" >&2
}

tier=""
selector=""
stages_csv="$(join_by_comma "${ALL_STAGES[@]}")"
for arg in "$@"; do
    case "$arg" in
        --tier=* | --stages=*) ;;
        *)
            printf 'unknown argument: %s\n' "$arg" >&2
            usage
            exit 1
            ;;
    esac
    if [ -n "$selector" ]; then
        printf 'conflicting stage selectors: %s and %s (pass one --tier or one --stages)\n' "$selector" "$arg" >&2
        usage
        exit 1
    fi
    selector="$arg"
    case "$arg" in
        --tier=seat) tier=seat stages_csv="$(join_by_comma "${SEAT_STAGES[@]}")" ;;
        --tier=landing) tier=landing stages_csv="$(join_by_comma "${LANDING_STAGES[@]}")" ;;
        --tier=release) tier=release stages_csv="$(join_by_comma "${RELEASE_STAGES[@]}")" ;;
        --tier=*)
            printf 'unknown tier: %s (valid: %s)\n' "${arg#--tier=}" "$(join_by_comma "${TIERS[@]}")" >&2
            exit 1
            ;;
        --stages=*) stages_csv="${arg#--stages=}" ;;
    esac
done

IFS=',' read -ra requested <<< "$stages_csv"
for s in "${requested[@]}"; do
    if ! is_one_of "$s" "${KNOWN_STAGES[@]}"; then
        printf 'unknown stage: %s (valid: %s)\n' "$s" "$(join_by_comma "${KNOWN_STAGES[@]}")" >&2
        exit 1
    fi
done

run_stage() {
    is_one_of "$1" "${requested[@]}"
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
# The installed targets, one per line, read whole before anything matches
# them: `rustup ... | grep -q` under pipefail is false whenever grep exits on
# its match while rustup is still writing, which skips a stage whose target is
# installed. Fails where rustup is missing or cannot list them.
installed_targets() {
    command -v rustup >/dev/null 2>&1 && rustup target list --installed 2>/dev/null
}

# Runs a cross-target check and lint for $2, or says why it skipped: the
# target is not installed, or the installed targets could not be read.
cross_check() {
    local stage="$1" target="$2" targets
    if ! targets="$(installed_targets)"; then
        printf '%s cross-check skipped: the installed targets could not be read (rustup missing, or `rustup target list --installed` failed)\n' "$stage"
    elif grep -qx "$target" <<<"$targets"; then
        header "cargo check --locked --all-targets --features test-pause --target $target"
        cargo check --locked --all-targets --features test-pause --target "$target"
        header "cargo clippy --locked --all-targets --target $target -- -D warnings"
        cargo clippy --locked --all-targets --target "$target" -- -D warnings
    else
        printf '%s cross-check skipped: %s not installed — rustup target add %s\n' "$stage" "$target" "$target"
    fi
}

with_symlinked_tmpdir() {
    local base gate_tmp status=0
    base="${TMPDIR:-/tmp}"
    gate_tmp="$(mktemp -d "${base%/}/rwv-gate-tmp.XXXXXX")"
    mkdir "$gate_tmp/real"
    if ln -s "$gate_tmp/real" "$gate_tmp/link" 2>/dev/null && [ -L "$gate_tmp/link" ]; then
        printf 'TMPDIR=%s (a symlink to %s), RUST_TEST_THREADS=%s\n' "$gate_tmp/link" "$gate_tmp/real" "$RUST_TEST_THREADS"
        TMPDIR="$gate_tmp/link" "$@" || status=$?
    else
        printf 'RUST_TEST_THREADS=%s\n' "$RUST_TEST_THREADS"
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

# Host clippy never sees a cfg(windows) or macOS-only item, so clippy.toml's
# bans reach those only through a run for that target. Linting needs no
# linker.
if run_stage windows; then
    cross_check windows x86_64-pc-windows-msvc
fi

if run_stage apple; then
    cross_check apple aarch64-apple-darwin
fi

if run_stage test; then
    header "cargo test --release --no-fail-fast --features test-pause"
    # --no-fail-fast: without it cargo stops at the first failing binary and
    # every later one goes unrun rather than green — this is what makes the
    # red list a list.
    with_symlinked_tmpdir cargo test --release --no-fail-fast --features test-pause
fi

if run_stage test-ambient-autocrlf; then
    header "cargo test --release --no-fail-fast --features test-pause (ambient core.autocrlf=true, as Git for Windows installs it)"
    # GIT_CONFIG_SYSTEM, not GIT_CONFIG_GLOBAL: the system tier is where Git
    # for Windows' installer sets it, below anyone's identity rather than in
    # place of it, so a test that sandboxes its own HOME for an identity or a
    # line-ending matrix of its own keeps both, as it would on Windows.
    ambient_config="$(mktemp)"
    printf '[core]\n\tautocrlf = true\n' > "$ambient_config"
    ambient_status=0
    with_symlinked_tmpdir env GIT_CONFIG_SYSTEM="$ambient_config" cargo test --release --no-fail-fast --features test-pause || ambient_status=$?
    rm -f "$ambient_config"
    [ "$ambient_status" -eq 0 ] || exit "$ambient_status"
fi

if run_stage no-test-pause; then
    header "cargo test --no-fail-fast --test test_pause_build -- --include-ignored (built as releases build it: without test-pause)"
    # No feature flags, as releases build. Debug, not the test stages' release
    # profile: whether the pause is compiled in turns on the features, never
    # on the profile, and the debug build compiles faster.
    with_symlinked_tmpdir cargo test --no-fail-fast --test test_pause_build -- --include-ignored
fi

if run_stage clippy; then
    header "cargo clippy --all-targets -- -D warnings"
    cargo clippy --all-targets -- -D warnings
    # The build the test stages run, which compiles what the feature adds.
    header "cargo clippy --all-targets --features test-pause -- -D warnings"
    cargo clippy --all-targets --features test-pause -- -D warnings
fi

if run_stage doc; then
    header "cargo doc --no-deps --document-private-items --lib --bins --examples (rustdoc warnings deny)"
    RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --document-private-items --lib --bins --examples
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
    # Also regenerates docs/reference/prime/overview.md from its template,
    # docs/reference/refusals.md from its template and the state-to-choices
    # table in src/diagnosis.rs, and the entries of
    # docs/reference/doctor-findings.md that table shares with a finding,
    # and docs/internals/refusal-census.tsv from the refusal sites in src/
    # and the tests that read them.
    # A pass says only that the paths below match what the generator
    # currently produces from current inputs — nothing about a hand edit
    # anywhere else, and nothing about one made inside them, since
    # regeneration already overwrote it before this diff ran.
    # Release with the test stages' features, so the generator reuses the
    # build they made rather than compiling the library a second time.
    cargo run --release --features test-pause --quiet --example generate-explain
    git diff --exit-code -- docs/reference/explain/ docs/reference/schemas/ docs/reference/prime/ docs/reference/refusals.md docs/reference/doctor-findings.md docs/internals/refusal-census.tsv || {
        status=$?
        printf 'explain artifacts changed by regeneration — commit them (this check compares the regenerated docs/reference/explain, docs/reference/schemas, docs/reference/prime, docs/reference/refusals.md, docs/reference/doctor-findings.md and docs/internals/refusal-census.tsv against the index; it does not inspect files the generator does not write; an entry of doctor-findings.md that the state-to-choices table generates is changed in the table, not in the page)\n' >&2
        exit "$status"
    }
fi

if run_stage kill-matrix; then
    header "cargo test --release --no-fail-fast --features test-pause --test kill_matrix (every cell)"
    with_symlinked_tmpdir env RWV_KILL_MATRIX=run cargo test --release --no-fail-fast --features test-pause --test kill_matrix
fi

if run_stage windows-wine; then
    header "test suite under Wine as x86_64-pc-windows-gnu binaries"
    "$(dirname "$0")/windows-wine.sh"
fi

if run_stage git-floor; then
    header "cargo test --release --no-fail-fast --features test-pause (against the oldest git rwv runs on)"
    floor_bin="$("$(dirname "$0")/git-floor.sh")"
    floor_release="$("$(dirname "$0")/git-floor.sh" --print-release)"
    found="$(PATH="$floor_bin:$PATH" git version)"
    if [ "$found" != "git version $floor_release" ]; then
        printf 'git-floor: PATH resolves %s, not git %s\n' "$found" "$floor_release" >&2
        exit 1
    fi
    printf '%s from %s\n' "$found" "$floor_bin"
    with_symlinked_tmpdir env PATH="$floor_bin:$PATH" cargo test --release --no-fail-fast --features test-pause
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
    printf '\nAll checks passed (%sstages: %s).\n' "${tier:+tier: $tier; }" "$(join_by_comma "${ran_stages[@]}")"
fi
