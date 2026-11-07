//! The records branch, as git keeps it: an ordinary branch whose commits are
//! written with object plumbing, never checked out, advanced by a
//! compare-and-swap of its ref.

use super::{git_command, remote_tracking, Argv, GitVcs, LOCAL_BRANCH_NAMESPACE};
use crate::vcs::{
    CommitId, CommittedFile, FileChange, NamedBranch, RecordsAdvance, RecordsPublish, RecordsSite,
    RemoteHandle, VcsError,
};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;
use std::process::Stdio;

pub(super) const RECORDS_BRANCH: &str = "rwv-records";

const REMOTE_TRACKING_ROOT: &str = "refs/remotes/";
const FALLBACK_IDENTITY: [(&str, &str); 4] = [
    ("GIT_AUTHOR_NAME", "rwv"),
    ("GIT_AUTHOR_EMAIL", "rwv@localhost"),
    ("GIT_COMMITTER_NAME", "rwv"),
    ("GIT_COMMITTER_EMAIL", "rwv@localhost"),
];

type RemoteSettings = (Option<String>, Option<String>);

fn failed(args: &[&str], repo: &Path, stderr: String) -> VcsError {
    VcsError::CommandFailed {
        args: args.iter().map(|a| (*a).to_owned()).collect(),
        repo: repo.to_path_buf(),
        stderr,
    }
}

fn commit_id(text: &str, args: &[&str], repo: &Path) -> Result<CommitId, VcsError> {
    CommitId::parse(text.trim()).map_err(|e| failed(args, repo, format!("{e:#}")))
}

impl GitVcs {
    fn rev_tip(repo: &Path, refname: &str) -> Result<Option<CommitId>, VcsError> {
        let spec = format!("{refname}^{{commit}}");
        let args = ["rev-parse", "--verify", "--quiet", spec.as_str()];
        match Self::run_trimmed(args, repo) {
            Ok(out) => commit_id(&out, &args, repo).map(Some),
            Err(VcsError::CommandFailed { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub(super) fn records_tip_at(
        repo: &Path,
        site: RecordsSite<'_>,
    ) -> Result<Option<CommitId>, VcsError> {
        let refname = match site {
            RecordsSite::Local => format!("{LOCAL_BRANCH_NAMESPACE}{RECORDS_BRANCH}"),
            RecordsSite::Remote(remote) => remote_tracking(remote, RECORDS_BRANCH),
        };
        Self::rev_tip(repo, &refname)
    }

    pub(super) fn advance_records_ref(
        repo: &Path,
        expected: Option<&CommitId>,
        to: &CommitId,
    ) -> Result<RecordsAdvance, VcsError> {
        let refname = format!("{LOCAL_BRANCH_NAMESPACE}{RECORDS_BRANCH}");
        let old = match expected {
            Some(old) => old.as_str().to_owned(),
            None => "0".repeat(to.as_str().len()),
        };
        crate::test_pause::point("records-write");
        let args = [
            "update-ref",
            "-m",
            "rwv: records",
            refname.as_str(),
            to.as_str(),
            old.as_str(),
        ];
        match Self::run(args, repo) {
            Ok(_) => Ok(RecordsAdvance::Advanced),
            Err(e) => {
                let held = Self::records_tip_at(repo, RecordsSite::Local)?;
                if held.as_ref() == expected {
                    Err(e)
                } else {
                    Ok(RecordsAdvance::Lost)
                }
            }
        }
    }

    pub(super) fn publish_records_commit(
        repo: &Path,
        remote: &RemoteHandle,
        commit: &CommitId,
    ) -> Result<RecordsPublish, VcsError> {
        let refspec = format!(
            "{}:{LOCAL_BRANCH_NAMESPACE}{RECORDS_BRANCH}",
            commit.as_str()
        );
        match Self::run(["push", remote.name(), refspec.as_str()], repo) {
            Ok(_) => Ok(RecordsPublish::Published),
            Err(VcsError::CommandFailed { stderr, .. }) if stderr.contains("[remote rejected]") => {
                Ok(RecordsPublish::Rejected(
                    stderr
                        .lines()
                        .map(|line| line.trim_start_matches("remote: ").trim())
                        .filter(|line| {
                            !line.is_empty() && !line.starts_with("error: failed to push")
                        })
                        .collect::<Vec<_>>()
                        .join("; "),
                ))
            }
            Err(VcsError::CommandFailed { stderr, .. }) if stderr.contains("[rejected]") => {
                Ok(RecordsPublish::RemoteMoved)
            }
            Err(e) => Err(e),
        }
    }

    fn run_bytes(args: &[&str], repo: &Path, input: &[u8]) -> Result<Vec<u8>, VcsError> {
        Self::run_bytes_in(args, repo, input, &[])
    }

    fn run_bytes_in(
        args: &[&str],
        repo: &Path,
        input: &[u8],
        env: &[(&str, &str)],
    ) -> Result<Vec<u8>, VcsError> {
        let mut command = git_command();
        command
            .args(Argv::from(args.iter().copied()))
            .current_dir(repo)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in env {
            command.env(key, value);
        }
        let mut child = command.spawn().map_err(|e| VcsError::Io {
            ctx: format!("failed to spawn git {args:?}"),
            source: e,
        })?;
        let mut stdin = child.stdin.take().expect("stdin was piped");
        let (written, output) = std::thread::scope(|s| {
            let writer = s.spawn(move || stdin.write_all(input));
            let output = child.wait_with_output();
            (writer.join().expect("stdin writer panicked"), output)
        });
        let output = output.map_err(|e| VcsError::Io {
            ctx: format!("failed to read git {args:?}"),
            source: e,
        })?;
        if !output.status.success() {
            return Err(failed(
                args,
                repo,
                String::from_utf8_lossy(&output.stderr).into_owned(),
            ));
        }
        written.map_err(|e| VcsError::Io {
            ctx: format!("failed to feed git {args:?}"),
            source: e,
        })?;
        Ok(output.stdout)
    }

    pub(super) fn files_of_commit(
        repo: &Path,
        commit: &CommitId,
    ) -> Result<Vec<CommittedFile>, VcsError> {
        let listing = Self::run_bytes(
            &["ls-tree", "-r", "-z", "--full-tree", commit.as_str()],
            repo,
            &[],
        )?;
        let mut entries: Vec<(String, String)> = Vec::new();
        for record in listing.split(|b| *b == 0).filter(|r| !r.is_empty()) {
            let record = std::str::from_utf8(record)
                .map_err(|_| failed(&["ls-tree"], repo, "a path is not UTF-8".to_owned()))?;
            let (meta, path) = record
                .split_once('\t')
                .ok_or_else(|| failed(&["ls-tree"], repo, format!("unreadable: {record}")))?;
            let mut fields = meta.split(' ');
            let (kind, oid) = (fields.nth(1), fields.next());
            if let (Some("blob"), Some(oid)) = (kind, oid) {
                entries.push((path.to_owned(), oid.to_owned()));
            }
        }
        if entries.is_empty() {
            return Ok(Vec::new());
        }
        let wanted: String = entries.iter().map(|(_, oid)| format!("{oid}\n")).collect();
        let blobs = Self::run_bytes(&["cat-file", "--batch"], repo, wanted.as_bytes())?;
        let mut files = Vec::with_capacity(entries.len());
        let mut rest = blobs.as_slice();
        for (path, oid) in entries {
            let newline = rest
                .iter()
                .position(|b| *b == b'\n')
                .ok_or_else(|| failed(&["cat-file"], repo, format!("no header for {oid}")))?;
            let header = std::str::from_utf8(&rest[..newline])
                .map_err(|_| failed(&["cat-file"], repo, "a header is not UTF-8".to_owned()))?;
            let size: usize = header
                .split(' ')
                .nth(2)
                .and_then(|n| n.parse().ok())
                .ok_or_else(|| failed(&["cat-file"], repo, format!("unreadable: {header}")))?;
            let body = rest
                .get(newline + 1..newline + 1 + size)
                .ok_or_else(|| failed(&["cat-file"], repo, format!("{oid} is cut short")))?;
            files.push(CommittedFile {
                path,
                content: body.to_vec(),
            });
            rest = &rest[(newline + 1 + size + 1).min(rest.len())..];
        }
        Ok(files)
    }

    pub(super) fn write_tree_commit(
        repo: &Path,
        parents: &[CommitId],
        changes: &[FileChange],
        message: &str,
    ) -> Result<CommitId, VcsError> {
        let index = Self::run_trimmed(
            [
                "rev-parse",
                "--path-format=absolute",
                "--git-path",
                &format!("rwv-records-index-{}", std::process::id()),
            ],
            repo,
        )?;
        let outcome = Self::commit_through_index(repo, &index, parents, changes, message);
        let _ = std::fs::remove_file(&index);
        outcome
    }

    fn commit_through_index(
        repo: &Path,
        index: &str,
        parents: &[CommitId],
        changes: &[FileChange],
        message: &str,
    ) -> Result<CommitId, VcsError> {
        let in_index = |args: &[&str], input: &[u8], extra_env: &[(&str, &str)]| {
            let mut env = vec![("GIT_INDEX_FILE", index)];
            env.extend_from_slice(extra_env);
            Self::run_bytes_in(args, repo, input, &env)
                .map(|out| String::from_utf8_lossy(&out).trim().to_owned())
        };
        if let Some(first) = parents.first() {
            in_index(&["read-tree", first.as_str()], &[], &[])?;
        }
        let mut info: Vec<u8> = Vec::new();
        for change in changes {
            match &change.content {
                Some(content) => {
                    let blob =
                        Self::run_with_stdin(["hash-object", "-w", "--stdin"], repo, content)?;
                    info.extend_from_slice(
                        format!("100644 {}\t{}", blob.trim(), change.path).as_bytes(),
                    );
                }
                None => {
                    let zero = "0".repeat(parents.first().map_or(40, |p| p.as_str().len()));
                    info.extend_from_slice(format!("0 {zero}\t{}", change.path).as_bytes());
                }
            }
            info.push(0);
        }
        if !info.is_empty() {
            in_index(&["update-index", "-z", "--index-info"], &info, &[])?;
        }
        let tree = in_index(&["write-tree"], &[], &[])?;
        let mut args: Vec<String> = vec!["commit-tree".to_owned(), tree];
        for parent in parents {
            args.push("-p".to_owned());
            args.push(parent.as_str().to_owned());
        }
        args.push("-m".to_owned());
        args.push(message.to_owned());
        let words: Vec<&str> = args.iter().map(String::as_str).collect();
        let fallback: &[(&str, &str)] = if Self::has_identity(repo)? {
            &[]
        } else {
            &FALLBACK_IDENTITY
        };
        let commit = in_index(&words, &[], fallback)?;
        commit_id(&commit, &["commit-tree"], repo)
    }

    fn has_identity(repo: &Path) -> Result<bool, VcsError> {
        let output = git_command()
            .args(["var", "GIT_COMMITTER_IDENT"])
            .current_dir(repo)
            .output()
            .map_err(|e| VcsError::Io {
                ctx: "failed to spawn git var".to_owned(),
                source: e,
            })?;
        Ok(output.status.success())
    }

    pub(super) fn merge_base_list(
        repo: &Path,
        a: &CommitId,
        b: &CommitId,
    ) -> Result<Vec<CommitId>, VcsError> {
        let args = ["merge-base", "--all", a.as_str(), b.as_str()];
        match Self::run_trimmed(args, repo) {
            Ok(out) => out
                .lines()
                .map(|line| commit_id(line, &args, repo))
                .collect(),
            Err(VcsError::CommandFailed { stderr, .. }) if stderr.trim().is_empty() => {
                Ok(Vec::new())
            }
            Err(e) => Err(e),
        }
    }

    pub(super) fn last_message_changing(
        repo: &Path,
        tip: &CommitId,
        path: &str,
    ) -> Result<Option<String>, VcsError> {
        let out = Self::run(
            [
                "--literal-pathspecs",
                "log",
                "-1",
                "--format=%B",
                tip.as_str(),
                "--",
                path,
            ],
            repo,
        )?;
        Ok((!out.trim().is_empty()).then_some(out))
    }

    fn branch_remote_settings(repo: &Path) -> Result<BTreeMap<String, RemoteSettings>, VcsError> {
        let listing = match Self::run(
            [
                "config",
                "--get-regexp",
                r"^branch\..*\.(pushremote|remote)$",
            ],
            repo,
        ) {
            Ok(listing) => listing,
            Err(VcsError::CommandFailed { stderr, .. }) if stderr.trim().is_empty() => {
                String::new()
            }
            Err(e) => return Err(e),
        };
        let mut settings: BTreeMap<String, RemoteSettings> = BTreeMap::new();
        for line in listing.lines() {
            let Some((key, value)) = line.split_once(' ') else {
                continue;
            };
            let Some((branch, variable)) = key
                .strip_prefix("branch.")
                .and_then(|rest| rest.rsplit_once('.'))
            else {
                continue;
            };
            let entry = settings.entry(branch.to_owned()).or_default();
            match variable {
                "pushremote" => entry.0 = Some(value.to_owned()),
                "remote" => entry.1 = Some(value.to_owned()),
                _ => {}
            }
        }
        Ok(settings)
    }

    pub(super) fn push_target_remotes(
        repo: &Path,
        url_remote: Option<&RemoteHandle>,
    ) -> Result<Vec<RemoteHandle>, VcsError> {
        let listing = Self::run_trimmed(
            [
                "for-each-ref",
                "--format=%(refname:lstrip=2)",
                LOCAL_BRANCH_NAMESPACE,
            ],
            repo,
        )?;
        let push_default = Self::config_get(repo, "remote.pushDefault")?;
        let settings = Self::branch_remote_settings(repo)?;
        let mut targets: Vec<RemoteHandle> = Vec::new();
        let mut note = |handle: RemoteHandle| {
            if !targets.contains(&handle) {
                targets.push(handle);
            }
        };
        for branch in listing.lines().map(str::trim).filter(|l| !l.is_empty()) {
            let (push_remote, remote) = settings.get(branch).cloned().unwrap_or_default();
            let named = [push_remote, push_default.clone(), remote]
                .into_iter()
                .flatten()
                .find(|name| name != ".");
            match (named, url_remote) {
                (Some(name), _) => note(RemoteHandle::new(name)),
                (None, Some(url)) => note(url.clone()),
                (None, None) => {}
            }
        }
        Ok(targets)
    }

    pub(super) fn every_branch(repo: &Path) -> Result<Vec<NamedBranch>, VcsError> {
        let mut by_name: BTreeMap<String, NamedBranch> = BTreeMap::new();
        let mut note = |name: &str, oid: &str, local: bool| -> Result<(), VcsError> {
            if name == "HEAD" {
                return Ok(());
            }
            let tip = commit_id(oid, &["for-each-ref"], repo)?;
            match by_name.get_mut(name) {
                Some(held) if held.local || !local => {}
                Some(held) => {
                    held.tip = tip;
                    held.local = true;
                }
                None => {
                    by_name.insert(
                        name.to_owned(),
                        NamedBranch {
                            name: name.to_owned(),
                            tip,
                            local,
                        },
                    );
                }
            }
            Ok(())
        };
        let listings = Self::remote_names(repo)?
            .into_iter()
            .map(|remote| {
                let skipped = 2 + remote.split('/').count();
                (format!("{REMOTE_TRACKING_ROOT}{remote}/"), skipped, false)
            })
            .chain([(LOCAL_BRANCH_NAMESPACE.to_owned(), 2, true)]);
        for (prefix, skipped, local) in listings {
            let format = format!("--format=%(objectname) %(refname:lstrip={skipped})");
            let out = Self::run_trimmed(["for-each-ref", format.as_str(), prefix.as_str()], repo)?;
            for line in out.lines().map(str::trim).filter(|l| !l.is_empty()) {
                let (oid, name) = line.split_once(' ').ok_or_else(|| {
                    failed(&["for-each-ref"], repo, format!("unreadable: {line}"))
                })?;
                note(name, oid, local)?;
            }
        }
        Ok(by_name.into_values().collect())
    }
}
