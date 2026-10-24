# go-work

Merges a managed `use (...)` block into a project-level `go.work` file listing every active member repo (excluding `reference` repos) that contains a `go.mod`.

| | |
|---|---|
| Default enabled | yes |
| Auto-detects | member repos with `go.mod` |
| Manages | `go.work` (hybrid merge) |
| Generates | — (`go.work.sum` is surfaced, but written by the Go toolchain) |
| Install hook | — |

## Configuration

Enabled by default. The table takes `enabled` and an optional `go-version`:

```toml
[integrations.go-work]
go-version = "1.26"
```

`go-version` is the value seeded into a `go.work` that has no `go` directive yet; see [Managed and generated files](#managed-and-generated-files). A `go-version` that is not a string is reported by `rwv doctor` as [`malformed-settings`](../doctor-findings.md#malformed-settings), and `go.work` is not authored until it is corrected.

## Managed and generated files

| Directive | Ownership | Behavior |
|---|---|---|
| `use (...)` | `Author` | Preceded by `// managed by repoweave`. Updated on authoring verbs (`rwv add`, `rwv remove`, `rwv update`, `rwv role`, `rwv doctor --fix`) via `go work` when `go` is on `PATH`, or via the built-in line-region editor when `go` is absent. |
| `go <version>` | `DefaultOnly` | Seeded only when `go.work` has no `go` directive yet, from `[integrations.go-work] go-version` or from the highest `go` directive across member `go.mod` files. Once present, `rwv` never overwrites or strips it (even when `go work use` would otherwise raise it). |

All other directives (`replace`, `toolchain`, `godebug`) and comments are user-owned and preserved byte-for-byte.

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

Written in `projects/<project>/go.work` and surfaced at the weave root via symlink. Committable. The corresponding `go.work.sum` is produced by the Go toolchain when Go commands run; its weave-root link exists before the file does, so the toolchain's write lands in the project directory. Its commit-vs-gitignore status is the operator's choice — see [Committed files and committability](./index.md#committed-files-and-committability).

## Install hook

None. `go.work.sum` appears when Go tooling runs against `go.work`.

## Deactivation

Gated on the `// managed by repoweave` marker: strips the `use (...)` block and the marker comment while preserving `go`, `toolchain`, `replace`, `godebug`, and user comments. Deletes `go.work` only when no `use` entries and no `replace`, `godebug`, or user comments beyond `go` and `toolchain` remain. Unmarked `go.work` files and `go.work.sum` ([commit policy](./index.md#committed-files-and-committability)) are left untouched.

- **Disabled** (`enabled = false`): `rwv materialize` strips the marked block (deleting a `go.work` that held nothing else of yours) and removes the weave-root `go.work` and `go.work.sum` links.
- **Last Go member removed**: the authoring verb strips the marked block on the same rule. The weave-root links stay; `rwv doctor` reports them as `surfacing` warnings, and `rwv materialize --remove-undeclared-links` removes them.

## Diagnostics

Reported by `rwv doctor`. Verify rows also print as warnings on `rwv activate`, `rwv materialize`, `rwv workweave create` and the activation a `rwv fetch` performs; see [Hook points](./index.md#hook-points).

| Kind | Severity | Condition | Repair |
|---|---|---|---|
| [`tool-missing`](../doctor-findings.md#tool-missing) | warning | Member repos with `go.mod` are present, but `go` is not on `PATH`. `rwv` edits `go.work` with its built-in fallback editor. | Install `go`, or accept the fallback. |
| [`member-incompatibility`](../doctor-findings.md#member-incompatibility) | warning | The `go` directive in `go.work` is lower than the highest `go` requirement across member `go.mod` files, and that requirement is `1.21` or later (where the Go toolchain enforces it). | Raise the `go` directive in `projects/<project>/go.work`, or lower the member's `go.mod` requirement. |
| [`managed-file-missing`](../doctor-findings.md#managed-file-missing) | warning | `go.work` is absent while Go member repos are present. | `rwv doctor --fix` |
| [`managed-file-drift`](../doctor-findings.md#managed-file-drift) | warning | `// managed by repoweave` is present, but the `use` entries differ from what `rwv.toml` declares, or no Go member repos remain and the marked `use` block is still there. | `rwv doctor --fix` |
| [`managed-file-user-held`](../doctor-findings.md#managed-file-user-held) | warning | `go.work` has a `use` block without `// managed by repoweave`. `rwv` leaves the file untouched. | Add `// managed by repoweave` above `use (`, or remove the `use` block to hand ownership to `rwv`. |
| [`disabled-integration-artifact`](../doctor-findings.md#disabled-integration-artifact) | warning | The integration is disabled, and the marked `use` block is still on disk. | `rwv materialize` |

## Related

- [Integrations reference](./index.md) — hook points and the committability of `go.work.sum`
- [add an integration](../../how-to/add-an-integration.md) — enabling and disabling integrations
