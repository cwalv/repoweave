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

# Derive project from .rwv-active
active_file="${cwd}/.rwv-active"
if [ ! -f "$active_file" ]; then
    echo "error: no .rwv-active found in ${cwd}" >&2
    exit 1
fi
project=$(cat "$active_file" | tr -d '[:space:]')

# Derive workweave name from branch_name (sanitize / → - for filesystem safety)
name=$(echo "$branch_name" | tr '/' '-')

# Create the workweave — hook-mode prints only the path to stdout
exec rwv workweave "$project" "$name" --hook-mode
