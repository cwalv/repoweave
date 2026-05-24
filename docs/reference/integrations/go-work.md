# go-work

Generates a `go.work` file listing every project repo (excluding `reference` repos) that contains a `go.mod`.

| | |
|---|---|
| Default enabled | yes |
| Auto-detects | repos with `go.mod` |
| Generates | `go.work` |
| Lock hook | — |

## Generated file

```
go 1.21

use (
    ./github/chatly/protocol
    ./github/chatly/server
)
```

Generated in the project directory, symlinked to the weave directory. Committable. The corresponding `go.sum` is produced by the Go toolchain and is also committable persistent state.

## Deactivation

Removes `go.work`. Does not remove `go.sum`.

## Check

No checks currently. Could warn if `go` is not on PATH when Go repos are present.
