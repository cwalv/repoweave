# vscode-workspace

Manages a hybrid `{project}.code-workspace` file in `projects/<project>/` and surfaces it at the weave or workweave root via symlink. Configures a single-root workspace (`"."`) with Git discovery settings and `files.exclude` rules tailored to the active project.

| | |
|---|---|
| Default enabled | yes |
| Auto-detects | all repos |
| Manages | `{project}.code-workspace` (hybrid merge) |
| Install hook | — |

The file is named after the project segment (e.g., `web-app.code-workspace`), making the active project visible in the VS Code title bar.

## Configuration

Enabled by default. Optional settings in `rwv.toml`:

```toml
[integrations.vscode-workspace]
hide-dotfiles = true   # default: true; adds ".*": true to settings.files.exclude
```

## Generated file

```json
{
  "folders": [
    {
      "name": "web-app (primary)",
      "path": "."
    }
  ],
  "rwv.generated": {
    "files.exclude": [
      ".*",
      "github/other-org",
      "projects/other-app"
    ],
    "managed": true
  },
  "settings": {
    "files.exclude": {
      ".*": true,
      "github/other-org": true,
      "projects/other-app": true
    },
    "git.autoRepositoryDetection": "subFolders",
    "git.repositoryScanMaxDepth": 3
  }
}
```

- **Primary root folder (`folders[0]`)** at `"."` with `"name": "<project> (primary)"`. Any user-added folder entries (where `"path" != "."`) are preserved in their original order after `"."`.
- **`git.autoRepositoryDetection: "subFolders"`** and **`git.repositoryScanMaxDepth: 3`** (`DefaultOnly`) are seeded when absent so VS Code discovers repos at the `registry/owner/repo` depth without walking up to a parent repo. If you change either value, `rwv` preserves your choice.
- **`settings.files.exclude`** hides on-disk repos not belonging to the active project, sibling `projects/<other>` directories, and `".*"` when `hide-dotfiles = true`.
  - **Hierarchy collapsing**: if all on-disk repos under `registry/owner` are excluded, they collapse to a single `"registry/owner": true` entry; if all owners under `registry` are excluded, they collapse to `"registry": true`.
  - **Workweave monotone preservation**: because a workweave materializes only its project's repos, authoring inside a workweave preserves previously recorded `rwv.generated["files.exclude"]` entries for paths not present on disk in that workweave, preventing a workweave regeneration from shrinking the committed exclusion set used by the primary weave.
  - **User exclude keys**: any `settings.files.exclude` keys not listed in `rwv.generated["files.exclude"]` are preserved across merges.

## Ownership

`folders` is `rwv`'s owned region probe, and `"rwv.generated"` is the top-level ownership marker. A `.code-workspace` file that has a `folders` array but no `"rwv.generated"` marker was authored by hand: `rwv` leaves it byte-for-byte alone on authoring verbs, and `rwv doctor` reports it as `USER-HELD` (not auto-fixable). A file with no `folders` key has no owned region to conflict with, so `rwv` writes `folders`, `settings`, and `"rwv.generated"` while preserving any other top-level blocks (`extensions`, `launch`, `tasks`).

**Taking the pen** — delete the `"rwv.generated"` key. `rwv` stops writing the file and only reports on it. Changing `rwv`'s managed values while leaving `"rwv.generated"` in place does not take the pen; the next authoring verb (`rwv add`, `remove`, `update`, `role`, `doctor --fix`) rewrites them.

## Deactivation

Strips `rwv`'s managed entries from each `.code-workspace` file in `projects/<project>/` carrying the `"rwv.generated"` marker: the `"."` folder entry, the `files.exclude` keys recorded in `rwv.generated["files.exclude"]`, and the `"rwv.generated"` marker itself. Everything else stays and the file is rewritten as a hand-owned workspace — including the `git.*` settings, which `rwv` seeds but never removes. Files without the marker are not touched.

The file is deleted only when nothing user-authored remains after stripping: no extra folder entries, no user `files.exclude` keys, no other top-level blocks, and `settings` holding at most the unmodified `git.*` defaults `rwv` seeded.

## Check

`rwv doctor` reports `managed-file-missing` (warning, auto-fixable) if `projects/<project>/{project}.code-workspace` does not exist as a file.

## Verify (drift)

`rwv doctor` and context verbs (`rwv activate`, `rwv fetch`, `rwv workweave create`) run the read-only verify pass on `projects/<project>/{project}.code-workspace`:

- **`malformed-settings`** (error): unknown keys or invalid values in `[integrations.vscode-workspace]`.
- **`MISSING`** (`managed-file-missing`, warning, auto-fixable via `rwv doctor --fix`): file is absent in `projects/<project>/`.
- **`USER-HELD`** (`user-held-featured`, warning, not auto-fixable): `folders` is present without the `"rwv.generated"` marker.
- **`DRIFT`** (`managed-file-drift`, warning, auto-fixable via `rwv doctor --fix`): `"rwv.generated"` is present, but `folders[0]` or the managed `settings.files.exclude` keys differ from what `rwv` would write in the current container.
- When the integration is disabled (`enabled = false`), a leftover `.code-workspace` file still carrying `"rwv.generated"` is reported as `orphaned-owned-region` (auto-fixable via `rwv doctor --fix` or `rwv materialize`).
