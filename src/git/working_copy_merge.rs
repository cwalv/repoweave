//! [`crate::vcs::Vcs::merge_into_working_copy`] for git: each path merged on
//! its clean-filtered content, the result written back through the checkout
//! filters, and an unresolved path left with its three stages in the index.

use super::{git_command, Argv, DERIVED_CONTENT_DRIVER_NAME};
use crate::vcs::WorkingCopyMerge;
use crate::vcs::{CommitId, DerivedContentPolicy, DerivedContentResolution, VcsError};
use std::path::Path;
use std::process::{Output, Stdio};

#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    mode: String,
    blob: String,
}

impl Entry {
    fn regular(&self) -> bool {
        matches!(self.mode.as_str(), "100644" | "100755")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Ours {
    Absent,
    File(String),
    NotAFile,
}

impl Ours {
    fn blob(&self) -> Option<&String> {
        match self {
            Self::File(blob) => Some(blob),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Standing {
    index: Option<Entry>,
    ours: Ours,
}

struct Read {
    path: String,
    base: Option<Entry>,
    theirs: Option<Entry>,
    standing: Standing,
    derived: bool,
}

enum Planned {
    Clean {
        file: Option<Vec<u8>>,
        entry: Option<Entry>,
    },
    Conflicted {
        file: Option<Vec<u8>>,
        ours: Option<Entry>,
    },
}

fn git(args: impl Into<Argv>, repo: &Path, input: Option<&[u8]>) -> Result<Output, VcsError> {
    let args = args.into();
    let mut command = git_command();
    command
        .args(&args)
        .current_dir(repo)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(match input {
            Some(_) => Stdio::piped(),
            None => Stdio::null(),
        });
    let io = |e| VcsError::Io {
        ctx: format!("failed to run git {args:?}"),
        source: e,
    };
    let mut child = command.spawn().map_err(io)?;
    if let Some(input) = input {
        use std::io::Write;
        let mut stdin = child.stdin.take().expect("stdin was piped");
        let (written, output) = std::thread::scope(|s| {
            let writer = s.spawn(move || stdin.write_all(input));
            let output = child.wait_with_output();
            (writer.join().expect("stdin writer panicked"), output)
        });
        let output = output.map_err(io)?;
        written.map_err(io)?;
        return Ok(output);
    }
    child.wait_with_output().map_err(io)
}

fn succeeded(
    args: impl Into<Argv>,
    repo: &Path,
    input: Option<&[u8]>,
) -> Result<Vec<u8>, VcsError> {
    let args = args.into();
    let output = git(args.clone(), repo, input)?;
    if !output.status.success() {
        return Err(VcsError::CommandFailed {
            args: args.into_words(),
            repo: repo.to_path_buf(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    Ok(output.stdout)
}

fn text(bytes: Vec<u8>) -> String {
    String::from_utf8_lossy(&bytes).trim().to_owned()
}

fn tree_entry(repo: &Path, commit: &CommitId, path: &str) -> Result<Option<Entry>, VcsError> {
    let listing = succeeded(
        [
            "--literal-pathspecs",
            "ls-tree",
            "-z",
            commit.as_str(),
            "--",
            path,
        ],
        repo,
        None,
    )?;
    Ok(listing
        .split(|b| *b == 0)
        .filter(|l| !l.is_empty())
        .find_map(|line| {
            let line = String::from_utf8_lossy(line);
            let (meta, named) = line.split_once('\t')?;
            let mut fields = meta.split(' ');
            let mode = fields.next()?.to_owned();
            let kind = fields.next()?;
            let blob = fields.next()?.to_owned();
            (named == path && kind == "blob").then_some(Entry { mode, blob })
        }))
}

fn index_entry(repo: &Path, path: &str) -> Result<Option<Entry>, VcsError> {
    let listing = succeeded(
        ["--literal-pathspecs", "ls-files", "-s", "-z", "--", path],
        repo,
        None,
    )?;
    Ok(listing
        .split(|b| *b == 0)
        .filter(|l| !l.is_empty())
        .find_map(|line| {
            let line = String::from_utf8_lossy(line);
            let (meta, named) = line.split_once('\t')?;
            let mut fields = meta.split(' ');
            let mode = fields.next()?.to_owned();
            let blob = fields.next()?.to_owned();
            (named == path).then_some(Entry { mode, blob })
        }))
}

/// The working copy's content as git would store it, after its clean filters.
fn working_blob(repo: &Path, path: &str) -> Result<Ours, VcsError> {
    match std::fs::symlink_metadata(repo.join(path)) {
        Ok(meta) if meta.is_file() => {}
        Ok(_) => return Ok(Ours::NotAFile),
        Err(_) => return Ok(Ours::Absent),
    }
    let blob = succeeded(
        Argv::from(["hash-object", "-w"])
            .text(format!("--path={path}"))
            .text("--")
            .text(path),
        repo,
        None,
    )?;
    Ok(Ours::File(text(blob)))
}

fn standing(repo: &Path, path: &str) -> Result<Standing, VcsError> {
    Ok(Standing {
        index: index_entry(repo, path)?,
        ours: working_blob(repo, path)?,
    })
}

fn derived_paths(repo: &Path, paths: &[String]) -> Result<Vec<String>, VcsError> {
    let mut args = Argv::from(["--literal-pathspecs", "check-attr", "-z", "merge", "--"]);
    for path in paths {
        args = args.text(path);
    }
    let listing = succeeded(args, repo, None)?;
    let fields: Vec<String> = listing
        .split(|b| *b == 0)
        .map(|f| String::from_utf8_lossy(f).into_owned())
        .collect();
    Ok(fields
        .chunks(3)
        .filter(|c| c.len() == 3 && c[2] == DERIVED_CONTENT_DRIVER_NAME)
        .map(|c| c[0].clone())
        .collect())
}

fn blob_bytes(repo: &Path, blob: &str) -> Result<Vec<u8>, VcsError> {
    succeeded(["cat-file", "blob", blob], repo, None)
}

fn smudged(repo: &Path, path: &str, blob: &str) -> Result<Vec<u8>, VcsError> {
    succeeded(
        Argv::from(["cat-file", "--filters"])
            .text(format!("--path={path}"))
            .text(blob),
        repo,
        None,
    )
}

fn store(repo: &Path, content: &[u8]) -> Result<String, VcsError> {
    Ok(text(succeeded(
        ["hash-object", "-w", "--no-filters", "--stdin"],
        repo,
        Some(content),
    )?))
}

/// `git merge-file` on the three blobs: the merged content and whether it
/// resolved, or `None` where git cannot merge them as text.
fn merge_text(
    repo: &Path,
    scratch: &Path,
    labels: [&str; 3],
    blobs: [&str; 3],
) -> Result<Option<(Vec<u8>, bool)>, VcsError> {
    let mut files = Vec::new();
    for (at, blob) in blobs.iter().enumerate() {
        let file = scratch.join(format!("{at}"));
        std::fs::write(&file, blob_bytes(repo, blob)?).map_err(|e| VcsError::Io {
            ctx: format!("failed to write {}", file.display()),
            source: e,
        })?;
        files.push(file);
    }
    let mut args = Argv::from(["merge-file", "-p"]);
    for label in labels {
        args = args.text("-L").text(label);
    }
    for file in &files {
        args = args.text(file.to_string_lossy());
    }
    let output = git(args, repo, None)?;
    Ok(match output.status.code() {
        Some(0) => Some((output.stdout, true)),
        Some(n) if (1..=127).contains(&n) => Some((output.stdout, false)),
        _ => None,
    })
}

fn plan(repo: &Path, scratch: &Path, read: &Read, labels: [&str; 3]) -> Result<Planned, VcsError> {
    let ours_entry = |mode: &str| {
        read.standing.ours.blob().map(|blob| Entry {
            mode: mode.to_owned(),
            blob: blob.clone(),
        })
    };
    let ours_mode = read
        .standing
        .index
        .as_ref()
        .or(read.base.as_ref())
        .or(read.theirs.as_ref())
        .map(|e| e.mode.clone())
        .unwrap_or_else(|| "100644".to_owned());
    if read.derived {
        return Ok(Planned::Clean {
            file: match &read.theirs {
                Some(theirs) => Some(smudged(repo, &read.path, &theirs.blob)?),
                None => None,
            },
            entry: read.theirs.clone(),
        });
    }
    let (Some(theirs), Some(ours)) = (&read.theirs, read.standing.ours.blob()) else {
        let file = match (&read.theirs, &read.standing.ours) {
            (Some(theirs), Ours::Absent) => Some(smudged(repo, &read.path, &theirs.blob)?),
            _ => None,
        };
        return Ok(Planned::Conflicted {
            file,
            ours: ours_entry(&ours_mode),
        });
    };
    let mergeable = theirs.regular()
        && read.base.as_ref().is_none_or(Entry::regular)
        && matches!(ours_mode.as_str(), "100644" | "100755");
    let empty = store(repo, b"")?;
    let base_blob = read
        .base
        .as_ref()
        .map_or(empty.as_str(), |b| b.blob.as_str());
    let merged = match mergeable {
        true => merge_text(repo, scratch, labels, [ours, base_blob, &theirs.blob])?,
        false => None,
    };
    Ok(match merged {
        Some((content, true)) => {
            let blob = store(repo, &content)?;
            Planned::Clean {
                file: Some(smudged(repo, &read.path, &blob)?),
                entry: Some(theirs.clone()),
            }
        }
        Some((content, false)) => {
            let blob = store(repo, &content)?;
            Planned::Conflicted {
                file: Some(smudged(repo, &read.path, &blob)?),
                ours: ours_entry(&ours_mode),
            }
        }
        None => Planned::Conflicted {
            file: None,
            ours: ours_entry(&ours_mode),
        },
    })
}

fn write_file(repo: &Path, path: &str, content: &[u8], mode: Option<&str>) -> Result<(), VcsError> {
    let file = repo.join(path);
    let io = |e| VcsError::Io {
        ctx: format!("failed to write {}", file.display()),
        source: e,
    };
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent).map_err(io)?;
    }
    let existed = file.exists();
    std::fs::write(&file, content).map_err(io)?;
    #[cfg(unix)]
    if !existed && mode == Some("100755") {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).map_err(io)?;
    }
    #[cfg(not(unix))]
    let _ = (existed, mode);
    Ok(())
}

pub(super) fn merge_into_working_copy(
    repo: &Path,
    base: &CommitId,
    incoming: &CommitId,
    paths: &[String],
    derived: DerivedContentPolicy,
    before_write: &mut dyn FnMut() -> Result<bool, VcsError>,
) -> Result<WorkingCopyMerge, VcsError> {
    if paths.is_empty() {
        return Ok(WorkingCopyMerge::Merged {
            clean: Vec::new(),
            conflicted: Vec::new(),
        });
    }
    if !succeeded(["ls-files", "-u", "-z"], repo, None)?.is_empty() {
        return Ok(WorkingCopyMerge::Declined {
            vcs_said: "the index holds unresolved conflicts".to_owned(),
        });
    }
    let keeps_incoming = derived.resolution() == DerivedContentResolution::KeepIncomingSide;
    let declared = match keeps_incoming {
        true => derived_paths(repo, paths)?,
        false => Vec::new(),
    };
    let mut reads = Vec::new();
    for path in paths {
        reads.push(Read {
            path: path.clone(),
            base: tree_entry(repo, base, path)?,
            theirs: tree_entry(repo, incoming, path)?,
            standing: standing(repo, path)?,
            derived: declared.contains(path),
        });
    }
    let two_edits: Vec<String> = reads
        .iter()
        .filter(|r| {
            let index = r.standing.index.as_ref().map(|e| &e.blob);
            index != r.base.as_ref().map(|e| &e.blob) && index != r.standing.ours.blob()
        })
        .map(|r| r.path.clone())
        .collect();
    if !two_edits.is_empty() {
        return Ok(WorkingCopyMerge::TwoEdits(two_edits));
    }
    let private = text(succeeded(
        Argv::from(["rev-parse", "--path-format=absolute", "--git-path"])
            .text(format!("rwv-working-copy-merge-{}", std::process::id())),
        repo,
        None,
    )?);
    let scratch = Path::new(&private);
    std::fs::create_dir_all(scratch).map_err(|e| VcsError::Io {
        ctx: format!("failed to create {}", scratch.display()),
        source: e,
    })?;
    let labels = ["working copy", base.short(), incoming.short()];
    let planned: Result<Vec<Planned>, VcsError> = reads
        .iter()
        .map(|read| plan(repo, scratch, read, labels))
        .collect();
    let _ = std::fs::remove_dir_all(scratch);
    let planned = planned?;
    for read in &reads {
        if standing(repo, &read.path)? != read.standing {
            return Ok(WorkingCopyMerge::Declined {
                vcs_said: format!("{} changed while it was being merged", read.path),
            });
        }
    }
    if !before_write()? {
        return Ok(WorkingCopyMerge::Stopped);
    }
    let zero = "0".repeat(base.as_str().len());
    let mut index_info = Vec::new();
    let mut line = |mode: &str, blob: &str, stage: u8, path: &str| {
        index_info.extend_from_slice(format!("{mode} {blob} {stage}\t{path}").as_bytes());
        index_info.push(0);
    };
    let (mut clean, mut conflicted) = (Vec::new(), Vec::new());
    for (read, planned) in reads.iter().zip(&planned) {
        let path = read.path.as_str();
        line("0", &zero, 0, path);
        match planned {
            Planned::Clean { file, entry } => {
                match (file, entry) {
                    (Some(content), Some(entry)) => {
                        write_file(repo, path, content, Some(&entry.mode))?;
                        line(&entry.mode, &entry.blob, 0, path);
                    }
                    _ => {
                        let _ = std::fs::remove_file(repo.join(path));
                    }
                }
                clean.push(path.to_owned());
            }
            Planned::Conflicted { file, ours } => {
                if let Some(content) = file {
                    let mode = read.theirs.as_ref().map(|e| e.mode.as_str());
                    write_file(repo, path, content, mode)?;
                }
                if let Some(base) = &read.base {
                    line(&base.mode, &base.blob, 1, path);
                }
                if let Some(ours) = ours {
                    line(&ours.mode, &ours.blob, 2, path);
                }
                if let Some(theirs) = &read.theirs {
                    line(&theirs.mode, &theirs.blob, 3, path);
                }
                conflicted.push(path.to_owned());
            }
        }
    }
    succeeded(
        ["update-index", "-z", "--index-info"],
        repo,
        Some(&index_info),
    )?;
    Ok(WorkingCopyMerge::Merged { clean, conflicted })
}
