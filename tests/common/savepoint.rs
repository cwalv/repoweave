//! Savepoints filed as rwv files them, for a test that synthesizes an op on
//! disk or reads what one left.
//!
//! A checkout's savepoint names the branch the checkout was on, so the ref
//! sits at `refs/rwv/pre-op/<key>/checkout/<branch>`, or
//! `refs/rwv/pre-op/<key>/detached`; a branch an op moves as a ref has one at
//! `refs/rwv/pre-op/<key>/ref/<branch>`. `<key>` is an op id, or an op id
//! with `-target` for the workspace a landing advances.

use std::path::Path;

use super::git_in;

/// The namespace every savepoint is filed below.
pub const NAMESPACE: &str = "refs/rwv/pre-op";

/// The ref a savepoint of the checkout `repo` under `key` is filed at,
/// for the branch the checkout is on now.
pub fn checkout_ref(repo: &Path, key: &str) -> String {
    let on = git_in(repo, &["branch", "--show-current"]);
    if on.is_empty() {
        format!("{NAMESPACE}/{key}/detached")
    } else {
        format!("{NAMESPACE}/{key}/checkout/{on}")
    }
}

/// Where a checkout savepoint says the checkout was when it was taken.
pub enum SavedOn<'a> {
    Branch(&'a str),
    Detached,
}

/// File a savepoint of the checkout `repo` under `key` at `sha`, on the
/// branch the checkout is on now.
pub fn plant_at(repo: &Path, key: &str, sha: &str) {
    git_in(repo, &["update-ref", &checkout_ref(repo, key), sha]);
}

/// File a savepoint of the checkout `repo` under `key` at `sha`, saying the
/// checkout was `on`, wherever it is now.
pub fn plant_on(repo: &Path, key: &str, on: SavedOn<'_>, sha: &str) {
    let name = match on {
        SavedOn::Branch(branch) => format!("{NAMESPACE}/{key}/checkout/{branch}"),
        SavedOn::Detached => format!("{NAMESPACE}/{key}/detached"),
    };
    git_in(repo, &["update-ref", &name, sha]);
}

/// File a savepoint of the checkout `repo` under `key` at its `HEAD`.
pub fn plant(repo: &Path, key: &str) {
    let head = git_in(repo, &["rev-parse", "HEAD"]);
    plant_at(repo, key, &head);
}

/// Every savepoint ref filed under `key` in `repo`'s store, with its tip.
pub fn under(repo: &Path, key: &str) -> Vec<(String, String)> {
    git_in(
        repo,
        &[
            "for-each-ref",
            "--format=%(refname) %(objectname)",
            &format!("{NAMESPACE}/{key}/"),
        ],
    )
    .lines()
    .filter_map(|line| line.split_once(' '))
    .map(|(name, tip)| (name.to_owned(), tip.to_owned()))
    .collect()
}

/// The tip the checkout savepoint under `key` holds, if one is filed.
pub fn checkout_tip(repo: &Path, key: &str) -> Option<String> {
    under(repo, key)
        .into_iter()
        .find(|(name, _)| {
            let below = &name[NAMESPACE.len() + key.len() + 2..];
            below == "detached" || below.starts_with("checkout/")
        })
        .map(|(_, tip)| tip)
}
