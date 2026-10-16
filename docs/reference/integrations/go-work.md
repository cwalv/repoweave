# go-work

Merges a managed `use (...)` block into a project-level `go.work` file listing every active member repo (excluding `reference` repos) that contains a `go.mod`.

| | |
|---|---|
| Default enabled | yes |
| Auto-detects | member repos with `go.mod` |
| Manages | `go.work` (hybrid merge) |
| Install hook | — |

## Configuration

Optional `go-version` override in `rwv.toml`:

```toml
[integrations.go-work]
go-version = "1.26"
```

## Managed keys

| Directive | Ownership | Behavior |
|---|---|---|
| `use (...)` | `Author` | Preceded by `// managed by repoweave`. Updated on authoring verbs (`rwv add`, `rwv remove`, `rwv update`, `rwv role`, `rwv doctor --fix`) via `go work` when `go` is on `PATH`, or via the built-in line-region editor when `go` is absent. |
| `go <version>` | `DefaultOnly` | Seeded only when `go.work` has no `go` directive yet, from `[integrations.go-work] go-version` or from the highest `go` directive across member `go.mod` files. Once present, `rwv` never overwrites or strips it (even when `go work use` would otherwise raise it). |

All other directives (`replace`, `toolchain`, `godebug`) and comments are user-owned and preserved byte-for-byte.

## Generated file

```go
go 1.26

// managed by repoweave
use (
	./github/chatly/protocol
	./github/chatly/server
)
```

The `go` directive is seeded from `[integrations.go-work] go-version` when configured, or from the maximum Go version declared across the active member repos' `go.mod` files.

The seed applies only when `go.work` has no `go` line yet. An existing `go` directive in the file is preserved unmodified whether or not `go` is on `PATH`. If your pin sits below what a member repo's `go.mod` requires (`>= 1.21`), `rwv doctor` reports the incompatibility instead of rewriting the pin.

If no member declares a `go` directive and `go-version` is not set in `rwv.toml`, the generated `go.work` omits the `go` line rather than stamping the host machine's installed Go version into a committed file. Set `go-version` if you want an explicit pin.

Written in `projects/<project>/go.work` and surfaced at the weave root via symlink. Committable. The corresponding `go.work.sum` is produced by the Go toolchain when Go commands run; its commit-vs-gitignore status is the operator's choice — see [Committed files and committability](./index.md#committed-files-and-committability).

## Deactivation

Gated on the `// managed by repoweave` marker: strips the `use (...)` block and the marker comment while preserving `go`, `toolchain`, `replace`, `godebug`, and user comments. Deletes `go.work` only when no `use` entries and no `replace`, `godebug`, or user comments beyond `go` and `toolchain` remain. Leaves unmarked `go.work` files and `go.work.sum` ([commit policy](./index.md#committed-files-and-committability)) untouched.

## Check

`rwv doctor` reports:

- **`tool-missing`** (warning): member repos with `go.mod` are present, but `go` is not on `PATH` (`rwv` uses its built-in fallback editor for `go.work`).
- **`member-incompatibility`** (warning, `safe_to_fix = false`): the on-disk `go` directive in `go.work` is lower than the highest `go` requirement across member `go.mod` files (enforced by the Go toolchain when a member requires `go >= 1.21`). Update the `go` directive in `projects/<project>/go.work` (or lower the member's `go.mod` requirement) to resolve.

## Verify (drift)

`rwv doctor` and context verbs (`rwv activate`, `rwv fetch`, `rwv workweave create`) run the read-only Axis-2 verify pass on `projects/<project>/go.work`:

- **MISSING** (`managed-file-missing`, `safe_to_fix = true`): `go.work` is absent while Go member repos are present (`rwv doctor --fix` creates it).
- **DRIFT** (`managed-file-drift`, `safe_to_fix = true`): `// managed by repoweave` is present, but the `use` entries differ from what `rwv.toml` declares (or no Go member repos remain and the marked `use` block is orphaned).
- **USER-HELD** (`managed-file-drift`, `safe_to_fix = false`): `go.work` has a `use` block without `// managed by repoweave`. `rwv` leaves the file untouched; add `// managed by repoweave` above `use (` or remove the `use` block to hand ownership to `rwv`.
