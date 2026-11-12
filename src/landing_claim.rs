//! The claim a landing holds on its target, `T`, from reading `T`'s tips
//! until `T`'s workspace has taken the last fast-forward.
//!
//! Landings into one target run one at a time: a second one waits on the
//! claim, as a fetch waits on [`crate::fetch_claim::FetchClaim`], and then
//! reads the tips the first one left. The compare-and-swap of each move stays
//! the guard against everything that takes no claim.

use crate::durable_file::CreateNewError;
use crate::manifest::ProjectName;
use crate::op_state::OpId;
use crate::refusal::RefusalKind;
use crate::state_file::{StateFile, StatePath};
use crate::vcs::Vcs;
use anyhow::Context;
use std::path::Path;

pub(crate) const LANDING_CLAIM_FILE: &str = ".rwv-landing.lock";
const CLAIMS_DIR: &str = "rwv-landings";

/// A held claim on one target branch of one project, released on
/// [`LandingClaim::release`] or when dropped.
#[derive(Debug)]
pub struct LandingClaim {
    path: StatePath,
    released: bool,
}

/// Where the claim on `target` of `project` lives: in the object store of
/// the project repo at `project_dir`, one directory per target, named so any
/// branch name spells a file name on every platform.
fn claim_path(
    vcs: &dyn Vcs,
    project_dir: &Path,
    project: &ProjectName,
    target: &str,
) -> anyhow::Result<StatePath> {
    let store = vcs.store_of(project_dir).with_context(|| {
        format!(
            "{} has no object store to hold a landing claim in",
            project_dir.display()
        )
    })?;
    let spelled: String = format!("{}\n{target}", project.as_str())
        .bytes()
        .map(|b| format!("{b:02x}"))
        .collect();
    Ok(StateFile::LandingClaim.in_dir(&store.join(CLAIMS_DIR).join(spelled)))
}

fn holder(op: &OpId) -> String {
    format!("op {op}\n")
}

impl LandingClaim {
    /// Claim `target` for the landing `op`, waiting the shared claim budget
    /// (`durable_file::CLAIM_WAIT`) for another landing holding it, then
    /// refusing with the holder and the file to remove. A claim `op` itself
    /// left, stopped or killed before it released it, is taken over.
    pub fn take(
        vcs: &dyn Vcs,
        project_dir: &Path,
        project: &ProjectName,
        target: &str,
        op: &OpId,
    ) -> anyhow::Result<Self> {
        let path = claim_path(vcs, project_dir, project, target)?;
        let mine = holder(op);
        let deadline = std::time::Instant::now() + crate::durable_file::CLAIM_WAIT;
        loop {
            match path.create(mine.as_bytes()) {
                Ok(()) => {
                    return Ok(Self {
                        path,
                        released: false,
                    })
                }
                Err(CreateNewError::AlreadyExists) => {
                    let held_by = path.read_to_string().unwrap_or_default();
                    if held_by == mine {
                        return Ok(Self {
                            path,
                            released: false,
                        });
                    }
                    if std::time::Instant::now() >= deadline {
                        crate::refuse!(
                            RefusalKind::StateClaimHeld,
                            "another landing into '{target}' is still running ({}). If no rwv \
                             is landing into '{target}', delete {path} and rerun.",
                            held_by.trim()
                        );
                    }
                    crate::test_pause::point("landing-claim-held");
                    std::thread::sleep(crate::durable_file::CLAIM_POLL);
                }
                Err(CreateNewError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
                    std::fs::create_dir_all(path.dir())
                        .with_context(|| format!("failed to make {}", path.dir().display()))?;
                }
                Err(CreateNewError::Io(e)) => {
                    return Err(anyhow::Error::new(e)
                        .context(format!("failed to claim '{target}' at {path}")))
                }
            }
        }
    }

    /// Remove the claim.
    pub fn release(mut self) -> anyhow::Result<()> {
        self.released = true;
        remove(&self.path)
    }

    /// Remove the claim on `target` that `op` holds, if any: what `rwv
    /// abort` does for a landing that stopped or was killed holding it.
    pub fn release_held_by(
        vcs: &dyn Vcs,
        project_dir: &Path,
        project: &ProjectName,
        target: &str,
        op: &OpId,
    ) -> anyhow::Result<()> {
        let path = claim_path(vcs, project_dir, project, target)?;
        match path.read_to_string() {
            Ok(held_by) if held_by == holder(op) => remove(&path),
            _ => Ok(()),
        }
    }
}

fn remove(path: &StatePath) -> anyhow::Result<()> {
    match path.remove() {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(anyhow::Error::new(e).context(format!(
                "failed to release the landing claim {path}; every later landing into its \
                 target refuses until it is deleted"
            )))
        }
    }
    let _ = std::fs::remove_dir(path.dir());
    Ok(())
}

impl Drop for LandingClaim {
    fn drop(&mut self) {
        if !self.released {
            let _ = remove(&self.path);
        }
    }
}
