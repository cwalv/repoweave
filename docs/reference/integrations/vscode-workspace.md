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
- **Merge on activate** — only `folders` and managed `settings` keys are replaced. Other keys (extensions, launch configs, other settings) are preserved, so user customizations survive re-activation.

## Deactivation

Removes any `.code-workspace` files from the directory.

## Check

Validates that the `.code-workspace` file exists as a regular file (not a symlink) in the directory.
