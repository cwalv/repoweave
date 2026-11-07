//! The url remote of a member: found before a verb fetches into the member's
//! clone, or the remote that is added first.

use crate::diagnosis::{Judged, Report, State};
use crate::registry::CloneUrl;
use crate::vcs::{AbsentRemote, RemoteHandle, RemoteNameTaken, UrlRemote, Vcs, VcsError};
use std::fmt;
use std::path::Path;

/// What a verb finds when it looks for a member's url remote.
pub(crate) enum Found {
    Present(RemoteHandle),
    Missing(AbsentRemote),
    Taken(Judged),
}

/// A member's url remote as a verb that fetches into the member's clone will
/// use it: the remote, and the one still to add before the fetch.
#[derive(Debug, Clone)]
pub(crate) struct Planned {
    pub(crate) remote: RemoteHandle,
    pub(crate) add: Option<AbsentRemote>,
}

impl Found {
    /// The plan this finding leaves, or the line judging the name taken.
    pub(crate) fn plan(self) -> Result<Planned, Judged> {
        match self {
            Found::Present(remote) => Ok(Planned { remote, add: None }),
            Found::Missing(absent) => Ok(Planned {
                remote: absent.remote().clone(),
                add: Some(absent),
            }),
            Found::Taken(judged) => Err(judged),
        }
    }
}

/// The remote of the clone at `dir` that has `url`, judged and not yet added.
pub(crate) fn find(
    vcs: &dyn Vcs,
    repo: impl fmt::Display,
    verb: &'static str,
    dir: &Path,
    url: &CloneUrl,
) -> Result<Found, VcsError> {
    Ok(match vcs.url_remote(dir, url)? {
        UrlRemote::Present(remote) => Found::Present(remote),
        UrlRemote::Absent(absent) => Found::Missing(absent),
        UrlRemote::NameTaken(taken) => Found::Taken(judge_taken(vcs, repo, verb, &taken)),
    })
}

/// `taken`, judged as a name held by another url.
pub(crate) fn judge_taken(
    vcs: &dyn Vcs,
    repo: impl fmt::Display,
    verb: &'static str,
    taken: &RemoteNameTaken,
) -> Judged {
    State::NameTaken
        .of(repo)
        .subject(format!("remote {}", taken.remote))
        .listing([format!(
            "{} holds {}, not {}",
            taken.remote, taken.holds, taken.wanted
        )])
        .by_verb(verb)
        .judge(vcs)
}

/// The remote of the clone at `dir` that has `url`, added when it is missing;
/// a name held by another url refuses.
pub(crate) fn ensure(
    vcs: &dyn Vcs,
    repo: impl fmt::Display,
    verb: &'static str,
    dir: &Path,
    url: &CloneUrl,
) -> anyhow::Result<RemoteHandle> {
    match find(vcs, repo, verb, dir, url)? {
        Found::Present(remote) => Ok(remote),
        Found::Missing(absent) => Ok(vcs.add_url_remote(dir, &absent)?),
        Found::Taken(judged) => {
            let mut report = Report::default();
            report.add(judged);
            Err(report
                .into_result()
                .expect_err("a report holding a line refuses"))
        }
    }
}

/// The remote of the clone at `dir` that has the member's `url`, when there is
/// one: what a verb that adds nothing finds, and the last step of the order a
/// branch's push target is found in.
pub(crate) fn present(
    vcs: &dyn Vcs,
    dir: &Path,
    url: &crate::manifest::RepoUrl,
) -> Result<Option<RemoteHandle>, VcsError> {
    let Ok(info) = crate::registry::resolve_to_clone_info(url) else {
        return Ok(None);
    };
    Ok(match vcs.url_remote(dir, &info.url)? {
        UrlRemote::Present(remote) => Some(remote),
        UrlRemote::Absent(_) | UrlRemote::NameTaken(_) => None,
    })
}
