//! The Names rule: a member branch serves one feature, or a declared branch,
//! never both. Every verb that writes, opens, aligns or publishes a feature
//! judges it here.

use std::path::Path;

use crate::alignment::Accounting;
use crate::diagnosis::{Judged, Report, State};
use crate::manifest::{Manifest, ProjectName, RepoPath, Role};
use crate::records::{BranchName, Lookup, Record, Store};
use crate::vcs::{project_vcs, vcs_for, ResolvedRevisionId, VcsError};

/// What the Names rule reads across the weave.
pub(crate) struct NameScan<'a> {
    pub(crate) primary: &'a Path,
    pub(crate) project: &'a ProjectName,
    pub(crate) accounting: Accounting,
}

impl<'a> NameScan<'a> {
    pub(crate) fn of(primary: &'a Path, project: &'a ProjectName) -> anyhow::Result<Self> {
        Ok(Self {
            primary,
            project,
            accounting: Accounting::scan(primary)?,
        })
    }
}

/// The projects other than `project` whose feature `name` lists `member` in
/// the `rwv.toml` at its tip.
pub(crate) fn other_projects_with_feature(
    primary: &Path,
    project: &ProjectName,
    name: &BranchName,
    member: &RepoPath,
) -> anyhow::Result<Vec<String>> {
    let vcs = project_vcs();
    let mut found = Vec::new();
    for other in crate::workspace::discover_projects(primary) {
        if &other == project {
            continue;
        }
        let dir = crate::workspace::project_dir(primary, &other);
        if !vcs.is_repo(&dir) {
            continue;
        }
        let view = Store::open(vcs.as_ref(), &dir)?.view()?;
        if !matches!(view.lookup(name), Lookup::Value(Record::Feature { .. })) {
            continue;
        }
        let branches = vcs.named_branches(&dir)?;
        let Some(branch) = branches.iter().find(|b| b.name == name.as_str()) else {
            continue;
        };
        let tip = ResolvedRevisionId::from(branch.tip.clone());
        let text = match vcs.read_file_at_revision(&dir, &tip, Path::new(Manifest::FILE_NAME)) {
            Ok(text) => text,
            Err(VcsError::RevisionNotFound { .. }) => continue,
            Err(e) => return Err(e.into()),
        };
        let lists = Manifest::from_toml_str(&text)
            .is_ok_and(|manifest| manifest.get_entry(member).is_some());
        if lists {
            found.push(crate::workspace::project_rel_path(&other));
        }
    }
    Ok(found)
}

/// The Names rule for a feature `name` whose `rwv.toml` is `manifest`, with
/// its members checked out under `root`: each `owned` or `fork` member whose
/// branch `name` a live weave branch declares (`name` declared), or another
/// project's feature `name` uses for a member both list (`name` feature).
pub(crate) fn judge_feature_names(
    root: &Path,
    scan: &NameScan<'_>,
    name: &BranchName,
    manifest: &Manifest,
    verb: Option<&'static str>,
) -> anyhow::Result<Vec<Judged>> {
    let mut judged = Vec::new();
    for (path, entry) in manifest.iter_entries() {
        if !matches!(entry.role, Role::Owned | Role::Fork) {
            continue;
        }
        let dir = root.join(path.as_path());
        let vcs = vcs_for(entry.vcs_type);
        let judging = |state: State| {
            let judging = state
                .of(path)
                .member()
                .bind("<m>", path)
                .bind("<W>", name)
                .checkout(&dir)
                .subject(name);
            match verb {
                Some(verb) => judging.by_verb(verb),
                None => judging,
            }
        };
        let declarers = scan.accounting.declarers(path, name.as_str());
        if !declarers.is_empty() {
            judged.push(
                judging(State::NameDeclared)
                    .listing(declarers)
                    .judge(vcs.as_ref()),
            );
        }
        let elsewhere = other_projects_with_feature(scan.primary, scan.project, name, path)?;
        if !elsewhere.is_empty() {
            judged.push(
                judging(State::NameFeature)
                    .listing(elsewhere)
                    .judge(vcs.as_ref()),
            );
        }
    }
    Ok(judged)
}

/// Refuse a feature `name` the Names rule refuses: the gate of the verbs that
/// align, land or publish a branch whose record is `record`.
pub(crate) fn require_feature_names(
    root: &Path,
    scan: &NameScan<'_>,
    name: &BranchName,
    record: &Record,
    manifest: &Manifest,
    verb: Option<&'static str>,
) -> anyhow::Result<()> {
    if !matches!(record, Record::Feature { .. }) {
        return Ok(());
    }
    let mut report = Report::default();
    for judged in judge_feature_names(root, scan, name, manifest, verb)? {
        report.add(judged);
    }
    report.into_result()
}
