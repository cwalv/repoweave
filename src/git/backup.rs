//! Backups, as git keeps them: one ref per backup under [`RwvRef::Backup`],
//! named for the branch it backs up and an id.
//!
//! The id is the abbreviated id of the backed-up tip, behind the mark when
//! there is one, so backing up the same tip twice names one ref. A tip whose
//! abbreviation another commit already holds takes a longer one.

use super::{git_command, Argv, GitVcs, RwvRef, Scratch, LOCAL_BRANCH_NAMESPACE};
use crate::vcs::{
    Backup, BackupMark, BackupName, BackupSource, BranchTip, CommitId, CommitSummary, HeldAlone,
    HeldKind, ListedBackup, RemoteTracking, ResolvedRevisionId, VcsError,
};
use std::collections::HashSet;
use std::path::Path;

const REMOTE_TRACKING_ROOT: &str = "refs/remotes/";
const ID_WIDTH: usize = 12;
const DETACHED_HEAD: &str = "HEAD";
const REBASED_PREFIX: &str = "rebased-";
const SUPERSEDED_PREFIX: &str = "superseded-";
const ABORTED_SLOT: &str = "aborted";
const ABORTED_WORKING_COPY_SLOT: &str = "aborted-working-copy";
const EDITS_MESSAGE: &str = "rwv backup of uncommitted edits";
/// The second parent of an edits backup whose index held content the
/// working tree no longer does: the index as it stood.
const STAGED_MESSAGE: &str = "rwv backup of staged changes";

fn source_component(source: &BackupSource) -> &str {
    match source {
        BackupSource::Branch(name) => name.as_str(),
        BackupSource::DetachedHead => DETACHED_HEAD,
    }
}

fn id_text(mark: Option<BackupMark>, hex: &str) -> String {
    match mark {
        None => hex.to_owned(),
        Some(BackupMark::Rebased) => format!("{REBASED_PREFIX}{hex}"),
        Some(BackupMark::Superseded) => format!("{SUPERSEDED_PREFIX}{hex}"),
        Some(BackupMark::Aborted) => ABORTED_SLOT.to_owned(),
        Some(BackupMark::AbortedWorkingCopy) => ABORTED_WORKING_COPY_SLOT.to_owned(),
    }
}

fn slot(mark: Option<BackupMark>) -> bool {
    matches!(
        mark,
        Some(BackupMark::Aborted | BackupMark::AbortedWorkingCopy)
    )
}

fn mark_of(id: &str) -> Option<BackupMark> {
    if id == ABORTED_SLOT {
        Some(BackupMark::Aborted)
    } else if id == ABORTED_WORKING_COPY_SLOT {
        Some(BackupMark::AbortedWorkingCopy)
    } else if id.starts_with(REBASED_PREFIX) {
        Some(BackupMark::Rebased)
    } else if id.starts_with(SUPERSEDED_PREFIX) {
        Some(BackupMark::Superseded)
    } else {
        None
    }
}

/// The `--glob` argument that selects the backups of tips a fetch found
/// rewritten away: the commits the remote once held.
pub(super) fn superseded_glob() -> String {
    format!(
        "--glob={}/*/{SUPERSEDED_PREFIX}*",
        RwvRef::Backup.namespace()
    )
}

fn backup_ref(name: &BackupName) -> String {
    format!("{}/{name}", RwvRef::Backup.namespace())
}

fn failed(args: &[&str], repo: &Path, stderr: String) -> VcsError {
    VcsError::CommandFailed {
        args: args.iter().map(|a| (*a).to_owned()).collect(),
        repo: repo.to_path_buf(),
        stderr,
    }
}

impl GitVcs {
    pub(super) fn ref_tip(
        repo: &Path,
        refname: &str,
    ) -> Result<Option<ResolvedRevisionId>, VcsError> {
        let spec = format!("{refname}^{{commit}}");
        match Self::run_trimmed(["rev-parse", "--verify", "--quiet", &spec], repo) {
            Ok(out) => Ok(ResolvedRevisionId::from_rev_parse_output(&out)),
            Err(VcsError::CommandFailed { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub(super) fn write_backup_ref(
        repo: &Path,
        source: &BackupSource,
        tip: &ResolvedRevisionId,
        mark: Option<BackupMark>,
    ) -> Result<Backup, VcsError> {
        let repo = Self::work_dir_for_store(repo);
        let full = tip.id().as_str();
        if slot(mark) {
            let name = BackupName::new(format!(
                "{}/{}",
                source_component(source),
                id_text(mark, full)
            ));
            Self::run(
                [
                    "update-ref",
                    "--create-reflog",
                    "-m",
                    "rwv abort",
                    &backup_ref(&name),
                    full,
                ],
                &repo,
            )?;
            return Ok(Backup {
                name,
                tip: tip.clone(),
                mark,
                source: source.clone(),
            });
        }
        for width in ID_WIDTH..=full.len() {
            let name = BackupName::new(format!(
                "{}/{}",
                source_component(source),
                id_text(mark, &full[..width])
            ));
            let refname = backup_ref(&name);
            match Self::ref_tip(&repo, &refname)? {
                None => {
                    Self::run(["update-ref", &refname, full], &repo)?;
                    return Ok(Backup {
                        name,
                        tip: tip.clone(),
                        mark,
                        source: source.clone(),
                    });
                }
                Some(held) if held == *tip => {
                    return Ok(Backup {
                        name,
                        tip: tip.clone(),
                        mark,
                        source: source.clone(),
                    })
                }
                Some(_) => {}
            }
        }
        Err(failed(
            &["backup"],
            &repo,
            format!("no unused backup name for {full}"),
        ))
    }

    pub(super) fn backups(
        repo: &Path,
        remote_tracking: RemoteTracking,
    ) -> Result<Vec<ListedBackup>, VcsError> {
        let repo = Self::work_dir_for_store(repo);
        let prefix = RwvRef::Backup.prefix();
        let out = Self::run_trimmed(
            ["for-each-ref", "--format=%(objectname) %(refname)", &prefix],
            &repo,
        )?;
        let mut listed = Vec::new();
        for line in out.lines().map(str::trim).filter(|l| !l.is_empty()) {
            let (oid, refname) = line
                .split_once(' ')
                .ok_or_else(|| failed(&["for-each-ref"], &repo, format!("unreadable: {line}")))?;
            let tip = ResolvedRevisionId::from_rev_parse_output(oid).ok_or_else(|| {
                failed(&["for-each-ref"], &repo, format!("not a commit id: {oid}"))
            })?;
            let name = refname.strip_prefix(&prefix).unwrap_or(refname);
            let (component, id) = name.rsplit_once('/').unwrap_or(("", name));
            let source = if component == DETACHED_HEAD {
                BackupSource::DetachedHead
            } else {
                BackupSource::Branch(crate::vcs::RawRefName::new(component))
            };
            let held = Self::commits_reached_by_no_branch(&repo, &tip, remote_tracking)?;
            listed.push(ListedBackup {
                backup: Backup {
                    name: BackupName::new(name),
                    tip,
                    mark: mark_of(id),
                    source,
                },
                held,
            });
        }
        Ok(listed)
    }

    pub(super) fn commits_reached_by_no_branch(
        repo: &Path,
        tip: &ResolvedRevisionId,
        remote_tracking: RemoteTracking,
    ) -> Result<Vec<CommitSummary>, VcsError> {
        let mut args = vec![
            "log",
            "--format=%H%x09%s",
            tip.id().as_str(),
            "--not",
            "--branches",
        ];
        if remote_tracking == RemoteTracking::Counted {
            args.push("--remotes");
        }
        args.push("--");
        let out = Self::run(&args[..], repo)?;
        out.lines()
            .filter(|l| !l.is_empty())
            .map(|line| {
                let (oid, subject) = line.split_once('\t').unwrap_or((line, ""));
                let id =
                    CommitId::parse(oid).map_err(|e| failed(&["log"], repo, format!("{e:#}")))?;
                Ok(CommitSummary {
                    id,
                    subject: subject.to_owned(),
                })
            })
            .collect()
    }

    pub(super) fn held_by_no_remote_tracking(repo: &Path) -> Result<Vec<HeldAlone>, VcsError> {
        let remotes = Self::run(["remote"], repo)?;
        let tracking: Vec<String> = remotes
            .lines()
            .map(str::trim)
            .filter(|r| !r.is_empty())
            .map(|r| format!("{REMOTE_TRACKING_ROOT}{r}/"))
            .collect();
        let remote_globs: Vec<String> = tracking
            .iter()
            .map(|root| format!("--glob={root}*"))
            .collect();
        let mut walk = vec![
            "rev-list".to_owned(),
            "--all".to_owned(),
            "--not".to_owned(),
        ];
        walk.extend(remote_globs.iter().cloned());
        walk.push("--".to_owned());
        let unreached = Self::run(walk, repo)?;
        let unreached: HashSet<&str> = unreached.lines().filter(|l| !l.is_empty()).collect();
        let listing = Self::run(
            [
                "for-each-ref",
                "--format=%(refname)%09%(objectname)%09%(objecttype)%09%(*objectname)%09%(*objecttype)",
            ],
            repo,
        )?;
        let mut held = Vec::new();
        let mut tips = HashSet::new();
        let mut not_commits = Vec::new();
        for line in listing.lines() {
            let fields: Vec<&str> = line.split('\t').collect();
            let [refname, object, kind, peeled, peeled_kind] = fields[..] else {
                continue;
            };
            let (tip, kind) = match peeled.is_empty() {
                true => (object, kind),
                false => (peeled, peeled_kind),
            };
            if tracking
                .iter()
                .any(|root| refname.starts_with(root.as_str()))
            {
                continue;
            }
            if kind != "commit" {
                not_commits.push((refname, tip));
            } else if unreached.contains(tip) {
                tips.insert(tip.to_owned());
                held.push(super::held_ref(refname));
            }
        }
        if !not_commits.is_empty() {
            let mut reach = vec!["rev-list".to_owned(), "--objects".to_owned()];
            reach.extend(remote_globs.iter().cloned());
            reach.push("--".to_owned());
            let reached = match remote_globs.is_empty() {
                true => String::new(),
                false => Self::run(reach, repo)?,
            };
            let reached: HashSet<&str> = reached
                .lines()
                .filter_map(|l| l.split(' ').next())
                .collect();
            held.extend(
                not_commits
                    .into_iter()
                    .filter(|(_, tip)| !reached.contains(tip))
                    .map(|(refname, _)| super::held_ref(refname)),
            );
        }
        let detached = Self::run(["symbolic-ref", "-q", "HEAD"], repo).is_err();
        if detached {
            let head = Self::run(["rev-parse", "--verify", "-q", "HEAD"], repo).unwrap_or_default();
            let head = head.trim();
            if unreached.contains(head) && !tips.contains(head) {
                held.push(HeldAlone {
                    kind: HeldKind::CheckedOut,
                    name: head.chars().take(ID_WIDTH).collect(),
                });
            }
        }
        Ok(held)
    }

    pub(super) fn holds_unreached(
        repo: &Path,
        tip: &CommitId,
        branch: Option<&crate::vcs::RawRefName>,
        beside: &CommitId,
    ) -> Result<bool, VcsError> {
        let exclude = branch.map(|branch| format!("--exclude={branch}"));
        let mut args = vec!["rev-list", "-n1", tip.as_str(), "--not", beside.as_str()];
        args.extend(exclude.as_deref());
        args.extend(["--branches", "--remotes", "--"]);
        Ok(!Self::run_trimmed(&args[..], repo)?.is_empty())
    }

    pub(super) fn show_kept_command(
        checkout: &Path,
        base: &CommitId,
        kept: &[&BackupName],
    ) -> String {
        let refs: Vec<String> = kept.iter().map(|name| backup_ref(name)).collect();
        format!(
            "git -C {} log -p --diff-merges=first-parent {} --not {} --branches --remotes",
            checkout.display(),
            refs.join(" "),
            base.as_str()
        )
    }

    pub(super) fn drop_backup_ref(repo: &Path, name: &BackupName) -> Result<(), VcsError> {
        let repo = Self::work_dir_for_store(repo);
        Self::run(["update-ref", "-d", &backup_ref(name)], &repo).map(|_| ())
    }

    pub(super) fn snapshot_working_tree(
        repo: &Path,
    ) -> Result<Option<ResolvedRevisionId>, VcsError> {
        Self::snapshot_working_tree_forcing(repo, &[])
    }

    /// [`Self::snapshot_working_tree`], the ignored paths `forced` names
    /// included.
    pub(super) fn snapshot_working_tree_forcing(
        repo: &Path,
        forced: &[String],
    ) -> Result<Option<ResolvedRevisionId>, VcsError> {
        let scratch = Scratch::take(repo)?;
        let head = Self::ref_tip(repo, "HEAD")?;
        Self::snapshot_into(repo, &scratch.path("backup-index"), head.as_ref(), forced)
    }

    fn snapshot_into(
        repo: &Path,
        index_path: &str,
        head: Option<&ResolvedRevisionId>,
        forced: &[String],
    ) -> Result<Option<ResolvedRevisionId>, VcsError> {
        let in_index = |args: &[&str]| -> Result<String, VcsError> {
            let output = git_command()
                .args(Argv::from(args.iter().copied()))
                .env("GIT_INDEX_FILE", index_path)
                .current_dir(repo)
                .output()
                .map_err(|e| VcsError::Io {
                    ctx: format!("failed to spawn git {args:?}"),
                    source: e,
                })?;
            if !output.status.success() {
                return Err(failed(
                    args,
                    repo,
                    String::from_utf8_lossy(&output.stderr).into_owned(),
                ));
            }
            Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
        };
        let real_index = Self::run_trimmed(
            ["rev-parse", "--path-format=absolute", "--git-path", "index"],
            repo,
        )?;
        let staged = match std::fs::copy(&real_index, index_path) {
            Ok(_) => in_index(&["write-tree"]).ok(),
            Err(_) => None,
        };
        if head.is_some() {
            in_index(&["read-tree", "HEAD"])?;
        }
        in_index(&["add", "-A"])?;
        if !forced.is_empty() {
            let mut add = vec!["add", "-f", "--"];
            add.extend(forced.iter().map(String::as_str));
            in_index(&add)?;
        }
        let tree = in_index(&["write-tree"])?;
        if let Some(head) = head {
            let head_tree = Self::run_trimmed(["rev-parse", "HEAD^{tree}"], repo)?;
            let staged = staged.filter(|staged| *staged != head_tree && *staged != tree);
            if head_tree == tree && staged.is_none() {
                return Ok(None);
            }
            let staged_commit = match &staged {
                Some(staged) => Some(Self::run_trimmed(
                    [
                        "commit-tree",
                        staged,
                        "-p",
                        head.id().as_str(),
                        "-m",
                        STAGED_MESSAGE,
                    ],
                    repo,
                )?),
                None => None,
            };
            let mut args = vec!["commit-tree", &tree, "-p", head.id().as_str()];
            if let Some(staged_commit) = &staged_commit {
                args.extend(["-p", staged_commit.as_str()]);
            }
            args.extend(["-m", EDITS_MESSAGE]);
            let commit = Self::run_trimmed(args, repo)?;
            return Ok(ResolvedRevisionId::from_rev_parse_output(&commit));
        }
        if in_index(&["ls-files"])?.is_empty() {
            return Ok(None);
        }
        let commit = Self::run_trimmed(["commit-tree", &tree, "-m", EDITS_MESSAGE], repo)?;
        Ok(ResolvedRevisionId::from_rev_parse_output(&commit))
    }

    pub(super) fn branch_tip_listing(repo: &Path) -> Result<Vec<BranchTip>, VcsError> {
        let repo = Self::work_dir_for_store(repo);
        let out = Self::run_trimmed(
            [
                "for-each-ref",
                "--format=%(objectname) %(symref) %(refname)",
                LOCAL_BRANCH_NAMESPACE,
                REMOTE_TRACKING_ROOT,
            ],
            &repo,
        )?;
        let remotes = Self::remote_names(&repo)?;
        let mut tips = Vec::new();
        for line in out.lines().map(str::trim).filter(|l| !l.is_empty()) {
            let mut parts = line.splitn(3, ' ');
            let (Some(oid), Some(symref), Some(refname)) =
                (parts.next(), parts.next(), parts.next())
            else {
                return Err(failed(
                    &["for-each-ref"],
                    &repo,
                    format!("unreadable: {line}"),
                ));
            };
            if !symref.is_empty() {
                continue;
            }
            let branch = Self::branch_ref_of(refname, &remotes);
            let tip = ResolvedRevisionId::from_rev_parse_output(oid)
                .ok_or_else(|| failed(&["for-each-ref"], &repo, format!("not a commit: {oid}")))?;
            tips.push(BranchTip {
                label: branch.to_string(),
                branch,
                tip,
            });
        }
        Ok(tips)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::op_state::SavepointKey;
    use crate::vcs::{Attribution, Vcs, VerifiedRestoreOutcome};

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = git_command()
            .args(args)
            .current_dir(dir)
            .output()
            .expect("run git");
        assert!(
            out.status.success(),
            "git {args:?} failed in {}: {}",
            dir.display(),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }

    fn repo() -> tempfile::TempDir {
        let tmp = crate::tempdir().unwrap();
        let p = tmp.path();
        git(p, &["init", "-b", "main"]);
        git(p, &["config", "user.email", "t@t"]);
        git(p, &["config", "user.name", "T"]);
        std::fs::write(p.join("f"), "1").unwrap();
        git(p, &["add", "."]);
        git(p, &["commit", "-m", "base"]);
        tmp
    }

    fn commit(p: &Path, name: &str) -> ResolvedRevisionId {
        std::fs::write(p.join(name), name).unwrap();
        git(p, &["add", "."]);
        git(p, &["commit", "-m", name]);
        GitVcs.head_revision(p).unwrap()
    }

    fn head(p: &Path) -> ResolvedRevisionId {
        GitVcs.head_revision(p).unwrap()
    }

    fn feature(name: &str) -> BackupSource {
        BackupSource::Branch(crate::vcs::RawRefName::new(name))
    }

    fn listed(p: &Path) -> Vec<ListedBackup> {
        GitVcs.list_backups(p, RemoteTracking::Counted).unwrap()
    }

    #[test]
    fn a_backup_is_named_for_its_branch_and_the_tip_it_holds() {
        let p = repo();
        let tip = commit(p.path(), "work");
        let made = GitVcs
            .create_backup(p.path(), &feature("feature/x"), &tip, None)
            .unwrap();

        let short = &tip.id().as_str()[..ID_WIDTH];
        assert_eq!(made.backup().name.as_str(), format!("feature/x/{short}"));
        assert_eq!(
            git(
                p.path(),
                &["rev-parse", &format!("refs/rwv/backup/feature/x/{short}")]
            ),
            tip.id().as_str()
        );
        assert_eq!(listed(p.path()).len(), 1);
        assert_eq!(listed(p.path())[0].backup, *made.backup());
    }

    #[test]
    fn backing_up_the_same_tip_twice_names_one_backup() {
        let p = repo();
        let tip = commit(p.path(), "work");
        let a = GitVcs
            .write_backup(p.path(), &feature("b"), &tip, None)
            .unwrap();
        let b = GitVcs
            .write_backup(p.path(), &feature("b"), &tip, None)
            .unwrap();

        assert_eq!(a, b);
        assert_eq!(listed(p.path()).len(), 1);
    }

    #[test]
    fn a_mark_is_part_of_the_name_and_is_read_back() {
        let p = repo();
        let tip = commit(p.path(), "work");
        let every = [None, Some(BackupMark::Rebased)];
        for mark in every {
            GitVcs
                .write_backup(p.path(), &feature("b"), &tip, mark)
                .unwrap();
        }

        let marks: Vec<Option<BackupMark>> = listed(p.path())
            .into_iter()
            .map(|l| l.backup.mark)
            .collect();
        assert_eq!(
            marks.len(),
            every.len(),
            "one tip backed up under each mark is one backup each: {marks:?}"
        );
        for expected in every {
            assert!(
                marks.contains(&expected),
                "{expected:?} not read back: {marks:?}"
            );
        }
    }

    #[test]
    fn a_short_id_another_commit_holds_takes_a_longer_one() {
        let p = repo();
        let first = commit(p.path(), "first");
        let second = commit(p.path(), "second");
        let squatter = format!("refs/rwv/backup/b/{}", &second.id().as_str()[..ID_WIDTH]);
        git(p.path(), &["update-ref", &squatter, first.id().as_str()]);

        let made = GitVcs
            .write_backup(p.path(), &feature("b"), &second, None)
            .unwrap();

        assert_eq!(
            made.name.as_str(),
            format!("b/{}", &second.id().as_str()[..ID_WIDTH + 1]),
            "the name the squatter holds is not overwritten"
        );
        assert_eq!(
            git(p.path(), &["rev-parse", &squatter]),
            first.id().as_str()
        );
    }

    #[test]
    fn a_backup_holds_what_no_branch_reaches() {
        let p = repo();
        git(p.path(), &["checkout", "-b", "feature"]);
        let work = commit(p.path(), "work");
        GitVcs
            .write_backup(p.path(), &feature("feature"), &work, None)
            .unwrap();

        let still_on_a_branch = listed(p.path());
        assert!(
            still_on_a_branch[0].redundant(),
            "a branch reaches the tip, so the backup holds nothing of its own"
        );

        git(p.path(), &["checkout", "main"]);
        git(p.path(), &["branch", "-D", "feature"]);

        let alone = listed(p.path());
        assert!(!alone[0].redundant());
        assert_eq!(
            alone[0]
                .held
                .iter()
                .map(|c| c.id.clone())
                .collect::<Vec<_>>(),
            vec![work.id().clone()],
            "the one commit no branch reaches"
        );
        assert_eq!(alone[0].held[0].subject, "work");
    }

    #[test]
    fn remote_tracking_branches_count_only_where_asked() {
        let origin = repo();
        let clone = crate::tempdir().unwrap();
        git(
            clone.path(),
            &["clone", "-q", origin.path().to_str().unwrap(), "c"],
        );
        let c = clone.path().join("c");
        git(&c, &["config", "user.email", "t@t"]);
        git(&c, &["config", "user.name", "T"]);
        let tip = head(&c);
        let detached = crate::vcs::RawRefName::new("detached");
        GitVcs
            .write_backup(&c, &BackupSource::Branch(detached), &tip, None)
            .unwrap();
        git(&c, &["checkout", "-q", "--detach"]);
        git(&c, &["branch", "-D", "main"]);

        assert!(GitVcs.list_backups(&c, RemoteTracking::Counted).unwrap()[0].redundant());
        assert!(
            !GitVcs.list_backups(&c, RemoteTracking::Ignored).unwrap()[0].redundant(),
            "origin/main is not known to be current, so it vouches for nothing"
        );
    }

    #[test]
    fn a_detached_head_is_backed_up_under_head_when_no_branch_reaches_it() {
        let p = repo();
        assert!(
            GitVcs.backup_unpreserved_head(p.path()).unwrap().is_none(),
            "attached"
        );
        git(p.path(), &["checkout", "--detach"]);
        assert!(
            GitVcs.backup_unpreserved_head(p.path()).unwrap().is_none(),
            "main reaches it"
        );
        let work = commit(p.path(), "work");

        let kept = GitVcs
            .backup_unpreserved_head(p.path())
            .unwrap()
            .expect("detached with commits no branch reaches");

        assert_eq!(
            kept.backup().name.as_str(),
            format!("HEAD/{}", &work.id().as_str()[..ID_WIDTH])
        );
        assert_eq!(kept.backup().tip, work);
    }

    #[test]
    fn edits_are_kept_as_a_commit_on_head_and_the_checkout_is_not_touched() {
        let p = repo();
        let base = head(p.path());
        std::fs::write(p.path().join("f"), "changed").unwrap();
        std::fs::write(p.path().join("untracked"), "new").unwrap();
        std::fs::write(p.path().join(".gitignore"), "ignored\n").unwrap();
        std::fs::write(p.path().join("ignored"), "noise").unwrap();
        git(p.path(), &["add", ".gitignore"]);
        let status_before = git(p.path(), &["status", "--porcelain"]);
        let index_before = git(p.path(), &["diff", "--cached", "--name-only"]);

        let kept = GitVcs
            .backup_edits(p.path())
            .unwrap()
            .expect("the checkout is dirty");

        let tip = kept.backup().tip.id().as_str().to_owned();
        assert_eq!(
            git(p.path(), &["rev-parse", &format!("{tip}^")]),
            base.id().as_str()
        );
        assert_eq!(git(p.path(), &["show", &format!("{tip}:f")]), "changed");
        assert_eq!(git(p.path(), &["show", &format!("{tip}:untracked")]), "new");
        let files = git(p.path(), &["ls-tree", "-r", "--name-only", &tip]);
        assert!(!files.lines().any(|f| f == "ignored"), "{files}");
        assert!(kept.backup().name.as_str().starts_with("main/"));
        assert_eq!(git(p.path(), &["status", "--porcelain"]), status_before);
        assert_eq!(
            git(p.path(), &["diff", "--cached", "--name-only"]),
            index_before,
            "the real index is not the one the snapshot was built in"
        );
        assert_eq!(head(p.path()), base);
    }

    #[test]
    fn a_clean_checkout_has_no_edits_to_keep() {
        let p = repo();
        std::fs::write(p.path().join(".gitignore"), "ignored\n").unwrap();
        git(p.path(), &["add", "."]);
        git(p.path(), &["commit", "-m", "ignore"]);
        std::fs::write(p.path().join("ignored"), "noise").unwrap();

        assert!(GitVcs.backup_edits(p.path()).unwrap().is_none());
        assert!(listed(p.path()).is_empty());
    }

    #[test]
    fn dropping_a_backup_removes_its_ref_and_no_other() {
        let p = repo();
        let one = commit(p.path(), "one");
        let two = commit(p.path(), "two");
        let a = GitVcs
            .write_backup(p.path(), &feature("b"), &one, None)
            .unwrap();
        let b = GitVcs
            .write_backup(p.path(), &feature("b"), &two, None)
            .unwrap();

        GitVcs.drop_backup(p.path(), &a.name).unwrap();

        assert_eq!(
            listed(p.path())
                .into_iter()
                .map(|l| l.backup)
                .collect::<Vec<_>>(),
            vec![b]
        );
        assert_eq!(head(p.path()), two, "no branch was touched");
    }

    #[test]
    fn branch_tips_list_local_and_remote_tracking_branches_but_not_the_remote_head() {
        let origin = repo();
        git(origin.path(), &["branch", "side"]);
        let clone = crate::tempdir().unwrap();
        git(
            clone.path(),
            &["clone", "-q", origin.path().to_str().unwrap(), "c"],
        );
        let c = clone.path().join("c");

        let mut labels: Vec<String> = GitVcs
            .branch_tips(&c)
            .unwrap()
            .into_iter()
            .map(|t| t.label)
            .collect();
        labels.sort();

        assert_eq!(labels, vec!["main", "origin/main", "origin/side"]);
    }

    #[test]
    fn a_pruning_fetch_drops_what_a_remote_dropped_and_names_a_remote_it_could_not_reach() {
        let origin = repo();
        git(origin.path(), &["branch", "gone-soon"]);
        let clone = crate::tempdir().unwrap();
        git(
            clone.path(),
            &["clone", "-q", origin.path().to_str().unwrap(), "c"],
        );
        let c = clone.path().join("c");
        git(origin.path(), &["branch", "-D", "gone-soon"]);
        git(&c, &["remote", "add", "lost", "/nonexistent/remote/path"]);

        let unconfirmed = crate::fetch::pruning_fetch(&GitVcs, &c, None)
            .unwrap()
            .unconfirmed;

        assert_eq!(unconfirmed.len(), 1);
        assert_eq!(unconfirmed[0].remote, "lost");
        let labels: Vec<String> = GitVcs
            .branch_tips(&c)
            .unwrap()
            .into_iter()
            .map(|t| t.label)
            .collect();
        assert!(
            !labels.contains(&"origin/gone-soon".to_owned()),
            "{labels:?}: the reachable remote was pruned although another was not"
        );
    }

    fn foreign_tip_fixture() -> (
        tempfile::TempDir,
        SavepointKey,
        ResolvedRevisionId,
        ResolvedRevisionId,
    ) {
        let p = repo();
        let key = SavepointKey::Op("1779769917405921588".parse().unwrap());
        let savepoint = GitVcs.create_savepoint(p.path(), &key).unwrap();
        let foreign = commit(p.path(), "foreign");
        (p, key, savepoint, foreign)
    }

    #[test]
    fn a_tip_holding_a_commit_nothing_else_reaches_is_kept_in_the_branch_slot_before_the_reset() {
        let (p, key, savepoint, foreign) = foreign_tip_fixture();

        let outcome = GitVcs
            .verified_restore_savepoint(p.path(), &key, &Attribution::default())
            .unwrap();

        let VerifiedRestoreOutcome::Kept { kept, .. } = &outcome else {
            panic!("expected the tip to be kept, got {outcome:?}");
        };
        let [backup] = kept.as_slice() else {
            panic!("expected one kept tip, got {outcome:?}");
        };
        assert_eq!(backup.backup().mark, Some(BackupMark::Aborted));
        assert_eq!(backup.backup().name.as_str(), "main/aborted");
        assert_eq!(
            head(p.path()),
            savepoint,
            "the repo is back at the savepoint"
        );
        assert_eq!(backup.backup().tip, foreign);
        let held = listed(p.path());
        assert_eq!(held.len(), 1);
        assert_eq!(held[0].backup, *backup.backup());
    }
}
