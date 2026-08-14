# rwv doctor

## Purpose

Run convention checks on the workspace. By default, checks are scoped to
the active project: its manifest, lock, workspace files, and integration
health (cargo-workspace, vscode-workspace, etc.). Use `--all` to run the
full weave-wide scan that includes orphan detection across every project.

The check is intentionally pure: filesystem scanning happens up front, then
a closed enum (`CheckViolation`) is reduced to violations. Each variant has
a stable kebab-case `kind` tag — agents key off `kind` to dispatch
follow-up actions.

## Invocation

```
rwv doctor [--all] [--locked] [--json] [--fix] [--reattach-checkouts]
```

- `--all` runs checks across every project under `projects/` and enables
  weave-wide orphan detection (repos on disk that belong to no project).
  Without `--all`, only the active project is checked and orphan detection
  is skipped (a repo absent from the active project may belong to another
  project — flagging it as orphaned would produce false positives).
- `--locked` exits zero iff every repo's tip matches its `rwv.lock`
  entry. Prints per-repo `ok` / `tip ≠ lock` lines to stdout. Useful
  as a scriptable precondition before `rwv sync` (note: `rwv sync`
  also runs its own lock-freshness check at op start; `--locked` is
  the explicit external gate). Mutually exclusive with `--fix` and
  `--json`.
- `--json` emits machine-readable output (see Output below). Mutually
  exclusive with `--locked` and `--fix`. Honors the same scoping as the
  default text output: project-scoped by default, weave-wide with `--all`.
- `--fix` repairs every finding marked **Auto-fixable** in
  `docs/reference/doctor-findings.md`. That page carries the mark on each
  finding it documents, alongside what the repair does and why the findings
  it does not mark are left to you. The set is not restated here: a second
  copy goes stale the first time an arm moves, and the one an operator reads
  is the last to be corrected. A finding marked **Report-only by default**
  is repaired only when you also pass the flag named in its entry —
  `--reattach-checkouts` or `--adopt-detached-checkouts`. Idempotent.
  Mutually exclusive with `--locked` and `--json`.
- `--reattach-checkouts` widens `--fix` by exactly one arm
  (`branch-model.md` §7.2): a canonical store whose HEAD is detached is
  reattached to its tracking declaration's local counterpart **when that
  branch exists and its tip equals HEAD**. Without the flag that finding is
  reported with the `git switch` that would repair it, and nothing moves.
  The condition is deliberately narrow — it is false for the ordinary
  post-fetch state (a stale counterpart with HEAD at the lock SHA), so this
  repairs the minority it can prove safe rather than reattaching a weave.
  Named for what it changes, per the house rule on override flags: moving
  which name your commits hang off is a different consequence from losing
  it (`--detach-checkouts`), so they are two flags.

**A branch that looks like rwv's is not rwv's.** Every deletion `--fix`
performs is gated on a persisted ownership receipt for that exact ref in
that exact store (`branch-model.md` R2), and on a warrant proving the loss
is safe (R3). A hand-made `<project>--<workweave>/<segment>` branch is
reported — so you can see it — and never removed.

Run `rwv --help doctor` for the full clap surface.

## Which weave `--fix` repairs

`--fix` arms fall into two classes, scoped differently on purpose. The
question the classes answer is whether the weave you invoked doctor from
determines what gets repaired.

**Weave-scoped arms — the invoking weave sets the scope.** Integration
content regeneration, surfacing symlinks, the `role: primary` manifest
rewrite, the `rwv.lock` replay-exclusion and its paired merge-driver config,
and the index / working-tree / state-hygiene drift arms all repair state that
exists once *per weave*. Run inside a workweave, they repair that workweave
and nothing else: primary's copy of the same file is left alone, including
primary's own drift, and the drift scan does not enumerate sibling weaves at
all. Repairing primary is a separate `rwv doctor --fix` run from primary.
Run at primary, these arms repair primary's own copy — except the drift and
state-hygiene arms, which additionally enumerate every workweave primary
parents, a scope only primary can name.

**Workspace-rooted arms — the invoking weave is ignored.** Dangling-receipt
retraction, the ref-ownership registry migration, the canonical-store
migration and reattach arms, safe-class stale-branch deletion, the
workweave-registry prune and adopt arms, the dangling-parent re-point, the
dangling-active-project clear, and the weave-root-identity-conflict clear all
act on state the workspace holds in exactly one place, so they take the
primary path unconditionally — from primary and from any workweave alike.
This is not an oversight in the weave-scoping above; there is nothing
weave-local for them to bind to:

- A workweave's `projects/<project>/` is a **linked worktree** of primary's
  clone, so `refs/heads/*` is one physical ref database shared by every
  weave. git keeps only `HEAD` and `refs/worktree/*` per worktree
  (`git rev-parse --git-path refs/heads` from inside a workweave resolves
  into primary's `.git`). A per-weave view of these refs would be a shadow of
  the shared store, not a separate store.
- The ownership receipts that describe those refs, and the registry that
  records which workweaves exist, live only in primary, at
  `projects/<project>/.rwv-workweave-index` — an untracked, primary-local
  file. A workweave has no copy to write instead.
- The active-project pointer is a primary-only selector, and the only place it
  exists. `.rwv-active` and `.rwv-workweave` name the same fact — which
  project a tree belongs to — and are **mutually exclusive**: a primary root
  carries the pointer, a workweave root carries the marker, never both. So a
  dangling selector has exactly one file to clear, and no per-weave copy to
  choose between.

### The exclusivity rule, and the arm that enforces it

Because the two files are mutually exclusive, they are one tier of the
project-resolution chain rather than two ranked ones:

```
--project > -w prefix > (.rwv-active | .rwv-workweave)
```

Nothing about a directory's *shape* distinguishes a primary root from a
workweave root — both hold `projects/` and registry directories — so the
identity file is the whole of the distinction, and a tree carrying both has
two answers with nothing keeping them in agreement. `rwv doctor` reports that
as `weave-root-identity-conflict`.

`--fix` repairs one of its three sub-kinds, and the split is not symmetric:

- **`registered-workweave`** — the marker names this workspace's primary, and
  that primary's `.rwv-workweave-index` records this exact directory as one of
  its workweaves. Evidence held *outside* the tree settles the identity, so
  the pointer is provably the redundant copy. `--fix` deletes `.rwv-active`
  and leaves the marker.
- **`marker-unverifiable`** — the marker itself cannot witness what it
  claims: unreadable, missing the required `parent:` field (a legacy marker),
  or naming a `primary:` that verifies as no workspace at all. Report-only —
  a marker that cannot prove its own claim cannot prove which file is the
  stray either. Never auto-fixed: repairing the marker (`rwv doctor --fix`
  migrates a legacy one; a dangling or unreadable one needs a hand edit) is a
  separate step from clearing the pointer.
- **`unwitnessed`** — the marker parses and verifies, but names a different
  primary, or names this primary with no registry entry pointing back at this
  directory (the usual cause: a workweave copied with `cp -r`, whose registry
  entry still names the original). Report-only. Deleting either file would be
  a guess, and the marker in particular carries `primary` and `parent` values
  that exist nowhere else.

The discriminator is deliberately the registry and not "does this tree contain
a `.rwv-workweave-index`". That looks like a primary-ness signature and is not
one: the index is untracked, so whether a workweave inherits a copy depends on
whether its `projects/<project>/` is a linked worktree or a plain directory
copy — a topology accident rather than a fact about identity.

Like the other arms in this class, the scan starts from primary
unconditionally: `--fix` run inside workweave A will clear a stray pointer in
sibling workweave B, because the registry that classifies both lives only at
primary.

**Acting on another weave's refs is a policy, not a consequence of the
above.** Sharing the ref database forces these arms to *see* every weave's
refs. It does not by itself force `--fix` run in weave A to *destroy* a ref
recorded for weave B. That it does so is a deliberate choice: the alternative
— acting only on refs the invoking weave minted — would leave the reclaimable
population permanently unreachable, because a stale ephemeral branch is by
definition one whose workweave is gone, so no weave could be the one
authorized to reclaim it.

What bounds the destroy is therefore not where you ran the command. It is the
receipt gate (`branch-model.md` R2 — a persisted receipt for that exact ref
in that exact store), the liveness test (a name some workweave still on disk
would mint is live-class and is never deleted), and the merged warrant (R3 —
the tip is an ancestor of the store's tip). The invoking weave contributes
nothing to that decision, which also means it backstops nothing: run `--fix`
from a throwaway weave and it will still act on registry entries and refs
belonging to weaves you are actively using — correctly when the
classification is right, with nothing about the invocation site to catch it
when it is not.

## Output

Default text output is one human-readable line per violation, grouped by
severity. Under `--json`, output is the envelope:

```
{
  "$schema": "<url>",
  "violations": [ { "kind": "...", ... }, ... ],
  "plugins": [ { "name": "...", "path": "...", "shadowed": false }, ... ]
}
```

The `$schema` URL points to the committed schema artifact. Variants are
discriminated by the `kind` tag — `branch-discipline`, `cargo-patch-shadowing`, `cargo-version-skew`, `clone-topology`, `dangling-active-project`, `dangling-ref-receipt`, `dangling-reference`, `dead-op-lease`, `head-unreadable`, `incomplete-lock`, `index-drift`, `legacy-manifest-format`, `legacy-workweave-index`, `legacy-workweave-marker`, `merge-driver-config-unreadable`, `missing-canonical-clone`, `missing-merge-driver-config`, `missing-replay-exclusion`, `missing-role`, `orphaned-clone`, `orphaned-savepoint`, `phantom-merge-driver`, `pre-flat-ref-receipt`, `projects-dir-unreadable`, `provenance`, `replay-exclusion-unreadable`, `stale-lock`, `stale-op-state`, `stale-worktree-registration`, `uninitialized-submodule`, `unparseable-project`, `unreadable-workweave-index`, `unresolvable-lock-entry`, `weave-root-identity-conflict`, `working-tree-drift`, `workweave-drift`, `workweave-tree-integrity`.
Every per-repo variant carries `path` (manifest-relative) and
`absolute_path` (fully resolved). Variants with subkinds
(`branch-discipline`, `clone-topology`, `dead-op-lease`, `index-drift`, `missing-replay-exclusion`, `orphaned-savepoint`, `provenance`, `weave-root-identity-conflict`, `working-tree-drift`, `workweave-drift`, `workweave-tree-integrity`) carry an additional `sub_kind` field.
`legacy-role-primary` carries `project` and
`manifest_path` so the caller can locate the file `--fix` will rewrite.
`workweave-tree-integrity` carries `workweave_dir` and a `sub_kind`
(`dangling-parent`, `foreign-primary`, `foreign-primary-other-workspace`, `misnamed-dir`, `parent-chain-anomaly`, `stale-registry-entry`, `tracked-index`, `unreadable-marker`, `unregistered-dir`, `unregistered-workweave`).

The `plugins` array is the PATH inventory of `rwv-*` executables found at
run time. Each record carries `name` (the `<verb>` in `rwv-<verb>`), `path`
(absolute), and `shadowed` (`true` when an earlier `PATH` entry shadows this
binary, with `shadowed_by` naming the winner). An empty array means no
`rwv-*` executables were found. Plugin presence is **never** a failed check —
the inventory is the audit surface for the PATH trust boundary, not a health
gate. The exit code is unaffected by this field.

Surfacing violations (missing or mis-resolved symlinks in the active
project's surfacing set) are reported as `core` integration warnings in
the text output; they do not have a dedicated `--json` kind because they
are emitted as `Issue` values through the same integration-issue channel
as per-integration check results.

`member-incompatibility` findings travel that same integration-issue channel
and so also have no dedicated `--json` kind; the kebab-case tag is the message
prefix, which is what a caller keys off. Doctor is the **standing observation
arm** for the category — `rwv update` reports the same finding at the moment it
creates one. Neither gates: nothing refuses on this category, and `--fix`
cannot repair it.

Schema:

```json
{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "title": "DoctorJsonOutput",
  "description": "Output envelope for `rwv doctor --json`. By default only the active project is checked and orphan detection is skipped; pass `--all` to scan every project and enable weave-wide orphan detection. Findings arrive on two disjoint arrays — `violations` for what rwv's own scans found, `issues` for what an integration reported — and both empty means the checked scope is clean. The `plugins` array is the PATH inventory of `rwv-*` executables (reporting only — plugin presence never fails the doctor check or affects the exit code).",
  "type": "object",
  "required": [
    "$schema",
    "advisories",
    "issues",
    "plugins",
    "violations"
  ],
  "properties": {
    "$schema": {
      "type": "string"
    },
    "advisories": {
      "description": "Standing advisories this checkout raises, in the vocabulary `rwv sync --json` already emits: a condition with a named remedy and the paths that raised it. Empty, not absent, so a consumer branches on length.",
      "type": "array",
      "items": {
        "$ref": "#/definitions/AdvisoryOutput"
      }
    },
    "issues": {
      "description": "Findings raised by an integration rather than by one of rwv's own scans: a missing ecosystem tool, drift or user-held content in a managed file, a surfacing symlink that does not resolve, a member incompatibility. Disjoint from `violations` — nothing on this array carries `kind: \"core-finding\"`.",
      "type": "array",
      "items": {
        "$ref": "#/definitions/IssueOutput"
      }
    },
    "plugins": {
      "description": "`rwv-*` executables discovered on `PATH`. Each record carries the verb name, absolute path, and a `shadowed` flag for duplicates: when the same name appears in multiple `PATH` directories, the first copy wins at exec time; later copies are marked `shadowed: true` with `shadowed_by` pointing at the winning binary. Records are sorted by `(name, path)` for deterministic output. An empty array means no `rwv-*` executables were found. Never a failed check — the inventory is the audit surface for the PATH trust boundary.",
      "type": "array",
      "items": {
        "$ref": "#/definitions/PluginRecord"
      }
    },
    "resolution": {
      "description": "Resolved workspace coordinates (workspace root, optional workweave identity, project). Absent when no project is resolved.",
      "anyOf": [
        {
          "$ref": "#/definitions/Resolution"
        },
        {
          "type": "null"
        }
      ]
    },
    "violations": {
      "type": "array",
      "items": {
        "$ref": "#/definitions/ViolationOutput"
      }
    }
  },
  "definitions": {
    "AdvisoryKindOutput": {
      "description": "Closed vocabulary for `AdvisoryOutput::kind`. Adding a member is additive — existing consumers keep matching the members they know.",
      "oneOf": [
        {
          "description": "Generated ecosystem state may no longer agree with the inputs it was derived from.",
          "type": "string",
          "enum": [
            "derived_state_stale"
          ]
        }
      ]
    },
    "AdvisoryOutput": {
      "description": "A condition worth an operator's attention that a verb's `--json` output reports alongside its result, without being a failure of the verb itself.\n\nEvery field is something a consumer branches on directly — `kind` a closed enum, `remedy` a verb string runnable in the checkout where the advisory appears, `inputs` the workspace-relative paths that raised it. None of the three is a sentence a consumer would have to parse to act on.\n\nShared across verbs so more than one `--json` surface can emit the same vocabulary: a sync-time note and a doctor-time standing finding both fit this shape without either owning it.",
      "type": "object",
      "required": [
        "inputs",
        "kind",
        "remedy"
      ],
      "properties": {
        "inputs": {
          "description": "Workspace-relative paths whose state raised this advisory.",
          "type": "array",
          "items": {
            "type": "string"
          }
        },
        "kind": {
          "$ref": "#/definitions/AdvisoryKindOutput"
        },
        "remedy": {
          "description": "The verb that resolves the advisory (e.g. `\"rwv materialize\"`).",
          "type": "string"
        }
      }
    },
    "BranchDisciplineKind": {
      "description": "Discriminator for `CheckViolation::BranchDiscipline` findings.\n\nThree groupings, mirroring the three checks in the spec:\n\n* (a) workweave-branch — a workweave checkout is on the wrong branch, or on a ref of its own namespace that predates the flat naming: `SharedBranch`, `ForeignEphemeral`, `Detached`, `UnmigratedEphemeralBranch`, `UnrecordedEphemeralBranch`, `UnbornCheckout`. * (b) canonical-store attachment — what the canonical store's HEAD is: `CanonicalHoldsLiveWorkweaveRef`, `CanonicalHoldsLeakedRef`, `CanonicalDetached`. * (c) stale-ephemeral-branches — a `<project>--<name>/...` branch exists in a canonical clone but workweave `<name>` no longer exists on disk: `StaleEphemeralBranchSafe`, `StaleEphemeralBranchLive`, or `StaleEphemeralBranchUnowned`. The safe/live split applies the doctrine in `docs/explanation/joints/shared-refs-drift.md` to refs: a tip that is an ancestor of the primary's tracking-branch tip carries no unique work and is safely removable; a tip with commits not reachable from the primary is live work and must be left alone.\n\n# Ownership is by record, never by name shape (R2)\n\nThe (b) grouping and the safe/live/unowned split in (c) both key on whether rwv holds a persisted ownership receipt (`crate::workweave_index::RefRegistry`) for the exact ref in the exact store. A branch that merely *looks* like one of rwv's — a hand-made `<a>--<b>/<c>` — is an operator branch: the canonical-store pass leaves it alone, and `--fix` never deletes it.",
      "oneOf": [
        {
          "description": "(a) The workweave checkout is on a non-ephemeral branch (e.g. `main`).\n\nCaused by `git switch main` inside a workweave or by a bare clone that was never moved to an ephemeral branch. The fixture for this sub-kind exercises the bare-main-in-workweave case from the spec's acceptance criteria: the violation must flag from creation, before any commit lands.\n\nReport-only, deliberately — not a missing arm. The state is operator-made (a `git switch` run by hand), unlike the fetch-written detachments whose repairs are native consented arms, and the remediation prints the exact registry-aware `git switch` (it names an existing recorded branch whenever a receipt exists), which is what keeps hand-running it safe. If measured recurrence reopens the question, the native form is a targeted repair naming one checkout, not a bulk consent flag.\n\nReference-alias carve-out: a symlinked `reference` checkout (a `CheckoutKind::ReferenceAlias`) legitimately shares the canonical store's non-ephemeral branch (e.g. `main`) — it has no per-workweave ephemeral branch by design, because it is the canonical store viewed through a symlink. The I3 branch-discipline scan skips such aliases, so they never fire this finding. A `reference` repo created with `--worktree-references` is a real worktree (`CheckoutKind::Worktree`) on its own ephemeral branch and is checked normally.",
          "type": "object",
          "required": [
            "shared-branch"
          ],
          "properties": {
            "shared-branch": {
              "type": "object",
              "required": [
                "actual_branch",
                "expected_ref"
              ],
              "properties": {
                "actual_branch": {
                  "description": "The branch currently checked out (e.g. `main`).",
                  "type": "string"
                },
                "expected_ref": {
                  "description": "The ephemeral ref this workweave mints (`<project>--<workweave>`).",
                  "type": "string"
                },
                "recorded_ref": {
                  "description": "The ephemeral ref rwv holds a receipt for in this repo's canonical store, when it holds one. Decides the remediation spelling: `git switch <name>` returns to an existing ref, `git switch -c` is only correct when there is none.",
                  "type": [
                    "string",
                    "null"
                  ]
                }
              }
            }
          },
          "additionalProperties": false
        },
        {
          "description": "(a) The workweave checkout is on an ephemeral ref rwv **recorded** for a *different* workweave. Report-only.\n\nKeyed on the receipt, not on the name (R2). Before the flat-name cutover this arm fired on any `<a>--<b>/<c>`-shaped name, which meant a hand-made branch was reported as \"another workweave's\" purely because of how it was spelled; now it fires only when some project's registry says the ref really is one rwv minted for another workweave. A look-alike lands in `SharedBranch` instead — both are report-only, so the distinction costs nothing but accuracy, and report-only is deliberate for the same reason as `SharedBranch`'s: the state is operator-made and the printed switch is exact.",
          "type": "object",
          "required": [
            "foreign-ephemeral"
          ],
          "properties": {
            "foreign-ephemeral": {
              "type": "object",
              "required": [
                "actual_branch",
                "expected_ref"
              ],
              "properties": {
                "actual_branch": {
                  "description": "The branch currently checked out.",
                  "type": "string"
                },
                "expected_ref": {
                  "description": "The ephemeral ref this workweave mints (`<project>--<workweave>`).",
                  "type": "string"
                },
                "recorded_ref": {
                  "description": "See `SharedBranch`'s field of the same name.",
                  "type": [
                    "string",
                    "null"
                  ]
                }
              }
            }
          },
          "additionalProperties": false
        },
        {
          "description": "(a) The workweave checkout is in detached-HEAD state — HEAD points directly at a commit instead of a named branch. With no branch name there is nothing for the merged-check to ask about and nothing for the workweave's ref namespace to be keyed by, so both invariants lapse for as long as the checkout stays detached.\n\n`--fix --adopt-detached-checkouts` mints the workweave's flat ref **at HEAD** — i.e. at the lock SHA — and, when `legacy_branch` is `Some`, gives that branch's name up to make room for it.",
          "type": "object",
          "required": [
            "detached"
          ],
          "properties": {
            "detached": {
              "type": "object",
              "required": [
                "at_sha",
                "expected_ref"
              ],
              "properties": {
                "at_sha": {
                  "description": "The commit HEAD names directly.",
                  "type": "string"
                },
                "expected_ref": {
                  "description": "The ephemeral ref this workweave mints (`<project>--<workweave>`).",
                  "type": "string"
                },
                "legacy_branch": {
                  "description": "A pre-flat branch of this workweave's own namespace, with its tip. **Both** tips are reported, because they are the two things the operator is choosing between.",
                  "anyOf": [
                    {
                      "$ref": "#/definitions/LegacyRefAtTip"
                    },
                    {
                      "type": "null"
                    }
                  ]
                },
                "recorded_ref": {
                  "description": "See `SharedBranch`'s field of the same name.",
                  "type": [
                    "string",
                    "null"
                  ]
                }
              }
            }
          },
          "additionalProperties": false
        },
        {
          "description": "(a) The workweave checkout is attached to a pre-flat `<project>--<workweave>/<segment>` ref of its **own** namespace.\n\nThe common migration case and the fully automatic one: `--fix` records a receipt at the ref's current tip and renames it to the flat name. Nothing is lost — a rename preserves the tip — and the namespace membership is decided against the name this workweave *mints*, never by taking the observed name apart (`LegacyEphemeralRefName`).",
          "type": "object",
          "required": [
            "unmigrated-ephemeral-branch"
          ],
          "properties": {
            "unmigrated-ephemeral-branch": {
              "type": "object",
              "required": [
                "actual_branch",
                "expected_ref"
              ],
              "properties": {
                "actual_branch": {
                  "description": "The pre-flat branch currently checked out.",
                  "type": "string"
                },
                "expected_ref": {
                  "description": "The flat ref it migrates to (`<project>--<workweave>`).",
                  "type": "string"
                }
              }
            }
          },
          "additionalProperties": false
        },
        {
          "description": "(a) Two or more refs share this workweave's namespace in one store, so the flat name cannot be created and no migration arm can run.\n\ngit holds `refs/heads/p--w` and `refs/heads/p--w/x` as a file and a directory of the same name, so the rename the migration would perform is refused whatever order the arms take. `fix_branch_model_migration` skips the pair before any arm, which is why this is reported in place of `UnmigratedEphemeralBranch` rather than beside it: that finding's message promises a rename this state cannot produce.\n\nReport-only, and the repair is an operator's judgement rather than a missing arm — which of the refs is this workweave's branch, and where the others belong, is not derivable from the refs themselves.",
          "type": "object",
          "required": [
            "blocked-ephemeral-namespace"
          ],
          "properties": {
            "blocked-ephemeral-namespace": {
              "type": "object",
              "required": [
                "blocking_refs",
                "expected_ref"
              ],
              "properties": {
                "blocking_refs": {
                  "description": "Every pre-flat ref found under that namespace, in listing order.",
                  "type": "array",
                  "items": {
                    "type": "string"
                  }
                },
                "expected_ref": {
                  "description": "The flat ref no arm can create while the namespace is shared (`<project>--<workweave>`).",
                  "type": "string"
                }
              }
            }
          },
          "additionalProperties": false
        },
        {
          "description": "(a) The workweave checkout is in detached-HEAD state AND two or more refs share this workweave's namespace, so `--adopt-detached-checkouts` cannot run.\n\n`fix_branch_model_migration` skips the whole repo before any arm when `legacy_refs.len() > 1` — including the consented detached arm — which is why this is reported in place of `Detached` rather than beside it: that finding's message promises `--adopt-detached-checkouts`, a flag whose arm the guard prevents from running. The principle is consent-tier-independent: a consented remedy that cannot run misleads the operator exactly as an auto remedy does — consent changes who acts, not whether the named action works.\n\nReport-only. The operator must reduce the namespace to at most one ref, then re-run to get the ordinary `Detached` finding with a remedy that will actually run.",
          "type": "object",
          "required": [
            "blocked-detached-namespace"
          ],
          "properties": {
            "blocked-detached-namespace": {
              "type": "object",
              "required": [
                "at_sha",
                "blocking_refs",
                "expected_ref"
              ],
              "properties": {
                "at_sha": {
                  "description": "The commit HEAD names directly.",
                  "type": "string"
                },
                "blocking_refs": {
                  "description": "Every pre-flat ref found under that namespace, in listing order.",
                  "type": "array",
                  "items": {
                    "type": "string"
                  }
                },
                "expected_ref": {
                  "description": "The flat ref that cannot be created while the namespace is shared (`<project>--<workweave>`).",
                  "type": "string"
                }
              }
            }
          },
          "additionalProperties": false
        },
        {
          "description": "(a) The workweave's flat ref exists in the canonical store, but rwv holds no receipt for it.\n\nThe state a build that minted flat names before receipts existed leaves behind, and the state a migration crash between the receipt and the rename would leave if the receipt had not been written first. Under R2 the ref is nobody's until adopted, so `workweave delete` cannot clean it up; `--fix` adopts it at its observed tip.",
          "type": "object",
          "required": [
            "unrecorded-ephemeral-branch"
          ],
          "properties": {
            "unrecorded-ephemeral-branch": {
              "type": "object",
              "required": [
                "branch"
              ],
              "properties": {
                "branch": {
                  "description": "The flat ref (`<project>--<workweave>`).",
                  "type": "string"
                }
              }
            }
          },
          "additionalProperties": false
        },
        {
          "description": "(a) The workweave checkout is on a branch with no commits.\n\nReport-only, and not because a fix is missing: there is no revision to record a receipt against, so there is nothing the migration could own. `rwv lock` is where an unborn HEAD is actionable.",
          "type": "object",
          "required": [
            "unborn-checkout"
          ],
          "properties": {
            "unborn-checkout": {
              "type": "object",
              "required": [
                "branch"
              ],
              "properties": {
                "branch": {
                  "description": "The branch HEAD points at, which has no commits yet.",
                  "type": "string"
                }
              }
            }
          },
          "additionalProperties": false
        },
        {
          "description": "(b) The canonical store is attached to a ref rwv recorded as belonging to a workweave that is **still on disk**.\n\nAn I3 disjointness violation. git forbids one branch being checked out in two worktrees of the same store, so reaching this state means a directory was moved or copied. Report-only — there is no fix that does not guess which of the two checkouts is the real one.",
          "type": "object",
          "required": [
            "canonical-holds-live-workweave-ref"
          ],
          "properties": {
            "canonical-holds-live-workweave-ref": {
              "type": "object",
              "required": [
                "actual_branch",
                "workweave_name"
              ],
              "properties": {
                "actual_branch": {
                  "description": "The branch the canonical store is attached to.",
                  "type": "string"
                },
                "workweave_name": {
                  "description": "The live workweave the receipt says that ref belongs to.",
                  "type": "string"
                }
              }
            }
          },
          "additionalProperties": false
        },
        {
          "description": "(b) The canonical store is attached to a ref rwv recorded as belonging to a workweave that is **gone** — a leak.\n\nReport-only in practice: the DESTROY that would reclaim the ref cannot run while this store's own HEAD is on it (git refuses to delete a branch a worktree uses), so `--fix` names the ref and the `git switch` that frees it rather than attempting a delete that cannot succeed. Once the store is off the ref it is an ordinary (c) finding and `--fix` reclaims it under a warrant.",
          "type": "object",
          "required": [
            "canonical-holds-leaked-ref"
          ],
          "properties": {
            "canonical-holds-leaked-ref": {
              "type": "object",
              "required": [
                "actual_branch",
                "project"
              ],
              "properties": {
                "actual_branch": {
                  "description": "The branch the canonical store is attached to.",
                  "type": "string"
                },
                "project": {
                  "description": "The project whose registry holds the receipt.\n\nNot the workweave: rwv does not try to reconstruct which workweave a stray ref belonged to. The receipt records `(store, name, created_at)`, and the workweave is recoverable only while one on disk would mint that name — which is exactly the case this variant is *not*.",
                  "type": "string"
                }
              }
            }
          },
          "additionalProperties": false
        },
        {
          "description": "(b) The canonical store — or the project repo — is in detached-HEAD state.\n\nThe project repo is an instance of the branch model, so it is checked here rather than exempted.\n\n`--fix --reattach-checkouts` reattaches when `reattachable` — the tracking declaration's local counterpart exists and its tip equals HEAD. That condition is false for the ordinary post-fetch state (stale counterpart, HEAD at the lock SHA), so the fix repairs the minority; it is not weave-wide reattachment.",
          "type": "object",
          "required": [
            "canonical-detached"
          ],
          "properties": {
            "canonical-detached": {
              "type": "object",
              "required": [
                "at_sha",
                "reattachable"
              ],
              "properties": {
                "at_sha": {
                  "description": "The commit HEAD names directly.",
                  "type": "string"
                },
                "counterpart": {
                  "description": "The local counterpart of the ref this repo tracks — the manifest's `version:` for a member, the remote's declared default branch for the project repo. `None` when no tracking declaration resolves, in which case there is nothing to name as a reattach target.",
                  "type": [
                    "string",
                    "null"
                  ]
                },
                "reattachable": {
                  "description": "Whether the reattach condition holds: `counterpart` exists as a local branch **and** its tip equals HEAD.",
                  "type": "boolean"
                }
              }
            }
          },
          "additionalProperties": false
        },
        {
          "description": "(c) A `<project>--<name>/...` branch in the canonical clone whose workweave `<name>` no longer exists on disk, **which rwv holds an ownership receipt for**, and whose tip is an ancestor of the primary tracking branch's tip (no unique commits). Safe-class per the shared-refs-drift doctrine — `--fix` deletes it under a `Merged` warrant, with no information loss.",
          "type": "object",
          "required": [
            "stale-ephemeral-branch-safe"
          ],
          "properties": {
            "stale-ephemeral-branch-safe": {
              "type": "object",
              "required": [
                "branch",
                "project"
              ],
              "properties": {
                "branch": {
                  "description": "The full branch name (e.g. `foundations--feat-a`).",
                  "type": "string"
                },
                "project": {
                  "description": "The project whose registry holds the receipt.\n\nNot the workweave, for the reason `CanonicalHoldsLeakedRef` gives: rwv does not reconstruct which workweave a ref belonged to, and for this class no workweave on disk would mint the name — that is what makes it stale.",
                  "type": "string"
                }
              }
            }
          },
          "additionalProperties": false
        },
        {
          "description": "(c) A `<project>--<name>/...` branch in the canonical clone whose workweave `<name>` no longer exists on disk, which rwv holds a receipt for, but whose tip carries commits not reachable from the primary tracking branch's tip (unique work). Live-class per the shared-refs-drift doctrine — report-only; `--fix` never touches this, because no `Merged` warrant can be established for it. The operator decides whether to land the commits, archive the branch, or delete it.",
          "type": "object",
          "required": [
            "stale-ephemeral-branch-live"
          ],
          "properties": {
            "stale-ephemeral-branch-live": {
              "type": "object",
              "required": [
                "branch",
                "project",
                "tip_sha"
              ],
              "properties": {
                "branch": {
                  "description": "The full branch name.",
                  "type": "string"
                },
                "project": {
                  "description": "The project whose registry holds the receipt. See `StaleEphemeralBranchSafe`.",
                  "type": "string"
                },
                "tip_sha": {
                  "description": "The branch tip SHA, surfaced so the operator can recover the commits before deleting (e.g. `git log <tip_sha>`).",
                  "type": "string"
                }
              }
            }
          },
          "additionalProperties": false
        },
        {
          "description": "(c) A branch shaped like one rwv minted before the naming scheme was flattened, sitting in a canonical store, which **rwv holds no ownership receipt for** and which no workweave on disk claims.\n\nUnder R2 this ref is not rwv's: name shape is not ownership. It is reported so the operator can see it, and it is never deleted — deleting this class is how a hand-made `<a>--<b>/<c>` branch can disappear under `--fix`.\n\n# Why this one is discovered by shape and nothing else is\n\nEvery other arm asks the registry or asks a live workweave's **minted** name. This arm has neither to ask: there is no receipt, and reconstructing which workweave the ref belonged to is forbidden — so the alternative to a shape heuristic is not a better signal, it is silence, and the refs the operator most needs to see (the pre-receipt population the migration cannot reach) would simply stop being reported.\n\nWhat keeps that sound is that the heuristic yields a `bool` and nothing else — see `looks_like_a_pre_flat_ref`. No name is taken apart, no workweave is named, and the only route to a DESTROY runs through an `OwnedRef`, which only a persisted receipt produces. A false positive costs one line of output and can cost nothing more.",
          "type": "object",
          "required": [
            "stale-ephemeral-branch-unowned"
          ],
          "properties": {
            "stale-ephemeral-branch-unowned": {
              "type": "object",
              "required": [
                "branch"
              ],
              "properties": {
                "branch": {
                  "description": "The full branch name.",
                  "type": "string"
                }
              }
            }
          },
          "additionalProperties": false
        }
      ]
    },
    "CargoSkewOccurrenceOutput": {
      "description": "Wire representation of `crate::integrations::cargo_workspace::CargoSkewOccurrence`.\n\nKept separate so the internal type stays free of serde/schemars deps and the wire shape is a single-source-of-truth definition here.",
      "type": "object",
      "required": [
        "member",
        "requirement"
      ],
      "properties": {
        "member": {
          "description": "Weave-relative member path.",
          "type": "string"
        },
        "requirement": {
          "description": "Requirement string (post `workspace = true` indirection).",
          "type": "string"
        }
      }
    },
    "CloneTopologyKind": {
      "description": "Discriminator for `CheckViolation::CloneTopology` findings.\n\nThe four sub-kinds enumerate the ways the bottom tier of the stability stack (clone-topology.md) can break: a manifest repo's slot at `<weave>/<repo_path>` must be a \"canonical store\" (a full clone), and every workweave checkout `<workweave>/<repo_path>` must be a linked workspace whose VCS common store resolves to that canonical store. Each variant names a distinct way the on-disk shape diverges from that spec.",
      "oneOf": [
        {
          "description": "A full clone (its own canonical store) is hosted under `.workweaves/` instead of at the manifest's canonical slot. The inverted-primary case: the canonical store has migrated into one workweave and other workweaves' checkouts link into *it*, not into `<weave>/<repo_path>`.\n\nReference-alias carve-out: a symlinked `reference` checkout (a `CheckoutKind::ReferenceAlias`, i.e. the workweave path is itself a symlink to the canonical store) is *not* a standalone store — it is the single canonical store viewed through a symlink, which upholds the single-canonical-store invariant by identity. The scan excludes it before this check. A *real* standalone store inside a workweave is a real directory (not a symlink) and still fires this finding.",
          "type": "object",
          "required": [
            "standalone-in-workweave"
          ],
          "properties": {
            "standalone-in-workweave": {
              "type": "object",
              "required": [
                "store_path"
              ],
              "properties": {
                "store_path": {
                  "description": "Absolute path of the standalone canonical store under `.workweaves/`.",
                  "type": "string"
                }
              }
            }
          },
          "additionalProperties": false
        },
        {
          "description": "The workspace at `<weave>/<repo_path>` is a full clone (its canonical store sits under itself), but one or more of this weave's workweave checkouts of the same repo resolve to a *different* canonical store. The weave-path clone publishes a separate object DAG nobody syncs to; push/pull becomes asymmetric and silent.",
          "type": "object",
          "required": [
            "disconnected-weave-clone"
          ],
          "properties": {
            "disconnected-weave-clone": {
              "type": "object",
              "required": [
                "other_store_path",
                "weave_store_path"
              ],
              "properties": {
                "other_store_path": {
                  "description": "Absolute path of a representative store one of the workweave checkouts actually uses (the one this weave clone is disconnected from).",
                  "type": "string"
                },
                "weave_store_path": {
                  "description": "Absolute path of the canonical store at the weave slot (the \"disconnected\" one).",
                  "type": "string"
                }
              }
            }
          },
          "additionalProperties": false
        },
        {
          "description": "A linked worktree under `.workweaves/<workweave>/<repo_path>` whose canonical store is not the weave canonical at `<weave>/<repo_path>`. The shared-DAG invariant between the canonical and the workweave is broken: commits made here land in a different object store than the canonical, and merged-checks across the two answer \"no\" silently.",
          "type": "object",
          "required": [
            "wrong-parent-worktree"
          ],
          "properties": {
            "wrong-parent-worktree": {
              "type": "object",
              "required": [
                "actual_store_path",
                "expected_store_path"
              ],
              "properties": {
                "actual_store_path": {
                  "description": "Absolute path of the canonical store this workweave checkout is actually linked into.",
                  "type": "string"
                },
                "expected_store_path": {
                  "description": "Absolute path of the canonical store this workweave checkout should be linked into (`<weave>/<repo_path>/.git`).",
                  "type": "string"
                }
              }
            }
          },
          "additionalProperties": false
        },
        {
          "description": "The weave path `<weave>/<repo_path>` itself is a linked worktree of some other clone — full inversion: there is no canonical store at the manifest slot, and the workspace there shares its DAG with whichever clone hosts the actual store.",
          "type": "object",
          "required": [
            "weave-clone-is-worktree"
          ],
          "properties": {
            "weave-clone-is-worktree": {
              "type": "object",
              "required": [
                "actual_store_path"
              ],
              "properties": {
                "actual_store_path": {
                  "description": "Absolute path of the canonical store this slot is linked into.",
                  "type": "string"
                }
              }
            }
          },
          "additionalProperties": false
        }
      ]
    },
    "DeadOpLeaseKind": {
      "description": "Discriminator for `CheckViolation::DeadOpLease` findings. Both shapes share the same `--fix` disposition (safe to remove the lease file) but name distinct root causes so the human-facing message can be specific.",
      "oneOf": [
        {
          "description": "The recorded owner workspace has no `.rwv-op` file at all — either the owner workspace was deleted, or the owner record was hand-removed while the lease survived. The classical crash-between-acquire-and-mark shape.",
          "type": "string",
          "enum": [
            "owner-record-absent"
          ]
        },
        {
          "description": "The recorded owner workspace has an `.rwv-op` file, but with a *different* op id than the lease references. The owner cleared and a new op started while this stale lease survived — the lease points at a completed op, not an in-flight one.",
          "type": "object",
          "required": [
            "owner-op-id-mismatch"
          ],
          "properties": {
            "owner-op-id-mismatch": {
              "type": "object",
              "required": [
                "owner_op_id"
              ],
              "properties": {
                "owner_op_id": {
                  "description": "Op id of the record currently living at the owner workspace.",
                  "type": "string"
                }
              }
            }
          },
          "additionalProperties": false
        }
      ]
    },
    "DriftKind": {
      "oneOf": [
        {
          "description": "Manifest lists it, but no worktree exists.",
          "type": "string",
          "enum": [
            "missing"
          ]
        },
        {
          "description": "Worktree exists, but manifest doesn't list it.",
          "type": "string",
          "enum": [
            "extra"
          ]
        }
      ]
    },
    "IndexDriftKind": {
      "description": "How a stale index should be treated.",
      "oneOf": [
        {
          "description": "Index tree matches the tree of some recent ancestor commit. Safe to auto-fix with `git reset` — the displaced tree is permanently in the DAG.",
          "type": "string",
          "enum": [
            "safe-to-fix"
          ]
        },
        {
          "description": "Index tree is not found in recent ancestor trees. The user has live staged content; `--fix` must not touch this.",
          "type": "string",
          "enum": [
            "live-staged"
          ]
        }
      ]
    },
    "IssueKindOutput": {
      "description": "`IssueKind` on the wire.\n\nExternally tagged, which is the shape the findings page already documents for `sub_kind`: a kind with no fields of its own is a plain string, and one that carries fields is a single-key object whose key is the tag. The tags are `IssueKind::tag`'s, and a divergence between the two is what `IssueKindOutput::from_kind` is exhaustive to prevent.",
      "oneOf": [
        {
          "type": "string",
          "enum": [
            "tool-missing",
            "managed-file-missing",
            "managed-file-drift",
            "managed-file-user-held",
            "surfacing",
            "config-rejected",
            "derived-state-stale",
            "disabled-integration-artifact",
            "integration-failed",
            "core-finding"
          ]
        },
        {
          "type": "object",
          "required": [
            "member-incompatibility"
          ],
          "properties": {
            "member-incompatibility": {
              "$ref": "#/definitions/MemberIncompatibilityOutput"
            }
          },
          "additionalProperties": false
        }
      ]
    },
    "IssueOutput": {
      "description": "One integration-reported finding as it appears in `rwv doctor --json`.",
      "type": "object",
      "required": [
        "integration",
        "kind",
        "message",
        "safe_to_fix",
        "severity"
      ],
      "properties": {
        "integration": {
          "description": "The integration that raised it, or `core` for a finding raised by `rwv doctor` itself while driving the integrations.",
          "type": "string"
        },
        "kind": {
          "$ref": "#/definitions/IssueKindOutput"
        },
        "message": {
          "description": "Operator-facing prose. Everything a consumer routes on is a field — matching on this string is what `kind` exists to replace.",
          "type": "string"
        },
        "safe_to_fix": {
          "description": "Whether `rwv doctor --fix` is permitted to auto-repair this finding. `false` marks a user-held file region auto-repair would destroy.",
          "type": "boolean"
        },
        "severity": {
          "$ref": "#/definitions/SeverityOutput"
        }
      }
    },
    "LegacyRefAtTip": {
      "description": "A pre-flat branch and the commit it reaches. **Both** tips are reported, side by side, because the operator is choosing between them.",
      "type": "object",
      "required": [
        "branch",
        "strands_commits",
        "tip_sha"
      ],
      "properties": {
        "branch": {
          "description": "The pre-flat branch name (`<project>--<workweave>/<segment>`).",
          "type": "string"
        },
        "strands_commits": {
          "description": "Whether that tip carries commits the detached HEAD does not — i.e. whether adopting the checkout would **strand** work. Arm 3 makes the warning mandatory in exactly this case.",
          "type": "boolean"
        },
        "tip_sha": {
          "description": "Its tip.",
          "type": "string"
        }
      }
    },
    "MarkerDefect": {
      "description": "Why a `.rwv-workweave` file cannot witness the identity it claims.\n\n`Legacy` covers every YAML marker `WorkweaveMarker::migrate_legacy` can repair: `primary:` present, with or without the `parent:` field that became required before the format changed (it backfills from `primary`). A YAML marker with no `primary:` of its own has nothing to backfill from, so it is `Unreadable` instead — `migrate_legacy` requires the field unconditionally, and a defect naming a repair nothing performs is worse than one that names none.\n\n`Serialize`/`JsonSchema` so `check::WeaveRootIdentityConflictKind` can carry a defect straight into a doctor finding's wire shape — the same value `require_exclusive` refuses on, not a re-description of it.",
      "oneOf": [
        {
          "type": "string",
          "enum": [
            "legacy"
          ]
        },
        {
          "type": "object",
          "required": [
            "dangling-primary"
          ],
          "properties": {
            "dangling-primary": {
              "type": "object",
              "required": [
                "primary"
              ],
              "properties": {
                "primary": {
                  "type": "string"
                }
              }
            }
          },
          "additionalProperties": false
        },
        {
          "type": "object",
          "required": [
            "unreadable"
          ],
          "properties": {
            "unreadable": {
              "type": "object",
              "required": [
                "detail"
              ],
              "properties": {
                "detail": {
                  "type": "string"
                }
              }
            }
          },
          "additionalProperties": false
        }
      ]
    },
    "MemberIncompatibilityOutput": {
      "description": "The four facts a `member-incompatibility` predicate established, as fields rather than as the sentence they are also rendered into.",
      "type": "object",
      "required": [
        "key",
        "on_disk",
        "path",
        "required",
        "required_by"
      ],
      "properties": {
        "key": {
          "description": "Display form of the `DefaultOnly` key.",
          "type": "string"
        },
        "on_disk": {
          "description": "The value currently on disk.",
          "type": "string"
        },
        "path": {
          "description": "The managed file holding the incompatible value.",
          "type": "string"
        },
        "required": {
          "description": "The strongest value the members require.",
          "type": "string"
        },
        "required_by": {
          "description": "The member file carrying that requirement.",
          "type": "string"
        }
      }
    },
    "OpVerb": {
      "description": "Which top-level verb started this op.",
      "oneOf": [
        {
          "description": "Single-step sync (existing `rwv sync`).",
          "type": "string",
          "enum": [
            "sync"
          ]
        },
        {
          "description": "Two-step sync-to.",
          "type": "string",
          "enum": [
            "sync-to"
          ]
        }
      ]
    },
    "OrphanedSavepointKind": {
      "description": "Classification of an orphaned savepoint, controlling `--fix` policy.",
      "oneOf": [
        {
          "description": "The savepoint tip is reachable from the current branch tip, so the ref is redundant — the underlying commits are still anchored by the live branch and dropping the savepoint loses no objects. `--fix` may drop redundant savepoints.",
          "type": "string",
          "enum": [
            "redundant"
          ]
        },
        {
          "description": "The savepoint tip is **not** reachable from the current branch tip. The ref is the last pointer to commits that would otherwise become unreachable. `--fix` must not drop these — the reflog is on the FORBIDDEN tripwire list, same rationale: don't cut the last recovery path.",
          "type": "string",
          "enum": [
            "live"
          ]
        }
      ]
    },
    "PluginRecord": {
      "description": "A discovered external command (`rwv-<verb>`) on `PATH`.\n\nRecords are sorted by `(name, path)` for deterministic output. When the same name appears in more than one `PATH` directory, the first occurrence wins at exec time; later occurrences are marked `shadowed = true` and carry `shadowed_by` pointing at the winning binary.",
      "type": "object",
      "required": [
        "name",
        "path",
        "shadowed"
      ],
      "properties": {
        "name": {
          "description": "Short verb name — the `<verb>` in `rwv-<verb>` and `rwv <verb>`.",
          "type": "string"
        },
        "path": {
          "description": "Absolute path of this binary on disk.",
          "type": "string"
        },
        "shadowed": {
          "description": "`true` when another binary with the same name appears earlier in `PATH` and will be executed instead. This binary is unreachable via `rwv <name>` until the shadowing copy is removed.",
          "type": "boolean"
        },
        "shadowed_by": {
          "description": "Absolute path of the binary that shadows this one. Present iff `shadowed` is `true`.",
          "type": [
            "string",
            "null"
          ]
        }
      }
    },
    "ProvenanceKind": {
      "description": "Discriminator for `CheckViolation::Provenance` findings.",
      "oneOf": [
        {
          "description": "The clone's `origin` remote URL differs from the URL recorded in the manifest. Until reconciled, pushes may publish to the wrong remote. Warning severity; report-only.\n\nNote: reference-role repos may intentionally point at a different remote (e.g. a local mirror). `is_reference_role` is `true` when the manifest records `role: reference` so the human-facing message can call out this nuance.",
          "type": "object",
          "required": [
            "origin-url-mismatch"
          ],
          "properties": {
            "origin-url-mismatch": {
              "type": "object",
              "required": [
                "actual_url",
                "is_reference_role",
                "manifest_url"
              ],
              "properties": {
                "actual_url": {
                  "description": "The actual fetch URL of the `origin` remote on disk.",
                  "type": "string"
                },
                "is_reference_role": {
                  "description": "`true` when the manifest entry carries `role: reference`. Reference-role repos may intentionally use a different remote (e.g. a local mirror), so the violation message notes this to help the operator decide whether to act.",
                  "type": "boolean"
                },
                "manifest_url": {
                  "description": "The URL recorded in the manifest (`rwv.toml`).",
                  "type": "string"
                }
              }
            }
          },
          "additionalProperties": false
        },
        {
          "description": "The SHA pinned in `rwv.lock` is absent from the clone's object store. The canonical store is missing the pinned revision; refresh it from its remote (run a fetch — not a sync — to recover). Error severity; report-only.",
          "type": "object",
          "required": [
            "lock-sha-unreachable"
          ],
          "properties": {
            "lock-sha-unreachable": {
              "type": "object",
              "required": [
                "sha"
              ],
              "properties": {
                "sha": {
                  "description": "The SHA pinned in `rwv.lock` that cannot be found locally.",
                  "type": "string"
                }
              }
            }
          },
          "additionalProperties": false
        }
      ]
    },
    "ReplayExclusionKind": {
      "description": "Which spelling of the replay exclusion the project repo carries, which decides whether `--fix` writes the entry fresh or migrates one in place.",
      "oneOf": [
        {
          "description": "`.gitattributes` carries no entry for `rwv.lock` at all.",
          "type": "string",
          "enum": [
            "absent"
          ]
        },
        {
          "description": "`.gitattributes` carries the legacy `merge=ours` spelling. The driver was renamed to close a collision with a global-config `ours` driver; the old name reads as the invariant being met while sync's check — which matches the current name — sees nothing.",
          "type": "string",
          "enum": [
            "legacy-spelling"
          ]
        },
        {
          "description": "`.gitattributes` carries both spellings for `rwv.lock`. Which one git applies is decided by reading order, and the legacy name is live either way: a global `merge.ours.driver` binds to it during a bare `git rebase --continue`.",
          "type": "string",
          "enum": [
            "legacy-alongside-current"
          ]
        }
      ]
    },
    "Resolution": {
      "description": "Resolved workspace coordinates for `--json` output and (future) plugin env-var envelope.\n\nCarries exactly the three result fields — `workspace` (primary root abs path), `workweave` (`<project>--<name>` identity when in a workweave, absent at primary), and `project` (resolved project name). Presence of `workweave` encodes the checkout kind; no separate `kind` or `location` field is needed.\n\nResults only — provenance (which chain step resolved the project, which flag addressed the workspace) is deliberately excluded: anything in default `--json` output becomes depended on, and the assertion use case needs the result, not the mechanism. Provenance appears only in the human-facing \"target:\" line printed to stderr.\n\nIsomorphic to the plugin env-var envelope (`RWV_WORKSPACE`/`RWV_WORKWEAVE`/`RWV_PROJECT`): both surfaces are pure projections of `WorkspaceContext::resolution`, never independently computed.",
      "type": "object",
      "required": [
        "project",
        "workspace"
      ],
      "properties": {
        "project": {
          "description": "Resolved project name.",
          "type": "string"
        },
        "workspace": {
          "description": "Primary workspace root (absolute path).",
          "type": "string"
        },
        "workweave": {
          "description": "Workweave identity (`<project>--<name>`).\n\nPresent when the invocation resolved into a workweave; absent at the primary. Presence encodes the checkout kind — no separate `kind` field.",
          "type": [
            "string",
            "null"
          ]
        }
      }
    },
    "SeverityOutput": {
      "description": "`crate::integration::Severity` on the wire.",
      "type": "string",
      "enum": [
        "warning",
        "error"
      ]
    },
    "ViolationOutput": {
      "description": "One violation as it appears in `rwv doctor --json` output.",
      "oneOf": [
        {
          "type": "object",
          "required": [
            "absolute_path",
            "kind",
            "path"
          ],
          "properties": {
            "absolute_path": {
              "type": "string"
            },
            "kind": {
              "type": "string",
              "enum": [
                "orphaned-clone"
              ]
            },
            "path": {
              "type": "string"
            }
          }
        },
        {
          "type": "object",
          "required": [
            "absolute_path",
            "kind",
            "path",
            "project"
          ],
          "properties": {
            "absolute_path": {
              "type": "string"
            },
            "kind": {
              "type": "string",
              "enum": [
                "dangling-reference"
              ]
            },
            "path": {
              "type": "string"
            },
            "project": {
              "type": "string"
            }
          }
        },
        {
          "type": "object",
          "required": [
            "absolute_path",
            "kind",
            "path",
            "project"
          ],
          "properties": {
            "absolute_path": {
              "type": "string"
            },
            "kind": {
              "type": "string",
              "enum": [
                "missing-role"
              ]
            },
            "path": {
              "type": "string"
            },
            "project": {
              "type": "string"
            }
          }
        },
        {
          "type": "object",
          "required": [
            "absolute_path",
            "actual",
            "kind",
            "locked",
            "path",
            "project"
          ],
          "properties": {
            "absolute_path": {
              "type": "string"
            },
            "actual": {
              "type": "string"
            },
            "kind": {
              "type": "string",
              "enum": [
                "stale-lock"
              ]
            },
            "locked": {
              "type": "string"
            },
            "path": {
              "type": "string"
            },
            "project": {
              "type": "string"
            }
          }
        },
        {
          "type": "object",
          "required": [
            "absolute_path",
            "kind",
            "path",
            "project"
          ],
          "properties": {
            "absolute_path": {
              "type": "string"
            },
            "kind": {
              "type": "string",
              "enum": [
                "incomplete-lock"
              ]
            },
            "path": {
              "type": "string"
            },
            "project": {
              "type": "string"
            }
          }
        },
        {
          "type": "object",
          "required": [
            "absolute_path",
            "kind",
            "path",
            "sub_kind",
            "workweave"
          ],
          "properties": {
            "absolute_path": {
              "type": "string"
            },
            "kind": {
              "type": "string",
              "enum": [
                "workweave-drift"
              ]
            },
            "path": {
              "type": "string"
            },
            "sub_kind": {
              "$ref": "#/definitions/DriftKind"
            },
            "workweave": {
              "type": "string"
            }
          }
        },
        {
          "type": "object",
          "required": [
            "absolute_path",
            "kind",
            "path",
            "sub_kind"
          ],
          "properties": {
            "absolute_path": {
              "type": "string"
            },
            "kind": {
              "type": "string",
              "enum": [
                "index-drift"
              ]
            },
            "path": {
              "type": "string"
            },
            "sub_kind": {
              "$ref": "#/definitions/IndexDriftKind"
            },
            "workweave": {
              "description": "`None` for the primary weave.",
              "type": [
                "string",
                "null"
              ]
            }
          }
        },
        {
          "type": "object",
          "required": [
            "absolute_path",
            "kind",
            "path",
            "sub_kind"
          ],
          "properties": {
            "absolute_path": {
              "type": "string"
            },
            "kind": {
              "type": "string",
              "enum": [
                "working-tree-drift"
              ]
            },
            "path": {
              "type": "string"
            },
            "sub_kind": {
              "$ref": "#/definitions/WorkingTreeDriftKind"
            },
            "workweave": {
              "description": "`None` for the primary weave.",
              "type": [
                "string",
                "null"
              ]
            }
          }
        },
        {
          "type": "object",
          "required": [
            "kind",
            "project",
            "sub_kind"
          ],
          "properties": {
            "kind": {
              "type": "string",
              "enum": [
                "missing-replay-exclusion"
              ]
            },
            "project": {
              "type": "string"
            },
            "sub_kind": {
              "$ref": "#/definitions/ReplayExclusionKind"
            }
          }
        },
        {
          "type": "object",
          "required": [
            "error",
            "kind",
            "project"
          ],
          "properties": {
            "error": {
              "type": "string"
            },
            "kind": {
              "type": "string",
              "enum": [
                "replay-exclusion-unreadable"
              ]
            },
            "project": {
              "type": "string"
            }
          }
        },
        {
          "type": "object",
          "required": [
            "config_key",
            "kind",
            "project"
          ],
          "properties": {
            "config_key": {
              "type": "string"
            },
            "kind": {
              "type": "string",
              "enum": [
                "missing-merge-driver-config"
              ]
            },
            "project": {
              "type": "string"
            }
          }
        },
        {
          "type": "object",
          "required": [
            "config_key",
            "error",
            "kind",
            "project"
          ],
          "properties": {
            "config_key": {
              "type": "string"
            },
            "error": {
              "type": "string"
            },
            "kind": {
              "type": "string",
              "enum": [
                "merge-driver-config-unreadable"
              ]
            },
            "project": {
              "type": "string"
            }
          }
        },
        {
          "type": "object",
          "required": [
            "absolute_path",
            "error",
            "kind",
            "path"
          ],
          "properties": {
            "absolute_path": {
              "type": "string"
            },
            "error": {
              "type": "string"
            },
            "kind": {
              "type": "string",
              "enum": [
                "head-unreadable"
              ]
            },
            "path": {
              "type": "string"
            }
          }
        },
        {
          "type": "object",
          "required": [
            "error",
            "kind",
            "path"
          ],
          "properties": {
            "error": {
              "type": "string"
            },
            "kind": {
              "type": "string",
              "enum": [
                "projects-dir-unreadable"
              ]
            },
            "path": {
              "type": "string"
            }
          }
        },
        {
          "type": "object",
          "required": [
            "absolute_path",
            "kind",
            "path",
            "project"
          ],
          "properties": {
            "absolute_path": {
              "type": "string"
            },
            "kind": {
              "type": "string",
              "enum": [
                "unresolvable-lock-entry"
              ]
            },
            "path": {
              "type": "string"
            },
            "project": {
              "type": "string"
            }
          }
        },
        {
          "type": "object",
          "required": [
            "kind",
            "legacy_path",
            "project"
          ],
          "properties": {
            "kind": {
              "type": "string",
              "enum": [
                "legacy-manifest-format"
              ]
            },
            "legacy_path": {
              "type": "string"
            },
            "project": {
              "type": "string"
            }
          }
        },
        {
          "type": "object",
          "required": [
            "kind",
            "missing_dir",
            "project"
          ],
          "properties": {
            "kind": {
              "type": "string",
              "enum": [
                "dangling-active-project"
              ]
            },
            "missing_dir": {
              "type": "string"
            },
            "project": {
              "type": "string"
            }
          }
        },
        {
          "type": "object",
          "required": [
            "kind",
            "root",
            "sub_kind"
          ],
          "properties": {
            "kind": {
              "type": "string",
              "enum": [
                "weave-root-identity-conflict"
              ]
            },
            "pointer_project": {
              "description": "The project named by `.rwv-active`; absent when that file is empty or unreadable.",
              "type": [
                "string",
                "null"
              ]
            },
            "root": {
              "description": "Absolute path of the weave root carrying both identity files.",
              "type": "string"
            },
            "sub_kind": {
              "$ref": "#/definitions/WeaveRootIdentityConflictKind"
            }
          }
        },
        {
          "type": "object",
          "required": [
            "kind",
            "marker_path",
            "primary"
          ],
          "properties": {
            "kind": {
              "type": "string",
              "enum": [
                "legacy-workweave-marker"
              ]
            },
            "marker_path": {
              "type": "string"
            },
            "primary": {
              "type": "string"
            }
          }
        },
        {
          "type": "object",
          "required": [
            "index_path",
            "kind",
            "project"
          ],
          "properties": {
            "index_path": {
              "type": "string"
            },
            "kind": {
              "type": "string",
              "enum": [
                "legacy-workweave-index"
              ]
            },
            "project": {
              "type": "string"
            }
          }
        },
        {
          "type": "object",
          "required": [
            "error",
            "index_path",
            "kind",
            "project"
          ],
          "properties": {
            "error": {
              "type": "string"
            },
            "index_path": {
              "type": "string"
            },
            "kind": {
              "type": "string",
              "enum": [
                "unreadable-workweave-index"
              ]
            },
            "project": {
              "type": "string"
            }
          }
        },
        {
          "type": "object",
          "required": [
            "kind",
            "manifest_path",
            "message",
            "project"
          ],
          "properties": {
            "kind": {
              "type": "string",
              "enum": [
                "unparseable-project"
              ]
            },
            "manifest_path": {
              "type": "string"
            },
            "message": {
              "description": "Free-form display string of the parse error. Named `message` (not `error`) to signal this is display text, not a typed discriminant.",
              "type": "string"
            },
            "project": {
              "type": "string"
            }
          }
        },
        {
          "type": "object",
          "required": [
            "kind",
            "sub_kind",
            "workweave_dir"
          ],
          "properties": {
            "kind": {
              "type": "string",
              "enum": [
                "workweave-tree-integrity"
              ]
            },
            "sub_kind": {
              "description": "Discriminator for the specific anomaly detected.",
              "allOf": [
                {
                  "$ref": "#/definitions/WorkweaveTreeIntegrityKind"
                }
              ]
            },
            "workweave_dir": {
              "description": "Absolute path to the workweave directory (or its marker file for file-level findings).",
              "type": "string"
            }
          }
        },
        {
          "type": "object",
          "required": [
            "absolute_path",
            "kind",
            "path",
            "project",
            "sub_kind"
          ],
          "properties": {
            "absolute_path": {
              "description": "Absolute path to the affected repo on disk.",
              "type": "string"
            },
            "kind": {
              "type": "string",
              "enum": [
                "provenance"
              ]
            },
            "path": {
              "description": "Manifest-relative path to the affected repo.",
              "type": "string"
            },
            "project": {
              "description": "Project the affected repo belongs to.",
              "type": "string"
            },
            "sub_kind": {
              "description": "Discriminator for the specific provenance anomaly.",
              "allOf": [
                {
                  "$ref": "#/definitions/ProvenanceKind"
                }
              ]
            }
          }
        },
        {
          "type": "object",
          "required": [
            "absolute_path",
            "kind",
            "path",
            "sub_kind"
          ],
          "properties": {
            "absolute_path": {
              "description": "Absolute path of the offending workspace (canonical slot or workweave checkout, per sub-kind semantics).",
              "type": "string"
            },
            "kind": {
              "type": "string",
              "enum": [
                "clone-topology"
              ]
            },
            "path": {
              "description": "Manifest-relative repo path (e.g. `github/cwalv/tmuxcc-broker`).",
              "type": "string"
            },
            "sub_kind": {
              "description": "Discriminator for the specific topology anomaly.",
              "allOf": [
                {
                  "$ref": "#/definitions/CloneTopologyKind"
                }
              ]
            }
          }
        },
        {
          "type": "object",
          "required": [
            "kind",
            "repo_path",
            "sub_kind"
          ],
          "properties": {
            "kind": {
              "type": "string",
              "enum": [
                "branch-discipline"
              ]
            },
            "repo_path": {
              "description": "Absolute path to the repo checkout where the violation was found (workweave checkout for (a), canonical clone for (b)/(c)).",
              "type": "string"
            },
            "sub_kind": {
              "description": "Discriminator for the specific branch-discipline anomaly.",
              "allOf": [
                {
                  "$ref": "#/definitions/BranchDisciplineKind"
                }
              ]
            }
          }
        },
        {
          "type": "object",
          "required": [
            "absolute_path",
            "kind",
            "missing_path",
            "path"
          ],
          "properties": {
            "absolute_path": {
              "type": "string"
            },
            "kind": {
              "type": "string",
              "enum": [
                "stale-worktree-registration"
              ]
            },
            "missing_path": {
              "description": "Absolute path of the missing worktree directory.",
              "type": "string"
            },
            "path": {
              "type": "string"
            },
            "workweave": {
              "description": "`None` for the primary weave.",
              "type": [
                "string",
                "null"
              ]
            }
          }
        },
        {
          "type": "object",
          "required": [
            "kind",
            "started_at",
            "verb",
            "workspace_dir"
          ],
          "properties": {
            "kind": {
              "type": "string",
              "enum": [
                "stale-op-state"
              ]
            },
            "started_at": {
              "description": "Raw `started_at` string from the op-state file (RFC3339 UTC).",
              "type": "string"
            },
            "verb": {
              "description": "The verb that started the stalled op — the one `--continue` resumes it under.",
              "allOf": [
                {
                  "$ref": "#/definitions/OpVerb"
                }
              ]
            },
            "workspace_dir": {
              "description": "Absolute path to the workspace dir that holds the `.rwv-op` file.",
              "type": "string"
            }
          }
        },
        {
          "type": "object",
          "required": [
            "kind",
            "op_id",
            "recorded_owner",
            "sub_kind",
            "workspace_dir"
          ],
          "properties": {
            "created_at": {
              "description": "RFC3339 UTC timestamp at which the lease was written. Observability-only — never a decision input.",
              "type": [
                "string",
                "null"
              ]
            },
            "kind": {
              "type": "string",
              "enum": [
                "dead-op-lease"
              ]
            },
            "op_id": {
              "description": "Op id recorded in the lease.",
              "type": "string"
            },
            "recorded_owner": {
              "description": "Owner workspace the lease pointed at.",
              "type": "string"
            },
            "sub_kind": {
              "description": "Discriminator for the specific dead-lease shape.",
              "allOf": [
                {
                  "$ref": "#/definitions/DeadOpLeaseKind"
                }
              ]
            },
            "workspace_dir": {
              "description": "Absolute path to the workspace dir holding the dangling lease.",
              "type": "string"
            }
          }
        },
        {
          "type": "object",
          "required": [
            "kind",
            "project",
            "ref_name",
            "store_path"
          ],
          "properties": {
            "kind": {
              "type": "string",
              "enum": [
                "dangling-ref-receipt"
              ]
            },
            "project": {
              "description": "The project whose registry holds the receipt.",
              "type": "string"
            },
            "ref_name": {
              "description": "The recorded ref name that does not exist in that store.",
              "type": "string"
            },
            "store_path": {
              "description": "Absolute path of the canonical store the receipt is keyed to.",
              "type": "string"
            }
          }
        },
        {
          "description": "See `CheckViolation::PreFlatRefReceipt`.",
          "type": "object",
          "required": [
            "kind",
            "project",
            "ref_name",
            "store_path"
          ],
          "properties": {
            "kind": {
              "type": "string",
              "enum": [
                "pre-flat-ref-receipt"
              ]
            },
            "project": {
              "description": "The project whose registry holds the receipt.",
              "type": "string"
            },
            "ref_name": {
              "description": "The recorded ref name that carries a `/` segment.",
              "type": "string"
            },
            "store_path": {
              "description": "Absolute path of the canonical store the receipt is keyed to.",
              "type": "string"
            }
          }
        },
        {
          "type": "object",
          "required": [
            "absolute_path",
            "kind",
            "op_id",
            "path",
            "sub_kind"
          ],
          "properties": {
            "absolute_path": {
              "type": "string"
            },
            "kind": {
              "type": "string",
              "enum": [
                "orphaned-savepoint"
              ]
            },
            "op_id": {
              "description": "Opaque op-id from the savepoint ref's trailing path component.",
              "type": "string"
            },
            "path": {
              "type": "string"
            },
            "sub_kind": {
              "description": "Safe-vs-live classification.",
              "allOf": [
                {
                  "$ref": "#/definitions/OrphanedSavepointKind"
                }
              ]
            },
            "workweave": {
              "description": "`None` for the primary weave.",
              "type": [
                "string",
                "null"
              ]
            }
          }
        },
        {
          "description": "See `CheckViolation::CargoVersionSkew`.",
          "type": "object",
          "required": [
            "crate_name",
            "kind",
            "occurrences"
          ],
          "properties": {
            "crate_name": {
              "description": "Registry crate name.",
              "type": "string"
            },
            "kind": {
              "type": "string",
              "enum": [
                "cargo-version-skew"
              ]
            },
            "occurrences": {
              "description": "Per-member requirement strings (post-`workspace = true` indirection). Sorted for stable output.",
              "type": "array",
              "items": {
                "$ref": "#/definitions/CargoSkewOccurrenceOutput"
              }
            }
          }
        },
        {
          "description": "See `CheckViolation::CargoPatchShadowing`.",
          "type": "object",
          "required": [
            "crate_name",
            "kind",
            "member_config",
            "registry",
            "weave_config"
          ],
          "properties": {
            "crate_name": {
              "description": "The specific crate name whose key collides.",
              "type": "string"
            },
            "kind": {
              "type": "string",
              "enum": [
                "cargo-patch-shadowing"
              ]
            },
            "member_config": {
              "description": "Member-level `.cargo/config.toml` that wins per cargo's closest-config-wins-per-key shadowing.",
              "type": "string"
            },
            "registry": {
              "description": "Registry sub-table name (e.g. `crates-io`).",
              "type": "string"
            },
            "weave_config": {
              "description": "Weave-level file (Cargo.toml or .cargo/config.toml) that carries the shadowed patch entry.",
              "type": "string"
            }
          }
        },
        {
          "description": "See `CheckViolation::MissingCanonicalClone`.",
          "type": "object",
          "required": [
            "absolute_path",
            "canonical_path",
            "kind",
            "path",
            "workweave"
          ],
          "properties": {
            "absolute_path": {
              "description": "Absolute path of the worktree checkout in the workweave.",
              "type": "string"
            },
            "canonical_path": {
              "description": "Absolute path of the canonical clone directory that is absent.",
              "type": "string"
            },
            "kind": {
              "type": "string",
              "enum": [
                "missing-canonical-clone"
              ]
            },
            "path": {
              "description": "Manifest-relative path to the affected repo (same value as `CheckViolation::MissingCanonicalClone::repo`).",
              "type": "string"
            },
            "workweave": {
              "description": "Workweave name.",
              "type": "string"
            }
          }
        },
        {
          "description": "See `CheckViolation::UninitializedSubmodule`.",
          "type": "object",
          "required": [
            "absolute_path",
            "empty_paths",
            "kind",
            "path",
            "workweave"
          ],
          "properties": {
            "absolute_path": {
              "description": "Absolute path to the repo worktree that has uninitialized submodules.",
              "type": "string"
            },
            "empty_paths": {
              "description": "Submodule paths (relative to the repo root) that are empty on disk.",
              "type": "array",
              "items": {
                "type": "string"
              }
            },
            "kind": {
              "type": "string",
              "enum": [
                "uninitialized-submodule"
              ]
            },
            "path": {
              "description": "Manifest-relative path to the repo.",
              "type": "string"
            },
            "workweave": {
              "description": "Workweave name.",
              "type": "string"
            }
          }
        },
        {
          "description": "See `CheckViolation::PhantomMergeDriver`.",
          "type": "object",
          "required": [
            "absolute_path",
            "driver",
            "kind",
            "path",
            "pattern"
          ],
          "properties": {
            "absolute_path": {
              "description": "Absolute path to that repo on disk.",
              "type": "string"
            },
            "driver": {
              "description": "The `rwv-`-prefixed driver name that resolves to nothing.",
              "type": "string"
            },
            "kind": {
              "type": "string",
              "enum": [
                "phantom-merge-driver"
              ]
            },
            "path": {
              "description": "Manifest-relative path to the repo carrying the `.gitattributes`.",
              "type": "string"
            },
            "pattern": {
              "description": "The path pattern the offending line assigns the driver to.",
              "type": "string"
            }
          }
        }
      ]
    },
    "WeaveRootIdentityConflictKind": {
      "description": "Discriminator for `CheckViolation::WeaveRootIdentityConflict` findings: whether anything outside the tree settles which of its two identity files is the true one.\n\nThe split is not symmetric, and deliberately so. The naive reading — \"a workweave's stray pointer is safe to delete, a primary's stray marker is not\" — cannot be implemented, because it presumes we already know which kind of root this is, and the marker's presence is the only witness of that. Primary-ness has no independent signature: a primary root and a workweave root both hold `projects/` and registry directories. So the question \"which file is the stray?\" is exactly the question the conflict makes unanswerable from the tree alone, and the discriminator has to come from somewhere else.\n\nThe registry is that somewhere else. It lives at `<primary>/projects/<project>/.rwv-workweave-index`, is written only by `rwv workweave create`, and records the absolute path of every workweave it made. A tree the registry names is a workweave on the authority of a file the tree does not contain and could not have forged by being copied.\n\nNote what is deliberately NOT used as the discriminator: whether the tree itself contains a `.rwv-workweave-index`. That looks like a primary-ness signature and is not one. The index is untracked, so whether a workweave inherits a copy depends on whether its `projects/<project>/` is a linked worktree (it is not copied) or a plain directory copy (it is) — a topology accident, not a fact about identity. Keying on it would classify real workweaves as unwitnessed in the copy topology and leave their stray pointers unfixable.",
      "oneOf": [
        {
          "description": "The marker names THIS workspace's primary, and that primary's registry for the marker's project records THIS exact directory. External evidence settles it: the tree is a workweave, so `.rwv-active` is the redundant copy and deleting it destroys nothing the marker and the registry do not already say. Auto-fixable — `--fix` deletes the pointer and leaves the marker.",
          "type": "object",
          "required": [
            "registered-workweave"
          ],
          "properties": {
            "registered-workweave": {
              "type": "object",
              "required": [
                "project",
                "workweave_name"
              ],
              "properties": {
                "project": {
                  "description": "Project the marker names (and under whose registry it is recorded).",
                  "type": "string"
                },
                "workweave_name": {
                  "description": "Name the registry records this directory under.",
                  "type": "string"
                }
              }
            }
          },
          "additionalProperties": false
        },
        {
          "description": "The marker itself cannot witness the identity it claims — unreadable, legacy (YAML, or missing `parent:`), or naming a `primary:` that verifies as no workspace at all. `observe_root` classifies a root like this `MarkerUnverifiable` rather than `Disputed` even with `.rwv-active` present alongside: a marker that cannot prove its own claim cannot prove which of the two files is the stray either, so this is report-only for the same reason `Unwitnessed` is. Never auto-fixed — repairing the marker (`rwv doctor --fix` migrates a legacy one; a dangling or unreadable one needs a hand edit) is a separate step from clearing a pointer whose redundancy the marker cannot yet vouch for.",
          "type": "object",
          "required": [
            "marker-unverifiable"
          ],
          "properties": {
            "marker-unverifiable": {
              "type": "object",
              "required": [
                "defect",
                "marker_path"
              ],
              "properties": {
                "defect": {
                  "description": "Why the marker cannot witness its own claim.",
                  "allOf": [
                    {
                      "$ref": "#/definitions/MarkerDefect"
                    }
                  ]
                },
                "marker_path": {
                  "description": "Absolute path to the `.rwv-workweave` file.",
                  "type": "string"
                }
              }
            }
          },
          "additionalProperties": false
        },
        {
          "description": "The marker is readable and verifies, but names a different primary, or names this primary with no registry entry pointing back at this directory. Report-only. Deleting either file here would be a guess, and the wrong guess destroys operator state — the marker in particular carries `primary` and `parent` values that exist nowhere else.\n\nThe most likely cause of the last shape is a workweave copied out-of-band (`cp -r`): the copy carries both files, and the registry still names only the original.",
          "type": "object",
          "required": [
            "unwitnessed"
          ],
          "properties": {
            "unwitnessed": {
              "type": "object",
              "required": [
                "detail"
              ],
              "properties": {
                "detail": {
                  "description": "Why no external evidence was found, in operator-facing terms.",
                  "type": "string"
                }
              }
            }
          },
          "additionalProperties": false
        }
      ]
    },
    "WorkingTreeDriftKind": {
      "description": "How stale working-tree files should be treated.",
      "oneOf": [
        {
          "description": "All modified files' on-disk content matches blobs reachable from HEAD. Safe to restore with `git checkout HEAD -- <files>` — no work is lost.",
          "type": "string",
          "enum": [
            "safe-to-fix"
          ]
        },
        {
          "description": "At least one modified file has on-disk content not found in any recent ancestor's tree. The user has active edits; `--fix` must not touch this.",
          "type": "string",
          "enum": [
            "live-edits"
          ]
        }
      ]
    },
    "WorkweaveTreeIntegrityKind": {
      "description": "Discriminator for `CheckViolation::WorkweaveTreeIntegrity` findings.",
      "oneOf": [
        {
          "description": "The marker's `parent:` path no longer exists on disk. The workweave's parent was retired or deleted out-of-band (a crash mid-adopt, or a hand-deletion) while this child remained. Bare `rwv sync-to` would otherwise mis-fire; instead it now surfaces friendly doctor-remediation text. Auto-fixable: `rwv doctor --fix` re-points `parent` to primary (which always exists). Normal retire/delete adopts children before the parent is destroyed, so this only arises off the happy path.",
          "type": "object",
          "required": [
            "dangling-parent"
          ],
          "properties": {
            "dangling-parent": {
              "type": "object",
              "required": [
                "parent_path"
              ],
              "properties": {
                "parent_path": {
                  "description": "The missing parent path recorded in the marker.",
                  "type": "string"
                }
              }
            }
          },
          "additionalProperties": false
        },
        {
          "description": "A parent-chain anomaly: cycle, parent==self, or the parent marker's project differs from this workweave's project. Cannot arise from `rwv workweave create`; can arise from hand-edited markers or directory copies. Report-only.",
          "type": "object",
          "required": [
            "parent-chain-anomaly"
          ],
          "properties": {
            "parent-chain-anomaly": {
              "type": "object",
              "required": [
                "detail"
              ],
              "properties": {
                "detail": {
                  "description": "Short human-readable description of the anomaly.",
                  "type": "string"
                }
              }
            }
          },
          "additionalProperties": false
        },
        {
          "description": "A directory under `.workweaves/` that has no `.rwv-workweave` marker file at all. It may be an orphaned directory from a failed create, a manually placed directory, or a remnant of a deleted workweave. Report-only.",
          "type": "string",
          "enum": [
            "unregistered-dir"
          ]
        },
        {
          "description": "The marker's `primary:` path does not resolve to the workspace this scan was started from, and the path itself resolves to no workspace either (missing, or exists but is not a workspace root) — e.g. an rsync'd workweave whose marker still points at the origin machine's absolute path. Report-only.",
          "type": "object",
          "required": [
            "foreign-primary"
          ],
          "properties": {
            "foreign-primary": {
              "type": "object",
              "required": [
                "marker_primary"
              ],
              "properties": {
                "marker_primary": {
                  "description": "The primary path recorded in the marker (unresolved).",
                  "type": "string"
                }
              }
            }
          },
          "additionalProperties": false
        },
        {
          "description": "The marker's `primary:` path does not match this workspace, but resolves to a different, valid workspace root — the normal shape when several weaves share one workweave container. Not a defect in this workweave, so excluded from the default text report: every sibling weave's doctor would otherwise repeat this about every other sibling. Still enumerated under `--json`.",
          "type": "object",
          "required": [
            "foreign-primary-other-workspace"
          ],
          "properties": {
            "foreign-primary-other-workspace": {
              "type": "object",
              "required": [
                "marker_primary"
              ],
              "properties": {
                "marker_primary": {
                  "description": "The other workspace's primary path (resolved).",
                  "type": "string"
                }
              }
            }
          },
          "additionalProperties": false
        },
        {
          "description": "A registered workweave entry whose recorded path is not a valid workweave (missing directory, missing marker, or marker validation fails). Auto-fixable: `rwv doctor --fix` prunes the stale entry.\n\nThis surfaces both \"workweave was deleted out-of-band with the registry left behind\" and \"index committed to VCS carries paths that are wrong on this machine\" — the design's advisory-index doctrine depends on doctor catching both.\n\n`project` is a plain `String` on the wire because `ProjectName` does not (yet) derive `JsonSchema`; every other sub-kind uses `String` for names on the wire for the same reason.",
          "type": "object",
          "required": [
            "stale-registry-entry"
          ],
          "properties": {
            "stale-registry-entry": {
              "type": "object",
              "required": [
                "project",
                "reason",
                "recorded_path",
                "workweave_name"
              ],
              "properties": {
                "project": {
                  "description": "Project the stale entry belongs to.",
                  "type": "string"
                },
                "reason": {
                  "description": "Human-readable reason the entry failed validation.",
                  "type": "string"
                },
                "recorded_path": {
                  "description": "The recorded absolute path (which no longer round-trips).",
                  "type": "string"
                },
                "workweave_name": {
                  "description": "The recorded name of the workweave.",
                  "type": "string"
                }
              }
            }
          },
          "additionalProperties": false
        },
        {
          "description": "A marker-bearing directory in a workweave container whose `(project, name)` are NOT recorded in that project's `.rwv-workweave-index`. The workweave exists on disk but the primary-side registry does not know about it. Auto-fixable via `rwv doctor --fix` (adopts the entry into the registry) — the design requires operator-consented adoption, so read paths (`list`, `delete`) deliberately do NOT auto-adopt on the fly.",
          "type": "object",
          "required": [
            "unregistered-workweave"
          ],
          "properties": {
            "unregistered-workweave": {
              "type": "object",
              "required": [
                "project",
                "workweave_name"
              ],
              "properties": {
                "project": {
                  "description": "Project this orphan workweave records in its marker.",
                  "type": "string"
                },
                "workweave_name": {
                  "description": "Workweave name parsed from the directory basename.",
                  "type": "string"
                }
              }
            }
          },
          "additionalProperties": false
        },
        {
          "description": "The `.rwv-workweave-index` file at `projects/<project>/` is tracked by the project repo's VCS. The index is machine-local state and should not be committed; a checked-in copy propagates absolute paths to every clone and every workweave checkout. Report-only — `--fix` cannot un-track without touching commit history; the operator runs `git rm --cached projects/<project>/.rwv-workweave-index` and updates `.gitignore`.",
          "type": "object",
          "required": [
            "tracked-index"
          ],
          "properties": {
            "tracked-index": {
              "type": "object",
              "required": [
                "index_path",
                "project"
              ],
              "properties": {
                "index_path": {
                  "description": "Path to the tracked index file.",
                  "type": "string"
                },
                "project": {
                  "description": "Project whose index is committed.",
                  "type": "string"
                }
              }
            }
          },
          "additionalProperties": false
        },
        {
          "description": "A `.rwv-workweave` marker that parses as neither current JSON nor a `migrate_legacy`-repairable legacy shape — most often YAML with no `primary:` for `migrate_legacy` to backfill from. Every marker rwv has ever written carries all three fields, so this is hand-corruption or a truncated write rather than a shape upgrading produces. Report-only: there is no value here to guess a repair from.",
          "type": "object",
          "required": [
            "unreadable-marker"
          ],
          "properties": {
            "unreadable-marker": {
              "type": "object",
              "required": [
                "detail"
              ],
              "properties": {
                "detail": {
                  "description": "Why the marker cannot be read, and what to write in its place.",
                  "type": "string"
                }
              }
            }
          },
          "additionalProperties": false
        },
        {
          "description": "A marker-bearing workweave directory whose basename disagrees with its records: it does not spell `{marker project}--{name}`, where the name is the one the project's registry records for this path (or, for an unregistered directory, the basename's own name half).\n\nOnly a hand-rename produces this — `rwv workweave create` derives the directory name from the same (project, name) pair it writes into the marker and the registry. Identity is by record, so the scans keep working from the records; what this finding reports is that the directory's own name now lies about them, which misleads operators and collides with any future workweave whose records genuinely mint this basename. When the basename is unparseable AND no registry entry names the path, identity is unrecoverable and the scans skip the directory entirely — this finding is then the only signal.\n\nReport-only: renaming the directory back is the operator's one-step remedy (the checkouts inside were registered under the recorded name, so restoring it also restores their worktree back-pointers), and when no record exists the intended name is not derivable from the directory itself.",
          "type": "object",
          "required": [
            "misnamed-dir"
          ],
          "properties": {
            "misnamed-dir": {
              "type": "object",
              "required": [
                "detail"
              ],
              "properties": {
                "detail": {
                  "description": "What disagrees: which half, and with which record.",
                  "type": "string"
                },
                "expected_dir_name": {
                  "description": "The basename the records expect (`{project}--{name}`), when the records pin one. `None` when the basename is unparseable and no registry entry names this path.",
                  "type": [
                    "string",
                    "null"
                  ]
                }
              }
            }
          },
          "additionalProperties": false
        }
      ]
    }
  }
}
```

## Exit codes

- `0` — no violations found.
- non-zero — violations found, or an error occurred resolving the
  workspace.

## Examples

Get a JSON report of violations for the active project:

```
rwv doctor --json
```

Get a weave-wide JSON report (all projects, orphan detection enabled):

```
rwv doctor --all --json
```

Find every stale lock and the paths involved (weave-wide):

```
rwv doctor --all --json | jq '.violations[] | select(.kind == "stale-lock")'
```

Auto-fix safe drift (index trees that match a known ancestor) and
migrate any manifests still using the legacy `role: primary` spelling:

```
rwv doctor --fix
```

## Common errors

- *missing-replay-exclusion* on a project repo — the project repo lacks
  `rwv.lock merge=rwv-ours` in `.gitattributes`, or still carries the
  legacy `rwv.lock merge=ours` spelling (renamed to close an
  accidental-collision hazard with an unrelated global
  `merge.ours.driver` during bare `git rebase --continue`), or carries
  both lines at once, which leaves the legacy name live whichever way
  git's reading order resolves them. Run `rwv doctor --fix` to add,
  migrate, or drop the line — those paths also commit the change
  (skipping the commit when the repo has other staged work).
- *phantom-merge-driver* — a `.gitattributes` line in a managed repo assigns
  an `rwv-`-prefixed merge driver rwv does not define. Git resolves
  `merge=<name>` through `merge.<name>.driver` config and falls back to a
  textual merge, silently, when nothing defines the name — and only rwv
  writes or defines names under the `rwv-` prefix, so the line will never do
  anything. Report-only: use `merge=rwv-ours` for a derived path whose
  target-side copy should win during replay, or drop the line. The inverse is
  deliberately not reported — declaring a path derived is each repo's own
  choice.
- *legacy-manifest-format* — a project directory holds an `rwv.yaml`, the
  name the manifest had before it became TOML, and no `rwv.toml`. Nothing in
  the project loads. `--fix` has no arm: the manifest is hand-authored, and
  the comments and key order in it do not survive a mechanical cross-format
  rewrite. Rewrite it as `rwv.toml` by hand and delete the `rwv.yaml`.
- *index-drift* with `sub_kind: live-staged` — the user has staged content
  that doesn't match a known tree. `--fix` will refuse; resolve manually.
- *orphaned-clone* — a directory under a registry path that isn't listed in
  any `rwv.toml`. Only surfaced under `--all`. Either add it to a manifest
  or remove it.
- *surfacing: `<file>` is not surfaced* (or *symlink resolves to …*) — the
  framework Axis-1 surfacing check found a missing or mis-resolved symlink
  in the active project's surfacing set. Run `rwv doctor --fix` to
  re-surface the symlink. If a real file occupies the surfacing path, the
  warning is marked not-safe-to-fix; resolve manually (move or remove the
  occupying file, then rerun `--fix`).
- *surfacing: `<file>` resolves into project `<other>`* — a weave-root
  symlink surfaces a SHARED name out of a project other than the one the
  weave root presents. Shared names — every surfaced name except
  `<project>.code-workspace`, which cannot collide and so may be surfaced
  for any project — follow `.rwv-active` (a workweave's `.rwv-workweave`)
  and nothing else. Run `rwv doctor --fix` scoped to the presented project
  to reclaim the name. `--project X` does not produce this state: a repair
  scoped to a project the root does not present surfaces only X's
  per-project names.
- *workweave-tree-integrity / dangling-parent* — a workweave's `.rwv-workweave`
  marker records a `parent:` path that no longer exists on disk (the parent
  was retired or deleted out-of-band). Run `rwv doctor --fix` to re-point
  the marker to the primary workspace. Branch names are left untouched. Once
  fixed, re-run `rwv sync` or `rwv sync-to` from the workweave.
- *workweave-tree-integrity / parent-chain-anomaly*, *unregistered-dir*,
  *foreign-primary* — other marker tree anomalies; report-only (`--fix` does
  not auto-remediate them).
- *member-incompatibility* — a value rwv seeded once and then stepped back
  from (an `Ownership::DefaultOnly` key) is incompatible with what the
  workspace members require: today, a `go.work` go directive below the
  highest `go` in the members' `go.mod` files, which the go toolchain
  refuses to build. **Not drift** — the value is the operator's by contract,
  so `rwv doctor` still reports the file itself as clean and `--fix` cannot
  repair this (it re-runs activation, which will not overwrite the value).
  The two remedies are both yours: raise the directive in `go.work`, or lower
  the requirement in the member's `go.mod`.
