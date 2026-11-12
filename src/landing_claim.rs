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
    Dead,
    /// The file names no op this rwv reads.
    Unread(String),
}

fn standing(seen: &[u8]) -> Standing {
    match Holder::parse(seen) {
        None => Standing::Unread(String::from_utf8_lossy(seen).trim().to_owned()),
        Some(holder) => match crate::op_state::records_op(holder.owner.as_path(), &holder.op) {
            Some(false) => Standing::Dead,
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
                return Ok(LandingClaim {
                    path,
                    op: op.clone(),
                })
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
        let Ok(seen) = path.read() else {
            continue;
        };
        let holder = match standing(&seen) {
            Standing::Live(holder) if holder.op == *op => {
                return Ok(LandingClaim {
                    path,
                    op: op.clone(),
                })
            }
            Standing::Dead => {
                path.remove_if_holds(&seen, &format!("taken.{op}"))
                    .with_context(|| format!("failed to take over the dead claim {path}"))?;
                continue;
            }
            Standing::Live(holder) => Ok(holder),
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
                    "another landing into '{target}' holds it: op {op}, recorded in {owner}. If \
                     it is running, rerun once it ends. If it stopped, either of these, run in \
                     {owner}, ends it, and then this landing runs:\n  rwv land --continue\n  \
                     rwv abort",
                    op = holder.op,
                    owner = holder.owner.as_path().display(),
                ),
                Err(text) => crate::refuse!(
                    RefusalKind::StateClaimHeld,
                    "another landing into '{target}' holds it, and {path} names it in a form \
                     this rwv does not read ({text}): an rwv of another release took it. End \
                     that landing with the release that started it, with `rwv land \
                     --continue` or `rwv abort`, then rerun."
                ),
            }
        }
        crate::test_pause::point("landing-claim-held");
        std::thread::sleep(crate::durable_file::CLAIM_POLL);
    }
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
    path.remove_if_holds(&seen, &format!("released.{op}"))
        .with_context(|| {
            format!(
                "failed to release the landing claim {path}; the next landing into its target \
                 takes it over once op {op} is no longer recorded"
            )
        })?;
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
        };
        let held = take_within(path.clone(), "t", &op(2), &owner, Duration::ZERO).unwrap();
        stale.release().unwrap();
        assert_eq!(Holder::parse(&path.read().unwrap()).unwrap().op, op(2));
        held.release().unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn a_claim_spelled_for_any_branch_name_is_a_short_file_name() {
        let long = "x".repeat(400);
        assert_eq!(digest(long.as_bytes()).len(), 32);
        assert_ne!(digest(b"p\nmain"), digest(b"p\nmaim"));
    }
}
