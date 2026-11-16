//! Answers git gave in a directory that this process may give again without
//! asking.
//!
//! Only a question whose answer cannot change while the directory's `.git`
//! entry is the same entry belongs here: where the repository and its git
//! directories are, and what a full object id names. Never a ref, `HEAD`, the
//! index, config or the worktree list. An answer is reused only while that
//! entry has the kind, creation time and, for a file, the bytes it had when the
//! answer was read; a git this process starts that can create, move or remove a
//! repository or worktree forgets every answer. A directory with no `.git`
//! entry, or on a filesystem that keeps no creation time, is asked every time.

use std::any::Any;
use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::SystemTime;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Entry {
    Dir { created: SystemTime },
    File { created: SystemTime, bytes: Vec<u8> },
}

impl Entry {
    fn at(dir: &Path) -> Option<Entry> {
        let path = dir.join(".git");
        let meta = std::fs::metadata(&path).ok()?;
        let created = meta.created().ok()?;
        if meta.is_dir() {
            return Some(Entry::Dir { created });
        }
        let bytes = std::fs::read(&path).ok()?;
        (std::fs::metadata(&path).ok()?.created().ok()? == created)
            .then_some(Entry::File { created, bytes })
    }
}

struct Answer {
    through: Entry,
    value: Arc<dyn Any + Send + Sync>,
}

type Answers = HashMap<(PathBuf, String), Answer>;

static ANSWERS: Mutex<Option<Answers>> = Mutex::new(None);

#[cfg(test)]
static FORGOTTEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn answers() -> MutexGuard<'static, Option<Answers>> {
    ANSWERS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// `ask`'s answer to `question` in `dir`, or the one it gave before while
/// `dir`'s `.git` entry has not changed. A failure is never kept.
pub(super) fn recalled<T, E>(
    dir: &Path,
    question: &str,
    ask: impl FnOnce() -> Result<T, E>,
) -> Result<T, E>
where
    T: Clone + Send + Sync + 'static,
{
    recalled_found(dir, question, || ask().map(Some))
        .map(|found| found.expect("an answer asked for is kept or given whole"))
}

/// [`recalled`] for a question git may answer with "nothing there", which is
/// never kept: what is absent now may be fetched or written later.
pub(super) fn recalled_found<T, E>(
    dir: &Path,
    question: &str,
    ask: impl FnOnce() -> Result<Option<T>, E>,
) -> Result<Option<T>, E>
where
    T: Clone + Send + Sync + 'static,
{
    let Some(through) = Entry::at(dir) else {
        return ask();
    };
    let key = (dir.to_path_buf(), question.to_owned());
    if let Some(answer) = answers().get_or_insert_with(HashMap::new).get(&key) {
        if answer.through == through {
            if let Some(value) = answer.value.downcast_ref::<T>() {
                return Ok(Some(value.clone()));
            }
        }
    }
    let Some(value) = ask()? else {
        return Ok(None);
    };
    if Entry::at(dir).as_ref() == Some(&through) {
        answers().get_or_insert_with(HashMap::new).insert(
            key,
            Answer {
                through,
                value: Arc::new(value.clone()),
            },
        );
    }
    Ok(Some(value))
}

const RESHAPING: &[&str] = &["clone", "init", "submodule", "core.worktree", "core.bare"];

const RESHAPING_WORKTREE: &[&str] = &["add", "move", "remove", "prune", "repair"];

/// Forget every answer when the git about to start with `words` can create,
/// move or remove a repository or worktree, or set where one's work tree is. A word that only looks like one
/// of those subcommands forgets too, which costs a question asked again.
pub(super) fn forget_if_reshaping<'a>(words: impl Iterator<Item = &'a OsStr>) {
    let words: Vec<&OsStr> = words.collect();
    let named = |listed: &[&str]| {
        words
            .iter()
            .any(|word| listed.iter().any(|sub| *word == OsStr::new(sub)))
    };
    if named(RESHAPING) || (named(&["worktree"]) && named(RESHAPING_WORKTREE)) {
        *answers() = None;
        #[cfg(test)]
        FORGOTTEN.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn forgotten() -> u64 {
        FORGOTTEN.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Whether `second` was recalled from `first`, unless a git another test
    /// started in between made this process forget every answer.
    fn recalled_unless_forgotten(before: u64, first: &str, second: &str) -> bool {
        first == second || forgotten() != before
    }

    fn counted(dir: &Path, question: &str, asked: &Cell<u32>) -> String {
        recalled(dir, question, || {
            asked.set(asked.get() + 1);
            Ok::<_, ()>(format!("answer {}", asked.get()))
        })
        .unwrap()
    }

    fn created_times_kept(dir: &Path) -> bool {
        std::fs::metadata(dir)
            .and_then(|meta| meta.created())
            .is_ok()
    }

    #[test]
    fn an_answer_is_given_again_while_the_git_entry_stands() {
        let tmp = crate::tempdir().unwrap();
        if !created_times_kept(tmp.path()) {
            eprintln!("this filesystem keeps no creation time: nothing is recalled here");
            return;
        }
        std::fs::create_dir(tmp.path().join(".git")).unwrap();
        let asked = Cell::new(0);
        let before = forgotten();
        let first = counted(tmp.path(), "q", &asked);
        let second = counted(tmp.path(), "q", &asked);
        assert!(
            recalled_unless_forgotten(before, &first, &second),
            "{first} then {second}"
        );
        assert_ne!(counted(tmp.path(), "other", &asked), first);
    }

    #[test]
    fn a_replaced_git_entry_is_asked_again() {
        let tmp = crate::tempdir().unwrap();
        if !created_times_kept(tmp.path()) {
            eprintln!("this filesystem keeps no creation time: nothing is recalled here");
            return;
        }
        let git = tmp.path().join(".git");
        std::fs::write(&git, "gitdir: /one\n").unwrap();
        let asked = Cell::new(0);
        assert_eq!(counted(tmp.path(), "q", &asked), "answer 1");
        std::fs::write(&git, "gitdir: /two\n").unwrap();
        assert_eq!(counted(tmp.path(), "q", &asked), "answer 2");
        std::fs::remove_file(&git).unwrap();
        std::fs::create_dir(&git).unwrap();
        assert_eq!(counted(tmp.path(), "q", &asked), "answer 3");
        std::fs::remove_dir(&git).unwrap();
        assert_eq!(counted(tmp.path(), "q", &asked), "answer 4");
        assert_eq!(counted(tmp.path(), "q", &asked), "answer 5");
    }

    #[test]
    fn a_failure_is_not_kept() {
        let tmp = crate::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join(".git")).unwrap();
        let failed: Result<String, &str> = recalled(tmp.path(), "q", || Err("no"));
        assert_eq!(failed, Err("no"));
        let asked = Cell::new(0);
        assert_eq!(counted(tmp.path(), "q", &asked), "answer 1");
    }
}
