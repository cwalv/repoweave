//! The claim an rwv holds on an object store while it fetches into it.
//!
//! Two fetches into one store race on the refs they update, and the loser
//! dies on a ref lock. Every pruning fetch therefore runs inside
//! [`FetchClaim::hold`], and [`crate::vcs::Vcs::fetch_pruning`] takes the
//! claim as a witness, so a fetch with none is not expressible.

use crate::durable_file::ClaimFile;
use crate::refusal::RefusalKind;
use crate::state_file::{StateFile, StatePath};
use crate::vcs::{PruningFetch, RemoteHandle, Vcs};
use anyhow::Context;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub(crate) const FETCH_CLAIM_FILE: &str = ".rwv-fetch.lock";
pub(crate) const FETCH_RECORD_FILE: &str = ".rwv-fetched";

/// When rwv's pruning fetch of each remote into one store last completed, in
/// seconds since the Unix epoch, keyed by the remote as its backend renders
/// it.
type FetchRecord = BTreeMap<String, u64>;

fn read_record(path: &StatePath) -> anyhow::Result<Option<Vec<u8>>> {
    match path.read() {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(anyhow::Error::new(e).context(format!("failed to read {path}"))),
    }
}

/// How long ago rwv's last pruning fetch of `remote` into the store `repo`
/// draws from completed, or `None` where none is on record.
pub(crate) fn last_fetch_age(
    vcs: &dyn Vcs,
    repo: &Path,
    remote: &RemoteHandle,
) -> anyhow::Result<Option<Duration>> {
    let Some(store) = vcs.store_of(repo) else {
        return Ok(None);
    };
    let path = StateFile::FetchRecord.in_dir(&store);
    let Some(bytes) = read_record(&path)? else {
        return Ok(None);
    };
    let record: FetchRecord =
        serde_json::from_slice(&bytes).with_context(|| format!("failed to parse {path}"))?;
    Ok(record.get(&remote.to_string()).map(|&at| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH + Duration::from_secs(at))
            .unwrap_or_default()
    }))
}

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

    /// The pruning fetch of `only`'s remotes into `repo` (every remote, when
    /// `only` is `None`), recording when each one it completed did.
    ///
    /// A record that does not parse is replaced: it holds nothing a verb
    /// needs.
    pub(crate) fn fetch_pruning(
        &self,
        vcs: &dyn Vcs,
        repo: &Path,
        only: Option<&[RemoteHandle]>,
    ) -> anyhow::Result<PruningFetch> {
        let fetched = vcs.fetch_pruning(self, repo, only)?;
        if !fetched.fetched.is_empty() {
            let path = StateFile::FetchRecord.in_dir(&self.store);
            let mut record: FetchRecord = read_record(&path)?
                .and_then(|bytes| serde_json::from_slice(&bytes).ok())
                .unwrap_or_default();
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            for remote in &fetched.fetched {
                record.insert(remote.to_string(), now);
            }
            path.publish(&serde_json::to_vec_pretty(&record)?)?;
        }
        Ok(fetched)
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
            Err(crate::durable_file::ClaimError::NoExclusivePublish(e)) => Err(e),
            Err(crate::durable_file::ClaimError::Io(e)) => Err(anyhow::Error::new(e).context(
                format!("failed to claim the object store of {}", repo.display()),
            )),
        }
    }
}
