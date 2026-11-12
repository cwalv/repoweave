//! Where each member of a workspace sits, and the engine that puts it there.
//!
//! A workspace is aligned when every member is in its expected state: the
//! state the checked-out project branch's record, its member entries and its
//! lock say. The engine reads the refs and the workspace's own pointer state
//! and nothing else; uncommitted edits only stop it from moving a checkout.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::diagnosis::{Fact, FactKind, Judged, Judging, State};
use crate::manifest::{LockFile, Manifest, ProjectName, RepoEntry, RepoPath, Role};
use crate::records::{BranchName, Lookup, Record, Store};
use crate::state_file::StateFile;
use crate::vcs::{
    project_vcs, CommitSummary, HeadAttachment, LocalRefName, RawRefName, ResolvedRevisionId,
    Upstream, Vcs,
};

pub const WORKSPACE_STATE_FILE: &str = "rwv-workspace-state";

/// Proof that alignment found a checkout may leave the attachment it has:
/// every commit it holds stays reachable from a ref after the move.
///
/// Minted only in this module, only after the stray set of the move was read.
#[derive(Debug, Clone, Copy)]
pub struct Departure(());

impl Departure {
    /// Mint unconditionally, for in-crate test fixtures. Absent from the
    /// library the binary and the integration tests link against.
    #[cfg(test)]
    pub(crate) fn granted() -> Self {
        Self(())
    }

    /// The proof for the project repo's own move to `destination`: a detached
    /// HEAD that `destination` does not reach keeps what it holds as a backup
    /// first, and a branch keeps its own commits.
    pub fn of_project_move(
        vcs: &dyn Vcs,
        project_dir: &Path,
        destination: &ResolvedRevisionId,
    ) -> anyhow::Result<Self> {
        let head = vcs.head_revision(project_dir)?;
        if !vcs.is_ancestor(project_dir, &head, destination)? {
            vcs.backup_unpreserved_head(project_dir)?;
        }
        Ok(Self(()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Aligned {
    Branch(String),
    Detached,
}

/// The pointer state a workspace keeps for itself: the branch it last aligned
/// members for, and what it stored for each branch it aligned.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aligned: Option<Aligned>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub stored: BTreeMap<String, Stored>,
}

/// What a workspace last aligned one branch by.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stored {
    pub mode: crate::records::Mode,
    pub entries: BTreeMap<String, RepoEntry>,
    /// The entries the branch last stood in without a change to report: what
    /// an entry change is reported against until its push branch carries it.
    pub baseline: BTreeMap<String, RepoEntry>,
}

/// Forget what the workspace at `project_dir` stored for `branch`, as for a
/// branch created under the name. Whether the state file changed is returned.
pub fn forget_branch(vcs: &dyn Vcs, project_dir: &Path, branch: &str) -> anyhow::Result<bool> {
    let StateReading { mut state, corrupt } = read_state(vcs, project_dir)?;
    if corrupt.is_some() || state.stored.remove(branch).is_none() {
        return Ok(false);
    }
    store_state(vcs, project_dir, &state)
}

pub struct StateReading {
    pub state: WorkspaceState,
    pub corrupt: Option<String>,
}

fn state_path(
    vcs: &dyn Vcs,
    project_dir: &Path,
) -> anyhow::Result<Option<crate::state_file::StatePath>> {
    if !vcs.is_repo(project_dir) {
        return Ok(None);
    }
    let dir = vcs.private_dir(project_dir)?;
    Ok(Some(StateFile::WorkspaceState.in_dir(&dir)))
}

/// The workspace state of the project repo at `project_dir`. A file that
/// cannot be read or does not parse reads as absent, and the reading says so.
pub fn read_state(vcs: &dyn Vcs, project_dir: &Path) -> anyhow::Result<StateReading> {
    let absent = |corrupt| StateReading {
        state: WorkspaceState::default(),
        corrupt,
    };
    let Some(path) = state_path(vcs, project_dir)? else {
        return Ok(absent(None));
    };
    let bytes = match path.read() {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(absent(None)),
        Err(e) => return Ok(absent(Some(format!("it could not be read: {e:#}")))),
    };
    Ok(match serde_json::from_slice(&bytes) {
        Ok(state) => StateReading {
            state,
            corrupt: None,
        },
        Err(e) => absent(Some(format!("it does not parse: {e:#}"))),
    })
}

/// Make `next` the workspace state, writing the file only when its bytes
/// differ from what is there. Whether it wrote is returned.
pub fn store_state(
    vcs: &dyn Vcs,
    project_dir: &Path,
    next: &WorkspaceState,
) -> anyhow::Result<bool> {
    let Some(path) = state_path(vcs, project_dir)? else {
        return Ok(false);
    };
    let bytes = serde_json::to_vec_pretty(next).context("failed to render the workspace state")?;
    if path.read().is_ok_and(|held| held == bytes) {
        return Ok(false);
    }
    path.publish(&bytes)?;
    Ok(true)
}

/// Carry what the workspace at `project_dir` holds for the branch `from` to
/// the branch `to`: the branch it last aligned for, and the member entries it
/// last aligned that branch by. A state file that reads as absent is left as
/// it is, and why it reads so is returned.
pub fn rename_branch(
    vcs: &dyn Vcs,
    project_dir: &Path,
    from: &str,
    to: &str,
) -> anyhow::Result<Option<String>> {
    let StateReading { mut state, corrupt } = read_state(vcs, project_dir)?;
    if corrupt.is_some() {
        return Ok(corrupt);
    }
    if state.aligned == Some(Aligned::Branch(from.to_owned())) {
        state.aligned = Some(Aligned::Branch(to.to_owned()));
    }
    if let Some(stored) = state.stored.remove(from) {
        state.stored.insert(to.to_owned(), stored);
    }
    store_state(vcs, project_dir, &state)?;
    Ok(None)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    Declared,
    Feature { branch: String },
    Detached,
}

/// Where the project repo stands against the workspace's aligned branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectStanding {
    Aligned,
    Moved { aligned: String },
    Unaligned,
    Detached,
}

/// Record the workspace at `root` as aligned for the branch its project repo
/// has checked out, as `rwv update` does once it has aligned every member.
/// Whether the state file changed is returned.
pub fn record(root: &Path, project: &ProjectName) -> anyhow::Result<bool> {
    let vcs = project_vcs();
    let project_dir = crate::workspace::project_dir(root, project);
    if !vcs.is_repo(&project_dir) {
        return Ok(false);
    }
    let aligner = Aligner::open(root, project, Accounting::default())?;
    if aligner.standing == ProjectStanding::Detached {
        return Ok(false);
    }
    let manifest = Manifest::from_path(&project_dir.join(Manifest::FILE_NAME))?;
    store_state(vcs.as_ref(), &project_dir, &aligner.next_state(&manifest))
}

/// Branches nothing the operator made by hand explains: the declared branches
/// of each member across the weave's live weave branches, and the names of the
/// recorded weave branches.
#[derive(Debug, Clone, Default)]
pub struct Accounting {
    declared: BTreeMap<String, BTreeMap<String, BTreeSet<String>>>,
    recorded: BTreeSet<String>,
}

impl Accounting {
    /// Read every project of the weave at `primary`: each branch with a
    /// record, and the members its `rwv.toml` declares.
    pub fn scan(primary: &Path) -> Self {
        let mut accounting = Self::default();
        let vcs = project_vcs();
        for project in crate::workspace::discover_projects(primary) {
            let dir = crate::workspace::project_dir(primary, &project);
            if !vcs.is_repo(&dir) {
                continue;
            }
            let Ok(store) = Store::open(vcs.as_ref(), &dir) else {
                continue;
            };
            let Ok(view) = store.view() else {
                continue;
            };
            accounting
                .recorded
                .extend(view.names().map(ToOwned::to_owned));
            let Ok(branches) = vcs.named_branches(&dir) else {
                continue;
            };
            for branch in branches {
                let Ok(name) = BranchName::parse(vcs.as_ref(), &branch.name) else {
                    continue;
                };
                if matches!(view.lookup(&name), Lookup::Unrecorded) {
                    continue;
                }
                let declarer = format!("{} (projects/{project})", branch.name);
                accounting.read_declarations(vcs.as_ref(), &dir, &branch.tip, &declarer);
            }
        }
        accounting
    }

    fn read_declarations(
        &mut self,
        vcs: &dyn Vcs,
        dir: &Path,
        tip: &crate::vcs::CommitId,
        declarer: &str,
    ) {
        let tip = ResolvedRevisionId::from(tip.clone());
        let Ok(text) = vcs.read_file_at_revision(dir, &tip, Path::new(Manifest::FILE_NAME)) else {
            return;
        };
        let Ok(manifest) = Manifest::from_toml_str(&text) else {
            return;
        };
        for (path, entry) in manifest.iter_entries() {
            self.declared
                .entry(path.to_string())
                .or_default()
                .entry(entry.branch.local_counterpart().as_str().to_owned())
                .or_default()
                .insert(declarer.to_owned());
        }
    }

    pub fn declares(&self, member: &RepoPath, branch: &str) -> bool {
        !self.declarers(member, branch).is_empty()
    }

    /// The live weave branches that declare `branch` for `member`, each as
    /// `<branch> (projects/<project>)`.
    pub fn declarers(&self, member: &RepoPath, branch: &str) -> Vec<String> {
        self.declared
            .get(member.as_str())
            .and_then(|by_branch| by_branch.get(branch))
            .map(|set| set.iter().cloned().collect())
            .unwrap_or_default()
    }

    pub fn is_recorded(&self, branch: &str) -> bool {
        self.recorded.contains(branch)
    }
}

/// The commit a lock pins for a member, as this checkout can see it.
#[derive(Debug, Clone)]
pub enum Pin {
    None,
    Unreachable,
    At(ResolvedRevisionId),
}

pub fn pin_of(
    vcs: &dyn Vcs,
    dir: &Path,
    lock: Option<&LockFile>,
    path: &RepoPath,
) -> Result<Pin, crate::vcs::VcsError> {
    let Some(entry) = lock.and_then(|lock| lock.get_entry(path)) else {
        return Ok(Pin::None);
    };
    match vcs.resolve_revision(dir, entry.version.as_str()) {
        Ok(at) => Ok(Pin::At(at)),
        Err(crate::vcs::VcsError::RevisionNotFound { .. }) => Ok(Pin::Unreachable),
        Err(e) => Err(e),
    }
}

/// What finding a member's expected branch `E` made of the refs.
#[derive(Debug, Clone)]
pub enum Found {
    /// A local branch to use as it is.
    Local { tip: ResolvedRevisionId },
    /// A local branch that is given the upstream of its own name first.
    LocalGivenUpstream {
        tip: ResolvedRevisionId,
        upstream: Upstream,
    },
    /// A local branch taking in from a branch of another name.
    Foreign { upstream: Upstream },
    /// Only a remote has it: created from the upstream.
    OnRemote {
        upstream: Upstream,
        tip: ResolvedRevisionId,
    },
    /// Nowhere.
    Nowhere,
}

pub(crate) fn find_e(
    vcs: &dyn Vcs,
    dir: &Path,
    entry: &RepoEntry,
    branch: &RawRefName,
) -> anyhow::Result<Found> {
    let local = vcs.resolve_local_branch_tip(dir, branch)?;
    let copies = vcs.remote_copies(dir, branch)?;
    let chosen = || -> anyhow::Result<Option<crate::vcs::RemoteTip>> {
        match copies.as_slice() {
            [] => Ok(None),
            [only] => Ok(Some(only.clone())),
            several => {
                let url_remote = crate::url_remote::present(vcs, dir, &entry.url)?;
                let Some(url_remote) = url_remote else {
                    return Ok(None);
                };
                let home = vcs.home_remote(dir, &url_remote)?;
                Ok(several.iter().find(|copy| copy.remote == home).cloned())
            }
        }
    };
    let same_name = |remote: crate::vcs::RemoteHandle| Upstream {
        remote,
        branch: branch.clone(),
    };
    match local {
        Some(tip) => match vcs.upstream_of(dir, branch)? {
            Some(upstream) if upstream.branch == *branch => Ok(Found::Local { tip }),
            Some(upstream) => Ok(Found::Foreign { upstream }),
            None => Ok(match chosen()? {
                Some(copy) => Found::LocalGivenUpstream {
                    tip,
                    upstream: same_name(copy.remote),
                },
                None => Found::Local { tip },
            }),
        },
        None => Ok(match chosen()? {
            Some(copy) => Found::OnRemote {
                upstream: same_name(copy.remote),
                tip: copy.tip,
            },
            None => Found::Nowhere,
        }),
    }
}

/// The member `repo` declares `branch`, which exists nowhere: not locally, not
/// on a remote as last fetched.
pub(crate) fn declared_nowhere(
    repo: impl std::fmt::Display,
    dir: &Path,
    branch: impl std::fmt::Display,
) -> crate::diagnosis::Judging {
    let branch = branch.to_string();
    State::NameNoSuchBranch
        .of(&repo)
        .member()
        .subject(&branch)
        .bind("<m>", repo)
        .bind("<b>", branch)
        .checkout(dir)
}

/// How a member is expected to sit, and why it sits there.
#[derive(Debug, Clone)]
pub enum Target {
    /// On the branch, created first where the refs call for it, and
    /// fast-forwarded to the pin where it is behind.
    Branch {
        name: RawRefName,
        found: Found,
        fast_forward_to: Option<ResolvedRevisionId>,
    },
    /// Detached at the pin.
    Detached {
        at: ResolvedRevisionId,
        why: DetachedBecause,
    },
    /// Where it is: the pin is missing or unreachable and no branch is found.
    AsFound,
}

#[derive(Debug, Clone)]
pub enum DetachedBecause {
    Workspace,
    CheckedOutElsewhere { branch: String, at: Vec<PathBuf> },
    NotOnDeclaredBranch { branch: String },
}

impl DetachedBecause {
    pub fn fact(&self) -> Option<Fact> {
        match self {
            Self::CheckedOutElsewhere { branch, at } => Some(Fact {
                kind: FactKind::Checkout,
                about: Some(format!(
                    "{branch} is checked out in {}",
                    at.iter()
                        .map(|p| p.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                )),
            }),
            Self::Workspace | Self::NotOnDeclaredBranch { .. } => None,
        }
    }
}

/// What a member's expected state is, read from the refs.
#[derive(Debug, Clone)]
pub struct Plan {
    pub target: Target,
    pub pin: Pin,
    /// The declared branch that exists nowhere, which no alignment creates.
    pub unfound: Option<String>,
}

/// One member as the engine sees it: where it sits in this workspace.
pub struct Member<'a> {
    pub path: &'a RepoPath,
    pub entry: &'a RepoEntry,
    pub dir: &'a Path,
    pub vcs: &'a dyn Vcs,
}

fn reaches(
    vcs: &dyn Vcs,
    dir: &Path,
    pin: &ResolvedRevisionId,
    tip: &ResolvedRevisionId,
) -> anyhow::Result<bool> {
    Ok(vcs.is_ancestor(dir, pin, tip)?)
}

fn checked_out_elsewhere(m: &Member<'_>, branch: &RawRefName) -> anyhow::Result<Vec<PathBuf>> {
    let own = crate::path_spelling::CanonicalPath::of(m.dir);
    Ok(m.vcs
        .checkouts_of_branch(m.dir, branch)?
        .into_iter()
        .filter(|p| crate::path_spelling::CanonicalPath::of(p) != own)
        .collect())
}

pub struct Aligner {
    pub root: PathBuf,
    pub project: ProjectName,
    pub project_dir: PathBuf,
    pub mode: Mode,
    pub standing: ProjectStanding,
    pub state: WorkspaceState,
    pub corrupt: Option<String>,
    pub accounting: Accounting,
    checked_out: Option<String>,
    outgoing: Option<Box<Aligner>>,
    carry_edits: bool,
}

impl Aligner {
    /// Read the workspace the project repo at `<root>/projects/<project>`
    /// belongs to. The project repo's branch must be recorded, or the
    /// workspace detached.
    pub fn open(
        root: &Path,
        project: &ProjectName,
        accounting: Accounting,
    ) -> anyhow::Result<Self> {
        let root = root.to_path_buf();
        let project_dir = crate::workspace::project_dir(&root, project);
        let vcs = project_vcs();
        let reading = read_state(vcs.as_ref(), &project_dir)?;
        let head = if vcs.is_repo(&project_dir) {
            Some(vcs.head_attachment(&project_dir)?)
        } else {
            None
        };
        let checked_out = match &head {
            Some(HeadAttachment::Attached(a)) => Some(a.to_string()),
            Some(HeadAttachment::Unborn(u)) => Some(u.name().as_str().to_owned()),
            _ => None,
        };
        let mode = match (&head, &checked_out) {
            (None, _) => Mode::Declared,
            (Some(_), Some(branch)) => {
                let name = BranchName::parse(vcs.as_ref(), branch).map_err(|e| {
                    crate::untokened!(
                        InternalInvariant,
                        "`{branch}` is not a usable weave branch name: {e:?}"
                    )
                })?;
                let store = Store::open(vcs.as_ref(), &project_dir)?;
                match store.view()?.lookup(&name) {
                    Lookup::Value(Record::Feature { .. }) => Mode::Feature {
                        branch: branch.clone(),
                    },
                    _ => Mode::Declared,
                }
            }
            (Some(_), None) => Mode::Detached,
        };
        let standing = match (&mode, &reading.state.aligned) {
            (Mode::Detached, Some(Aligned::Detached)) => ProjectStanding::Aligned,
            (Mode::Detached, _) => ProjectStanding::Detached,
            (_, None) => ProjectStanding::Unaligned,
            (_, Some(Aligned::Detached)) => ProjectStanding::Unaligned,
            (_, Some(Aligned::Branch(aligned))) => {
                if checked_out.as_deref() == Some(aligned.as_str()) {
                    ProjectStanding::Aligned
                } else if vcs
                    .resolve_local_branch_tip(&project_dir, &RawRefName::new(aligned.clone()))?
                    .is_some()
                {
                    ProjectStanding::Moved {
                        aligned: aligned.clone(),
                    }
                } else {
                    ProjectStanding::Unaligned
                }
            }
        };
        Ok(Self {
            root,
            project: project.clone(),
            project_dir,
            mode,
            standing,
            state: reading.state,
            corrupt: reading.corrupt,
            accounting,
            checked_out,
            outgoing: None,
            carry_edits: false,
        })
    }

    /// The workspace as it will read once the project repo is on `branch`
    /// (`None`: detached), judged before the project repo moves. Members that
    /// hold edits move as the VCS carries them.
    pub fn prospective(
        root: &Path,
        project: &ProjectName,
        accounting: Accounting,
        branch: Option<(&BranchName, &Record)>,
    ) -> anyhow::Result<Self> {
        let project_dir = crate::workspace::project_dir(root, project);
        let reading = read_state(project_vcs().as_ref(), &project_dir)?;
        let (mode, checked_out) = match branch {
            Some((name, Record::Feature { .. })) => (
                Mode::Feature {
                    branch: name.to_string(),
                },
                Some(name.to_string()),
            ),
            Some((name, Record::Declared)) => (Mode::Declared, Some(name.to_string())),
            None => (Mode::Detached, None),
        };
        Ok(Self {
            root: root.to_path_buf(),
            project: project.clone(),
            project_dir,
            mode,
            standing: ProjectStanding::Aligned,
            state: reading.state,
            corrupt: reading.corrupt,
            accounting,
            checked_out,
            outgoing: None,
            carry_edits: true,
        })
    }

    /// Move a member that holds edits as the VCS carries them: only edits the
    /// move would overwrite hold it back.
    pub fn carrying_edits(mut self) -> Self {
        self.carry_edits = true;
        self
    }

    /// Count a member aligned for `outgoing`'s branch as one that may be left,
    /// as a switch of the presented project does.
    pub fn leaving(mut self, outgoing: Aligner) -> Self {
        self.outgoing = Some(Box::new(outgoing));
        self
    }

    /// The branch the project repo has checked out.
    pub fn checked_out(&self) -> Option<&str> {
        self.checked_out.as_deref()
    }

    /// The line for a project repo whose HEAD is on no branch and whose
    /// workspace was not made detached: the commits it holds that no branch
    /// reaches, listed, with the choice that keeps them where it holds any,
    /// else the one that returns the project repo to a branch.
    pub fn project_detached(&self) -> anyhow::Result<crate::diagnosis::Judging> {
        let vcs = project_vcs();
        let held = match vcs.head_revision(&self.project_dir) {
            Ok(head) => vcs.stray_commits(&self.project_dir, &head, None, None)?,
            Err(_) => Vec::new(),
        };
        let choice = if held.is_empty() {
            crate::diagnosis::ChoiceId::C30
        } else {
            crate::diagnosis::ChoiceId::C14
        };
        Ok(State::AlignmentProjectDetached
            .of(crate::workspace::project_rel_path(&self.project))
            .project_repo()
            .listing(held.iter().map(|c| {
                format!(
                    "{} {}",
                    crate::vcs::short_revision(c.id.as_str()),
                    c.subject
                )
            }))
            .checkout(&self.project_dir)
            .only(&[choice]))
    }

    /// The aligned branch as a choice spells it: its name, or `detached`.
    pub fn state_aligned_text(&self) -> String {
        match &self.state.aligned {
            Some(Aligned::Branch(name)) => name.clone(),
            Some(Aligned::Detached) => "detached".to_owned(),
            None => String::new(),
        }
    }

    pub fn is_detached_workspace(&self) -> bool {
        self.mode == Mode::Detached && self.standing == ProjectStanding::Aligned
    }

    fn wanted(&self, role: Role, entry: &RepoEntry) -> (RawRefName, bool) {
        match (&self.mode, role) {
            (Mode::Feature { branch }, Role::Owned | Role::Fork) => {
                (RawRefName::new(branch.clone()), false)
            }
            _ => (
                RawRefName::new(entry.branch.local_counterpart().as_str()),
                true,
            ),
        }
    }

    /// Usability: the member's expected state against its pin.
    pub fn plan(&self, m: &Member<'_>, lock: Option<&LockFile>) -> anyhow::Result<Plan> {
        let pin = pin_of(m.vcs, m.dir, lock, m.path)?;
        let (branch, declared) = self.wanted(m.entry.role, m.entry);
        let found = find_e(m.vcs, m.dir, m.entry, &branch)?;
        let unfound =
            (declared && matches!(found, Found::Nowhere)).then(|| branch.as_str().to_owned());
        let target = match (&pin, self.mode == Mode::Detached) {
            (Pin::At(at), true) => Target::Detached {
                at: at.clone(),
                why: DetachedBecause::Workspace,
            },
            (Pin::None, _) => match &found {
                Found::Foreign { .. } | Found::Nowhere => Target::AsFound,
                _ if !checked_out_elsewhere(m, &branch)?.is_empty() => Target::AsFound,
                _ => Target::Branch {
                    name: branch,
                    found,
                    fast_forward_to: None,
                },
            },
            (Pin::Unreachable, _) => Target::AsFound,
            (Pin::At(_), false) if unfound.is_some() => Target::AsFound,
            (Pin::At(at), false) => self.usable(m, &branch, declared, found, at.clone())?,
        };
        Ok(Plan {
            target,
            pin,
            unfound,
        })
    }

    fn usable(
        &self,
        m: &Member<'_>,
        branch: &RawRefName,
        declared: bool,
        found: Found,
        pin: ResolvedRevisionId,
    ) -> anyhow::Result<Target> {
        let elsewhere = checked_out_elsewhere(m, branch)?;
        if !elsewhere.is_empty() {
            return Ok(Target::Detached {
                at: pin,
                why: DetachedBecause::CheckedOutElsewhere {
                    branch: branch.as_str().to_owned(),
                    at: elsewhere,
                },
            });
        }
        let tip = match &found {
            Found::Local { tip } | Found::LocalGivenUpstream { tip, .. } => Some(tip.clone()),
            Found::OnRemote { tip, .. } => Some(tip.clone()),
            Found::Foreign { .. } | Found::Nowhere => None,
        };
        if declared {
            let upstream_tip = match &found {
                Found::Local { .. } | Found::LocalGivenUpstream { .. } => m
                    .vcs
                    .remote_copies(m.dir, branch)?
                    .into_iter()
                    .map(|copy| copy.tip)
                    .collect::<Vec<_>>(),
                Found::OnRemote { tip, .. } => vec![tip.clone()],
                _ => Vec::new(),
            };
            let mut on_declared = false;
            for tip in tip.iter().chain(upstream_tip.iter()) {
                if reaches(m.vcs, m.dir, &pin, tip)? {
                    on_declared = true;
                    break;
                }
            }
            if !matches!(found, Found::Foreign { .. } | Found::Nowhere) && !on_declared {
                return Ok(Target::Detached {
                    at: pin,
                    why: DetachedBecause::NotOnDeclaredBranch {
                        branch: branch.as_str().to_owned(),
                    },
                });
            }
        }
        let fast_forward_to = match &found {
            Found::Local { tip } | Found::LocalGivenUpstream { tip, .. }
                if *tip != pin && reaches(m.vcs, m.dir, tip, &pin)? =>
            {
                Some(pin)
            }
            _ => None,
        };
        Ok(Target::Branch {
            name: branch.clone(),
            found,
            fast_forward_to,
        })
    }

    /// Whether the member sits where the plan says, by attachment.
    pub fn is_aligned(&self, m: &Member<'_>, plan: &Plan) -> anyhow::Result<bool> {
        let head = m.vcs.head_attachment(m.dir)?;
        Ok(match (&plan.target, &head) {
            (Target::AsFound, _) => true,
            (Target::Detached { at, .. }, HeadAttachment::Detached(now)) => now.at() == at,
            (Target::Detached { .. }, _) => false,
            (Target::Branch { name, found, .. }, HeadAttachment::Attached(a)) => {
                a.is_named(&LocalRefName::of_branch(name))
                    && !matches!(found, Found::Nowhere | Found::OnRemote { .. })
            }
            (Target::Branch { .. }, _) => false,
        })
    }

    /// What a member that is not aligned is standing on.
    pub fn standing_of(&self, m: &Member<'_>, plan: &Plan) -> anyhow::Result<Off> {
        let head = m.vcs.head_attachment(m.dir)?;
        let onto = match &plan.target {
            Target::Branch {
                found:
                    Found::Local { tip }
                    | Found::LocalGivenUpstream { tip, .. }
                    | Found::OnRemote { tip, .. },
                ..
            } => Some(tip.clone()),
            Target::Detached { at, .. } => Some(at.clone()),
            _ => None,
        };
        Ok(match head {
            HeadAttachment::Attached(a) => {
                let tip = m.vcs.head_revision(m.dir)?;
                let leaving = RawRefName::new(a.to_string());
                let stray = m
                    .vcs
                    .stray_commits(m.dir, &tip, Some(&leaving), onto.as_ref())?;
                Off::OnBranch {
                    branch: a.to_string(),
                    stray,
                }
            }
            HeadAttachment::Unborn(u) => Off::OnBranch {
                branch: u.name().as_str().to_owned(),
                stray: Vec::new(),
            },
            HeadAttachment::Detached(d) => {
                let unpushed = m.vcs.stray_commits(m.dir, d.at(), None, None)?;
                if unpushed.is_empty() {
                    Off::DetachedElsewhere
                } else {
                    Off::DetachedUnpushed { commits: unpushed }
                }
            }
        })
    }

    fn may_leave(&self, m: &Member<'_>, off: &Off) -> bool {
        match off {
            Off::DetachedElsewhere => true,
            Off::DetachedUnpushed { .. } => false,
            Off::OnBranch { branch, stray } => {
                stray.is_empty()
                    || self.accounting.declares(m.path, branch)
                    || self.accounting.is_recorded(branch)
                    || self.aligned_for_aligned_branch(m, branch)
            }
        }
    }

    fn aligned_for_aligned_branch(&self, m: &Member<'_>, branch: &str) -> bool {
        self.held_by_aligned_branch(m, branch)
            || self
                .outgoing
                .as_ref()
                .is_some_and(|outgoing| outgoing.aligned_for_aligned_branch(m, branch))
    }

    fn held_by_aligned_branch(&self, m: &Member<'_>, branch: &str) -> bool {
        let Some(Aligned::Branch(aligned)) = &self.state.aligned else {
            return false;
        };
        let Some(stored) = self.state.stored.get(aligned) else {
            return false;
        };
        let Some(entry) = stored.entries.get(m.path.as_str()) else {
            return false;
        };
        match (stored.mode, entry.role) {
            (crate::records::Mode::Feature, Role::Owned | Role::Fork) => aligned == branch,
            _ => entry.branch.local_counterpart().as_str() == branch,
        }
    }
}

/// A member that is not in its expected state, and what it is on instead.
#[derive(Debug, Clone)]
pub enum Off {
    OnBranch {
        branch: String,
        stray: Vec<CommitSummary>,
    },
    DetachedElsewhere,
    DetachedUnpushed {
        commits: Vec<CommitSummary>,
    },
}

/// What aligning one member did.
#[derive(Debug)]
pub enum Applied {
    Aligned {
        moved_from: Option<String>,
        facts: Vec<Fact>,
        /// Lines that report the state the member was put in.
        notes: Vec<Judged>,
    },
    /// Not moved; the lines say why and what the operator can run.
    Left { judged: Vec<Judged> },
}

impl Aligner {
    /// Put the member in its expected state when it may move there.
    pub fn apply(
        &self,
        m: &Member<'_>,
        plan: &Plan,
        repo: &dyn std::fmt::Display,
    ) -> anyhow::Result<Applied> {
        let facts: Vec<Fact> = match &plan.target {
            Target::Detached { why, .. } => why.fact().into_iter().collect(),
            _ => Vec::new(),
        };
        let mut notes = self.notes(m, plan, repo);
        notes.extend(Self::pin_note(m, plan, repo));
        if let Target::Branch {
            found: Found::Foreign { upstream },
            name,
            ..
        } = &plan.target
        {
            return Ok(Applied::Left {
                judged: vec![State::PushBranchForeignUpstream
                    .of(repo)
                    .member()
                    .subject(format!(
                        "{name} takes in from {}/{}",
                        upstream.remote, upstream.branch
                    ))
                    .checkout(m.dir)
                    .judge(m.vcs)],
            });
        }
        if let Target::Branch {
            found: Found::LocalGivenUpstream { upstream, .. },
            name,
            ..
        } = &plan.target
        {
            m.vcs.set_upstream(m.dir, name, upstream)?;
        }
        if self.is_aligned(m, plan)? {
            self.finish_in_place(m, plan)?;
            return Ok(Applied::Aligned {
                moved_from: None,
                facts,
                notes,
            });
        }
        let blocked = self.blocked(m, plan, repo)?;
        if !blocked.is_empty() {
            return Ok(Applied::Left { judged: blocked });
        }
        let off = self.standing_of(m, plan)?;
        let departure = Departure(());
        let moved_from = match &off {
            Off::OnBranch { branch, .. }
                if !self.accounting.declares(m.path, branch)
                    && !self.accounting.is_recorded(branch)
                    && !self.aligned_for_aligned_branch(m, branch) =>
            {
                Some(branch.clone())
            }
            _ => None,
        };
        self.move_to(m, plan, &departure)?;
        Ok(Applied::Aligned {
            moved_from,
            facts,
            notes,
        })
    }

    /// The lines that say why a member cannot move to where its plan puts it:
    /// none where it is there already, or may leave where it is.
    pub fn blocked(
        &self,
        m: &Member<'_>,
        plan: &Plan,
        repo: &dyn std::fmt::Display,
    ) -> anyhow::Result<Vec<Judged>> {
        if self.is_aligned(m, plan)? {
            return Ok(Vec::new());
        }
        let facts: Vec<Fact> = match &plan.target {
            Target::Detached { why, .. } => why.fact().into_iter().collect(),
            _ => Vec::new(),
        };
        let off = self.standing_of(m, plan)?;
        let mut dirty = m.vcs.tracked_dirty_file_names(m.dir)?;
        if self.carry_edits {
            dirty = self.overwritten(m, plan, dirty)?;
        }
        let busy = m.vcs.mid_operation(m.dir).is_some();
        if dirty.is_empty() && !busy && self.may_leave(m, &off) {
            return Ok(Vec::new());
        }
        let mut judged = vec![self.judge_off(m, &off, repo, &facts)];
        if let Some(edits) = crate::diagnosis::judge_edits(m.vcs, repo, m.dir, dirty, None) {
            judged.push(edits);
        }
        Ok(judged)
    }

    /// The commit the plan leaves the member's checkout at, where it has one.
    fn destination(m: &Member<'_>, plan: &Plan) -> anyhow::Result<Option<ResolvedRevisionId>> {
        Ok(match &plan.target {
            Target::AsFound => None,
            Target::Detached { at, .. } => Some(at.clone()),
            Target::Branch {
                fast_forward_to: Some(to),
                ..
            } => Some(to.clone()),
            Target::Branch { found, .. } => match (found, &plan.pin) {
                (Found::Local { tip } | Found::LocalGivenUpstream { tip, .. }, _) => {
                    Some(tip.clone())
                }
                (Found::OnRemote { tip, .. }, Pin::At(pin)) if reaches(m.vcs, m.dir, pin, tip)? => {
                    Some(pin.clone())
                }
                (Found::OnRemote { tip, .. }, _) => Some(tip.clone()),
                (Found::Nowhere, Pin::At(pin)) => Some(pin.clone()),
                (Found::Foreign { .. } | Found::Nowhere, _) => None,
            },
        })
    }

    /// The edited files the move to the plan's commit would overwrite: all of
    /// them where the destination is not known.
    fn overwritten(
        &self,
        m: &Member<'_>,
        plan: &Plan,
        dirty: Vec<String>,
    ) -> anyhow::Result<Vec<String>> {
        if dirty.is_empty() {
            return Ok(dirty);
        }
        let Some(to) = Self::destination(m, plan)? else {
            return Ok(dirty);
        };
        let from = m.vcs.head_revision(m.dir)?;
        let changed = m.vcs.changed_paths_between(m.dir, &from, &to)?;
        Ok(dirty
            .into_iter()
            .filter(|file| changed.contains(file))
            .collect())
    }

    /// The refusal for a member that is not where its plan puts it, `None`
    /// where it is, or where the plan leaves it as found.
    pub fn refusal(
        &self,
        m: &Member<'_>,
        plan: &Plan,
        repo: &dyn std::fmt::Display,
    ) -> anyhow::Result<Option<Judged>> {
        if matches!(plan.target, Target::AsFound) || self.is_aligned(m, plan)? {
            return Ok(None);
        }
        let facts: Vec<Fact> = match &plan.target {
            Target::Detached { why, .. } => why.fact().into_iter().collect(),
            _ => Vec::new(),
        };
        let off = self.standing_of(m, plan)?;
        Ok(Some(self.judge_off(m, &off, repo, &facts)))
    }

    pub fn pin_note(m: &Member<'_>, plan: &Plan, repo: &dyn std::fmt::Display) -> Option<Judged> {
        let state = match plan.pin {
            Pin::At(_) => return None,
            Pin::None => State::PinNone,
            Pin::Unreachable => State::PinUnreachable,
        };
        Some(
            state
                .of(repo)
                .member()
                .checkout(m.dir)
                .bind("<m>", m.path)
                .judge(m.vcs),
        )
    }

    fn notes(&self, m: &Member<'_>, plan: &Plan, repo: &dyn std::fmt::Display) -> Vec<Judged> {
        if let Some(branch) = &plan.unfound {
            return vec![declared_nowhere(repo, m.dir, branch).judge(m.vcs)];
        }
        match &plan.target {
            Target::Detached {
                why: DetachedBecause::NotOnDeclaredBranch { branch },
                ..
            } => vec![State::PinNotOnDeclaredBranch
                .of(repo)
                .member()
                .subject(branch)
                .checkout(m.dir)
                .bind("<m>", m.path)
                .bind("<b>", branch)
                .judge(m.vcs)],
            _ => Vec::new(),
        }
    }

    fn finish_in_place(&self, m: &Member<'_>, plan: &Plan) -> anyhow::Result<()> {
        if let (
            Target::Branch {
                fast_forward_to: Some(pin),
                ..
            },
            HeadAttachment::Attached(a),
        ) = (&plan.target, m.vcs.head_attachment(m.dir)?)
        {
            m.vcs.advance_attached_ref(&a, pin)?;
        }
        Ok(())
    }

    fn move_to(&self, m: &Member<'_>, plan: &Plan, departure: &Departure) -> anyhow::Result<()> {
        let head = m.vcs.head_attachment(m.dir)?;
        match &plan.target {
            Target::AsFound => {}
            Target::Detached { at, .. } => match head {
                HeadAttachment::Attached(a) => m.vcs.detach_head(&a, at, departure)?,
                HeadAttachment::Detached(was) => m.vcs.advance_detached_head(&was, at)?,
                HeadAttachment::Unborn(_) => {
                    return Err(crate::untokened!(
                        InternalInvariant,
                        "{}: an unborn branch cannot be detached",
                        m.path
                    ))
                }
            },
            Target::Branch {
                name,
                found,
                fast_forward_to,
            } => {
                match found {
                    Found::OnRemote { upstream, tip } => {
                        let at = match &plan.pin {
                            Pin::At(pin) if reaches(m.vcs, m.dir, pin, tip)? => pin.clone(),
                            _ => tip.clone(),
                        };
                        m.vcs.create_branch(m.dir, name, &at, Some(upstream))?;
                    }
                    Found::Nowhere => {
                        let Pin::At(pin) = &plan.pin else {
                            return Ok(());
                        };
                        m.vcs.create_branch(m.dir, name, pin, None)?;
                    }
                    _ => {}
                }
                let local = LocalRefName::of_branch(name);
                m.vcs.reattach_head(head, &local, departure)?;
                if let Some(pin) = fast_forward_to {
                    if let HeadAttachment::Attached(a) = m.vcs.head_attachment(m.dir)? {
                        m.vcs.advance_attached_ref(&a, pin)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn judge_off(
        &self,
        m: &Member<'_>,
        off: &Off,
        repo: &dyn std::fmt::Display,
        facts: &[Fact],
    ) -> Judged {
        self.judging_off(m, off, repo, facts).judge(m.vcs)
    }

    fn judging_off(
        &self,
        m: &Member<'_>,
        off: &Off,
        repo: &dyn std::fmt::Display,
        facts: &[Fact],
    ) -> Judging {
        let commits = match off {
            Off::OnBranch { stray, .. } => stray.as_slice(),
            Off::DetachedUnpushed { commits } => commits.as_slice(),
            Off::DetachedElsewhere => &[],
        };
        let ids: Vec<String> = commits
            .iter()
            .rev()
            .map(|c| crate::vcs::short_revision(c.id.as_str()).to_string())
            .collect();
        let (state, listing, subject) = match off {
            Off::OnBranch { branch, stray } => (
                if stray.is_empty() {
                    State::AlignmentOnBranch
                } else {
                    State::AlignmentOnBranchWithStrayCommits
                },
                stray
                    .iter()
                    .map(|c| {
                        format!(
                            "{} {}",
                            crate::vcs::short_revision(c.id.as_str()),
                            c.subject
                        )
                    })
                    .collect::<Vec<_>>(),
                Some(branch.clone()),
            ),
            Off::DetachedElsewhere => (State::AlignmentDetachedElsewhere, Vec::new(), None),
            Off::DetachedUnpushed { commits } => (
                State::AlignmentDetachedUnpushed,
                commits
                    .iter()
                    .map(|c| {
                        format!(
                            "{} {}",
                            crate::vcs::short_revision(c.id.as_str()),
                            c.subject
                        )
                    })
                    .collect(),
                None,
            ),
        };
        let mut judging = state
            .of(repo)
            .member()
            .listing(listing)
            .checkout(m.dir)
            .bind("<m>", m.path)
            .bind("<b>", self.wanted(m.entry.role, m.entry).0)
            .bind("<ids>", ids.join(" "));
        if let Some(subject) = subject {
            judging = judging.subject(subject);
        }
        for fact in facts {
            judging = judging.fact(fact.clone());
        }
        judging
    }

    /// The line for a member that is not in its expected state, judged as an
    /// activation of this project would: `None` when it is, or when there is no
    /// expected state to put it in.
    pub fn activation_line(
        &self,
        m: &Member<'_>,
        lock: Option<&LockFile>,
        repo: &dyn std::fmt::Display,
    ) -> anyhow::Result<Option<Judged>> {
        let plan = self.plan(m, lock)?;
        if self.is_aligned(m, &plan)? {
            return Ok(None);
        }
        let facts: Vec<Fact> = match &plan.target {
            Target::Detached { why, .. } => why.fact().into_iter().collect(),
            _ => Vec::new(),
        };
        let off = self.standing_of(m, &plan)?;
        Ok(Some(
            self.judging_off(m, &off, repo, &facts)
                .by_verb(crate::diagnosis::ACTIVATE)
                .bind("<project>", &self.project)
                .judge(m.vcs),
        ))
    }

    /// The lines `rwv status` reports for the member: its alignment, when it
    /// is not aligned or carries a fact, and the pin state it was detached for.
    pub fn status_lines(
        &self,
        m: &Member<'_>,
        lock: Option<&LockFile>,
        repo: &dyn std::fmt::Display,
    ) -> anyhow::Result<Vec<Judged>> {
        let plan = self.plan(m, lock)?;
        let facts: Vec<Fact> = match &plan.target {
            Target::Detached { why, .. } => why.fact().into_iter().collect(),
            _ => Vec::new(),
        };
        let mut lines = Vec::new();
        if self.is_aligned(m, &plan)? {
            if !facts.is_empty() {
                let mut judging = State::AlignmentAligned.of(repo).member().checkout(m.dir);
                for fact in facts {
                    judging = judging.fact(fact);
                }
                lines.push(judging.judge(m.vcs));
            }
        } else if !matches!(plan.target, Target::AsFound) {
            let off = self.standing_of(m, &plan)?;
            lines.push(self.judge_off(m, &off, repo, &facts));
        } else if matches!(plan.pin, Pin::None)
            && !matches!(m.vcs.head_attachment(m.dir)?, HeadAttachment::Detached(_))
        {
            lines.push(
                State::AlignmentUnaligned
                    .of(repo)
                    .member()
                    .checkout(m.dir)
                    .bind("<m>", m.path)
                    .judge(m.vcs),
            );
        }
        lines.extend(self.notes(m, &plan, repo));
        Ok(lines)
    }

    /// The pointer state after members were aligned for the checked-out
    /// branch.
    pub fn next_state(&self, manifest: &Manifest) -> WorkspaceState {
        self.next_state_storing(manifest, true)
    }

    /// [`Self::next_state`], keeping the baseline an entry change is reported
    /// against where `advance_baseline` is false: a change still reported
    /// stays reported.
    pub fn next_state_storing(
        &self,
        manifest: &Manifest,
        advance_baseline: bool,
    ) -> WorkspaceState {
        let mut next = self.state.clone();
        let mode = match &self.mode {
            Mode::Detached => {
                next.aligned = Some(Aligned::Detached);
                return next;
            }
            Mode::Feature { .. } => crate::records::Mode::Feature,
            Mode::Declared => crate::records::Mode::Declared,
        };
        let Some(branch) = &self.checked_out else {
            return next;
        };
        next.aligned = Some(Aligned::Branch(branch.clone()));
        let current: BTreeMap<String, RepoEntry> = manifest
            .iter_entries()
            .map(|(path, entry)| (path.to_string(), entry.clone()))
            .collect();
        let baseline = match next.stored.remove(branch) {
            Some(held) if !advance_baseline => held.baseline,
            _ => current.clone(),
        };
        next.stored.insert(
            branch.clone(),
            Stored {
                mode,
                entries: current,
                baseline,
            },
        );
        next
    }
}
