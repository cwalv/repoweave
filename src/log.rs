//! `rwv log` — the VCS's log in every repo of the workspace, or the commits
//! beyond what a weave branch pins.

use std::path::Path;

use anyhow::Context;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::diagnosis::{Judged, JudgedOutput, Report, State};
use crate::manifest::{LockFile, Manifest, RepoKey};
use crate::parallel::OutputMode;
use crate::vcs::{
    project_vcs, vcs_for, CommitId, CommitSummary, RawRefName, RawRevisionId, ResolvedRevisionId,
    TrackingRef, Vcs, VcsError,
};
use crate::workspace::{project_dir, project_rel_path, WorkspaceContext};

pub const LOG_SCHEMA_URL: &str = crate::schema_url::schema_url!("log");

/// What `rwv log` reads: a range in the VCS's own syntax, or what HEAD adds
/// beyond a weave branch's pins.
pub enum LogQuery {
    Range(Option<String>),
    Against { weave_branch: String, diff: bool },
}

impl LogQuery {
    fn measure<'a>(&'a self, base: Base<'a>) -> Measure<'a> {
        match self {
            Self::Range(range) => Measure::Range(range.as_deref()),
            Self::Against { weave_branch, diff } => Measure::Beyond {
                branch: weave_branch,
                diff: *diff,
                base,
            },
        }
    }

    fn header(&self) -> String {
        match self {
            Self::Range(Some(range)) => format!("log {range}"),
            Self::Range(None) => "log".to_owned(),
            Self::Against { weave_branch, diff } => {
                let what = if *diff { "diff" } else { "log" };
                format!("{what} against {weave_branch}")
            }
        }
    }

    fn nothing(&self) -> String {
        match self {
            Self::Range(_) => "(no commits)".to_owned(),
            Self::Against { weave_branch, .. } => format!("(nothing beyond {weave_branch})"),
        }
    }
}

/// Top-level envelope for `rwv log --json`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct LogJsonOutput {
    #[serde(rename = "$schema")]
    pub schema_url: String,
    /// The range as spelled; absent for a bare log and with `--against`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range: Option<String>,
    /// The weave branch `--against` read the pins of.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub against: Option<String>,
    /// Whether each repo carries a diff in place of its commits (`--diff`).
    pub diff: bool,
    /// Every member of the workspace's project, then the project repo.
    pub repos: Vec<LogRepo>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct LogRepo {
    /// The repo's manifest-relative path, or `(project)` for the project repo.
    pub path: RepoKey,
    /// The checkout's HEAD, if readable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head: Option<CommitId>,
    /// With `--against`, the commit the weave branch pins for this repo: the
    /// branch's own tip for the project repo. Absent for a member the branch
    /// does not pin.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pin: Option<CommitId>,
    /// With a range, the commit each endpoint of the range names in this repo.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub endpoints: Vec<LogEndpoint>,
    /// The commits, newest first; empty with `--diff`.
    pub commits: Vec<LogCommit>,
    /// With `--diff`, the common ancestor of the pin and HEAD that the diff
    /// starts from.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff_base: Option<CommitId>,
    /// With `--diff`, the unified diff from `diff_base` to HEAD.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
    /// Why this repo could not be listed in full. The repo is still listed,
    /// so the gap is seen rather than silently omitted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// The state that made the verb exit non-zero in this repo.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub refused: Vec<JudgedOutput>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct LogEndpoint {
    /// The endpoint as spelled in the range.
    pub spelled: String,
    pub id: CommitId,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct LogCommit {
    pub sha: CommitId,
    pub short: String,
    pub subject: String,
}

impl From<CommitSummary> for LogCommit {
    fn from(commit: CommitSummary) -> Self {
        Self {
            short: commit.id.short().to_owned(),
            sha: commit.id,
            subject: commit.subject,
        }
    }
}

/// One repo's listing, and the state it was refused in when an endpoint
/// named nothing there.
struct Section {
    repo: LogRepo,
    refused: Option<Judged>,
}

impl Section {
    fn new(path: RepoKey) -> Self {
        Self {
            repo: LogRepo {
                path,
                head: None,
                pin: None,
                endpoints: Vec::new(),
                commits: Vec::new(),
                diff_base: None,
                diff: None,
                note: None,
                refused: Vec::new(),
            },
            refused: None,
        }
    }

    fn noting(mut self, note: String) -> Self {
        self.repo.note = Some(note);
        self
    }
}

/// What a repo's listing is measured against.
enum Measure<'a> {
    Range(Option<&'a str>),
    Beyond {
        branch: &'a str,
        diff: bool,
        base: Base<'a>,
    },
}

enum Base<'a> {
    Pin(&'a RawRevisionId),
    Tip(&'a ResolvedRevisionId),
    NotPinned,
}

/// A weave branch as `--against` reads it: its tip in the project repo and
/// the pins its `rwv.lock` holds there.
struct WeaveBranch {
    tip: ResolvedRevisionId,
    lock: Option<LockFile>,
}

pub fn run(ctx: &WorkspaceContext, query: &LogQuery, mode: OutputMode) -> anyhow::Result<()> {
    let project = ctx.require_active_project_on_disk()?;
    let workspace = ctx.active_path();
    let project_repo = project_dir(workspace, project);
    let manifest = Manifest::from_path(&project_repo.join(Manifest::FILE_NAME))?;
    let project_vcs = project_vcs();

    let branch = match query {
        LogQuery::Against { weave_branch, .. } => Some(
            read_weave_branch(project_vcs.as_ref(), &project_repo, weave_branch)?.ok_or_else(
                || {
                    State::NameNoSuchBranch
                        .of(project_rel_path(project))
                        .subject(weave_branch)
                        .judge(project_vcs.as_ref())
                        .refused()
                },
            )?,
        ),
        LogQuery::Range(_) => None,
    };

    let mut sections: Vec<Section> = Vec::new();
    for (path, entry) in manifest.iter_entries() {
        let pinned = branch
            .as_ref()
            .and_then(|b| b.lock.as_ref())
            .and_then(|lock| lock.get_entry(path));
        let base = match pinned {
            Some(pin) => Base::Pin(&pin.version),
            None => Base::NotPinned,
        };
        sections.push(read_repo(
            vcs_for(entry.vcs_type).as_ref(),
            RepoKey::Member(path.clone()),
            path.as_str(),
            &workspace.join(path.as_path()),
            query.measure(base),
        ));
    }
    let project_base = match &branch {
        Some(b) => Base::Tip(&b.tip),
        None => Base::NotPinned,
    };
    sections.push(read_repo(
        project_vcs.as_ref(),
        RepoKey::Project,
        &project_rel_path(project),
        &project_repo,
        query.measure(project_base),
    ));

    let mut report = Report::default();
    for section in &mut sections {
        if let Some(judged) = &section.refused {
            section.repo.refused.push(judged.output());
            report.add(judged.clone());
        }
    }

    if mode.is_json() {
        let (range, against, diff) = match query {
            LogQuery::Range(range) => (range.clone(), None, false),
            LogQuery::Against { weave_branch, diff } => (None, Some(weave_branch.clone()), *diff),
        };
        let envelope = LogJsonOutput {
            schema_url: LOG_SCHEMA_URL.to_owned(),
            range,
            against,
            diff,
            repos: sections.into_iter().map(|s| s.repo).collect(),
        };
        let out = serde_json::to_string_pretty(&envelope)
            .context("failed to serialize the log to JSON")?;
        outln!("{out}");
    } else {
        print_text(query, &sections);
    }
    report.into_result()
}

/// The weave branch `name` in the project repo at `project_repo`, or `None`
/// when `name` is no branch there or its tip carries no `rwv.toml`.
fn read_weave_branch(
    vcs: &dyn Vcs,
    project_repo: &Path,
    name: &str,
) -> anyhow::Result<Option<WeaveBranch>> {
    let Some(tip) = branch_tip(vcs, project_repo, name)? else {
        return Ok(None);
    };
    match vcs.read_file_at_revision(project_repo, &tip, Path::new(Manifest::FILE_NAME)) {
        Ok(_) => {}
        Err(VcsError::RevisionNotFound { .. }) => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    let lock = match vcs.read_file_at_revision(project_repo, &tip, Path::new(LockFile::FILE_NAME)) {
        Ok(text) => Some(
            LockFile::from_json_str(&text)
                .with_context(|| format!("failed to read the pins of `{name}`"))?,
        ),
        Err(VcsError::RevisionNotFound { .. }) => None,
        Err(e) => return Err(e.into()),
    };
    Ok(Some(WeaveBranch { tip, lock }))
}

/// The tip of the branch `name` in the project repo: the local branch, else the
/// one on its url remote as last fetched.
fn branch_tip(
    vcs: &dyn Vcs,
    repo: &Path,
    name: &str,
) -> anyhow::Result<Option<ResolvedRevisionId>> {
    let raw = RawRefName::new(name);
    if let Some(tip) = vcs.resolve_local_branch_tip(repo, &raw)? {
        return Ok(Some(tip));
    }
    let Ok(tracking) = TrackingRef::parse(raw) else {
        return Ok(None);
    };
    let Some(url_remote) = vcs.project_url_remote(repo)? else {
        return Ok(None);
    };
    match vcs.resolve_branch_on_remote(repo, &url_remote, &tracking.on_remote()) {
        Ok(tip) => Ok(Some(tip)),
        Err(VcsError::RevisionNotFound { .. }) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn read_repo(
    vcs: &dyn Vcs,
    key: RepoKey,
    judged_as: &str,
    repo: &Path,
    measure: Measure<'_>,
) -> Section {
    let mut section = Section::new(key);
    if !repo.exists() {
        return section.noting(format!(
            "checkout missing at {}; `rwv doctor` names the repair",
            repo.display()
        ));
    }
    let head = vcs.head_revision(repo);
    section.repo.head = head.as_ref().ok().map(|h| h.id().clone());
    match measure {
        Measure::Range(range) => match vcs.log(repo, range) {
            Ok(listing) => {
                section.repo.endpoints = listing
                    .endpoints
                    .into_iter()
                    .map(|e| LogEndpoint {
                        spelled: e.spelled,
                        id: e.id,
                    })
                    .collect();
                section.repo.commits = listing.commits.into_iter().map(Into::into).collect();
            }
            Err(VcsError::RevisionNotFound { rev, .. }) => {
                section.refused = Some(
                    State::NameNoSuchBranch
                        .of(judged_as)
                        .subject(rev)
                        .judge(vcs),
                );
            }
            Err(e) => section.repo.note = Some(format!("log failed: {e:#}")),
        },
        Measure::Beyond { branch, diff, base } => {
            let head = match head {
                Ok(head) => head,
                Err(e) => return section.noting(format!("HEAD unreadable: {e:#}")),
            };
            let base = match base {
                Base::NotPinned => return section.noting(format!("not in {branch}")),
                Base::Tip(tip) => tip.clone(),
                Base::Pin(pin) => match vcs.resolve_revision(repo, pin.as_str()) {
                    Ok(pin) => pin,
                    Err(e) => {
                        return section.noting(format!(
                            "{branch} pins {pin}, which this checkout does not resolve: {e:#}"
                        ))
                    }
                },
            };
            section.repo.pin = Some(base.id().clone());
            if diff {
                match vcs.diff_beyond(repo, &base) {
                    Ok(beyond) => {
                        section.repo.diff_base = beyond.base;
                        section.repo.diff = Some(beyond.text);
                    }
                    Err(e) => section.repo.note = Some(format!("diff failed: {e:#}")),
                }
            } else {
                match vcs.commits_beyond(repo, &head, &base) {
                    Ok(commits) => {
                        section.repo.commits = commits.into_iter().map(Into::into).collect();
                    }
                    Err(e) => section.repo.note = Some(format!("log failed: {e:#}")),
                }
            }
        }
    }
    section
}

fn print_text(query: &LogQuery, sections: &[Section]) {
    outln!("{}", query.header());
    for section in sections {
        let repo = &section.repo;
        outln!();
        outln!("=== {} ===", repo.path);
        if let Some(note) = &repo.note {
            outln!("  note: {note}");
        }
        if let Some(judged) = &section.refused {
            outln!("  {}", judged.state_line());
        } else if let Some(diff) = &repo.diff {
            if diff.is_empty() {
                outln!("  {}", query.nothing());
            } else {
                if let Some(base) = &repo.diff_base {
                    outln!("  (diff range: {}..HEAD)", base.short());
                }
                outln!("{diff}");
            }
        } else if repo.commits.is_empty() {
            if repo.note.is_none() {
                outln!("  {}", query.nothing());
            }
        } else {
            for commit in &repo.commits {
                outln!("  {} {}", commit.short, commit.subject);
            }
        }
    }
}
