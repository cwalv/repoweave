#!/usr/bin/env bash
set -euo pipefail

# E2E test: Claude Code workweave isolation via --claude-hook.
# Requires: claude CLI on PATH with valid auth, rwv dev build installed.
# Gate: only runs when RWV_E2E_CLAUDE=1 is set.
#
# Usage:
#   RWV_E2E_CLAUDE=1 ./tests/e2e-claude-workweave.sh

if [ "${RWV_E2E_CLAUDE:-}" != "1" ]; then
    echo "SKIP: set RWV_E2E_CLAUDE=1 to run this test"
    exit 0
fi

# Verify prerequisites
command -v claude >/dev/null 2>&1 || { echo "FAIL: claude CLI not on PATH"; exit 1; }
command -v rwv >/dev/null 2>&1 || { echo "FAIL: rwv not on PATH"; exit 1; }

echo "rwv version: $(rwv --version)"

# Determine workspace root
WS_ROOT=$(cd "$(dirname "$0")/.." && rwv resolve 2>/dev/null) || {
    # Fallback: try from cwd
    WS_ROOT=$(rwv resolve 2>/dev/null) || {
        echo "FAIL: cannot resolve repoweave workspace"
        exit 1
    }
}
echo "workspace root: $WS_ROOT"

WORKWEAVES_DIR=$(dirname "$WS_ROOT")/.workweaves
echo "workweaves dir: $WORKWEAVES_DIR"

# Count workweaves before
before_count=$(ls "$WORKWEAVES_DIR" 2>/dev/null | wc -l)

# Run claude with worktree isolation — a simple prompt that reports its environment
echo "--- spawning claude --worktree ---"
output=$(claude --worktree -p "Run these commands and report the output: pwd; cat .rwv-workweave; cat .rwv-active; ls github/" 2>&1) || true
echo "$output"

# Count workweaves after — there may be one left if WorktreeRemove didn't fire
after_count=$(ls "$WORKWEAVES_DIR" 2>/dev/null | wc -l)

# Check that a workweave was created (after >= before, or a new one exists)
if [ "$after_count" -gt "$before_count" ]; then
    new_ww=$(ls -t "$WORKWEAVES_DIR" 2>/dev/null | head -1)
    echo "--- workweave created: $new_ww ---"

    # Verify structure
    ww_path="$WORKWEAVES_DIR/$new_ww"
    [ -f "$ww_path/.rwv-workweave" ] && echo "PASS: .rwv-workweave exists" || echo "FAIL: .rwv-workweave missing"
    [ -f "$ww_path/.rwv-active" ] && echo "PASS: .rwv-active exists" || echo "FAIL: .rwv-active missing"
    [ -d "$ww_path/github" ] && echo "PASS: github/ exists" || echo "FAIL: github/ missing"

    # Clean up
    echo "--- cleaning up ---"
    project=$(cat "$ww_path/.rwv-active" 2>/dev/null | tr -d '[:space:]')
    name="${new_ww#*--}"
    rwv workweave "$project" delete "$name" 2>&1 || echo "WARN: delete had errors"

    [ ! -d "$ww_path" ] && echo "PASS: cleanup successful" || echo "FAIL: workweave dir still exists"
else
    # The workweave may have been created and cleaned up already
    echo "--- checking claude output for workweave evidence ---"
    if echo "$output" | grep -q ".workweaves/"; then
        echo "PASS: claude output references a workweave path"
    else
        echo "WARN: no workweave evidence in output (may have been created and auto-cleaned)"
    fi
fi

echo "--- e2e test complete ---"
