#!/usr/bin/env bash
set -euo pipefail

# Claude Code WorktreeRemove hook for repoweave workweaves.
# Reads JSON from stdin, deletes the workweave.
#
# Dependencies: jq, rwv
#
# Input (stdin JSON):
#   { "worktree_path": "...", "hook_event_name": "WorktreeRemove", ... }
#
# This hook is fire-and-forget: it exits 0 even if cleanup fails,
# so Claude Code is never blocked by a stale workweave.

input=$(cat)
worktree_path=$(echo "$input" | jq -r '.worktree_path')

# Read the marker to get the project name
marker="${worktree_path}/.rwv-workweave"
if [ ! -f "$marker" ]; then
    echo "warning: no .rwv-workweave marker in ${worktree_path}, skipping cleanup" >&2
    exit 0
fi

project=$(grep 'project:' "$marker" | sed 's/project: *//' | tr -d '[:space:]')

# Derive workweave name from directory basename.
# The directory is named {primary}--{name}, so strip the primary-- prefix.
dir_basename=$(basename "$worktree_path")
name="${dir_basename#*--}"

rwv workweave "$project" "$name" --delete 2>/dev/null || true
