#!/usr/bin/env bash
# Reclaim orphaned pre-op savepoint refs from a single repository.
#
# Usage:
#   scripts/reap-orphaned-savepoints.sh --repo=<path> [--expire-ref=<ref>]
#
# Prints a report of every refs/rwv/pre-op/<id> found, notes its disposition
# (WOULD-REAP / KEEP), and — only if --expire-ref is given AND this is not
# a dry run — deletes the WOULD-REAP refs.
#
# Options:
#   --repo=<path>        Path to the git repository (bare or working tree).
#                        Required.
#   --expire-ref=<ref>   Operator-vouched named ref R used as the ancestry
#                        threshold.  Must resolve in the repository.
#                        When omitted, the script prints the report but
#                        deletes nothing and exits 0.
#   --dry-run            Print what would happen but delete nothing even if
#                        --expire-ref is given.
#   --output=<file>      Append the report to <file> in addition to stdout.
#                        Omit to write to stdout only.
#
# Predicate (decided 2026-06-15; see docs/sync-state-space/rwv-gc-verb.md §4):
#   B = merge-base(sp_tip, HEAD)
#   Reap iff is_ancestor(B, R)
#
# Structural predicates only — no wall-clock, no age heuristics.
#
# Report is always produced before any deletion.  The caller is responsible
# for keeping the output (pipe to a file with --output or redirect stdout).
#
# Tombstone note (savepoint-gc.md §7):
#   --discard-local-commits refs are indistinguishable on disk once their
#   record is cleared.  Report-before-drop is the only guard.  No
#   heuristic is applied here to identify them; the operator reviews the
#   report before running without --dry-run.

set -euo pipefail

REPO=""
EXPIRE_REF=""
DRY_RUN=0
OUTPUT_FILE=""

for arg in "$@"; do
    case "$arg" in
        --repo=*)        REPO="${arg#--repo=}"        ;;
        --expire-ref=*)  EXPIRE_REF="${arg#--expire-ref=}"  ;;
        --dry-run)       DRY_RUN=1                    ;;
        --output=*)      OUTPUT_FILE="${arg#--output=}"  ;;
        *)
            printf 'unknown argument: %s\n' "$arg" >&2
            printf 'Usage: %s --repo=<path> [--expire-ref=<ref>] [--dry-run] [--output=<file>]\n' "$0" >&2
            exit 1
            ;;
    esac
done

if [ -z "$REPO" ]; then
    printf 'error: --repo is required\n' >&2
    exit 1
fi

if [ ! -e "$REPO" ]; then
    printf 'error: repo path does not exist: %s\n' "$REPO" >&2
    exit 1
fi

# Canonicalize repo path for git -C.
REPO="$(cd "$REPO" && pwd)"

git_repo() {
    git -C "$REPO" "$@"
}

# Verify the repo is a valid git repository.
if ! git_repo rev-parse --git-dir >/dev/null 2>&1; then
    printf 'error: not a git repository: %s\n' "$REPO" >&2
    exit 1
fi

# Resolve R; refuse early if named but unresolvable.
R_SHA=""
if [ -n "$EXPIRE_REF" ]; then
    if ! R_SHA="$(git_repo rev-parse --verify "$EXPIRE_REF" 2>/dev/null)"; then
        printf 'error: --expire-ref %s does not resolve in %s\n' "$EXPIRE_REF" "$REPO" >&2
        exit 1
    fi
fi

HEAD_SHA="$(git_repo rev-parse HEAD 2>/dev/null)" || {
    printf 'error: cannot resolve HEAD in %s\n' "$REPO" >&2
    exit 1
}

# Collect all pre-op savepoint refs.
SAVEPOINT_PREFIX="refs/rwv/pre-op/"
mapfile -t SP_REFS < <(git_repo for-each-ref --format='%(refname)' "${SAVEPOINT_PREFIX}" 2>/dev/null || true)

TIMESTAMP="$(date -u '+%Y-%m-%dT%H:%M:%SZ')"

emit() {
    if [ -n "$OUTPUT_FILE" ]; then
        printf '%s\n' "$@" | tee -a "$OUTPUT_FILE"
    else
        printf '%s\n' "$@"
    fi
}

emit "# orphaned-savepoint reap report"
emit "# repo:       $REPO"
emit "# HEAD:       $HEAD_SHA"
if [ -n "$EXPIRE_REF" ]; then
    emit "# expire-ref: $EXPIRE_REF ($R_SHA)"
else
    emit "# expire-ref: (none — no refs will be deleted)"
fi
emit "# dry-run:    $( [ "$DRY_RUN" -eq 1 ] && printf 'yes' || printf 'no' )"
emit "# generated:  $TIMESTAMP"
emit "# found:      ${#SP_REFS[@]} refs/rwv/pre-op/* refs"
emit "#"

if [ "${#SP_REFS[@]}" -eq 0 ]; then
    emit "# (no refs to process)"
    emit ""
    emit "Summary: 0 WOULD-REAP, 0 KEEP, 0 deleted."
    exit 0
fi

WOULD_REAP=()
KEEP=()

for ref in "${SP_REFS[@]}"; do
    sp_tip="$(git_repo rev-parse --verify "$ref" 2>/dev/null)" || {
        emit "SKIP $ref (ref no longer resolves)"
        continue
    }

    # B = merge-base(sp_tip, HEAD)
    # merge-base exits non-zero if there is no common ancestor.
    if ! B="$(git_repo merge-base "$sp_tip" HEAD 2>/dev/null)"; then
        emit "KEEP $ref  # no common ancestor with HEAD — cannot determine ancestry"
        KEEP+=("$ref")
        continue
    fi

    if [ -n "$EXPIRE_REF" ]; then
        # is_ancestor(B, R): B is an ancestor of R iff merge-base(B, R) == B.
        if git_repo merge-base --is-ancestor "$B" "$R_SHA" 2>/dev/null; then
            emit "WOULD-REAP $ref  sp_tip=$sp_tip  B=$B"
            WOULD_REAP+=("$ref")
        else
            emit "KEEP $ref  sp_tip=$sp_tip  B=$B  # B not an ancestor of R"
            KEEP+=("$ref")
        fi
    else
        # No R given — report only, cannot determine disposition.
        emit "REPORT $ref  sp_tip=$sp_tip  B=$B  # no expire-ref given; not reaped"
        KEEP+=("$ref")
    fi
done

emit ""
emit "Summary: ${#WOULD_REAP[@]} WOULD-REAP, ${#KEEP[@]} KEEP."

if [ "${#WOULD_REAP[@]}" -eq 0 ]; then
    exit 0
fi

if [ -z "$EXPIRE_REF" ] || [ "$DRY_RUN" -eq 1 ]; then
    if [ "$DRY_RUN" -eq 1 ]; then
        emit "Dry run — no refs deleted."
    else
        emit "No --expire-ref given — no refs deleted."
    fi
    exit 0
fi

emit ""
emit "Deleting ${#WOULD_REAP[@]} refs..."
DELETED=0
FAILED=0
for ref in "${WOULD_REAP[@]}"; do
    if git_repo update-ref -d "$ref" 2>/dev/null; then
        emit "DELETED $ref"
        DELETED=$(( DELETED + 1 ))
    else
        emit "FAILED  $ref  (ref may have changed; skipping)"
        FAILED=$(( FAILED + 1 ))
    fi
done

emit ""
emit "Done: $DELETED deleted, $FAILED failed."

if [ "$FAILED" -gt 0 ]; then
    exit 1
fi
