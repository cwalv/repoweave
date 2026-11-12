//! Each answer the backend draws from a git command that failed, or might
//! have, driven through real git with git's locale pinned and again with git
//! unpinned and speaking German. The answer comes from an exit status, a
//! machine-readable format or the repository's state, so it is the same in
//! both.

use super::records::{pushed_ref, PushedRef, RECORDS_BRANCH};
use super::*;
use std::time::Duration;

fn git(dir: &Path, args: &[&str]) -> String {
    let out = git_command_in_test()
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

fn identify(repo: &Path) {
    git(repo, &["config", "user.email", "t@t"]);
    git(repo, &["config", "user.name", "T"]);
}

/// A repo on `main` with one commit holding the file `f`.
fn repo() -> (tempfile::TempDir, PathBuf) {
    let tmp = crate::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    identify(&repo);
    std::fs::write(repo.join("f"), "content\n").unwrap();
    git(&repo, &["add", "f"]);
    git(&repo, &["commit", "-q", "-m", "one"]);
    (tmp, repo)
}

fn not_a_repo() -> tempfile::TempDir {
    crate::tempdir().unwrap()
}

fn break_config(repo: &Path) {
    let config = repo.join(".git").join("config");
    let mut text = std::fs::read_to_string(&config).unwrap();
    text.push_str("[broken\n");
    std::fs::write(config, text).unwrap();
}

fn at(repo: &Path, rev: &str) -> ResolvedRevisionId {
    ResolvedRevisionId::from_canonical(
        CommitId::parse(&git(repo, &["rev-parse", rev])).unwrap(),
        None,
    )
}

const ABSENT_COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

fn absent_commit() -> ResolvedRevisionId {
    ResolvedRevisionId::from_canonical(CommitId::parse(ABSENT_COMMIT).unwrap(), None)
}

fn is_not_found(result: &Result<impl std::fmt::Debug, VcsError>) -> bool {
    matches!(result, Err(VcsError::RevisionNotFound { .. }))
}

fn a_file_is_read_and_its_absence_told_from_a_failure() {
    let (_tmp, repo) = repo();
    let head = at(&repo, "HEAD");

    let read = GitVcs.read_file_at_revision(&repo, &head, Path::new("f"));
    assert_eq!(read.unwrap(), "content\n");

    let no_path = GitVcs.read_file_at_revision(&repo, &head, Path::new("missing"));
    assert!(is_not_found(&no_path), "{no_path:?}");

    let no_revision = GitVcs.read_file_at_revision(&repo, &absent_commit(), Path::new("f"));
    assert!(is_not_found(&no_revision), "{no_revision:?}");

    let blob = git(&repo, &["rev-parse", "HEAD:f"]);
    let loose = repo
        .join(".git")
        .join("objects")
        .join(&blob[..2])
        .join(&blob[2..]);
    std::fs::remove_file(loose).unwrap();
    let lost = GitVcs.read_file_at_revision(&repo, &head, Path::new("f"));
    assert!(
        matches!(lost, Err(VcsError::CommandFailed { .. })),
        "a blob the store lost is a failure, not an absence: {lost:?}"
    );
}

fn a_revision_resolves_or_is_not_found_or_fails() {
    let (_tmp, repo) = repo();
    let head = git(&repo, &["rev-parse", "HEAD"]);
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "two"]);
    git(&repo, &["tag", "same", &head]);
    git(&repo, &["branch", "same", "HEAD"]);

    let ambiguous = GitVcs.resolve_revision(&repo, "same").unwrap();
    assert_eq!(
        ambiguous.id().as_str(),
        head,
        "git reads a tag before a branch"
    );

    let missing = GitVcs.resolve_revision(&repo, "no-such-revision");
    assert!(is_not_found(&missing), "{missing:?}");

    let elsewhere = not_a_repo();
    let failed = GitVcs.resolve_revision(elsewhere.path(), "HEAD");
    assert!(
        matches!(failed, Err(VcsError::CommandFailed { .. })),
        "{failed:?}"
    );
}

fn a_range_endpoint_resolves_or_is_not_found_or_fails() {
    let (_tmp, repo) = repo();
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "two"]);

    let listed = GitVcs.log(&repo, Some("HEAD~1..HEAD")).unwrap();
    assert_eq!(listed.commits.len(), 1);

    let missing = GitVcs.log(&repo, Some("no-such-revision..HEAD"));
    assert!(is_not_found(&missing), "{missing:?}");

    let elsewhere = not_a_repo();
    let failed = GitVcs.log(elsewhere.path(), Some("HEAD~1..HEAD"));
    assert!(
        matches!(failed, Err(VcsError::CommandFailed { .. })),
        "{failed:?}"
    );
}

fn head_is_a_commit_or_unborn_or_a_failure() {
    let (_tmp, repo) = repo();
    let head = GitVcs.head_revision(&repo).unwrap();
    assert_eq!(head.id().as_str(), git(&repo, &["rev-parse", "HEAD"]));

    let fresh = crate::tempdir().unwrap();
    git(fresh.path(), &["init", "-q", "-b", "trunk"]);
    let unborn = GitVcs.head_revision(fresh.path());
    let Err(VcsError::CommandFailed { stderr, .. }) = &unborn else {
        panic!("{unborn:?}");
    };
    assert!(stderr.contains("unborn HEAD"), "{stderr}");
    assert!(
        stderr.contains("'trunk'"),
        "the unborn branch is named: {stderr}"
    );

    let elsewhere = not_a_repo();
    let failed = GitVcs.head_revision(elsewhere.path());
    let Err(VcsError::CommandFailed { stderr, .. }) = &failed else {
        panic!("{failed:?}");
    };
    assert!(!stderr.contains("unborn HEAD"), "{stderr}");
}

fn a_config_key_is_set_or_unset_or_unreadable() {
    let (_tmp, repo) = repo();
    git(&repo, &["config", "branch.main.remote", "origin"]);
    git(
        &repo,
        &["config", "remote.origin.url", "https://example.invalid/r"],
    );

    assert_eq!(
        GitVcs::config_get(&repo, "branch.main.remote").unwrap(),
        Some("origin".to_owned())
    );
    assert!(GitVcs::has_branch_section(&repo, "main").unwrap());
    assert_eq!(GitVcs.recorded_urls(&repo).unwrap().len(), 1);

    let (_unset_tmp, unset) = self::repo();
    assert_eq!(
        GitVcs::config_get(&unset, "branch.main.remote").unwrap(),
        None
    );
    assert!(!GitVcs::has_branch_section(&unset, "main").unwrap());
    assert!(GitVcs.recorded_urls(&unset).unwrap().is_empty());
    assert!(GitVcs::push_target_remotes(&unset, None)
        .unwrap()
        .is_empty());

    break_config(&repo);
    assert!(GitVcs::config_get(&repo, "branch.main.remote").is_err());
    assert!(GitVcs::has_branch_section(&repo, "main").is_err());
    assert!(GitVcs.recorded_urls(&repo).is_err());
    assert!(GitVcs::push_target_remotes(&repo, None).is_err());
}

fn commits_share_a_base_or_none_or_the_question_fails() {
    let (_tmp, repo) = repo();
    let base = CommitId::parse(&git(&repo, &["rev-parse", "HEAD"])).unwrap();
    let unrelated = CommitId::parse(&git(
        &repo,
        &["commit-tree", "HEAD^{tree}", "-m", "unrelated"],
    ))
    .unwrap();

    assert_eq!(
        GitVcs::merge_base_list(&repo, &base, &base).unwrap(),
        vec![base.clone()]
    );
    assert!(GitVcs::merge_base_list(&repo, &base, &unrelated)
        .unwrap()
        .is_empty());
    let absent = CommitId::parse(ABSENT_COMMIT).unwrap();
    assert!(GitVcs::merge_base_list(&repo, &base, &absent).is_err());
}

/// A bare `origin`, a clone of it publishing records, and a second clone
/// standing for another machine.
struct Records {
    _tmp: tempfile::TempDir,
    origin: PathBuf,
    here: PathBuf,
    there: PathBuf,
}

impl Records {
    fn new() -> Records {
        let (tmp, seed) = repo();
        let origin = tmp.path().join("origin.git");
        git(
            tmp.path(),
            &[
                "clone",
                "-q",
                "--bare",
                seed.to_str().unwrap(),
                origin.to_str().unwrap(),
            ],
        );
        let clone = |name: &str| {
            let path = tmp.path().join(name);
            git(
                tmp.path(),
                &[
                    "clone",
                    "-q",
                    origin.to_str().unwrap(),
                    path.to_str().unwrap(),
                ],
            );
            identify(&path);
            path
        };
        let (here, there) = (clone("here"), clone("there"));
        Records {
            _tmp: tmp,
            origin,
            here,
            there,
        }
    }

    fn commit(&self, repo: &Path, parent: Option<&CommitId>, message: &str) -> CommitId {
        let tree = git(repo, &["rev-parse", "HEAD^{tree}"]);
        let mut args = vec!["commit-tree", tree.as_str(), "-m", message];
        if let Some(parent) = parent {
            args.extend(["-p", parent.as_str()]);
        }
        CommitId::parse(&git(repo, &args)).unwrap()
    }

    fn publish(&self, repo: &Path, commit: &CommitId) -> Result<RecordsPublish, VcsError> {
        GitVcs::publish_records_commit(repo, &RemoteHandle::new("origin"), commit)
    }

    fn remote_tip(&self) -> String {
        git(
            &self.origin,
            &["rev-parse", &format!("refs/heads/{RECORDS_BRANCH}")],
        )
    }

    fn hook(&self, name: &str, body: &str) {
        let hook = self.origin.join("hooks").join(name);
        std::fs::write(&hook, format!("#!/bin/sh\n{body}\n")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
}

fn a_records_commit_is_published_where_the_remote_has_not_moved() {
    let fx = Records::new();
    let first = fx.commit(&fx.here, None, "first");
    assert_eq!(
        fx.publish(&fx.here, &first).unwrap(),
        RecordsPublish::Published
    );
    let second = fx.commit(&fx.here, Some(&first), "second");
    assert_eq!(
        fx.publish(&fx.here, &second).unwrap(),
        RecordsPublish::Published
    );
    assert_eq!(fx.remote_tip(), second.as_str());
}

/// Another machine published first, a commit this clone has never seen.
fn a_records_commit_built_on_a_tip_the_remote_left_is_a_lost_race() {
    let fx = Records::new();
    let first = fx.commit(&fx.here, None, "first");
    fx.publish(&fx.here, &first).unwrap();
    git(&fx.there, &["fetch", "-q", "origin", RECORDS_BRANCH]);
    let theirs = fx.commit(&fx.there, Some(&first), "theirs");
    fx.publish(&fx.there, &theirs).unwrap();

    let ours = fx.commit(&fx.here, Some(&first), "ours");
    assert_eq!(
        fx.publish(&fx.here, &ours).unwrap(),
        RecordsPublish::RemoteMoved
    );
    assert_eq!(fx.remote_tip(), theirs.as_str());
}

/// The remote advertised the tip this commit was built on, then moved
/// before swapping its ref: git reports that as the remote refusing.
fn a_records_commit_the_remote_lost_the_swap_for_is_a_lost_race() {
    let fx = Records::new();
    let first = fx.commit(&fx.here, None, "first");
    fx.publish(&fx.here, &first).unwrap();
    let theirs = fx.commit(&fx.here, Some(&first), "theirs");
    git(
        &fx.here,
        &[
            "push",
            "-q",
            "origin",
            &format!("{}:refs/heads/elsewhere", theirs.as_str()),
        ],
    );
    fx.hook(
        "pre-receive",
        &format!(
            "unset GIT_QUARANTINE_PATH\ngit update-ref refs/heads/{RECORDS_BRANCH} {}",
            theirs.as_str()
        ),
    );

    let ours = fx.commit(&fx.here, Some(&first), "ours");
    assert_eq!(
        fx.publish(&fx.here, &ours).unwrap(),
        RecordsPublish::RemoteMoved
    );
    assert_eq!(fx.remote_tip(), theirs.as_str());
}

fn a_remote_refusing_the_records_branch_says_why() {
    let fx = Records::new();
    fx.hook("update", "echo this remote keeps no records >&2\nexit 1");

    let first = fx.commit(&fx.here, None, "first");
    let refused = fx.publish(&fx.here, &first).unwrap();
    let RecordsPublish::Rejected(why) = &refused else {
        panic!("{refused:?}");
    };
    assert!(why.contains("this remote keeps no records"), "{why}");
}

fn a_remote_that_cannot_be_reached_is_an_error() {
    let fx = Records::new();
    git(
        &fx.here,
        &[
            "remote",
            "set-url",
            "origin",
            fx.origin.join("gone").to_str().unwrap(),
        ],
    );
    let first = fx.commit(&fx.here, None, "first");
    let unreached = fx.publish(&fx.here, &first);
    assert!(
        matches!(unreached, Err(VcsError::CommandFailed { .. })),
        "{unreached:?}"
    );
}

/// A repo with a worktree whose `post-checkout` refuses every checkout.
fn store_refusing_checkouts() -> (tempfile::TempDir, PathBuf) {
    let (tmp, repo) = repo();
    let hook = repo.join(".git").join("hooks").join("post-checkout");
    std::fs::write(&hook, "#!/bin/sh\nexit 1\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    (tmp, repo)
}

/// The registration a refused `worktree add` leaves is its own, not a
/// peer's, so the add is not made again.
fn an_add_that_leaves_its_own_registration_is_not_repeated() {
    let (tmp, store) = store_refusing_checkouts();
    let dest = tmp.path().join("added");
    let dest_arg = dest.to_str().unwrap().to_owned();
    let attempts = std::cell::Cell::new(0);

    let added =
        GitVcs::awaiting_peer_registration(&store, Some(&dest), Duration::from_secs(5), || {
            attempts.set(attempts.get() + 1);
            GitVcs::run(["worktree", "add", "--detach", &dest_arg, "HEAD"], &store)
        });

    assert!(added.is_err(), "the hook refused the checkout");
    assert!(
        dest.join(".git").exists(),
        "the add registered its worktree"
    );
    assert_eq!(attempts.get(), 1);
}

/// What git said does not decide whether a peer is waited on: a failure
/// while a registration is unwritten is waited out whatever its words, and
/// one while none is in flight is believed at once whatever its words.
fn the_wait_turns_on_the_registrations_not_on_what_git_said() {
    let (tmp, store) = repo();
    let peer = tmp.path().join("peer");
    git(
        &store,
        &["worktree", "add", "-q", "--detach", peer.to_str().unwrap()],
    );
    let commondir = store
        .join(".git")
        .join("worktrees")
        .join("peer")
        .join("commondir");
    let written = std::fs::read(&commondir).unwrap();
    let failure = |stderr: &str| VcsError::CommandFailed {
        args: vec!["worktree".to_owned(), "list".to_owned()],
        repo: store.clone(),
        stderr: stderr.to_owned(),
    };
    let attempts = std::cell::Cell::new(0);

    let waited = GitVcs::awaiting_peer_registration(&store, None, Duration::from_secs(5), || {
        attempts.set(attempts.get() + 1);
        match attempts.get() {
            1 => {
                std::fs::write(&commondir, b"").unwrap();
                Err(failure("Schwerwiegend: konnte nicht lesen"))
            }
            2 => {
                std::fs::write(&commondir, &written).unwrap();
                Ok(())
            }
            _ => unreachable!(),
        }
    });
    waited.expect("the unwritten registration was waited out");
    assert_eq!(attempts.get(), 2);

    attempts.set(0);
    let believed = GitVcs::awaiting_peer_registration(&store, None, Duration::from_secs(5), || {
        attempts.set(attempts.get() + 1);
        Err::<(), _>(failure("fatal: failed to read x/commondir: Success"))
    });
    assert!(believed.is_err());
    assert_eq!(attempts.get(), 1);
}

#[test]
fn a_push_status_line_is_read_by_its_flag_and_documented_kind() {
    let refname = format!("refs/heads/{RECORDS_BRANCH}");
    let line =
        |flag: &str, summary: &str| format!("To /r\n{flag}\tabc:{refname}\t{summary}\nDone\n");
    assert_eq!(
        pushed_ref(&line("*", "[new branch]"), &refname),
        Some(PushedRef::Updated)
    );
    assert_eq!(
        pushed_ref(&line(" ", "a..b"), &refname),
        Some(PushedRef::Updated)
    );
    assert_eq!(
        pushed_ref(&line("!", "[rejected] (fetch first)"), &refname),
        Some(PushedRef::Rejected)
    );
    assert_eq!(
        pushed_ref(&line("!", "[remote rejected] (hook declined)"), &refname),
        Some(PushedRef::RemoteRejected {
            summary: "[remote rejected] (hook declined)".to_owned()
        })
    );
    assert_eq!(
        pushed_ref(
            &line("!", "[remote failure] (remote failed to report status)"),
            &refname
        ),
        Some(PushedRef::Failed)
    );
    assert_eq!(
        pushed_ref(&line("*", "[new branch]"), "refs/heads/other"),
        None
    );
}

in_both_languages!(
    a_file_is_read_and_its_absence_told_from_a_failure,
    a_revision_resolves_or_is_not_found_or_fails,
    a_range_endpoint_resolves_or_is_not_found_or_fails,
    head_is_a_commit_or_unborn_or_a_failure,
    a_config_key_is_set_or_unset_or_unreadable,
    commits_share_a_base_or_none_or_the_question_fails,
    a_records_commit_is_published_where_the_remote_has_not_moved,
    a_records_commit_built_on_a_tip_the_remote_left_is_a_lost_race,
    a_records_commit_the_remote_lost_the_swap_for_is_a_lost_race,
    a_remote_refusing_the_records_branch_says_why,
    a_remote_that_cannot_be_reached_is_an_error,
    an_add_that_leaves_its_own_registration_is_not_repeated,
    the_wait_turns_on_the_registrations_not_on_what_git_said,
);
