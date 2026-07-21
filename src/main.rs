use repoweave::activate;
use repoweave::add_remove;
use repoweave::check;
use repoweave::cli::{Cli, Commands, SetupAction, WorkweaveAction};
use repoweave::explain;
use repoweave::fetch;
use repoweave::init;
use repoweave::lock;
use repoweave::prime;
use repoweave::push;
use repoweave::setup;
use repoweave::status;
use repoweave::sync;
use repoweave::update;

use anyhow::Context;
use clap::{CommandFactory, Parser};
use repoweave::manifest::WorkweaveName;
use repoweave::workspace::{acquire_origin_dir, WorkspaceContext};

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

fn main() -> anyhow::Result<()> {
    // Early-dispatch did-you-mean hints for removed/relocated flags.
    // These run before clap's full parse so we can produce a friendly error
    // instead of clap's generic "unexpected argument" message.
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
        // Detect: rwv workweave <PROJECT> <WORD> where WORD is a bare token that
        // is neither a known subcommand nor a flag. clap consumes <PROJECT> as
        // the `[PROJECT]` positional, then sees WORD as an *unexpected argument*
        // for the outer `workweave` command (it never reaches the subcommand
        // recognition path), so its generic message is "unexpected argument" and
        // it can't offer a "did you mean". Reframe it as a missing-subcommand
        // error with a create-shaped suggestion. See rwv-b2z / the CLI UX audit.
        //
        // Guards (audit §5 design note):
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
        //     (`fo-city`, edit distance >= 6 from every action) is far from any
        //     subcommand and still gets the create-shaped reframe. See rwv-b2z
        //     review.
        if raw_args.get(1).map(|s| s.as_str()) == Some("workweave") {
            let project = raw_args.get(2).map(|s| s.as_str());
            let word = raw_args.get(3).map(|s| s.as_str());
            let is_flag = |s: &str| s.starts_with('-');
            if let (Some(project), Some(word)) = (project, word) {
                const KNOWN_SUBCOMMANDS: &[&str] =
                    &["create", "delete", "list", "log", "set-container", "help"];
                // WorkweaveAction names a typo could be aiming at. `help` is a
                // clap builtin, not a typo target worth fuzzy-matching, so it's
                // excluded here (an exact `help` is already handled above).
                const SUBCOMMAND_ACTIONS: &[&str] =
                    &["create", "delete", "list", "log", "set-container"];
                // Edit-distance threshold below which WORD is treated as a
                // subcommand typo and deferred to clap's native suggestion.
                const SUBCOMMAND_TYPO_THRESHOLD: usize = 2;
                let near_subcommand = SUBCOMMAND_ACTIONS
                    .iter()
                    .any(|sub| levenshtein(word, sub) <= SUBCOMMAND_TYPO_THRESHOLD);
                if !is_flag(project)
                    && !is_flag(word)
                    && !KNOWN_SUBCOMMANDS.contains(&word)
                    && !near_subcommand
                {
                    eprintln!(
                        "error: '{word}' is not a valid subcommand for 'rwv workweave {project}'\n\
                         Did you mean:  rwv workweave {project} create {word}\n\
                         Available subcommands: create, delete, list, log, set-container"
                    );
                    std::process::exit(2);
                }
            }
        }
    }

    // Suppress the "For more information, try '--help'" footer on clap errors
    // when `--help`/`-h` is already present in the invocation: re-advising the
    // flag the user just typed is noise. We can't hook this cleanly in clap 4
    // (no stable on-error footer override in derive mode), so detect `--help`
    // in the raw args and, on a clap error, re-render it with that one footer
    // line filtered out. Non-help invocations keep clap's default error path.
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
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
    let origin_dir = acquire_origin_dir()?;

    match cli.command {
        None => {
            let ctx = WorkspaceContext::resolve(&origin_dir, None)?;
            println!("{}", ctx.display());
        }
        Some(Commands::Activate {
            project,
            no_install,
        }) => {
            let ctx = WorkspaceContext::resolve(&origin_dir, None)?;
            activate::activate_with_options(
                &project,
                &ctx,
                activate::ActivateOptions { no_install },
            )?;
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
            let project_override = project.map(repoweave::manifest::ProjectName::new);
            let ctx = WorkspaceContext::resolve(&origin_dir, project_override)?;
            if new {
                add_remove::run_add_new(&url, &ctx)?;
            } else {
                add_remove::run_add(&url, role, &ctx)?;
            }
        }
        Some(Commands::Fetch {
            source,
            frozen,
            force,
            no_reference,
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
            let filter = repoweave::selector::RepoFilter::parse(&roles, &repos)?;
            // fetch's default is auto-resolve (min(nproc, 8)), unlike sync which
            // defaults to serial to preserve envelope vs NDJSON contract. fetch's
            // JSON contract follows the same shape: envelope when -j 1 or no -j
            // with default resolution, NDJSON when -j > 1. Because the default
            // can resolve to > 1 on multi-core hosts, agents should pass `-j 1`
            // explicitly when they require the envelope shape.
            let jobs = repoweave::parallel::resolve_jobs(jobs);
            match source {
                Some(src) => {
                    // SOURCE-mode: bootstrap a project into the workspace (or
                    // an empty directory under --force). The origin dir is the
                    // bootstrap target — no invocation-context resolution yet.
                    repoweave::workspace::require_workspace_or_empty(&origin_dir, force)?;
                    fetch::run_fetch(&src, &origin_dir, mode, no_reference, &filter, jobs, json)?;
                }
                None => {
                    // In-place mode: no SOURCE, no --force needed (in-place
                    // requires a workspace). Re-materialize missing manifest
                    // members of the active project. `--force` is a bootstrap
                    // knob (non-empty non-workspace directory); it has no
                    // meaning in-place, so reject it to keep the UX honest.
                    if force {
                        anyhow::bail!(
                            "rwv fetch: --force has no effect without SOURCE; \
                             pass a SOURCE to bootstrap into a non-empty directory, \
                             or drop --force to re-materialize missing members in place"
                        );
                    }
                    let ctx = WorkspaceContext::resolve(&origin_dir, None).with_context(|| {
                        format!(
                            "rwv fetch: no SOURCE and no repoweave workspace found above {}",
                            origin_dir.display(),
                        )
                    })?;
                    fetch::run_fetch_in_place(&ctx, mode, no_reference, &filter, jobs, json)?;
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
            force,
            project,
        }) => {
            let project_override = project.map(repoweave::manifest::ProjectName::new);
            let ctx = WorkspaceContext::resolve(&origin_dir, project_override)?;
            add_remove::run_remove(&path, delete, force, &ctx)?;
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
                repoweave::workweave::handle_claude_hook()?;
            } else {
                let project = project.expect("project is required unless --claude-hook is set");
                let project = repoweave::manifest::ProjectName::new(project);
                let ctx = WorkspaceContext::resolve(&origin_dir, None)?;
                let primary_root = ctx.primary_path();

                match action {
                    Some(WorkweaveAction::List) | None => {
                        let names = repoweave::workweave::list_workweaves(primary_root, &project)?;
                        for n in &names {
                            println!("{}", n);
                        }
                    }
                    Some(WorkweaveAction::Delete { name, force }) => {
                        repoweave::workweave::delete_workweave(
                            primary_root,
                            &project,
                            &WorkweaveName::new(name),
                            force,
                        )?;
                    }
                    Some(WorkweaveAction::Create {
                        name,
                        force,
                        from,
                        capture_dirty,
                        worktree_references,
                        dir,
                    }) => {
                        let source_root = match from.as_deref() {
                            None => ctx.active_path().to_path_buf(),
                            Some("primary") => primary_root.to_path_buf(),
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
                        let workweave_path = repoweave::workweave::create_workweave(
                            primary_root,
                            &source_root,
                            &project,
                            &WorkweaveName::new(name),
                            force,
                            capture_dirty,
                            worktree_references,
                            dir_override,
                        )?;
                        if hook_mode {
                            println!("{}", workweave_path.display());
                        }
                    }
                    Some(WorkweaveAction::Log { diff, json }) => {
                        repoweave::workweave::workweave_log(&ctx, diff, json)?;
                    }
                    Some(WorkweaveAction::SetContainer { path }) => {
                        let raw = std::path::PathBuf::from(&path);
                        let abs = if raw.is_absolute() {
                            raw
                        } else {
                            primary_root.join(&raw)
                        };
                        let canonical = abs.canonicalize().unwrap_or(abs);
                        repoweave::workweave_index::set_container(
                            primary_root,
                            &project,
                            canonical.clone(),
                        )?;
                        // Best-effort: keep the machine-local index out of VCS.
                        let _ =
                            repoweave::workweave_index::ensure_ignore_entry(primary_root, &project);
                        eprintln!(
                            "recorded workweave container for project `{}`: {}",
                            project.as_str(),
                            canonical.display()
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
            project,
        }) => {
            let project_override = project.map(repoweave::manifest::ProjectName::new);
            let ctx = WorkspaceContext::resolve(&origin_dir, project_override)?;
            if locked {
                let has_drift = check::run_check_locked(&ctx)?;
                if has_drift {
                    std::process::exit(1);
                }
            } else if json {
                let has_errors = check::run_check_json(&ctx, all)?;
                if has_errors {
                    std::process::exit(1);
                }
            } else {
                let has_errors = check::run_check(&ctx, fix, all)?;
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
            let project_override = project.map(repoweave::manifest::ProjectName::new);
            let ctx = WorkspaceContext::resolve(&origin_dir, project_override)?;
            lock::lock(&ctx, dirty, commit)?;
        }
        Some(Commands::Status { json, project }) => {
            let project_override = project.map(repoweave::manifest::ProjectName::new);
            let ctx = WorkspaceContext::resolve(&origin_dir, project_override)?;
            status::run_status(&ctx, json)?;
        }
        Some(Commands::Abort) => {
            let ctx = WorkspaceContext::resolve(&origin_dir, None)?;
            sync::run_abort(&ctx)?;
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
            let project_override = project.map(repoweave::manifest::ProjectName::new);
            let ctx = WorkspaceContext::resolve(&origin_dir, project_override.clone())?;
            // sync's default is serial (jobs=1). This differs from fetch/update
            // (which auto-resolve to min(nproc, 8)) because sync's `--json`
            // contract pins envelope output under `-j 1` and NDJSON under
            // `-j > 1`; defaulting to auto would silently switch envelope ->
            // NDJSON on multi-core hosts. No `-j` or `-j 1` emits the
            // pretty envelope.
            let jobs = match jobs {
                Some(n) => repoweave::parallel::resolve_jobs(Some(n)),
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
            let project_override = project.map(repoweave::manifest::ProjectName::new);
            let ctx = WorkspaceContext::resolve(&origin_dir, project_override.clone())?;
            let jobs = match jobs {
                Some(n) => repoweave::parallel::resolve_jobs(Some(n)),
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
                            repoweave::workspace::Checkout::Workweave { dir, .. } => {
                                let marker = repoweave::workspace::WorkweaveMarker::read(dir)?
                                    .ok_or_else(|| {
                                        anyhow::anyhow!(
                                            "bare `rwv sync-to` requires a \
                                                 `.rwv-workweave` marker in the workweave; \
                                                 found none at {}",
                                            dir.display()
                                        )
                                    })?;
                                // Replace the raw `failed to canonicalize …
                                // (os error 2)` a dangling parent would
                                // otherwise produce with friendly
                                // doctor-remediation text.
                                sync::check_parent_not_dangling(
                                    &marker.parent,
                                    ctx.primary_path(),
                                )?;
                                sync::SyncSource::Path(marker.parent)
                            }
                            repoweave::workspace::Checkout::Primary { .. } => {
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
            let project_override = project.map(repoweave::manifest::ProjectName::new);
            let ctx = WorkspaceContext::resolve(&origin_dir, project_override)?;
            let filter = repoweave::selector::RepoFilter::parse(&roles, &repos)?;
            // push's default is serial (jobs=1). This differs from fetch/update
            // (which auto-resolve to min(nproc, 8)) because push's `--json`
            // contract pins envelope output under `-j 1` and NDJSON under
            // `-j > 1`; defaulting to auto would silently switch envelope ->
            // NDJSON on multi-core hosts.
            let jobs = match jobs {
                Some(n) => repoweave::parallel::resolve_jobs(Some(n)),
                None => 1,
            };
            push::run_push(&ctx, dry_run, force, &filter, jobs, json)?;
        }
        Some(Commands::Update {
            dirty,
            commit,
            project,
            roles,
            repos,
            jobs,
            json,
        }) => {
            let project_override = project.map(repoweave::manifest::ProjectName::new);
            let ctx = WorkspaceContext::resolve(&origin_dir, project_override)?;
            let filter = repoweave::selector::RepoFilter::parse(&roles, &repos)?;
            // Update's default is auto-parallel (min(nproc, 8)). The envelope/NDJSON
            // split mirrors sync: -j 1 (or unspecified with --json) emits the
            // envelope; -j > 1 streams NDJSON. Note: unlike sync, update defaults
            // to auto-parallel even without --json, so --json + no -j will default
            // to multi-worker NDJSON on multi-core machines. Callers that want the
            // envelope must pass `-j 1` explicitly alongside --json.
            let jobs = repoweave::parallel::resolve_jobs(jobs);
            update::run_update(&ctx, dirty, commit, json, &filter, jobs)?;
        }
        Some(Commands::Completions { shell }) => {
            let mut cmd = Cli::command();
            clap_complete::generate(shell, &mut cmd, "rwv", &mut std::io::stdout());
        }
        Some(Commands::Explain { command }) => {
            explain::explain(command.as_deref())?;
        }
        Some(Commands::Setup { action }) => match action {
            SetupAction::AgentsMd => {
                let ctx = WorkspaceContext::resolve(&origin_dir, None)?;
                setup::agents_md(&ctx)?;
            }
            SetupAction::Claude { uninstall } => {
                if uninstall {
                    setup::claude_uninstall()?;
                } else {
                    setup::claude()?;
                }
            }
        },
    }

    Ok(())
}
