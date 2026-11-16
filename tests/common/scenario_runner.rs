use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

use repoweave::diagnosis::State;
use repoweave::refusal::RefusalKind;

use super::scenario_invariants::{
    self, Allowance, LockSite, Record, RemoteWrite, Snapshot, Violation,
};
use super::scenario_sandbox::{
    Machine, Place, Remote, RemoteKind, Repo, Sandbox, Seed, Unreachable,
};
use crate::common::pause::Paused;

/// What the owner of a remote does to it from outside every machine — a forge
/// merging or deleting a branch, a colleague pushing — performed as plain git
/// in a clone of the forge's own, or in the bare remote itself where no push
/// can do it.
#[derive(Debug, Clone)]
pub enum Forge {
    Commit {
        repo: Repo,
        branch: &'static str,
        mark: &'static str,
    },
    /// A commit of `files` on `branch`, pushed.
    Files {
        repo: Repo,
        branch: &'static str,
        files: &'static [(&'static str, &'static str)],
    },
    Merge {
        repo: Repo,
        branch: &'static str,
        how: MergeHow,
    },
    DeleteBranch {
        repo: Repo,
        branch: &'static str,
    },
    /// The origin's default branch follows a rename of it, as a forge's
    /// rename does; no change when the default was another branch.
    DefaultFollowsRename {
        repo: Repo,
        from: &'static str,
        to: &'static str,
    },
    /// Both mirrors of `repo` take the origin's branches and tags, the
    /// pruning one dropping those the origin lacks.
    SyncMirrors {
        repo: Repo,
    },
}

impl Forge {
    fn writes(&self) -> Vec<RemoteWrite> {
        match self {
            Forge::Commit { repo, branch, .. } => {
                vec![RemoteWrite::Push(
                    Remote::origin(*repo),
                    (*branch).to_owned(),
                )]
            }
            Forge::Files { repo, branch, .. } => {
                vec![RemoteWrite::Push(
                    Remote::origin(*repo),
                    (*branch).to_owned(),
                )]
            }
            Forge::Merge { repo, .. } => {
                vec![RemoteWrite::Push(Remote::origin(*repo), "main".to_owned())]
            }
            Forge::DeleteBranch { repo, branch } => {
                vec![RemoteWrite::Delete(
                    Remote::origin(*repo),
                    (*branch).to_owned(),
                )]
            }
            Forge::DefaultFollowsRename { .. } => Vec::new(),
            Forge::SyncMirrors { repo } => vec![
                RemoteWrite::Every(Remote::keeping_mirror(*repo)),
                RemoteWrite::Every(Remote::pruning_mirror(*repo)),
            ],
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum MergeHow {
    Merge,
    Squash,
}

/// Where a printed choice stands on its line.
#[derive(Debug, Clone, Copy)]
pub enum Printed {
    /// The whole line.
    Exactly,
    /// The line begins with it.
    Starting,
    /// The line ends with it, after what the line says.
    Within,
    /// The line quotes it in backticks, among what the line says.
    Quoted,
}

#[derive(Debug, Clone)]
pub enum Action {
    Git {
        at: Place,
        repo: Repo,
        args: Vec<String>,
    },
    /// `git remote add <name>` in the checkout, at `remote`'s url.
    AddRemote {
        at: Place,
        repo: Repo,
        name: &'static str,
        remote: Remote,
    },
    /// Write `<mark>.txt` and commit it with message `<mark>`, recording the
    /// commit under that mark for claims to name.
    Commit {
        at: Place,
        repo: Repo,
        mark: &'static str,
    },
    /// Write `content` to `file` and commit it with message `<mark>`,
    /// recording the commit under that mark.
    CommitFile {
        at: Place,
        repo: Repo,
        mark: &'static str,
        file: &'static str,
        content: &'static str,
    },
    /// Replace the one occurrence of `from` in a tracked file with `to`, as
    /// an operator editing it by hand would.
    HandEdit {
        at: Place,
        repo: Repo,
        file: &'static str,
        from: String,
        to: String,
    },
    /// Commit `files` over `branch`'s tip without a working tree; see
    /// [`Sandbox::hand_commit`].
    HandCommit {
        at: Place,
        repo: Repo,
        branch: String,
        files: Vec<(String, String)>,
    },
    Rwv {
        at: Place,
        args: Vec<String>,
    },
    /// The choice line the last rwv step printed that begins `prefix` and
    /// holds `holding`, run at `at` word for word as the terminal shows it:
    /// by rwv, or by git for a VCS action; each placeholder it leaves for
    /// the operator to choose filled from `fill`.
    RwvPrinted {
        at: Place,
        found: Printed,
        prefix: &'static str,
        holding: &'static str,
        fill: &'static [(&'static str, &'static str)],
    },
    /// The lines of one printed choice, the line of the last rwv step that
    /// begins each of `prefixes`, run in that order as printed; the step
    /// stops at the first that fails.
    RwvPrintedChoice {
        at: Place,
        prefixes: &'static [&'static str],
    },
    DropLockEntry {
        at: Place,
        project: Repo,
        member: Repo,
    },
    /// Rewrites the project's `rwv.toml` at `at` with its member tables in
    /// `order` and no `[repositories]` header, as a hand edit would leave it.
    ReorderManifest {
        at: Place,
        order: Vec<Repo>,
    },
    /// Removes `member`'s table from the project's `rwv.toml` at `at`, as an
    /// operator settling a conflict by hand would.
    DropManifestEntry {
        at: Place,
        project: Repo,
        member: Repo,
    },
    /// Cut the workspace-state file the project repo at `at` keeps in its
    /// private directory to half its bytes.
    TruncateWorkspaceState {
        at: Place,
    },
    /// Remove the workspace-state file the project repo at `at` keeps in its
    /// private directory.
    DeleteWorkspaceState {
        at: Place,
    },
    /// An edit to one of rwv's own records, made behind its back.
    Tamper(Tamper),
    /// An empty weave root on `machine`, filled by `rwv clone` from `proj`'s
    /// remote.
    NewMachine(Machine),
    /// `rwv clone` of another project's remote into an existing weave.
    CloneProject(Machine, Repo),
    Forge(Forge),
    /// rwv spawned in the background armed at `point`, the step ending once
    /// it is held there; the process stays held under `handle` until a
    /// [`Action::Release`] names it.
    RwvPaused {
        at: Place,
        args: Vec<String>,
        point: &'static str,
        handle: &'static str,
    },
    /// The held process lets go of its point and runs to its exit, which is
    /// this step's outcome.
    Release(&'static str),
    /// The held process is killed where it stands, as an interrupted run is:
    /// whatever it left behind is what the next step meets.
    Kill(&'static str),
    /// From here, every rwv spawned writes the points it passes to the
    /// sandbox's point trace, until the step that turns it off.
    TracePoints(bool),
    Unreachable(Remote),
    Reachable(Remote),
    /// Every live place's heads, branch tips, working-tree state and
    /// `rwv.toml` and `rwv.lock`, kept under this name for
    /// [`Claim::TipsAsCaptured`].
    Capture(&'static str),
}

impl Action {
    /// Whether the step's outcome is an rwv process's exit.
    pub fn runs_rwv(&self) -> bool {
        matches!(
            self,
            Action::Rwv { .. }
                | Action::RwvPrinted { .. }
                | Action::RwvPrintedChoice { .. }
                | Action::RwvPaused { .. }
                | Action::Release(_)
                | Action::NewMachine(_)
                | Action::CloneProject(..)
        )
    }
}

/// What a scenario does to a record rwv keeps.
#[derive(Debug, Clone)]
pub enum Tamper {
    DeleteMarker(Place),
    /// Overwrite the marker with text no parser reads.
    CorruptMarker(Place),
    /// Point the marker's `parent` at a directory the index does not record.
    RepointMarkerParent(Place),
    /// Overwrite the operation record with text no parser reads.
    CorruptOpRecord(Place),
    /// Rewrite the operation record as a release that stamped no schema
    /// wrote it.
    OldSchemaOpRecord(Place),
    /// Overwrite the op-state lease with text no parser reads.
    CorruptOpLease(Place),
    DeleteIndex(Machine),
    /// Overwrite the index with what `content` holds.
    CorruptIndex(Machine, &'static str),
    /// Write the index as an earlier release kept it: at the top of the
    /// project checkout, `container` for `basedir`, bare paths, an empty
    /// `receipts`, and the container somewhere the current default is not.
    MoveIndexToOldPath(Machine),
}

/// What a claim captured before its step ran.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Captured {
    Checkouts,
    StateFile,
}

fn same_dir(one: &std::path::Path, other: &std::path::Path) -> bool {
    let resolved = |path: &std::path::Path| {
        repoweave::path_spelling::resolve(path).unwrap_or_else(|_| path.to_path_buf())
    };
    resolved(one) == resolved(other)
}

/// Which index file a claim is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexAt {
    /// In the directory the project clone shares with its worktrees.
    Common,
    /// At the top of the project checkout, where earlier releases kept it.
    Old,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    Succeeds,
    /// rwv refused: status 1, the refusal's line printed, and the output
    /// ending in the route lines, this refusal's among them.
    Refuses(Refusal),
    /// The process ended with exactly this status. For rwv it is never 1:
    /// rwv's status 1 is a refusal, and names what refused.
    Code(i32),
    /// `rwv doctor` reported findings: status 1, and a finding line printed.
    Findings,
    Undecided,
}

impl Exit {
    pub fn refuses(repo: Repo, state: State) -> Exit {
        Exit::Refuses(Refusal::Judged(repo, state))
    }

    pub fn refuses_kind(kind: RefusalKind) -> Exit {
        Exit::Refuses(Refusal::Kind(kind))
    }
}

/// What a refusal names: a repo judged in a state of the axis table, or a
/// condition no repo state spells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    Judged(Repo, State),
    Kind(RefusalKind),
}

impl Refusal {
    fn route(self) -> String {
        match self {
            Refusal::Judged(_, state) => format!("rwv explain {}", state.axis().token()),
            Refusal::Kind(kind) => format!("rwv explain {}", kind.token()),
        }
    }

    /// The refusal in `output`: its own line where it judged a repo, and its
    /// route among the route lines that end the output.
    fn printed_in(self, output: &str) -> Result<(), String> {
        if let Refusal::Judged(repo, state) = self {
            judged_line(output, repo, state)?;
        }
        let lines: Vec<&str> = output.lines().filter(|l| !l.trim().is_empty()).collect();
        let routes: Vec<&str> = lines
            .iter()
            .rev()
            .take_while(|l| l.starts_with("rwv explain "))
            .copied()
            .collect();
        let route = self.route();
        if routes.contains(&route.as_str()) {
            Ok(())
        } else {
            Err(format!(
                "the output does not end in the route `{route}`:\n{output}"
            ))
        }
    }
}

/// The line a refusal or a report prints for `repo` judged in `state`:
/// `<repo>: <axis> <state>`, then nothing or the thing it names in
/// parentheses, with the repo optionally placed `in <workspace>`.
fn judged_line(output: &str, repo: Repo, state: State) -> Result<(), String> {
    let state_line = format!(": {} {}", state.axis().token(), state.name());
    let path = repo.rel_path();
    let found = output.lines().any(|line| {
        line.match_indices(&state_line).any(|(at, _)| {
            let head = &line[..at];
            let rest = &line[at + state_line.len()..];
            (rest.is_empty() || rest.starts_with(" ("))
                && (head.ends_with(&path) || head.contains(&format!("{path} in ")))
        })
    });
    if found {
        Ok(())
    } else {
        Err(format!("no line judges {path}{state_line}:\n{output}"))
    }
}

/// The lines of `output` that judge `repo`: its path, standing alone as a
/// word, then `: ` or ` in <workspace>`.
fn lines_judging(output: &str, repo: Repo) -> impl Iterator<Item = &str> {
    let path = repo.rel_path();
    output.lines().filter(move |line| {
        line.match_indices(&path).any(|(at, _)| {
            let before = line[..at].chars().next_back();
            let after = &line[at + path.len()..];
            matches!(before, None | Some(' '))
                && (after.starts_with(": ") || after.starts_with(" in "))
        })
    })
}

/// A fact about the state after a step. Marks name commits recorded by
/// [`Action::Commit`] and [`Forge::Commit`].
#[derive(Debug, Clone)]
pub enum Claim {
    Contains {
        at: Place,
        repo: Repo,
        rev: &'static str,
        mark: &'static str,
    },
    /// The checkout of `repo` at `at` has `rev`, and `rev` does not reach the
    /// commit `mark` names.
    Lacks {
        at: Place,
        repo: Repo,
        rev: &'static str,
        mark: &'static str,
    },
    RemoteContains {
        remote: Remote,
        branch: &'static str,
        mark: &'static str,
    },
    PinContains {
        site: LockSite,
        member: Repo,
        mark: &'static str,
    },
    /// The file `path` at `branch` of `remote` holds `has` when `present`,
    /// else does not.
    RemoteFile {
        remote: Remote,
        branch: &'static str,
        path: &'static str,
        has: &'static str,
        present: bool,
    },
    /// The same of the file `path` at the commit `site` pins `member` at.
    PinFile {
        site: LockSite,
        member: Repo,
        path: &'static str,
        has: &'static str,
        present: bool,
    },
    /// The pin at `site` for `member` does not reach the commit `mark` names.
    PinLacks {
        site: LockSite,
        member: Repo,
        mark: &'static str,
    },
    /// `site` pins `member` at exactly the commit `mark` names.
    PinAt {
        site: LockSite,
        member: Repo,
        mark: &'static str,
    },
    /// The lock of the project commit `rev` at `at` pins `member` at a
    /// commit whose subject is `subject` and that `member`'s checkout at `at`
    /// reaches from `reached_from`: a commit a rebase replayed there, never
    /// the one it replayed.
    PinsReplayed {
        at: Place,
        rev: &'static str,
        member: Repo,
        subject: &'static str,
        reached_from: &'static str,
    },
    WorkweaveGone(Place),
    /// A backup in machine A's primary clone of `repo` reaches the commit
    /// `mark` names, and, where `marked` is given, is marked so.
    BackedUp {
        repo: Repo,
        mark: &'static str,
        marked: Option<&'static str>,
    },
    /// No backup in machine A's primary clone of `repo` reaches the commit
    /// `mark` names.
    NoBackup {
        repo: Repo,
        mark: &'static str,
    },
    /// What the step printed judges `repo` in `state`, as a refusal's or a
    /// report's line does.
    Judged(Repo, State),
    /// What the step printed judges `repo` in `state` and in nothing else:
    /// every line judging it, in any workspace, is that state's.
    JudgedOnly(Repo, State),
    /// What the step printed holds this text.
    OutputContains(&'static str),
    /// What the step printed holds this text exactly once.
    OutputOnce(&'static str),
    /// What the step printed does not hold this text.
    OutputLacks(&'static str),
    /// One line of what the step printed is exactly this text.
    OutputLine(&'static str),
    /// A line of what `rwv workweave list` printed names exactly this
    /// workweave, before the tab that separates it from its path.
    ListsWorkweave(&'static str),
    /// `rwv doctor` printed at least one finding, and every finding it
    /// printed is of this kind.
    FindingsOnly(&'static str),
    /// What the step printed holds each of these texts, each after the one
    /// before it.
    OutputInOrder(&'static [&'static str]),
    /// The checkout of `repo` at `at` has `branch` take in from `remote`'s
    /// same-named branch.
    UpstreamOn {
        at: Place,
        repo: Repo,
        branch: &'static str,
        remote: Remote,
    },
    /// Machine `machine`'s records branch holds exactly this entry for
    /// `branch`: none when it holds none.
    RecordIs {
        machine: Machine,
        branch: &'static str,
        record: Option<Record>,
    },
    /// The records branch of `proj`'s origin holds exactly this entry for
    /// `branch`: none when it holds none.
    RemoteRecordIs {
        branch: &'static str,
        record: Option<Record>,
    },
    /// The origin of `repo` has no branch `branch`.
    RemoteLacks {
        repo: Repo,
        branch: &'static str,
    },
    /// The checkout of `repo` at `at` is at the commit the lock of the project
    /// checked out there pins it at.
    AtPin {
        at: Place,
        repo: Repo,
    },
    /// Every place live when [`Step::capture`] took `name` is as it was then:
    /// each repo's HEAD, working-tree state, `rwv.toml` and `rwv.lock`, and
    /// every branch it had, at the tip it had. A branch made since is not
    /// read. Each repo's tree is as it was too.
    TipsAsCaptured(&'static str),
    /// Every repo's `refs/rwv/backup` is as it was when [`Step::capture`]
    /// took `name`, but for backups whose last path component is one of
    /// `kept` (`<tip>` standing for a twelve-digit abbreviated commit id).
    BackupsAsCaptured {
        name: &'static str,
        kept: &'static [&'static str],
    },
    /// [`Claim::TipsAsCaptured`] without each repo's HEAD, working-tree
    /// state, files and tree: the branches only.
    BranchesAsCaptured(&'static str),
    /// The checkout of `repo` at `at` is on `branch`.
    OnBranch {
        at: Place,
        repo: Repo,
        branch: &'static str,
    },
    /// No branch, HEAD, working-tree change or `rwv.lock` of any checkout at
    /// `at` differs from what it was before the step.
    LocalStateUnchanged(Place),
    /// The weave root at `at` presents `project`: its `.rwv-active` names it.
    Presents {
        at: Place,
        project: Repo,
    },
    /// The marker at `at` names its machine's primary, project `proj`, and
    /// `parent` as the workspace it was forked from.
    MarkerParent {
        at: Place,
        parent: Place,
    },
    /// The index on `machine` lists workweave `name` at its place, forked from
    /// `parent`.
    IndexLists {
        machine: Machine,
        name: &'static str,
        parent: Place,
    },
    /// The index on `machine` records `<machine's directory>/<dir>` as its
    /// basedir, or none when `dir` is none.
    IndexBasedir {
        machine: Machine,
        dir: Option<&'static str>,
    },
    /// Whether an index file is at `at` on `machine`.
    IndexFile {
        machine: Machine,
        at: IndexAt,
        present: bool,
    },
    /// The workspace-state file of the project checkout at `at` has the
    /// identity (inode and modification time) it had before the step.
    WorkspaceStateKept(Place),
    /// The two places keep their workspace state in files of their own.
    WorkspaceStatesApart(Place, Place),
    /// The marker at `at` is as a tamper left it.
    MarkerIs(Place, MarkerState),
    /// `rel`, under machine `machine`'s directory, is a directory.
    DirStands(Machine, &'static str),
    /// Nothing is at `rel` under machine `machine`'s directory.
    DirGone(Machine, &'static str),
    /// The project's `rwv.toml` at `rev` in the checkout at `at` parses and
    /// lists exactly `members`.
    ManifestLists {
        at: Place,
        rev: &'static str,
        members: &'static [Repo],
    },
    /// Each of `members` has a pin the step's landing report says it lands,
    /// in text or in `--json`, and every such pin is the one `site` holds.
    ReportedPinsLanded {
        site: LockSite,
        members: &'static [Repo],
    },
}

/// Whether a commit's subject line is the prescribed message: the whole line,
/// or the message followed by a space or a colon and more text. A body or a
/// trailer on later lines is not in the subject line at all.
pub fn subject_matches(line: &str, prescribed: &str) -> bool {
    line == prescribed
        || line
            .strip_prefix(prescribed)
            .is_some_and(|rest| rest.starts_with([' ', ':']))
}

impl Claim {
    /// The commit marks this claim looks for: a commit it names by the
    /// subject it was made with.
    pub fn marks(&self) -> Vec<&'static str> {
        match self {
            Claim::Contains { mark, .. }
            | Claim::Lacks { mark, .. }
            | Claim::RemoteContains { mark, .. }
            | Claim::PinContains { mark, .. }
            | Claim::PinLacks { mark, .. }
            | Claim::PinAt { mark, .. }
            | Claim::BackedUp { mark, .. }
            | Claim::NoBackup { mark, .. } => vec![mark],
            Claim::PinsReplayed { subject, .. } => vec![subject],
            _ => Vec::new(),
        }
    }
}

/// What is left at a marker's path after a tamper.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkerState {
    Absent,
    Corrupt,
    /// Parses, and names a parent other than the one the index records.
    ParentElsewhere,
}

/// The outcome a step is expected to have. A step whose outcome the adopted
/// design decides carries [`Expect::undecided`] until that design fills it.
#[derive(Debug, Clone)]
pub struct Expect {
    pub exit: Exit,
    pub claims: Vec<Claim>,
}

impl Expect {
    pub fn undecided() -> Expect {
        Expect {
            exit: Exit::Undecided,
            claims: Vec::new(),
        }
    }

    pub fn succeeds() -> Expect {
        Expect {
            exit: Exit::Succeeds,
            claims: Vec::new(),
        }
    }

    pub fn is_decided(&self) -> bool {
        self.exit != Exit::Undecided
    }
}

/// One step of a scenario: what runs, what it may do that the invariants
/// would otherwise report, and its expected outcome. Between a step that
/// holds a process at a pause point and the step that releases it, the held
/// process is blocked, so every step's invariants read a state nothing is
/// changing.
#[derive(Debug, Clone)]
pub struct Step {
    pub label: String,
    pub action: Action,
    pub allow: Allowance,
    pub expect: Expect,
}

impl Step {
    fn new(label: impl Into<String>, action: Action, expect: Expect) -> Step {
        Step {
            label: label.into(),
            action,
            allow: Allowance::default(),
            expect,
        }
    }

    fn forge_git(repo: Repo, args: &[&str]) -> Step {
        Step::new(
            format!("forge: git -C {} {}", repo.name(), args.join(" ")),
            Action::Git {
                at: Place::Forge,
                repo,
                args: args.iter().map(|a| (*a).to_owned()).collect(),
            },
            Expect::succeeds(),
        )
    }

    /// A colleague replaces `branch`'s tip on the origin with a commit
    /// holding the tip's changes and `<mark>.txt`, and force-pushes it.
    pub fn forge_force_push(repo: Repo, branch: &'static str, mark: &'static str) -> Vec<Step> {
        let tracking = format!("origin/{branch}");
        vec![
            Step::forge_git(repo, &["switch", "-C", branch, &tracking]),
            Step::forge_git(repo, &["reset", "--soft", "HEAD~1"]),
            Step::commit(&Place::Forge, repo, mark),
            Step::forge_git(repo, &["push", "--force", "origin", branch])
                .overwrites(Remote::origin(repo), branch),
        ]
    }

    /// The forge deletes `branch` on the origin.
    pub fn forge_delete_branch(repo: Repo, branch: &'static str) -> Step {
        let delete = format!(":refs/heads/{branch}");
        Step::forge_git(repo, &["push", "origin", &delete]).deletes(Remote::origin(repo), branch)
    }

    /// The forge renames `from` to `to` on the origin, moving the default
    /// branch with it when `from` was the default.
    pub fn forge_rename_branch(repo: Repo, from: &'static str, to: &'static str) -> Vec<Step> {
        let create = format!("refs/remotes/origin/{from}:refs/heads/{to}");
        let delete = format!(":refs/heads/{from}");
        vec![
            Step::forge_git(repo, &["push", "origin", &create]).pushes(Remote::origin(repo), to),
            Step::forge(Forge::DefaultFollowsRename { repo, from, to }),
            Step::forge_git(repo, &["push", "origin", &delete]).deletes(Remote::origin(repo), from),
        ]
    }

    /// The forge rebase-merges `branch` into `into`: a copy of each of its
    /// commits replayed onto `into`, which then moves to the last copy.
    pub fn forge_rebase_merge(repo: Repo, branch: &'static str, into: &'static str) -> Vec<Step> {
        let tracking = format!("origin/{branch}");
        let onto = format!("origin/{into}");
        let target = format!("HEAD:refs/heads/{into}");
        vec![
            Step::forge_git(repo, &["switch", "-C", branch, &tracking]),
            Step::forge_git(
                repo,
                &[
                    "-c",
                    "user.name=Forge",
                    "-c",
                    "user.email=forge@example.invalid",
                    "rebase",
                    "--force-rebase",
                    &onto,
                ],
            ),
            Step::forge_git(repo, &["push", "origin", &target]).pushes(Remote::origin(repo), into),
        ]
    }

    pub fn git(at: &Place, repo: Repo, args: &[&str]) -> Step {
        Step::new(
            format!("git -C {} {}", repo.name(), args.join(" ")),
            Action::Git {
                at: at.clone(),
                repo,
                args: args.iter().map(|a| (*a).to_owned()).collect(),
            },
            Expect::succeeds(),
        )
    }

    pub fn add_remote(at: &Place, repo: Repo, name: &'static str, remote: Remote) -> Step {
        Step::new(
            format!("git -C {} remote add {name} ({remote:?})", repo.name()),
            Action::AddRemote {
                at: at.clone(),
                repo,
                name,
                remote,
            },
            Expect::succeeds(),
        )
    }

    pub fn commit(at: &Place, repo: Repo, mark: &'static str) -> Step {
        Step::new(
            format!("commit {mark} in {}", repo.name()),
            Action::Commit {
                at: at.clone(),
                repo,
                mark,
            },
            Expect::succeeds(),
        )
    }

    pub fn commit_file(
        at: &Place,
        repo: Repo,
        mark: &'static str,
        file: &'static str,
        content: &'static str,
    ) -> Step {
        Step::new(
            format!("commit {mark} in {}: {file}", repo.name()),
            Action::CommitFile {
                at: at.clone(),
                repo,
                mark,
                file,
                content,
            },
            Expect::succeeds(),
        )
    }

    pub fn hand_edit(at: &Place, repo: Repo, file: &'static str, from: &str, to: &str) -> Step {
        Step::new(
            format!("hand-edit {} {file}: {from:?} -> {to:?}", repo.name()),
            Action::HandEdit {
                at: at.clone(),
                repo,
                file,
                from: from.to_owned(),
                to: to.to_owned(),
            },
            Expect::succeeds(),
        )
    }

    /// Removes `member`'s entry from the project's `rwv.lock` at `at`, leaving
    /// the change uncommitted.
    pub fn drop_lock_entry(at: &Place, project: Repo, member: Repo) -> Step {
        Step::new(
            format!(
                "drop {}'s entry from {}'s lock",
                member.name(),
                project.name()
            ),
            Action::DropLockEntry {
                at: at.clone(),
                project,
                member,
            },
            Expect::succeeds(),
        )
    }

    /// Lays the project's `rwv.toml` at `at` out as a serializer would, its
    /// member tables first and in `order`, the other tables after; leaves the
    /// change uncommitted.
    pub fn reorder_manifest(at: &Place, order: &[Repo]) -> Step {
        let names: Vec<&str> = order.iter().map(|r| r.name()).collect();
        Step::new(
            format!("hand-reorder proj's rwv.toml: {}", names.join(", ")),
            Action::ReorderManifest {
                at: at.clone(),
                order: order.to_vec(),
            },
            Expect::succeeds(),
        )
    }

    /// Removes `member`'s table from the project's `rwv.toml` at `at`, leaving
    /// the change uncommitted.
    pub fn drop_manifest_entry(at: &Place, project: Repo, member: Repo) -> Step {
        Step::new(
            format!(
                "drop {}'s table from {}'s rwv.toml",
                member.name(),
                project.name()
            ),
            Action::DropManifestEntry {
                at: at.clone(),
                project,
                member,
            },
            Expect::succeeds(),
        )
    }

    pub fn tamper(label: &str, tamper: Tamper) -> Step {
        Step::new(label, Action::Tamper(tamper), Expect::succeeds())
    }

    pub fn truncate_workspace_state(at: &Place) -> Step {
        Step::new(
            "truncate the workspace state",
            Action::TruncateWorkspaceState { at: at.clone() },
            Expect::succeeds(),
        )
    }

    pub fn delete_workspace_state(at: &Place) -> Step {
        Step::new(
            "delete the workspace state",
            Action::DeleteWorkspaceState { at: at.clone() },
            Expect::succeeds(),
        )
    }

    pub fn hand_commit(at: &Place, repo: Repo, branch: &str, files: &[(&str, &str)]) -> Step {
        let paths: Vec<&str> = files.iter().map(|(path, _)| *path).collect();
        Step::new(
            format!(
                "hand-commit {} on {} {branch}",
                paths.join(", "),
                repo.name()
            ),
            Action::HandCommit {
                at: at.clone(),
                repo,
                branch: branch.to_owned(),
                files: files
                    .iter()
                    .map(|(path, content)| ((*path).to_owned(), (*content).to_owned()))
                    .collect(),
            },
            Expect::succeeds(),
        )
    }

    pub fn rwv(at: &Place, args: &[&str]) -> Step {
        Step::new(
            format!("rwv {}", args.join(" ")),
            Action::Rwv {
                at: at.clone(),
                args: args.iter().map(|a| (*a).to_owned()).collect(),
            },
            Expect::undecided(),
        )
    }

    /// The choice the previous rwv step printed beginning `prefix`, run as
    /// printed.
    pub fn rwv_printed(at: &Place, prefix: &'static str) -> Step {
        Step::rwv_printed_holding(at, prefix, "")
    }

    /// [`Step::rwv_printed`] of the line that also holds `holding`, for
    /// choices whose lines begin alike.
    pub fn rwv_printed_holding(at: &Place, prefix: &'static str, holding: &'static str) -> Step {
        Step::new(
            format!("the printed `{prefix}...{holding}...`"),
            Action::RwvPrinted {
                at: at.clone(),
                found: Printed::Starting,
                prefix,
                holding,
                fill: &[],
            },
            Expect::undecided(),
        )
    }

    /// [`Step::rwv_printed_holding`] with each placeholder the line leaves
    /// for the operator to choose filled as `fill` says.
    pub fn rwv_printed_filling(
        at: &Place,
        prefix: &'static str,
        holding: &'static str,
        fill: &'static [(&'static str, &'static str)],
    ) -> Step {
        Step::new(
            format!("the printed `{prefix}...{holding}...`, filled {fill:?}"),
            Action::RwvPrinted {
                at: at.clone(),
                found: Printed::Starting,
                prefix,
                holding,
                fill,
            },
            Expect::undecided(),
        )
    }

    /// The command a printed line holding `holding` ends with, from its
    /// `prefix` on, run as printed.
    pub fn rwv_printed_within(at: &Place, prefix: &'static str, holding: &'static str) -> Step {
        Step::new(
            format!("the printed `...{prefix}...{holding}...`"),
            Action::RwvPrinted {
                at: at.clone(),
                found: Printed::Within,
                prefix,
                holding,
                fill: &[],
            },
            Expect::undecided(),
        )
    }

    /// The command a printed line quotes in backticks, beginning `prefix`
    /// and holding `holding`, run as printed.
    pub fn rwv_printed_quoted(at: &Place, prefix: &'static str, holding: &'static str) -> Step {
        Step::new(
            format!("the quoted `{prefix}...{holding}...`"),
            Action::RwvPrinted {
                at: at.clone(),
                found: Printed::Quoted,
                prefix,
                holding,
                fill: &[],
            },
            Expect::undecided(),
        )
    }

    /// [`Step::rwv_printed`] of the line that is exactly `line`, for a
    /// choice whose words begin another printed line.
    pub fn rwv_printed_exactly(at: &Place, line: &'static str) -> Step {
        Step::new(
            format!("the printed `{line}`"),
            Action::RwvPrinted {
                at: at.clone(),
                found: Printed::Exactly,
                prefix: line,
                holding: "",
                fill: &[],
            },
            Expect::undecided(),
        )
    }

    /// Every line of one printed choice, in order, each beginning the
    /// matching entry of `prefixes`.
    pub fn rwv_printed_choice(at: &Place, prefixes: &'static [&'static str]) -> Step {
        Step::new(
            format!("the printed choice {prefixes:?}"),
            Action::RwvPrintedChoice {
                at: at.clone(),
                prefixes,
            },
            Expect::undecided(),
        )
    }

    pub fn new_machine(machine: Machine) -> Step {
        Step::new(
            format!("fresh clone on {machine:?}"),
            Action::NewMachine(machine),
            Expect::succeeds(),
        )
    }

    pub fn clone_project(machine: Machine, project: Repo) -> Step {
        Step::new(
            format!("rwv clone {} on {machine:?}", project.name()),
            Action::CloneProject(machine, project),
            Expect::succeeds(),
        )
    }

    pub fn forge(forge: Forge) -> Step {
        let writes = forge.writes();
        let mut step = Step::new(
            format!("forge: {forge:?}"),
            Action::Forge(forge),
            Expect::succeeds(),
        );
        step.allow.writes = writes;
        step
    }

    /// rwv run at `at` and held at `point` under `handle`.
    pub fn rwv_paused(
        at: &Place,
        args: &[&str],
        point: &'static str,
        handle: &'static str,
    ) -> Step {
        Step::new(
            format!("rwv {} (held at {point} as {handle})", args.join(" ")),
            Action::RwvPaused {
                at: at.clone(),
                args: args.iter().map(|a| (*a).to_owned()).collect(),
                point,
                handle,
            },
            Expect::succeeds(),
        )
    }

    pub fn release(handle: &'static str) -> Step {
        Step::new(
            format!("release {handle}"),
            Action::Release(handle),
            Expect::undecided(),
        )
    }

    /// Keep every live place's tips for [`Claim::TipsAsCaptured`].
    pub fn capture(name: &'static str) -> Step {
        Step::new(
            format!("capture {name}"),
            Action::Capture(name),
            Expect::succeeds(),
        )
    }

    pub fn kill(handle: &'static str) -> Step {
        Step::new(
            format!("kill {handle}"),
            Action::Kill(handle),
            Expect::succeeds(),
        )
    }

    pub fn trace_points(on: bool) -> Step {
        Step::new(
            format!("trace points {on}"),
            Action::TracePoints(on),
            Expect::succeeds(),
        )
    }

    pub fn unreachable(remote: Remote) -> Step {
        Step::new(
            format!("{remote:?} unreachable"),
            Action::Unreachable(remote),
            Expect::succeeds(),
        )
    }

    pub fn reachable(remote: Remote) -> Step {
        Step::new(
            format!("{remote:?} reachable again"),
            Action::Reachable(remote),
            Expect::succeeds(),
        )
    }

    pub fn expect(mut self, exit: Exit) -> Step {
        self.expect.exit = exit;
        self
    }

    pub fn claim(mut self, claim: Claim) -> Step {
        self.expect.claims.push(claim);
        self
    }

    pub fn may_rewind(mut self, members: &[Repo]) -> Step {
        self.allow.may_rewind.extend_from_slice(members);
        self
    }

    pub fn may_strand(mut self, repos: &[Repo]) -> Step {
        self.allow.may_strand.extend_from_slice(repos);
        self
    }

    /// The step continues a carry or replay whose conflict the operator
    /// resolved by hand: the commit the resolution replaced has no patch
    /// equivalent, so no ref reaches it or one like it.
    pub fn resolved_by_hand(self, repos: &[Repo]) -> Step {
        self.may_strand(repos)
    }

    pub fn may_restore(mut self, repos: &[Repo]) -> Step {
        self.allow.may_restore.extend_from_slice(repos);
        self
    }

    pub fn may_rewrite(mut self, repos: &[Repo]) -> Step {
        self.allow.may_rewrite.extend_from_slice(repos);
        self
    }

    pub fn may_leave_unresolved(mut self, members: &[Repo]) -> Step {
        self.allow.may_leave_unresolved.extend_from_slice(members);
        self
    }

    pub fn pushes(mut self, remote: Remote, branch: &str) -> Step {
        self.allow
            .writes
            .push(RemoteWrite::Push(remote, branch.to_owned()));
        self
    }

    pub fn overwrites(mut self, remote: Remote, branch: &str) -> Step {
        self.allow
            .writes
            .push(RemoteWrite::Overwrite(remote, branch.to_owned()));
        self
    }

    pub fn deletes(mut self, remote: Remote, branch: &str) -> Step {
        self.allow
            .writes
            .push(RemoteWrite::Delete(remote, branch.to_owned()));
        self
    }
}

pub struct Scenario {
    pub name: &'static str,
    pub seed: Seed,
    pub steps: Vec<Step>,
}

#[derive(Debug, Clone)]
pub struct StepRecord {
    pub label: String,
    pub succeeded: bool,
    pub decided: bool,
    pub output: String,
}

#[derive(Debug, Default)]
pub struct Report {
    pub name: &'static str,
    pub ran: Vec<StepRecord>,
    pub violations: Vec<(String, Violation)>,
    pub unmet: Vec<(String, String)>,
    pub marks: BTreeMap<&'static str, String>,
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "scenario {}", self.name)?;
        for record in &self.ran {
            let verdict = if record.succeeded { "ok" } else { "FAILED" };
            writeln!(f, "  [{verdict}] {}", record.label)?;
            if !record.succeeded {
                for line in record.output.lines() {
                    writeln!(f, "        {line}")?;
                }
            }
        }
        for (label, violation) in &self.violations {
            writeln!(f, "  VIOLATION after `{label}`: {violation}")?;
        }
        for (label, why) in &self.unmet {
            writeln!(f, "  UNMET after `{label}`: {why}")?;
        }
        Ok(())
    }
}

impl Report {
    /// Every invariant and every decided outcome held. A step that ran with
    /// no decided outcome is announced as a skip: what it did is unchecked.
    pub fn assert_green(&self) {
        assert!(
            self.violations.is_empty() && self.unmet.is_empty(),
            "expected a green run:\n{self}"
        );
        assert!(!self.ran.is_empty(), "a green run ran no steps:\n{self}");
        self.announce_unchecked();
    }

    fn announce_unchecked(&self) {
        for label in self.undecided() {
            crate::common::report_skip(&format!(
                "scenario {}: `{label}` ran with no decided outcome",
                self.name
            ));
        }
    }

    /// [`Report::assert_green`], and every step carried a decided outcome.
    pub fn assert_fully_decided_green(&self) {
        self.assert_green();
        let undecided = self.undecided();
        assert!(
            undecided.is_empty(),
            "undecided steps {undecided:?}:\n{self}"
        );
    }

    fn undecided(&self) -> Vec<&str> {
        self.ran
            .iter()
            .filter(|r| !r.decided)
            .map(|r| r.label.as_str())
            .collect()
    }
}

struct Run<'a> {
    sandbox: &'a Sandbox,
    last_code: Option<i32>,
    last_rwv_output: String,
    /// Whether the step just performed ran a printed VCS action, which
    /// leaves the rwv output it was printed in to be read again.
    ran_printed_vcs_action: bool,
    marks: BTreeMap<&'static str, String>,
    held: BTreeMap<&'static str, Paused>,
    offline: BTreeMap<Remote, Unreachable<'a>>,
    captured: BTreeMap<&'static str, BTreeMap<Place, Tips>>,
}

/// What [`Claim::TipsAsCaptured`] compares for one place.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Tips {
    exact: String,
    backups: String,
    branches: BTreeMap<String, String>,
}

pub fn run(scenario: Scenario) -> Report {
    let sandbox = Sandbox::new(scenario.seed.clone());
    run_in(&sandbox, scenario)
}

pub fn run_in(sandbox: &Sandbox, scenario: Scenario) -> Report {
    let steps = std::iter::once(Step::new_machine(Machine::A)).chain(scenario.steps);
    run_steps(sandbox, scenario.name, steps)
}

/// Steps run against a sandbox that already holds a finished journey's work,
/// whoever did it: no commit is recorded under a mark, so a claim naming a
/// mark finds the commit by its subject.
pub fn check_in(sandbox: &Sandbox, name: &'static str, steps: Vec<Step>) -> Report {
    run_steps(sandbox, name, steps.into_iter())
}

fn run_steps(sandbox: &Sandbox, name: &'static str, steps: impl Iterator<Item = Step>) -> Report {
    let mut report = Report {
        name,
        ..Report::default()
    };
    let mut run = Run {
        sandbox,
        last_code: None,
        last_rwv_output: String::new(),
        ran_printed_vcs_action: false,
        marks: BTreeMap::new(),
        held: BTreeMap::new(),
        offline: BTreeMap::new(),
        captured: BTreeMap::new(),
    };
    let mut before = scenario_invariants::snapshot(sandbox);
    for step in steps {
        let unmoved: Vec<(Captured, Place, String)> = step
            .expect
            .claims
            .iter()
            .filter_map(|claim| match claim {
                Claim::LocalStateUnchanged(at) => {
                    Some((Captured::Checkouts, at.clone(), run.local_state(at)))
                }
                Claim::WorkspaceStateKept(at) => {
                    Some((Captured::StateFile, at.clone(), run.state_file_identity(at)))
                }
                _ => None,
            })
            .collect();
        run.last_code = None;
        run.ran_printed_vcs_action = false;
        let (succeeded, output) = run.perform(&step.action);
        if step.action.runs_rwv() && !run.ran_printed_vcs_action {
            run.last_rwv_output = output.clone();
        }
        let after = scenario_invariants::snapshot(sandbox);
        for violation in scenario_invariants::check(sandbox, &before, &after, &step.allow) {
            report.violations.push((step.label.clone(), violation));
        }
        let outcome = Outcome {
            by_rwv: step.action.runs_rwv(),
            succeeded,
            code: run.last_code,
            output: &output,
        };
        if let Err(why) = outcome.meets(step.expect.exit) {
            report.unmet.push((step.label.clone(), why));
        }
        if step.action.runs_rwv() {
            if let Some(line) = unbound_placeholder_line(&output) {
                report.unmet.push((
                    step.label.clone(),
                    format!("a printed line holds a placeholder the judgment binds:\n{line}"),
                ));
            }
        }
        for claim in &step.expect.claims {
            if let Err(why) = run.holds(claim, &after, &output, &unmoved) {
                report.unmet.push((step.label.clone(), why));
            }
        }
        report.ran.push(StepRecord {
            label: step.label,
            succeeded,
            decided: step.expect.is_decided(),
            output,
        });
        before = after;
    }
    for handle in std::mem::take(&mut run.held).into_keys() {
        report.unmet.push((
            format!("release {handle}"),
            "the scenario ended with this process still held".to_owned(),
        ));
    }
    report.marks = std::mem::take(&mut run.marks);
    report
}

/// The placeholders a judgment always binds from what it judged: one left
/// in a printed choice is a line nothing filled. The ones the operator
/// chooses (`<T>`, `<W>`, `<b>`, `<rev>`, `<h>`, `<role>`, `<url>`) are not
/// among them.
const BOUND_PLACEHOLDERS: &[&str] = &[
    "<dep>",
    "<m>",
    "<repo>",
    "<checkout>",
    "<target>",
    "<primary>",
    "<other>",
    "<b-checkout>",
    "<changed>",
    "<resume>",
    "<abort>",
];

fn unbound_placeholder_line(output: &str) -> Option<&str> {
    output.lines().find(|line| {
        let line = line.trim_start();
        !line.starts_with("Error:")
            && !line.starts_with("cd ")
            && BOUND_PLACEHOLDERS.iter().any(|p| line.contains(p))
    })
}

/// How a step ended, as its expected [`Exit`] reads it.
pub struct Outcome<'a> {
    pub by_rwv: bool,
    pub succeeded: bool,
    pub code: Option<i32>,
    pub output: &'a str,
}

impl Outcome<'_> {
    pub fn meets(&self, exit: Exit) -> Result<(), String> {
        let output = self.output;
        match exit {
            Exit::Succeeds if !self.succeeded => Err(format!("expected success:\n{output}")),
            Exit::Refuses(refusal) if !self.by_rwv => Err(format!(
                "expected a refusal ({refusal:?}) of a step that runs no rwv"
            )),
            Exit::Refuses(refusal) => {
                if self.code != Some(1) {
                    return Err(format!(
                        "expected a refusal ({refusal:?}), exit status 1; got {:?}:\n{output}",
                        self.code
                    ));
                }
                refusal.printed_in(output)
            }
            Exit::Findings => {
                if self.code != Some(1) {
                    return Err(format!(
                        "expected findings, exit status 1; got {:?}:\n{output}",
                        self.code
                    ));
                }
                if output
                    .lines()
                    .any(|l| l.starts_with("[error: ") || l.starts_with("[warning: "))
                {
                    Ok(())
                } else {
                    Err(format!("expected a finding line:\n{output}"))
                }
            }
            Exit::Code(1) if self.by_rwv => Err(
                "rwv's exit status 1 is a refusal: expect Exit::Refuses naming what refused"
                    .to_owned(),
            ),
            Exit::Code(want) if self.code != Some(want) => Err(format!(
                "expected exit status {want}; got {:?}:\n{output}",
                self.code
            )),
            _ => Ok(()),
        }
    }
}

/// Each pin a landing's report says a member lands at: a named re-pin, a
/// pin entry change, or a relock of its checkout, read from `--json`'s
/// `reports` where the output opens with an envelope, else from its lines.
fn reported_landed_pins(output: &str) -> Vec<(Repo, String)> {
    let member_of = |path: &str| {
        Repo::MEMBERS
            .into_iter()
            .find(|repo| repo.rel_path() == path)
    };
    if output.trim_start().starts_with('{') {
        let envelope = serde_json::Deserializer::from_str(output)
            .into_iter::<serde_json::Value>()
            .next()
            .and_then(Result::ok)
            .unwrap_or_default();
        return envelope["reports"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|report| match report["kind"].as_str() {
                Some("re-pinned" | "relocked") => true,
                Some("entry-changed") => report["field"] == "pin",
                _ => false,
            })
            .filter_map(|report| {
                Some((
                    member_of(report["path"].as_str()?)?,
                    report["now"].as_str()?.to_owned(),
                ))
            })
            .collect();
    }
    output
        .lines()
        .filter_map(|line| {
            let (path, said) = line.trim_start().split_once(": ")?;
            let member = member_of(path)?;
            let pin = if let Some(rest) = said.strip_prefix("re-pinned ") {
                rest.split_whitespace().nth(2)?
            } else if let Some(rest) = said.strip_prefix("pin: ") {
                rest.split_whitespace().nth(2)?
            } else {
                said.split_once("the landing records ")?
                    .1
                    .split_whitespace()
                    .next()?
            };
            Some((member, pin.to_owned()))
        })
        .collect()
}

fn rendered(out: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

impl Run<'_> {
    fn perform(&mut self, action: &Action) -> (bool, String) {
        let sb = self.sandbox;
        match action {
            Action::Git { at, repo, args } => {
                if *at == Place::Forge {
                    self.forge_clone(*repo);
                }
                let args: Vec<&str> = args.iter().map(String::as_str).collect();
                let out = sb.git(&sb.checkout(at, *repo), &args);
                self.last_code = out.status.code();
                (out.status.success(), rendered(&out))
            }
            Action::AddRemote {
                at,
                repo,
                name,
                remote,
            } => {
                let out = sb.git(
                    &sb.checkout(at, *repo),
                    &["remote", "add", name, &sb.remote_url(*remote)],
                );
                (out.status.success(), rendered(&out))
            }
            Action::Commit { at, repo, mark } => {
                if *at == Place::Forge {
                    self.forge_clone(*repo);
                }
                self.commit_in(sb.checkout(at, *repo), mark)
            }
            Action::CommitFile {
                at,
                repo,
                mark,
                file,
                content,
            } => {
                let dir = sb.checkout(at, *repo);
                std::fs::write(dir.join(file), content).unwrap();
                let add = sb.git(&dir, &["add", file]);
                let commit = sb.git(&dir, &["commit", "-m", mark]);
                if !add.status.success() || !commit.status.success() {
                    return (false, format!("{}{}", rendered(&add), rendered(&commit)));
                }
                let sha = sb.git_ok(&dir, &["rev-parse", "HEAD"]);
                self.marks.insert(mark, sha);
                (true, rendered(&commit))
            }
            Action::HandEdit {
                at,
                repo,
                file,
                from,
                to,
            } => {
                let path = sb.checkout(at, *repo).join(file);
                let text = std::fs::read_to_string(&path).unwrap();
                if text.matches(from.as_str()).count() != 1 {
                    return (
                        false,
                        format!("{from:?} is not in {} exactly once", path.display()),
                    );
                }
                // raw lock bytes: a hand edit replaces text in whatever file the step names, a lock included; the edit itself is the subject.
                std::fs::write(&path, text.replacen(from.as_str(), to, 1)).unwrap();
                (true, String::new())
            }
            Action::HandCommit {
                at,
                repo,
                branch,
                files,
            } => {
                let commit = sb.hand_commit(&sb.checkout(at, *repo), branch, files);
                (true, commit)
            }
            Action::Rwv { at, args } => {
                let expanded: Vec<String> = args
                    .iter()
                    .map(|arg| match arg.strip_prefix("url:") {
                        Some(spec) => {
                            let (kind, repo) = spec.split_once(':').expect("url:<kind>:<repo>");
                            let repo = Repo::MEMBERS
                                .into_iter()
                                .find(|m| m.name() == repo)
                                .expect("a member");
                            let remote = match kind {
                                "origin" => Remote::origin(repo),
                                "fork" => Remote::fork(repo),
                                "mirror-keeping" => Remote::keeping_mirror(repo),
                                "mirror-pruning" => Remote::pruning_mirror(repo),
                                other => panic!("no remote kind {other}"),
                            };
                            sb.remote_url(remote)
                        }
                        None => arg.clone(),
                    })
                    .collect();
                let args: Vec<&str> = expanded.iter().map(String::as_str).collect();
                let out = sb.rwv(&sb.place_root(at), &args);
                self.last_code = out.status.code();
                (out.status.success(), rendered(&out))
            }
            Action::RwvPrinted {
                at,
                found,
                prefix,
                holding,
                fill,
            } => {
                let Some(line) =
                    self.last_rwv_output
                        .lines()
                        .map(str::trim)
                        .find_map(|line| match found {
                            Printed::Exactly => (line == *prefix).then_some(line),
                            Printed::Starting => {
                                (line.starts_with(prefix) && line.contains(holding)).then_some(line)
                            }
                            Printed::Within => line
                                .find(prefix)
                                .map(|at| &line[at..])
                                .filter(|command| command.contains(holding)),
                            Printed::Quoted => line.split('`').skip(1).step_by(2).find(|quoted| {
                                quoted.starts_with(prefix) && quoted.contains(holding)
                            }),
                        })
                else {
                    return (
                        false,
                        format!(
                            "no `{prefix}...{holding}...` line was printed:\n{}",
                            self.last_rwv_output
                        ),
                    );
                };
                let line = fill
                    .iter()
                    .fold(line.to_owned(), |line, (placeholder, value)| {
                        line.replace(placeholder, value)
                    });
                let mut words = line.split_whitespace();
                let program = words.next();
                let args: Vec<&str> = words.collect();
                let out = match program {
                    Some("git") => {
                        self.ran_printed_vcs_action = true;
                        sb.git(&sb.place_root(at), &args)
                    }
                    _ => sb.rwv(&sb.place_root(at), &args),
                };
                self.last_code = out.status.code();
                (
                    out.status.success(),
                    format!("ran: {line}\n{}", rendered(&out)),
                )
            }
            Action::RwvPrintedChoice { at, prefixes } => {
                let mut shown = String::new();
                let mut last = None;
                for prefix in *prefixes {
                    let Some(line) = self
                        .last_rwv_output
                        .lines()
                        .map(str::trim)
                        .find(|line| line.starts_with(prefix))
                    else {
                        return (
                            false,
                            format!(
                                "no `{prefix}...` line was printed:\n{}",
                                self.last_rwv_output
                            ),
                        );
                    };
                    let mut words = line.split_whitespace();
                    let program = words.next();
                    let args: Vec<&str> = words.collect();
                    let out = match program {
                        Some("git") => {
                            self.ran_printed_vcs_action = true;
                            sb.git(&sb.place_root(at), &args)
                        }
                        _ => sb.rwv(&sb.place_root(at), &args),
                    };
                    shown.push_str(&format!("ran: {line}\n{}", rendered(&out)));
                    self.last_code = out.status.code();
                    last = Some(out.status.success());
                    if !out.status.success() {
                        break;
                    }
                }
                (last.unwrap_or(false), shown)
            }
            Action::DropLockEntry {
                at,
                project,
                member,
            } => {
                let path = sb.checkout(at, *project).join("rwv.lock");
                let mut lock: serde_json::Value =
                    serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
                let removed = lock["repositories"]
                    .as_object_mut()
                    .unwrap()
                    .remove(&member.rel_path());
                if removed.is_none() {
                    return (
                        false,
                        format!("{} has no entry for {}", path.display(), member.name()),
                    );
                }
                let mut text = serde_json::to_string_pretty(&lock).unwrap();
                text.push('\n');
                // raw lock bytes: a lock with one entry removed, which the builder, taking the entries to write, cannot express over a lock it did not write.
                std::fs::write(&path, text).unwrap();
                (true, String::new())
            }
            Action::ReorderManifest { at, order } => {
                let manifest_path = sb.checkout(at, Repo::Proj).join("rwv.toml");
                let text = std::fs::read_to_string(&manifest_path).unwrap();
                let mut blocks: Vec<(String, String)> = Vec::new();
                for line in text.lines() {
                    if line.starts_with('[') {
                        blocks.push((line.to_owned(), String::new()));
                    }
                    if let Some((_, body)) = blocks.last_mut() {
                        if !line.is_empty() {
                            body.push_str(line);
                            body.push('\n');
                        }
                    }
                }
                let mut tables: Vec<String> = Vec::new();
                for member in order {
                    let header = format!("[repositories.\"{}\"]", member.rel_path());
                    match blocks.iter().find(|(h, _)| *h == header) {
                        Some((_, body)) => tables.push(body.clone()),
                        None => {
                            return (
                                false,
                                format!(
                                    "{} has no table for {}",
                                    manifest_path.display(),
                                    member.name()
                                ),
                            )
                        }
                    }
                }
                tables.extend(
                    blocks
                        .iter()
                        .filter(|(h, _)| !h.starts_with("[repositories"))
                        .map(|(_, body)| body.clone()),
                );
                std::fs::write(&manifest_path, tables.join("\n")).unwrap();
                (true, String::new())
            }
            Action::DropManifestEntry {
                at,
                project,
                member,
            } => {
                let manifest_path = sb.checkout(at, *project).join("rwv.toml");
                let text = std::fs::read_to_string(&manifest_path).unwrap();
                let header = format!("[repositories.\"{}\"]", member.rel_path());
                let mut kept = Vec::new();
                let mut dropping = false;
                let mut found = false;
                for line in text.lines() {
                    if line.starts_with('[') {
                        dropping = line == header;
                        found |= dropping;
                    }
                    if !dropping {
                        kept.push(line);
                    }
                }
                if !found {
                    return (
                        false,
                        format!(
                            "{} has no table for {}",
                            manifest_path.display(),
                            member.name()
                        ),
                    );
                }
                std::fs::write(&manifest_path, format!("{}\n", kept.join("\n"))).unwrap();
                (true, String::new())
            }
            Action::TruncateWorkspaceState { at } => {
                let project = sb.checkout(at, Repo::Proj);
                let private = sb.git_ok(&project, &["rev-parse", "--absolute-git-dir"]);
                let state = std::path::Path::new(&private).join("rwv-workspace-state");
                let bytes = match std::fs::read(&state) {
                    Ok(bytes) => bytes,
                    Err(e) => return (false, format!("{}: {e}", state.display())),
                };
                std::fs::write(&state, &bytes[..bytes.len() / 2]).unwrap();
                (true, String::new())
            }
            Action::DeleteWorkspaceState { at } => {
                let project = sb.checkout(at, Repo::Proj);
                let private = sb.git_ok(&project, &["rev-parse", "--absolute-git-dir"]);
                let state = std::path::Path::new(&private).join("rwv-workspace-state");
                match std::fs::remove_file(&state) {
                    Ok(()) => (true, String::new()),
                    Err(e) => (false, format!("{}: {e}", state.display())),
                }
            }
            Action::Tamper(tamper) => match self.tamper(tamper) {
                Ok(()) => (true, String::new()),
                Err(why) => (false, why),
            },
            Action::NewMachine(machine) => {
                let root = sb.prepare_machine(*machine);
                let fetch = sb.clone_command(Repo::Proj);
                let args: Vec<&str> = fetch.iter().map(String::as_str).collect();
                let out = sb.rwv(&root, &args);
                self.last_code = out.status.code();
                (out.status.success(), rendered(&out))
            }
            Action::CloneProject(machine, project) => {
                let fetch = sb.clone_command(*project);
                let args: Vec<&str> = fetch.iter().map(String::as_str).collect();
                let out = sb.rwv(&sb.weave_root(*machine), &args);
                self.last_code = out.status.code();
                (out.status.success(), rendered(&out))
            }
            Action::Forge(forge) => self.forge(forge),
            Action::RwvPaused {
                at,
                args,
                point,
                handle,
            } => {
                assert!(!self.held.contains_key(handle), "{handle} is already held");
                let args: Vec<&str> = args.iter().map(String::as_str).collect();
                let dir = sb.root.join("pause").join(handle);
                std::fs::create_dir_all(dir.parent().unwrap()).unwrap();
                match Paused::spawn(sb.rwv_command(&sb.place_root(at), &args), point, &dir) {
                    Ok(held) => {
                        self.held.insert(handle, held);
                        (true, String::new())
                    }
                    Err(why) => (false, why),
                }
            }
            Action::Release(handle) => match self.held.remove(handle) {
                Some(held) => {
                    let out = held.release();
                    self.last_code = out.status.code();
                    (out.status.success(), rendered(&out))
                }
                None => (false, format!("nothing is held as {handle}")),
            },
            Action::Kill(handle) => match self.held.remove(handle) {
                Some(held) => {
                    drop(held);
                    (true, String::new())
                }
                None => (false, format!("nothing is held as {handle}")),
            },
            Action::TracePoints(on) => {
                sb.trace_points(*on);
                (true, String::new())
            }
            Action::Capture(name) => {
                let kept = sb
                    .live_places()
                    .into_iter()
                    .map(|place| {
                        let tips = self.tips(&place);
                        (place, tips)
                    })
                    .collect();
                self.persist_capture(name, &kept);
                self.captured.insert(name, kept);
                (true, String::new())
            }
            Action::Unreachable(remote) => {
                assert!(
                    !self.offline.contains_key(remote),
                    "{remote:?} is already unreachable"
                );
                self.offline.insert(*remote, sb.unreachable(*remote));
                (true, String::new())
            }
            Action::Reachable(remote) => {
                let back_online = self
                    .offline
                    .remove(remote)
                    .unwrap_or_else(|| panic!("{remote:?} is not unreachable"));
                drop(back_online);
                (true, String::new())
            }
        }
    }

    fn commit_in(&mut self, dir: PathBuf, mark: &'static str) -> (bool, String) {
        let sb = self.sandbox;
        assert!(
            !self.marks.contains_key(mark),
            "mark {mark} is recorded twice in one scenario"
        );
        std::fs::write(dir.join(format!("{mark}.txt")), format!("{mark}\n")).unwrap();
        let add = sb.git(&dir, &["add", &format!("{mark}.txt")]);
        let commit = sb.git(&dir, &["commit", "-m", mark]);
        if !add.status.success() || !commit.status.success() {
            return (false, format!("{}{}", rendered(&add), rendered(&commit)));
        }
        let sha = sb.git_ok(&dir, &["rev-parse", "HEAD"]);
        self.marks.insert(mark, sha);
        (true, rendered(&commit))
    }

    /// The forge's clone of `repo`'s origin, fetched to the origin's state.
    fn forge_clone(&self, repo: Repo) -> PathBuf {
        let sb = self.sandbox;
        let dir = sb.checkout(&Place::Forge, repo);
        if dir.is_dir() {
            sb.git_ok(&dir, &["fetch", "--prune", "origin"]);
        } else {
            std::fs::create_dir_all(dir.parent().unwrap()).unwrap();
            sb.git_ok(
                dir.parent().unwrap(),
                &[
                    "clone",
                    &sb.remote_url(Remote::origin(repo)),
                    dir.to_str().unwrap(),
                ],
            );
        }
        dir
    }

    fn forge(&mut self, forge: &Forge) -> (bool, String) {
        let sb = self.sandbox;
        match forge {
            Forge::Commit { repo, branch, mark } => {
                let dir = self.forge_clone(*repo);
                let tracking = format!("refs/remotes/origin/{branch}");
                let start = if sb
                    .git(&dir, &["rev-parse", "--verify", &tracking])
                    .status
                    .success()
                {
                    tracking
                } else {
                    "refs/remotes/origin/main".to_owned()
                };
                sb.git_ok(&dir, &["switch", "-C", branch, &start]);
                let result = self.commit_in(dir.clone(), mark);
                let out = sb.git(&dir, &["push", "origin", branch]);
                (result.0 && out.status.success(), result.1 + &rendered(&out))
            }
            Forge::Files {
                repo,
                branch,
                files,
            } => {
                let dir = self.forge_clone(*repo);
                sb.git_ok(
                    &dir,
                    &[
                        "switch",
                        "-C",
                        branch,
                        &format!("refs/remotes/origin/{branch}"),
                    ],
                );
                let owned: Vec<(String, String)> = files
                    .iter()
                    .map(|(path, content)| ((*path).to_owned(), (*content).to_owned()))
                    .collect();
                let commit = sb.hand_commit(&dir, branch, &owned);
                sb.git_ok(&dir, &["reset", "-q", "--hard"]);
                let out = sb.git(&dir, &["push", "origin", branch]);
                (out.status.success(), commit + &rendered(&out))
            }
            Forge::Merge { repo, branch, how } => {
                let dir = self.forge_clone(*repo);
                sb.git_ok(&dir, &["switch", "-C", "main", "origin/main"]);
                let source = format!("origin/{branch}");
                let merged = match how {
                    MergeHow::Merge => sb.git(&dir, &["merge", "--no-ff", "--no-edit", &source]),
                    MergeHow::Squash => {
                        sb.git_ok(&dir, &["merge", "--squash", &source]);
                        sb.git(&dir, &["commit", "-m", &format!("squash {branch}")])
                    }
                };
                let out = sb.git(&dir, &["push", "origin", "main"]);
                (
                    merged.status.success() && out.status.success(),
                    rendered(&merged) + &rendered(&out),
                )
            }
            Forge::DeleteBranch { repo, branch } => {
                let origin = sb.remote_store(Remote::origin(*repo));
                let out = sb.git(&origin, &["branch", "-D", branch]);
                (out.status.success(), rendered(&out))
            }
            Forge::DefaultFollowsRename { repo, from, to } => {
                let origin = sb.remote_store(Remote::origin(*repo));
                let head = sb.git_ok(&origin, &["symbolic-ref", "HEAD"]);
                if head != format!("refs/heads/{from}") {
                    return (true, String::new());
                }
                let out = sb.git(
                    &origin,
                    &["symbolic-ref", "HEAD", &format!("refs/heads/{to}")],
                );
                (out.status.success(), rendered(&out))
            }
            Forge::SyncMirrors { repo } => {
                let origin = sb.remote_url(Remote::origin(*repo));
                let mut synced = String::new();
                for (kind, prune) in [
                    (RemoteKind::KeepingMirror, false),
                    (RemoteKind::PruningMirror, true),
                ] {
                    let mirror = Remote { repo: *repo, kind };
                    let mut args = vec!["fetch", "--quiet"];
                    if prune {
                        args.push("--prune");
                    }
                    args.extend([
                        origin.as_str(),
                        "+refs/heads/*:refs/heads/*",
                        "+refs/tags/*:refs/tags/*",
                    ]);
                    let out = sb.git(&sb.remote_store(mirror), &args);
                    synced += &rendered(&out);
                    if !out.status.success() {
                        return (false, synced);
                    }
                }
                (true, synced)
            }
        }
    }

    fn mark(&self, mark: &str) -> Result<&String, String> {
        self.marks
            .get(mark)
            .ok_or_else(|| format!("no commit is recorded under mark {mark}"))
    }

    fn contains(&self, dir: &std::path::Path, rev: &str, mark: &str) -> Result<(), String> {
        let Some(sha) = self.marks.get(mark) else {
            return self.contains_subject(dir, rev, mark);
        };
        let ok = self
            .sandbox
            .git(dir, &["merge-base", "--is-ancestor", sha, rev])
            .status
            .success();
        if ok {
            Ok(())
        } else {
            Err(format!(
                "{} at {rev} does not contain {mark} ({sha})",
                dir.display()
            ))
        }
    }

    fn contains_subject(
        &self,
        dir: &std::path::Path,
        rev: &str,
        subject: &str,
    ) -> Result<(), String> {
        let out = self.sandbox.git(dir, &["log", "--format=%s", rev]);
        if out.status.success()
            && String::from_utf8_lossy(&out.stdout)
                .lines()
                .any(|line| subject_matches(line, subject))
        {
            Ok(())
        } else {
            Err(format!(
                "{} at {rev} holds no commit whose subject line is or begins with '{subject}'",
                dir.display()
            ))
        }
    }

    /// Every checkout at `at`, as one text: its HEAD, its branches, its
    /// uncommitted changes, and the project's lock bytes.
    fn read_index(&self, machine: Machine) -> Result<serde_json::Value, String> {
        let path = self.sandbox.index_file(machine);
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("no index at {}: {e}", path.display()))?;
        serde_json::from_str(&text)
            .map_err(|e| format!("{} does not parse: {e}\n{text}", path.display()))
    }

    fn state_file_identity(&self, at: &Place) -> String {
        let file = self.sandbox.private_dir(at).join("rwv-workspace-state");
        let Ok(meta) = std::fs::metadata(&file) else {
            return format!("{}: absent", file.display());
        };
        #[cfg(unix)]
        let inode = {
            use std::os::unix::fs::MetadataExt;
            meta.ino()
        };
        #[cfg(not(unix))]
        let inode = 0u64;
        format!(
            "{}: inode {inode}, {} bytes, modified {:?}",
            file.display(),
            meta.len(),
            meta.modified().ok()
        )
    }

    fn tamper(&self, tamper: &Tamper) -> Result<(), String> {
        let sb = self.sandbox;
        let io = |what: &std::path::Path, e: std::io::Error| format!("{}: {e}", what.display());
        match tamper {
            Tamper::DeleteMarker(at) => {
                let marker = sb.place_root(at).join(".rwv-workweave");
                std::fs::remove_file(&marker).map_err(|e| io(&marker, e))
            }
            Tamper::CorruptMarker(at) => {
                let marker = sb.place_root(at).join(".rwv-workweave");
                std::fs::write(&marker, "{ not a marker").map_err(|e| io(&marker, e))
            }
            Tamper::CorruptOpRecord(at) => {
                let record = sb.place_root(at).join(".rwv-op");
                if !record.is_file() {
                    return Err(format!("{}: no operation record", record.display()));
                }
                std::fs::write(&record, "{ not a record").map_err(|e| io(&record, e))
            }
            Tamper::OldSchemaOpRecord(at) => {
                let record = sb.place_root(at).join(".rwv-op");
                let text = std::fs::read_to_string(&record).map_err(|e| io(&record, e))?;
                let mut value: serde_json::Value = serde_json::from_str(&text)
                    .map_err(|e| format!("{}: {e}", record.display()))?;
                value
                    .as_object_mut()
                    .ok_or_else(|| format!("{}: not an object", record.display()))?
                    .remove("schema");
                std::fs::write(&record, serde_json::to_string(&value).unwrap())
                    .map_err(|e| io(&record, e))
            }
            Tamper::CorruptOpLease(at) => {
                let lease = sb.place_root(at).join(".rwv-op-lease");
                if !lease.is_file() {
                    return Err(format!("{}: no lease", lease.display()));
                }
                std::fs::write(&lease, "{ not a lease").map_err(|e| io(&lease, e))
            }
            Tamper::RepointMarkerParent(at) => {
                let marker = sb.place_root(at).join(".rwv-workweave");
                let text = std::fs::read_to_string(&marker).map_err(|e| io(&marker, e))?;
                let mut value: serde_json::Value = serde_json::from_str(&text)
                    .map_err(|e| format!("{}: {e}", marker.display()))?;
                value["parent"] = serde_json::json!(sb.root.join("nowhere"));
                std::fs::write(&marker, serde_json::to_string_pretty(&value).unwrap())
                    .map_err(|e| io(&marker, e))
            }
            Tamper::CorruptIndex(machine, content) => {
                let index = sb.index_file(*machine);
                if !index.is_file() {
                    return Err(format!("{}: no index", index.display()));
                }
                std::fs::write(&index, content).map_err(|e| io(&index, e))
            }
            Tamper::DeleteIndex(machine) => {
                let index = sb.index_file(*machine);
                std::fs::remove_file(&index).map_err(|e| io(&index, e))
            }
            Tamper::MoveIndexToOldPath(machine) => {
                let index = self.read_index(*machine)?;
                let workweaves: serde_json::Map<String, serde_json::Value> = index["workweaves"]
                    .as_object()
                    .into_iter()
                    .flatten()
                    .map(|(name, entry)| (name.clone(), entry["path"].clone()))
                    .collect();
                let old = serde_json::json!({
                    "container": sb.machine_dir(*machine).join("old-container"),
                    "workweaves": workweaves,
                    "receipts": [],
                });
                let at = sb.old_path_index(*machine);
                std::fs::write(&at, serde_json::to_string_pretty(&old).unwrap())
                    .map_err(|e| io(&at, e))?;
                let new = sb.index_file(*machine);
                std::fs::remove_file(&new).map_err(|e| io(&new, e))
            }
        }
    }

    fn captures_file(&self, name: &str) -> PathBuf {
        self.sandbox
            .root
            .join("captures")
            .join(format!("{name}.json"))
    }

    fn persist_capture(&self, name: &str, kept: &BTreeMap<Place, Tips>) {
        let text: BTreeMap<String, serde_json::Value> = kept
            .iter()
            .map(|(place, tips)| {
                (
                    format!("{place:?}"),
                    serde_json::json!({
                        "exact": tips.exact,
                        "backups": tips.backups,
                        "branches": tips.branches,
                    }),
                )
            })
            .collect();
        let file = self.captures_file(name);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, serde_json::to_string(&text).unwrap()).unwrap();
    }

    /// What was captured as `name`: in this run, else as the run that built
    /// the sandbox left it on disk.
    fn captured_named(&self, name: &str) -> Result<BTreeMap<Place, Tips>, String> {
        if let Some(kept) = self.captured.get(name) {
            return Ok(kept.clone());
        }
        let path = self.captures_file(name);
        let text = std::fs::read_to_string(&path).map_err(|_| {
            format!(
                "nothing was captured as {name}: {} is missing, so this sandbox was not \
                 built with the journey's start state",
                path.display()
            )
        })?;
        let stored: BTreeMap<String, serde_json::Value> = serde_json::from_str(&text).unwrap();
        Ok(self
            .sandbox
            .live_places()
            .into_iter()
            .filter_map(|place| {
                let value = stored.get(&format!("{place:?}"))?;
                let field = |key: &str| value[key].as_str().unwrap_or_default().to_owned();
                let branches = value["branches"]
                    .as_object()
                    .map(|map| {
                        map.iter()
                            .map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_owned()))
                            .collect()
                    })
                    .unwrap_or_default();
                Some((
                    place,
                    Tips {
                        exact: field("exact"),
                        backups: field("backups"),
                        branches,
                    },
                ))
            })
            .collect())
    }

    fn tips(&self, at: &Place) -> Tips {
        let sb = self.sandbox;
        let mut exact = String::new();
        let mut backups = String::new();
        let mut branches = BTreeMap::new();
        for repo in [Repo::Proj, Repo::App, Repo::Lib, Repo::Docs] {
            let dir = sb.checkout(at, repo);
            if !dir.is_dir() {
                continue;
            }
            exact.push_str(&format!("== {}\n", repo.name()));
            for args in [
                &["rev-parse", "HEAD"][..],
                &["symbolic-ref", "-q", "HEAD"][..],
                &["status", "--porcelain"][..],
                &["rev-parse", "HEAD^{tree}"][..],
            ] {
                exact.push_str(&rendered(&sb.git(&dir, args)));
            }
            backups.push_str(&format!("== {}\n", repo.name()));
            backups.push_str(&rendered(&sb.git(
                &dir,
                &[
                    "for-each-ref",
                    "--format=%(refname) %(objectname)",
                    "refs/rwv/backup",
                ],
            )));
            for file in ["rwv.lock", "rwv.toml"] {
                if let Ok(text) = std::fs::read_to_string(dir.join(file)) {
                    exact.push_str(&text);
                }
            }
            let listed = sb.git_ok(
                &dir,
                &[
                    "for-each-ref",
                    "--format=%(refname) %(objectname)",
                    "refs/heads",
                ],
            );
            for line in listed.lines() {
                if let Some((name, sha)) = line.split_once(' ') {
                    branches.insert(format!("{}/{name}", repo.name()), sha.to_owned());
                }
            }
        }
        Tips {
            exact,
            backups,
            branches,
        }
    }

    fn local_state(&self, at: &Place) -> String {
        let sb = self.sandbox;
        let mut text = String::new();
        for repo in [Repo::Proj, Repo::App, Repo::Lib, Repo::Docs] {
            let dir = sb.checkout(at, repo);
            if !dir.is_dir() {
                continue;
            }
            text.push_str(&format!("== {}\n", repo.name()));
            for args in [
                &["rev-parse", "HEAD"][..],
                &["symbolic-ref", "-q", "HEAD"][..],
                &[
                    "for-each-ref",
                    "--format=%(refname) %(objectname)",
                    "refs/heads",
                ][..],
                &["status", "--porcelain"][..],
            ] {
                let out = sb.git(&dir, args);
                text.push_str(&rendered(&out));
            }
            if let Ok(lock) = std::fs::read_to_string(dir.join("rwv.lock")) {
                text.push_str(&lock);
            }
        }
        text
    }

    fn holds(
        &self,
        claim: &Claim,
        after: &Snapshot,
        output: &str,
        unmoved: &[(Captured, Place, String)],
    ) -> Result<(), String> {
        let sb = self.sandbox;
        match claim {
            Claim::Judged(repo, state) => judged_line(output, *repo, *state),
            Claim::JudgedOnly(repo, state) => {
                judged_line(output, *repo, *state)?;
                let others = lines_judging(output, *repo)
                    .filter(|line| judged_line(line, *repo, *state).is_err())
                    .collect::<Vec<_>>();
                if others.is_empty() {
                    Ok(())
                } else {
                    Err(format!(
                        "{} is judged in more than {}: {others:#?}",
                        repo.rel_path(),
                        state.name()
                    ))
                }
            }
            Claim::OutputContains(text) => {
                if output.contains(text) {
                    Ok(())
                } else {
                    Err(format!("the output does not hold {text:?}:\n{output}"))
                }
            }
            Claim::OutputOnce(text) => match output.matches(text).count() {
                1 => Ok(()),
                n => Err(format!("the output holds {text:?} {n} times:\n{output}")),
            },
            Claim::OutputInOrder(texts) => {
                let mut from = 0;
                for text in *texts {
                    match output[from..].find(text) {
                        Some(at) => from += at + text.len(),
                        None => {
                            return Err(format!(
                                "the output does not hold {text:?} after the texts before it \
                                 in {texts:?}:\n{output}"
                            ))
                        }
                    }
                }
                Ok(())
            }
            Claim::OutputLine(text) => {
                if output.lines().any(|line| line == *text) {
                    Ok(())
                } else {
                    Err(format!("no line of the output is {text:?}:\n{output}"))
                }
            }
            Claim::ListsWorkweave(name) => {
                if output
                    .lines()
                    .any(|line| line.split('\t').next() == Some(*name))
                {
                    Ok(())
                } else {
                    Err(format!("no line of the output lists {name:?}:\n{output}"))
                }
            }
            Claim::FindingsOnly(kind) => {
                let findings: Vec<&str> = output
                    .lines()
                    .filter(|line| {
                        ["[error: ", "[warning: "]
                            .iter()
                            .any(|opens| line.starts_with(opens))
                    })
                    .collect();
                let other: Vec<&&str> = findings
                    .iter()
                    .filter(|line| {
                        let named = line.split_once(": ").map_or("", |(_, rest)| rest);
                        !(named.starts_with(&format!("{kind}/"))
                            || named.starts_with(&format!("{kind}]")))
                    })
                    .collect();
                if findings.is_empty() {
                    Err(format!("no {kind} finding was printed:\n{output}"))
                } else if !other.is_empty() {
                    Err(format!("findings other than {kind}: {other:?}\n{output}"))
                } else {
                    Ok(())
                }
            }
            Claim::UpstreamOn {
                at,
                repo,
                branch,
                remote,
            } => {
                let dir = sb.checkout(at, *repo);
                let config = |key: String| {
                    let out = sb.git(&dir, &["config", "--get", &key]);
                    String::from_utf8_lossy(&out.stdout).trim().to_owned()
                };
                let name = config(format!("branch.{branch}.remote"));
                let merge = config(format!("branch.{branch}.merge"));
                let url = config(format!("remote.{name}.url"));
                let want = sb.remote_url(*remote);
                if url == want && merge == format!("refs/heads/{branch}") {
                    Ok(())
                } else {
                    Err(format!(
                        "{} {branch} takes in {merge:?} from {name:?} at {url:?}, not {branch} at {want}",
                        repo.name()
                    ))
                }
            }
            Claim::OutputLacks(text) => {
                if output.contains(text) {
                    Err(format!("the output holds {text:?}:\n{output}"))
                } else {
                    Ok(())
                }
            }
            Claim::RecordIs {
                machine,
                branch,
                record,
            } => {
                let canonical = sb.canonical(*machine, Repo::Proj);
                let held = super::scenario_invariants::read_records(sb, &canonical)
                    .and_then(|records| records.get(*branch).cloned());
                if held == *record {
                    Ok(())
                } else {
                    Err(format!(
                        "machine {machine:?} records {branch} as {held:?}, not {record:?}"
                    ))
                }
            }
            Claim::RemoteRecordIs { branch, record } => {
                let origin = sb.remote_store(Remote::origin(Repo::Proj));
                let held = super::scenario_invariants::read_records(sb, &origin)
                    .and_then(|records| records.get(*branch).cloned());
                if held == *record {
                    Ok(())
                } else {
                    Err(format!(
                        "the origin records {branch} as {held:?}, not {record:?}"
                    ))
                }
            }
            Claim::RemoteLacks { repo, branch } => {
                let origin = sb.remote_store(Remote::origin(*repo));
                let refname = format!("refs/heads/{branch}");
                if sb
                    .git(&origin, &["rev-parse", "--verify", "--quiet", &refname])
                    .status
                    .success()
                {
                    Err(format!("the origin of {} has {branch}", repo.name()))
                } else {
                    Ok(())
                }
            }
            Claim::AtPin { at, repo } => {
                let lock = std::fs::read_to_string(sb.checkout(at, Repo::Proj).join("rwv.lock"))
                    .map_err(|e| format!("no lock at {at:?}: {e}"))?;
                let lock: serde_json::Value =
                    serde_json::from_str(&lock).map_err(|e| format!("unreadable lock: {e}"))?;
                let pin = lock["repositories"][repo.rel_path()]["version"]
                    .as_str()
                    .ok_or_else(|| format!("the lock at {at:?} pins no {}", repo.name()))?
                    .to_owned();
                let out = sb.git(&sb.checkout(at, *repo), &["rev-parse", "HEAD"]);
                let head = String::from_utf8_lossy(&out.stdout).trim().to_owned();
                if head == pin {
                    Ok(())
                } else {
                    Err(format!(
                        "{} at {at:?} is at {head}, not its pin {pin}",
                        repo.name()
                    ))
                }
            }
            Claim::OnBranch { at, repo, branch } => {
                let out = sb.git(
                    &sb.checkout(at, *repo),
                    &["symbolic-ref", "--short", "HEAD"],
                );
                let head = String::from_utf8_lossy(&out.stdout).trim().to_owned();
                if head == *branch {
                    Ok(())
                } else {
                    Err(format!(
                        "{} at {at:?} is on {head:?}, not {branch:?}",
                        repo.name()
                    ))
                }
            }
            Claim::Presents { at, project } => {
                let pointer = sb.place_root(at).join(".rwv-active");
                let named = std::fs::read_to_string(&pointer)
                    .map_err(|e| format!("no pointer at {}: {e}", pointer.display()))?;
                if named.trim() == project.name() {
                    Ok(())
                } else {
                    Err(format!(
                        "{at:?} presents {:?}, not {}",
                        named.trim(),
                        project.name()
                    ))
                }
            }
            Claim::MarkerIs(at, state) => {
                let marker = sb.place_root(at).join(".rwv-workweave");
                let found = std::fs::read_to_string(&marker).ok();
                let matches = match (state, found.as_deref()) {
                    (MarkerState::Absent, None) => true,
                    (MarkerState::Corrupt, Some(text)) => text == "{ not a marker",
                    (MarkerState::ParentElsewhere, Some(text)) => {
                        serde_json::from_str::<serde_json::Value>(text)
                            .ok()
                            .and_then(|marker| marker["parent"].as_str().map(str::to_owned))
                            .is_some_and(|parent| parent.ends_with("nowhere"))
                    }
                    _ => false,
                };
                if matches {
                    Ok(())
                } else {
                    Err(format!("the marker at {at:?} is {found:?}, not {state:?}"))
                }
            }
            Claim::DirStands(machine, rel) => {
                let dir = sb.machine_dir(*machine).join(rel);
                if dir.is_dir() {
                    Ok(())
                } else {
                    Err(format!("{} is not a directory", dir.display()))
                }
            }
            Claim::ReportedPinsLanded { site, members } => {
                let pins = after
                    .pins
                    .get(site)
                    .ok_or_else(|| format!("{site:?} holds no lock"))?;
                let reported = reported_landed_pins(output);
                for member in *members {
                    let said: Vec<&String> = reported
                        .iter()
                        .filter(|(m, _)| m == member)
                        .map(|(_, pin)| pin)
                        .collect();
                    if said.is_empty() {
                        return Err(format!(
                            "the landing reported no pin for {}:\n{output}",
                            member.name()
                        ));
                    }
                    let held = pins
                        .get(member)
                        .ok_or_else(|| format!("{site:?} has no pin for {}", member.name()))?;
                    if let Some(other) = said.iter().find(|pin| !held.starts_with(pin.as_str())) {
                        return Err(format!(
                            "the landing reported {} landing at {other}, and {site:?} pins \
                             {held}:\n{output}",
                            member.name()
                        ));
                    }
                }
                Ok(())
            }
            Claim::ManifestLists { at, rev, members } => {
                let dir = sb.checkout(at, Repo::Proj);
                let shown = sb.git(&dir, &["show", &format!("{rev}:rwv.toml")]);
                if !shown.status.success() {
                    return Err(format!("{} has no rwv.toml at {rev}", dir.display()));
                }
                let text = String::from_utf8_lossy(&shown.stdout).into_owned();
                let manifest = repoweave::manifest::Manifest::from_toml_str(&text)
                    .map_err(|e| format!("rwv.toml at {rev} does not parse: {e:#}\n{text}"))?;
                let listed: std::collections::BTreeSet<String> = manifest
                    .iter_repo_paths()
                    .map(ToString::to_string)
                    .collect();
                let wanted: std::collections::BTreeSet<String> =
                    members.iter().map(|m| m.rel_path()).collect();
                if listed == wanted {
                    Ok(())
                } else {
                    Err(format!(
                        "rwv.toml at {rev} lists {listed:?}, not {wanted:?}\n{text}"
                    ))
                }
            }
            Claim::DirGone(machine, rel) => {
                let path = sb.machine_dir(*machine).join(rel);
                if path.symlink_metadata().is_err() {
                    Ok(())
                } else {
                    Err(format!("{} is still there", path.display()))
                }
            }
            Claim::WorkspaceStateKept(at) => {
                let before = &unmoved
                    .iter()
                    .find(|(kind, place, _)| *kind == Captured::StateFile && place == at)
                    .expect("captured before the step")
                    .2;
                let now = self.state_file_identity(at);
                if *before == now {
                    Ok(())
                } else {
                    Err(format!(
                        "the workspace state at {at:?} was written:\n--- before\n{before}\n--- after\n{now}"
                    ))
                }
            }
            Claim::WorkspaceStatesApart(one, other) => {
                let (one_dir, other_dir) = (sb.private_dir(one), sb.private_dir(other));
                let file = |dir: &std::path::Path| dir.join("rwv-workspace-state");
                if one_dir == other_dir {
                    Err(format!("{one:?} and {other:?} share {}", one_dir.display()))
                } else if !file(&one_dir).is_file() || !file(&other_dir).is_file() {
                    Err(format!(
                        "no workspace state at {} or {}",
                        file(&one_dir).display(),
                        file(&other_dir).display()
                    ))
                } else {
                    Ok(())
                }
            }
            Claim::MarkerParent { at, parent } => {
                let marker = sb.place_root(at).join(".rwv-workweave");
                let text = std::fs::read_to_string(&marker)
                    .map_err(|e| format!("no marker at {}: {e}", marker.display()))?;
                let value: serde_json::Value = serde_json::from_str(&text)
                    .map_err(|e| format!("{} does not parse: {e}\n{text}", marker.display()))?;
                let same = |field: &str, want: &std::path::Path| {
                    let got = value[field].as_str().unwrap_or_default();
                    if same_dir(std::path::Path::new(got), want) {
                        Ok(())
                    } else {
                        Err(format!("marker {field} is {got:?}, not {}", want.display()))
                    }
                };
                let machine = at.machine().expect("a marker belongs to a machine");
                same("primary", &sb.weave_root(machine))?;
                same("parent", &sb.place_root(parent))?;
                if value["project"].as_str() == Some(Repo::Proj.name()) {
                    Ok(())
                } else {
                    Err(format!("marker project is {:?}", value["project"]))
                }
            }
            Claim::IndexLists {
                machine,
                name,
                parent,
            } => {
                let index = self.read_index(*machine)?;
                let entry = &index["workweaves"][*name];
                let path = entry["path"].as_str().unwrap_or_default();
                let recorded_parent = entry["parent"].as_str().unwrap_or_default();
                let place = Place::Workweave(*machine, Repo::Proj, (*name).to_owned());
                if !same_dir(std::path::Path::new(path), &sb.place_root(&place)) {
                    return Err(format!("the index lists {name} at {path:?}:\n{index}"));
                }
                if !same_dir(
                    std::path::Path::new(recorded_parent),
                    &sb.place_root(parent),
                ) {
                    return Err(format!(
                        "the index lists {name} as forked from {recorded_parent:?}:\n{index}"
                    ));
                }
                Ok(())
            }
            Claim::IndexBasedir { machine, dir } => {
                let index = self.read_index(*machine)?;
                match (index.get("basedir").and_then(|v| v.as_str()), dir) {
                    (None, None) => Ok(()),
                    (Some(got), Some(dir)) => {
                        let want = sb.machine_dir(*machine).join(dir);
                        if same_dir(std::path::Path::new(got), &want) {
                            Ok(())
                        } else {
                            Err(format!("basedir is {got:?}, not {}", want.display()))
                        }
                    }
                    (got, want) => Err(format!("basedir is {got:?}; expected {want:?}")),
                }
            }
            Claim::IndexFile {
                machine,
                at,
                present,
            } => {
                let path = match at {
                    IndexAt::Common => sb.index_file(*machine),
                    IndexAt::Old => sb.old_path_index(*machine),
                };
                if path.is_file() == *present {
                    Ok(())
                } else {
                    Err(format!(
                        "{} is {}",
                        path.display(),
                        if *present { "absent" } else { "present" }
                    ))
                }
            }
            Claim::TipsAsCaptured(name) | Claim::BranchesAsCaptured(name) => {
                let kept = &self.captured_named(name)?;
                let whole = matches!(claim, Claim::TipsAsCaptured(_));
                let mut off = Vec::new();
                for (place, before) in kept {
                    let now = self.tips(place);
                    if whole && before.exact != now.exact {
                        off.push(format!(
                            "{place:?}:\n--- captured\n{}--- now\n{}",
                            before.exact, now.exact
                        ));
                    }
                    for (branch, sha) in &before.branches {
                        if now.branches.get(branch) != Some(sha) {
                            off.push(format!(
                                "{place:?}: {branch} was {sha}, now {:?}",
                                now.branches.get(branch)
                            ));
                        }
                    }
                }
                if off.is_empty() {
                    Ok(())
                } else {
                    Err(format!("not as captured as {name}:\n{}", off.join("\n")))
                }
            }
            Claim::BackupsAsCaptured { name, kept } => {
                let before = &self.captured_named(name)?;
                let mut off = Vec::new();
                for (place, before) in before {
                    let now = self.tips(place).backups;
                    let unkept = |text: &str| -> Vec<String> {
                        text.lines()
                            .filter(|line| {
                                let name = line.split(' ').next().unwrap_or(line);
                                let last = name.rsplit('/').next().unwrap_or(name);
                                let tip =
                                    last.len() == 12 && last.bytes().all(|b| b.is_ascii_hexdigit());
                                !kept.contains(&last) && !(tip && kept.contains(&"<tip>"))
                            })
                            .map(str::to_owned)
                            .collect()
                    };
                    if unkept(&before.backups) != unkept(&now) {
                        off.push(format!(
                            "{place:?} backups, but for {kept:?}:\n--- captured\n{}--- now\n{}",
                            before.backups, now
                        ));
                    }
                }
                if off.is_empty() {
                    Ok(())
                } else {
                    Err(format!(
                        "backups not as captured as {name}:\n{}",
                        off.join("\n")
                    ))
                }
            }
            Claim::LocalStateUnchanged(at) => {
                let before = &unmoved
                    .iter()
                    .find(|(kind, place, _)| *kind == Captured::Checkouts && place == at)
                    .expect("captured before the step")
                    .2;
                let now = self.local_state(at);
                if *before == now {
                    Ok(())
                } else {
                    Err(format!(
                        "the checkouts at {at:?} moved:\n--- before\n{before}--- after\n{now}"
                    ))
                }
            }
            Claim::Contains {
                at,
                repo,
                rev,
                mark,
            } => self.contains(&sb.checkout(at, *repo), rev, mark),
            Claim::Lacks {
                at,
                repo,
                rev,
                mark,
            } => {
                let dir = sb.checkout(at, *repo);
                let sha = self.marks.get(*mark).map_or(*mark, String::as_str);
                if !self
                    .sandbox
                    .git(&dir, &["rev-parse", "--verify", "--quiet", rev])
                    .status
                    .success()
                {
                    return Err(format!("{} has no {rev}", dir.display()));
                }
                match self.contains(&dir, rev, mark) {
                    Ok(()) => Err(format!(
                        "{} at {rev} contains {mark} ({sha})",
                        dir.display()
                    )),
                    Err(_) => Ok(()),
                }
            }
            Claim::RemoteContains {
                remote,
                branch,
                mark,
            } => self.contains(
                &sb.remote_store(*remote),
                &format!("refs/heads/{branch}"),
                mark,
            ),
            Claim::PinContains { site, member, mark } => {
                let pin = after
                    .pins
                    .get(site)
                    .and_then(|pins| pins.get(member))
                    .ok_or_else(|| format!("{site:?} has no pin for {}", member.name()))?;
                let store = scenario_invariants::store_for(sb, after, site, *member)
                    .ok_or_else(|| format!("no repository backs {} at {site:?}", member.name()))?;
                self.contains(&store, pin, mark)
            }
            Claim::RemoteFile {
                remote,
                branch,
                path,
                has,
                present,
            } => {
                let out = sb.git(
                    &sb.remote_path(*remote),
                    &["show", &format!("{branch}:{path}")],
                );
                let text = String::from_utf8_lossy(&out.stdout).into_owned();
                if text.contains(has) == *present {
                    Ok(())
                } else {
                    Err(format!(
                        "{remote:?} {branch}:{path} {} '{has}':\n{text}",
                        if *present { "lacks" } else { "holds" }
                    ))
                }
            }
            Claim::PinFile {
                site,
                member,
                path,
                has,
                present,
            } => {
                let pin = after
                    .pins
                    .get(site)
                    .and_then(|pins| pins.get(member))
                    .ok_or_else(|| format!("{site:?} has no pin for {}", member.name()))?;
                let store = scenario_invariants::store_for(sb, after, site, *member)
                    .ok_or_else(|| format!("no repository backs {} at {site:?}", member.name()))?;
                let out = sb.git(&store, &["show", &format!("{pin}:{path}")]);
                let text = String::from_utf8_lossy(&out.stdout).into_owned();
                if text.contains(has) == *present {
                    Ok(())
                } else {
                    Err(format!(
                        "{site:?} pins {} at {pin}, whose {path} {} '{has}':\n{text}",
                        member.name(),
                        if *present { "lacks" } else { "holds" }
                    ))
                }
            }
            Claim::PinLacks { site, member, mark } => {
                let pin = after
                    .pins
                    .get(site)
                    .and_then(|pins| pins.get(member))
                    .ok_or_else(|| format!("{site:?} has no pin for {}", member.name()))?;
                let store = scenario_invariants::store_for(sb, after, site, *member)
                    .ok_or_else(|| format!("no repository backs {} at {site:?}", member.name()))?;
                match self.contains(&store, pin, mark) {
                    Ok(()) => Err(format!(
                        "{site:?} pins {} at {pin}, which contains {mark}",
                        member.name()
                    )),
                    Err(_) => Ok(()),
                }
            }
            Claim::PinAt { site, member, mark } => {
                let pin = after
                    .pins
                    .get(site)
                    .and_then(|pins| pins.get(member))
                    .ok_or_else(|| format!("{site:?} has no pin for {}", member.name()))?;
                let sha = self.mark(mark)?;
                if pin == sha {
                    Ok(())
                } else {
                    Err(format!(
                        "{site:?} pins {} at {pin}, not at {mark} ({sha})",
                        member.name()
                    ))
                }
            }
            Claim::PinsReplayed {
                at,
                rev,
                member,
                subject,
                reached_from,
            } => {
                let proj = sb.checkout(at, Repo::Proj);
                let lock = sb.git_ok(&proj, &["show", &format!("{rev}:rwv.lock")]);
                let parsed = repoweave::manifest::LockFile::from_json_str(&lock)
                    .map_err(|e| format!("the lock at {rev} does not parse: {e:#}"))?;
                let pin = parsed
                    .iter_entries()
                    .find(|(key, _)| Repo::from_member_key(&key.to_string()) == *member)
                    .map(|(_, entry)| entry.version.to_string())
                    .ok_or_else(|| format!("the lock at {rev} has no pin for {}", member.name()))?;
                let dir = sb.checkout(at, *member);
                let pinned = sb.git_ok(&dir, &["log", "-1", "--format=%s", &pin]);
                if pinned.trim() != *subject {
                    return Err(format!(
                        "the lock at {rev} pins {} at {pin} ({}), not at a commit '{subject}'",
                        member.name(),
                        pinned.trim()
                    ));
                }
                if !self
                    .sandbox
                    .git(&dir, &["merge-base", "--is-ancestor", &pin, reached_from])
                    .status
                    .success()
                {
                    return Err(format!(
                        "the lock at {rev} pins {} at {pin} '{subject}', which {reached_from} \
                         does not reach: the commit the rebase replayed, not its image",
                        member.name()
                    ));
                }
                if self.mark(subject).is_ok_and(|original| *original == pin) {
                    return Err(format!(
                        "the lock at {rev} pins {} at the commit '{subject}' as it was before \
                         any rebase",
                        member.name()
                    ));
                }
                Ok(())
            }
            Claim::WorkweaveGone(place) => {
                if sb.place_root(place).exists() {
                    Err(format!("{place:?} still exists"))
                } else {
                    Ok(())
                }
            }
            Claim::NoBackup { repo, mark } => {
                let sha = self.mark(mark)?;
                let dir = sb.checkout(&Place::Primary(Machine::A), *repo);
                let holders = sb.git_ok(
                    &dir,
                    &[
                        "for-each-ref",
                        "--contains",
                        sha,
                        "--format=%(refname)",
                        "refs/rwv/backup/",
                    ],
                );
                if holders.trim().is_empty() {
                    Ok(())
                } else {
                    Err(format!(
                        "backups in {} still reach {mark} ({sha}): {holders:?}",
                        dir.display()
                    ))
                }
            }
            Claim::BackedUp { repo, mark, marked } if !self.marks.contains_key(mark) => {
                let dir = sb.checkout(&Place::Primary(Machine::A), *repo);
                let refs = sb.git_ok(
                    &dir,
                    &["for-each-ref", "--format=%(refname)", "refs/rwv/backup/"],
                );
                let held = refs.lines().any(|name| {
                    marked.is_none_or(|marked| {
                        name.rsplit('/')
                            .next()
                            .is_some_and(|id| id.starts_with(&format!("{marked}-")))
                    }) && self.contains_subject(&dir, name, mark).is_ok()
                });
                if held {
                    Ok(())
                } else {
                    Err(format!(
                        "no backup in {} holds a commit with the subject '{mark}'",
                        dir.display()
                    ))
                }
            }
            Claim::BackedUp { repo, mark, marked } => {
                let sha = self.mark(mark)?;
                let dir = sb.checkout(&Place::Primary(Machine::A), *repo);
                let holders = sb.git_ok(
                    &dir,
                    &[
                        "for-each-ref",
                        "--contains",
                        sha,
                        "--format=%(refname)",
                        "refs/rwv/backup/",
                    ],
                );
                let holder = holders.lines().find(|name| {
                    marked.is_none_or(|marked| {
                        name.rsplit('/')
                            .next()
                            .is_some_and(|id| id.starts_with(&format!("{marked}-")))
                    })
                });
                holder.map(|_| ()).ok_or_else(|| {
                    format!(
                        "no backup in {} reaches {mark} ({sha}){}; backups reaching it: {holders:?}",
                        dir.display(),
                        marked.map(|m| format!(" marked {m}")).unwrap_or_default()
                    )
                })
            }
        }
    }
}

/// What a finished operation left in every live place, with each commit
/// named by its subject and depth rather than its id, so two sandboxes that
/// ran the same steps at different times read alike when they ended alike.
pub fn end_state(sb: &Sandbox) -> String {
    let mut text = String::new();
    for place in sb.live_places() {
        let dirs: Vec<PathBuf> = [Repo::Proj, Repo::App, Repo::Lib, Repo::Docs]
            .into_iter()
            .map(|repo| sb.checkout(&place, repo))
            .filter(|dir| dir.is_dir())
            .collect();
        let named = |sha: &str| -> String {
            dirs.iter()
                .find_map(|dir| {
                    let subject = sb.git(dir, &["log", "-1", "--format=%s", sha]);
                    let depth = sb.git(dir, &["rev-list", "--count", sha]);
                    (subject.status.success() && depth.status.success()).then(|| {
                        format!(
                            "{} #{}",
                            rendered(&subject).trim_end(),
                            rendered(&depth).trim_end()
                        )
                    })
                })
                .unwrap_or_else(|| sha.to_owned())
        };
        let tip_named = |raw: &str| -> String {
            raw.lines()
                .map(|line| match line.split_once(' ') {
                    Some((name, rest))
                        if name.rsplit('/').next().is_some_and(|last| {
                            last.len() == 12 && last.bytes().all(|b| b.is_ascii_hexdigit())
                        }) =>
                    {
                        let cut = name.rfind('/').unwrap_or(0);
                        format!("{}/<tip> {rest}\n", &name[..cut])
                    }
                    _ => format!("{line}\n"),
                })
                .collect()
        };
        let renamed = |raw: &str| -> String {
            let mut out = String::new();
            let bytes = raw.as_bytes();
            let mut at = 0;
            while at < bytes.len() {
                let run = bytes[at..]
                    .iter()
                    .take_while(|b| b.is_ascii_hexdigit())
                    .count();
                if run == 40 {
                    out.push_str(&format!("[{}]", named(&raw[at..at + 40])));
                    at += 40;
                } else if run > 0 {
                    out.push_str(&raw[at..at + run]);
                    at += run;
                } else {
                    let width = raw[at..].chars().next().map_or(1, char::len_utf8);
                    out.push_str(&raw[at..at + width]);
                    at += width;
                }
            }
            out
        };
        text.push_str(&format!("#### {place:?}\n"));
        for repo in [Repo::Proj, Repo::App, Repo::Lib, Repo::Docs] {
            let dir = sb.checkout(&place, repo);
            if !dir.is_dir() {
                continue;
            }
            text.push_str(&format!("== {}\n", repo.name()));
            for args in [
                &["rev-parse", "HEAD"][..],
                &["symbolic-ref", "-q", "HEAD"][..],
                &[
                    "for-each-ref",
                    "--format=%(refname) %(objectname)",
                    "refs/heads",
                ][..],
                &["status", "--porcelain"][..],
            ] {
                text.push_str(&renamed(&rendered(&sb.git(&dir, args))));
            }
            let mut backups: Vec<String> = tip_named(&sb.git_ok(
                &dir,
                &[
                    "for-each-ref",
                    "--format=%(refname) %(objectname)",
                    "refs/rwv/backup",
                ],
            ))
            .lines()
            .map(|line| renamed(&format!("{line}\n")))
            .collect();
            backups.sort();
            text.extend(backups);
            let branches = sb.git_ok(
                &dir,
                &["for-each-ref", "--format=%(refname:short)", "refs/heads"],
            );
            for rev in
                std::iter::once("HEAD").chain(branches.lines().filter(|b| *b != "rwv-records"))
            {
                text.push_str(&format!("tree of {rev}:\n"));
                for line in sb.git_ok(&dir, &["ls-tree", "-r", rev]).lines() {
                    if !line.ends_with("\trwv.lock") && !line.ends_with("\trwv.toml") {
                        text.push_str(&format!("{line}\n"));
                    }
                }
            }
            for file in ["rwv.lock", "rwv.toml"] {
                if let Ok(content) = std::fs::read_to_string(dir.join(file)) {
                    text.push_str(&renamed(&content));
                }
            }
        }
    }
    let mut text = text.replace(sb.root.to_string_lossy().as_ref(), "<sandbox>");
    while let Some(at) = text
        .match_indices("Merge ")
        .map(|(at, _)| at + "Merge ".len())
        .find(|at| {
            let run = text[*at..]
                .bytes()
                .take_while(u8::is_ascii_hexdigit)
                .count();
            (7..40).contains(&run)
        })
    {
        let run = text[at..].bytes().take_while(u8::is_ascii_hexdigit).count();
        text.replace_range(at..at + run, "<id>");
    }
    text
}
