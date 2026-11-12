//! What the weave's live weave branches declare: for each member, the
//! branches some live weave branch of any project names as its declared
//! branch. Protection, the Names rule and alignment all read this one scan.

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use crate::branch_ref::BranchRef;
use crate::manifest::{Manifest, ProjectName, RepoPath};
use crate::vcs::{project_vcs, VcsError};

/// A live weave branch that declares a member branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declarer {
    pub project: ProjectName,
    pub branch: BranchRef,
}

impl fmt::Display for Declarer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} (projects/{})", self.branch, self.project)
    }
}

/// The `history` one project's live weave branch records for a member: the
/// member's own, else the top-level one.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct RecordedHistory {
    pub project: ProjectName,
    pub branch: String,
    pub history: crate::manifest::History,
}

/// Every declaration, and every recorded `history`, of every live weave
/// branch, local or remote-tracking, recorded or not, in every project of
/// the weave.
#[derive(Debug, Clone, Default)]
pub struct LiveDeclarations {
    declared: BTreeMap<String, BTreeMap<String, Vec<Declarer>>>,
    histories: BTreeMap<RepoPath, Vec<RecordedHistory>>,
}

impl LiveDeclarations {
    /// Read every project of the weave at `primary`. A tip with no
    /// `rwv.toml`, or a pre-schema one, declares nothing; a tip or a file
    /// that cannot be read is an error, never "declares nothing".
    pub fn scan(primary: &Path) -> anyhow::Result<Self> {
        let mut live = Self::default();
        let vcs = project_vcs();
        for project in crate::workspace::discover_projects(primary) {
            let dir = crate::workspace::project_dir(primary, &project);
            if !vcs.is_repo(&dir) {
                continue;
            }
            for tip in vcs.branch_tips(&dir)? {
                let text =
                    match vcs.read_file_at_revision(&dir, &tip.tip, Path::new(Manifest::FILE_NAME))
                    {
                        Ok(text) => text,
                        Err(VcsError::RevisionNotFound { .. }) => continue,
                        Err(e) => return Err(e.into()),
                    };
                let Ok(manifest) = Manifest::from_toml_str(&text) else {
                    continue;
                };
                for (path, entry) in manifest.iter_entries() {
                    let declarers = live
                        .declared
                        .entry(path.to_string())
                        .or_default()
                        .entry(entry.branch.local_counterpart().as_str().to_owned())
                        .or_default();
                    let declarer = Declarer {
                        project: project.clone(),
                        branch: tip.branch.clone(),
                    };
                    if !declarers.contains(&declarer) {
                        declarers.push(declarer);
                    }
                    if let Some(history) = manifest.history_for(path) {
                        live.histories
                            .entry(path.clone())
                            .or_default()
                            .push(RecordedHistory {
                                project: project.clone(),
                                branch: tip.branch.to_string(),
                                history,
                            });
                    }
                }
            }
        }
        Ok(live)
    }

    /// The live weave branches that declare `branch` for `member`.
    pub fn declarers(&self, member: &RepoPath, branch: &str) -> &[Declarer] {
        self.declared
            .get(member.as_str())
            .and_then(|by_branch| by_branch.get(branch))
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    pub fn declares(&self, member: &RepoPath, branch: &str) -> bool {
        !self.declarers(member, branch).is_empty()
    }

    /// Each member's `history` as every live weave branch listing it records
    /// one.
    pub fn histories(&self) -> &BTreeMap<RepoPath, Vec<RecordedHistory>> {
        &self.histories
    }
}
