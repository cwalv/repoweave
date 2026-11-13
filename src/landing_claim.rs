//! The claim a landing holds on its target, `T`, for as long as its operation
//! stands: from before it reads `T`'s tips until it released `T` at its
//! retire, ended, or was aborted. A landing stopped on a conflict or a lost
//! race still holds it, so no other landing builds on what it moved.
//!
//! Landings into one target run one at a time: a second one waits on the
//! claim, and then reads the tips the first one left. The compare-and-swap of
//! each move stays the guard against everything that takes no claim.
//!
//! The claim names its op and the workspace holding that op's record, which
//! is written before the claim is taken and removed only after it is
//! released. A claim whose workspace is here and records no such op is dead,
//! with no process id consulted, and the next landing takes it over; a claim
//! whose op is recorded is never taken from it. A claim whose workspace is
//! not here to read is never judged dead: a landing waits on it and refuses,
//! and `rwv doctor --fix` releases it. Nor is a claim or takeover guard whose
//! bytes this rwv does not read: `rwv doctor --fix
//! --release-unreadable-claims` releases it, on the operator's word that no
//! operation of another release holds it.

use crate::durable_file::CreateNewError;
use crate::manifest::{ProjectName, RepoKey};
use crate::op_state::OpId;
use crate::path_spelling::CanonicalPath;
use crate::refusal::RefusalKind;
use crate::state_file::{StateFile, StatePath};
use crate::vcs::Vcs;
use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::{Duration, Instant};

pub(crate) const LANDING_CLAIM_FILE: &str = ".rwv-landing.lock";
const CLAIMS_DIR: &str = "rwv-landings";

/// What a claim file holds: the op holding the claim and the workspace that
/// holds the op's record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Holder {
    op: OpId,
    owner: CanonicalPath,
}

impl Holder {
    fn bytes(&self) -> anyhow::Result<Vec<u8>> {
        serde_json::to_vec(self).context("failed to serialize a landing claim")
    }

    fn parse(bytes: &[u8]) -> Option<Self> {
        serde_json::from_slice(bytes).ok()
    }
}

/// A claim on one target branch of one project, held by one op. It is
/// released by [`LandingClaim::release`] or by [`LandingClaim::release_held_by`]
/// and by nothing else: dropping it, or the process ending, leaves it held.
#[derive(Debug)]
pub struct LandingClaim {
    path: StatePath,
    op: OpId,
    held_before: bool,
}

fn claims_dir(vcs: &dyn Vcs, project_dir: &Path) -> anyhow::Result<std::path::PathBuf> {
    let store = vcs.store_of(project_dir).with_context(|| {
        format!(
            "{} has no object store to hold a landing claim in",
            project_dir.display()
        )
    })?;
    Ok(store.join(CLAIMS_DIR))
}

/// Where the claim on `target` of `project` lives: in the object store of the
/// project repo at `project_dir`, one directory per target, named by a digest
/// so any branch name spells a short file name on every platform.
fn claim_path(
    vcs: &dyn Vcs,
    project_dir: &Path,
    project: &ProjectName,
    target: &str,
) -> anyhow::Result<StatePath> {
    let spelled = digest(format!("{}\n{target}", project.as_str()).as_bytes());
    Ok(StateFile::LandingClaim.in_dir(&claims_dir(vcs, project_dir)?.join(spelled)))
}

/// FNV-1a, 128 bits, in hex. Two targets that spelled one digest would share
/// a claim and land one at a time; nothing else follows from a collision.
fn digest(bytes: &[u8]) -> String {
    let mut hash: u128 = 0x6c62_272e_07bb_0142_62b8_2175_6295_c58d;
    for b in bytes {
        hash ^= u128::from(*b);
        hash = hash.wrapping_mul(0x0000_0000_0100_0000_0000_0000_0000_013b);
    }
    format!("{hash:032x}")
}

/// Whether the claim `seen` holds belongs to a standing op.
enum Standing {
    Live(Holder),
    /// Its owner workspace is here and records no such op.
    Dead(Holder),
    /// Its owner workspace is not here to read: on another mount, outside a
    /// sandbox, or deleted. Never judged dead.
    Unreachable(Holder),
    /// The file names no op this rwv reads.
    Unread(String),
    /// The file, the claim or a takeover guard beside it, cannot be read at
    /// all: whose it is cannot be told, so nothing here releases it.
    Unopenable {
        file: std::path::PathBuf,
        why: String,
    },
}

fn standing(seen: &[u8]) -> Standing {
    match Holder::parse(seen) {
        None => Standing::Unread(String::from_utf8_lossy(seen).trim().to_owned()),
        Some(holder) => match crate::op_state::records_op(holder.owner.as_path(), &holder.op) {
            Some(false) if crate::workspace::is_workspace_root(holder.owner.as_path()) => {
                Standing::Dead(holder)
            }
            Some(false) => Standing::Unreachable(holder),
            Some(true) | None => Standing::Live(holder),
        },
    }
}

impl LandingClaim {
    /// Claim `target` for the landing `op`, whose record `owner` holds.
    ///
    /// A claim `op` already holds is taken as it is. A claim whose op is no
    /// longer recorded is taken over. A claim another standing op holds is
    /// waited on for as long as that op's record keeps changing, and up to
    /// the shared claim budget (`durable_file::CLAIM_WAIT`) past its last
    /// change; then the landing refuses, naming the op and where its exits
    /// run.
    pub fn take(
        vcs: &dyn Vcs,
        project_dir: &Path,
        project: &ProjectName,
        target: &str,
        op: &OpId,
        owner: &Path,
    ) -> anyhow::Result<Self> {
        take_within(
            claim_path(vcs, project_dir, project, target)?,
            target,
            op,
            owner,
            crate::durable_file::CLAIM_WAIT,
        )
    }

    /// Remove the claim, if it still names this op.
    pub fn release(self) -> anyhow::Result<()> {
        release_if_held(&self.path, &self.op)
    }

    /// Undo this take: release the claim unless its op held it before it.
    pub fn put_back(self) -> anyhow::Result<()> {
        if self.held_before {
            return Ok(());
        }
        self.release()
    }

    /// Remove every landing claim `op` holds in the store of `project_dir`,
    /// whatever target it is on: what `rwv abort` does, whether or not the
    /// op's record names its target.
    pub fn release_held_by(vcs: &dyn Vcs, project_dir: &Path, op: &OpId) -> anyhow::Result<()> {
        for claim in claim_dirs(&claims_dir(vcs, project_dir)?)? {
            release_takeovers_held_by(&claim, op)?;
            release_if_held(&StateFile::LandingClaim.in_dir(&claim), op)?;
        }
        Ok(())
    }
}

/// Refuse moving, deleting, renaming or re-pointing any of `branches` of
/// `project` while a claim another op holds names it: the target branch the
/// claim is on, or a branch its op's record lands into or carries. `own` is the
/// op making the move, whose own claims never refuse it.
pub fn refuse_moving_claimed(
    vcs: &dyn Vcs,
    project_dir: &Path,
    project: &ProjectName,
    branches: &[(RepoKey, String)],
    own: Option<&OpId>,
) -> anyhow::Result<()> {
    if branches.is_empty() {
        return Ok(());
    }
    refuse_claimed_in(&claims_dir(vcs, project_dir)?, project, branches, own)
}

fn refuse_claimed_in(
    dir: &Path,
    project: &ProjectName,
    branches: &[(RepoKey, String)],
    own: Option<&OpId>,
) -> anyhow::Result<()> {
    for claim in claim_dirs(dir)? {
        let path = StateFile::LandingClaim.in_dir(&claim);
        let read = match path.read() {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            read => read,
        };
        let spelled = claim
            .file_name()
            .map(|name| name.to_string_lossy().into_owned());
        let on_target = branches.iter().find(|(repo, branch)| {
            *repo == RepoKey::Project
                && spelled.as_deref()
                    == Some(&digest(
                        format!("{}\n{branch}", project.as_str()).as_bytes(),
                    ))
        });
        let judged = match read {
            Ok(seen) => standing(&seen),
            Err(e) => Standing::Unopenable {
                file: path.reported(),
                why: format!("{e:#}"),
            },
        };
        let holder = match judged {
            Standing::Dead(_) => continue,
            Standing::Live(holder) | Standing::Unreachable(holder) => {
                if Some(&holder.op) == own {
                    continue;
                }
                holder
            }
            Standing::Unread(text) => match on_target {
                None => continue,
                Some((_, branch)) => crate::refuse!(
                    RefusalKind::ClaimUnreadable,
                    "a landing into '{branch}' holds it, and {path} names it in a form this rwv \
                     does not read ({text}): an rwv of another release took it, or the file is \
                     corrupt. End that landing with the release that started it, resuming or \
                     aborting it, then rerun. If no such landing stands, `rwv doctor --fix \
                     --release-unreadable-claims` run here releases the claim; a live landing \
                     of another release would lose it."
                ),
            },
            Standing::Unopenable { file, why } => match on_target {
                None => continue,
                Some((_, branch)) => {
                    return Err(unopenable_refusal(&format!("'{branch}'"), &file, &why))
                }
            },
        };
        let recorded = crate::op_state::read_owner(holder.owner.as_path())
            .ok()
            .flatten()
            .filter(|record| record.id == holder.op);
        let named = recorded
            .as_ref()
            .map(crate::op_state::OwnerRecord::claimed_branches)
            .unwrap_or_default();
        let Some((_, branch)) =
            on_target.or_else(|| branches.iter().find(|wanted| named.contains(wanted)))
        else {
            continue;
        };
        crate::refuse!(
            RefusalKind::StateClaimHeld,
            "an operation landing into '{branch}' holds it: op {op}, recorded in {owner}. If it \
             is running, rerun once it ends. If it stopped, either of these, run in {owner}, \
             ends it, and then this runs:\n  {resume}\n  rwv abort",
            op = holder.op,
            owner = holder.owner.as_path().display(),
            resume = crate::op_state::resume_command(
                recorded.map_or(crate::op_state::OpVerb::Land, |record| record.verb)
            ),
        );
    }
    Ok(())
}

/// The directory of each claim in `dir`, passing over anything else there.
fn claim_dirs(dir: &Path) -> anyhow::Result<Vec<std::path::PathBuf>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => {
            return Err(anyhow::Error::new(e).context(format!(
                "failed to read the landing claims in {}",
                dir.display()
            )))
        }
    };
    let mut dirs = Vec::new();
    for entry in entries {
        let entry = entry.with_context(|| format!("failed to read {}", dir.display()))?;
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            dirs.push(entry.path());
        }
    }
    Ok(dirs)
}

/// A claim whose owner workspace is not here to read, as `rwv doctor` reports
/// it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnreachableClaim {
    /// The directory holding the claim file.
    pub dir: std::path::PathBuf,
    pub op: OpId,
    pub owner: std::path::PathBuf,
}

/// Every claim in the store of `repo_dir` whose owner workspace is not here to
/// read.
pub fn unreachable_claims(vcs: &dyn Vcs, repo_dir: &Path) -> Vec<UnreachableClaim> {
    let Ok(dir) = claims_dir(vcs, repo_dir) else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let seen = StateFile::LandingClaim.in_dir(&entry.path()).read().ok()?;
            match standing(&seen) {
                Standing::Unreachable(holder) => Some(UnreachableClaim {
                    dir: entry.path(),
                    op: holder.op,
                    owner: holder.owner.into_path_buf(),
                }),
                _ => None,
            }
        })
        .collect()
}

/// Release `claim`, which `rwv doctor --fix` was asked to clear, if it still
/// names the same op and its owner is still not here to read. `fixer` is the
/// workspace the fix runs in, named as the holder of the guard it removes the
/// claim under. Returns whether the claim is gone.
pub fn release_unreachable(claim: &UnreachableClaim, fixer: &Path) -> anyhow::Result<bool> {
    let path = StateFile::LandingClaim.in_dir(&claim.dir);
    let seen = match path.read() {
        Ok(seen) => seen,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(true),
        Err(e) => {
            return Err(anyhow::Error::new(e).context(format!("failed to read the claim {path}")))
        }
    };
    match standing(&seen) {
        Standing::Unreachable(holder) if holder.op == claim.op => {}
        _ => return Ok(false),
    }
    let mine = Holder {
        op: OpId::new_now(),
        owner: CanonicalPath::of(fixer),
    };
    Ok(remove_dead(Removable::Claim(&path), &seen, &claim.op, &mine)?.is_none())
}

/// A claim, or a takeover guard beside one, holding bytes this rwv does not
/// read, as `rwv doctor` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnreadableClaim {
    pub file: std::path::PathBuf,
    /// The bytes it held when it was reported.
    pub seen: Vec<u8>,
}

/// Every claim and takeover guard in the store of `repo_dir` whose bytes this
/// rwv does not read.
pub fn unreadable_claims(vcs: &dyn Vcs, repo_dir: &Path) -> Vec<UnreadableClaim> {
    match claims_dir(vcs, repo_dir) {
        Ok(dir) => unreadable_in(&dir),
        Err(_) => Vec::new(),
    }
}

/// A claim, or a takeover guard beside one, that this rwv cannot read at
/// all, as `rwv doctor` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnopenableClaim {
    pub file: std::path::PathBuf,
    pub error: String,
}

/// Every claim and takeover guard in the store of `repo_dir` that this rwv
/// cannot read.
pub fn unopenable_claims(vcs: &dyn Vcs, repo_dir: &Path) -> Vec<UnopenableClaim> {
    match claims_dir(vcs, repo_dir) {
        Ok(dir) => unopenable_in(&dir),
        Err(_) => Vec::new(),
    }
}

fn unopenable_in(dir: &Path) -> Vec<UnopenableClaim> {
    claim_files_in(dir)
        .into_iter()
        .filter_map(|file| match std::fs::read(&file) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Some(UnopenableClaim {
                error: format!("{e:#}"),
                file,
            }),
            _ => None,
        })
        .collect()
}

/// Every claim file and takeover guard under the claims directory `dir`.
fn claim_files_in(dir: &Path) -> Vec<std::path::PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .flat_map(|entry| {
            let claim = StateFile::LandingClaim.in_dir(&entry.path()).reported();
            std::iter::once(claim).chain(takeover_guards(&entry.path()))
        })
        .collect()
}

fn unreadable_in(dir: &Path) -> Vec<UnreadableClaim> {
    claim_files_in(dir)
        .into_iter()
        .filter_map(|file| {
            let seen = std::fs::read(&file).ok()?;
            Holder::parse(&seen)
                .is_none()
                .then_some(UnreadableClaim { file, seen })
        })
        .collect()
}

/// Release `claim`, which `rwv doctor --fix --release-unreadable-claims` was
/// asked to clear, if it still holds the bytes it was reported with: one that
/// changed since, readable again or not, is left for the next report. Returns
/// whether it is gone.
pub fn release_unreadable(claim: &UnreadableClaim) -> anyhow::Result<bool> {
    match std::fs::read(&claim.file) {
        Ok(now) if now == claim.seen => {}
        Ok(_) => return Ok(false),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(true),
        Err(e) => {
            return Err(anyhow::Error::new(e)
                .context(format!("failed to read the claim {}", claim.file.display())))
        }
    }
    crate::durable_file::remove_named(&claim.file)
        .with_context(|| format!("failed to release the claim {}", claim.file.display()))?;
    Ok(true)
}

fn take_within(
    path: StatePath,
    target: &str,
    op: &OpId,
    owner: &Path,
    wait: Duration,
) -> anyhow::Result<LandingClaim> {
    let mine = Holder {
        op: op.clone(),
        owner: CanonicalPath::of(owner),
    };
    let bytes = mine.bytes()?;
    let mut last_seen: Option<(Vec<u8>, Option<Vec<u8>>)> = None;
    let mut deadline = Instant::now() + wait;
    let mut announced: Option<String> = None;
    loop {
        match path.create(&bytes) {
            Ok(()) => {
                sweep_dead_takeovers(path.dir(), &mine);
                return Ok(LandingClaim {
                    path,
                    op: op.clone(),
                    held_before: false,
                });
            }
            Err(CreateNewError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir_all(path.dir())
                    .with_context(|| format!("failed to make {}", path.dir().display()))?;
                continue;
            }
            Err(CreateNewError::Io(e)) => {
                return Err(
                    anyhow::Error::new(e).context(format!("failed to claim '{target}' at {path}"))
                )
            }
            Err(CreateNewError::NoExclusivePublish(e)) => return Err(e),
            Err(CreateNewError::AlreadyExists) => {}
        }
        let (seen, judged) = match path.read() {
            Ok(seen) => {
                let judged = standing(&seen);
                (seen, judged)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => (
                format!("{e:#}").into_bytes(),
                Standing::Unopenable {
                    file: path.reported(),
                    why: format!("{e:#}"),
                },
            ),
        };
        let blocking = match judged {
            Standing::Live(holder) if holder.op == *op => {
                return Ok(LandingClaim {
                    path,
                    op: op.clone(),
                    held_before: true,
                })
            }
            Standing::Dead(dead) => {
                match remove_dead(Removable::Claim(&path), &seen, &dead.op, &mine)
                    .with_context(|| format!("failed to take over the dead claim {path}"))?
                {
                    None => continue,
                    Some(blocking) => blocking,
                }
            }
            other => other,
        };
        let whom = waiting_on(&blocking);
        if announced.as_ref() != Some(&whom) {
            eprintln!(
                "waiting on '{target}': {whom} This refuses once it shows no progress for {}s.",
                wait.as_secs()
            );
            announced = Some(whom);
        }
        let progress = match &blocking {
            Standing::Live(holder) | Standing::Dead(holder) => {
                crate::op_state::OwnerRecord::path_in(holder.owner.as_path())
                    .read()
                    .ok()
            }
            Standing::Unreachable(_) | Standing::Unread(_) | Standing::Unopenable { .. } => None,
        };
        let now = (seen, progress);
        if last_seen.as_ref() != Some(&now) {
            last_seen = Some(now);
            deadline = Instant::now() + wait;
        }
        if Instant::now() >= deadline {
            match blocking {
                Standing::Unreachable(holder) => crate::refuse!(
                    RefusalKind::ClaimOwnerUnreachable,
                    "another operation moving '{target}' holds it: op {op}, recorded in {owner}, \
                     which is not here to read, so whether that operation still stands cannot be \
                     told from here. If it runs where {owner} is, end it there. If it is gone, \
                     `rwv doctor --fix` run here releases its claim, and then this landing runs.",
                    op = holder.op,
                    owner = holder.owner.as_path().display(),
                ),
                Standing::Live(holder) | Standing::Dead(holder) => crate::refuse!(
                    RefusalKind::StateClaimHeld,
                    "another operation moving '{target}' holds it: op {op}, recorded in {owner}. \
                     If it is running, rerun once it ends. If it stopped, either of these, run \
                     in {owner}, ends it, and then this landing runs:\n  {resume}\n  rwv abort",
                    op = holder.op,
                    owner = holder.owner.as_path().display(),
                    resume = crate::op_state::resume_command(
                        crate::op_state::read_owner(holder.owner.as_path())
                            .ok()
                            .flatten()
                            .map_or(crate::op_state::OpVerb::Land, |record| record.verb)
                    ),
                ),
                Standing::Unread(text) => crate::refuse!(
                    RefusalKind::ClaimUnreadable,
                    "another landing into '{target}' holds it, and {path} or a takeover guard \
                     beside it names it in a form this rwv does not read ({text}): an rwv of \
                     another release took it, or the file is corrupt. End that landing with the \
                     release that started it, resuming or aborting it, then rerun. If no such \
                     landing stands, `rwv doctor --fix --release-unreadable-claims` run here \
                     releases the claim; a live landing of another release would lose it."
                ),
                Standing::Unopenable { file, why } => {
                    return Err(unopenable_refusal(&format!("'{target}'"), &file, &why))
                }
            }
        }
        crate::test_pause::point("landing-claim-held");
        std::thread::sleep(crate::durable_file::CLAIM_POLL);
    }
}

/// What a waiting command prints of whom it waits on, and what ends the
/// wait.
fn waiting_on(blocking: &Standing) -> String {
    match blocking {
        Standing::Live(holder) | Standing::Dead(holder) => format!(
            "op {op} holds it, recorded in {owner}. The wait ends when that operation ends, \
             or when `{resume}` or `rwv abort`, run in {owner}, ends it.",
            op = holder.op,
            owner = holder.owner.as_path().display(),
            resume = resume_of(holder),
        ),
        Standing::Unreachable(holder) => format!(
            "op {op} holds it, recorded in {owner}, which is not here to read. The wait ends \
             when that operation ends where {owner} is.",
            op = holder.op,
            owner = holder.owner.as_path().display(),
        ),
        Standing::Unread(text) => format!(
            "an operation of another rwv release holds it ({text}). The wait ends when that \
             operation ends."
        ),
        Standing::Unopenable { file, why } => format!(
            "{} cannot be read ({why}). The wait ends when it goes or becomes readable.",
            file.display()
        ),
    }
}

/// The refusal for a claim or guard this rwv cannot read at all. No release is
/// offered: a claim rwv cannot read may be a live landing's, and its bytes
/// cannot be compared before a removal.
fn unopenable_refusal(moving: &str, file: &Path, why: &str) -> anyhow::Error {
    crate::refusal::refusal(
        RefusalKind::ClaimUnopenable,
        format!(
            "a landing may hold {moving}, but {file} cannot be read ({why}), so whose it is \
             cannot be told and rwv releases nothing it cannot read. If it is a file this user \
             may not read, a landing run as another user holds it: end that landing as that \
             user, or make the file readable to this one. If what stands there is not a file, \
             rwv did not put it there: remove it. Then rerun.",
            file = file.display()
        ),
    )
}

fn resume_of(holder: &Holder) -> String {
    crate::op_state::resume_command(
        crate::op_state::read_owner(holder.owner.as_path())
            .ok()
            .flatten()
            .map_or(crate::op_state::OpVerb::Land, |record| record.verb),
    )
    .to_string()
}

/// Remove `path`, which read `seen` when it was judged to name the dead op
/// `dead`, under the guard `<path>.takeover.<dead>` that `mine` creates
/// exclusively: only the guard's holder removes `path`, and only while it
/// still reads `seen`. While it reads `seen` nothing can replace it (no create
/// succeeds on an occupied path, and its dead op releases nothing), so the
/// re-read and the removal under the guard are one step to every other
/// remover. A guard whose holder is dead is itself removed this way first.
/// Returns what blocks the removal: a guard another standing op holds, or
/// one this rwv does not read; `None` once `path` is removed or gone.
fn remove_dead(
    path: Removable<'_>,
    seen: &[u8],
    dead: &OpId,
    mine: &Holder,
) -> anyhow::Result<Option<Standing>> {
    let guard = path.guard(dead);
    loop {
        crate::test_pause::point("landing-claim-dead-seen");
        match crate::durable_file::create_new(&guard, &mine.bytes()?) {
            Ok(()) => break,
            Err(CreateNewError::AlreadyExists) => {}
            // The claim's directory went with the release of the last claim in
            // it, and the dead claim with it: nothing is left to take over.
            Err(CreateNewError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(None)
            }
            Err(CreateNewError::Io(e)) => return Err(e.into()),
            Err(CreateNewError::NoExclusivePublish(e)) => return Err(e),
        }
        let held = match std::fs::read(&guard) {
            Ok(held) => held,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                return Ok(Some(Standing::Unopenable {
                    file: guard,
                    why: format!("{e:#}"),
                }))
            }
        };
        match standing(&held) {
            Standing::Live(holder) if holder.op == mine.op => break,
            Standing::Dead(holder) => {
                if let Some(blocking) =
                    remove_dead(Removable::Guard(&guard), &held, &holder.op, mine)?
                {
                    return Ok(Some(blocking));
                }
            }
            blocking => return Ok(Some(blocking)),
        }
    }
    crate::test_pause::point("landing-claim-takeover");
    if path.read().is_ok_and(|now| now == seen) {
        path.remove()?;
    }
    crate::durable_file::remove_named(&guard)?;
    Ok(None)
}

/// What [`remove_dead`] removes: the claim, or a takeover guard beside it.
#[derive(Clone, Copy)]
enum Removable<'a> {
    Claim(&'a StatePath),
    Guard(&'a Path),
}

impl Removable<'_> {
    fn read(self) -> std::io::Result<Vec<u8>> {
        match self {
            Self::Claim(path) => path.read(),
            Self::Guard(path) => std::fs::read(path),
        }
    }

    fn remove(self) -> std::io::Result<()> {
        match self {
            Self::Claim(path) => match path.remove() {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                removed => removed,
            },
            Self::Guard(path) => crate::durable_file::remove_named(path),
        }
    }

    fn guard(self, dead: &OpId) -> std::path::PathBuf {
        match self {
            Self::Claim(path) => path
                .dir()
                .join(format!("{LANDING_CLAIM_FILE}.{TAKEOVER}.{dead}")),
            Self::Guard(path) => {
                let mut name = path.as_os_str().to_owned();
                name.push(format!(".{TAKEOVER}.{dead}"));
                name.into()
            }
        }
    }
}

const TAKEOVER: &str = "takeover";

/// Whether `name` spells a takeover guard beside the claim: the claim's file
/// name followed by one or more `.takeover.<op>`.
fn is_takeover_guard(name: &str) -> bool {
    let Some(rest) = name.strip_prefix(LANDING_CLAIM_FILE) else {
        return false;
    };
    let parts: Vec<&str> = rest.split('.').collect();
    parts.len() >= 3
        && parts.len() % 2 == 1
        && parts[0].is_empty()
        && parts[1..]
            .chunks(2)
            .all(|pair| pair[0] == TAKEOVER && pair[1].parse::<OpId>().is_ok())
}

fn takeover_guards(dir: &Path) -> Vec<std::path::PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|entry| is_takeover_guard(&entry.file_name().to_string_lossy()))
        .map(|entry| entry.path())
        .collect()
}

/// Remove each takeover guard in `dir` whose holder is dead: one left by a
/// taker whose op ended without removing it. Run by a landing that just took
/// the claim; a guard it cannot remove is left for the next.
fn sweep_dead_takeovers(dir: &Path, mine: &Holder) {
    for guard in takeover_guards(dir) {
        let Ok(held) = std::fs::read(&guard) else {
            continue;
        };
        if let Standing::Dead(holder) = standing(&held) {
            let _ = remove_dead(Removable::Guard(&guard), &held, &holder.op, mine);
        }
    }
}

/// Remove each takeover guard in `dir` that `op` holds, left by a landing
/// killed while it took over a dead claim.
fn release_takeovers_held_by(dir: &Path, op: &OpId) -> anyhow::Result<()> {
    for guard in takeover_guards(dir) {
        if std::fs::read(&guard)
            .ok()
            .and_then(|held| Holder::parse(&held))
            .is_some_and(|holder| holder.op == *op)
        {
            crate::durable_file::remove_named(&guard)?;
        }
    }
    Ok(())
}

fn release_if_held(path: &StatePath, op: &OpId) -> anyhow::Result<()> {
    let seen = match path.read() {
        Ok(seen) => seen,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => {
            return Err(anyhow::Error::new(e).context(format!("failed to read the claim {path}")))
        }
    };
    if Holder::parse(&seen).is_none_or(|holder| holder.op != *op) {
        return Ok(());
    }
    match path.remove() {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(anyhow::Error::new(e).context(format!(
                "failed to release the landing claim {path}; the next landing into its target \
                 takes it over once op {op} is no longer recorded"
            )))
        }
    }
    let _ = std::fs::remove_dir(path.dir());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claim_in(dir: &Path) -> StatePath {
        StateFile::LandingClaim.in_dir(&dir.join("claims").join("t"))
    }

    fn op(n: u32) -> OpId {
        format!("{n}").parse().unwrap()
    }

    fn record(workspace: &Path, id: &OpId) {
        let record = crate::op_state::OwnerRecord::new_land(
            id,
            None,
            ProjectName::new("p").unwrap(),
            workspace,
            None,
            false,
        );
        crate::op_state::write_owner(workspace, &record).unwrap();
    }

    #[test]
    fn a_claim_whose_op_is_recorded_is_never_taken_from_it() {
        let tmp = crate::tempdir().unwrap();
        let owner = tmp.path().join("one");
        std::fs::create_dir_all(owner.join("projects")).unwrap();
        record(&owner, &op(1));
        let path = claim_in(tmp.path());
        let first = take_within(path.clone(), "t", &op(1), &owner, Duration::ZERO).unwrap();
        let second = take_within(path.clone(), "t", &op(2), tmp.path(), Duration::ZERO);
        assert!(crate::refusal::kind_of(&second.unwrap_err()) == Some(RefusalKind::StateClaimHeld));
        assert_eq!(
            Holder::parse(&path.read().unwrap()).unwrap().op,
            op(1),
            "the refused landing left the claim to its holder"
        );
        first.release().unwrap();
        assert!(!path.exists());
    }

    fn claims_on(tmp: &Path, target: &str) -> (std::path::PathBuf, StatePath) {
        let dir = tmp.join("claims");
        let path =
            StateFile::LandingClaim.in_dir(&dir.join(digest(format!("p\n{target}").as_bytes())));
        (dir, path)
    }

    fn landing_into(workspace: &Path, id: &OpId, target: &str, member_branch: &str) {
        std::fs::create_dir_all(workspace.join("projects")).unwrap();
        let mut record = crate::op_state::OwnerRecord::new_land(
            id,
            None,
            ProjectName::new("p").unwrap(),
            workspace,
            None,
            false,
        );
        let mut take_in = crate::op_state::TakeInRecord {
            sync_from: Some(crate::op_state::SyncFrom::Target(target.to_owned())),
            ..Default::default()
        };
        take_in.receiving.insert(
            RepoKey::Member(crate::manifest::RepoPath::new("app").unwrap()),
            crate::op_state::Receiving {
                branch: member_branch.to_owned(),
                tip: crate::vcs::CommitId::parse(&"a".repeat(40)).unwrap(),
                moves: crate::op_state::Moves::AsRef { create: false },
            },
        );
        record.take_in = Some(take_in);
        crate::op_state::write_owner(workspace, &record).unwrap();
    }

    fn refused(result: anyhow::Result<()>) -> bool {
        match result {
            Ok(()) => false,
            Err(e) => {
                assert_eq!(
                    crate::refusal::kind_of(&e),
                    Some(RefusalKind::StateClaimHeld),
                    "{e:#}"
                );
                true
            }
        }
    }

    #[test]
    fn a_move_of_a_branch_a_live_claim_names_is_refused_but_not_to_its_holder() {
        let tmp = crate::tempdir().unwrap();
        let owner = tmp.path().join("one");
        landing_into(&owner, &op(1), "main", "trunk");
        let (dir, path) = claims_on(tmp.path(), "main");
        take_within(path, "main", &op(1), &owner, Duration::ZERO).unwrap();
        let p = ProjectName::new("p").unwrap();
        let app = RepoKey::Member(crate::manifest::RepoPath::new("app").unwrap());
        let check = |repo: &RepoKey, branch: &str, own: Option<&OpId>| {
            refuse_claimed_in(&dir, &p, &[(repo.clone(), branch.to_owned())], own)
        };
        assert!(refused(check(&RepoKey::Project, "main", None)));
        assert!(refused(check(&app, "trunk", None)));
        assert!(!refused(check(&RepoKey::Project, "other", None)));
        assert!(!refused(check(&app, "main", None)));
        assert!(!refused(check(&RepoKey::Project, "main", Some(&op(1)))));
        assert!(!refused(check(&app, "trunk", Some(&op(1)))));
    }

    /// A claim no rwv can read at all: something that is not a file at its
    /// name, since a mode bit does not stop a test run as root.
    fn unopenable_claim(tmp: &Path, target: &str) -> (std::path::PathBuf, StatePath) {
        let (dir, path) = claims_on(tmp, target);
        std::fs::create_dir_all(path.reported()).unwrap();
        (dir, path)
    }

    #[test]
    fn a_claim_that_cannot_be_read_is_refused_by_name_never_offered_a_release() {
        let tmp = crate::tempdir().unwrap();
        let (dir, path) = unopenable_claim(tmp.path(), "main");
        let next = tmp.path().join("next");
        std::fs::create_dir_all(next.join("projects")).unwrap();
        record(&next, &op(2));

        let refused = take_within(path.clone(), "main", &op(2), &next, Duration::ZERO).unwrap_err();
        assert_eq!(
            crate::refusal::kind_of(&refused),
            Some(RefusalKind::ClaimUnopenable)
        );
        let text = format!("{refused:#}");
        assert!(text.contains(&path.to_string()), "{text}");
        assert!(!text.contains("--release-unreadable-claims"), "{text}");

        assert_eq!(unreadable_in(&dir), Vec::new());
        let reported = unopenable_in(&dir);
        assert_eq!(reported.len(), 1, "{reported:?}");
        assert_eq!(reported[0].file, path.reported());
    }

    #[test]
    fn a_takeover_guard_that_cannot_be_read_is_refused_by_name() {
        let tmp = crate::tempdir().unwrap();
        let (path, _, dead) = dead_claim(tmp.path());
        let guard = path
            .dir()
            .join(format!("{LANDING_CLAIM_FILE}.takeover.{}", dead.op));
        std::fs::create_dir_all(&guard).unwrap();
        let next = tmp.path().join("next");
        std::fs::create_dir_all(next.join("projects")).unwrap();
        record(&next, &op(3));

        let refused = take_within(path.clone(), "t", &op(3), &next, Duration::ZERO).unwrap_err();
        assert_eq!(
            crate::refusal::kind_of(&refused),
            Some(RefusalKind::ClaimUnopenable)
        );
        assert!(format!("{refused:#}").contains(&guard.display().to_string()));
        let reported = unopenable_in(&tmp.path().join("claims"));
        assert_eq!(
            reported.iter().map(|c| &c.file).collect::<Vec<_>>(),
            vec![&guard]
        );
    }

    #[test]
    fn a_move_of_a_branch_an_unreadable_claim_may_name_is_refused_as_the_landing_is() {
        let tmp = crate::tempdir().unwrap();
        let p = ProjectName::new("p").unwrap();
        let main = [(RepoKey::Project, "main".to_owned())];

        let (dir, path) = claims_on(tmp.path(), "main");
        std::fs::create_dir_all(path.dir()).unwrap();
        std::fs::write(path.reported(), b"garbage").unwrap();
        let refused = refuse_claimed_in(&dir, &p, &main, None).unwrap_err();
        assert_eq!(
            crate::refusal::kind_of(&refused),
            Some(RefusalKind::ClaimUnreadable)
        );
        assert!(format!("{refused:#}").contains("--release-unreadable-claims"));
        std::fs::remove_file(path.reported()).unwrap();

        std::fs::create_dir_all(path.reported()).unwrap();
        let refused = refuse_claimed_in(&dir, &p, &main, None).unwrap_err();
        assert_eq!(
            crate::refusal::kind_of(&refused),
            Some(RefusalKind::ClaimUnopenable)
        );
        assert!(
            refuse_claimed_in(&dir, &p, &[(RepoKey::Project, "other".to_owned())], None).is_ok()
        );
    }

    #[test]
    fn a_claim_whose_op_is_no_longer_recorded_names_nothing_to_move() {
        let tmp = crate::tempdir().unwrap();
        let owner = tmp.path().join("one");
        landing_into(&owner, &op(1), "main", "trunk");
        let (dir, path) = claims_on(tmp.path(), "main");
        take_within(path, "main", &op(1), &owner, Duration::ZERO).unwrap();
        crate::op_state::clear_all_at(&owner);
        let p = ProjectName::new("p").unwrap();
        assert!(!refused(refuse_claimed_in(
            &dir,
            &p,
            &[(RepoKey::Project, "main".to_owned())],
            None
        )));
    }

    #[test]
    fn a_stray_file_among_the_claims_is_passed_over() {
        let tmp = crate::tempdir().unwrap();
        let owner = tmp.path().join("one");
        landing_into(&owner, &op(1), "main", "trunk");
        let (dir, path) = claims_on(tmp.path(), "main");
        take_within(path.clone(), "main", &op(1), &owner, Duration::ZERO).unwrap();
        std::fs::write(dir.join("stray"), b"x").unwrap();
        assert_eq!(claim_dirs(&dir).unwrap(), vec![path.dir().to_path_buf()]);
        assert!(refused(refuse_claimed_in(
            &dir,
            &ProjectName::new("p").unwrap(),
            &[(RepoKey::Project, "main".to_owned())],
            None
        )));
    }

    #[test]
    fn a_holder_whose_record_keeps_changing_is_waited_on_past_the_budget() {
        let tmp = crate::tempdir().unwrap();
        let owner = tmp.path().join("one");
        std::fs::create_dir_all(owner.join("projects")).unwrap();
        record(&owner, &op(1));
        let path = claim_in(tmp.path());
        let held = take_within(path.clone(), "t", &op(1), &owner, Duration::ZERO).unwrap();
        let budget = Duration::from_millis(1000);
        let holder = {
            let owner = owner.clone();
            std::thread::spawn(move || {
                for step in 0..40 {
                    let mut record = crate::op_state::OwnerRecord::new_land(
                        &op(1),
                        None,
                        ProjectName::new("p").unwrap(),
                        &owner,
                        None,
                        step % 2 == 0,
                    );
                    record.take_in = Some(crate::op_state::TakeInRecord {
                        message: Some(format!("{step}")),
                        ..Default::default()
                    });
                    crate::op_state::write_owner(&owner, &record).unwrap();
                    std::thread::sleep(Duration::from_millis(100));
                }
                held.release().unwrap();
            })
        };
        let started = Instant::now();
        let taken = take_within(path, "t", &op(2), tmp.path(), budget);
        holder.join().unwrap();
        assert!(taken.is_ok(), "{:#}", taken.unwrap_err());
        assert!(
            started.elapsed() > budget * 3,
            "the wait outlasted the budget"
        );
    }

    #[test]
    fn a_holder_that_shows_no_progress_is_refused_once_the_budget_runs_out() {
        let tmp = crate::tempdir().unwrap();
        let owner = tmp.path().join("one");
        std::fs::create_dir_all(owner.join("projects")).unwrap();
        record(&owner, &op(1));
        let path = claim_in(tmp.path());
        let _held = take_within(path.clone(), "t", &op(1), &owner, Duration::ZERO).unwrap();
        let budget = Duration::from_millis(200);
        let started = Instant::now();
        let taken = take_within(path, "t", &op(2), tmp.path(), budget);
        assert!(refused(taken.map(|_| ())));
        assert!(started.elapsed() >= budget);
    }

    #[test]
    fn a_claim_whose_op_is_no_longer_recorded_is_taken_over() {
        let tmp = crate::tempdir().unwrap();
        let gone = tmp.path().join("gone");
        std::fs::create_dir_all(gone.join("projects")).unwrap();
        let path = claim_in(tmp.path());
        let _orphan = take_within(path.clone(), "t", &op(1), &gone, Duration::ZERO).unwrap();
        take_within(path.clone(), "t", &op(2), tmp.path(), Duration::ZERO).unwrap();
        assert_eq!(Holder::parse(&path.read().unwrap()).unwrap().op, op(2));

        let moved_on = tmp.path().join("moved-on");
        std::fs::create_dir_all(moved_on.join("projects")).unwrap();
        record(&moved_on, &op(4));
        let path = claim_in(&tmp.path().join("other"));
        let _stale = take_within(path.clone(), "t", &op(3), &moved_on, Duration::ZERO).unwrap();
        take_within(path.clone(), "t", &op(5), tmp.path(), Duration::ZERO).unwrap();
        assert_eq!(Holder::parse(&path.read().unwrap()).unwrap().op, op(5));
    }

    #[test]
    fn a_release_removes_only_its_own_claim() {
        let tmp = crate::tempdir().unwrap();
        let owner = tmp.path().join("one");
        std::fs::create_dir_all(owner.join("projects")).unwrap();
        record(&owner, &op(2));
        let path = claim_in(tmp.path());
        let stale = LandingClaim {
            path: path.clone(),
            op: op(1),
            held_before: false,
        };
        let held = take_within(path.clone(), "t", &op(2), &owner, Duration::ZERO).unwrap();
        stale.release().unwrap();
        assert_eq!(Holder::parse(&path.read().unwrap()).unwrap().op, op(2));
        held.release().unwrap();
        assert!(!path.exists());
    }

    fn holder(op: OpId, owner: &Path) -> Holder {
        Holder {
            op,
            owner: CanonicalPath::of(owner),
        }
    }

    fn dead_claim(tmp: &Path) -> (StatePath, Vec<u8>, Holder) {
        let gone = tmp.join("gone");
        std::fs::create_dir_all(gone.join("projects")).unwrap();
        let path = claim_in(tmp);
        let _dead = take_within(path.clone(), "t", &op(1), &gone, Duration::ZERO).unwrap();
        (path.clone(), path.read().unwrap(), holder(op(1), &gone))
    }

    #[test]
    fn a_taker_that_judged_a_claim_dead_never_removes_the_live_one_since_taken() {
        let tmp = crate::tempdir().unwrap();
        let (path, seen, dead) = dead_claim(tmp.path());
        let live = tmp.path().join("live");
        std::fs::create_dir_all(live.join("projects")).unwrap();
        record(&live, &op(2));
        assert!(remove_dead(
            Removable::Claim(&path),
            &seen,
            &dead.op,
            &holder(op(2), &live)
        )
        .unwrap()
        .is_none());
        let _held = take_within(path.clone(), "t", &op(2), &live, Duration::ZERO).unwrap();
        let late = tmp.path().join("late");
        std::fs::create_dir_all(late.join("projects")).unwrap();
        record(&late, &op(3));
        assert!(remove_dead(
            Removable::Claim(&path),
            &seen,
            &dead.op,
            &holder(op(3), &late)
        )
        .unwrap()
        .is_none());
        assert_eq!(Holder::parse(&path.read().unwrap()).unwrap().op, op(2));
    }

    #[test]
    fn a_dead_claim_another_live_taker_is_removing_is_left_to_it() {
        let tmp = crate::tempdir().unwrap();
        let (path, _, dead) = dead_claim(tmp.path());
        let first = tmp.path().join("first");
        std::fs::create_dir_all(first.join("projects")).unwrap();
        record(&first, &op(2));
        let lock = path
            .dir()
            .join(format!("{LANDING_CLAIM_FILE}.takeover.{}", dead.op));
        crate::durable_file::create_new(&lock, &holder(op(2), &first).bytes().unwrap()).unwrap();
        let second = tmp.path().join("second");
        std::fs::create_dir_all(second.join("projects")).unwrap();
        record(&second, &op(3));
        let refused = take_within(path.clone(), "t", &op(3), &second, Duration::ZERO);
        assert!(refused.is_err());
        assert_eq!(Holder::parse(&path.read().unwrap()).unwrap().op, op(1));
    }

    #[test]
    fn a_takeover_lock_whose_taker_died_is_passed_over() {
        let tmp = crate::tempdir().unwrap();
        let (path, _, dead) = dead_claim(tmp.path());
        let died = tmp.path().join("died");
        std::fs::create_dir_all(died.join("projects")).unwrap();
        let lock = path
            .dir()
            .join(format!("{LANDING_CLAIM_FILE}.takeover.{}", dead.op));
        crate::durable_file::create_new(&lock, &holder(op(2), &died).bytes().unwrap()).unwrap();
        let next = tmp.path().join("next");
        std::fs::create_dir_all(next.join("projects")).unwrap();
        record(&next, &op(3));
        take_within(path.clone(), "t", &op(3), &next, Duration::ZERO).unwrap();
        assert_eq!(Holder::parse(&path.read().unwrap()).unwrap().op, op(3));
    }

    #[test]
    fn a_takeover_guard_whose_taker_died_is_removed_by_the_next_take() {
        let tmp = crate::tempdir().unwrap();
        let path = claim_in(tmp.path());
        std::fs::create_dir_all(path.dir()).unwrap();
        let died = tmp.path().join("died");
        std::fs::create_dir_all(died.join("projects")).unwrap();
        let guard = Removable::Claim(&path).guard(&op(1));
        crate::durable_file::create_new(&guard, &holder(op(2), &died).bytes().unwrap()).unwrap();
        let nested = Removable::Guard(&guard).guard(&op(4));
        crate::durable_file::create_new(&nested, &holder(op(4), &died).bytes().unwrap()).unwrap();
        let next = tmp.path().join("next");
        std::fs::create_dir_all(next.join("projects")).unwrap();
        record(&next, &op(3));
        take_within(path.clone(), "t", &op(3), &next, Duration::ZERO).unwrap();
        assert!(!guard.exists(), "the dead taker's guard is gone");
        assert!(!nested.exists(), "a dead guard on a guard is gone");
        assert_eq!(
            takeover_guards(path.dir()),
            Vec::<std::path::PathBuf>::new()
        );
    }

    #[test]
    fn a_takeover_guard_a_stopped_op_holds_is_waited_on_and_named() {
        let tmp = crate::tempdir().unwrap();
        let (path, _, dead) = dead_claim(tmp.path());
        let stopped = tmp.path().join("stopped");
        std::fs::create_dir_all(stopped.join("projects")).unwrap();
        record(&stopped, &op(2));
        let guard = Removable::Claim(&path).guard(&dead.op);
        crate::durable_file::create_new(&guard, &holder(op(2), &stopped).bytes().unwrap()).unwrap();
        let next = tmp.path().join("next");
        std::fs::create_dir_all(next.join("projects")).unwrap();
        record(&next, &op(3));
        let refused = take_within(path.clone(), "t", &op(3), &next, Duration::ZERO).unwrap_err();
        assert!(crate::refusal::kind_of(&refused) == Some(RefusalKind::StateClaimHeld));
        assert!(
            format!("{refused:#}").contains(&format!("op {}", op(2))),
            "the refusal names the guard's holder: {refused:#}"
        );
        assert!(guard.exists());
        assert_eq!(Holder::parse(&path.read().unwrap()).unwrap().op, op(1));
    }

    #[test]
    fn only_a_claims_takeover_guards_read_as_guards() {
        assert!(is_takeover_guard(".rwv-landing.lock.takeover.12"));
        assert!(is_takeover_guard(
            ".rwv-landing.lock.takeover.12.takeover.7"
        ));
        assert!(!is_takeover_guard(".rwv-landing.lock.takeover.12.tmp.99.0"));
        assert!(!is_takeover_guard(".rwv-landing.lock"));
        assert!(!is_takeover_guard(".rwv-landing.lock.tmp.99.0"));
    }

    #[test]
    fn a_claim_whose_owner_is_not_here_is_never_taken_over_and_doctor_releases_it() {
        let tmp = crate::tempdir().unwrap();
        let elsewhere = tmp.path().join("elsewhere");
        std::fs::create_dir_all(elsewhere.join("projects")).unwrap();
        let path = claim_in(tmp.path());
        let _held = take_within(path.clone(), "t", &op(1), &elsewhere, Duration::ZERO).unwrap();
        std::fs::remove_dir_all(&elsewhere).unwrap();
        let next = tmp.path().join("next");
        std::fs::create_dir_all(next.join("projects")).unwrap();
        record(&next, &op(2));
        let refused = take_within(path.clone(), "t", &op(2), &next, Duration::ZERO).unwrap_err();
        assert!(crate::refusal::kind_of(&refused) == Some(RefusalKind::ClaimOwnerUnreachable));
        assert!(format!("{refused:#}").contains("rwv doctor --fix"));
        assert_eq!(Holder::parse(&path.read().unwrap()).unwrap().op, op(1));

        let unreachable = UnreachableClaim {
            dir: path.dir().to_path_buf(),
            op: op(1),
            owner: elsewhere.clone(),
        };
        assert!(!release_unreachable(
            &UnreachableClaim {
                op: op(9),
                ..unreachable.clone()
            },
            &next
        )
        .unwrap());
        assert!(path.exists(), "a fix for another op releases nothing");
        assert!(release_unreachable(&unreachable, &next).unwrap());
        assert!(!path.exists());
        take_within(path.clone(), "t", &op(2), &next, Duration::ZERO).unwrap();
    }

    #[test]
    fn an_unreadable_claim_is_never_taken_over_and_the_consented_fix_releases_it() {
        let tmp = crate::tempdir().unwrap();
        let path = claim_in(tmp.path());
        std::fs::create_dir_all(path.dir()).unwrap();
        std::fs::write(path.reported(), b"{\"format\": 2}").unwrap();
        let next = tmp.path().join("next");
        std::fs::create_dir_all(next.join("projects")).unwrap();
        record(&next, &op(2));

        let refused = take_within(path.clone(), "t", &op(2), &next, Duration::ZERO).unwrap_err();
        assert!(crate::refusal::kind_of(&refused) == Some(RefusalKind::ClaimUnreadable));
        assert!(format!("{refused:#}").contains("rwv doctor --fix --release-unreadable-claims"));
        assert_eq!(path.read().unwrap(), b"{\"format\": 2}");

        let reported = unreadable_in(&tmp.path().join("claims"));
        assert_eq!(
            reported,
            vec![UnreadableClaim {
                file: path.reported(),
                seen: b"{\"format\": 2}".to_vec(),
            }]
        );
        assert!(release_unreadable(&reported[0]).unwrap());
        assert!(!path.exists());
        take_within(path.clone(), "t", &op(2), &next, Duration::ZERO).unwrap();
    }

    #[test]
    fn an_unreadable_claim_that_changed_since_its_report_is_not_released() {
        let tmp = crate::tempdir().unwrap();
        let path = claim_in(tmp.path());
        std::fs::create_dir_all(path.dir()).unwrap();
        std::fs::write(path.reported(), b"garbage").unwrap();
        let [reported] = unreadable_in(&tmp.path().join("claims"))
            .try_into()
            .unwrap();

        let owner = tmp.path().join("owner");
        std::fs::create_dir_all(owner.join("projects")).unwrap();
        record(&owner, &op(1));
        std::fs::remove_file(path.reported()).unwrap();
        let live = take_within(path.clone(), "t", &op(1), &owner, Duration::ZERO).unwrap();

        assert!(!release_unreadable(&reported).unwrap());
        assert_eq!(Holder::parse(&path.read().unwrap()).unwrap().op, op(1));
        live.release().unwrap();
    }

    #[test]
    fn an_unreadable_takeover_guard_blocks_the_takeover_and_the_consented_fix_releases_it() {
        let tmp = crate::tempdir().unwrap();
        let (path, _, dead) = dead_claim(tmp.path());
        let guard = path
            .dir()
            .join(format!("{LANDING_CLAIM_FILE}.takeover.{}", dead.op));
        std::fs::write(&guard, b"not a holder").unwrap();
        let next = tmp.path().join("next");
        std::fs::create_dir_all(next.join("projects")).unwrap();
        record(&next, &op(3));

        let refused = take_within(path.clone(), "t", &op(3), &next, Duration::ZERO).unwrap_err();
        assert!(crate::refusal::kind_of(&refused) == Some(RefusalKind::ClaimUnreadable));
        let reported = unreadable_in(&tmp.path().join("claims"));
        assert_eq!(
            reported,
            vec![UnreadableClaim {
                file: guard.clone(),
                seen: b"not a holder".to_vec(),
            }]
        );
        assert!(release_unreadable(&reported[0]).unwrap());
        take_within(path.clone(), "t", &op(3), &next, Duration::ZERO).unwrap();
        assert_eq!(Holder::parse(&path.read().unwrap()).unwrap().op, op(3));
    }

    #[test]
    fn a_claim_a_sync_holds_is_judged_as_a_landings_is() {
        let tmp = crate::tempdir().unwrap();
        let carrying = tmp.path().join("carrying");
        std::fs::create_dir_all(carrying.join("projects")).unwrap();
        let sync = crate::op_state::OwnerRecord::new_sync(
            &op(1),
            None,
            ProjectName::new("p").unwrap(),
            &carrying,
            &carrying,
        );
        crate::op_state::write_owner(&carrying, &sync).unwrap();
        let path = claim_in(tmp.path());
        let _held = take_within(path.clone(), "t", &op(1), &carrying, Duration::ZERO).unwrap();
        let next = tmp.path().join("next");
        std::fs::create_dir_all(next.join("projects")).unwrap();
        record(&next, &op(2));
        let waited = take_within(path.clone(), "t", &op(2), &next, Duration::ZERO).unwrap_err();
        assert!(crate::refusal::kind_of(&waited) == Some(RefusalKind::StateClaimHeld));

        let moved = tmp.path().join("moved");
        std::fs::rename(&carrying, &moved).unwrap();
        let unreachable =
            take_within(path.clone(), "t", &op(2), &next, Duration::ZERO).unwrap_err();
        assert!(crate::refusal::kind_of(&unreachable) == Some(RefusalKind::ClaimOwnerUnreachable));
        assert_eq!(Holder::parse(&path.read().unwrap()).unwrap().op, op(1));

        std::fs::rename(&moved, &carrying).unwrap();
        crate::op_state::clear_owner(&carrying);
        take_within(path.clone(), "t", &op(2), &next, Duration::ZERO).unwrap();
        assert_eq!(Holder::parse(&path.read().unwrap()).unwrap().op, op(2));
    }

    #[test]
    fn a_claim_whose_owner_is_back_is_not_released_by_a_doctor_fix() {
        let tmp = crate::tempdir().unwrap();
        let owner = tmp.path().join("owner");
        std::fs::create_dir_all(owner.join("projects")).unwrap();
        record(&owner, &op(1));
        let path = claim_in(tmp.path());
        let _held = take_within(path.clone(), "t", &op(1), &owner, Duration::ZERO).unwrap();
        let stale_finding = UnreachableClaim {
            dir: path.dir().to_path_buf(),
            op: op(1),
            owner: owner.clone(),
        };
        assert!(!release_unreachable(&stale_finding, tmp.path()).unwrap());
        assert_eq!(Holder::parse(&path.read().unwrap()).unwrap().op, op(1));
    }

    #[test]
    fn a_claim_spelled_for_any_branch_name_is_a_short_file_name() {
        let long = "x".repeat(400);
        assert_eq!(digest(long.as_bytes()).len(), 32);
        assert_ne!(digest(b"p\nmain"), digest(b"p\nmaim"));
    }
}
