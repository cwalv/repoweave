#!/usr/bin/env bash
set -euo pipefail

# Claude Code WorktreeCreate hook for repoweave workweaves.
# Reads JSON from stdin, creates a multi-repo workweave, prints the path.
#
# Dependencies: jq, rwv
#
# Input (stdin JSON):
#   { "cwd": "...", "branch_name": "...", "hook_event_name": "WorktreeCreate", ... }
#
# Output (stdout):
#   Absolute path to the created workweave directory.
#
# This hook replaces Claude Code's default worktree creation entirely.
# It must print only the workweave root path to stdout — no other output.

input=$(cat)
cwd=$(echo "$input" | jq -r '.cwd')
branch_name=$(echo "$input" | jq -r '.branch_name')
session_id=$(echo "$input" | jq -r '.session_id // empty')

# Find workspace root via rwv resolve, then read .rwv-active
ws_root=$(cd "$cwd" && rwv resolve 2>/dev/null) || {
    echo "error: could not resolve repoweave workspace from ${cwd}" >&2
    exit 1
}
active_file="${ws_root}/.rwv-active"
if [ ! -f "$active_file" ]; then
    echo "error: no .rwv-active found in ${ws_root}" >&2
    exit 1
fi
project=$(cat "$active_file" | tr -d '[:space:]')

# Derive workweave name from branch_name, falling back to session_id or timestamp.
# branch_name can arrive as the literal string "null" when Claude Code fires
# WorktreeCreate for a subagent without a real branch name.
if [ -z "$branch_name" ] || [ "$branch_name" = "null" ]; then
    if [ -n "$session_id" ] && [ "$session_id" != "null" ]; then
        raw_name="$session_id"
    else
        raw_name="ww-$(date +%s)"
    fi
else
    raw_name="$branch_name"
fi

# Sanitize / → - for filesystem safety
name=$(echo "$raw_name" | tr '/' '-')

# Create the workweave — hook-mode prints only the path to stdout
exec rwv workweave "$project" "$name" --hook-mode
