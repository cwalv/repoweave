# Changelog

This changelog starts at 0.23.0. Releases 0.4.0 through 0.22.0 shipped without
entries, and none are reconstructed here — the gap is a gap in the record, not a
claim that nothing changed in them.

## 0.23.0

### Refusals carry stable names

When `rwv` declines to act on purpose, it now prints a stable kebab-case token
for the *condition* and ends the message with `rwv explain <token>`. The token
names the condition rather than the message, which may be reworded, or the site,
which may move — so what you looked up once stays lookupable.

- `rwv explain <token>` serves that token's entry. The entry is sliced out of the
  published page rather than kept as a second copy, so the terminal and the site
  cannot drift into two spellings.
- `docs/reference/refusals.md` is new and carries one entry per refusal-only
  token, 78 of them. A condition `rwv doctor` also reports keeps its single entry
  on `docs/reference/doctor-findings.md`: one condition has one name and one
  entry, whichever surface you met it on.
- `rwv explain` also serves the VCS wire kinds and the sync failure kinds
  `ff-impossible` and `rebase-failed`. `rwv sync --json` published those and
  nothing explained them.
- The suggestion hint for an unrecognised name now spans tokens as well as verbs.

### Breaking: three tokens leave the `--json` VCS wire surface

`branch-already-exists`, `worktree-exists` and `uncommitted-changes` are gone
from `VcsErrorOutput`. A consumer branching on `cause.kind` can no longer receive
them — it never could. Each was published, schema-conformant and
conformance-tested, and no production path constructed any of them.

The conditions themselves are still reported, and always were, under other
names. A `worktree add` onto a taken path and a git call that fails on a dirty
tree come back as `command-failed` carrying git's own stderr, and
`dirty-checkout` names the dirty-tree condition on the refusal register.

### Fixed

- `rwv activate` no longer dies with a `rwv doctor --fix` remedy when an
  `[integrations.<name>]` block does not parse. It refuses the install hooks up
  front and tells you to edit `rwv.toml`. The old advice pointed at a
  regeneration that has to read the block it had just failed to read.
- Malformed integration settings are their own condition, `malformed-settings`,
  on every verb that meets them. They used to surface as `integration-failed` —
  the runner's one catch-all kind, naming neither the field nor a remedy — and
  `rwv activate` was the last verb still routing them there. `--fix` no longer
  offers a repair for them, because the repair is an edit to `rwv.toml`.
- `rwv add <path> --new --role <role>` records the role you asked for. It parsed
  `--role` and wrote `owned` regardless.
- Errors from `rwv sync`, `rwv sync-to` and the workweave phase transitions reach
  you whole. Ten printing sites rendered only an error's top sentence and dropped
  the context beneath it — including git's own account of why it refused. They
  print the full chain now.
- `rwv sync --json` carries a typed cause on the `ff-impossible` arm. The arm had
  the `VcsError` in hand and discarded it, so a consumer following the documented
  advice to branch on `cause.kind` instead of parsing the message got nothing to
  branch on.
- `untracked-collision` is reported only for untracked collisions. A fast-forward
  refused because a *tracked* file was modified was reported under that name,
  carrying a remedy — move the file — that does not apply to it. The obstructing
  set is now computed instead of parsed back out of git's refusal text, which
  also covers hosts that disable `advice.commitBeforeMerge` (GitHub's macOS
  runner image among them); there the parse recovered nothing and a collision
  arrived as `command-failed` with no path and no `--continue` hint.
- `rwv doctor` sees members outside the built-in registry segments. `rwv add`
  mints the first path segment from the source URL — `local` for `file://`, the
  URL's own host for anything else — while the disk scan walked three hardcoded
  directories. A member outside those three was invisible to every scan keyed on
  that walk: its manifest entry reported as `dangling-reference` advising
  `rwv fetch`, which cannot clear it, and its lock was never compared against its
  HEAD. The walk set is derived from what the manifests name.
- `unreadable-owned-state` advises a repair that works. In a project with no
  cargo workspace, `rwv materialize` exited 0 and left the record byte-identical;
  it rebuilds the ledger there now. Separately, the finding walks every project
  while the repair reaches only the presented one, so the advice names the
  activation route when the affected project is not the active one.
- A marker carrying `primary:` with no usable `project:` no longer reports as
  auto-fixable and then errors under `--fix`. It reports unreadable, with a
  detail that says why.
- `Error: Error:` is gone. Four `rwv add` / `rwv remove` messages spelled their
  own decoration in front of the one the reporter adds, and `rwv fetch`'s
  occupied-project refusal reached stderr twice.
- `rwv explain <unknown>` no longer answers with a suggestion that replaces the
  whole input. The threshold was a flat edit distance of 2, which puts the
  two-character token `io` within reach of any name four characters or shorter —
  and since a suggestion is checked before the external-command fallback, an
  operator running a real plugin verb was told about `io` instead of being
  pointed at their own tool.
- An undeclared-link finding spells its target the way `rwv` spells it, so the
  path in the message can be pasted back into the manifest the name came from.
  On Windows the two halves of that sentence disagreed.
- The refusal over a persisted `strategy="merge"` op-state names its exit —
  `rwv abort`, then re-invoke. That path existed only in a source comment.

### Documentation

- `docs/reference/refusals.md`: new, one entry per refusal token.
- `docs/reference/doctor-findings.md`: one servable entry per integration kind.
- `--remove-undeclared-links` is documented in the surfacing how-to. The flag
  already shipped in 0.22.0; only its documentation is new.
- Registry documentation corrected on four points: `RepoUrl` has no `File`
  variant; registries are fixed at build time rather than user-configurable;
  `--new` takes a canonical path and derives the URL from it, not the reverse;
  and `rwv.toml` is TOML, not YAML.
- Anchors under `docs/` are checked, and the three that did not resolve are
  repaired.

### Internal

- Blocking CI runs `cargo test --no-fail-fast`, so a red leg reports every
  failure instead of a lower bound on them.
- The internals-on-operator-surfaces gate covers every page `docs/SUMMARY.md`
  lists, not only the two generated directories.
- Test and census work across the refusal register, doctor findings, the skip
  audit and the lock-freshness fixtures. No operator-visible effect.
