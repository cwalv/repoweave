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

The `go` line reflects the maximum Go version declared across the member repos'
`go.mod` files (`max_go_version()`). The value above is illustrative; your
workspace will show whatever version your repos declare.

Generated in the project directory, symlinked to the weave directory. Committable. The corresponding `go.sum` is produced by the Go toolchain and is also committable persistent state.

## Deactivation

Removes `go.work`. Does not remove `go.sum`.

## Check

No checks currently. Could warn if `go` is not on PATH when Go repos are present.
