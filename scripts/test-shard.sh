#!/usr/bin/env bash
# Runs one shard of the test suite, for ci-local.sh's --test-shard=K/N.
#
# Usage: scripts/test-shard.sh [--only-test TARGET] K/N <cargo test arguments that precede `--`>
#        scripts/test-shard.sh --check K/N
#
# cargo itself says what the suite is: `cargo test --no-run
# --message-format=json` names every test target (library, binaries,
# integration tests, examples), and each target's own executable lists its
# tests with `--list`; `cargo test --doc -- --list` lists the doctests. Every
# test of every target is one item, named by its target and its own name, and
# lands in the shard its name hashes to. Nothing here names a target or a
# test, so a new one is in some shard the moment it exists, and a single
# binary larger than a shard's share is split across shards.
#
# The hash is a polynomial over the item's characters, in awk arithmetic that
# stays exact in a double, so every awk on every host agrees on it.
#
# The doctests are one item, in the shard "doctests" hashes to: rustdoc names
# a doctest differently when it runs than when it lists, so a filter by name
# cannot pick some of them.
#
# --only-test TARGET shards that one integration-test target alone and leaves
# the rest of the suite, doctests included, to another job.
#
# Each target that has tests in the shard runs as its own cargo invocation
# with `-- --exact <names>`, the names in as many invocations as keep each
# command line under CHUNK_BYTES, far under the 32767 characters Windows
# allows; one that has none does not run, since a run with no filter would run
# everything. Every invocation runs even after one fails,
# as --no-fail-fast does, and the script exits non-zero if any did.
#
# Needs jq to read cargo's messages.

set -euo pipefail

CHUNK_BYTES=12000

valid_shard() {
    case "$1" in
        [1-9]*/[1-9]*) ;;
        *) return 1 ;;
    esac
    local index="${1%/*}" count="${1#*/}"
    case "$index$count" in
        *[!0-9]*) return 1 ;;
    esac
    [ "$index" -le "$count" ]
}

if [ "${1:-}" = "--check" ]; then
    valid_shard "${2:-}" || {
        printf 'a test shard is K/N with whole numbers 1 <= K <= N, not %s\n' "${2:-}" >&2
        exit 1
    }
    exit 0
fi

only_test=""
if [ "${1:-}" = "--only-test" ]; then
    only_test="${2:?--only-test names a test target}"
    shift 2
fi

shard="${1:-}"
valid_shard "$shard" || {
    printf 'a test shard is K/N with whole numbers 1 <= K <= N, not %s\n' "$shard" >&2
    exit 1
}
shift
index="${shard%/*}"
count="${shard#*/}"

command -v jq >/dev/null 2>&1 || {
    printf 'test-shard: jq is needed to read cargo'"'"'s list of test targets\n' >&2
    exit 1
}

cargo_args=("$@")

in_this_shard() {
    awk -v n="$count" -v k="$index" -v prefix="$1" '
        BEGIN { for (i = 32; i < 127; i++) ord[sprintf("%c", i)] = i }
        {
            if ($0 == "") next
            item = prefix "\t" $0
            h = 0
            for (i = 1; i <= length(item); i++) {
                c = substr(item, i, 1)
                h = (h * 65599 + ((c in ord) ? ord[c] : 0)) % 2147483648
            }
            if (h % n == k - 1) print
        }'
}

listed_tests() {
    sed -n 's/: test$//p'
}

selected_total=0
listed_total=0
only_test_found=0
failed=()

run_selected() {
    local label="$1" listing="$2"
    shift 2
    local picked=() line
    while IFS= read -r line; do
        picked+=("$line")
    done < <(in_this_shard "$label" <<<"$listing")
    local listed
    listed="$(grep -c . <<<"$listing" || true)"
    listed_total=$((listed_total + listed))
    selected_total=$((selected_total + ${#picked[@]}))
    printf 'shard %s: %s: %s of %s tests\n' "$shard" "$label" "${#picked[@]}" "$listed"
    [ "${#picked[@]}" -gt 0 ] || return 0
    local chunk=() bytes=0 name
    for name in "${picked[@]}"; do
        if [ "${#chunk[@]}" -gt 0 ] && [ $((bytes + ${#name} + 1)) -gt "$CHUNK_BYTES" ]; then
            cargo test ${cargo_args[@]+"${cargo_args[@]}"} "$@" -- --exact "${chunk[@]}" || failed+=("$label")
            chunk=()
            bytes=0
        fi
        chunk+=("$name")
        bytes=$((bytes + ${#name} + 1))
    done
    cargo test ${cargo_args[@]+"${cargo_args[@]}"} "$@" -- --exact "${chunk[@]}" || failed+=("$label")
}

messages="$(cargo test ${cargo_args[@]+"${cargo_args[@]}"} --no-run --message-format=json)"
targets="$(jq -r 'select(.reason == "compiler-artifact" and .profile.test == true and .executable != null) | [.target.kind[0], .target.name, .executable] | join("\t")' <<<"$messages" | sort -u)"

while IFS=$'\t' read -r kind name executable; do
    [ -n "$kind" ] || continue
    if [ -n "$only_test" ] && { [ "$kind" != test ] || [ "$name" != "$only_test" ]; }; then
        continue
    fi
    case "$kind" in
        lib | rlib | dylib | cdylib | staticlib | proc-macro) selector=(--lib) ;;
        bin) selector=(--bin "$name") ;;
        test) selector=(--test "$name") ;;
        example) selector=(--example "$name") ;;
        *)
            printf 'test-shard: target %s is of kind %s, which no shard knows how to select\n' "$name" "$kind" >&2
            exit 1
            ;;
    esac
    only_test_found=1
    listing="$("$executable" --list --format terse | listed_tests)"
    run_selected "$kind $name" "$listing" "${selector[@]}"
done <<<"$targets"

if [ -n "$only_test" ] && [ "$only_test_found" -eq 0 ]; then
    printf 'test-shard: cargo lists no test target named %s\n' "$only_test" >&2
    exit 1
fi

if [ -z "$only_test" ]; then
    doctests="$(cargo test ${cargo_args[@]+"${cargo_args[@]}"} --doc -- --list --format terse | listed_tests)"
    doctest_count="$(grep -c . <<<"$doctests" || true)"
    listed_total=$((listed_total + doctest_count))
    if [ -n "$(echo doctests | in_this_shard "")" ]; then
        selected_total=$((selected_total + doctest_count))
        printf 'shard %s: doctests: all %s\n' "$shard" "$doctest_count"
        cargo test ${cargo_args[@]+"${cargo_args[@]}"} --doc || failed+=("doctests")
    else
        printf 'shard %s: doctests: 0 of %s tests\n' "$shard" "$doctest_count"
    fi
fi

printf 'shard %s: %s of %s tests selected\n' "$shard" "$selected_total" "$listed_total"
if [ "${#failed[@]}" -gt 0 ]; then
    printf 'shard %s: failed: %s\n' "$shard" "$(IFS=,; echo "${failed[*]}")" >&2
    exit 1
fi
