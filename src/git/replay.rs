//! What a rebase made of each commit it replayed, and copies of replayed
//! commits with a file rewritten.

use super::records::{commit_id, failed};
use super::{git_command, GitVcs};
use crate::vcs::{AttachedRef, CommitId, FileChange, Replayed, VcsError};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
struct Authored {
    commit: CommitId,
    author: String,
    subject: String,
}

/// Pair each written commit with the next replayed commit of the same
/// authorship and subject, else of the same authorship alone; a replayed
/// commit passed over was dropped.
fn pair(old: &[Authored], new: &[Authored]) -> Replayed {
    let mut images: Vec<(CommitId, Option<CommitId>)> =
        old.iter().map(|o| (o.commit.clone(), None)).collect();
    let mut cursor = 0;
    for written in new {
        let rest = &old[cursor..];
        let found = rest
            .iter()
            .position(|o| o.author == written.author && o.subject == written.subject)
            .or_else(|| rest.iter().position(|o| o.author == written.author));
        if let Some(offset) = found {
            images[cursor + offset].1 = Some(written.commit.clone());
            cursor += offset + 1;
        }
    }
    Replayed {
        images,
        written: new.iter().map(|n| n.commit.clone()).collect(),
    }
}

impl GitVcs {
    fn authored(repo: &Path, from: &CommitId, tip: &CommitId) -> Result<Vec<Authored>, VcsError> {
        let range = format!("{}..{}", from.as_str(), tip.as_str());
        let args = [
            "-c",
            "log.showSignature=false",
            "log",
            "--no-color",
            "--reverse",
            "--topo-order",
            "--date=raw",
            "--format=%H%x00%an%x00%ae%x00%ad%x00%s%x1e",
            range.as_str(),
        ];
        let out = Self::run(args, repo)?;
        out.split('\u{1e}')
            .map(|record| record.trim_start_matches('\n'))
            .filter(|record| !record.is_empty())
            .map(|record| {
                let fields: Vec<&str> = record.split('\0').collect();
                let [id, name, email, date, subject] = fields[..] else {
                    return Err(failed(&args, repo, format!("unreadable record {record:?}")));
                };
                Ok(Authored {
                    commit: commit_id(id, &args, repo)?,
                    author: format!("{name}\0{email}\0{date}"),
                    subject: subject.to_owned(),
                })
            })
            .collect()
    }

    pub(super) fn pair_replayed(
        repo: &Path,
        from: &CommitId,
        old: &CommitId,
        onto: &CommitId,
        new: &CommitId,
    ) -> Result<Replayed, VcsError> {
        let replayed = Self::authored(repo, from, old)?;
        let written = Self::authored(repo, onto, new)?;
        Ok(pair(&replayed, &written))
    }

    pub(super) fn copy_with_changes(
        repo: &Path,
        commit: &CommitId,
        parent: &CommitId,
        changes: &[FileChange],
    ) -> Result<CommitId, VcsError> {
        let raw = Self::run(["cat-file", "commit", commit.as_str()], repo)?;
        let (headers, message) = raw.split_once("\n\n").unwrap_or((raw.as_str(), ""));
        let author = headers
            .lines()
            .find_map(|line| line.strip_prefix("author "))
            .ok_or_else(|| {
                failed(
                    &["cat-file", "commit"],
                    repo,
                    format!("{commit} names no author"),
                )
            })?;
        let (ident, date) = author
            .rsplit_once("> ")
            .map(|(ident, date)| (format!("{ident}>"), date.to_owned()))
            .ok_or_else(|| {
                failed(
                    &["cat-file", "commit"],
                    repo,
                    format!("{commit} has an unreadable author"),
                )
            })?;
        let (name, email) = ident
            .rsplit_once(" <")
            .map(|(name, email)| (name.to_owned(), email.trim_end_matches('>').to_owned()))
            .unwrap_or_default();
        let index = Self::run_trimmed(
            [
                "rev-parse",
                "--path-format=absolute",
                "--git-path",
                &format!("rwv-replay-index-{}", std::process::id()),
            ],
            repo,
        )?;
        let outcome = Self::copy_through_index(
            repo,
            &index,
            commit,
            parent,
            changes,
            message,
            &[
                ("GIT_AUTHOR_NAME", name.as_str()),
                ("GIT_AUTHOR_EMAIL", email.as_str()),
                ("GIT_AUTHOR_DATE", date.as_str()),
            ],
        );
        let _ = std::fs::remove_file(&index);
        outcome
    }

    #[allow(clippy::too_many_arguments)]
    fn copy_through_index(
        repo: &Path,
        index: &str,
        commit: &CommitId,
        parent: &CommitId,
        changes: &[FileChange],
        message: &str,
        author: &[(&str, &str)],
    ) -> Result<CommitId, VcsError> {
        let in_index = |args: &[&str], input: &[u8], extra: &[(&str, &str)]| {
            let mut env = vec![("GIT_INDEX_FILE", index)];
            env.extend_from_slice(extra);
            Self::run_bytes_in(args, repo, input, &env)
                .map(|out| String::from_utf8_lossy(&out).trim().to_owned())
        };
        in_index(&["read-tree", commit.as_str()], &[], &[])?;
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
                    let zero = "0".repeat(commit.as_str().len());
                    info.extend_from_slice(format!("0 {zero}\t{}", change.path).as_bytes());
                }
            }
            info.push(0);
        }
        if !info.is_empty() {
            in_index(&["update-index", "-z", "--index-info"], &info, &[])?;
        }
        let tree = in_index(&["write-tree"], &[], &[])?;
        let args = [
            "commit-tree",
            tree.as_str(),
            "-p",
            parent.as_str(),
            "-F",
            "-",
        ];
        let mut env: Vec<(&str, &str)> = author.to_vec();
        if !Self::has_identity(repo)? {
            env.extend_from_slice(&super::records::FALLBACK_IDENTITY);
        }
        let written = Self::run_bytes_in(&args, repo, message.as_bytes(), &env)?;
        commit_id(&String::from_utf8_lossy(&written), &args, repo)
    }

    pub(super) fn reset_keeping_to(
        on: &AttachedRef,
        from: &CommitId,
        to: &CommitId,
    ) -> Result<(), VcsError> {
        let repo = on.repo();
        let head = Self::head_commit(repo)?;
        if &head != from {
            return Err(VcsError::StaleRefWitness {
                repo: repo.to_path_buf(),
                expected: from.to_string(),
                observed: head.to_string(),
            });
        }
        let output = git_command()
            .args(["reset", "--keep", "--quiet", to.as_str()])
            .current_dir(repo)
            .output()
            .map_err(|e| VcsError::Io {
                ctx: "failed to spawn git reset --keep".to_owned(),
                source: e,
            })?;
        if !output.status.success() {
            return Err(failed(
                &["reset", "--keep", to.as_str()],
                repo,
                String::from_utf8_lossy(&output.stderr).into_owned(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commit(n: u8) -> CommitId {
        CommitId::parse(&format!("{n:040x}")).unwrap()
    }

    fn authored(n: u8, author: &str, subject: &str) -> Authored {
        Authored {
            commit: commit(n),
            author: author.to_owned(),
            subject: subject.to_owned(),
        }
    }

    #[test]
    fn a_commit_the_replay_dropped_has_no_image_and_the_rest_pair_in_order() {
        let old = [
            authored(1, "a 1", "one"),
            authored(2, "a 2", "two"),
            authored(3, "a 3", "three"),
        ];
        let new = [authored(11, "a 1", "one"), authored(13, "a 3", "three")];
        let replayed = pair(&old, &new);
        assert_eq!(replayed.image(&commit(1)), Some(&commit(11)));
        assert_eq!(replayed.image(&commit(2)), None);
        assert_eq!(replayed.image(&commit(3)), Some(&commit(13)));
    }

    #[test]
    fn commits_of_one_authorship_pair_by_subject_and_a_reworded_one_by_order() {
        let old = [
            authored(1, "same", "one"),
            authored(2, "same", "two"),
            authored(3, "same", "three"),
        ];
        let new = [
            authored(12, "same", "two, reworded"),
            authored(13, "same", "three"),
        ];
        let replayed = pair(&old, &new);
        assert_eq!(replayed.image(&commit(1)), Some(&commit(12)));
        assert_eq!(replayed.image(&commit(3)), Some(&commit(13)));
        let new = [authored(12, "same", "two"), authored(13, "same", "three")];
        let replayed = pair(&old, &new);
        assert_eq!(replayed.image(&commit(1)), None);
        assert_eq!(replayed.image(&commit(2)), Some(&commit(12)));
    }

    #[test]
    fn a_written_commit_of_another_authorship_is_no_commit_s_image() {
        let old = [authored(1, "a", "one")];
        let new = [authored(11, "b", "added"), authored(12, "a", "one")];
        let replayed = pair(&old, &new);
        assert_eq!(replayed.image(&commit(1)), Some(&commit(12)));
        assert_eq!(replayed.original(&commit(11)), None);
        assert_eq!(replayed.written, vec![commit(11), commit(12)]);
    }
}
