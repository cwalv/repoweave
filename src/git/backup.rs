//! Backups, as git keeps them: one ref per backup under [`BACKUP_NAMESPACE`],
//! named for the branch it backs up and an id.
//!
//! The id is the abbreviated id of the backed-up tip, behind the mark when
//! there is one, so backing up the same tip twice names one ref. A tip whose
//! abbreviation another commit already holds takes a longer one.

use super::{git_command, Argv, GitVcs, BACKUP_NAMESPACE, LOCAL_BRANCH_NAMESPACE};
use crate::branch_ref::BranchRef;
use crate::vcs::{
    Backup, BackupMark, BackupName, BackupSource, BranchTip, CommitId, CommitSummary, ListedBackup,
    RawRefName, RemoteHandle, RemoteTracking, ResolvedRevisionId, VcsError,
};
use std::path::Path;

const REMOTE_TRACKING_ROOT: &str = "refs/remotes/";
const ID_WIDTH: usize = 12;
const DETACHED_HEAD: &str = "HEAD";
const REBASED_PREFIX: &str = "rebased-";
const ABANDONED_PREFIX: &str = "abandoned-";
const EDITS_MESSAGE: &str = "rwv backup of uncommitted edits";

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
        Some(BackupMark::Abandoned) => format!("{ABANDONED_PREFIX}{hex}"),
    }
}

fn mark_of(id: &str) -> Option<BackupMark> {
    if id.starts_with(REBASED_PREFIX) {
        Some(BackupMark::Rebased)
    } else if id.starts_with(ABANDONED_PREFIX) {
        Some(BackupMark::Abandoned)
    } else {
        None
    }
}

fn backup_ref(name: &BackupName) -> String {
    format!("{BACKUP_NAMESPACE}/{name}")
}

fn failed(args: &[&str], repo: &Path, stderr: String) -> VcsError {
    VcsError::CommandFailed {
        args: args.iter().map(|a| (*a).to_owned()).collect(),
        repo: repo.to_path_buf(),
        stderr,
    }
}

impl GitVcs {
    fn ref_tip(repo: &Path, refname: &str) -> Result<Option<ResolvedRevisionId>, VcsError> {
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
                    });
                }
                Some(held) if held == *tip => {
                    return Ok(Backup {
                        name,
                        tip: tip.clone(),
                        mark,
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
        let prefix = format!("{BACKUP_NAMESPACE}/");
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
            let id = name.rsplit('/').next().unwrap_or(name);
            let held = Self::commits_reached_by_no_branch(&repo, &tip, remote_tracking)?;
            listed.push(ListedBackup {
                backup: Backup {
                    name: BackupName::new(name),
                    tip,
                    mark: mark_of(id),
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

    pub(super) fn unreached_without(
        &self,
        repo: &Path,
        commits: &[CommitId],
        dropping: &[BackupName],
    ) -> Result<Vec<CommitId>, VcsError> {
        let repo = Self::work_dir_for_store(repo);
        let dropped: Vec<String> = dropping.iter().map(backup_ref).collect();
        let backups = format!("{BACKUP_NAMESPACE}/");
        let mut unreached = Vec::new();
        for commit in commits {
            if !crate::vcs::Vcs::commit_object_exists(self, &repo, commit)? {
                continue;
            }
            let holders = Self::run_trimmed(
                [
                    "for-each-ref",
                    "--format=%(refname)",
                    "--contains",
                    commit.as_str(),
                    "refs/heads/",
                    "refs/remotes/",
                    &backups,
                ],
                &repo,
            )?;
            if holders
                .lines()
                .all(|holder| dropped.iter().any(|d| d == holder.trim()))
            {
                unreached.push(commit.clone());
            }
        }
        Ok(unreached)
    }

    pub(super) fn drop_backup_ref(repo: &Path, name: &BackupName) -> Result<(), VcsError> {
        let repo = Self::work_dir_for_store(repo);
        Self::run(["update-ref", "-d", &backup_ref(name)], &repo).map(|_| ())
    }

    pub(super) fn snapshot_working_tree(
        repo: &Path,
    ) -> Result<Option<ResolvedRevisionId>, VcsError> {
        let index_path = Self::run_trimmed(
            [
                "rev-parse",
                "--path-format=absolute",
                "--git-path",
                &format!("rwv-backup-index-{}", std::process::id()),
            ],
            repo,
        )?;
        let head = Self::ref_tip(repo, "HEAD")?;
        let outcome = Self::snapshot_into(repo, &index_path, head.as_ref());
        let _ = std::fs::remove_file(&index_path);
        outcome
    }

    fn snapshot_into(
        repo: &Path,
        index_path: &str,
        head: Option<&ResolvedRevisionId>,
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
        if head.is_some() {
            in_index(&["read-tree", "HEAD"])?;
        }
        in_index(&["add", "-A"])?;
        let tree = in_index(&["write-tree"])?;
        if let Some(head) = head {
            let head_tree = Self::run_trimmed(["rev-parse", "HEAD^{tree}"], repo)?;
            if head_tree == tree {
                return Ok(None);
            }
            let commit = Self::run_trimmed(
                [
                    "commit-tree",
                    &tree,
                    "-p",
                    head.id().as_str(),
                    "-m",
                    EDITS_MESSAGE,
                ],
                repo,
            )?;
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
        let mut remotes = Self::remote_names(&repo)?;
        remotes.sort_by_key(|name| std::cmp::Reverse(name.len()));
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
            let branch = if let Some(local) = refname.strip_prefix(LOCAL_BRANCH_NAMESPACE) {
                BranchRef::Local(RawRefName::new(local))
            } else {
                let tracked = refname
                    .strip_prefix(REMOTE_TRACKING_ROOT)
                    .unwrap_or(refname);
                let (remote, name) = remotes
                    .iter()
                    .find_map(|r| {
                        tracked
                            .strip_prefix(r.as_str())
                            .and_then(|rest| rest.strip_prefix('/'))
                            .map(|name| (r.as_str(), name))
                    })
                    .or_else(|| tracked.split_once('/'))
                    .unwrap_or(("", tracked));
                BranchRef::Remote {
                    remote: RemoteHandle::new(remote),
                    name: RawRefName::new(name),
                }
            };
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
    use crate::vcs::{ForeignTipPolicy, Vcs, VerifiedRestoreOutcome};

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
        for mark in [None, Some(BackupMark::Rebased), Some(BackupMark::Abandoned)] {
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
            3,
            "one tip backed up three ways is three backups: {marks:?}"
        );
        for expected in [None, Some(BackupMark::Rebased), Some(BackupMark::Abandoned)] {
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
    fn a_commit_is_left_unreached_only_when_every_holder_is_being_dropped() {
        let p = repo();
        git(p.path(), &["checkout", "--detach"]);
        let pinned = commit(p.path(), "pinned");
        git(p.path(), &["checkout", "-q", "main"]);
        let a = GitVcs
            .write_backup(p.path(), &feature("a"), &pinned, None)
            .unwrap();
        let b = GitVcs
            .write_backup(
                p.path(),
                &feature("b"),
                &pinned,
                Some(BackupMark::Abandoned),
            )
            .unwrap();
        let pins = [pinned.id().clone()];
        let stray = CommitId::parse(&"1".repeat(40)).unwrap();

        let left = |dropping: &[BackupName]| {
            GitVcs
                .left_unreached_by_dropping(p.path(), &pins, dropping)
                .unwrap()
        };

        assert!(
            left(std::slice::from_ref(&a.name)).is_empty(),
            "the other backup reaches it"
        );
        assert_eq!(
            left(&[a.name.clone(), b.name.clone()]),
            pins.to_vec(),
            "dropped together, nothing reaches it"
        );
        git(p.path(), &["branch", "keeper", pinned.id().as_str()]);
        assert!(
            left(&[a.name, b.name]).is_empty(),
            "a branch reaches it, whatever is dropped"
        );
        assert!(
            GitVcs
                .left_unreached_by_dropping(p.path(), &[stray], &[])
                .unwrap()
                .is_empty(),
            "a commit this repo lacks is not one dropping can unreach"
        );
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
    fn a_foreign_tip_is_refused_and_nothing_is_written_without_consent() {
        let (p, key, savepoint, foreign) = foreign_tip_fixture();

        let outcome = GitVcs
            .verified_restore_savepoint(p.path(), &key, None, None, ForeignTipPolicy::Refuse)
            .unwrap();

        assert!(matches!(outcome, VerifiedRestoreOutcome::ForeignTip { .. }));
        assert_eq!(head(p.path()), foreign);
        assert_ne!(head(p.path()), savepoint);
        assert!(listed(p.path()).is_empty());
    }

    #[test]
    fn an_abandoned_foreign_tip_is_kept_as_an_abandoned_backup_before_the_reset() {
        let (p, key, savepoint, foreign) = foreign_tip_fixture();

        let outcome = GitVcs
            .verified_restore_savepoint(p.path(), &key, None, None, ForeignTipPolicy::Abandon)
            .unwrap();

        let VerifiedRestoreOutcome::AbandonedForeignTip { backup, .. } = outcome else {
            panic!("expected the tip to be abandoned, got {outcome:?}");
        };
        assert_eq!(
            head(p.path()),
            savepoint,
            "the repo is back at the savepoint"
        );
        assert_eq!(backup.backup().tip, foreign);
        assert_eq!(backup.backup().mark, Some(BackupMark::Abandoned));
        let held = listed(p.path());
        assert_eq!(held.len(), 1);
        assert_eq!(held[0].backup, *backup.backup());
        assert_eq!(
            git(
                p.path(),
                &["for-each-ref", "--format=%(refname)", "refs/rwv/pre-abort/"]
            ),
            "",
            "no pre-abort ref is written"
        );
    }
}
