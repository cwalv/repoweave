# go-work

Generates a `go.work` file listing every project repo (excluding `reference` repos) that contains a `go.mod`.

| | |
|---|---|
| Default enabled | yes |
| Auto-detects | repos with `go.mod` |
| Generates | `go.work` |
| Install hook | — |

## Generated file

```
go 1.26

use (
    ./github/chatly/protocol
    ./github/chatly/server
)
```

The `go` line is seeded from the maximum Go version declared across the member
repos' `go.mod` files (`max_go_version()`), or from the integration's
`go-version` setting when you set one. The value above is illustrative; your
workspace will show whatever version your repos declare.

The seed applies only when `go.work` has no `go` line yet. A version already in
the file is yours: repoweave neither raises nor lowers it, whether or not `go`
is on PATH. If your pin sits below what a member's `go.mod` requires, `rwv
doctor` reports the incompatibility instead of rewriting the pin.

If no member declares a `go` directive and you have set no `go-version`, the
generated `go.work` carries no `go` line, and the Go toolchain accepts that. The
absence is deliberate: the only version available to write would be the one the
locally installed Go stamps into a new `go.work`, which would make a committed
file a property of whichever machine ran the command first — and repoweave would
then preserve that accident as though you had chosen it. Set `go-version` if you
want a pin.

Generated in the project directory, symlinked to the weave directory. Committable. The corresponding `go.sum` is produced by the Go toolchain and is also committable persistent state.

## Deactivation

Removes `go.work`. Does not remove `go.sum`.

## Check

No checks currently. Could warn if `go` is not on PATH when Go repos are present.
