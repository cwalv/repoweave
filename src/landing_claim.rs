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
//! released. A claim whose workspace records no such op is dead, on any host
//! that can read the path and with no process id consulted, and the next
//! landing takes it over; a claim whose op is recorded is never taken from it.

use crate::durable_file::CreateNewError;
use crate::manifest::ProjectName;
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
    Dead(Holder),
    /// The file names no op this rwv reads.
    Unread(String),
}

fn standing(seen: &[u8]) -> Standing {
    match Holder::parse(seen) {
        None => Standing::Unread(String::from_utf8_lossy(seen).trim().to_owned()),
        Some(holder) => match crate::op_state::records_op(holder.owner.as_path(), &holder.op) {
            Some(false) => Standing::Dead(holder),
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
        let dir = claims_dir(vcs, project_dir)?;
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => {
                return Err(anyhow::Error::new(e).context(format!(
                    "failed to read the landing claims in {}",
                    dir.display()
                )))
            }
        };
        for entry in entries {
            let entry = entry.with_context(|| format!("failed to read {}", dir.display()))?;
            release_takeovers_held_by(&entry.path(), op)?;
            release_if_held(&StateFile::LandingClaim.in_dir(&entry.path()), op)?;
        }
        Ok(())
    }
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
            Err(CreateNewError::AlreadyExists) => {}
        }
        let seen = match path.read() {
            Ok(seen) => seen,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => format!("{e:#}").into_bytes(),
        };
        let blocking = match standing(&seen) {
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
        let holder = match blocking {
            Standing::Live(holder) | Standing::Dead(holder) => Ok(holder),
            Standing::Unread(text) => Err(text),
        };
        let progress = match &holder {
            Ok(holder) => crate::op_state::OwnerRecord::path_in(holder.owner.as_path())
                .read()
                .ok(),
            Err(_) => None,
        };
        let now = (seen, progress);
        if last_seen.as_ref() != Some(&now) {
            last_seen = Some(now);
            deadline = Instant::now() + wait;
        }
        if Instant::now() >= deadline {
            match holder {
                Ok(holder) => crate::refuse!(
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
                Err(text) => crate::refuse!(
                    RefusalKind::StateClaimHeld,
                    "another landing into '{target}' holds it, and {path} names it in a form \
                     this rwv does not read ({text}): an rwv of another release took it. End \
                     that landing with the release that started it, resuming or aborting it, \
                     then rerun."
                ),
            }
        }
        crate::test_pause::point("landing-claim-held");
        std::thread::sleep(crate::durable_file::CLAIM_POLL);
    }
}

/// Remove `path`, which read `seen` when it was judged to name the dead op
/// `dead`, under the guard `<path>.takeover.<dead>` that `mine` creates
/// exclusively: only the guard's holder removes `path`, and only while it
/// still reads `seen`. While it reads `seen` nothing can replace it (no create
/// succeeds on an occupied path, and its dead op releases nothing), so the
/// re-read and the removal under the guard are one step to every other
/// remover. A guard whose holder is dead is itself removed this way first.
/// Returns what blocks the removal: a guard another standing op holds, or
/// one this rwv does not read.
fn remove_dead(
    path: Removable<'_>,
    seen: &[u8],
    dead: &OpId,
    mine: &Holder,
) -> anyhow::Result<Option<Standing>> {
    let guard = path.guard(dead);
    loop {
        match crate::durable_file::create_new(&guard, &mine.bytes()?) {
            Ok(()) => break,
            Err(CreateNewError::AlreadyExists) => {}
            Err(CreateNewError::Io(e)) => return Err(e.into()),
        }
        let held = match std::fs::read(&guard) {
            Ok(held) => held,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Ok(Some(Standing::Unread(format!("{e:#}")))),
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
        std::fs::create_dir_all(&owner).unwrap();
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

    #[test]
    fn a_claim_whose_op_is_no_longer_recorded_is_taken_over() {
        let tmp = crate::tempdir().unwrap();
        let gone = tmp.path().join("gone");
        std::fs::create_dir_all(&gone).unwrap();
        let path = claim_in(tmp.path());
        let _orphan = take_within(path.clone(), "t", &op(1), &gone, Duration::ZERO).unwrap();
        take_within(path.clone(), "t", &op(2), tmp.path(), Duration::ZERO).unwrap();
        assert_eq!(Holder::parse(&path.read().unwrap()).unwrap().op, op(2));

        let moved_on = tmp.path().join("moved-on");
        std::fs::create_dir_all(&moved_on).unwrap();
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
        std::fs::create_dir_all(&owner).unwrap();
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
        std::fs::create_dir_all(&gone).unwrap();
        let path = claim_in(tmp);
        let _dead = take_within(path.clone(), "t", &op(1), &gone, Duration::ZERO).unwrap();
        (path.clone(), path.read().unwrap(), holder(op(1), &gone))
    }

    #[test]
    fn a_taker_that_judged_a_claim_dead_never_removes_the_live_one_since_taken() {
        let tmp = crate::tempdir().unwrap();
        let (path, seen, dead) = dead_claim(tmp.path());
        let live = tmp.path().join("live");
        std::fs::create_dir_all(&live).unwrap();
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
        std::fs::create_dir_all(&late).unwrap();
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
        std::fs::create_dir_all(&first).unwrap();
        record(&first, &op(2));
        let lock = path
            .dir()
            .join(format!("{LANDING_CLAIM_FILE}.takeover.{}", dead.op));
        crate::durable_file::create_new(&lock, &holder(op(2), &first).bytes().unwrap()).unwrap();
        let second = tmp.path().join("second");
        std::fs::create_dir_all(&second).unwrap();
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
        std::fs::create_dir_all(&died).unwrap();
        let lock = path
            .dir()
            .join(format!("{LANDING_CLAIM_FILE}.takeover.{}", dead.op));
        crate::durable_file::create_new(&lock, &holder(op(2), &died).bytes().unwrap()).unwrap();
        let next = tmp.path().join("next");
        std::fs::create_dir_all(&next).unwrap();
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
        std::fs::create_dir_all(&died).unwrap();
        let guard = Removable::Claim(&path).guard(&op(1));
        crate::durable_file::create_new(&guard, &holder(op(2), &died).bytes().unwrap()).unwrap();
        let nested = Removable::Guard(&guard).guard(&op(4));
        crate::durable_file::create_new(&nested, &holder(op(4), &died).bytes().unwrap()).unwrap();
        let next = tmp.path().join("next");
        std::fs::create_dir_all(&next).unwrap();
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
        std::fs::create_dir_all(&stopped).unwrap();
        record(&stopped, &op(2));
        let guard = Removable::Claim(&path).guard(&dead.op);
        crate::durable_file::create_new(&guard, &holder(op(2), &stopped).bytes().unwrap()).unwrap();
        let next = tmp.path().join("next");
        std::fs::create_dir_all(&next).unwrap();
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
    fn a_claim_spelled_for_any_branch_name_is_a_short_file_name() {
        let long = "x".repeat(400);
        assert_eq!(digest(long.as_bytes()).len(), 32);
        assert_ne!(digest(b"p\nmain"), digest(b"p\nmaim"));
    }
}
