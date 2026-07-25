# vscode-workspace

Generates a `{project}.code-workspace` file in the weave (or workweave) directory. Uses a single-root workspace (the directory itself) with git settings configured for the multi-repo layout.

| | |
|---|---|
| Default enabled | yes |
| Auto-detects | all repos |
| Generates | `{project}.code-workspace` |
| Lock hook | — |

The file is named after the project (e.g., `web-app.code-workspace`), making the project visible in the VS Code title bar.

## Generated file

```json
{
  "folders": [
    { "path": ".", "name": "web-app (weave)" }
  ],
  "settings": {
    "git.autoRepositoryDetection": "subFolders",
    "git.repositoryScanMaxDepth": 3
  }
}
```

- **Single root folder** at `"."` — the weave or workweave directory.
- **Folder name** includes the context (e.g., `"web-app (weave)"`, `"web-app (agent-42)"`).
- **`git.autoRepositoryDetection: subFolders`** prevents VS Code from walking up to a parent repo.
- **`git.repositoryScanMaxDepth: 3`** ensures VS Code discovers repos at the `registry/owner/repo` depth.
- **Merge on activate** — rwv writes the `"."` folder entry and its own `files.exclude` keys, and seeds the `git.*` settings once. Extra folder entries, user-added `files.exclude` keys, and other blocks (extensions, launch configs, other settings) are preserved, so user customizations survive re-activation.

## Ownership

`folders` is rwv's owned region. A `.code-workspace` that has a `folders` array but no `rwv.generated` marker was authored by hand: rwv leaves it byte-for-byte alone on activate, and `rwv doctor` reports it as user-held (not auto-fixable). A file with no `folders` has no owned region to take over, so rwv creates the key and the marker and manages it from that point on, merging around whatever blocks are already there.

**Taking the pen** — delete the `rwv.generated` key. rwv stops writing the file and only reports on it. Changing rwv's *values* while leaving the marker in place does not take the pen; the next intent verb rewrites them.

## Deactivation

Strips rwv's keys from each `.code-workspace` file carrying the `rwv.generated` marker: the `"."` folder entry, the `files.exclude` keys the marker records, and the marker itself. Everything else stays and the file is rewritten as a hand-owned workspace — including the `git.*` settings, which rwv seeds but never removes. Files without the marker are not touched.

The file is deleted only when nothing user-authored would remain: no extra folder entries, no unrecorded `files.exclude` keys, no other blocks, and the `git.*` settings still at the values rwv seeded.

## Check

Validates that the `.code-workspace` file exists as a regular file (not a symlink) in the directory.
