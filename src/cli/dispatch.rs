//! Argument dispatch for the `rwv` binary: everything between clap's parse
//! and the verb handlers.
//!
//! Dispatch lives in the library rather than in `main.rs`, and that is
//! load-bearing rather than tidy. A `[[bin]]` target is a *separate crate*
//! from the `[lib]`, so a dispatch layer in `main.rs` can only reach the
//! library through its `pub` surface — which would force the consent tokens
//! to expose a `pub` mint for dispatch to call, and a `pub` mint is reachable
//! from every module of the library, `vcs.rs` included. With dispatch
//! in-crate the mint narrows to `pub(in crate::cli)` and the compiler
//! enforces that only the flag module can construct one. See
//! [`super::consent`].
//!
//! `main.rs` is a shim over [`run`] and holds no logic of its own.

use crate::activate;
use crate::add_remove;
use crate::check;
use crate::cli::{consent, Cli, Commands, SetupAction, WorkweaveAction};
use crate::explain;
use crate::fetch;
use crate::init;
use crate::lock;
use crate::plugins;
use crate::prime;
use crate::push;
use crate::setup;
use crate::status;
use crate::sync;
use crate::update;

use crate::manifest::{ProjectName, WorkweaveName};
use crate::workspace::{acquire_origin_dir, WorkspaceContext};
use anyhow::Context;
use clap::{CommandFactory, FromArgMatches};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Validate and canonicalize the `-C <path>` argument.
///
/// The argument must be an existing path on disk (canonicalize will error
/// otherwise). A bare `<project>--<name>` string that matches the workweave
/// name shape but does not exist as a path gets a corrective error pointing
/// at `-w/--workweave`.
fn resolve_cwd_override(raw: &str) -> anyhow::Result<PathBuf> {
    let p = std::path::Path::new(raw);

    // Before trying to canonicalize, check whether this looks like a
    // workweave name (<project>--<name>) that does not exist on disk.
    // That is the most common mistake: passing a workweave identity to
    // -C instead of -w/--workweave.
    if !p.exists() && looks_like_workweave_name(raw) {
        anyhow::bail!(
            "'-C {raw}' looks like a workweave name rather than a path, \
             and no path exists at '{raw}'.\n\
             \n\
             To address a workweave by name, use -w/--workweave:\n\
             \n  rwv -w {raw} <verb>\n\
             \n\
             To address by path, pass the full path to the workweave directory."
        );
    }

    p.canonicalize()
        .with_context(|| format!("'-C {raw}': path does not exist or cannot be accessed"))
}

/// Returns true when `s` has no path separators and parses as the
/// `<project>--<name>` workweave name shape via
/// [`crate::workspace::parse_weave_dir_name`].
fn looks_like_workweave_name(s: &str) -> bool {
    // Must not contain any path separator — a bare name, not a path.
    if s.contains('/') || s.contains('\\') {
        return false;
    }
    crate::workspace::parse_weave_dir_name(s).is_some()
}

/// Resolve the workweave directory for a `-w <project>--<name>` argument.
///
/// ## Validation
///
/// The argument must be in strict `<project>--<name>` form: exactly one `--`
/// separator (split at the FIRST `--`, matching the directory-name convention
/// used elsewhere in workweave.rs), non-empty project and name on both sides,
/// and no path separators. A path-shaped argument (contains `/` or `\`, or
/// exists on disk as a path) gets a corrective error pointing at `-C`.
///
/// ## Resolution
///
/// Workspace is located from `workspace_origin` (already resolved from `-C` or
/// process cwd). The workweave path is resolved via the registry for the named
/// project, with `.rwv-workweave` marker round-trip validation.
///
/// ## Return value
///
/// Returns `(workweave_path, project)` — the path to feed to the resolver as
/// origin, and the project name parsed from the `-w` prefix.
fn resolve_workweave_flag(
    raw: &str,
    workspace_origin: &Path,
) -> anyhow::Result<(PathBuf, ProjectName)> {
    // A path-shaped argument (contains a separator) is a mistake: -C handles
    // path addressing. Give a corrective error rather than a confusing
    // "not found" message.
    if raw.contains('/') || raw.contains('\\') {
        anyhow::bail!(
            "'-w {raw}' contains a path separator — it looks like a path, not a workweave name.\n\
             \n\
             To address a workweave by path, use -C:\n\
             \n  rwv -C {raw} <verb>\n\
             \n\
             To address by name, pass <project>--<name> with no separators."
        );
    }
    // If the argument exists on disk as a path, the operator probably meant -C.
    if Path::new(raw).exists() {
        anyhow::bail!(
            "'-w {raw}' exists on disk as a path.\n\
             \n\
             To address a workweave by path, use -C:\n\
             \n  rwv -C {raw} <verb>\n\
             \n\
             To address by name, pass <project>--<name> (bare name, no path separators)."
        );
    }

    let (project_str, name_str) =
        crate::workspace::split_at_weave_separator(raw).ok_or_else(|| {
            anyhow::anyhow!(
                "'-w {raw}' is not in the required <project>--<name> form.\n\
             \n\
             Provide both the project and the workweave name separated by `--`:\n\
             \n  rwv -w <project>--<name> <verb>\n\
             \n\
             Example:  rwv -w myproj--hotfix sync-to"
            )
        })?;

    if project_str.is_empty() || name_str.is_empty() {
        anyhow::bail!(
            "'-w {raw}' is not in the required <project>--<name> form: \
             both the project and name must be non-empty.\n\
             \n\
             Example:  rwv -w myproj--hotfix sync-to"
        );
    }

    let project = ProjectName::new(project_str)
        .with_context(|| format!("'-w {raw}' has an invalid project name"))?;
    let name =
        WorkweaveName::new(name_str).with_context(|| format!("'-w {raw}' has an invalid name"))?;

    // Find the primary workspace root from the workspace_origin path (from -C
    // or process cwd). The registry lives on the primary; look up from there.
    let primary_ctx = WorkspaceContext::resolve(workspace_origin, None).with_context(|| {
        format!(
            "'-w {raw}': could not locate a workspace from {}",
            workspace_origin.display()
        )
    })?;
    let primary_root = primary_ctx.primary_path().to_path_buf();

    // Registry lookup with marker round-trip validation.
    let workweave_path =
        crate::workweave::resolve_registered_workweave(&primary_root, &project, &name)
            .with_context(|| {
                format!(
                    "'-w {raw}': registry lookup failed for project `{}`, name `{}`",
                    project.as_str(),
                    name.as_str()
                )
            })?;

    match workweave_path {
        Some(path) => Ok((path, project)),
        None => {
            // Registry has no valid entry for this name. Build an actionable
            // error: list the known names for the project so the operator can
            // spot a typo or learn what workweaves exist.
            let known = crate::workweave_index::read(&primary_root, &project)
                .ok()
                .flatten()
                .map(|idx| {
                    let mut names: Vec<String> = idx.workweaves.into_keys().collect();
                    names.sort();
                    names
                })
                .unwrap_or_default();

            if known.is_empty() {
                anyhow::bail!(
                    "no workweave named `{}` is registered for project `{}` \
                     (the registry has no entries; create one with \
                     `rwv workweave {} create <name>`)",
                    name.as_str(),
                    project.as_str(),
                    project.as_str(),
                )
            } else {
                anyhow::bail!(
                    "no workweave named `{}` is registered for project `{}`.\n\
                     \n\
                     Known workweaves for `{}`:\n  {}\n\
                     \n\
                     Run `rwv doctor` if a workweave exists on disk but is missing from the registry.",
                    name.as_str(),
                    project.as_str(),
                    project.as_str(),
                    known.join("\n  "),
                )
            }
        }
    }
}

/// Resolve the workspace context for a project-scoped verb and surface the
/// pointer fall-through as a "target:" line to stderr before acting.
///
/// The rule: any project-scoped verb whose project came from the
/// `.rwv-active` pointer (rather than `--project` or a workweave marker)
/// prints the resolved target before acting, so operators catch a wrong
/// pointer at invocation time instead of by post-hoc git-status forensics.
/// Structurally- or explicitly-resolved invocations stay silent.
///
/// When `use_workweave_flag` is true (the `-w` global flag was given),
/// the resolved context's provenance is re-branded from `Marker` to
/// `WorkweaveFlag` before the target-line check runs, so the explicit
/// `-w` addressing form is correctly recorded in the chain-step field
/// and the target line is suppressed (it only fires for `.rwv-active`
/// fall-throughs, never for explicit addressing).
///
/// Wrapping resolve + emit in one call keeps the per-verb dispatch site to
/// a single line and makes it impossible to forget the surfacing on a new
/// project-scoped verb.
fn resolve_project_scoped(
    origin_dir: &Path,
    project_override: Option<crate::manifest::ProjectName>,
    use_workweave_flag: bool,
) -> anyhow::Result<WorkspaceContext> {
    Ok(with_target_surfaced(
        WorkspaceContext::resolve(origin_dir, project_override)?,
        use_workweave_flag,
    ))
}

/// [`resolve_project_scoped`] for `doctor` and `status`, the two verbs that
/// report on a weave root carrying both identity files instead of refusing to
/// act through it.
///
/// Reachable only by naming it: every other project-scoped verb resolves
/// through [`resolve_project_scoped`] and refuses.
fn resolve_project_scoped_tolerating_disputed_root(
    origin_dir: &Path,
    project_override: Option<crate::manifest::ProjectName>,
    use_workweave_flag: bool,
) -> anyhow::Result<WorkspaceContext> {
    Ok(with_target_surfaced(
        WorkspaceContext::resolve_tolerating_disputed_root(origin_dir, project_override)?,
        use_workweave_flag,
    ))
}

fn with_target_surfaced(ctx: WorkspaceContext, use_workweave_flag: bool) -> WorkspaceContext {
    let ctx = if use_workweave_flag {
        ctx.with_workweave_flag_provenance()
    } else {
        ctx
    };
    ctx.emit_target_line();
    ctx
}

/// Levenshtein edit distance between two strings (two-row dynamic-programming
/// variant). Kept self-contained here so the early-dispatch interceptor can
/// distinguish a workweave-subcommand *typo* (small distance from
/// `create`/`delete`/`list`) from a genuine bare workweave name (large
/// distance). Intentionally a local copy rather than a dependency on
/// `explain.rs`'s sibling helper, to keep `main()`'s pre-parse path free of
/// cross-module coupling.
fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let m = a.len();
    let n = b.len();
    if m == 0 {
        return n;
    }
    if n == 0 {
        return m;
    }
    let mut prev: Vec<usize> = (0..=n).collect();
    let mut curr = vec![0usize; n + 1];
    for i in 1..=m {
        curr[0] = i;
        for j in 1..=n {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            curr[j] = (prev[j] + 1).min(curr[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[n]
}

/// `rwv workweave <PROJECT> delete <NAME>` — the dispatch arm, named.
///
/// The other minting arms are written inline in [`run`]'s `match`; this one
/// is a named function because it is the only one integration tests drive
/// from *below* the process boundary. `workweave_delete_topology_test` and
/// `e2e_workweave_isolation_test` build clone topologies that would be
/// laborious to reach through argv, and then assert on the `Err` value
/// rather than on stderr. They cannot mint a
/// [`consent::DiscardUnmergedConsent`] themselves — that is the point of the
/// token — so they pass the flags and come through the door dispatch uses.
pub fn workweave_delete(
    primary_root: &Path,
    project: &ProjectName,
    name: &WorkweaveName,
    discard_uncommitted: bool,
    discard_unmerged_commits: bool,
) -> anyhow::Result<()> {
    // The token IS the warrant an unmerged ref's DESTROY needs:
    // `delete_workweave` waives the verb-level precondition on it AND hands
    // it to `DeletionWarrant::operator_discarded` per ref.
    let discard_unmerged = consent::DiscardUnmergedConsent::from_flag(discard_unmerged_commits);
    crate::workweave::delete_workweave(
        primary_root,
        project,
        name,
        None,
        discard_uncommitted,
        discard_unmerged,
    )
}

/// Parse `argv` and run the requested verb. The whole of `rwv`'s behaviour
/// hangs off this call; `main.rs` does nothing but forward to it.
pub fn run() -> anyhow::Result<()> {
    // Early-dispatch did-you-mean hints for removed/relocated flags.
    // These run before clap's full parse so we can produce a friendly error
    // instead of clap's generic "unexpected argument" message.
    //
    // MIGRATORY shims: the flag spellings they intercept were renamed in
    // v0.10.0. Expiry follows the alpha rule rather than a health floor —
    // verify no local automation still spells the old flags, then delete
    // the shim (`crate::health_floor` documents the floor-gated arms).
    {
        let raw_args: Vec<String> = std::env::args().collect();
        // Detect: rwv sync --retire (--retire has moved to sync-to)
        if raw_args.get(1).map(|s| s.as_str()) == Some("sync")
            && raw_args.iter().any(|a| a == "--retire")
        {
            eprintln!(
                "error: `--retire` has moved to `rwv sync-to`; use `rwv sync-to --retire` instead"
            );
            std::process::exit(2);
        }
        // Detect: rwv sync --force / rwv sync-to --force (--force has been
        // split into named overrides; emit a friendly migration message).
        let is_sync = raw_args.get(1).map(|s| s.as_str()) == Some("sync")
            || raw_args.get(1).map(|s| s.as_str()) == Some("sync-to");
        if is_sync && raw_args.iter().any(|a| a == "--force") {
            eprintln!(
                "error: `--force` has been removed from `rwv sync` and `rwv sync-to`.\n\
                 \n\
                 Replace it with the specific override(s) you need:\n\
                   --allow-stale-lock        skip the lock-freshness precondition\n\
                   --discard-local-commits   discard CWD project commits not in source \
                 (recoverable via `rwv abort`)"
            );
            std::process::exit(2);
        }
        // Detect: `--force` on a verb whose precondition waiver was renamed to
        // name the consequence it consents to. `push --force` is untouched — it
        // is git's force-push, not a precondition waiver.
        if raw_args.iter().any(|a| a == "--force") {
            let migration = match raw_args.get(1).map(|s| s.as_str()) {
                Some("fetch") => Some(
                    "`--force` has been renamed on `rwv fetch`.\n\
                     \n\
                     Use the specific override you need:\n\
                       --allow-non-empty-dir  bootstrap into a non-empty directory \
                     that is not a workspace\n\
                       --detach-checkouts     realign a present clone even where that \
                     changes what HEAD is attached to",
                ),
                Some("remove") => Some(
                    "`--force` has been renamed on `rwv remove`.\n\
                     \n\
                     Use `--delete-shared-clone` to delete a clone that other projects \
                     still reference.",
                ),
                Some("workweave") => match raw_args.get(3).map(|s| s.as_str()) {
                    Some("create") => Some(
                        "`--force` has been renamed on `rwv workweave <project> create`.\n\
                         \n\
                         Use `--replace-existing` to destroy an existing workweave and \
                         recreate it from scratch.",
                    ),
                    Some("delete") => Some(
                        "`--force` has been split on `rwv workweave <project> delete`.\n\
                         \n\
                         Replace it with the specific override(s) you need:\n\
                           --discard-uncommitted       delete despite uncommitted changes\n\
                           --discard-unmerged-commits  delete despite commits not merged \
                         into the parent weave",
                    ),
                    _ => None,
                },
                _ => None,
            };
            if let Some(msg) = migration {
                eprintln!("error: {msg}");
                std::process::exit(2);
            }
        }
        // Detect: `--detach-working-branch` on `rwv fetch` — an interim
        // override that shipped ahead of the full branch model. The model
        // renames it to `--detach-checkouts`, which also covers `rwv
        // update` — rename, no alias (alpha, no back-compat shims).
        if raw_args.get(1).map(|s| s.as_str()) == Some("fetch")
            && raw_args.iter().any(|a| a == "--detach-working-branch")
        {
            eprintln!(
                "error: `--detach-working-branch` has been renamed to `--detach-checkouts` \
                 on `rwv fetch`."
            );
            std::process::exit(2);
        }
        // Detect: rwv workweave <PROJECT> <WORD> where WORD is a bare token that
        // is neither a known subcommand nor a flag. clap consumes <PROJECT> as
        // the `[PROJECT]` positional, then sees WORD as an *unexpected argument*
        // for the outer `workweave` command (it never reaches the subcommand
        // recognition path), so its generic message is "unexpected argument" and
        // it can't offer a "did you mean". Reframe it as a missing-subcommand
        // error with a create-shaped suggestion.
        //
        // Guards:
        //   - Only fire when a 4th token (argv[3]) is present and non-flag, so
        //     `rwv workweave foundations` (list default) and
        //     `rwv workweave foundations --help` (help) are untouched.
        //   - PROJECT (argv[2]) must itself be non-flag, so `rwv workweave
        //     --claude-hook` (no [PROJECT]) is untouched.
        //   - Skip when WORD is a known WorkweaveAction so the real subcommand
        //     path (create/delete/list, plus clap's `help`) keeps working.
        //   - Skip when WORD is a *near-miss* of a known WorkweaveAction (edit
        //     distance <= SUBCOMMAND_TYPO_THRESHOLD). A bare WORD that is one or
        //     two edits from `create`/`delete`/`list` (e.g. `crete`) is far more
        //     likely a subcommand typo than a workweave name; deferring to clap
        //     lets its native "tip: a similar subcommand exists" message fire,
        //     which is strictly more helpful than suggesting we create a
        //     workweave literally named `crete`. The genuine bare-name case
        //     (`hotfix`, edit distance 6 from every action) is far from any
        //     subcommand and still gets the create-shaped reframe.
        if raw_args.get(1).map(|s| s.as_str()) == Some("workweave") {
            let project = raw_args.get(2).map(|s| s.as_str());
            let word = raw_args.get(3).map(|s| s.as_str());
            let is_flag = |s: &str| s.starts_with('-');
            if let (Some(project), Some(word)) = (project, word) {
                let cli_cmd = Cli::command();
                let workweave_cmd = cli_cmd
                    .find_subcommand("workweave")
                    .expect("`workweave` is a Commands variant");
                // WorkweaveAction names a typo could be aiming at. `help` is a
                // clap builtin, not a typo target worth fuzzy-matching, so it's
                // excluded here (an exact `help` is already handled above).
                let subcommand_actions: Vec<&str> = workweave_cmd
                    .get_subcommands()
                    .map(|c| c.get_name())
                    .filter(|&name| name != "help")
                    .collect();
                // Edit-distance threshold below which WORD is treated as a
                // subcommand typo and deferred to clap's native suggestion.
                const SUBCOMMAND_TYPO_THRESHOLD: usize = 2;
                let near_subcommand = subcommand_actions
                    .iter()
                    .any(|sub| levenshtein(word, sub) <= SUBCOMMAND_TYPO_THRESHOLD);
                let is_known_subcommand = workweave_cmd
                    .get_subcommands()
                    .any(|c| c.get_name() == word);
                if !is_flag(project) && !is_flag(word) && !is_known_subcommand && !near_subcommand {
                    eprintln!(
                        "error: '{word}' is not a valid subcommand for 'rwv workweave {project}'\n\
                         Did you mean:  rwv workweave {project} create {word}\n\
                         Available subcommands: {}",
                        subcommand_actions.join(", ")
                    );
                    std::process::exit(2);
                }
            }
        }
    }

    // Build the clap command and dynamically append an "External commands"
    // section when `rwv-*` executables are found on PATH. The section lists
    // names only (no descriptions) and appears after clap's core usage
    // summary, keeping the core surface unmodified. An empty PATH scan
    // produces no section at all. `None::<&OsStr>` passes through to the
    // process's inherited PATH — the `which` crate owns the OS lookup.
    let cmd = {
        let base = Cli::command();
        match plugins::external_commands_help_section(None::<&std::ffi::OsStr>) {
            Some(section) => base.after_help(section),
            None => base,
        }
    };

    // Suppress the "For more information, try '--help'" footer on clap errors
    // when `--help`/`-h` is already present in the invocation: re-advising the
    // flag the user just typed is noise. We can't hook this cleanly in clap 4
    // (no stable on-error footer override in derive mode), so detect `--help`
    // in the raw args and, on a clap error, re-render it with that one footer
    // line filtered out. Non-help invocations keep clap's default error path.
    let cli = match cmd.try_get_matches() {
        Ok(matches) => Cli::from_arg_matches(&matches).unwrap_or_else(|e| e.exit()),
        Err(err) => {
            let help_requested = std::env::args().any(|a| a == "--help" || a == "-h");
            if help_requested && err.use_stderr() {
                let rendered = err.render().to_string();
                let stripped: String = rendered
                    .lines()
                    .filter(|line| !line.contains("For more information, try"))
                    .collect::<Vec<_>>()
                    .join("\n");
                eprintln!("{stripped}");
                std::process::exit(err.exit_code());
            }
            err.exit();
        }
    };

    // ------------------------------------------------------------------
    // Single resolution point.
    //
    // Every workspace-scoped verb below reads its context from THIS
    // one call to `acquire_origin_dir`. No downstream handler calls
    // `std::env::current_dir()`; no handler re-resolves the invocation
    // context. Resolution is a pure function of `(argv, origin_dir)` —
    // `project_override` is baked in per verb before `resolve` runs, and
    // the resolved context is threaded to the handler as `&WorkspaceContext`.
    //
    // When `-C <path>` is given, that path (canonicalized, must exist)
    // substitutes as the origin dir; `acquire_origin_dir` is not called.
    // No `chdir` occurs — the address is threaded through the resolver
    // as a pure argument, just as process cwd would be otherwise.
    //
    // When `-w <project>--<name>` is given, the workspace origin is first
    // located (from `-C` or process cwd), then the registry is consulted to
    // find the workweave path for the named project+name.  That workweave
    // path becomes the origin dir for all downstream resolution.  The flags
    // compose: `-C establishes the workspace, `-w` selects the checkout within
    // it.  The resolved context's provenance is re-branded from `Marker` to
    // `WorkweaveFlag` (via `WorkspaceContext::with_workweave_flag_provenance`)
    // because the explicit `-w` flag, not the containment walk, addressed the
    // workweave — the distinction matters for `emit_target_line` (silent for
    // all explicit forms) and for the chain-step record kept on the context.
    //
    // Exemptions from the pre-resolve step:
    //   - `init` / `init --adopt`: may run in an empty directory that has
    //     no workspace yet, so bootstrap-then-first-resolve happens
    //     inside the handler with the origin dir passed through.
    //   - `fetch <SOURCE>`: same shape — a bootstrap into an empty (or
    //     `--force`d) directory, resolved after the bootstrap.
    //   - `prime`: resolves at dispatch but tolerates the "no workspace"
    //     case with a graceful `--no-suppress` fallback (Option<&ctx>).
    //   - `completions`, `explain`, `setup claude*`: no workspace involved.
    // ------------------------------------------------------------------

    // Step 1: workspace origin — cwd or -C path.
    let workspace_origin = match cli.cwd_override.as_deref() {
        None => acquire_origin_dir()?,
        Some(raw) => resolve_cwd_override(raw)?,
    };

    // Step 2: workweave selection — registry lookup when -w is given.
    // `workweave_flag_project` carries the project parsed from `-w <proj>--<name>`.
    // When absent, dispatch uses `workspace_origin` directly.
    let (origin_dir, workweave_flag_project): (PathBuf, Option<ProjectName>) =
        match cli.workweave_flag.as_deref() {
            None => (workspace_origin, None),
            Some(raw) => {
                let (ww_path, project) = resolve_workweave_flag(raw, &workspace_origin)?;
                (ww_path, Some(project))
            }
        };
    // `use_workweave_flag` is true when `-w` was given; downstream resolver
    // calls apply `with_workweave_flag_provenance()` to stamp the correct
    // chain-step provenance on the resolved context.
    let use_workweave_flag = workweave_flag_project.is_some();

    match cli.command {
        None => {
            let ctx = WorkspaceContext::resolve(&origin_dir, None)?;
            println!("{}", ctx.display());
        }
        Some(Commands::Activate {
            project,
            no_materialize,
        }) => {
            let ctx = WorkspaceContext::resolve(&origin_dir, None)?;
            activate::activate_with_options(
                &project,
                &ctx,
                activate::ActivateOptions { no_materialize },
            )?;
        }
        Some(Commands::Materialize {
            regenerate_drifted,
            adopt_drifted,
        }) => {
            let ctx = WorkspaceContext::resolve(&origin_dir, None)?;
            // clap refuses the two together, so this reads as the exclusive
            // choice it is rather than a precedence.
            let consent = consent::RegenerateDriftedConsent::from_flag(regenerate_drifted)
                .map(consent::DriftConsent::Regenerate)
                .or_else(|| {
                    consent::AdoptDriftedConsent::from_flag(adopt_drifted)
                        .map(consent::DriftConsent::Adopt)
                });
            activate::materialize(&ctx, consent)?;
        }
        Some(Commands::Prime { no_suppress }) => {
            // `prime` tolerates the not-in-a-workspace case (silent unless
            // `--no-suppress`); resolve here and pass `Option<&ctx>` so
            // the handler stays free of resolution logic.
            let ctx = WorkspaceContext::resolve(&origin_dir, None).ok();
            prime::prime(ctx.as_ref(), no_suppress)?;
        }
        Some(Commands::Resolve) => {
            let ctx = WorkspaceContext::resolve(&origin_dir, None)?;
            println!("{}", ctx.active_path().display());
        }
        Some(Commands::Add {
            url,
            role,
            new,
            project,
        }) => {
            let project_override = project.map(crate::manifest::ProjectName::new).transpose()?;
            let ctx = resolve_project_scoped(&origin_dir, project_override, use_workweave_flag)?;
            if new {
                add_remove::run_add_new(&url, &ctx)?;
            } else {
                add_remove::run_add(&url, role, &ctx)?;
            }
        }
        Some(Commands::Fetch {
            source,
            frozen,
            allow_non_empty_dir,
            no_reference,
            detach_checkouts,
            roles,
            repos,
            jobs,
            json,
        }) => {
            let mode = if frozen {
                fetch::FetchMode::Frozen
            } else {
                fetch::FetchMode::Default
            };
            // Mint once from the parsed flag; the token threads down to
            // fetch_one, where it gates the realign-detach refusal.
            let detach_checkouts = consent::DetachConsent::from_flag(detach_checkouts);
            let filter = crate::selector::RepoFilter::parse(&roles, &repos)?;
            // fetch's default is auto-resolve (min(nproc, 8)), unlike sync which
            // defaults to serial to preserve envelope vs NDJSON contract. fetch's
            // JSON contract follows the same shape: envelope when -j 1 or no -j
            // with default resolution, NDJSON when -j > 1. Because the default
            // can resolve to > 1 on multi-core hosts, agents should pass `-j 1`
            // explicitly when they require the envelope shape.
            let jobs = crate::parallel::resolve_jobs(jobs);
            match source {
                Some(src) => {
                    // SOURCE-mode: bootstrap a project into the workspace (or
                    // an empty directory, or a non-empty one under
                    // --allow-non-empty-dir). The origin dir is the bootstrap
                    // target — no invocation-context resolution yet.
                    crate::workspace::require_workspace_or_empty(&origin_dir, allow_non_empty_dir)?;
                    fetch::run_fetch(
                        &src,
                        &origin_dir,
                        mode,
                        no_reference,
                        detach_checkouts,
                        &filter,
                        jobs,
                        json,
                    )?;
                }
                None => {
                    // In-place mode: no SOURCE (in-place requires a
                    // workspace). Re-materialize missing manifest members of
                    // the active project. `--allow-non-empty-dir` is a
                    // bootstrap knob (non-empty non-workspace directory); it
                    // has no meaning in-place, so reject it to keep the UX
                    // honest.
                    if allow_non_empty_dir {
                        anyhow::bail!(
                            "rwv fetch: --allow-non-empty-dir has no effect without SOURCE; \
                             pass a SOURCE to bootstrap into a non-empty directory, \
                             or drop --allow-non-empty-dir to re-materialize missing members \
                             in place"
                        );
                    }
                    let ctx = WorkspaceContext::resolve(&origin_dir, None).with_context(|| {
                        format!(
                            "rwv fetch: no SOURCE and no repoweave workspace found above {}",
                            origin_dir.display(),
                        )
                    })?;
                    // In-place fetch operates on the active project — surface
                    // the pointer-decided target before acting.
                    ctx.emit_target_line();
                    fetch::run_fetch_in_place(
                        &ctx,
                        mode,
                        no_reference,
                        detach_checkouts,
                        &filter,
                        jobs,
                        json,
                    )?;
                }
            }
        }
        Some(Commands::Init {
            project,
            provider,
            adopt,
        }) => {
            // Bootstrap-then-resolve happens inside the handler — see init.rs.
            if adopt {
                init::init_adopt(&project, &origin_dir)?;
            } else {
                init::init(&project, provider.as_deref(), &origin_dir)?;
            }
        }
        Some(Commands::Remove {
            path,
            delete,
            delete_shared_clone,
            project,
        }) => {
            let project_override = project.map(crate::manifest::ProjectName::new).transpose()?;
            let ctx = resolve_project_scoped(&origin_dir, project_override, use_workweave_flag)?;
            add_remove::run_remove(&path, delete, delete_shared_clone, &ctx)?;
        }
        Some(Commands::Workweave {
            project,
            hook_mode,
            claude_hook,
            action,
        }) => {
            if claude_hook {
                // The Claude hook reads its own cwd from stdin JSON — that
                // input is a hook-provided argument, not the process cwd,
                // so it does not go through `acquire_origin_dir`.
                crate::workweave::handle_claude_hook()?;
            } else {
                let project = project.expect("project is required unless --claude-hook is set");
                let project = crate::manifest::ProjectName::new(project)?;
                let ctx = WorkspaceContext::resolve(&origin_dir, None)?;
                let primary_root = ctx.primary_path();

                match action {
                    Some(WorkweaveAction::List) | None => {
                        let names = crate::workweave::list_workweaves(primary_root, &project)?;
                        for n in &names {
                            println!("{}", n);
                        }
                    }
                    Some(WorkweaveAction::Delete {
                        name,
                        discard_uncommitted,
                        discard_unmerged_commits,
                    }) => {
                        workweave_delete(
                            primary_root,
                            &project,
                            &WorkweaveName::new(name)?,
                            discard_uncommitted,
                            discard_unmerged_commits,
                        )?;
                    }
                    Some(WorkweaveAction::Create {
                        name,
                        replace_existing,
                        from,
                        capture_dirty,
                        worktree_references,
                        dir,
                    }) => {
                        let source_root = match from.as_deref() {
                            None => ctx.active_path().to_path_buf(),
                            Some(s)
                                if matches!(
                                    s.parse::<sync::SyncSource>(),
                                    Ok(sync::SyncSource::Primary)
                                ) =>
                            {
                                primary_root.to_path_buf()
                            }
                            Some(s) => {
                                let p = std::path::PathBuf::from(s);
                                if p.is_absolute() {
                                    p
                                } else {
                                    primary_root.join(s)
                                }
                            }
                        };
                        let dir_override = dir.as_deref().map(std::path::Path::new);
                        let workweave_path = crate::workweave::create_workweave(
                            primary_root,
                            &source_root,
                            &project,
                            &WorkweaveName::new(name)?,
                            replace_existing,
                            capture_dirty,
                            worktree_references,
                            dir_override,
                        )?;
                        if hook_mode {
                            println!("{}", workweave_path.display());
                        }
                    }
                    Some(WorkweaveAction::Log { diff, json }) => {
                        crate::workweave::workweave_log(&ctx, diff, json)?;
                    }
                    Some(WorkweaveAction::SetContainer { path }) => {
                        let raw = std::path::PathBuf::from(&path);
                        let abs = if raw.is_absolute() {
                            raw
                        } else {
                            primary_root.join(&raw)
                        };
                        let recorded =
                            crate::workweave_index::set_container(primary_root, &project, abs)?;
                        eprintln!(
                            "recorded workweave container for project `{}`: {}",
                            project.as_str(),
                            recorded.display()
                        );
                    }
                }
            }
        }
        Some(Commands::Doctor {
            locked,
            fix,
            json,
            all,
            reattach_checkouts,
            adopt_detached_checkouts,
            kind,
            project,
        }) => {
            let project_override = project.map(crate::manifest::ProjectName::new).transpose()?;
            let ctx = resolve_project_scoped_tolerating_disputed_root(
                &origin_dir,
                project_override,
                use_workweave_flag,
            )?;
            // Minted here — the CLI boundary is the only place that knows
            // the operator asked. `run_check` gates the detached canonical
            // store's reattach on it; without the flag `--fix` reports the
            // `git switch` instead of performing the ATTACH.
            let reattach_checkouts = consent::ReattachConsent::from_flag(reattach_checkouts);
            // Consent for minting a workweave's ref at a detached HEAD, and
            // for giving up a legacy branch's name to make room for it.
            let adopt_detached_checkouts =
                consent::AdoptDetachedConsent::from_flag(adopt_detached_checkouts);
            // Validated at the boundary: an unknown kind refuses here,
            // before any scan runs, naming the valid set.
            let kind_filter = if kind.is_empty() {
                None
            } else {
                Some(check::KindFilter::new(&kind)?)
            };
            if locked {
                let has_drift = check::run_check_locked(&ctx)?;
                if has_drift {
                    std::process::exit(1);
                }
            } else if json {
                let has_errors = check::run_check_json(&ctx, all, kind_filter.as_ref())?;
                if has_errors {
                    std::process::exit(1);
                }
            } else {
                let has_errors = check::run_check(
                    &ctx,
                    fix,
                    all,
                    reattach_checkouts,
                    adopt_detached_checkouts,
                    kind_filter.as_ref(),
                )?;
                if has_errors {
                    std::process::exit(1);
                }
            }
        }
        Some(Commands::Lock {
            dirty,
            commit,
            project,
        }) => {
            let project_override = project.map(crate::manifest::ProjectName::new).transpose()?;
            let ctx = resolve_project_scoped(&origin_dir, project_override, use_workweave_flag)?;
            lock::lock(&ctx, dirty, commit)?;
        }
        Some(Commands::Status { json, project }) => {
            let project_override = project.map(crate::manifest::ProjectName::new).transpose()?;
            let ctx = resolve_project_scoped_tolerating_disputed_root(
                &origin_dir,
                project_override,
                use_workweave_flag,
            )?;
            status::run_status(&ctx, json)?;
        }
        Some(Commands::Abort {
            abandon_foreign_tip,
        }) => {
            let ctx = WorkspaceContext::resolve(&origin_dir, None)?;
            sync::run_abort(
                &ctx,
                &crate::cli::consent::AbandonForeignTipConsent::from_flag(&abandon_foreign_tip),
            )?;
        }
        Some(Commands::Sync {
            source,
            strategy,
            allow_stale_lock,
            discard_local_commits,
            json,
            jobs,
            project,
            do_continue,
        }) => {
            let project_override = project.map(crate::manifest::ProjectName::new).transpose()?;
            let ctx =
                resolve_project_scoped(&origin_dir, project_override.clone(), use_workweave_flag)?;
            // sync's default is serial (jobs=1). This differs from fetch/update
            // (which auto-resolve to min(nproc, 8)) because sync's `--json`
            // contract pins envelope output under `-j 1` and NDJSON under
            // `-j > 1`; defaulting to auto would silently switch envelope ->
            // NDJSON on multi-core hosts. No `-j` or `-j 1` emits the
            // pretty envelope.
            let jobs = match jobs {
                Some(n) => crate::parallel::resolve_jobs(Some(n)),
                None => 1,
            };
            // When --continue is set, source is None (read from op-state).
            // When --continue is absent, source is Some (required by clap).
            let request = sync::SyncRequest {
                source,
                strategy,
                allow_stale_lock,
                discard_local_commits,
                retire: false,
                project_override,
                jobs,
                do_continue,
            };
            if json {
                sync::run_sync_json(&ctx, request)?;
            } else {
                sync::run_sync(&ctx, request)?;
            }
        }
        Some(Commands::SyncTo {
            target,
            strategy,
            allow_stale_lock,
            discard_local_commits,
            retire,
            json,
            jobs,
            project,
            do_continue,
        }) => {
            let project_override = project.map(crate::manifest::ProjectName::new).transpose()?;
            let ctx =
                resolve_project_scoped(&origin_dir, project_override.clone(), use_workweave_flag)?;
            let jobs = match jobs {
                Some(n) => crate::parallel::resolve_jobs(Some(n)),
                None => 1,
            };
            // Resolve target: if None and not --continue, read .rwv-workweave marker's
            // parent field. If --continue, target is read from op-state inside
            // run_sync_to (target is always None when --continue due to clap conflict).
            let resolved_target = if do_continue {
                // --continue: target comes from op-state; pass None sentinel.
                None
            } else {
                Some(match target {
                    Some(t) => t,
                    None => {
                        // Bare `rwv sync-to` — must be inside a workweave. Reuse
                        // the invocation context we already resolved above.
                        match &ctx.checkout {
                            crate::workspace::Checkout::Workweave { parent, .. } => {
                                // Replace the raw `failed to canonicalize …
                                // (os error 2)` a dangling parent would
                                // otherwise produce with friendly
                                // doctor-remediation text.
                                sync::check_parent_not_dangling(parent, ctx.primary_path())?;
                                sync::SyncSource::Path(parent.clone())
                            }
                            crate::workspace::Checkout::Primary { .. } => {
                                anyhow::bail!(
                                    "bare `rwv sync-to` targets the workweave's recorded \
                                     parent, but CWD ({}) is in the primary weave, not a \
                                     workweave. Provide a target explicitly.",
                                    ctx.active_path().display()
                                );
                            }
                        }
                    }
                })
            };
            let request = sync::SyncRequest {
                source: resolved_target,
                strategy,
                allow_stale_lock,
                discard_local_commits,
                retire,
                project_override,
                jobs,
                do_continue,
            };
            if json {
                sync::run_sync_to_json(&ctx, request)?;
            } else {
                sync::run_sync_to(&ctx, request)?;
            }
        }
        Some(Commands::Push {
            project,
            dry_run,
            force,
            roles,
            repos,
            jobs,
            json,
        }) => {
            let project_override = project.map(crate::manifest::ProjectName::new).transpose()?;
            let ctx = resolve_project_scoped(&origin_dir, project_override, use_workweave_flag)?;
            let filter = crate::selector::RepoFilter::parse(&roles, &repos)?;
            // push's default is serial (jobs=1). This differs from fetch/update
            // (which auto-resolve to min(nproc, 8)) because push's `--json`
            // contract pins envelope output under `-j 1` and NDJSON under
            // `-j > 1`; defaulting to auto would silently switch envelope ->
            // NDJSON on multi-core hosts.
            let jobs = match jobs {
                Some(n) => crate::parallel::resolve_jobs(Some(n)),
                None => 1,
            };
            push::run_push(&ctx, dry_run, force, &filter, jobs, json)?;
        }
        Some(Commands::Update {
            dirty,
            commit,
            detach_checkouts,
            project,
            roles,
            repos,
            jobs,
            json,
        }) => {
            let project_override = project.map(crate::manifest::ProjectName::new).transpose()?;
            let ctx = resolve_project_scoped(&origin_dir, project_override, use_workweave_flag)?;
            // Mint once from the parsed flag; the token threads down to
            // advance_checkout, where it gates the ff-or-refuse guard.
            let detach_checkouts = consent::DetachConsent::from_flag(detach_checkouts);
            let filter = crate::selector::RepoFilter::parse(&roles, &repos)?;
            // Update's default is auto-parallel (min(nproc, 8)). The envelope/NDJSON
            // split mirrors sync: -j 1 (or unspecified with --json) emits the
            // envelope; -j > 1 streams NDJSON. Note: unlike sync, update defaults
            // to auto-parallel even without --json, so --json + no -j will default
            // to multi-worker NDJSON on multi-core machines. Callers that want the
            // envelope must pass `-j 1` explicitly alongside --json.
            let jobs = crate::parallel::resolve_jobs(jobs);
            update::run_update(&ctx, dirty, commit, json, &filter, jobs, detach_checkouts)?;
        }
        Some(Commands::Completions { shell }) => {
            let mut cmd = Cli::command();
            clap_complete::generate(shell, &mut cmd, "rwv", &mut std::io::stdout());
        }
        Some(Commands::Explain { command }) => {
            explain::explain(command.as_deref())?;
        }
        Some(Commands::Setup { action }) => match action {
            SetupAction::Claude { uninstall } => {
                if uninstall {
                    setup::claude_uninstall()?;
                } else {
                    setup::claude()?;
                }
            }
        },
        Some(Commands::External(argv)) => {
            // clap routes any subcommand it does not recognise here. The
            // "builtin first" invariant is enforced by clap's match order:
            // a plugin named `rwv-status` cannot shadow the core `status`
            // verb because clap already matched `status` to `Commands::Status`
            // before this arm is reached. See plugins.rs.
            //
            // `argv` is guaranteed non-empty by clap when an external
            // subcommand is captured (element 0 is the verb, remainder are
            // its args). Verb must be UTF-8 because clap's external-
            // subcommand support requires a nameable subcommand and we
            // format it into the "unknown verb" and "rwv-<verb>" messages.
            let mut iter = argv.into_iter();
            let verb_os: OsString = iter.next().expect(
                "clap's external_subcommand invariant: argv is non-empty when \
                 the External variant is constructed",
            );
            let verb = verb_os.to_str().ok_or_else(|| {
                anyhow::anyhow!("external verb name is not valid UTF-8: {:?}", verb_os)
            })?;
            let plugin_args: Vec<OsString> = iter.collect();

            // Resolution: always attempt, but treat failure differently
            // depending on whether an explicit addressing flag was given.
            //
            // - Explicit-flag case (`-C` or `-w`): the named target must
            //   exist. A stale path or unregistered name is an rwv error
            //   before any exec — a plugin cannot salvage a wrong address.
            //   (`-w` already errored out above if the registry lookup
            //   failed; `-C` validated existence but workspace containment
            //   still needs to succeed.)
            // - No-flag case: resolution failure is tolerated (soft
            //   fallthrough). Some plugins legitimately run outside a
            //   workspace (`--help`, generators). The plugin receives the
            //   envelope with `RWV_WORKSPACE`/`RWV_PROJECT` unset —
            //   `RWV_WORKWEAVE` being absent is its signal that no
            //   workspace was resolved.
            //
            // In both cases we attempt resolution so the envelope is set
            // on the child. On success, the full envelope is injected;
            // on soft-fallthrough failure, only `RWV_VERSION` is set.
            let resolution = if cli.cwd_override.is_some() || workweave_flag_project.is_some() {
                // Explicit address: failure is an rwv error before exec.
                let ctx = WorkspaceContext::resolve(&origin_dir, None).with_context(|| {
                    format!(
                        "external verb `{verb}`: could not resolve workspace from {}",
                        origin_dir.display()
                    )
                })?;
                ctx.resolution()
            } else {
                // No flags: soft fallthrough — tolerate resolution failure.
                WorkspaceContext::resolve(&origin_dir, None)
                    .ok()
                    .and_then(|ctx| ctx.resolution())
            };

            // Exec. Never returns on success; on error, the two documented
            // rwv-side surfaces (unknown verb, exec failure). Successful
            // dispatch exits the process from within `dispatch_external`,
            // so the Ok arm is `Infallible` — the empty match is how the
            // type-checker proves that arm is unreachable.
            let never = plugins::dispatch_external(verb, &plugin_args, resolution.as_ref())?;
            match never {}
        }
    }

    Ok(())
}
