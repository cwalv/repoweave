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
use crate::cli::{
    consent, BackupAction, BranchAction, Cli, Commands, MemberAction, SetupAction, WorkweaveAction,
};
use crate::clone;
use crate::explain;
use crate::fetch;
use crate::init;
use crate::lock;
use crate::member_selection::MemberSelection;
use crate::plugins;
use crate::prime;
use crate::pull;
use crate::push;
use crate::setup;
use crate::status;
use crate::sync;
use crate::update;
use crate::update_remote;

use crate::manifest::{ProjectName, WorkweaveName};
use crate::parallel::OutputMode;
use crate::path_spelling::CanonicalPath;
use crate::refusal::{refusal, RefusalKind};
use crate::workspace::{acquire_origin_dir, WorkspaceContext};

use super::operand::{InvocationDir, Operand};
use anyhow::Context;
use clap::{CommandFactory, FromArgMatches};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The `-C <path>` directory, read from the process cwd, which must exist.
///
/// A bare `<project>--<name>` string that matches the workweave name shape but
/// does not exist as a path gets a corrective error pointing at
/// `-w/--workweave`.
fn resolve_cwd_override(operand: Operand, cwd: &InvocationDir) -> anyhow::Result<InvocationDir> {
    let raw = operand.text().to_string_lossy().into_owned();
    let p = operand.locate(cwd);

    // Before trying to canonicalize, check whether this looks like a
    // workweave name (<project>--<name>) that does not exist on disk.
    // That is the most common mistake: passing a workweave identity to
    // -C instead of -w/--workweave.
    if !p.exists() && crate::naming::has_flat_address_shape(&raw) {
        crate::refuse!(
            RefusalKind::WrongAddressFlag,
            "'-C {raw}' looks like a workweave name rather than a path, \
             and no path exists at '{raw}'.\n\
             \n\
             To address a workweave by name, use -w/--workweave:\n\
             \n  rwv -w {raw} <verb>\n\
             \n\
             To address by path, pass the full path to the workweave directory."
        );
    }

    InvocationDir::resolved(&p)
        .with_context(|| format!("'-C {raw}': path does not exist or cannot be accessed"))
}

/// What `rwv branch set-mode [W] MODE [--target T]` asks for: the last word is
/// the mode, so a branch named like a mode is still a branch when two words
/// are given. A mode that is neither, `--target` with `declared`, and
/// `feature` without `--target` are usage errors, as clap's own are.
fn branch_entries(
    existing: Vec<String>,
    created: Vec<String>,
    pins: Vec<String>,
) -> anyhow::Result<Vec<crate::member_branch::BranchEntry>> {
    use crate::member_branch::{BranchEntry, Creation};
    let entries = existing
        .iter()
        .map(|spec| BranchEntry::parse(spec, Creation::Existing))
        .chain(
            created
                .iter()
                .map(|spec| BranchEntry::parse(spec, Creation::New)),
        )
        .collect::<anyhow::Result<Vec<_>>>()?;
    BranchEntry::pinned(entries, &pins)
}

fn spelling_of(target: Option<String>, declared: bool) -> Option<crate::switch::Spelling> {
    match (target, declared) {
        (Some(target), _) => Some(crate::switch::Spelling::Target(target)),
        (None, true) => Some(crate::switch::Spelling::Declared),
        (None, false) => None,
    }
}

fn set_mode_request(mut words: Vec<String>, target: Option<String>) -> crate::branch::SetMode {
    use clap::error::ErrorKind;
    let usage = |kind: ErrorKind, message: String| -> ! {
        Cli::command()
            .find_subcommand_mut("branch")
            .and_then(|branch| branch.find_subcommand_mut("set-mode"))
            .expect("`branch set-mode` is a Commands variant")
            .error(kind, message)
            .exit()
    };
    let mode_word = words.pop().expect("clap requires at least one word");
    let branch = words.pop();
    let mode = <crate::records::Mode as clap::ValueEnum>::from_str(&mode_word, false)
        .unwrap_or_else(|_| {
            usage(
                ErrorKind::InvalidValue,
                format!(
                    "`{mode_word}` is not a mode: expected `feature` or `declared`, after the \
                     branch when one is named (`rwv branch set-mode [W] <feature|declared>`)"
                ),
            )
        });
    match (mode, &target) {
        (crate::records::Mode::Feature, None) => usage(
            ErrorKind::MissingRequiredArgument,
            "`feature` needs `--target <T>`, the branch the feature lands into".to_owned(),
        ),
        (crate::records::Mode::Declared, Some(_)) => usage(
            ErrorKind::ArgumentConflict,
            "`declared` takes no `--target`: only a feature lands into a target".to_owned(),
        ),
        _ => {}
    }
    crate::branch::SetMode {
        branch,
        mode,
        target,
    }
}

/// The `-w` address for a `<project>--<name>` argument whose project half was
/// spelled with `/`, or `None` when `raw` is not that mistake.
///
/// Only the project half is re-spelled: a workweave name may not contain `/`
/// at all, so a `/` on the right of the separator is a path and not an address
/// missing its encoding.
fn flat_spelling_of(raw: &str) -> Option<String> {
    let (project, name) = crate::naming::split_at_weave_separator(raw)?;
    let id = crate::manifest::WorkweaveId {
        project: ProjectName::new(project).ok()?,
        name: WorkweaveName::new(name).ok()?,
    };
    let flat = id.flat();
    crate::naming::has_flat_address_shape(flat.as_str()).then(|| flat.to_string())
}

/// Resolve the workweave directory for a `-w <project>--<name>` argument.
///
/// ## Validation
///
/// The argument must carry the separator with something on each side of it,
/// and must not be path-shaped: a `/` or `\`, or an argument that exists on
/// disk, gets a corrective error pointing at `-C`. That is the whole of the
/// form check, and it decides only which refusal to print — where the halves
/// end is not its question, because nothing downstream reads them.
///
/// ## Resolution
///
/// Workspace is located from `workspace_origin` (already resolved from `-C` or
/// process cwd). The address is then **resolved, not decoded**: every
/// `(project, name)` pair the weave's registries record is rendered and the
/// ones spelling `raw` are kept, and the unique match is confirmed against its
/// `.rwv-workweave` marker. A project half nothing recorded therefore reports
/// that no such workweave exists, where reading the project out of the string
/// reported that the operator's typo was not a valid project name.
///
/// ## Return value
///
/// Returns `(workweave_path, project)` — the path to feed to the resolver as
/// origin, and the project of the pair the address resolved to.
fn resolve_workweave_flag(
    raw: &str,
    workspace_origin: &Path,
    command: Option<&Commands>,
) -> anyhow::Result<(PathBuf, ProjectName)> {
    // A path-shaped argument (contains a separator) is a mistake: -C handles
    // path addressing. Give a corrective error rather than a confusing
    // "not found" message.
    if raw.contains('/') || raw.contains('\\') {
        if let Some(flat) = flat_spelling_of(raw) {
            crate::refuse!(
                RefusalKind::WrongAddressFlag,
                "'-w {raw}' contains a path separator. A workweave address renders a \
                 multi-segment project name's `/` as `+`:\n\
                 \n  rwv -w {flat} <verb>\n\
                 \n\
                 To address a workweave by path instead, pass -C the full path to the \
                 workweave's directory — the address is not one."
            );
        }
        crate::refuse!(
            RefusalKind::WrongAddressFlag,
            "'-w {raw}' contains a path separator — it looks like a path, not a workweave name.\n\
             \n\
             To address a workweave by path, use -C with the full path to its directory:\n\
             \n  rwv -C {raw} <verb>\n\
             \n\
             To address by name, pass <project>--<name> with no separators."
        );
    }
    // If the argument exists on disk as a path, the operator probably meant -C.
    if Path::new(raw).exists() {
        crate::refuse!(
            RefusalKind::WrongAddressFlag,
            "'-w {raw}' exists on disk as a path.\n\
             \n\
             To address a workweave by path, use -C:\n\
             \n  rwv -C {raw} <verb>\n\
             \n\
             To address by name, pass <project>--<name> (bare name, no path separators)."
        );
    }

    let (project_str, name_str) =
        crate::naming::split_at_weave_separator(raw).ok_or_else(|| {
            refusal(
                RefusalKind::MalformedWorkweaveAddress,
                format!(
                    "'-w {raw}' is not in the required <project>--<name> form.\n\
                     \n\
                     Provide both the project and the workweave name separated by `--`:\n\
                     \n  rwv -w <project>--<name> <verb>\n\
                     \n\
                     Example:  rwv -w myproj--hotfix land"
                ),
            )
        })?;

    if project_str.is_empty() || name_str.is_empty() {
        crate::refuse!(
            RefusalKind::MalformedWorkweaveAddress,
            "'-w {raw}' is not in the required <project>--<name> form: \
             both the project and name must be non-empty.\n\
             \n\
             Example:  rwv -w myproj--hotfix land"
        );
    }

    // Find the primary workspace root from the workspace_origin path (from -C
    // or process cwd). The registry lives on the primary; look up from there.
    let primary_ctx = WorkspaceContext::resolve_unbound(workspace_origin).with_context(|| {
        format!(
            "'-w {raw}': could not locate a workspace from {}",
            workspace_origin.display()
        )
    })?;
    let primary_root = primary_ctx.primary_path().to_path_buf();

    let recorded = crate::workweave::registered_workweaves(&primary_root);
    let matched = crate::naming::resolve_flat_address(raw, &recorded);

    let crate::manifest::WorkweaveId { project, name } = match matched.as_slice() {
        [id] => id,
        [] => return Err(no_such_workweave(raw, &recorded)),
        ambiguous => {
            let spelled = ambiguous
                .iter()
                .map(|id| format!("  project `{}`, workweave `{}`", id.project, id.name))
                .collect::<Vec<_>>()
                .join("\n");
            crate::refuse!(
                RefusalKind::AmbiguousWorkweaveAddress,
                "'-w {raw}' is ambiguous — {} registered workweaves render that \
                 address:\n{spelled}\n\
                 \n\
                 Address one of them from its own directory with -C, or rename \
                 a project so the two stop spelling the same thing.",
                ambiguous.len(),
            )
        }
    };

    if let Some(recorded) = crate::workweave::recorded_workweave_dir(&primary_root, project, name)?
    {
        if recorded.is_dir() {
            crate::workspace::reconcile_invocation(&recorded, |root| {
                reconcile_policy(command, root)
            })?;
        }
    }

    // The pair is recorded; whether the directory it points at still is a
    // workweave of that project is the marker round-trip's question.
    let workweave_path =
        crate::workweave::resolve_registered_workweave(&primary_root, project, name).with_context(
            || format!("'-w {raw}': registry lookup failed for project `{project}`, name `{name}`"),
        )?;

    match workweave_path {
        Some(path) => Ok((path, project.clone())),
        // Listing `raw` back to the operator as a candidate is what a plain
        // "no such workweave" would do here, since the entry is recorded.
        None => crate::refuse!(
            RefusalKind::StaleRegistryEntry,
            "'-w {raw}' is recorded for project `{project}` as workweave `{name}`, but that \
             registry entry does not round-trip through a `.rwv-workweave` marker — the \
             directory is gone, or is another workspace's.\n\
             \n\
             Run `rwv doctor --fix` to prune or re-adopt the entry, then retry."
        ),
    }
}

/// The `-w` refusal for an address nothing recorded renders.
///
/// The candidate menu is every address the weave can actually be given, which
/// is what makes a mistyped project half legible — it is listed beside the
/// spellings that would have worked, where reading the project out of the
/// string could only report that the typo was not a valid project name.
fn no_such_workweave(raw: &str, recorded: &[crate::manifest::WorkweaveId]) -> anyhow::Error {
    if recorded.is_empty() {
        return crate::refusal::refusal(
            RefusalKind::NoSuchWorkweave,
            format!(
                "no workweave is addressable as `{raw}` (this weave's registries \
                 record no workweaves at all; create one with \
                 `rwv workweave create <name>`)"
            ),
        );
    }
    let known = recorded
        .iter()
        .map(|id| id.flat().to_string())
        .collect::<Vec<_>>()
        .join("\n  ");
    crate::refusal::refusal(
        RefusalKind::NoSuchWorkweave,
        format!(
            "no workweave is addressable as `{raw}`.\n\
             \n\
             Registered workweaves:\n  {known}\n\
             \n\
             Run `rwv doctor` if a workweave exists on disk but is missing from the registry."
        ),
    )
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
/// When `-w` addressed the workweave,
/// the resolved context's provenance is re-branded from `Marker` to
/// `WorkweaveFlag` before the target-line check runs, so the explicit
/// `-w` addressing form is correctly recorded in the chain-step field
/// and the target line is suppressed (it only fires for `.rwv-active`
/// fall-throughs, never for explicit addressing).
///
/// Every verb resolved here writes into its project, so it also refuses when
/// `.rwv-active` would send it somewhere other than the project whose
/// directory CWD is inside.
///
/// Wrapping resolve + emit in one call keeps the per-verb dispatch site to
/// a single line and makes it impossible to forget the surfacing on a new
/// project-scoped verb.
fn resolve_project_scoped(
    origin_dir: &Path,
    project_override: Option<crate::manifest::ProjectName>,
    binding: Binding,
) -> anyhow::Result<WorkspaceContext> {
    bind_project_scoped(
        WorkspaceContext::resolve_invocation(origin_dir, project_override)?,
        binding,
    )
}

fn bind_project_scoped(
    ctx: WorkspaceContext,
    binding: Binding,
) -> anyhow::Result<WorkspaceContext> {
    ctx.refuse_cwd_in_inactive_project()?;
    refuse_unsettled_lock(&ctx, binding)?;
    Ok(with_target_surfaced(ctx, binding.workweave_flag))
}

/// How a project-scoped verb binds to its project: whether `-w` addressed it,
/// whether the verb writes the project repo or its lock, and whether it
/// commits the project repo.
#[derive(Clone, Copy)]
struct Binding {
    workweave_flag: bool,
    writes_project: bool,
    commits_project: bool,
}

/// Whether `command` writes the project repo or its lock, and so refuses a
/// lock merge driver this release did not set up before it writes anything.
fn writes_project(command: Option<&Commands>) -> bool {
    match command {
        Some(Commands::Member { .. })
        | Some(Commands::Lock { .. })
        | Some(Commands::Sync { .. })
        | Some(Commands::Land { .. })
        | Some(Commands::CherryPick { .. })
        | Some(Commands::Pull { .. }) => true,
        Some(Commands::Update { remote, .. }) => *remote,
        Some(Commands::Branch { action }) => !matches!(action, BranchAction::List { .. }),
        Some(Commands::Workweave {
            action: Some(action),
            ..
        }) => matches!(
            action,
            WorkweaveAction::Create { .. } | WorkweaveAction::Delete { .. }
        ),
        _ => false,
    }
}

fn continues(
    command: Option<&Commands>,
) -> Option<(
    crate::op_state::OpVerb,
    Option<crate::manifest_conflict::Side>,
)> {
    use crate::op_state::OpVerb;
    match command? {
        Commands::Sync {
            do_continue: true,
            keep,
            ..
        } => Some((OpVerb::Sync, *keep)),
        Commands::Land {
            do_continue: true,
            keep,
            ..
        } => Some((OpVerb::Land, *keep)),
        Commands::Pull {
            do_continue: true,
            keep,
            ..
        } => Some((OpVerb::Pull, *keep)),
        _ => None,
    }
}

/// Whether `command` commits the project repo. Each operation's own
/// `--continue` is not counted: it concludes the operation that stopped.
fn commits_project(command: Option<&Commands>) -> bool {
    match command {
        Some(Commands::Member { .. }) => true,
        Some(Commands::Lock { commit, .. }) => *commit,
        Some(Commands::Sync { do_continue, .. })
        | Some(Commands::Land { do_continue, .. })
        | Some(Commands::CherryPick { do_continue, .. })
        | Some(Commands::Pull { do_continue, .. }) => !do_continue,
        Some(Commands::Update { remote, .. }) => *remote,
        Some(Commands::Branch { action }) => !matches!(action, BranchAction::List { .. }),
        _ => false,
    }
}

/// Refuse while the project's lock is unmerged, and, for a verb that writes
/// the project repo or its lock, while its lock merge driver is not set up as
/// this release sets it up.
fn refuse_unsettled_lock(ctx: &WorkspaceContext, binding: Binding) -> anyhow::Result<()> {
    let Some(project) = ctx.active_project() else {
        return Ok(());
    };
    let dir = crate::workspace::project_dir(ctx.active_path(), project);
    if !dir.is_dir() {
        return Ok(());
    }
    let vcs = crate::vcs::project_vcs();
    let repo = crate::workspace::project_rel_path(project);
    crate::lock_driver::refuse_unresolved(vcs.as_ref(), &repo, &dir)?;
    if binding.commits_project {
        crate::commit_guard::refuse_commit_mid_operation(
            vcs.as_ref(),
            &repo,
            &dir,
            Some(ctx.active_path()),
        )?;
    }
    if binding.writes_project {
        crate::lock_driver::refuse_unmigrated(vcs.as_ref(), &repo, &dir)?;
    }
    Ok(())
}

/// The context of a `workweave` verb: its project chosen as any verb's is,
/// by `--project`, the workweave's marker, or `.rwv-active`.
fn workweave_context(
    origin_dir: &Path,
    project: Option<String>,
    binding: Binding,
) -> anyhow::Result<WorkspaceContext> {
    let project_override = project.map(ProjectName::new).transpose()?;
    resolve_project_scoped(origin_dir, project_override, binding)
}

/// [`resolve_project_scoped`] for `status`, which reports on a weave root
/// carrying both identity files instead of refusing to act through it.
/// `doctor` tolerates that and more, through
/// [`WorkspaceContext::resolve_for_doctor`].
///
/// Reachable only by naming it: every other project-scoped verb resolves
/// through [`resolve_project_scoped`] and refuses.
fn resolve_project_scoped_tolerating_disputed_root(
    origin_dir: &Path,
    project_override: Option<crate::manifest::ProjectName>,
    binding: Binding,
) -> anyhow::Result<WorkspaceContext> {
    let ctx = WorkspaceContext::resolve_invocation_tolerating_disputed_root(
        origin_dir,
        project_override,
    )?;
    refuse_unsettled_lock(&ctx, binding)?;
    Ok(with_target_surfaced(ctx, binding.workweave_flag))
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

/// [`crate::workweave::refuse_symlinked_members`] for the workspace a verb was
/// invoked in, under the project it acts for.
fn refuse_symlinked_members_here(ctx: &WorkspaceContext) -> anyhow::Result<()> {
    match ctx.active_project() {
        Some(project) => crate::workweave::refuse_symlinked_members(ctx.active_path(), project),
        None => Ok(()),
    }
}

fn reconcile_policy(
    command: Option<&Commands>,
    root: &Path,
) -> anyhow::Result<crate::workspace::Policy> {
    Ok(
        if destroys_workweave(command) || resumes_a_retire(command, root)? {
            crate::workspace::Policy::Refuse
        } else {
            crate::workspace::Policy::Restore
        },
    )
}

/// Whether `command` continues, at `root`, an op that retires the workweave:
/// `--retire` is not repeated with `--continue`, so the op record says so.
fn resumes_a_retire(command: Option<&Commands>, root: &Path) -> anyhow::Result<bool> {
    if !matches!(
        command,
        Some(Commands::Land {
            do_continue: true,
            ..
        })
    ) {
        return Ok(false);
    }
    Ok(crate::op_state::resolve_to_owner(root)?.is_some_and(|owner| owner.record.retire))
}

/// Whether `command` removes the workweave it runs in, or one it names.
fn destroys_workweave(command: Option<&Commands>) -> bool {
    matches!(
        command,
        Some(Commands::Land { retire: true, .. })
            | Some(Commands::Workweave {
                action: Some(WorkweaveAction::Delete { .. }),
                ..
            })
    )
}

/// Run the requested verb and render whatever it declined with.
///
/// The single funnel every refusal leaves through, which is what lets a
/// refusal's route line be printed once and in one shape. Verbs that exit
/// through `std::process::exit` bypass it deliberately and own their own
/// stdout; nothing else may print an error and return one.
pub fn run_and_report() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(err) if crate::exit_status::is_remainder(&err) => {
            std::process::ExitCode::from(crate::exit_status::COMPLETED_WITH_REMAINDER)
        }
        Err(err) => {
            let (verb, output, unpresented) = invocation();
            let unpresented =
                unpresented.or_else(|| crate::refusal::choices_spelled_for().cloned());
            crate::refusal::report(&err, &verb, output, unpresented.as_ref());
            std::process::ExitCode::FAILURE
        }
    }
}

/// The project `--project` names, where the workspace the invocation
/// resolves to presents another: the choices printed for it must name it
/// too.
fn unpresented_project(at: &clap::ArgMatches) -> Option<ProjectName> {
    let named = at.try_get_one::<String>("project").ok().flatten()?;
    let named = ProjectName::new(named).ok()?;
    let root = RESOLVED_ORIGIN
        .get()?
        .ancestors()
        .find(|dir| crate::workspace::is_workspace_root(dir))?;
    crate::shared_members::outgoing(root, &named).map(|_| named)
}

/// The directory the invocation resolved its workspace from, `-C` and `-w`
/// applied, once dispatch has resolved it.
static RESOLVED_ORIGIN: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// The subcommand path this process was invoked with — `workweave delete` for
/// `rwv workweave delete <name>` — and the output mode it asked for,
/// read again from argv: `run` takes none, and a parse it already passed
/// passes here the same way.
fn invocation() -> (String, OutputMode, Option<ProjectName>) {
    let Ok(matches) = Cli::command().try_get_matches_from(std::env::args_os()) else {
        return (String::new(), OutputMode::Text, None);
    };
    let mut path: Vec<String> = Vec::new();
    let mut at = &matches;
    while let Some((name, sub)) = at.subcommand() {
        path.push(name.to_owned());
        at = sub;
    }
    let json = matches!(at.try_get_one::<bool>("json"), Ok(Some(true)));
    (
        path.join(" "),
        OutputMode::from_flag(json),
        unpresented_project(at),
    )
}

/// Whether the verb reads or writes a repository, and so needs the VCS tools
/// its backends drive. One that does not still runs where no usable tool is
/// installed, which is where an operator reads why the others refuse. A plugin
/// is its own program: rwv reads no repository to start it, and the plugin
/// meets whatever tools it needs itself.
fn drives_repositories(command: Option<&Commands>) -> bool {
    match command {
        Some(
            Commands::Explain { .. }
            | Commands::Completions { .. }
            | Commands::Setup { .. }
            | Commands::External(_),
        ) => false,
        None
        | Some(
            Commands::Activate { .. }
            | Commands::Materialize { .. }
            | Commands::Prime { .. }
            | Commands::Resolve
            | Commands::Clone { .. }
            | Commands::Init { .. }
            | Commands::Member { .. }
            | Commands::Branch { .. }
            | Commands::Switch { .. }
            | Commands::Workweave { .. }
            | Commands::Doctor { .. }
            | Commands::Lock { .. }
            | Commands::Status { .. }
            | Commands::Log { .. }
            | Commands::Abort { .. }
            | Commands::Backup { .. }
            | Commands::Sync { .. }
            | Commands::Land { .. }
            | Commands::CherryPick { .. }
            | Commands::Fetch { .. }
            | Commands::Pull { .. }
            | Commands::Update { .. }
            | Commands::Push { .. },
        ) => true,
    }
}

/// Whether clap refused the parse over a `--force` that the verb does not
/// declare.
fn rejects_force(err: &clap::Error) -> bool {
    err.kind() == clap::error::ErrorKind::UnknownArgument
        && err
            .get(clap::error::ContextKind::InvalidArg)
            .map(ToString::to_string)
            .is_some_and(|arg| arg == "--force" || arg.starts_with("--force="))
}

/// Parse `argv` and run the requested verb. The whole of `rwv`'s behavior
/// hangs off this call; `main.rs` does nothing but forward to it.
pub fn run() -> anyhow::Result<()> {
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
    let (cli, spelling) = match cmd.try_get_matches() {
        Ok(matches) => (
            Cli::from_arg_matches(&matches).unwrap_or_else(|e| e.exit()),
            super::during_op::spelling(&Cli::command(), &matches),
        ),
        Err(err) => {
            if rejects_force(&err) {
                crate::refuse!(
                    RefusalKind::InapplicableFlag,
                    "`--force` is not a flag of this verb, and rwv has no blanket force: each \
                     refusal names the flag that consents to the one loss it would cause. Run \
                     the verb without `--force` and read the refusal it prints."
                );
            }
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

    if drives_repositories(cli.command.as_ref()) {
        crate::vcs::refuse_unready_tools()?;
    }

    if let Some(Commands::Lock {
        merge_driver: Some(files),
        ..
    }) = &cli.command
    {
        let [base, ours, theirs] = files.as_slice() else {
            unreachable!("clap takes exactly three values for --merge-driver")
        };
        let dir = acquire_origin_dir()?;
        return match crate::lock_driver::run(base, ours, theirs, &dir)? {
            crate::lock_driver::DriverOutcome::Merged => Ok(()),
            crate::lock_driver::DriverOutcome::Conflicted => std::process::exit(1),
        };
    }

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
    let cwd = InvocationDir::resolved(&acquire_origin_dir()?)
        .context("failed to resolve the current directory")?;
    crate::workspace::record_typed_from(cwd.as_path());
    let cwd_overridden = cli.cwd_override.is_some();
    let workspace_origin = match cli.cwd_override {
        None => cwd,
        Some(operand) => resolve_cwd_override(operand, &cwd)?,
    };

    // Step 2: workweave selection — registry lookup when -w is given.
    // `workweave_flag_project` carries the project parsed from `-w <proj>--<name>`.
    // When absent, dispatch uses `workspace_origin` directly.
    let (invocation, workweave_flag_project): (InvocationDir, Option<ProjectName>) =
        match cli.workweave_flag.as_deref() {
            None => (workspace_origin, None),
            Some(raw) => {
                let (ww_path, project) =
                    resolve_workweave_flag(raw, workspace_origin.as_path(), cli.command.as_ref())?;
                let dir = InvocationDir::resolved(&ww_path).with_context(|| {
                    format!("'-w {raw}': failed to resolve {}", ww_path.display())
                })?;
                (dir, Some(project))
            }
        };
    let origin_dir = invocation.as_path();
    let _ = RESOLVED_ORIGIN.set(origin_dir.to_path_buf());
    // `-w` addressing re-brands the resolved context's provenance, so the
    // chain-step record names the flag rather than the marker.
    let use_workweave_flag = workweave_flag_project.is_some();
    let binding = Binding {
        workweave_flag: use_workweave_flag,
        writes_project: writes_project(cli.command.as_ref()),
        commits_project: commits_project(cli.command.as_ref()),
    };

    super::during_op::refuse_where_held(
        &spelling,
        crate::workspace::enclosing_root(origin_dir).as_deref(),
        binding.commits_project,
    )?;

    // Step 3: the root's marker against the index and the worktree relation,
    // before the verb resolves the root it runs in. A verb that destroys the
    // workweave refuses on a disagreement; every other verb repairs it.
    crate::workspace::reconcile_invocation(origin_dir, |root| {
        reconcile_policy(cli.command.as_ref(), root)
    })?;

    if let Some((typed, keep)) = continues(cli.command.as_ref()) {
        let ctx = WorkspaceContext::resolve_invocation(origin_dir, None)?;
        crate::manifest_conflict::continue_at_stop(&ctx, typed, keep)?;
    }

    match cli.command {
        None => {
            let ctx = WorkspaceContext::resolve_invocation(origin_dir, None)?;
            outln!("{}", ctx.display());
        }
        Some(Commands::Activate {
            project,
            update,
            no_materialize,
        }) => {
            // Unbound: activation replaces the pointer and never reads it, so
            // it is also the repair for one that does not hold a project name.
            let ctx = WorkspaceContext::resolve_unbound(origin_dir)?;
            let project = ProjectName::new(project)?;
            let opts = activate::ActivateOptions { no_materialize };
            if update {
                activate::activate_aligning(&project, &ctx, opts)?;
            } else {
                activate::activate_with_options(&project, &ctx, opts)?;
            }
        }
        Some(Commands::Materialize {
            regenerate_drifted,
            adopt_drifted,
            remove_undeclared_links,
        }) => {
            let ctx = WorkspaceContext::resolve_invocation(origin_dir, None)?;
            // clap refuses the two together, so this reads as the exclusive
            // choice it is rather than a precedence.
            let consent = consent::RegenerateDriftedConsent::from_flag(regenerate_drifted)
                .map(consent::DriftConsent::Regenerate)
                .or_else(|| {
                    consent::AdoptDriftedConsent::from_flag(adopt_drifted)
                        .map(consent::DriftConsent::Adopt)
                });
            let undeclared =
                consent::RemoveUndeclaredLinksConsent::from_flag(remove_undeclared_links);
            refuse_symlinked_members_here(&ctx)?;
            activate::materialize(&ctx, consent, undeclared)?;
        }
        Some(Commands::Prime { no_suppress }) => {
            // `prime` tolerates the not-in-a-workspace case (silent unless
            // `--no-suppress`); resolve here and pass `Option<&ctx>` so
            // the handler stays free of resolution logic.
            let ctx = WorkspaceContext::resolve_invocation(origin_dir, None).ok();
            prime::prime(ctx.as_ref(), no_suppress)?;
        }
        Some(Commands::Resolve) => {
            let ctx = WorkspaceContext::resolve_unbound(origin_dir)?;
            outln!("{}", ctx.active_path().display());
        }
        Some(Commands::Clone {
            source,
            allow_non_empty_dir,
            no_reference,
            jobs,
            json,
            no_materialize,
        }) => {
            let jobs = crate::parallel::resolve_jobs(jobs);
            let output = OutputMode::from_flag(json);
            let selection =
                MemberSelection::new(crate::selector::RepoFilter::parse(&[], &[])?, no_reference);
            // The origin dir is the clone target — no invocation-context
            // resolution yet.
            crate::workspace::require_workspace_or_empty(origin_dir, allow_non_empty_dir)?;
            clone::run_clone(
                &source,
                origin_dir,
                &selection,
                jobs,
                output,
                activate::ActivateOptions { no_materialize },
            )?;
        }
        Some(Commands::Init {
            project,
            provider,
            adopt,
            no_materialize,
        }) => {
            // Bootstrap-then-resolve happens inside the handler — see init.rs.
            if adopt {
                init::init_adopt(
                    &project,
                    origin_dir,
                    activate::ActivateOptions { no_materialize },
                )?;
            } else {
                init::init(&ProjectName::new(project)?, provider.as_deref(), origin_dir)?;
            }
        }
        Some(Commands::Member { action }) => match action {
            MemberAction::Add {
                url,
                role,
                new,
                adopt,
                no_recurse_submodules,
                params,
                params_json,
                project,
                no_materialize,
            } => {
                let project_override =
                    project.map(crate::manifest::ProjectName::new).transpose()?;
                let ctx = resolve_project_scoped(origin_dir, project_override, binding)?;
                let activation = activate::ActivateOptions { no_materialize };
                let settings = add_remove::NewMemberSettings {
                    role,
                    no_recurse_submodules,
                };
                if new {
                    let mut request =
                        add_remove::CreationRequest::parse(&url, &params, params_json.as_deref())?;
                    request.locate_path_params(|raw| {
                        Operand::from(std::ffi::OsString::from(raw)).locate(&invocation)
                    })?;
                    add_remove::run_add_new(request, settings, &ctx, activation)?;
                } else if adopt {
                    let path = Operand::from(std::ffi::OsString::from(url)).locate(&invocation);
                    add_remove::run_add_adopt(&path, settings, &ctx, activation)?;
                } else {
                    add_remove::run_add(&url, settings, &ctx, activation)?;
                }
            }
            MemberAction::Remove {
                path,
                delete,
                delete_shared_clone,
                project,
                no_materialize,
            } => {
                let project_override =
                    project.map(crate::manifest::ProjectName::new).transpose()?;
                let ctx = resolve_project_scoped(origin_dir, project_override, binding)?;
                add_remove::run_remove(
                    &path,
                    add_remove::Removal::from_flags(delete, delete_shared_clone)?,
                    &ctx,
                    activate::ActivateOptions { no_materialize },
                )?;
            }
            MemberAction::SetRole {
                repo,
                role,
                project,
                no_materialize,
            } => {
                let project_override =
                    project.map(crate::manifest::ProjectName::new).transpose()?;
                let ctx = resolve_project_scoped(origin_dir, project_override, binding)?;
                refuse_symlinked_members_here(&ctx)?;
                crate::member_settings::run_set_role(
                    &repo,
                    role,
                    &ctx,
                    activate::ActivateOptions { no_materialize },
                )?;
            }
            MemberAction::Pin {
                repo,
                rev,
                commit,
                project,
            } => {
                let project_override =
                    project.map(crate::manifest::ProjectName::new).transpose()?;
                let ctx = resolve_project_scoped(origin_dir, project_override, binding)?;
                refuse_symlinked_members_here(&ctx)?;
                crate::member_pin::run_pin(
                    &repo,
                    &rev,
                    commit.then_some(lock::CommitMessage::Generated),
                    &ctx,
                )?;
            }
            MemberAction::SetBranch {
                repo,
                branch,
                new,
                at,
                url,
                project,
                no_materialize,
            } => {
                let project_override =
                    project.map(crate::manifest::ProjectName::new).transpose()?;
                let ctx = resolve_project_scoped(origin_dir, project_override, binding)?;
                refuse_symlinked_members_here(&ctx)?;
                crate::member_branch::run_set_branch(
                    crate::member_branch::SetBranch {
                        repo: &repo,
                        branch: &branch,
                        new,
                        at: at.as_deref(),
                        url: url.as_deref(),
                    },
                    &ctx,
                    activate::ActivateOptions { no_materialize },
                )?;
            }
            MemberAction::SetUrl {
                repo,
                url,
                project,
                no_materialize,
            } => {
                let project_override =
                    project.map(crate::manifest::ProjectName::new).transpose()?;
                let ctx = resolve_project_scoped(origin_dir, project_override, binding)?;
                refuse_symlinked_members_here(&ctx)?;
                crate::member_url::run_set_url(
                    &repo,
                    &url,
                    &ctx,
                    activate::ActivateOptions { no_materialize },
                )?;
            }
            MemberAction::SetRecurseSubmodules {
                repo,
                recurse,
                project,
                no_materialize,
            } => {
                let project_override =
                    project.map(crate::manifest::ProjectName::new).transpose()?;
                let ctx = resolve_project_scoped(origin_dir, project_override, binding)?;
                refuse_symlinked_members_here(&ctx)?;
                crate::member_settings::run_set_recurse_submodules(
                    &repo,
                    crate::manifest::SubmoduleRecursion::from(recurse),
                    &ctx,
                    activate::ActivateOptions { no_materialize },
                )?;
            }
        },
        Some(Commands::Workweave {
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
                let Some(action) = action else {
                    Cli::command()
                        .find_subcommand_mut("workweave")
                        .expect("`workweave` is a Commands variant")
                        .error(
                            clap::error::ErrorKind::MissingSubcommand,
                            "`--hook-mode` modifies `create`; name the workweave subcommand to run",
                        )
                        .exit();
                };
                match action {
                    WorkweaveAction::List { project } => {
                        let ctx = workweave_context(origin_dir, project, binding)?;
                        let names = crate::workweave::list_workweaves(
                            ctx.primary_path(),
                            ctx.require_active_project_on_disk()?,
                        )?;
                        for n in &names {
                            outln!("{}", n);
                        }
                    }
                    WorkweaveAction::Delete {
                        name,
                        discard_uncommitted,
                        discard_unlanded,
                        delete_remote,
                        delete_remote_unlanded,
                        json,
                        project,
                    } => {
                        let ctx = workweave_context(origin_dir, project, binding)?;
                        let project = ctx.require_active_project_on_disk()?;
                        let deletion = crate::workweave::delete_workweave(
                            ctx.primary_path(),
                            project,
                            &WorkweaveName::new(name)?,
                            None,
                            consent::DiscardUncommittedConsent::from_flag(discard_uncommitted),
                            consent::DiscardUnlandedConsent::from_flag(discard_unlanded),
                            &crate::deletion::RemoteDeletes {
                                landed: delete_remote,
                                unlanded: delete_remote_unlanded,
                            },
                        )?;
                        crate::workweave::report_deletion(
                            &deletion,
                            project,
                            OutputMode::from_flag(json),
                        )?;
                        if deletion.left_something() {
                            return Err(crate::exit_status::CompletedWithRemainder.into());
                        }
                    }
                    WorkweaveAction::Create {
                        name,
                        new_branch,
                        branch,
                        detach,
                        target,
                        declared,
                        set_branch,
                        set_new_branch,
                        pin,
                        at,
                        dir,
                        basedir,
                        project,
                        no_materialize,
                    } => {
                        let ctx = workweave_context(origin_dir, project, binding)?;
                        let primary_root = ctx.primary_path();
                        let project = ctx.require_active_project_on_disk()?;
                        let source_root = ctx.active_path().to_path_buf();
                        let placement = match (dir, basedir) {
                            (Some(dir), _) => {
                                crate::workweave::Placement::Dir(dir.locate(&invocation))
                            }
                            (None, Some(basedir)) => {
                                crate::workweave::Placement::Basedir(basedir.locate(&invocation))
                            }
                            (None, None) => crate::workweave::Placement::DefaultBasedir,
                        };
                        let branch = match (new_branch, branch, detach) {
                            (Some(new), _, _) => crate::workweave::BranchChoice::New(new),
                            (None, Some(open), _) => crate::workweave::BranchChoice::Open(open),
                            (None, None, Some(rev)) => crate::workweave::BranchChoice::Detach(rev),
                            (None, None, None) => crate::workweave::BranchChoice::Minted,
                        };
                        let created =
                            crate::workweave::create_workweave(crate::workweave::CreateWorkweave {
                                primary_root,
                                source_root: &source_root,
                                project,
                                name: &WorkweaveName::new(name)?,
                                branch,
                                spelling: spelling_of(target, declared),
                                at,
                                entries: branch_entries(set_branch, set_new_branch, pin)?,
                                placement: &placement,
                                activation: activate::ActivateOptions { no_materialize },
                            });
                        let workweave_path = if hook_mode {
                            crate::workweave::hand_over_seat(created)?
                        } else {
                            created?
                        };
                        if hook_mode {
                            outln!("{}", workweave_path.display());
                        }
                    }
                    WorkweaveAction::SetDefaultCreateBasedir { path, project } => {
                        let ctx = workweave_context(origin_dir, project, binding)?;
                        let project = ctx.require_active_project_on_disk()?;
                        let recorded = crate::workweave_index::set_basedir(
                            ctx.primary_path(),
                            project,
                            CanonicalPath::of(&path.locate(&invocation)),
                        )?;
                        eprintln!(
                            "recorded the default workweave directory for project `{}`: {}",
                            project.as_str(),
                            recorded.as_path().display()
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
            abandon_lost_pins,
            release_unreadable_claims,
            release_unreadable_claims_on,
            merge_edited_drift,
            set_aside_edited_drift,
            kind,
            project,
        }) => {
            let project_override = project.map(crate::manifest::ProjectName::new).transpose()?;
            let ctx = with_target_surfaced(
                WorkspaceContext::resolve_for_doctor(origin_dir, project_override)?,
                binding.workweave_flag,
            );
            let abandon_lost_pins = consent::AbandonLostPinsConsent::from_flag(abandon_lost_pins);
            let release_unreadable_claims = consent::ReleaseUnreadableClaimsConsent::from_flags(
                release_unreadable_claims,
                release_unreadable_claims_on,
            );
            let edited_drift = consent::MergeEditedDriftConsent::from_flag(merge_edited_drift)
                .map(consent::EditedDriftConsent::Merge)
                .or(
                    consent::SetAsideEditedDriftConsent::from_flag(set_aside_edited_drift)
                        .map(consent::EditedDriftConsent::SetAside),
                );
            // Validated at the boundary: an unknown kind refuses here,
            // before any scan runs, naming the valid set.
            let kind_filter = if kind.is_empty() {
                None
            } else {
                Some(check::KindFilter::new(&kind)?)
            };
            if fix {
                ctx.refuse_cwd_in_inactive_project()?;
            }
            let scope = check::Scope::from_all_flag(all);
            let verdict = if locked {
                check::run_check_locked(&ctx)?
            } else if json {
                check::run_check_json(&ctx, scope, kind_filter.as_ref())?
            } else {
                check::run_check(
                    &ctx,
                    check::Repair::from_fix_flag(fix),
                    scope,
                    abandon_lost_pins,
                    release_unreadable_claims,
                    edited_drift,
                    kind_filter.as_ref(),
                )?
            };
            if verdict == check::Verdict::Fail {
                std::process::exit(1);
            }
        }
        Some(Commands::Lock {
            dirty,
            repos,
            commit,
            message,
            project,
            merge_driver: _,
        }) => {
            let project_override = project.map(crate::manifest::ProjectName::new).transpose()?;
            let ctx = resolve_project_scoped(origin_dir, project_override, binding)?;
            refuse_symlinked_members_here(&ctx)?;
            let commit = commit.then(|| lock::CommitMessage::from_flag(message));
            let acting = crate::selector::RepoFilter::parse(&[], &repos)?;
            lock::lock(&ctx, &acting, dirty, commit)?;
        }
        Some(Commands::Status {
            json,
            project,
            selector,
        }) => {
            let project_override = project.map(crate::manifest::ProjectName::new).transpose()?;
            let ctx = resolve_project_scoped_tolerating_disputed_root(
                origin_dir,
                project_override,
                binding,
            )?;
            // A project whose manifest does not load is status's to warn about
            // and skip, not to refuse over.
            let manifest_loads = ctx.active_project().is_none_or(|project| {
                crate::manifest::Manifest::from_path(
                    &crate::workspace::project_dir(ctx.active_path(), project)
                        .join(crate::manifest::Manifest::FILE_NAME),
                )
                .is_ok()
            });
            if manifest_loads {
                refuse_symlinked_members_here(&ctx)?;
            }
            let filter = selector.parse_filter()?;
            status::run_status(&ctx, OutputMode::from_flag(json), &filter)?;
        }
        Some(Commands::Log {
            range,
            against,
            diff,
            json,
            project,
        }) => {
            let project_override = project.map(crate::manifest::ProjectName::new).transpose()?;
            let ctx = resolve_project_scoped_tolerating_disputed_root(
                origin_dir,
                project_override,
                binding,
            )?;
            let query = match against {
                Some(weave_branch) => crate::log::LogQuery::Against { weave_branch, diff },
                None => crate::log::LogQuery::Range(range),
            };
            crate::log::run(&ctx, &query, OutputMode::from_flag(json))?;
        }
        Some(Commands::Abort {
            abandon_foreign_tip,
            leave_target,
            discard_unreadable_record,
        }) => {
            let ctx = WorkspaceContext::resolve_unbound(origin_dir)?;
            if discard_unreadable_record {
                sync::run_discard_unreadable_record(&ctx)?;
            } else {
                sync::run_abort(
                    &ctx,
                    &crate::cli::consent::AbandonForeignTipConsent::from_flag(
                        &abandon_foreign_tip,
                        leave_target,
                    ),
                )?;
            }
        }
        Some(Commands::Branch { action }) => match action {
            BranchAction::List { json, project } => {
                let project_override =
                    project.map(crate::manifest::ProjectName::new).transpose()?;
                let ctx = resolve_project_scoped(origin_dir, project_override, binding)?;
                crate::branch::run_list(&ctx, OutputMode::from_flag(json))?;
            }
            BranchAction::Delete {
                branch,
                target,
                discard_unlanded,
                delete_remote,
                delete_remote_unlanded,
                json,
                project,
            } => {
                let project_override =
                    project.map(crate::manifest::ProjectName::new).transpose()?;
                let ctx = resolve_project_scoped(origin_dir, project_override, binding)?;
                crate::branch::run_delete(
                    &ctx,
                    crate::branch::Delete {
                        branch,
                        target,
                        discard_unlanded: consent::DiscardUnlandedConsent::from_flag(
                            discard_unlanded,
                        ),
                        remote_deletes: crate::deletion::RemoteDeletes {
                            landed: delete_remote,
                            unlanded: delete_remote_unlanded,
                        },
                    },
                    OutputMode::from_flag(json),
                )?;
            }
            BranchAction::Create {
                branch,
                target,
                declared,
                set_branch,
                set_new_branch,
                pin,
                at,
                project,
            } => {
                let project_override =
                    project.map(crate::manifest::ProjectName::new).transpose()?;
                let ctx = resolve_project_scoped(origin_dir, project_override, binding)?;
                crate::switch::run_create(
                    &ctx,
                    crate::switch::Create {
                        branch,
                        spelling: spelling_of(target, declared),
                        at,
                        entries: branch_entries(set_branch, set_new_branch, pin)?,
                    },
                )?;
            }
            BranchAction::Rename {
                new,
                adopt,
                project,
            } => {
                let project_override =
                    project.map(crate::manifest::ProjectName::new).transpose()?;
                let ctx = resolve_project_scoped(origin_dir, project_override, binding)?;
                crate::branch::run_rename(&ctx, crate::branch::Rename { new, adopt })?;
            }
            BranchAction::SetHistory {
                history,
                unset: _,
                repo,
                project,
            } => {
                let project_override =
                    project.map(crate::manifest::ProjectName::new).transpose()?;
                let ctx = resolve_project_scoped(origin_dir, project_override, binding)?;
                refuse_symlinked_members_here(&ctx)?;
                crate::set_history::run(&ctx, crate::set_history::SetHistory { history, repo })?;
            }
            BranchAction::SetMode {
                words,
                target,
                project,
            } => {
                let request = set_mode_request(words, target);
                let project_override =
                    project.map(crate::manifest::ProjectName::new).transpose()?;
                let ctx = resolve_project_scoped(origin_dir, project_override, binding)?;
                crate::branch::run_set_mode(&ctx, request)?;
            }
        },
        Some(Commands::Switch {
            branch,
            detach,
            project,
        }) => {
            let request = match (branch, detach) {
                (Some(branch), None) => crate::switch::Switch::Branch(branch),
                (None, Some(rev)) => crate::switch::Switch::Detach(rev),
                _ => Cli::command()
                    .find_subcommand_mut("switch")
                    .expect("`switch` is a Commands variant")
                    .error(
                        clap::error::ErrorKind::MissingRequiredArgument,
                        "name the branch to switch to, or `--detach <rev>`; aligning members \
                         for the checked-out branch is `rwv update`",
                    )
                    .exit(),
            };
            let project_override = project.map(crate::manifest::ProjectName::new).transpose()?;
            let ctx = resolve_project_scoped(origin_dir, project_override, binding)?;
            crate::switch::run_switch(&ctx, request)?;
        }
        Some(Commands::Backup { action }) => match action {
            BackupAction::List { json, project } => {
                let project_override =
                    project.map(crate::manifest::ProjectName::new).transpose()?;
                let ctx = resolve_project_scoped(origin_dir, project_override, binding)?;
                crate::backup::run_list(&ctx, OutputMode::from_flag(json))?;
            }
            BackupAction::Drop {
                repo,
                names,
                all,
                project,
            } => {
                let project_override =
                    project.map(crate::manifest::ProjectName::new).transpose()?;
                let ctx = resolve_project_scoped(origin_dir, project_override, binding)?;
                let dropping = if all {
                    crate::backup::Dropping::All
                } else {
                    crate::backup::Dropping::Named(names)
                };
                crate::backup::run_drop(&ctx, &repo, &dropping)?;
            }
            BackupAction::Prune { project } => {
                let project_override =
                    project.map(crate::manifest::ProjectName::new).transpose()?;
                let ctx = resolve_project_scoped(origin_dir, project_override, binding)?;
                crate::backup::run_prune(&ctx)?;
            }
        },
        Some(Commands::Sync {
            keep: _,
            from,
            target,
            reset_to_target,
            history,
            json,
            jobs,
            project,
            do_continue,
        }) => {
            let project_override = project.map(crate::manifest::ProjectName::new).transpose()?;
            let ctx = resolve_project_scoped(origin_dir, project_override, binding)?;
            let run = if do_continue {
                crate::feature_sync::Run::Continue
            } else {
                crate::feature_sync::Run::Fresh(crate::feature_sync::Args {
                    from,
                    target,
                    reset_to_target,
                    history,
                    jobs: jobs.map_or(1, |n| crate::parallel::resolve_jobs(Some(n))),
                })
            };
            crate::feature_sync::run_sync(&ctx, run, OutputMode::from_flag(json))?;
        }
        Some(Commands::CherryPick {
            rev,
            mainline,
            except,
            json,
            project,
            do_continue,
        }) => {
            let project_override = project.map(crate::manifest::ProjectName::new).transpose()?;
            let ctx = resolve_project_scoped(origin_dir, project_override, binding)?;
            let run = match (do_continue, rev) {
                (false, Some(rev)) => crate::cherry_pick::Run::Fresh(crate::cherry_pick::Args {
                    rev,
                    mainline,
                    except,
                }),
                _ => crate::cherry_pick::Run::Continue,
            };
            crate::cherry_pick::run_cherry_pick(&ctx, run, OutputMode::from_flag(json))?;
        }
        Some(Commands::Land {
            keep: _,
            target,
            allow_member_change,
            history,
            retire,
            json,
            jobs,
            project,
            do_continue,
        }) => {
            let project_override = project.map(crate::manifest::ProjectName::new).transpose()?;
            let ctx = resolve_project_scoped(origin_dir, project_override.clone(), binding)?;
            let request = if do_continue {
                sync::SyncRequest::Continue {
                    op: Box::new(crate::op_state::resume(ctx.active_path())?),
                    project_override,
                    allow_member_change,
                }
            } else {
                crate::branch::require_recorded(&ctx)?;
                sync::SyncRequest::Fresh {
                    landing: Box::new(crate::land::resolve(
                        &ctx,
                        target.as_deref(),
                        allow_member_change,
                        retire,
                    )?),
                    history,
                    retire,
                    jobs: jobs.map_or(1, |n| crate::parallel::resolve_jobs(Some(n))),
                }
            };
            sync::run_land(&ctx, request, OutputMode::from_flag(json))?;
        }
        Some(Commands::Push {
            project,
            dry_run,
            branch,
            overwrite,
            roles,
            repos,
            jobs,
            json,
        }) => {
            let project_override = project.map(crate::manifest::ProjectName::new).transpose()?;
            let ctx = resolve_project_scoped(origin_dir, project_override, binding)?;
            let filter = crate::selector::RepoFilter::parse(&roles, &repos)?;
            let jobs = match jobs {
                Some(n) => crate::parallel::resolve_jobs(Some(n)),
                None => 1,
            };
            let overwrite = consent::OverwriteConsent::from_flag(&overwrite);
            push::run_push(
                &ctx,
                push::PushRequest {
                    mode: if dry_run {
                        push::PushMode::DryRun
                    } else {
                        push::PushMode::Publish
                    },
                    filter: &filter,
                    overwrite: &overwrite,
                    branch: branch.as_deref(),
                    jobs,
                    output: OutputMode::from_flag(json),
                },
            )?;
        }
        Some(Commands::Fetch {
            no_reference,
            selector,
            jobs,
            json,
            project,
        }) => {
            let project_override = project.map(crate::manifest::ProjectName::new).transpose()?;
            let ctx = resolve_project_scoped(origin_dir, project_override, binding)?;
            let selection = MemberSelection::new(selector.parse_filter()?, no_reference);
            fetch::run_fetch(
                &ctx,
                &selection,
                crate::parallel::resolve_jobs(jobs),
                OutputMode::from_flag(json),
            )?;
        }
        Some(Commands::Pull {
            keep: _,
            merge_upstream,
            reset_to_upstream,
            do_continue,
            project,
            selector,
            jobs,
            json,
            no_materialize,
        }) => {
            let project_override = project.map(crate::manifest::ProjectName::new).transpose()?;
            let ctx = resolve_project_scoped(origin_dir, project_override, binding)?;
            let selection = MemberSelection::new(selector.parse_filter()?, false);
            pull::run_pull(
                &ctx,
                &selection,
                crate::parallel::resolve_jobs(jobs),
                OutputMode::from_flag(json),
                activate::ActivateOptions { no_materialize },
                if do_continue {
                    pull::Take::Continue
                } else {
                    pull::Take::Fresh(crate::merge_upstream::Flags {
                        merge_upstream,
                        reset_to_upstream,
                    })
                },
            )?;
        }
        Some(Commands::Update {
            remote,
            merge_upstream,
            reset_to_upstream,
            do_continue,
            dirty,
            commit,
            message,
            project,
            selector,
            jobs,
            json,
            no_materialize,
        }) => {
            let project_override = project.map(crate::manifest::ProjectName::new).transpose()?;
            let ctx = if remote {
                resolve_project_scoped(origin_dir, project_override, binding)?
            } else {
                // A bare update selects the weave's only project when none is
                // active, so a missing selection is not yet a refusal.
                let ctx = WorkspaceContext::resolve_invocation(origin_dir, project_override)
                    .with_context(|| {
                        format!(
                            "rwv update: no repoweave workspace found above {}",
                            origin_dir.display(),
                        )
                    })?;
                bind_project_scoped(ctx, binding)?
            };
            let jobs = crate::parallel::resolve_jobs(jobs);
            let output = OutputMode::from_flag(json);
            let opts = activate::ActivateOptions { no_materialize };
            if remote {
                refuse_symlinked_members_here(&ctx)?;
                let commit = commit.then(|| lock::CommitMessage::from_flag(message));
                update_remote::run_update_remote(
                    &ctx,
                    dirty,
                    commit,
                    output,
                    &update_remote::Selection::parse(&selector.roles, &selector.repos)?,
                    jobs,
                    opts,
                    if do_continue {
                        update_remote::Take::Continue
                    } else {
                        update_remote::Take::Fresh(
                            crate::merge_upstream::Flags {
                                merge_upstream,
                                reset_to_upstream,
                            },
                            update_remote::Wire {
                                roles: selector.roles.clone(),
                                repos: selector.repos.clone(),
                                no_materialize,
                            },
                        )
                    },
                )?;
            } else {
                update::run_update(
                    &ctx,
                    &MemberSelection::new(selector.parse_filter()?, false),
                    jobs,
                    output,
                    opts,
                )?;
            }
        }
        Some(Commands::Completions { shell }) => {
            let mut cmd = Cli::command();
            let mut script = Vec::new();
            clap_complete::generate(shell, &mut cmd, "rwv", &mut script);
            crate::stdout::write_bytes(&script)
                .context("failed to write the completions to stdout")?;
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
            // clap routes any subcommand it does not recognize here. The
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
                crate::untokened!(
                    ArgvShim,
                    "external verb name is not valid UTF-8: {:?}",
                    verb_os
                )
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
            //   workspace (`--help`, generators). The plugin receives an
            //   envelope carrying `RWV_VERSION` and nothing else, so
            //   `RWV_WORKSPACE` being absent is its signal that no workspace
            //   was resolved. `RWV_WORKWEAVE` is absent at the primary too
            //   and answers a different question.
            //
            // In both cases we attempt resolution so the envelope is set
            // on the child. On success, the full envelope is injected;
            // on soft-fallthrough failure, only `RWV_VERSION` is set.
            let resolution = if cwd_overridden || workweave_flag_project.is_some() {
                // Explicit address: failure is an rwv error before exec.
                let ctx =
                    WorkspaceContext::resolve_invocation(origin_dir, None).with_context(|| {
                        format!(
                            "external verb `{verb}`: could not resolve workspace from {}",
                            origin_dir.display()
                        )
                    })?;
                Some(ctx.resolution())
            } else {
                // No flags: soft fallthrough — tolerate resolution failure.
                WorkspaceContext::resolve_invocation(origin_dir, None)
                    .ok()
                    .map(|ctx| ctx.resolution())
            };

            // Exec. Never returns on success; on error, the two documented
            // rwv-side surfaces (unknown verb, exec failure). Successful
            // dispatch exits the process from within `dispatch_external`,
            // so the Ok arm is `Infallible` — the empty match is how the
            // type-checker proves that arm is unreachable.
            let never =
                plugins::dispatch_external(verb, &plugin_args, origin_dir, resolution.as_ref())?;
            match never {}
        }
    }

    Ok(())
}
