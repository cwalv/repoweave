//! What a rebase made of each commit it replayed, and copies of replayed
//! commits with a file rewritten.

use super::records::{commit_id, failed};
use super::{git_command, GitVcs, Scratch};
use crate::vcs::{AttachedRef, CommitId, FileChange, Replayed, VcsError};
use std::path::{Path, PathBuf};

/// The name, under the checkout's own git dir, of the record a replay of
/// `old` onto `onto` keeps.
const RECORD_PREFIX: &str = "rwv-replayed-";

fn record_name(old: &CommitId, onto: &CommitId) -> String {
    format!("{RECORD_PREFIX}{}-{}", old.as_str(), onto.as_str())
}

/// The name of the record of what rwv made of that replay by rewriting it,
/// kept beside the replay's own so that either tip is described.
fn rewritten_record_name(old: &CommitId, onto: &CommitId) -> String {
    format!("{}.rewritten", record_name(old, onto))
}

/// The name of the copy of the todo git built for the same replay.
fn todo_name(old: &CommitId, onto: &CommitId) -> String {
    format!("{}.todo", record_name(old, onto))
}

/// The commits a todo git built schedules, each as the id it names them by,
/// abbreviated where git abbreviated it.
fn scheduled(todo: &str) -> Vec<String> {
    todo.lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            match words.next()? {
                "pick" | "p" | "reword" | "r" | "edit" | "e" | "squash" | "s" | "fixup" | "f" => {
                    words.next().map(str::to_owned)
                }
                _ => None,
            }
        })
        .collect()
}

/// What a record's pairs, each replayed commit with the commit git noted
/// for it, say of `olds` replayed onto `onto` as `written`, given the
/// commits git's todo scheduled; `None` where a pair names a commit of
/// neither, or where a scheduled commit has no pair: a replay whose
/// recording did not run for every commit describes nothing.
///
/// Git notes a commit it dropped as left empty against the commit HEAD was
/// on, which is the image of the commit before it or `onto`: a commit noted
/// against `onto` or against an earlier commit's image was dropped. A commit
/// of `olds` git's todo does not schedule is one git left out as already
/// upstream: dropped too.
fn read_pairs(
    pairs: &[(CommitId, CommitId)],
    scheduled: &[String],
    olds: &[CommitId],
    onto: &CommitId,
    written: &[CommitId],
) -> Option<Replayed> {
    if pairs.iter().any(|(old, _)| !olds.contains(old)) {
        return None;
    }
    let mut claimed: Vec<&CommitId> = Vec::new();
    let mut images = Vec::with_capacity(olds.len());
    for old in olds {
        let noted = pairs.iter().find(|(o, _)| o == old).map(|(_, new)| new);
        let image = match noted {
            Some(new) if new == onto || claimed.contains(&new) => None,
            Some(new) if written.contains(new) => {
                claimed.push(new);
                Some(new.clone())
            }
            None if !scheduled
                .iter()
                .any(|id| !id.is_empty() && old.as_str().starts_with(id.as_str())) =>
            {
                None
            }
            Some(_) | None => return None,
        };
        images.push((old.clone(), image));
    }
    Some(Replayed {
        images,
        written: written.to_vec(),
    })
}

impl GitVcs {
    fn todo_path(repo: &Path, old: &CommitId, onto: &CommitId) -> Result<PathBuf, VcsError> {
        Self::run_trimmed(
            [
                "rev-parse",
                "--path-format=absolute",
                "--git-path",
                &todo_name(old, onto),
            ],
            repo,
        )
        .map(PathBuf::from)
    }

    fn rewritten_record_path(
        repo: &Path,
        old: &CommitId,
        onto: &CommitId,
    ) -> Result<PathBuf, VcsError> {
        Self::run_trimmed(
            [
                "rev-parse",
                "--path-format=absolute",
                "--git-path",
                &rewritten_record_name(old, onto),
            ],
            repo,
        )
        .map(PathBuf::from)
    }

    fn record_path(repo: &Path, old: &CommitId, onto: &CommitId) -> Result<PathBuf, VcsError> {
        Self::run_trimmed(
            [
                "rev-parse",
                "--path-format=absolute",
                "--git-path",
                &record_name(old, onto),
            ],
            repo,
        )
        .map(PathBuf::from)
    }

    /// Start the record of a replay of `old` onto `onto`, empty, and return
    /// the command the replay runs after each commit to keep it, a copy of
    /// git's own list of what it rewrote, which git keeps only while the
    /// replay is in progress; and the sequence editor that keeps the todo git
    /// hands it, unchanged.
    pub(super) fn start_replay_record(
        repo: &Path,
        old: &CommitId,
        onto: &CommitId,
    ) -> Result<(String, String), VcsError> {
        let path = Self::record_path(repo, old, onto)?;
        std::fs::write(&path, b"").map_err(|source| VcsError::Io {
            ctx: format!("failed to start {}", path.display()),
            source,
        })?;
        let todo = Self::todo_path(repo, old, onto)?;
        std::fs::write(&todo, b"").map_err(|source| VcsError::Io {
            ctx: format!("failed to start {}", todo.display()),
            source,
        })?;
        Ok((
            format!(
                "cp -f \"$(git rev-parse --git-path rebase-merge/rewritten-list)\" \
                 \"$(git rev-parse --git-path {})\" 2>/dev/null || :",
                record_name(old, onto)
            ),
            format!(
                "f() {{ cp -f \"$1\" \"$(git rev-parse --git-path {})\"; }}; f",
                todo_name(old, onto)
            ),
        ))
    }

    fn commits_between(
        repo: &Path,
        from: &CommitId,
        to: &CommitId,
    ) -> Result<Vec<CommitId>, VcsError> {
        let range = format!("{}..{}", from.as_str(), to.as_str());
        let args = ["rev-list", "--reverse", "--topo-order", range.as_str()];
        Self::run(args, repo)?
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| commit_id(line, &args, repo))
            .collect()
    }

    pub(super) fn recorded_replay(
        repo: &Path,
        from: &CommitId,
        old: &CommitId,
        onto: &CommitId,
        new: &CommitId,
    ) -> Result<Option<Replayed>, VcsError> {
        let path = Self::record_path(repo, old, onto)?;
        if let Some(replayed) = Self::read_replay(repo, from, old, onto, new, &path)? {
            return Ok(Some(replayed));
        }
        let rewritten = Self::rewritten_record_path(repo, old, onto)?;
        if rewritten.exists() {
            return Self::read_replay(repo, from, old, onto, new, &rewritten);
        }
        Ok(None)
    }

    fn read_replay(
        repo: &Path,
        from: &CommitId,
        old: &CommitId,
        onto: &CommitId,
        new: &CommitId,
        path: &Path,
    ) -> Result<Option<Replayed>, VcsError> {
        let olds = Self::commits_between(repo, from, old)?;
        let written = Self::commits_between(repo, onto, new)?;
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(olds.is_empty().then(|| Replayed {
                    images: Vec::new(),
                    written,
                }))
            }
            Err(source) => {
                return Err(VcsError::Io {
                    ctx: format!("failed to read {}", path.display()),
                    source,
                })
            }
        };
        let mut pairs = Vec::new();
        for line in text.lines().filter(|line| !line.trim().is_empty()) {
            let mut ids = line.split_whitespace().map(CommitId::parse);
            match (ids.next(), ids.next()) {
                (Some(Ok(was)), Some(Ok(now))) => pairs.push((was, now)),
                _ => return Ok(None),
            }
        }
        let todo = match std::fs::read_to_string(Self::todo_path(repo, old, onto)?) {
            Ok(todo) if !todo.trim().is_empty() => scheduled(&todo),
            Ok(_) => {
                return Ok(olds.is_empty().then(|| Replayed {
                    images: Vec::new(),
                    written,
                }))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(olds.is_empty().then(|| Replayed {
                    images: Vec::new(),
                    written,
                }))
            }
            Err(source) => {
                return Err(VcsError::Io {
                    ctx: format!("failed to read the todo of {}", path.display()),
                    source,
                })
            }
        };
        Ok(read_pairs(&pairs, &todo, &olds, onto, &written))
    }

    /// Whether every commit from `onto` to `head` is the replay of one of
    /// `from..old`, as the record of a rebase still in progress notes it:
    /// the commits it has yet to replay are no evidence against it.
    pub(super) fn replaying_so_far(
        repo: &Path,
        from: &CommitId,
        old: &CommitId,
        onto: &CommitId,
        head: &CommitId,
    ) -> Result<bool, VcsError> {
        let olds = Self::commits_between(repo, from, old)?;
        let written = Self::commits_between(repo, onto, head)?;
        let path = Self::record_path(repo, old, onto)?;
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(written.is_empty()),
            Err(source) => {
                return Err(VcsError::Io {
                    ctx: format!("failed to read {}", path.display()),
                    source,
                })
            }
        };
        let mut images = Vec::new();
        for line in text.lines().filter(|line| !line.trim().is_empty()) {
            let mut ids = line.split_whitespace().map(CommitId::parse);
            match (ids.next(), ids.next()) {
                (Some(Ok(was)), Some(Ok(now))) if olds.contains(&was) => images.push(now),
                _ => return Ok(false),
            }
        }
        Ok(written.iter().all(|commit| images.contains(commit)))
    }

    pub(super) fn remove_replay_records(repo: &Path) -> Result<(), VcsError> {
        let held = Self::run_trimmed(
            [
                "rev-parse",
                "--path-format=absolute",
                "--git-path",
                RECORD_PREFIX,
            ],
            repo,
        )
        .map(PathBuf::from)?;
        let Some(dir) = held.parent() else {
            return Ok(());
        };
        let io = |source| VcsError::Io {
            ctx: format!("failed to clear the replay records in {}", dir.display()),
            source,
        };
        for entry in std::fs::read_dir(dir).map_err(io)? {
            let entry = entry.map_err(io)?;
            if entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with(RECORD_PREFIX))
            {
                std::fs::remove_file(entry.path()).map_err(io)?;
            }
        }
        Ok(())
    }

    pub(super) fn write_replay_record(
        repo: &Path,
        old: &CommitId,
        onto: &CommitId,
        replayed: &Replayed,
    ) -> Result<(), VcsError> {
        let mut text = String::new();
        let mut last = onto.clone();
        for (was, image) in &replayed.images {
            if let Some(image) = image {
                last = image.clone();
            }
            text.push_str(&format!("{} {}\n", was.as_str(), last.as_str()));
        }
        let path = Self::rewritten_record_path(repo, old, onto)?;
        std::fs::write(&path, text.as_bytes()).map_err(|source| VcsError::Io {
            ctx: format!("failed to write {}", path.display()),
            source,
        })
    }

    pub(super) fn copy_with_changes(
        repo: &Path,
        commit: &CommitId,
        parent: &CommitId,
        changes: &[FileChange],
        annotations: Option<(&str, &[crate::vcs::Annotation])>,
    ) -> Result<CommitId, VcsError> {
        let raw = Self::run_bytes_in(&["cat-file", "commit", commit.as_str()], repo, &[], &[])?;
        let split = raw
            .windows(2)
            .position(|pair| pair == b"\n\n")
            .map_or((raw.len(), raw.len()), |at| (at, at + 2));
        let headers = String::from_utf8_lossy(&raw[..split.0]).into_owned();
        let message = &raw[split.1..];
        let encoding = headers
            .lines()
            .find_map(|line| line.strip_prefix("encoding "))
            .unwrap_or("UTF-8")
            .to_owned();
        let replaced;
        let message = match annotations {
            Some((key, annotations)) => {
                replaced = super::with_trailers_replaced(message, key, annotations);
                replaced.as_slice()
            }
            None => message,
        };
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
        let scratch = Scratch::take(repo)?;
        Self::copy_through_index(
            repo,
            &scratch.path("replay-index"),
            commit,
            parent,
            changes,
            message,
            &encoding,
            &[
                ("GIT_AUTHOR_NAME", name.as_str()),
                ("GIT_AUTHOR_EMAIL", email.as_str()),
                ("GIT_AUTHOR_DATE", date.as_str()),
            ],
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn copy_through_index(
        repo: &Path,
        index: &str,
        commit: &CommitId,
        parent: &CommitId,
        changes: &[FileChange],
        message: &[u8],
        encoding: &str,
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
        let setting = format!("i18n.commitEncoding={encoding}");
        let mut args = vec![
            "-c",
            setting.as_str(),
            "commit-tree",
            tree.as_str(),
            "-p",
            parent.as_str(),
            "-F",
            "-",
        ];
        if Self::signs_commits(repo)? {
            args.push("-S");
        }
        let mut env: Vec<(&str, &str)> = author.to_vec();
        if !Self::has_identity(repo)? {
            env.extend_from_slice(&super::records::FALLBACK_IDENTITY);
        }
        let written = Self::run_bytes_in(&args, repo, message, &env)?;
        commit_id(&String::from_utf8_lossy(&written), &args, repo)
    }

    pub(super) fn checkout_rewritten(
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
        let branch = on.to_string();
        let args = ["checkout", "-q", "-B", branch.as_str(), to.as_str(), "--"];
        let output = git_command()
            .args(args)
            .current_dir(repo)
            .output()
            .map_err(|e| VcsError::Io {
                ctx: "failed to spawn git checkout -B".to_owned(),
                source: e,
            })?;
        if !output.status.success() {
            return Err(failed(
                &args,
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

    fn ids(olds: &[CommitId]) -> Vec<String> {
        olds.iter().map(|c| c.as_str().to_owned()).collect()
    }

    fn pairs(of: &[(u8, u8)]) -> Vec<(CommitId, CommitId)> {
        of.iter().map(|(o, n)| (commit(*o), commit(*n))).collect()
    }

    #[test]
    fn a_commit_noted_against_onto_or_an_earlier_image_was_dropped() {
        let olds = [commit(1), commit(2), commit(3), commit(4)];
        let written = [commit(12), commit(14)];
        let replayed = read_pairs(
            &pairs(&[(1, 9), (2, 12), (3, 12), (4, 14)]),
            &ids(&olds),
            &olds,
            &commit(9),
            &written,
        )
        .unwrap();
        assert_eq!(replayed.image(&commit(1)), None);
        assert_eq!(replayed.image(&commit(2)), Some(&commit(12)));
        assert_eq!(replayed.image(&commit(3)), None);
        assert_eq!(replayed.image(&commit(4)), Some(&commit(14)));
    }

    #[test]
    fn a_record_missing_a_replayed_commit_describes_nothing() {
        let olds = [commit(1), commit(2)];
        let written = [commit(11)];
        assert_eq!(
            read_pairs(&pairs(&[(2, 11)]), &ids(&olds), &olds, &commit(9), &written),
            None
        );
        assert_eq!(
            read_pairs(&[], &ids(&olds), &olds, &commit(9), &written),
            None
        );
    }

    #[test]
    fn a_commit_written_that_the_record_does_not_note_was_added() {
        let olds = [commit(1), commit(2)];
        let written = [commit(11), commit(20)];
        let replayed = read_pairs(
            &pairs(&[(1, 9), (2, 11)]),
            &ids(&olds),
            &olds,
            &commit(9),
            &written,
        )
        .unwrap();
        assert_eq!(replayed.image(&commit(1)), None);
        assert_eq!(replayed.image(&commit(2)), Some(&commit(11)));
        assert_eq!(replayed.original(&commit(20)), None);
    }

    #[test]
    fn a_commit_git_s_todo_does_not_schedule_was_dropped_as_already_upstream() {
        let olds = [commit(1), commit(2)];
        let written = [commit(11)];
        let replayed = read_pairs(
            &pairs(&[(2, 11)]),
            &ids(&olds[1..]),
            &olds,
            &commit(9),
            &written,
        )
        .unwrap();
        assert_eq!(replayed.image(&commit(1)), None);
        assert_eq!(replayed.image(&commit(2)), Some(&commit(11)));
    }

    #[test]
    fn the_scheduled_commits_are_the_picks_of_git_s_todo() {
        assert_eq!(
            scheduled("pick 0731fb9 # c2\nexec cp a b\np abc1234 x\n\n# Rebase da9613e..0731fb9\n"),
            vec!["0731fb9".to_owned(), "abc1234".to_owned()]
        );
    }

    #[test]
    fn a_record_naming_a_commit_the_branch_does_not_hold_describes_nothing() {
        let olds = [commit(1)];
        assert_eq!(
            read_pairs(
                &pairs(&[(1, 11)]),
                &ids(&olds),
                &olds,
                &commit(9),
                &[commit(12)]
            ),
            None
        );
        assert_eq!(
            read_pairs(
                &pairs(&[(5, 11)]),
                &ids(&olds),
                &olds,
                &commit(9),
                &[commit(11)]
            ),
            None
        );
    }

    fn replaced(message: &str, values: &[&str]) -> String {
        let annotations: Vec<crate::vcs::Annotation> = values
            .iter()
            .map(|value| crate::vcs::Annotation {
                key: "Rwv-Repin",
                value: (*value).to_owned(),
            })
            .collect();
        String::from_utf8(super::super::with_trailers_replaced(
            message.as_bytes(),
            "Rwv-Repin",
            &annotations,
        ))
        .unwrap()
    }

    #[test]
    fn a_replaced_record_closes_the_trailer_block_it_was_in() {
        assert_eq!(
            replaced(
                "lock\n\nbody\n\nRwv-Repin: old\nCo-Authored-By: x\n",
                &["new"]
            ),
            "lock\n\nbody\n\nCo-Authored-By: x\nRwv-Repin: new\n"
        );
    }

    #[test]
    fn a_record_opens_a_trailer_block_where_the_message_has_none() {
        assert_eq!(
            replaced("lock\n\nbody\n", &["new"]),
            "lock\n\nbody\n\nRwv-Repin: new\n"
        );
    }

    #[test]
    fn a_prose_line_starting_with_the_key_outside_the_trailer_block_is_kept() {
        assert_eq!(
            replaced(
                "lock\n\nRwv-Repin: is how this is written up\nmore prose\n\nRwv-Repin: old\n",
                &["new"]
            ),
            "lock\n\nRwv-Repin: is how this is written up\nmore prose\n\nRwv-Repin: new\n"
        );
    }

    #[test]
    fn a_record_under_the_key_in_another_case_is_replaced_as_git_reads_it() {
        assert_eq!(
            replaced("lock\n\nrwv-repin: old\n", &["new"]),
            "lock\n\nRwv-Repin: new\n"
        );
    }

    #[test]
    fn a_record_dropped_with_nothing_in_its_place_takes_its_emptied_block_with_it() {
        assert_eq!(
            replaced("lock\n\nbody\n\nRwv-Repin: old\n", &[]),
            "lock\n\nbody\n"
        );
    }

    #[test]
    fn crlf_lines_keep_their_endings_and_the_new_record_takes_them() {
        assert_eq!(
            replaced("lock\r\n\r\nbody\r\n", &["new"]),
            "lock\r\n\r\nbody\r\n\r\nRwv-Repin: new\r\n"
        );
    }
}
