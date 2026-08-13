//! go.work integration — hybrid merge port.
//!
//! # Strategy
//!
//! **PRIMARY** (when `go` is on PATH and `FORCE_GOWORK_FALLBACK` thread-local
//! is not set in tests): `go work use ./<dir>` and `go work edit
//! -dropuse=./<old>` always run; `go work edit -go=<v>` runs only when the
//! pre-edit file has no go-line yet (DefaultOnly, mirroring FALLBACK below).
//! The `go` tool round-trips `replace`/`toolchain`/`godebug` and all comments
//! via x/mod/modfile.
//!
//! Guarding rwv's own `-go=` call is not sufficient to honour DefaultOnly on
//! this path: `go work use` raises the go directive to the strongest `go`
//! requirement across the modules it adds, whatever the file said, and `go work
//! init` stamps a fresh file with the version of whichever `go` ran it. So the
//! directive is snapshotted before the `use` loop and restored after it by
//! `restore_go_directive` — otherwise the same `rwv add` would rewrite an
//! operator's pin on a machine with `go` installed and preserve it on one
//! without.
//!
//! **FALLBACK** (no `go` on PATH, or forced in tests): use
//! [`merge_activate`] / [`strip_deactivate`].  Edits the `use (…)`
//! region (Author) and the leading `go <version>` line (DefaultOnly — sets the
//! default from config or `max_go_version` but never overwrites an existing
//! go-line).  All other directives survive byte-for-byte.
//!
//! The fallback is mandatory because:
//! 1. `go` is not on PATH in CI / typical test environments.
//! 2. Tests exercise it deterministically via the thread-local override.
//!
//! # max_go_version
//!
//! Used on both paths as the DefaultOnly go-version default when config does
//! not supply one. The `["go"]` entry is `Ownership::DefaultOnly` on PRIMARY
//! and FALLBACK alike: if a go-line is already present in the file it is
//! preserved unconditionally — `max_go_version` only takes effect on a fresh
//! (greenfield) or go-line-absent file.
//!
//! When neither config nor `max_go_version` supplies a value — no member go.mod
//! carries a parseable `go` directive — neither path writes a go-line at all.
//! The only version available to write would be the one `go work init` stamped
//! from the toolchain on this machine, and go.work is committed.
//!
//! # Deactivate
//!
//! Uses [`strip_deactivate`] with `owned_keys = [["use"]]` only — never
//! includes `["go"]` per the C2 author's note.  Delete-if-empty is delegated
//! to [`GoWorkDoc::is_empty`] which returns true only when no `use` entries
//! AND no `replace`/`godebug`/non-comment lines beyond `go`/`toolchain`/
//! whitespace remain.
//!
//! # Test-only fallback override
//!
//! In `#[cfg(test)]` builds a thread-local `FORCE_GOWORK_FALLBACK` is
//! declared.  Tests set it to `true` to guarantee the hand-parse path is
//! taken regardless of whether `go` happens to be on PATH in the test runner.

use crate::integration::{Integration, IntegrationContext, Issue, IssueKind, OwnedPath, Severity};
use crate::integrations::merge::{
    drift_issues, holds_owned_region, keypath, merge_activate, missing_issue,
    orphaned_region_issues, strip_deactivate, GoWorkDoc, KeyPath, ManagedDoc,
    MemberIncompatibility, OwnedValue, Ownership,
};
use crate::manifest::GoWorkConfig;
use anyhow::Context;
use std::path::Path;

pub struct GoWork;

// ---------------------------------------------------------------------------
// Test-only PATH override
// ---------------------------------------------------------------------------

#[cfg(test)]
std::thread_local! {
    /// Set to `true` inside a test to force the hand-parse fallback even when
    /// `go` is on PATH.  Reset to `false` after each test (each test is a
    /// separate thread invocation).
    static FORCE_GOWORK_FALLBACK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn go_on_path() -> bool {
    #[cfg(test)]
    {
        let forced = FORCE_GOWORK_FALLBACK.with(|f| f.get());
        if forced {
            return false;
        }
    }
    which::which("go").is_ok()
}

// ---------------------------------------------------------------------------
// Integration impl
// ---------------------------------------------------------------------------

impl Integration for GoWork {
    fn name(&self) -> &str {
        "go-work"
    }

    fn default_enabled(&self) -> bool {
        true
    }

    fn detection_manifests(&self) -> &[&str] {
        &["go.mod"]
    }

    fn activate(&self, ctx: &IntegrationContext) -> anyhow::Result<()> {
        let paths = ctx.detect_repos_with_manifest("go.mod");
        // The authored `use` block is a function of the manifest alone.
        // Returning early instead would make it a function of history too: the
        // last member's path stays behind, in a marked key rwv still owns and
        // would no longer author.
        if paths.is_empty() {
            return Self::strip_managed_region(ctx.output_dir);
        }

        let go_work_path = ctx.output_dir.join("go.work");

        // ── USER-HELD OWNERSHIP GUARD ──────────────────────────────────────
        // Parity with cargo-workspace: if go.work already exists with a `use`
        // block but NO `// managed by repoweave` marker, the user holds the
        // pen. Leave the file byte-for-byte unchanged and return without
        // dispatching to either the go-tool or hand-edit path.
        //
        // This guard must live here (before the path split) so it covers both:
        //   - `activate_via_go_tool`: would unconditionally run `go work use`
        //     and inject the marker via `ensure_marker_present`.
        //   - `activate_via_hand_edit`: `merge_activate` defers the `use` key
        //     but still serializes + writes the file (could mutate bytes when
        //     a go-version default is injected).
        if go_work_path.exists() {
            let text = std::fs::read_to_string(&go_work_path).with_context(|| {
                format!("reading {} for ownership check", go_work_path.display())
            })?;
            let doc = GoWorkDoc::parse(&text).with_context(|| {
                format!("parsing {} for ownership check", go_work_path.display())
            })?;
            let owned_keys = Self::owned_keys();
            if !doc.has_marker(&owned_keys) && doc.key_present(&keypath(["use"])) {
                // User-held: use block present but no rwv marker.
                // Do NOT clobber the file. The ownership condition is surfaced
                // structurally by verify() via drift_issues() (Severity::Warning,
                // safe_to_fix=false), consistent with all other hybrid integrations.
                // No ad-hoc eprintln here — callers see the Issue through the
                // standard Issue stream.
                return Ok(());
            }
        }

        // Parse per-integration config (tolerates absent block).
        let cfg: GoWorkConfig = ctx.config.settings().unwrap_or_default();

        // Determine the go-version default, if any: explicit config wins,
        // else the max across member go.mod files. Both paths apply this as
        // Ownership::DefaultOnly — only when the target file's go-line is
        // currently absent; an existing go-line is never overwritten.
        let go_version_override: Option<String> = cfg
            .go_version
            .clone()
            .or_else(|| max_go_version(&paths, ctx.workspace_root));

        if go_on_path() {
            activate_via_go_tool(
                &go_work_path,
                &paths,
                go_version_override.as_deref(),
                ctx.workspace_root,
            )?;
        } else {
            activate_via_hand_edit(&go_work_path, &paths, go_version_override.as_deref())?;
        }

        Ok(())
    }

    fn deactivate(&self, root: &Path) -> anyhow::Result<()> {
        Self::strip_managed_region(root)
    }

    fn check(&self, ctx: &IntegrationContext) -> anyhow::Result<Vec<Issue>> {
        let paths = ctx.detect_repos_with_manifest("go.mod");
        if paths.is_empty() {
            return Ok(vec![]);
        }

        let mut issues = Vec::new();
        if which::which("go").is_err() {
            issues.push(Issue {
                integration: self.name().to_string(),
                severity: Severity::Warning,
                message: "go is not on PATH; using hand-edit fallback for go.work".to_string(),
                kind: IssueKind::ToolMissing,
                safe_to_fix: true,
            });
        }
        Ok(issues)
    }

    /// Content-correct check (Axis-2) for `go.work`.
    ///
    /// States mirrored from cargo-workspace:
    ///
    /// - **MISSING** (`safe_to_fix=true`): file absent but repos detected.
    /// - **USER-HELD** (`safe_to_fix=false`): file present, has a `use (...)` block,
    ///   but NO `// managed by repoweave` marker.
    /// - **DRIFT** (`safe_to_fix=true`): marker present but `use` entries diverge
    ///   from what the current config would generate.
    /// - **CLEAN**: marker present and content matches.
    ///
    /// Note: always uses the fallback path for comparison (GoWorkDoc) so verify()
    /// is deterministic regardless of whether `go` is on PATH.
    fn verify(&self, ctx: &IntegrationContext) -> anyhow::Result<Vec<Issue>> {
        use crate::integrations::merge::ManagedDoc;

        let repo_paths = ctx.detect_repos_with_manifest("go.mod");
        if repo_paths.is_empty() {
            return Ok(orphaned_region_issues::<GoWorkDoc>(
                self.name(),
                &ctx.output_dir.join("go.work"),
                &Self::owned_keys(),
                "rwv.toml declares no go members, so the use block no longer \
                 belongs to rwv.",
            ));
        }

        let path = ctx.output_dir.join("go.work");

        // ── MISSING ────────────────────────────────────────────────────────
        if !path.exists() {
            return Ok(vec![missing_issue(self.name(), &path)]);
        }

        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {} for verify", path.display()))?;
        let doc = GoWorkDoc::parse(&text)
            .with_context(|| format!("parsing {} for verify", path.display()))?;

        let owned_keys = Self::owned_keys();
        let marker_present = doc.has_marker(&owned_keys);
        let owned_key_present = doc.key_present(&keypath(["use"]));

        // Only `use` (Ownership::Author) is checked for drift here. `go` is
        // Ownership::DefaultOnly: once present, any on-disk value is CLEAN by
        // contract — activate() never overwrites an existing go-line, so
        // flagging its value as drift would report a finding `--fix` cannot
        // actually resolve. Same exclusion cargo-workspace applies to its
        // DefaultOnly `resolver` key.
        //
        // Compare on-disk `use` entries against what activate() would write
        // (`./<repo-path>` entries). The shared helper sorts + dedups both
        // sides before comparing and dispatches USER-HELD → DRIFT → CLEAN.
        let expected: Vec<String> = repo_paths.iter().map(|p| format!("./{}", p)).collect();
        // `read_current_uses_from_file` returns an empty vec when the use block
        // is absent (never `None`); pre-lift compared that empty vec directly,
        // so pass `Some`.
        let on_disk = read_current_uses_from_file(&path);

        Ok(drift_issues(
            self.name(),
            &path,
            marker_present,
            owned_key_present,
            Some(&on_disk),
            &expected,
            "Cut over manually or add the '// managed by repoweave' marker",
            "on-disk use entries differ from rwv.toml config.",
        ))
    }

    /// The go-work member-incompatibility predicate: the on-disk go directive
    /// is **below** the strongest `go` requirement across the members' `go.mod`
    /// files.
    ///
    /// A hard predicate in the sense
    /// [`Integration::member_incompatibility`]
    /// requires: it reads the members' `go.mod` files and the managed `go.work`
    /// and nothing else, and the consequence needs no interpretation — the go
    /// toolchain refuses to build a workspace whose go.work asks for less than
    /// a member's go.mod declares.
    ///
    /// Not the same question as [`Self::verify`]: the go-line is
    /// `Ownership::DefaultOnly`, so a divergence from rwv's computed default
    /// stays CLEAN there and always will. This reports the orthogonal fact that
    /// the value the operator holds does not build.
    ///
    /// Silent (returns `None`) when:
    /// - no member declares a `go.mod` — nothing to require;
    /// - `go.work` has no go-line — with no pin on disk there is no choice to
    ///   be incompatible with;
    /// - either side is unparseable — the category states facts or nothing;
    /// - the on-disk version is at or above the requirement.
    ///
    /// Comparison includes the patch component (absent reads as `0`), so
    /// `go 1.26` against a member's `go 1.26.1` is a breach and `go 1.9`
    /// against `go 1.21` is a breach — the latter being the case a string
    /// compare would get backwards.
    fn member_incompatibility(
        &self,
        ctx: &IntegrationContext,
    ) -> anyhow::Result<Option<MemberIncompatibility>> {
        let repo_paths = ctx.detect_repos_with_manifest("go.mod");
        if repo_paths.is_empty() {
            return Ok(None);
        }

        let path = ctx.output_dir.join("go.work");
        let Some(on_disk_raw) = read_go_directive_from_file(&path) else {
            return Ok(None);
        };
        let Some(on_disk) = parse_go_version(&on_disk_raw) else {
            return Ok(None);
        };
        let Some((required, required_raw, required_member)) =
            max_member_go_requirement(&repo_paths, ctx.workspace_root)
        else {
            return Ok(None);
        };

        if on_disk >= required {
            return Ok(None);
        }

        Ok(Some(MemberIncompatibility::new(
            self.name(),
            &path,
            "go",
            &on_disk_raw,
            &required_raw,
            &format!("{required_member}/go.mod"),
        )))
    }

    /// go.work is HYBRID — it lives in managed_files(), not generated_files().
    /// `generated_files()` is for fully-rwv-owned files
    /// (whole-deletable, gitignore-ok); `managed_files()` is for hybrid files
    /// that are symlinked but never gitignored or whole-deleted.
    /// `go.work`'s marked `use` block, and nothing else. `go.sum` is declared
    /// generated so it is surfaced, but the go tool writes it and rwv has never
    /// authored a byte of one.
    fn owned_paths_on_disk(&self, ctx: &IntegrationContext) -> Vec<OwnedPath> {
        if holds_owned_region::<GoWorkDoc>(&ctx.output_dir.join("go.work"), &Self::owned_keys()) {
            vec![OwnedPath::MarkedRegion("go.work".to_string())]
        } else {
            vec![]
        }
    }

    fn generated_files(&self, _ctx: &IntegrationContext) -> Vec<String> {
        // go.sum is still fully-generated (tool-managed), so it stays here.
        // go.work itself moves to managed_files().
        vec!["go.sum".to_string()]
    }

    fn managed_files(&self, ctx: &IntegrationContext) -> Vec<String> {
        if ctx.detect_repos_with_manifest("go.mod").is_empty() {
            return vec![];
        }
        vec!["go.work".to_string()]
    }
}

impl GoWork {
    /// The `go.work` keys rwv authors. `go` is deliberately absent: it is
    /// `Ownership::DefaultOnly`, so an operator's value must survive a strip.
    fn owned_keys() -> Vec<KeyPath> {
        vec![keypath(["use"])]
    }

    /// Remove rwv's `use` block from the `go.work` under `root`, leaving
    /// user-authored content untouched.
    ///
    /// Both callers reach this from the same premise — rwv has no `use` block to
    /// author — and they differ only in why: the project is going away, or its
    /// Go membership emptied. Marker-gated and idempotent, so it is safe over an
    /// absent file and over one the user holds the pen on.
    fn strip_managed_region(root: &Path) -> anyhow::Result<()> {
        strip_deactivate::<GoWorkDoc>(&root.join("go.work"), &Self::owned_keys())?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// PRIMARY: activate via `go work edit`
// ---------------------------------------------------------------------------

fn activate_via_go_tool(
    go_work_path: &Path,
    new_paths: &[impl AsRef<str>],
    go_version: Option<&str>,
    workspace_root: &Path,
) -> anyhow::Result<()> {
    use std::process::Command;

    // Strategy: run all `go work` commands from workspace_root without setting
    // GOWORK.  This makes `go` operate on a go.work at workspace_root (which
    // it finds by walking up, or creates fresh).  Paths stored in go.work are
    // then `./github/chatly/protocol` — relative to workspace_root.
    //
    // After the tool operations, we copy the workspace_root/go.work into
    // go_work_path (output_dir/go.work, the committed location) and remove
    // the workspace_root copy so the only canonical file is in output_dir.
    //
    // The symlink created by the framework (root/go.work →
    // projects/<project>/go.work) points to the output_dir copy.  When `go`
    // walks up from a repo dir and finds the symlink, it resolves paths from
    // the symlink's directory (workspace_root/), so `./github/...` paths work
    // correctly.
    let work_tmp = workspace_root.join("go.work");

    // Seed work_tmp from the existing output_dir copy (preserves user content).
    if !work_tmp.exists() && go_work_path.exists() {
        std::fs::copy(go_work_path, &work_tmp)?;
    }
    // If neither exists, `go work init` (below) will create work_tmp.

    // Which releases of the `go` tool stamp the toolchain they ran under into
    // a go.work they rewrite is not stable: 1.24 and earlier write the line,
    // 1.25 does not. Snapshot the operator's state before any `go work` runs,
    // so the restore below can tell a line they wrote from a witness of
    // whichever Go happens to be installed here.
    let seeded_toolchain = read_toolchain_directive_from_file(&work_tmp);

    // Ownership::DefaultOnly for the go-line: `go_version` is written only
    // when absent here, checked before `go work init` seeds its own
    // (toolchain-version) go-line into a fresh file.
    let go_line_absent = !std::fs::read_to_string(&work_tmp)
        .ok()
        .and_then(|text| GoWorkDoc::parse(&text).ok())
        .is_some_and(|doc| doc.key_present(&keypath(["go"])));

    // Initialize go.work at workspace_root if needed.
    if !work_tmp.exists() {
        let status = Command::new("go")
            .args(["work", "init"])
            .current_dir(workspace_root)
            .status()?;
        if !status.success() {
            anyhow::bail!("go work init failed");
        }
    }

    if let (Some(ver), true) = (go_version, go_line_absent) {
        let status = Command::new("go")
            .args(["work", "edit", &format!("-go={ver}")])
            .current_dir(workspace_root)
            .status()?;
        if !status.success() {
            anyhow::bail!("go work edit -go={ver} failed");
        }
    }

    // Ownership::DefaultOnly for the go-line, part 2. `go work use` raises the
    // go directive to the strongest requirement across the modules it adds, and
    // `go work init` stamps a fresh file with the version of whichever `go` ran
    // it — both write the slot the guard above was careful not to touch.
    // Snapshot what rwv is accountable for here, after the guarded `-go=` write
    // and before the first `use`, so one restore covers all three cases: the
    // operator's pre-existing value, a default just written into an absent slot,
    // and no line at all when rwv had no value for one. That last case is why
    // the snapshot is not simply whatever the file now says — init's version is
    // a property of this machine, and go.work is committed.
    let no_go_line_to_publish = go_line_absent && go_version.is_none();
    let settled_go = if no_go_line_to_publish {
        None
    } else {
        read_go_directive_from_file(&work_tmp)
    };

    // Read the current `use` entries so we can dropuse stale ones.
    let current_uses = read_current_uses_from_file(&work_tmp);

    // Add all new paths (relative to workspace_root).
    for p in new_paths {
        let use_path = format!("./{}", p.as_ref());
        let status = Command::new("go")
            .args(["work", "use", &use_path])
            .current_dir(workspace_root)
            .status()?;
        if !status.success() {
            // Clean up on failure.
            let _ = std::fs::remove_file(&work_tmp);
            anyhow::bail!("go work use {use_path} failed");
        }
    }

    // Drop entries no longer in new_paths.
    let new_set: std::collections::BTreeSet<String> = new_paths
        .iter()
        .map(|p| format!("./{}", p.as_ref()))
        .collect();
    for old in current_uses {
        if !new_set.contains(&old) {
            let status = Command::new("go")
                .args(["work", "edit", &format!("-dropuse={old}")])
                .current_dir(workspace_root)
                .status()?;
            if !status.success() {
                // Non-fatal: entry may already be gone.
                eprintln!("warning: go work edit -dropuse={old} failed (non-fatal)");
            }
        }
    }

    restore_go_directive(&work_tmp, settled_go.as_deref())?;

    restore_toolchain_directive(&work_tmp, seeded_toolchain.as_deref())?;

    // Inject the ownership marker above the use block in work_tmp.
    ensure_marker_present(&work_tmp)?;

    // Copy work_tmp → go_work_path only if they refer to different files on
    // disk. A path-string inequality is not sufficient: in the production
    // weave layout work_tmp (workspace_root/go.work) is a symlink to
    // go_work_path (projects/<project>/go.work), so the path strings differ
    // but both resolve to the same inode. fs::copy on a symlink-to-self
    // truncates the file. Canonicalize both sides and skip the
    // copy when they resolve to the same path.
    let same_file = match (
        std::fs::canonicalize(&work_tmp),
        std::fs::canonicalize(go_work_path),
    ) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    };
    if !same_file {
        if let Some(parent) = go_work_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(&work_tmp, go_work_path)?;
        // Remove the workspace_root copy (it will be created as a symlink by
        // the framework once activate() returns).
        let _ = std::fs::remove_file(&work_tmp);
    }
    // When they resolve to the same file (production layout with a symlink,
    // or unit tests where output_dir == workspace_root), the file is already
    // in the right place — no copy or remove needed.

    Ok(())
}

/// Read the current `use` paths from go.work using [`GoWorkDoc::current_uses`].
fn read_current_uses_from_file(path: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return vec![];
    };
    GoWorkDoc::parse(&text)
        .map(|doc| doc.current_uses())
        .unwrap_or_default()
}

/// Put the go directive rwv is accountable for back into `path`, undoing what
/// `go work init` and `go work use` write into that slot as side effects of
/// creating the file and adding a module. `None` means the file carried no
/// go-line before any `go work` ran and rwv had no value to supply, so it
/// carries none afterwards either — the FALLBACK path's output for those inputs.
///
/// No-op when the on-disk state already matches, which is the common case —
/// the tool only ever raises, and only when a member declares more than the
/// file does.
///
/// A value is written through [`GoWorkDoc`] rather than a third `go work edit
/// -go=<v>` call for two reasons: the tool would re-render the whole file, and
/// on a pin above the installed toolchain any `go work` invocation tries to
/// download that toolchain. This shares the writer the FALLBACK path uses, so a
/// given go-line lands identically whichever path produced it. Removal is
/// line-level instead, because [`GoWorkDoc`] models `go` and `use` and
/// round-trips the rest as opaque text — it cannot see the blank line the tool
/// pads the directive with.
fn restore_go_directive(path: &Path, want: Option<&str>) -> anyhow::Result<()> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading {} to restore its go directive", path.display()))?;
    if go_directive_in(&text).as_deref() == want {
        return Ok(());
    }

    let out = match want {
        Some(v) => {
            let mut doc = GoWorkDoc::parse(&text).with_context(|| {
                format!("parsing {} to restore its go directive", path.display())
            })?;
            doc.set_owned(&keypath(["go"]), &OwnedValue::String(v.to_string()));
            doc.serialize()?
        }
        None => {
            let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
            let Some(at) = lines.iter().position(|l| go_directive_in(l).is_some()) else {
                return Ok(());
            };
            remove_directive_line(&mut lines, at);
            let mut joined = lines.join("\n");
            if text.ends_with('\n') && !joined.is_empty() {
                joined.push('\n');
            }
            joined
        }
    };

    std::fs::write(path, out)
        .with_context(|| format!("writing {} to restore its go directive", path.display()))?;
    Ok(())
}

/// Remove the line at `at`, along with a blank line immediately after it — the
/// padding `go work` writes between directives, which would otherwise outlive
/// the directive it was separating.
fn remove_directive_line(lines: &mut Vec<String>, at: usize) {
    if lines.get(at + 1).is_some_and(|l| l.trim().is_empty()) {
        lines.remove(at + 1);
    }
    lines.remove(at);
}

/// Force the `toolchain` directive of `path` back to `want`, where `None` means
/// the file carried no toolchain line before any `go work` command ran.
///
/// A `go` tool old enough to stamp the toolchain it executed under leaves that
/// line behind in the committed file, which makes activate's output a function
/// of the Go installed on this machine — the fallback path never writes one,
/// and neither does a newer tool. Only a line the operator wrote survives.
///
/// Line-level rather than through [`GoWorkDoc`], which models `go` and `use`
/// and round-trips everything else as opaque text.
fn restore_toolchain_directive(path: &Path, want: Option<&str>) -> anyhow::Result<()> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading {} to restore its toolchain", path.display()))?;
    if toolchain_directive_in(&text).as_deref() == want {
        return Ok(());
    }

    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let at = lines
        .iter()
        .position(|l| l.trim().starts_with("toolchain "));
    match (at, want) {
        // Rewrite in place, so a line the operator put somewhere deliberate
        // keeps its position among the other directives.
        (Some(i), Some(v)) => lines[i] = format!("toolchain {v}"),
        (Some(i), None) => remove_directive_line(&mut lines, i),
        (None, Some(v)) => {
            let after_go = lines
                .iter()
                .position(|l| l.trim().starts_with("go "))
                .map_or(0, |i| i + 1);
            lines.insert(after_go, String::new());
            lines.insert(after_go + 1, format!("toolchain {v}"));
        }
        // Equal states returned above.
        (None, None) => return Ok(()),
    }

    let mut out = lines.join("\n");
    if text.ends_with('\n') {
        out.push('\n');
    }
    std::fs::write(path, out)
        .with_context(|| format!("writing {} to restore its toolchain", path.display()))?;
    Ok(())
}

/// Ensure the ownership marker is present immediately above the `use (…)`
/// block in go.work (post-tool injection).
fn ensure_marker_present(path: &Path) -> anyhow::Result<()> {
    let text = std::fs::read_to_string(path)?;
    let mut doc = GoWorkDoc::parse(&text)?;
    doc.ensure_marker();
    std::fs::write(path, doc.serialize()?)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// FALLBACK: activate via GoWorkDoc hand-edit
// ---------------------------------------------------------------------------

fn activate_via_hand_edit(
    go_work_path: &Path,
    new_paths: &[impl AsRef<str>],
    go_version_default: Option<&str>,
) -> anyhow::Result<()> {
    // Build owned keys.
    // ["use"] is always Author-owned.
    // ["go"] is DefaultOnly — rwv provides a default (from config or
    // max_go_version) but never overwrites an existing go-line. This
    // preserves "go 1.26" in an existing file even when config.go_version
    // is None (fixing the C11 downgrade bug without a is_some() guard).
    let use_items: Vec<String> = new_paths
        .iter()
        .map(|p| format!("./{}", p.as_ref()))
        .collect();

    let mut owned: Vec<(Vec<String>, Ownership, OwnedValue)> = vec![(
        keypath(["use"]),
        Ownership::Author,
        OwnedValue::sorted_array(use_items),
    )];

    if let Some(ver) = go_version_default {
        owned.push((
            keypath(["go"]),
            Ownership::DefaultOnly,
            OwnedValue::String(ver.to_string()),
        ));
    }

    merge_activate::<GoWorkDoc>(go_work_path, &owned)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Go-version reading and comparison
//
// One parser feeds three consumers: the DefaultOnly seed value
// (`max_go_version`), the incompatibility predicate's requirement side
// (`max_member_go_requirement`), and its on-disk side
// (`read_go_directive_from_file`). Ordering is on the parsed tuple, never on
// the string — `1.9` is BELOW `1.21`, which a lexicographic compare gets
// backwards.
// ---------------------------------------------------------------------------

/// A go language version as `(major, minor, patch)`, with an absent patch
/// component read as `0` (`1.26` and `1.26.0` are the same requirement).
type GoVersion = (u64, u64, u64);

/// Parse a go-directive value (`1.26`, `1.26.3`) into comparable components.
/// Returns `None` for anything without at least `<major>.<minor>` numerics —
/// an unparseable directive is simply not a fact this module states anything
/// about.
fn parse_go_version(raw: &str) -> Option<GoVersion> {
    let parts: Vec<&str> = raw.trim().splitn(3, '.').collect();
    if parts.len() < 2 {
        return None;
    }
    let maj = parts[0].parse::<u64>().ok()?;
    let min = parts[1].parse::<u64>().ok()?;
    // Third component is optional and may carry a pre-release suffix
    // (`1.26.0-rc1`); take the leading digits and ignore the rest.
    let patch = parts
        .get(2)
        .map(|p| {
            let digits: String = p.chars().take_while(|c| c.is_ascii_digit()).collect();
            digits.parse::<u64>().unwrap_or(0)
        })
        .unwrap_or(0);
    Some((maj, min, patch))
}

/// Read the `go <version>` directive value from a go.work / go.mod file's text.
///
/// Uses the same line rule as [`GoWorkDoc`]'s go-line locator (`go ` followed
/// by a digit), so the reader and the writer agree on what the go-line is.
/// Returns the raw value token, trailing comments stripped.
fn go_directive_in(text: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let trimmed = line.trim();
        if trimmed.starts_with("go ") && trimmed.chars().nth(3).is_some_and(|c| c.is_ascii_digit())
        {
            trimmed[3..].split_whitespace().next().map(str::to_string)
        } else {
            None
        }
    })
}

/// Read the go-directive value from `path`. `None` when the file is absent,
/// unreadable, or carries no go-line.
fn read_go_directive_from_file(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    go_directive_in(&text)
}

/// The `toolchain` directive's value (`go1.24.13`), if `text` carries one.
fn toolchain_directive_in(text: &str) -> Option<String> {
    text.lines().find_map(|line| {
        line.trim()
            .strip_prefix("toolchain ")
            .map(|v| v.trim().to_string())
    })
}

/// Read the toolchain-directive value from `path`. `None` when the file is
/// absent, unreadable, or carries no toolchain line.
fn read_toolchain_directive_from_file(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    toolchain_directive_in(&text)
}

/// The strongest go version the members require: `(parsed, raw value, member
/// repo path)`. Ties keep the first member in `paths` order (the detection list
/// is sorted, so the result is deterministic).
fn max_member_go_requirement(
    paths: &[impl AsRef<str>],
    workspace_root: &Path,
) -> Option<(GoVersion, String, String)> {
    let mut max: Option<(GoVersion, String, String)> = None;
    for p in paths {
        let go_mod = workspace_root.join(p.as_ref()).join("go.mod");
        let Ok(content) = std::fs::read_to_string(go_mod) else {
            continue;
        };
        let Some(raw) = go_directive_in(&content) else {
            continue;
        };
        let Some(parsed) = parse_go_version(&raw) else {
            continue;
        };
        if max.as_ref().is_none_or(|(m, _, _)| parsed > *m) {
            max = Some((parsed, raw, p.as_ref().to_string()));
        }
    }
    max
}

// ---------------------------------------------------------------------------
// max_go_version — read go <version> from each go.mod, return the maximum.
// Used only when config.go_version is None and we need a version for the
// `go work edit -go=<v>` primary path call.
// ---------------------------------------------------------------------------

fn max_go_version(paths: &[impl AsRef<str>], workspace_root: &Path) -> Option<String> {
    max_member_go_requirement(paths, workspace_root)
        .map(|((maj, min, _), _, _)| format!("{maj}.{min}"))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{Manifest, ProjectName, Role};
    use crate::workspace::ContainerKind;
    use std::collections::HashMap;
    use tempfile::TempDir;

    // Force the hand-parse fallback for all tests in this module.
    // This is idempotent and deterministic: each test body calls this before
    // exercising the integration.  go_on_path() reads the thread-local.
    fn force_fallback() {
        FORCE_GOWORK_FALLBACK.with(|f| f.set(true));
    }

    fn make_manifest_local(repos: Vec<(&str, Role)>) -> Manifest {
        let mut manifest_toml = String::from("[repositories]\n");
        for (path, role) in &repos {
            let last = path.split('/').next_back().unwrap();
            manifest_toml.push_str(&format!(
                "[repositories.\"{path}\"]\ntype = \"git\"\nurl = \"https://github.com/test/{last}.git\"\nversion = \"main\"\nrole = \"{}\"\n",
                role.as_str()
            ));
        }
        Manifest::from_toml_str(&manifest_toml).unwrap()
    }

    fn make_ctx_local<'a>(
        root: &'a Path,
        project: &'a ProjectName,
        manifest: &'a Manifest,
        config: &'a IntegrationConfig,
        cache: &'a HashMap<String, Vec<String>>,
    ) -> IntegrationContext<'a> {
        IntegrationContext {
            output_dir: root,
            workspace_root: root,
            container_kind: ContainerKind::Primary,
            project,
            repos: manifest
                .iter_entries()
                .map(|(rp, e)| (rp.clone(), e.clone()))
                .collect(),
            config,
            all_repos_on_disk: &[],
            all_project_paths: &[],
            detection_cache: cache,
            workweave: None,
        }
    }

    fn touch(root: &Path, rel: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "").unwrap();
    }

    fn write_file(root: &Path, rel: &str, content: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    use crate::manifest::IntegrationConfig;

    // -----------------------------------------------------------------------
    // Scenario 1: Adding a repo preserves a hand-authored `replace` + comment;
    //             `go 1.26` UNCHANGED (config None).
    // -----------------------------------------------------------------------

    #[test]
    fn scenario1_adding_repo_preserves_replace_and_go_version() {
        force_fallback();

        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // Pre-existing go.work with replace directive (no marker — no use block yet).
        let seed = "go 1.26\n\n// pin local fork of legacy\nreplace example.com/legacy => ./vendor/legacy\n";
        write_file(root, "go.work", seed);

        // Two repos with go.mod files.
        touch(root, "github/test/repoweave/go.mod");
        touch(root, "github/test/some-go-tool/go.mod");

        let manifest = make_manifest_local(vec![
            ("github/test/repoweave", Role::Owned),
            ("github/test/some-go-tool", Role::Owned),
        ]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default(); // go_version = None
        let cache = HashMap::new();
        let ctx = make_ctx_local(root, &project, &manifest, &config, &cache);

        let integration = GoWork;
        integration.activate(&ctx).unwrap();

        let text = std::fs::read_to_string(root.join("go.work")).unwrap();

        // use block has both repos.
        assert!(
            text.contains("./github/test/repoweave"),
            "use entry missing: {text}"
        );
        assert!(
            text.contains("./github/test/some-go-tool"),
            "use entry missing: {text}"
        );

        // go 1.26 UNCHANGED (config None → no go-line write in fallback).
        assert!(
            text.contains("go 1.26"),
            "go 1.26 must be preserved: {text}"
        );
        assert!(
            !text.contains("go 1.21"),
            "must not downgrade to 1.21: {text}"
        );

        // replace block and comment survive.
        assert!(
            text.contains("replace example.com/legacy"),
            "replace must survive: {text}"
        );
        assert!(
            text.contains("// pin local fork"),
            "comment must survive: {text}"
        );

        // Ownership marker is present.
        assert!(
            text.contains("// managed by repoweave"),
            "marker must be present: {text}"
        );
    }

    // -----------------------------------------------------------------------
    // Scenario 2: Removing a repo strips its use entry; toolchain + godebug +
    //             `go 1.26` survive.
    // -----------------------------------------------------------------------

    #[test]
    fn scenario2_removing_repo_keeps_toolchain_and_godebug() {
        force_fallback();

        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // Pre-existing go.work with three entries under the marker.
        let seed = concat!(
            "go 1.26\n\n",
            "toolchain go1.26.0\n\n",
            "godebug default=go1.26\n\n",
            "// managed by repoweave\n",
            "use (\n",
            "\t./github/test/repoweave\n",
            "\t./github/test/some-go-tool\n",
            "\t./github/test/another-module\n",
            ")\n"
        );
        write_file(root, "go.work", seed);

        // Only two repos remain in the manifest (another-module removed).
        touch(root, "github/test/repoweave/go.mod");
        touch(root, "github/test/some-go-tool/go.mod");

        let manifest = make_manifest_local(vec![
            ("github/test/repoweave", Role::Owned),
            ("github/test/some-go-tool", Role::Owned),
        ]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx_local(root, &project, &manifest, &config, &cache);

        let integration = GoWork;
        integration.activate(&ctx).unwrap();

        let text = std::fs::read_to_string(root.join("go.work")).unwrap();

        // Removed entry is gone.
        assert!(
            !text.contains("another-module"),
            "removed entry must be gone: {text}"
        );

        // Remaining entries present.
        assert!(
            text.contains("./github/test/repoweave"),
            "repoweave must remain: {text}"
        );
        assert!(
            text.contains("./github/test/some-go-tool"),
            "some-go-tool must remain: {text}"
        );

        // toolchain, godebug, go 1.26 survive.
        assert!(
            text.contains("toolchain go1.26.0"),
            "toolchain must survive: {text}"
        );
        assert!(
            text.contains("godebug default=go1.26"),
            "godebug must survive: {text}"
        );
        assert!(text.contains("go 1.26"), "go 1.26 must survive: {text}");
    }

    // -----------------------------------------------------------------------
    // Scenario 3: Deactivate strips the use set but keeps replace + go 1.26.
    // -----------------------------------------------------------------------

    #[test]
    fn scenario3_deactivate_strips_use_keeps_replace() {
        force_fallback();

        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        let seed = concat!(
            "go 1.26\n\n",
            "// managed by repoweave\n",
            "use (\n",
            "\t./github/test/repoweave\n",
            "\t./github/test/some-go-tool\n",
            ")\n\n",
            "replace example.com/foo => ../foo\n"
        );
        write_file(root, "go.work", seed);

        let integration = GoWork;
        integration.deactivate(root).unwrap();

        // File still exists — replace + go line are user content.
        assert!(
            root.join("go.work").exists(),
            "file must survive (user content present)"
        );

        let text = std::fs::read_to_string(root.join("go.work")).unwrap();

        // use block gone.
        assert!(
            !text.contains("./github/test/repoweave"),
            "use entry must be stripped: {text}"
        );
        assert!(
            !text.contains("use ("),
            "use block must be stripped: {text}"
        );

        // Marker gone.
        assert!(
            !text.contains("// managed by repoweave"),
            "marker must be stripped: {text}"
        );

        // go 1.26 and replace survive.
        assert!(text.contains("go 1.26"), "go 1.26 must survive: {text}");
        assert!(
            text.contains("replace example.com/foo"),
            "replace must survive: {text}"
        );
    }

    // -----------------------------------------------------------------------
    // Scenario 4: Deactivate deletes when only rwv content remained.
    // -----------------------------------------------------------------------

    #[test]
    fn scenario4_deactivate_deletes_when_only_rwv_content() {
        force_fallback();

        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // go 1.26 + use block only (no replace/godebug/toolchain).
        // After stripping use, the remaining content is just `go 1.26` which
        // GoWorkDoc::is_empty() considers "empty enough" (only go/toolchain/
        // blank/comment lines remain).
        let seed = concat!(
            "go 1.26\n\n",
            "// managed by repoweave\n",
            "use (\n",
            "\t./github/test/repoweave\n",
            ")\n"
        );
        write_file(root, "go.work", seed);

        let integration = GoWork;
        integration.deactivate(root).unwrap();

        assert!(
            !root.join("go.work").exists(),
            "file must be deleted (delete-if-empty)"
        );
    }

    // -----------------------------------------------------------------------
    // Guard: deactivate with no marker is a no-op (user holds the pen).
    // -----------------------------------------------------------------------

    #[test]
    fn deactivate_no_marker_is_noop() {
        force_fallback();

        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // Hand-written go.work with no marker.
        let seed = "go 1.26\n\nuse (\n\t./mine\n)\n";
        write_file(root, "go.work", seed);

        let integration = GoWork;
        integration.deactivate(root).unwrap();

        // File untouched.
        assert!(
            root.join("go.work").exists(),
            "hand-owned file must survive"
        );
        let text = std::fs::read_to_string(root.join("go.work")).unwrap();
        assert!(
            text.contains("./mine"),
            "user use entry must survive: {text}"
        );
    }

    // -----------------------------------------------------------------------
    // USER-HELD: activate() must not clobber a user-held go.work;
    //            verify() must surface it as a structured Issue (safe_to_fix=false).
    // -----------------------------------------------------------------------

    #[test]
    fn user_held_activate_noop_and_verify_returns_issue() {
        force_fallback();

        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // Hand-written go.work: use block present but NO managed-by marker.
        let seed = "go 1.26\n\nuse (\n\t./mine\n)\n";
        write_file(root, "go.work", seed);

        // A repo with a go.mod so activate() and verify() detect repos.
        touch(root, "github/test/repoweave/go.mod");

        let manifest = make_manifest_local(vec![("github/test/repoweave", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx_local(root, &project, &manifest, &config, &cache);

        let integration = GoWork;

        // activate() must return Ok(()) without touching the file.
        integration.activate(&ctx).unwrap();
        let text_after = std::fs::read_to_string(root.join("go.work")).unwrap();
        assert_eq!(
            text_after, seed,
            "activate() must not mutate a user-held go.work"
        );

        // verify() must return a single USER-HELD Issue with safe_to_fix=false.
        let issues = integration.verify(&ctx).unwrap();
        assert_eq!(
            issues.len(),
            1,
            "expected exactly one USER-HELD issue: {issues:?}"
        );
        let issue = &issues[0];
        assert!(
            !issue.safe_to_fix,
            "USER-HELD issue must have safe_to_fix=false: {issue:?}"
        );
        assert!(
            issue.message.contains("unmarked") || issue.message.contains("user content"),
            "issue message should describe the USER-HELD condition: {}",
            issue.message
        );
    }

    // -----------------------------------------------------------------------
    // Guard: go_version in config writes the go line in fallback.
    // -----------------------------------------------------------------------

    #[test]
    fn go_version_config_writes_go_line_in_fallback() {
        force_fallback();

        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // No existing go.work.
        touch(root, "github/test/repoweave/go.mod");

        let manifest = make_manifest_local(vec![("github/test/repoweave", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        // Set go_version explicitly in config.
        let config = IntegrationConfig::from_toml("go-version = \"1.23\"");
        let cache = HashMap::new();
        let ctx = make_ctx_local(root, &project, &manifest, &config, &cache);

        let integration = GoWork;
        integration.activate(&ctx).unwrap();

        let text = std::fs::read_to_string(root.join("go.work")).unwrap();
        assert!(
            text.contains("go 1.23"),
            "config go-version must be written: {text}"
        );
    }

    // -----------------------------------------------------------------------
    // Regression — no-downgrade (C11 scenario):
    //   existing go.work has `go 1.26`, config.go_version is None,
    //   but max_go_version computes 1.24 from member go.mod files.
    //   After activate, `go 1.26` must be preserved (DefaultOnly never
    //   overwrites an existing value).
    // -----------------------------------------------------------------------

    #[test]
    fn regression_no_downgrade_defaultonly_preserves_existing_go_line() {
        force_fallback();

        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // Existing go.work with go 1.26.
        let seed = concat!(
            "go 1.26\n\n",
            "// managed by repoweave\n",
            "use (\n",
            "\t./github/test/repoweave\n",
            ")\n"
        );
        write_file(root, "go.work", seed);

        // Member go.mod with go 1.24 — lower than the existing 1.26.
        write_file(
            root,
            "github/test/repoweave/go.mod",
            "module example.com/repoweave\n\ngo 1.24\n",
        );

        let manifest = make_manifest_local(vec![("github/test/repoweave", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default(); // go_version = None
        let cache = HashMap::new();
        let ctx = make_ctx_local(root, &project, &manifest, &config, &cache);

        let integration = GoWork;
        integration.activate(&ctx).unwrap();

        let text = std::fs::read_to_string(root.join("go.work")).unwrap();

        // go 1.26 must be preserved — DefaultOnly never overwrites.
        assert!(
            text.contains("go 1.26"),
            "go 1.26 must be preserved (DefaultOnly): {text}"
        );
        assert!(
            !text.contains("go 1.24"),
            "must not downgrade to 1.24 (from max_go_version): {text}"
        );
        assert!(
            !text.contains("go 1.21"),
            "must not downgrade to 1.21: {text}"
        );
    }

    // -----------------------------------------------------------------------
    // PRIMARY-path tests.
    //
    // The tests above force the FALLBACK. These deliberately do not, so they
    // run `go work` for real — the only way to reach the code under test.
    // -----------------------------------------------------------------------

    /// Whether the real `go` binary is available, printing a skip reason when
    /// it is not.
    ///
    /// The PRIMARY path *is* `go work`; there is nothing to exercise without
    /// the tool, and a stub `go` on PATH would pin the stub's behaviour rather
    /// than the side effect these tests exist to catch. A machine without `go`
    /// takes the FALLBACK path, which the tests above pin in full — what goes
    /// unproven there is not rwv's logic but the premise that the tool clobbers
    /// the go-line, which is exactly the part that cannot be observed without
    /// it.
    fn require_go() -> bool {
        // Reads the same `go_on_path()` the integration dispatches on, so a
        // test that skips here is exactly a test whose activate would have
        // taken the fallback anyway.
        if go_on_path() {
            return true;
        }
        eprintln!("skipping test: `go` is not on PATH, so the PRIMARY path is unreachable");
        false
    }

    /// A go.work pin and a member requirement above it, chosen so that no
    /// invocation in these tests can reach the network.
    ///
    /// `go work` consults GOTOOLCHAIN and downloads a toolchain whenever a
    /// version above the installed one is demanded. Both constants sit at or
    /// below every `go` that could do that: 1.21 is the oldest release with the
    /// toolchain machinery at all, so `installed >= 1.21 >= MEMBER_GO` holds
    /// for anything able to switch, and anything older has no switch to make.
    /// Deliberately not derived from `go env GOVERSION` — the pair must not
    /// move from machine to machine, or a failure reads differently everywhere.
    const PINNED_GO: &str = "1.20";
    const MEMBER_GO: &str = "1.21";

    /// Fixture shared by the primary-path tests: an existing managed go.work
    /// pinned at `PINNED_GO`, one member declaring the higher `MEMBER_GO`.
    /// `go work use` raises the pin to the member's requirement unless rwv puts
    /// it back.
    fn seed_pin_below_member(root: &Path) {
        write_file(
            root,
            "go.work",
            &format!(
                "go {PINNED_GO}\n\n// managed by repoweave\nuse (\n\t./github/test/repoweave\n)\n"
            ),
        );
        write_file(
            root,
            "github/test/repoweave/go.mod",
            &format!("module example.com/repoweave\n\ngo {MEMBER_GO}\n"),
        );
    }

    fn activate_in(root: &Path) {
        let manifest = make_manifest_local(vec![("github/test/repoweave", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default(); // go_version = None
        let cache = HashMap::new();
        let ctx = make_ctx_local(root, &project, &manifest, &config, &cache);
        GoWork.activate(&ctx).unwrap();
    }

    // -----------------------------------------------------------------------
    // Twin of regression_no_downgrade_defaultonly_preserves_existing_go_line,
    // on the tool path: `go work use` raises the go directive to the member's
    // requirement as a side effect of adding the module. DefaultOnly says an
    // existing go-line is preserved unconditionally — on BOTH paths.
    // -----------------------------------------------------------------------

    #[test]
    fn regression_go_tool_path_preserves_existing_go_line() {
        if !require_go() {
            return;
        }

        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        seed_pin_below_member(root);

        activate_in(root);

        let text = std::fs::read_to_string(root.join("go.work")).unwrap();
        assert!(
            text.contains(&format!("go {PINNED_GO}")),
            "go {PINNED_GO} must survive `go work use` (DefaultOnly): {text}"
        );
        assert!(
            !text.contains(&format!("go {MEMBER_GO}")),
            "the member's requirement must not be promoted into the go-line: {text}"
        );
        // The member is still in the workspace — the restore must not have
        // undone the `use` edit the tool was called for.
        assert!(
            text.contains("./github/test/repoweave"),
            "use entry must be present: {text}"
        );
        assert!(
            text.contains("// managed by repoweave"),
            "marker must be present: {text}"
        );
    }

    // -----------------------------------------------------------------------
    // The property the twin above is an instance of: which path ran is not
    // observable in the output. Least-surprise is a global property, so the
    // same inputs must produce the same file whether or not `go` is installed.
    // -----------------------------------------------------------------------

    #[test]
    fn primary_and_fallback_agree_byte_for_byte() {
        if !require_go() {
            return;
        }

        // Order is load-bearing: `force_fallback()` latches a thread-local for
        // the rest of the test, so every tool run has to come first.
        let tool_pinned = activate_fresh(seed_pin_below_member);
        let tool_unseeded = activate_fresh(seed_member_without_go_directive);

        force_fallback();

        let hand_pinned = activate_fresh(seed_pin_below_member);
        let hand_unseeded = activate_fresh(seed_member_without_go_directive);

        assert_eq!(
            tool_pinned, hand_pinned,
            "activate must not depend on whether `go` is installed\n\
             --- primary (go work) ---\n{tool_pinned}\n\
             --- fallback (hand-edit) ---\n{hand_pinned}"
        );
        assert_eq!(
            tool_unseeded, hand_unseeded,
            "with no go-version to supply, activate must not depend on whether \
             `go` is installed\n\
             --- primary (go work) ---\n{tool_unseeded}\n\
             --- fallback (hand-edit) ---\n{hand_unseeded}"
        );
    }

    /// Seed a fresh temp root, activate in it, and return the resulting go.work.
    fn activate_fresh(seed: impl Fn(&Path)) -> String {
        let tmp = TempDir::new().unwrap();
        seed(tmp.path());
        activate_in(tmp.path());
        std::fs::read_to_string(tmp.path().join("go.work")).unwrap()
    }

    /// Greenfield, and a member whose go.mod carries no `go` directive — legal
    /// Go, which the toolchain reads as `go 1.16`. `max_go_version` has nothing
    /// to return, so rwv has no go-version to supply, and `go work init` seeds
    /// the fresh file with the version of whichever `go` ran it.
    fn seed_member_without_go_directive(root: &Path) {
        write_file(
            root,
            "github/test/repoweave/go.mod",
            "module example.com/repoweave\n",
        );
    }

    // -----------------------------------------------------------------------
    // The absence below is the behaviour, not an oversight. `go work init`
    // stamps the running toolchain's version into a fresh go.work, which is a
    // property of the machine rather than of the workspace, and go.work is
    // committed — publishing it would hand whoever activated first a pin that
    // DefaultOnly then preserves forever. With no value of its own to write,
    // rwv leaves the slot empty; a go.work with no go directive is one the go
    // tool accepts and builds.
    // -----------------------------------------------------------------------

    #[test]
    fn greenfield_without_go_version_writes_no_go_line() {
        if !require_go() {
            return;
        }

        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        seed_member_without_go_directive(root);

        activate_in(root);

        let text = std::fs::read_to_string(root.join("go.work")).unwrap();
        assert!(
            go_directive_in(&text).is_none(),
            "`go work init` seeds the running toolchain's version; rwv must not \
             publish it as a go-line it has no value for: {text}"
        );
        assert!(
            text.contains("./github/test/repoweave"),
            "use entry must be present: {text}"
        );
        assert!(
            text.contains("// managed by repoweave"),
            "marker must be present: {text}"
        );
    }

    /// The same absence on the path that has always produced it, so the
    /// property is pinned on a machine with no `go` to run the test above.
    #[test]
    fn greenfield_without_go_version_writes_no_go_line_in_fallback() {
        force_fallback();

        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        seed_member_without_go_directive(root);

        activate_in(root);

        let text = std::fs::read_to_string(root.join("go.work")).unwrap();
        assert!(
            go_directive_in(&text).is_none(),
            "rwv must not invent a go-line it has no value for: {text}"
        );
        assert!(
            text.contains("./github/test/repoweave"),
            "use entry must be present: {text}"
        );
    }

    // -----------------------------------------------------------------------
    // The other half of the same property: a toolchain line the operator wrote
    // is theirs. A `go` tool old enough to stamp its own overwrites it with the
    // installed version, so this discriminates only on such a tool — a newer
    // one leaves the line alone and the assertion holds without the restore
    // having done anything. `go{MEMBER_GO}.0` inherits the no-download argument
    // the constants above carry.
    // -----------------------------------------------------------------------

    #[test]
    fn regression_go_tool_path_preserves_existing_toolchain_line() {
        if !require_go() {
            return;
        }

        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        seed_pin_below_member(root);
        write_file(
            root,
            "go.work",
            &format!(
                "go {PINNED_GO}\n\ntoolchain go{MEMBER_GO}.0\n\n\
                 // managed by repoweave\nuse (\n\t./github/test/repoweave\n)\n"
            ),
        );

        activate_in(root);

        let text = std::fs::read_to_string(root.join("go.work")).unwrap();
        assert!(
            text.contains(&format!("toolchain go{MEMBER_GO}.0")),
            "the operator's toolchain line must survive activate: {text}"
        );
    }

    // -----------------------------------------------------------------------
    // Greenfield: no existing go.work, max_go_version detects 1.22 from
    //   member go.mod. The go-line should be written at greenfield.
    // -----------------------------------------------------------------------

    #[test]
    fn greenfield_go_line_written_from_max_go_version() {
        force_fallback();

        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // No existing go.work. Member go.mod reports go 1.22.
        write_file(
            root,
            "github/test/repoweave/go.mod",
            "module example.com/repoweave\n\ngo 1.22\n",
        );

        let manifest = make_manifest_local(vec![("github/test/repoweave", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default(); // go_version = None
        let cache = HashMap::new();
        let ctx = make_ctx_local(root, &project, &manifest, &config, &cache);

        let integration = GoWork;
        integration.activate(&ctx).unwrap();

        let text = std::fs::read_to_string(root.join("go.work")).unwrap();

        // go 1.22 written from max_go_version (DefaultOnly on a missing key).
        assert!(
            text.contains("go 1.22"),
            "go 1.22 must be written at greenfield from max_go_version: {text}"
        );
    }

    // -----------------------------------------------------------------------
    // Existing file with marker but no go-line: DefaultOnly should write the
    //   default version into the missing slot.
    // -----------------------------------------------------------------------

    #[test]
    fn existing_without_go_line_defaultonly_writes_default() {
        force_fallback();

        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // go.work with marker + use block but NO go-line.
        let seed = concat!(
            "// managed by repoweave\n",
            "use (\n",
            "\t./github/test/repoweave\n",
            ")\n"
        );
        write_file(root, "go.work", seed);

        // Member go.mod reports go 1.23.
        write_file(
            root,
            "github/test/repoweave/go.mod",
            "module example.com/repoweave\n\ngo 1.23\n",
        );

        let manifest = make_manifest_local(vec![("github/test/repoweave", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default(); // go_version = None
        let cache = HashMap::new();
        let ctx = make_ctx_local(root, &project, &manifest, &config, &cache);

        let integration = GoWork;
        integration.activate(&ctx).unwrap();

        let text = std::fs::read_to_string(root.join("go.work")).unwrap();

        // go 1.23 must be written — DefaultOnly fills an absent key.
        assert!(
            text.contains("go 1.23"),
            "go 1.23 must be written into missing go-line slot (DefaultOnly): {text}"
        );
    }

    // -----------------------------------------------------------------------
    // Guard: go-line drift (DefaultOnly) is CLEAN, mirroring cargo's
    //   resolver_default_only_drift_is_clean. The on-disk go-line is far
    //   below what the member go.mod requires; verify() must not report
    //   that as drift.
    // -----------------------------------------------------------------------

    #[test]
    fn go_directive_default_only_drift_is_clean() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        let seed = concat!(
            "go 1.21\n\n",
            "// managed by repoweave\n",
            "use (\n",
            "\t./github/test/repoweave\n",
            ")\n"
        );
        write_file(root, "go.work", seed);

        write_file(
            root,
            "github/test/repoweave/go.mod",
            "module example.com/repoweave\n\ngo 1.26\n",
        );

        let manifest = make_manifest_local(vec![("github/test/repoweave", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx_local(root, &project, &manifest, &config, &cache);

        let issues = GoWork.verify(&ctx).unwrap();
        assert!(
            issues.is_empty(),
            "go-line drift (DefaultOnly) must be CLEAN — got: {issues:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Guard: managed_files() returns go.work; generated_files() returns go.sum.
    // -----------------------------------------------------------------------

    #[test]
    fn managed_files_split() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        touch(root, "github/test/repoweave/go.mod");

        let manifest = make_manifest_local(vec![("github/test/repoweave", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx_local(root, &project, &manifest, &config, &cache);

        let integration = GoWork;
        let gen = integration.generated_files(&ctx);
        let man = integration.managed_files(&ctx);

        assert!(
            !gen.contains(&"go.work".to_string()),
            "go.work must not be in generated_files"
        );
        assert!(
            gen.contains(&"go.sum".to_string()),
            "go.sum must be in generated_files"
        );
        assert!(
            man.contains(&"go.work".to_string()),
            "go.work must be in managed_files"
        );
    }

    // -----------------------------------------------------------------------
    // member-incompatibility: the go-line is below what the members require.
    //   Distinct from drift — rule 5 keeps DefaultOnly divergence CLEAN, and
    //   this coexists with that.
    // -----------------------------------------------------------------------

    /// Build a workspace whose go.work carries `go_work_version` (or no
    /// go-line at all when `None`) and whose single member's go.mod declares
    /// `member_version`.
    fn seed_go_workspace(root: &Path, go_work_version: Option<&str>, member_version: &str) {
        let seed = match go_work_version {
            Some(v) => {
                format!("go {v}\n\n// managed by repoweave\nuse (\n\t./github/test/repoweave\n)\n")
            }
            None => "// managed by repoweave\nuse (\n\t./github/test/repoweave\n)\n".to_string(),
        };
        write_file(root, "go.work", &seed);
        write_file(
            root,
            "github/test/repoweave/go.mod",
            &format!("module example.com/repoweave\n\ngo {member_version}\n"),
        );
    }

    fn member_incompatibility_for(
        root: &Path,
        manifest: &Manifest,
        project: &ProjectName,
        config: &IntegrationConfig,
        cache: &HashMap<String, Vec<String>>,
    ) -> Option<MemberIncompatibility> {
        let ctx = make_ctx_local(root, project, manifest, config, cache);
        GoWork.member_incompatibility(&ctx).unwrap()
    }

    /// Build a workspace whose go.work carries `go_work_version` and whose
    /// members — one go.mod per `(repo path, go version)` — declare the given
    /// requirements. The multi-member counterpart of [`seed_go_workspace`]:
    /// with a single member, `max`, `min` and first-found are the same value,
    /// so aggregation only becomes observable here.
    fn seed_multi_member_workspace(root: &Path, go_work_version: &str, members: &[(&str, &str)]) {
        let uses: String = members.iter().map(|(p, _)| format!("\t./{p}\n")).collect();
        write_file(
            root,
            "go.work",
            &format!("go {go_work_version}\n\n// managed by repoweave\nuse (\n{uses})\n"),
        );
        for (path, version) in members {
            let name = path.split('/').next_back().unwrap();
            write_file(
                root,
                &format!("{path}/go.mod"),
                &format!("module example.com/{name}\n\ngo {version}\n"),
            );
        }
    }

    /// Run the predicate over a multi-member workspace and return the finding.
    fn probe_members(
        go_work_version: &str,
        members: &[(&str, &str)],
    ) -> Option<MemberIncompatibility> {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        seed_multi_member_workspace(root, go_work_version, members);

        let manifest =
            make_manifest_local(members.iter().map(|(p, _)| (*p, Role::Owned)).collect());
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        member_incompatibility_for(root, &manifest, &project, &config, &cache)
    }

    /// Run the predicate over a one-member workspace and return the finding.
    fn probe(go_work_version: Option<&str>, member_version: &str) -> Option<MemberIncompatibility> {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        seed_go_workspace(root, go_work_version, member_version);

        let manifest = make_manifest_local(vec![("github/test/repoweave", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        member_incompatibility_for(root, &manifest, &project, &config, &cache)
    }

    #[test]
    fn member_incompatibility_reports_pin_below_max_member() {
        let found = probe(Some("1.21"), "1.26").expect("go 1.21 under a member's 1.26 is a breach");
        let issue = found.into_issue();

        assert_eq!(issue.integration, "go-work");
        assert!(
            !issue.safe_to_fix,
            "member-incompatibility is never auto-repairable: {issue:?}"
        );
        assert!(
            issue.message.contains("member-incompatibility"),
            "message must carry the kind tag; got: {}",
            issue.message
        );
        assert!(
            issue.message.contains("1.21") && issue.message.contains("1.26"),
            "message must name both the on-disk value and the requirement; got: {}",
            issue.message
        );
        assert!(
            issue.message.contains("github/test/repoweave/go.mod"),
            "message must name the member carrying the requirement; got: {}",
            issue.message
        );
    }

    #[test]
    fn member_incompatibility_message_names_both_remedies_and_never_fix() {
        let issue = probe(Some("1.21"), "1.26")
            .expect("fixture must produce a finding")
            .into_issue();

        // Both remedies, in both directions — the operator's choice, not rwv's.
        assert!(
            issue.message.contains("raise"),
            "message must offer raising the managed value; got: {}",
            issue.message
        );
        assert!(
            issue.message.contains("lower the requirement"),
            "message must offer lowering the member requirement; got: {}",
            issue.message
        );
        // `--fix` re-runs activate(), which refuses to overwrite an existing
        // DefaultOnly value by design. Advertising it would be a lie.
        assert!(
            !issue.message.contains("--fix"),
            "message must never advertise --fix; got: {}",
            issue.message
        );
    }

    #[test]
    fn member_incompatibility_silent_when_pin_meets_or_exceeds_members() {
        assert!(
            probe(Some("1.26"), "1.26").is_none(),
            "an on-disk value equal to the requirement is compatible"
        );
        assert!(
            probe(Some("1.27"), "1.26").is_none(),
            "an on-disk value above the requirement is compatible"
        );
        assert!(
            probe(Some("1.26.1"), "1.26").is_none(),
            "a patch-level bump above the requirement is compatible"
        );
    }

    #[test]
    fn member_incompatibility_silent_when_go_line_absent() {
        // activate() seeds the go-line when it is absent, so there is no
        // operator choice yet to be incompatible with.
        assert!(
            probe(None, "1.26").is_none(),
            "an absent go-line is not a breach — activate() seeds it"
        );
    }

    #[test]
    fn member_incompatibility_compares_numerically_not_lexicographically() {
        // "1.9" sorts ABOVE "1.21" as a string and BELOW it as a version.
        // The string answer would silently miss a real broken build.
        assert!(
            probe(Some("1.9"), "1.21").is_some(),
            "go 1.9 under a member's go 1.21 is a breach"
        );
        assert!(
            probe(Some("1.26"), "1.26.1").is_some(),
            "go 1.26 under a member's go 1.26.1 is a breach"
        );
    }

    /// The requirement is the STRONGEST member's, not any member's. Three
    /// members straddle the pin, and the detection list is sorted by path, so
    /// the one that decides (`member-b`) is neither first nor last: min and
    /// first-found land on `member-a`'s 1.20, which the pin satisfies, and
    /// report nothing; last-found lands on `member-c`'s 1.22 and names the
    /// wrong member with the wrong version.
    #[test]
    fn member_incompatibility_takes_the_maximum_member_requirement() {
        let found = probe_members(
            "1.21",
            &[
                ("github/test/member-a", "1.20"),
                ("github/test/member-b", "1.26"),
                ("github/test/member-c", "1.22"),
            ],
        )
        .expect("go 1.21 is below member-b's 1.26 — the strongest requirement decides");
        let issue = found.into_issue();

        assert!(
            issue.message.contains("github/test/member-b/go.mod"),
            "message must name the member carrying the strongest requirement; got: {}",
            issue.message
        );
        assert!(
            issue.message.contains("1.26"),
            "message must report the strongest requirement, not a weaker one; got: {}",
            issue.message
        );
        // Naming a member the pin already satisfies would send the operator to
        // a file that needs no change.
        assert!(
            !issue.message.contains("1.20") && !issue.message.contains("member-a"),
            "message must not name a member the pin already satisfies; got: {}",
            issue.message
        );
        assert!(
            !issue.message.contains("1.22") && !issue.message.contains("member-c"),
            "message must not name a member the pin already satisfies; got: {}",
            issue.message
        );
    }

    #[test]
    fn member_incompatibility_silent_when_every_member_is_below_the_pin() {
        assert!(
            probe_members(
                "1.26",
                &[
                    ("github/test/member-a", "1.20"),
                    ("github/test/member-b", "1.21"),
                    ("github/test/member-c", "1.22"),
                ],
            )
            .is_none(),
            "a pin above every member's requirement is compatible, however many members there are"
        );
    }

    /// Rule-5 coexistence: on ONE fixture, `verify()` reports CLEAN (the
    /// DefaultOnly go-line is the operator's, permanently) while the
    /// member-incompatibility predicate reports the breach. The two facts are
    /// separate; neither reinterprets the other. `go_directive_default_only_
    /// drift_is_clean` pins the CLEAN half on its own.
    #[test]
    fn member_incompatibility_coexists_with_clean_verify() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        seed_go_workspace(root, Some("1.21"), "1.26");

        let manifest = make_manifest_local(vec![("github/test/repoweave", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx_local(root, &project, &manifest, &config, &cache);

        let drift = GoWork.verify(&ctx).unwrap();
        assert!(
            drift.is_empty(),
            "rule 5: DefaultOnly divergence stays CLEAN in verify() — got: {drift:?}"
        );

        let found = GoWork.member_incompatibility(&ctx).unwrap();
        assert!(
            found.is_some(),
            "the same fixture must still report the incompatibility"
        );
    }
}
