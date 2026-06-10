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
rwv doctor [--all] [--locked] [--json] [--fix]
```

- `--all` runs checks across every project under `projects/` and enables
  weave-wide orphan detection (repos on disk that belong to no project).
  Without `--all`, only the active project is checked and orphan detection
  is skipped (a repo absent from the active project may belong to another
  project — flagging it as orphaned would produce false positives).
- `--locked` exits zero iff every repo's tip matches its `rwv.lock`
  entry. Prints per-repo `ok` / `tip ≠ lock` lines to stdout. Useful
  as a scriptable precondition before `rwv sync`. Mutually exclusive
  with `--fix` and `--json`.
- `--json` emits machine-readable output (see Output below). Mutually
  exclusive with `--locked` and `--fix`. Honors the same scoping as the
  default text output: project-scoped by default, weave-wide with `--all`.
- `--fix` attempts auto-remediation for variants that are safe to fix:
  index drift where the displaced tree is a known ancestor, working-tree
  drift where on-disk content matches a known blob, missing
  `rwv.lock merge=ours` replay-exclusion, and legacy `role: primary`
  manifest spellings (rewritten to `role: owned` in place — preserves
  comments and key order). Idempotent. Mutually exclusive with
  `--locked` and `--json`.

Run `rwv --help doctor` for the full clap surface.

## Output

Default text output is one human-readable line per violation, grouped by
severity. Under `--json`, output is the envelope:

```
{
  "$schema": "<url>",
  "violations": [ { "kind": "...", ... }, ... ]
}
```

The `$schema` URL points to the committed schema artifact. Variants are
discriminated by the `kind` tag — `orphaned-clone`, `dangling-reference`,
`missing-role`, `stale-lock`, `workweave-drift`, `index-drift`,
`working-tree-drift`, `missing-replay-exclusion`, `legacy-role-primary`.
Every per-repo variant carries `path` (manifest-relative) and
`absolute_path` (fully resolved). Variants with subkinds
(`workweave-drift`, `index-drift`, `working-tree-drift`) carry an
additional `sub_kind` field. `legacy-role-primary` carries `project` and
`manifest_path` so the caller can locate the file `--fix` will rewrite.

Schema:

```json
{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "title": "DoctorEnvelope",
  "description": "Output envelope for `rwv doctor --json`. By default only the active project is checked and orphan detection is skipped; pass `--all` to scan every project and enable weave-wide orphan detection. The `violations` array contains one entry per finding; an empty array means the checked scope is clean.",
  "type": "object",
  "required": [
    "$schema",
    "violations"
  ],
  "properties": {
    "$schema": {
      "type": "string"
    },
    "violations": {
      "type": "array",
      "items": {
        "$ref": "#/definitions/ViolationOutput"
      }
    }
  },
  "definitions": {
    "CloneTopologyKind": {
      "description": "Discriminator for [`CheckViolation::CloneTopology`] findings.\n\nThe four sub-kinds enumerate the ways the bottom tier of the stability stack ([clone-topology.md](../../docs/explanation/joints/clone-topology.md)) can break: a manifest repo's slot at `<weave>/<repo_path>` must be a \"canonical store\" (a full clone), and every workweave checkout `<workweave>/<repo_path>` must be a linked workspace whose VCS common store resolves to that canonical store. Each variant names a distinct way the on-disk shape diverges from that spec.",
      "oneOf": [
        {
          "description": "A full clone (its own canonical store) is hosted under `.workweaves/` instead of at the manifest's canonical slot. The inverted-primary case (fo-a0spgj `tmuxcc-broker`): the canonical store has migrated into one workweave and other workweaves' checkouts link into *it*, not into `<weave>/<repo_path>`.",
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
    "ProvenanceKind": {
      "description": "Discriminator for [`CheckViolation::Provenance`] findings.",
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
                  "description": "The URL recorded in the manifest (`rwv.yaml`).",
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
            "project"
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
            }
          }
        },
        {
          "type": "object",
          "required": [
            "kind",
            "manifest_path",
            "project"
          ],
          "properties": {
            "kind": {
              "type": "string",
              "enum": [
                "legacy-role-primary"
              ]
            },
            "manifest_path": {
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
              "description": "Free-form display string of the YAML parse error. Named `message` (not `error`) to signal this is display text, not a typed discriminant.",
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
      "description": "Discriminator for [`CheckViolation::WorkweaveTreeIntegrity`] findings.",
      "oneOf": [
        {
          "description": "The marker's `parent:` path no longer exists on disk. The workweave's parent was retired or deleted while this child remained. Bare `rwv sync-to` and `--retire` will mis-fire until the operator re-points `parent` to a valid workspace. Report-only.",
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
          "description": "The marker's `primary:` path does not resolve to the workspace this scan was started from (e.g. an rsync'd workweave whose marker still points at the origin machine's absolute path). Report-only.",
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
  `rwv.lock merge=ours` in `.gitattributes`. Either run `rwv doctor --fix`
  to append it, or add the line manually.
- *legacy-role-primary* — a project `rwv.yaml` still uses `role: primary`
  (renamed to `role: owned`; the back-compat alias has since been dropped).
  Run `rwv doctor --fix` to migrate every affected manifest in place;
  comments and key order are preserved.
- *index-drift* with `sub_kind: live-staged` — the user has staged content
  that doesn't match a known tree. `--fix` will refuse; resolve manually.
- *orphaned-clone* — a directory under a registry path that isn't listed in
  any `rwv.yaml`. Only surfaced under `--all`. Either add it to a manifest
  or remove it.
