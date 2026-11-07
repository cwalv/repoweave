//! The claim an rwv holds on an object store while it fetches into it.
//!
//! Two fetches into one store race on the refs they update, and the loser
//! dies on a ref lock. Every pruning fetch therefore runs inside
//! [`FetchClaim::hold`], and [`crate::vcs::Vcs::fetch_pruning`] takes the
//! claim as a witness, so a fetch with none is not expressible.

use crate::durable_file::ClaimFile;
use crate::refusal::RefusalKind;
use crate::state_file::StateFile;
use crate::vcs::Vcs;
use anyhow::Context;
use std::path::{Path, PathBuf};

pub(crate) const FETCH_CLAIM_FILE: &str = ".rwv-fetch.lock";

/// A held claim on the object store a repo draws from, released when
/// [`FetchClaim::hold`] returns.
#[derive(Debug)]
pub struct FetchClaim {
    store: PathBuf,
    file: ClaimFile,
}

impl FetchClaim {
    /// Run `fetch` holding the claim on the store `repo` draws from, waiting
    /// the shared claim budget (`durable_file::CLAIM_WAIT`) for a peer
    /// holding it, then refusing with the path to remove.
    ///
    /// The claim is released before this returns, and a release that fails is
    /// an error naming the file left behind, whatever `fetch` returned.
    pub fn hold<T>(
        vcs: &dyn Vcs,
        repo: &Path,
        fetch: impl FnOnce(&Self) -> anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        let claim = Self::acquire(vcs, repo)?;
        crate::test_pause::point("fetch-under-claim");
        let outcome = fetch(&claim);
        let released = claim.file.release().with_context(|| {
            format!(
                "failed to release the fetch claim on the object store of {repo}. \
                 Every later fetch into it refuses until {path} is deleted",
                repo = repo.display(),
                path = StateFile::FetchClaim.in_dir(&claim.store),
            )
        });
        crate::durable_file::after_release(outcome, released)
    }

    fn acquire(vcs: &dyn Vcs, repo: &Path) -> anyhow::Result<Self> {
        let store = vcs
            .store_of(repo)
            .with_context(|| format!("{} has no object store to fetch into", repo.display()))?;
        let path = StateFile::FetchClaim.in_dir(&store);
        match path.claim() {
            Ok(file) => Ok(Self { store, file }),
            Err(crate::durable_file::ClaimError::Held(held_by)) => {
                crate::refuse!(
                    RefusalKind::StateClaimHeld,
                    "another rwv is still fetching into the object store of {repo} \
                     ({held_by}). If no rwv is running, delete {claim} and rerun.",
                    repo = repo.display(),
                    claim = path,
                );
            }
            Err(crate::durable_file::ClaimError::Io(e)) => Err(anyhow::Error::new(e).context(
                format!("failed to claim the object store of {}", repo.display()),
            )),
        }
    }
}
