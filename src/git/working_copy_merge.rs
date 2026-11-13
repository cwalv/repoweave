//! [`crate::vcs::Vcs::merge_into_working_copy`] for git: each path merged on
//! its clean-filtered content, the result written back through the checkout
//! filters, and an unresolved path left with its three stages in the index.
//!
//! Nothing is written through a symbolic link, at a path or at any of its
//! leading directories. Before the first write a journal of what each path
//! held is kept in the checkout's git directory; a failed write puts every
//! path back from it, and a journal that outlives its process names the
//! checkout as interrupted until [`put_back`] runs.

use super::{
    git_command, git_reads_attributes_from_a_commit, refresh_paths_if_left, Argv,
    DERIVED_CONTENT_DRIVER_NAME,
};
use crate::vcs::{
    BeforeWrite, CommitId, DerivedContentPolicy, DerivedContentResolution, EditResolution,
    InterruptedMerge, MergeDeclined, RefreshScope, RefreshWrite, VcsError, WorkingCopyMerge,
    WorkingCopyMergeRequest,
};
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};

const GITLINK: &str = "160000";
const LINK: &str = "120000";
const EXECUTABLE: &str = "100755";
const REGULAR: &str = "100644";
const JOURNAL: &str = "rwv-drift-merge";
const JOURNAL_FORMAT: &str = "rwv-drift-merge 1";

#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    mode: String,
    blob: String,
}

impl Entry {
    fn regular(&self) -> bool {
        matches!(self.mode.as_str(), REGULAR | EXECUTABLE)
    }
}

/// What stands on disk at a path, read without following a symbolic link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OnDisk {
    Absent,
    File {
        executable: bool,
    },
    Link,
    Other,
    /// Something other than a directory stands where a leading directory of
    /// the path would.
    Behind,
}

fn on_disk(repo: &Path, path: &str) -> OnDisk {
    let mut at = repo.to_path_buf();
    let components: Vec<&str> = path.split('/').collect();
    for (n, component) in components.iter().enumerate() {
        at.push(component);
        let Ok(meta) = std::fs::symlink_metadata(&at) else {
            return OnDisk::Absent;
        };
        if n + 1 < components.len() {
            if !meta.is_dir() {
                return OnDisk::Behind;
            }
            continue;
        }
        if meta.file_type().is_symlink() {
            return OnDisk::Link;
        }
        if meta.is_file() {
            return OnDisk::File {
                executable: executable(&meta),
            };
        }
        return OnDisk::Other;
    }
    OnDisk::Other
}

#[cfg(unix)]
fn executable(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o100 != 0
}

#[cfg(not(unix))]
fn executable(_meta: &std::fs::Metadata) -> bool {
    false
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Ours {
    Absent,
    File { blob: String, executable: bool },
    Link(String),
    NotAFile,
    Behind,
}

impl Ours {
    fn blob(&self) -> Option<&String> {
        match self {
            Self::File { blob, .. } | Self::Link(blob) => Some(blob),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Standing {
    index: Option<Entry>,
    ours: Ours,
}

/// The `merge` attribute a path carries at the incoming tip.
#[derive(Debug, Clone, PartialEq, Eq)]
enum MergeAttribute {
    Text,
    Union,
    Unmergeable,
    Derived,
    Driver(String),
}

struct Read {
    path: String,
    base: Option<Entry>,
    theirs: Option<Entry>,
    standing: Standing,
    merge: MergeAttribute,
}

enum Planned {
    Clean {
        write: Write,
        entry: Option<Entry>,
        took_incoming: bool,
    },
    Conflicted {
        write: Write,
        ours: Option<Entry>,
    },
}

enum Write {
    Keep,
    Remove,
    Content {
        bytes: Vec<u8>,
        executable: bool,
    },
    /// The link standing at the path removed, then the content written.
    OverLink {
        bytes: Vec<u8>,
        executable: bool,
    },
}

fn git(
    args: impl Into<Argv>,
    repo: &Path,
    input: Option<&[u8]>,
    index: Option<&Path>,
) -> Result<Output, VcsError> {
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
    if let Some(index) = index {
        command.env("GIT_INDEX_FILE", &index.to_string_lossy());
    }
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

fn succeeded_in(
    args: impl Into<Argv>,
    repo: &Path,
    input: Option<&[u8]>,
    index: Option<&Path>,
) -> Result<Vec<u8>, VcsError> {
    let args = args.into();
    let output = git(args.clone(), repo, input, index)?;
    if !output.status.success() {
        return Err(VcsError::CommandFailed {
            args: args.into_words(),
            repo: repo.to_path_buf(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    Ok(output.stdout)
}

fn succeeded(
    args: impl Into<Argv>,
    repo: &Path,
    input: Option<&[u8]>,
) -> Result<Vec<u8>, VcsError> {
    succeeded_in(args, repo, input, None)
}

fn text(bytes: Vec<u8>) -> String {
    String::from_utf8_lossy(&bytes).trim().to_owned()
}

fn io_error(ctx: String, said: &str) -> VcsError {
    VcsError::Io {
        ctx,
        source: std::io::Error::other(said.to_owned()),
    }
}

fn fields(listing: &[u8]) -> impl Iterator<Item = String> + '_ {
    listing
        .split(|b| *b == 0)
        .filter(|l| !l.is_empty())
        .map(|l| String::from_utf8_lossy(l).into_owned())
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
    let found = fields(&listing).find_map(|line| {
        let (meta, named) = line.split_once('\t')?;
        let mut fields = meta.split(' ');
        let mode = fields.next()?.to_owned();
        let kind = fields.next()?;
        let blob = fields.next()?.to_owned();
        (named == path && matches!(kind, "blob" | "commit")).then_some(Entry { mode, blob })
    });
    Ok(found)
}

/// Each stage `path` holds in the index, as `(mode, object, stage)`.
fn index_stages(repo: &Path, path: &str) -> Result<Vec<(String, String, String)>, VcsError> {
    let listing = succeeded(
        ["--literal-pathspecs", "ls-files", "-s", "-z", "--", path],
        repo,
        None,
    )?;
    Ok(fields(&listing)
        .filter_map(|line| {
            let (meta, named) = line.split_once('\t')?;
            let mut fields = meta.split(' ');
            let mode = fields.next()?.to_owned();
            let blob = fields.next()?.to_owned();
            let stage = fields.next()?.to_owned();
            (named == path).then_some((mode, blob, stage))
        })
        .collect())
}

/// The paths of `paths` whose index entry only records an intent to add: the
/// working copy alone holds what it names, whatever `HEAD` holds there.
fn intended(repo: &Path, paths: &[String]) -> Result<Vec<String>, VcsError> {
    let mut args = Argv::from([
        "--literal-pathspecs",
        "diff",
        "--name-only",
        "--no-renames",
        "--diff-filter=A",
        "--ignore-submodules=all",
        "-z",
        "--",
    ]);
    for path in paths {
        args = args.text(path);
    }
    Ok(fields(&succeeded(args, repo, None)?).collect())
}

/// The paths of `paths` git does not compare with what the index records:
/// ignored files the index lacks, and entries marked assume-unchanged or
/// skip-worktree.
pub(super) fn uncompared(
    repo: &Path,
    paths: &[String],
) -> Result<(Vec<String>, Vec<String>), VcsError> {
    let mut flagged = Argv::from(["--literal-pathspecs", "ls-files", "-v", "-z", "--"]);
    let mut ignored = Argv::from([
        "--literal-pathspecs",
        "ls-files",
        "-z",
        "--others",
        "--ignored",
        "--exclude-standard",
        "--",
    ]);
    for path in paths {
        flagged = flagged.text(path);
        ignored = ignored.text(path);
    }
    let marked: Vec<String> = fields(&succeeded(flagged, repo, None)?)
        .filter_map(|record| {
            let (tag, path) = record.split_once(' ')?;
            (tag.chars().any(|c| c.is_ascii_lowercase()) || tag == "S").then(|| path.to_owned())
        })
        .filter(|path| paths.contains(path))
        .collect();
    let ignored: Vec<String> = fields(&succeeded(ignored, repo, None)?)
        .filter(|path| paths.contains(path))
        .collect();
    Ok((ignored, marked))
}

fn index_entry(repo: &Path, path: &str) -> Result<Option<Entry>, VcsError> {
    Ok(index_stages(repo, path)?
        .into_iter()
        .find(|(_, _, stage)| stage == "0")
        .map(|(mode, blob, _)| Entry { mode, blob }))
}

fn link_target(repo: &Path, path: &str) -> Result<Vec<u8>, VcsError> {
    let file = repo.join(path);
    let target = std::fs::read_link(&file).map_err(|e| VcsError::Io {
        ctx: format!("failed to read the link {}", file.display()),
        source: e,
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        Ok(target.as_os_str().as_bytes().to_vec())
    }
    #[cfg(not(unix))]
    Ok(crate::path_spelling::wire_path(&target).into_bytes())
}

/// The working copy's content as git would store it, after its clean filters.
fn working_copy(repo: &Path, path: &str) -> Result<Ours, VcsError> {
    match on_disk(repo, path) {
        OnDisk::Absent => Ok(Ours::Absent),
        OnDisk::Other => Ok(Ours::NotAFile),
        OnDisk::Behind => Ok(Ours::Behind),
        OnDisk::Link => Ok(Ours::Link(store(repo, &link_target(repo, path)?)?)),
        OnDisk::File { executable } => {
            let blob = succeeded(
                Argv::from(["hash-object", "-w"])
                    .text(format!("--path={path}"))
                    .text("--")
                    .text(path),
                repo,
                None,
            )?;
            Ok(Ours::File {
                blob: text(blob),
                executable,
            })
        }
    }
}

fn standing(repo: &Path, path: &str, intended: bool) -> Result<Standing, VcsError> {
    Ok(Standing {
        index: match intended {
            true => None,
            false => index_entry(repo, path)?,
        },
        ours: working_copy(repo, path)?,
    })
}

/// A scratch index holding `commit`'s tree, removed when dropped.
struct ScratchIndex(PathBuf);

impl Drop for ScratchIndex {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn git_path(repo: &Path, name: &str) -> Result<PathBuf, VcsError> {
    Ok(PathBuf::from(text(succeeded(
        Argv::from(["rev-parse", "--path-format=absolute", "--git-path"]).text(name),
        repo,
        None,
    )?)))
}

/// Each path's `merge` attribute as `incoming`'s tree declares it.
fn merge_attributes(
    repo: &Path,
    incoming: &CommitId,
    paths: &[String],
    derived: DerivedContentPolicy,
) -> Result<Vec<MergeAttribute>, VcsError> {
    let scratch = ScratchIndex(git_path(
        repo,
        &format!("rwv-working-copy-merge-attributes-{}", std::process::id()),
    )?);
    succeeded_in(
        ["read-tree", incoming.as_str()],
        repo,
        None,
        Some(&scratch.0),
    )?;
    let mut args = Argv::from([
        "--literal-pathspecs",
        "check-attr",
        "--cached",
        "-z",
        "merge",
        "--",
    ]);
    for path in paths {
        args = args.text(path);
    }
    let listing = succeeded_in(args, repo, None, Some(&scratch.0))?;
    let found: Vec<String> = listing
        .split(|b| *b == 0)
        .map(|f| String::from_utf8_lossy(f).into_owned())
        .collect();
    let keeps_incoming = derived.resolution() == DerivedContentResolution::KeepIncomingSide;
    let mut attributes = Vec::new();
    for path in paths {
        let value = found
            .chunks(3)
            .find(|c| c.len() == 3 && &c[0] == path)
            .map(|c| c[2].as_str())
            .unwrap_or("unspecified");
        attributes.push(match value {
            "unspecified" | "set" | "text" => MergeAttribute::Text,
            "unset" | "binary" => MergeAttribute::Unmergeable,
            "union" => MergeAttribute::Union,
            DERIVED_CONTENT_DRIVER_NAME if keeps_incoming => MergeAttribute::Derived,
            driver => {
                let defined = git(
                    Argv::from(["config", "--get"]).text(format!("merge.{driver}.driver")),
                    repo,
                    None,
                    None,
                )?;
                match defined.status.success() {
                    true => MergeAttribute::Driver(driver.to_owned()),
                    false => MergeAttribute::Text,
                }
            }
        });
    }
    Ok(attributes)
}

/// The object name no object has, in the repository's object format.
fn zero_id(repo: &Path) -> Result<String, VcsError> {
    let format = text(succeeded(
        ["rev-parse", "--show-object-format"],
        repo,
        None,
    )?);
    Ok("0".repeat(match format.as_str() {
        "sha256" => 64,
        _ => 40,
    }))
}

fn file_mode_honored(repo: &Path) -> Result<bool, VcsError> {
    let said = git(
        ["config", "--type=bool", "--get", "core.fileMode"],
        repo,
        None,
        None,
    )?;
    Ok(!said.status.success() || text(said.stdout) != "false")
}

fn blob_bytes(repo: &Path, blob: &str) -> Result<Vec<u8>, VcsError> {
    succeeded(["cat-file", "blob", blob], repo, None)
}

fn smudged(repo: &Path, path: &str, blob: &str, incoming: &CommitId) -> Result<Vec<u8>, VcsError> {
    let mut args = Argv::from(Vec::<String>::new());
    if git_reads_attributes_from_a_commit() {
        args = args.text(format!("--attr-source={}", incoming.as_str()));
    }
    succeeded(
        args.text("cat-file")
            .text("--filters")
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
    union: bool,
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
    if union {
        args = args.text("--union");
    }
    for label in labels {
        args = args.text("-L").text(label);
    }
    for file in &files {
        args = args.text(file.to_string_lossy());
    }
    let output = git(args, repo, None, None)?;
    Ok(match output.status.code() {
        Some(0) => Some((output.stdout, true)),
        Some(n) if (1..=127).contains(&n) => Some((output.stdout, false)),
        _ => None,
    })
}

/// The mode a three-way merge of the modes gives, or `None` where the
/// sides changed it differently.
fn merged_mode<'a>(base: Option<&'a str>, ours: &'a str, theirs: &'a str) -> Option<&'a str> {
    match base {
        Some(base) if ours == base => Some(theirs),
        Some(base) if theirs == base => Some(ours),
        _ if ours == theirs => Some(ours),
        _ => None,
    }
}

struct Planning<'a> {
    repo: &'a Path,
    scratch: &'a Path,
    labels: [&'a str; 3],
    incoming: &'a CommitId,
    file_mode: bool,
    resolution: EditResolution,
}

impl Planning<'_> {
    fn ours_mode(&self, read: &Read) -> String {
        let recorded = read
            .standing
            .index
            .as_ref()
            .or(read.base.as_ref())
            .or(read.theirs.as_ref())
            .map(|e| e.mode.clone())
            .unwrap_or_else(|| REGULAR.to_owned());
        match &read.standing.ours {
            Ours::Link(_) => LINK.to_owned(),
            Ours::File { executable, .. } if self.file_mode => match executable {
                true => EXECUTABLE.to_owned(),
                false => REGULAR.to_owned(),
            },
            _ => recorded,
        }
    }

    fn content(&self, path: &str, blob: &str, mode: &str) -> Result<Write, VcsError> {
        Ok(Write::Content {
            bytes: smudged(self.repo, path, blob, self.incoming)?,
            executable: mode == EXECUTABLE,
        })
    }

    fn plan(&self, read: &Read) -> Result<Planned, VcsError> {
        let ours_mode = self.ours_mode(read);
        let ours_entry = read.standing.ours.blob().map(|blob| Entry {
            mode: ours_mode.clone(),
            blob: blob.clone(),
        });
        let stages_only = || Planned::Conflicted {
            write: Write::Keep,
            ours: ours_entry.clone(),
        };
        let setting_aside = self.resolution == EditResolution::SetAside;
        if setting_aside && read.theirs.as_ref().is_some_and(|t| t.mode == GITLINK) {
            return Ok(Planned::Clean {
                write: Write::Keep,
                entry: read.theirs.clone(),
                took_incoming: true,
            });
        }
        if setting_aside && matches!(read.standing.ours, Ours::Link(_)) {
            return Ok(Planned::Clean {
                write: match &read.theirs {
                    Some(theirs) => match self.content(&read.path, &theirs.blob, &theirs.mode)? {
                        Write::Content { bytes, executable } => {
                            Write::OverLink { bytes, executable }
                        }
                        other => other,
                    },
                    None => Write::Remove,
                },
                entry: read.theirs.clone(),
                took_incoming: true,
            });
        }
        let replaceable = matches!(read.standing.ours, Ours::File { .. } | Ours::Absent);
        if read.merge == MergeAttribute::Derived || setting_aside {
            if !replaceable {
                return Ok(stages_only());
            }
            return Ok(Planned::Clean {
                write: match &read.theirs {
                    Some(theirs) => self.content(&read.path, &theirs.blob, &theirs.mode)?,
                    None if read.standing.ours == Ours::Absent => Write::Keep,
                    None => Write::Remove,
                },
                entry: read.theirs.clone(),
                took_incoming: true,
            });
        }
        let (Some(theirs), Some(ours)) = (&read.theirs, read.standing.ours.blob()) else {
            let write = match (&read.theirs, &read.standing.ours) {
                (Some(theirs), Ours::Absent) => {
                    self.content(&read.path, &theirs.blob, &theirs.mode)?
                }
                _ => Write::Keep,
            };
            return Ok(Planned::Conflicted {
                write,
                ours: ours_entry,
            });
        };
        let mergeable = theirs.regular()
            && read.base.as_ref().is_none_or(Entry::regular)
            && matches!(ours_mode.as_str(), REGULAR | EXECUTABLE)
            && matches!(read.merge, MergeAttribute::Text | MergeAttribute::Union);
        if !mergeable {
            return Ok(stages_only());
        }
        let empty = store(self.repo, b"")?;
        let base_blob = read
            .base
            .as_ref()
            .map_or(empty.as_str(), |b| b.blob.as_str());
        let mode = merged_mode(
            read.base.as_ref().map(|b| b.mode.as_str()),
            &ours_mode,
            &theirs.mode,
        );
        let merged = merge_text(
            self.repo,
            self.scratch,
            self.labels,
            [ours, base_blob, &theirs.blob],
            read.merge == MergeAttribute::Union,
        )?;
        Ok(match (merged, mode) {
            (Some((content, true)), Some(mode)) => {
                let blob = store(self.repo, &content)?;
                Planned::Clean {
                    write: self.content(&read.path, &blob, mode)?,
                    entry: Some(theirs.clone()),
                    took_incoming: false,
                }
            }
            (Some((content, _)), mode) => {
                let blob = store(self.repo, &content)?;
                Planned::Conflicted {
                    write: self.content(&read.path, &blob, mode.unwrap_or(&ours_mode))?,
                    ours: ours_entry,
                }
            }
            (None, _) => stages_only(),
        })
    }
}

/// Every leading directory of `path` that is missing is made; one that is
/// anything but a directory refuses the write.
fn leading_directories(repo: &Path, path: &str) -> Result<PathBuf, VcsError> {
    let mut at = repo.to_path_buf();
    let components: Vec<&str> = path.split('/').collect();
    let (name, leading) = components.split_last().expect("a path has a name");
    for component in leading {
        at.push(component);
        match std::fs::symlink_metadata(&at) {
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => {
                return Err(io_error(
                    format!("not written: {path}"),
                    &format!("{} is not a directory", at.display()),
                ))
            }
            Err(_) => std::fs::create_dir(&at).map_err(|e| VcsError::Io {
                ctx: format!("failed to create {}", at.display()),
                source: e,
            })?,
        }
    }
    at.push(name);
    Ok(at)
}

/// Write `bytes` at `path` by renaming a file written beside it over the
/// path, so a symbolic link standing there is replaced rather than followed;
/// one standing there, or anything but a file, refuses the write.
fn write_file(
    repo: &Path,
    path: &str,
    bytes: &[u8],
    executable: Option<bool>,
) -> Result<(), VcsError> {
    let file = leading_directories(repo, path)?;
    let io = |e| VcsError::Io {
        ctx: format!("failed to write {}", file.display()),
        source: e,
    };
    let standing = std::fs::symlink_metadata(&file).ok();
    if standing
        .as_ref()
        .is_some_and(|meta| meta.file_type().is_symlink() || !meta.is_file())
    {
        return Err(io_error(
            format!("not written: {path}"),
            &format!("{} is not a file", file.display()),
        ));
    }
    let beside = file.with_file_name(format!(
        ".{}.rwv-merge-{}",
        file.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        std::process::id()
    ));
    let written = (|| {
        std::fs::write(&beside, bytes)?;
        let mut permissions = match &standing {
            Some(meta) => meta.permissions(),
            None => std::fs::metadata(&beside)?.permissions(),
        };
        #[cfg(unix)]
        if let Some(executable) = executable {
            use std::os::unix::fs::PermissionsExt;
            let mode = permissions.mode();
            permissions.set_mode(match executable {
                true => mode | ((mode & 0o444) >> 2),
                false => mode & !0o111,
            });
        }
        #[cfg(not(unix))]
        let _ = executable;
        std::fs::set_permissions(&beside, permissions)?;
        std::fs::rename(&beside, &file)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&beside);
    }
    written.map_err(io)
}

/// Remove the file or link at `path` itself, never what a link names.
fn remove_entry(repo: &Path, path: &str) -> Result<(), VcsError> {
    let file = leading_directories(repo, path)?;
    match std::fs::symlink_metadata(&file) {
        Ok(meta) if meta.is_file() || meta.file_type().is_symlink() => {
            crate::symlink::remove(&file).map_err(|e| VcsError::Io {
                ctx: format!("failed to remove {}", file.display()),
                source: e,
            })
        }
        Ok(_) => Err(io_error(
            format!("not removed: {path}"),
            &format!("{} is not a file", file.display()),
        )),
        Err(_) => Ok(()),
    }
}

fn write_link(repo: &Path, path: &str, target: &[u8]) -> Result<(), VcsError> {
    remove_entry(repo, path)?;
    let file = leading_directories(repo, path)?;
    let target = PathBuf::from(String::from_utf8_lossy(target).into_owned());
    let kind = crate::symlink::LinkTarget::on_disk(&file.parent().unwrap_or(repo).join(&target));
    crate::symlink::create(&target, &file, kind).map_err(|e| {
        io_error(
            format!("failed to make the link {}", file.display()),
            &format!("{e:#}"),
        )
    })
}

/// What one path held before the first write, as the journal keeps it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Held {
    path: String,
    stages: Vec<(String, String, String)>,
    disk: Disk,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Disk {
    Keep,
    Absent,
    File { executable: bool, blob: String },
    Link { blob: String },
}

impl Disk {
    fn spelled(&self) -> String {
        match self {
            Self::Keep => "keep".to_owned(),
            Self::Absent => "absent".to_owned(),
            Self::File { executable, blob } => format!("file {} {blob}", u8::from(*executable)),
            Self::Link { blob } => format!("link {blob}"),
        }
    }

    fn read(spelled: &str) -> Option<Self> {
        let words: Vec<&str> = spelled.split(' ').collect();
        Some(match words[..] {
            ["keep"] => Self::Keep,
            ["absent"] => Self::Absent,
            ["file", executable, blob] => Self::File {
                executable: executable == "1",
                blob: blob.to_owned(),
            },
            ["link", blob] => Self::Link {
                blob: blob.to_owned(),
            },
            _ => return None,
        })
    }
}

fn held_now(repo: &Path, path: &str) -> Result<Held, VcsError> {
    let disk = match on_disk(repo, path) {
        OnDisk::Absent => Disk::Absent,
        OnDisk::File { executable } => {
            let file = repo.join(path);
            let bytes = std::fs::read(&file).map_err(|e| VcsError::Io {
                ctx: format!("failed to read {}", file.display()),
                source: e,
            })?;
            Disk::File {
                executable,
                blob: store(repo, &bytes)?,
            }
        }
        OnDisk::Link => Disk::Link {
            blob: store(repo, &link_target(repo, path)?)?,
        },
        OnDisk::Other | OnDisk::Behind => Disk::Keep,
    };
    Ok(Held {
        path: path.to_owned(),
        stages: index_stages(repo, path)?,
        disk,
    })
}

struct Journal {
    backup: Option<String>,
    held: Vec<Held>,
}

impl Journal {
    fn spelled(&self) -> Vec<u8> {
        let mut out = Vec::new();
        let mut field = |text: &str| {
            out.extend_from_slice(text.as_bytes());
            out.push(0);
        };
        field(JOURNAL_FORMAT);
        field(self.backup.as_deref().unwrap_or(""));
        for held in &self.held {
            field(&held.path);
            field(
                &held
                    .stages
                    .iter()
                    .map(|(mode, blob, stage)| format!("{mode} {blob} {stage}"))
                    .collect::<Vec<_>>()
                    .join(","),
            );
            field(&held.disk.spelled());
        }
        out
    }

    fn read(bytes: &[u8]) -> Option<Self> {
        let fields: Vec<String> = bytes
            .split(|b| *b == 0)
            .map(|f| String::from_utf8_lossy(f).into_owned())
            .collect();
        let (format, rest) = fields.split_first()?;
        if format != JOURNAL_FORMAT {
            return None;
        }
        let (backup, rest) = rest.split_first()?;
        let rest = rest.strip_suffix(&[String::new()]).unwrap_or(rest);
        if rest.len() % 3 != 0 {
            return None;
        }
        let mut held = Vec::new();
        for record in rest.chunks(3) {
            let stages = record[1]
                .split(',')
                .filter(|s| !s.is_empty())
                .map(|s| {
                    let words: Vec<&str> = s.split(' ').collect();
                    match words[..] {
                        [mode, blob, stage] => {
                            Some((mode.to_owned(), blob.to_owned(), stage.to_owned()))
                        }
                        _ => None,
                    }
                })
                .collect::<Option<Vec<_>>>()?;
            held.push(Held {
                path: record[0].clone(),
                stages,
                disk: Disk::read(&record[2])?,
            });
        }
        Some(Self {
            backup: (!backup.is_empty()).then(|| backup.clone()),
            held,
        })
    }

    fn write(&self, repo: &Path) -> Result<(), VcsError> {
        let at = git_path(repo, JOURNAL)?;
        let beside = at.with_extension(format!("{}", std::process::id()));
        let io = |e| VcsError::Io {
            ctx: format!("failed to write {}", at.display()),
            source: e,
        };
        std::fs::write(&beside, self.spelled()).map_err(io)?;
        std::fs::rename(&beside, &at).map_err(io)
    }

    fn found(repo: &Path) -> Result<Option<(PathBuf, Self)>, VcsError> {
        let at = git_path(repo, JOURNAL)?;
        match std::fs::read(&at) {
            Ok(bytes) => match Self::read(&bytes) {
                Some(journal) => Ok(Some((at, journal))),
                None => Err(io_error(
                    format!("failed to read {}", at.display()),
                    "not a journal this release writes",
                )),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(VcsError::Io {
                ctx: format!("failed to read {}", at.display()),
                source: e,
            }),
        }
    }

    /// Put every path back as the journal holds it.
    fn restore(&self, repo: &Path) -> Result<(), VcsError> {
        for held in &self.held {
            if held_now(repo, &held.path)?.disk == held.disk {
                continue;
            }
            match &held.disk {
                Disk::Keep => {}
                Disk::Absent => remove_entry(repo, &held.path)?,
                Disk::File { executable, blob } => {
                    if on_disk(repo, &held.path) == OnDisk::Link {
                        remove_entry(repo, &held.path)?;
                    }
                    write_file(
                        repo,
                        &held.path,
                        &blob_bytes(repo, blob)?,
                        Some(*executable),
                    )?
                }
                Disk::Link { blob } => write_link(repo, &held.path, &blob_bytes(repo, blob)?)?,
            }
        }
        let zero = zero_id(repo)?;
        let mut info = Vec::new();
        for held in &self.held {
            info.extend_from_slice(format!("0 {zero}\t{}", held.path).as_bytes());
            info.push(0);
            for (mode, blob, stage) in &held.stages {
                info.extend_from_slice(format!("{mode} {blob} {stage}\t{}", held.path).as_bytes());
                info.push(0);
            }
        }
        succeeded(["update-index", "-z", "--index-info"], repo, Some(&info))?;
        let _ = git(["update-index", "-q", "--refresh"], repo, None, None);
        Ok(())
    }
}

pub(super) fn interrupted(repo: &Path) -> Result<Option<InterruptedMerge>, VcsError> {
    Ok(Journal::found(repo)?.map(|(_, journal)| InterruptedMerge {
        backup: journal.backup,
        paths: journal.held.into_iter().map(|h| h.path).collect(),
    }))
}

pub(super) fn put_back(repo: &Path) -> Result<(), VcsError> {
    let Some((at, journal)) = Journal::found(repo)? else {
        return Ok(());
    };
    journal.restore(repo)?;
    std::fs::remove_file(&at).map_err(|e| VcsError::Io {
        ctx: format!("failed to remove {}", at.display()),
        source: e,
    })
}

pub(super) fn merge_into_working_copy(
    repo: &Path,
    request: &WorkingCopyMergeRequest<'_>,
    before_write: &mut dyn FnMut() -> Result<BeforeWrite, VcsError>,
) -> Result<WorkingCopyMerge, VcsError> {
    let (base, incoming, paths) = (request.base, request.incoming, request.paths);
    if Journal::found(repo)?.is_some() {
        return Ok(WorkingCopyMerge::Declined(MergeDeclined::Interrupted));
    }
    if !succeeded(["ls-files", "-u", "-z"], repo, None)?.is_empty() {
        return Ok(WorkingCopyMerge::Declined(MergeDeclined::Unresolved));
    }
    let behind: Vec<String> = paths
        .iter()
        .filter(|p| on_disk(repo, p) == OnDisk::Behind)
        .cloned()
        .collect();
    if !behind.is_empty() {
        return Ok(WorkingCopyMerge::Declined(MergeDeclined::BehindLink(
            behind,
        )));
    }
    let attributes = merge_attributes(repo, incoming, paths, request.derived)?;
    let intended = intended(repo, paths)?;
    let mut reads = Vec::new();
    for (path, merge) in paths.iter().zip(attributes) {
        reads.push(Read {
            path: path.clone(),
            base: tree_entry(repo, base, path)?,
            theirs: tree_entry(repo, incoming, path)?,
            standing: standing(repo, path, intended.contains(path))?,
            merge,
        });
    }
    let setting_aside = request.resolution == EditResolution::SetAside;
    let submodules: Vec<String> = reads
        .iter()
        .filter(|_| !setting_aside)
        .filter(|r| {
            [&r.base, &r.theirs, &r.standing.index]
                .into_iter()
                .flatten()
                .any(|e| e.mode == GITLINK)
        })
        .map(|r| r.path.clone())
        .collect();
    if !submodules.is_empty() {
        return Ok(WorkingCopyMerge::Declined(MergeDeclined::Submodule(
            submodules,
        )));
    }
    let (ignored, marked) = uncompared(repo, paths)?;
    if !ignored.is_empty() || !marked.is_empty() {
        return Ok(WorkingCopyMerge::Declined(MergeDeclined::Uncompared {
            ignored,
            marked,
        }));
    }
    if let Some(read) = reads
        .iter()
        .filter(|_| !setting_aside)
        .find(|r| matches!(r.merge, MergeAttribute::Driver(_)))
    {
        let MergeAttribute::Driver(driver) = &read.merge else {
            unreachable!("found by its driver");
        };
        return Ok(WorkingCopyMerge::Declined(MergeDeclined::Driver {
            path: read.path.clone(),
            driver: driver.clone(),
        }));
    }
    let two_edits: Vec<String> = reads
        .iter()
        .filter(|r| {
            let index = r.standing.index.as_ref().map(|e| &e.blob);
            index != r.base.as_ref().map(|e| &e.blob) && index != r.standing.ours.blob()
        })
        .map(|r| r.path.clone())
        .collect();
    if !two_edits.is_empty() && !setting_aside {
        return Ok(WorkingCopyMerge::TwoEdits(two_edits));
    }
    let scratch = git_path(
        repo,
        &format!("rwv-working-copy-merge-{}", std::process::id()),
    )?;
    std::fs::create_dir_all(&scratch).map_err(|e| VcsError::Io {
        ctx: format!("failed to create {}", scratch.display()),
        source: e,
    })?;
    let planning = Planning {
        repo,
        scratch: &scratch,
        labels: ["working copy", base.short(), incoming.short()],
        incoming,
        file_mode: file_mode_honored(repo)?,
        resolution: request.resolution,
    };
    let planned: Result<Vec<Planned>, VcsError> = reads.iter().map(|r| planning.plan(r)).collect();
    let _ = std::fs::remove_dir_all(&scratch);
    let planned = planned?;
    for read in &reads {
        if standing(repo, &read.path, intended.contains(&read.path))? != read.standing {
            return Ok(WorkingCopyMerge::Declined(MergeDeclined::Changed(
                read.path.clone(),
            )));
        }
    }
    let BeforeWrite::Write { backup } = before_write()? else {
        return Ok(WorkingCopyMerge::Stopped);
    };
    let mut journaled = Vec::new();
    for path in paths
        .iter()
        .chain(request.stale_files)
        .chain(request.stale_index_entries)
    {
        journaled.push(held_now(repo, path)?);
    }
    for held in journaled
        .iter_mut()
        .filter(|h| request.stale_index_entries.contains(&h.path))
    {
        held.disk = Disk::Keep;
    }
    let journal = Journal {
        backup,
        held: journaled,
    };
    journal.write(repo)?;
    let written = write_all(repo, request, &reads, &planned, &intended).map(|written| {
        written.map(|merged| match merged {
            WorkingCopyMerge::Merged {
                clean,
                took_incoming,
                conflicted,
                staged_apart: _,
            } => WorkingCopyMerge::Merged {
                staged_apart: took_incoming
                    .iter()
                    .filter(|p| two_edits.contains(p))
                    .cloned()
                    .collect(),
                clean,
                took_incoming,
                conflicted,
            },
            other => other,
        })
    });
    let declined_once_put_back = |declined: MergeDeclined| -> Result<WorkingCopyMerge, VcsError> {
        put_back(repo)?;
        Ok(WorkingCopyMerge::Declined(declined))
    };
    match written {
        Ok(Ok(merged)) => {
            std::fs::remove_file(git_path(repo, JOURNAL)?).map_err(|e| VcsError::Io {
                ctx: format!("failed to remove the journal in {}", repo.display()),
                source: e,
            })?;
            Ok(merged)
        }
        Ok(Err(declined)) => declined_once_put_back(declined),
        Err(e) => match declined_once_put_back(MergeDeclined::WriteFailed(format!("{e:#}"))) {
            Ok(declined) => Ok(declined),
            Err(putting_back) => Err(io_error(
                format!("{e:#}"),
                &format!("putting back what was written failed: {putting_back:#}"),
            )),
        },
    }
}

/// Every write of the merge, in order: the stale paths refreshed, each
/// merged path written once its standing is read again unchanged, then the
/// merged paths' index entries in one update.
fn write_all(
    repo: &Path,
    request: &WorkingCopyMergeRequest<'_>,
    reads: &[Read],
    planned: &[Planned],
    intended: &[String],
) -> Result<Result<WorkingCopyMerge, MergeDeclined>, VcsError> {
    let (base, incoming) = (request.base, request.incoming);
    crate::test_pause::point("drift-merge-journaled");
    for (scope, stale) in [
        (RefreshScope::IndexAndFiles, request.stale_files),
        (RefreshScope::IndexOnly, request.stale_index_entries),
    ] {
        match refresh_paths_if_left(repo, base, incoming, stale, scope)? {
            RefreshWrite::Written => {}
            RefreshWrite::Unresolved => return Ok(Err(MergeDeclined::Unresolved)),
            RefreshWrite::Declined { vcs_said } => {
                return Ok(Err(MergeDeclined::RefreshDeclined(vcs_said)))
            }
        }
    }
    crate::test_pause::point("drift-merge-refreshed");
    let zero = zero_id(repo)?;
    let mut index_info = Vec::new();
    let mut line = |mode: &str, blob: &str, stage: u8, path: &str| {
        index_info.extend_from_slice(format!("{mode} {blob} {stage}\t{path}").as_bytes());
        index_info.push(0);
    };
    let (mut clean, mut took_incoming, mut conflicted) = (Vec::new(), Vec::new(), Vec::new());
    for (read, planned) in reads.iter().zip(planned) {
        let path = read.path.as_str();
        if standing(repo, path, intended.iter().any(|p| p == path))? != read.standing {
            return Ok(Err(MergeDeclined::Changed(path.to_owned())));
        }
        line("0", &zero, 0, path);
        let write = match planned {
            Planned::Clean {
                write,
                entry,
                took_incoming: took,
            } => {
                if let Some(entry) = entry {
                    line(&entry.mode, &entry.blob, 0, path);
                }
                match took {
                    true => took_incoming.push(path.to_owned()),
                    false => clean.push(path.to_owned()),
                }
                write
            }
            Planned::Conflicted { write, ours } => {
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
                write
            }
        };
        match write {
            Write::Keep => {}
            Write::Remove => remove_entry(repo, path)?,
            Write::Content { bytes, executable } => {
                write_file(repo, path, bytes, Some(*executable))?
            }
            Write::OverLink { bytes, executable } => {
                remove_entry(repo, path)?;
                write_file(repo, path, bytes, Some(*executable))?
            }
        }
    }
    crate::test_pause::point("drift-merge-written");
    succeeded(
        ["update-index", "-z", "--index-info"],
        repo,
        Some(&index_info),
    )?;
    Ok(Ok(WorkingCopyMerge::Merged {
        clean,
        took_incoming,
        conflicted,
        staged_apart: Vec::new(),
    }))
}

#[cfg(test)]
mod tests {
    //! The merge against a checkout whose branch moved under it: `main`
    //! moved from `base` to `moved` while the index and files stayed at
    //! `base`, then each test's edits made here.
    use super::*;
    use crate::git::{git_command_in_test, LineEndings};

    fn run(dir: &Path, args: &[&str]) -> String {
        let out = git_command_in_test()
            .args(args)
            .current_dir(dir)
            .output()
            .expect("run git");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    struct Moved {
        _tmp: tempfile::TempDir,
        outside: PathBuf,
        repo: PathBuf,
        base: CommitId,
        moved: CommitId,
        resolution: EditResolution,
    }

    type Files<'a> = &'a [(&'a str, &'a str)];

    fn write(repo: &Path, files: Files<'_>) {
        for (path, content) in files {
            let file = repo.join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, content).unwrap();
        }
    }

    impl Moved {
        fn new(endings: LineEndings, base: Files<'_>, moved: Files<'_>) -> Self {
            Self::shaped(endings, base, moved, |_| {})
        }

        /// `shape` runs on the moved tip's files before it is committed.
        fn shaped(
            endings: LineEndings,
            base: Files<'_>,
            moved: Files<'_>,
            shape: impl FnOnce(&Path),
        ) -> Self {
            Self::configured(endings, &[], base, moved, shape)
        }

        /// As [`Moved::shaped`], with `config` set before anything is
        /// committed.
        fn configured(
            endings: LineEndings,
            config: &[(&str, &str)],
            base: Files<'_>,
            moved: Files<'_>,
            shape: impl FnOnce(&Path),
        ) -> Self {
            let tmp = crate::tempdir().unwrap();
            let repo = tmp.path().join("repo");
            let outside = tmp.path().join("outside");
            std::fs::create_dir_all(&repo).unwrap();
            std::fs::create_dir_all(&outside).unwrap();
            run(&repo, &["init", "-q", "-b", "main"]);
            run(&repo, &["config", "user.email", "t@t"]);
            run(&repo, &["config", "user.name", "T"]);
            endings.pin(&repo);
            for (key, value) in config {
                run(&repo, &["config", key, value]);
            }
            write(&repo, base);
            run(&repo, &["add", "-A"]);
            run(&repo, &["commit", "-q", "-m", "base"]);
            let base = CommitId::parse(&run(&repo, &["rev-parse", "HEAD"])).unwrap();
            run(&repo, &["checkout", "-q", "-b", "elsewhere"]);
            write(&repo, moved);
            shape(&repo);
            run(&repo, &["add", "-A"]);
            run(&repo, &["commit", "-q", "-m", "moved"]);
            let moved = CommitId::parse(&run(&repo, &["rev-parse", "HEAD"])).unwrap();
            run(&repo, &["checkout", "-q", "main"]);
            run(&repo, &["update-ref", "refs/heads/main", moved.as_str()]);
            Self {
                _tmp: tmp,
                outside,
                repo,
                base,
                moved,
                resolution: EditResolution::Merge,
            }
        }

        fn merge(&self, paths: &[&str], stale: &[&str]) -> WorkingCopyMerge {
            self.merge_with(paths, stale, &mut || {
                Ok(BeforeWrite::Write { backup: None })
            })
        }

        fn merge_with(
            &self,
            paths: &[&str],
            stale: &[&str],
            before_write: &mut dyn FnMut() -> Result<BeforeWrite, VcsError>,
        ) -> WorkingCopyMerge {
            let paths: Vec<String> = paths.iter().map(|p| (*p).to_owned()).collect();
            let stale: Vec<String> = stale.iter().map(|p| (*p).to_owned()).collect();
            merge_into_working_copy(
                &self.repo,
                &WorkingCopyMergeRequest {
                    base: &self.base,
                    incoming: &self.moved,
                    paths: &paths,
                    stale_files: &stale,
                    stale_index_entries: &[],
                    derived: DerivedContentPolicy::keep_incoming_side(),
                    resolution: self.resolution,
                },
                before_write,
            )
            .unwrap()
        }

        fn text(&self, path: &str) -> String {
            std::fs::read_to_string(self.repo.join(path))
                .unwrap()
                .replace("\r\n", "\n")
        }

        fn stages(&self, path: &str) -> Vec<String> {
            index_stages(&self.repo, path)
                .unwrap()
                .into_iter()
                .map(|(_, _, stage)| stage)
                .collect()
        }

        fn status(&self) -> String {
            run(&self.repo, &["status", "--porcelain"])
        }

        fn journal_left(&self) -> bool {
            interrupted(&self.repo).unwrap().is_some()
        }
    }

    const THREE: &str = "one\ntwo\nthree\n";
    const MOVED: &str = "one\ntwo\nTHREE\n";

    fn a_clean_merge_stands_as_an_edit_on_the_moved_tips_entry(endings: LineEndings) {
        let at = Moved::new(
            endings,
            &[("f.txt", THREE), ("stale.txt", "s\n")],
            &[("f.txt", MOVED), ("stale.txt", "s2\n")],
        );
        write(&at.repo, &[("f.txt", "ONE\ntwo\nthree\n")]);
        assert_eq!(
            at.merge(&["f.txt"], &["stale.txt"]),
            WorkingCopyMerge::Merged {
                clean: vec!["f.txt".to_owned()],
                took_incoming: vec![],
                conflicted: vec![],
                staged_apart: vec![],
            }
        );
        assert_eq!(at.text("f.txt"), "ONE\ntwo\nTHREE\n");
        assert_eq!(at.text("stale.txt"), "s2\n");
        assert_eq!(at.status(), "M f.txt");
        assert_eq!(
            run(&at.repo, &["diff", "--cached", "--name-only", "HEAD"]),
            ""
        );
        if endings == LineEndings::Converted {
            let bytes = std::fs::read(at.repo.join("f.txt")).unwrap();
            assert!(bytes.windows(2).any(|w| w == b"\r\n"), "{bytes:?}");
        }
        assert!(!at.journal_left());
    }

    fn a_conflict_is_left_with_its_three_stages(endings: LineEndings) {
        let at = Moved::new(endings, &[("f.txt", THREE)], &[("f.txt", MOVED)]);
        write(&at.repo, &[("f.txt", "one\ntwo\nmine\n")]);
        assert_eq!(
            at.merge(&["f.txt"], &[]),
            WorkingCopyMerge::Merged {
                clean: vec![],
                took_incoming: vec![],
                conflicted: vec!["f.txt".to_owned()],
                staged_apart: vec![],
            }
        );
        assert_eq!(at.stages("f.txt"), ["1", "2", "3"]);
        let text = at.text("f.txt");
        assert!(text.contains("<<<<<<< working copy\nmine\n"), "{text}");
        assert!(text.contains("THREE\n>>>>>>> "), "{text}");
        run(&at.repo, &["checkout", "--ours", "--", "f.txt"]);
        assert_eq!(at.text("f.txt"), "one\ntwo\nmine\n");
    }

    #[test]
    fn a_clean_filter_is_merged_on_and_a_smudge_filter_written_through() {
        let at = Moved::configured(
            LineEndings::Verbatim,
            &[
                ("filter.up.clean", "tr a-z A-Z"),
                ("filter.up.smudge", "tr A-Z a-z"),
            ],
            &[
                (".gitattributes", "*.up filter=up\n"),
                ("f.up", "one\ntwo\nthree\n"),
            ],
            &[("f.up", "one\ntwo\nmoved\n")],
            |_| {},
        );
        write(&at.repo, &[("f.up", "one-mine\ntwo\nthree\n")]);
        let merged = at.merge(&["f.up"], &[]);
        assert_eq!(
            merged,
            WorkingCopyMerge::Merged {
                clean: vec!["f.up".to_owned()],
                took_incoming: vec![],
                conflicted: vec![],
                staged_apart: vec![],
            }
        );
        assert_eq!(at.text("f.up"), "one-mine\ntwo\nmoved\n");
        assert_eq!(
            run(&at.repo, &["show", ":f.up"]),
            "ONE\nTWO\nMOVED",
            "the index holds the moved tip's clean-filtered entry"
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_moves_executable_bit_survives_a_clean_merge_of_the_content() {
        use std::os::unix::fs::PermissionsExt;
        let at = Moved::shaped(
            LineEndings::Verbatim,
            &[("run.sh", "one\ntwo\n")],
            &[],
            |repo| {
                std::fs::set_permissions(
                    repo.join("run.sh"),
                    std::fs::Permissions::from_mode(0o755),
                )
                .unwrap()
            },
        );
        std::fs::set_permissions(
            at.repo.join("run.sh"),
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        write(&at.repo, &[("run.sh", "one\ntwo\nthree\n")]);
        assert!(matches!(
            at.merge(&["run.sh"], &[]),
            WorkingCopyMerge::Merged { ref clean, .. } if clean == &["run.sh"]
        ));
        let mode = std::fs::metadata(at.repo.join("run.sh"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o100, 0o100, "{mode:o}");
        assert!(
            !run(&at.repo, &["diff"]).contains("mode"),
            "{}",
            run(&at.repo, &["diff"])
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_file_replaced_by_a_link_is_left_conflicted_and_its_target_untouched() {
        let at = Moved::new(
            LineEndings::Verbatim,
            &[
                (".gitattributes", "GEN merge=rwv-ours\n"),
                ("GEN", "base\n"),
                ("f.txt", THREE),
            ],
            &[("GEN", "moved\n"), ("f.txt", MOVED)],
        );
        for (path, held) in [("GEN", "gen-local"), ("f.txt", "f-local")] {
            std::fs::write(at.outside.join(held), "precious\n").unwrap();
            std::fs::remove_file(at.repo.join(path)).unwrap();
            std::os::unix::fs::symlink(at.outside.join(held), at.repo.join(path)).unwrap();
        }
        assert_eq!(
            at.merge(&["GEN", "f.txt"], &[]),
            WorkingCopyMerge::Merged {
                clean: vec![],
                took_incoming: vec![],
                conflicted: vec!["GEN".to_owned(), "f.txt".to_owned()],
                staged_apart: vec![],
            }
        );
        for (path, held) in [("GEN", "gen-local"), ("f.txt", "f-local")] {
            assert_eq!(
                std::fs::read_to_string(at.outside.join(held)).unwrap(),
                "precious\n"
            );
            assert!(std::fs::symlink_metadata(at.repo.join(path))
                .unwrap()
                .file_type()
                .is_symlink());
            assert_eq!(at.stages(path), ["1", "2", "3"], "{path}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_path_behind_a_linked_directory_is_declined_and_nothing_is_written() {
        let at = Moved::new(
            LineEndings::Verbatim,
            &[("d/f.txt", THREE), ("stale.txt", "s\n")],
            &[("d/f.txt", MOVED), ("stale.txt", "s2\n")],
        );
        std::fs::rename(at.repo.join("d"), at.outside.join("d")).unwrap();
        std::os::unix::fs::symlink(at.outside.join("d"), at.repo.join("d")).unwrap();
        write(&at.outside, &[("d/f.txt", "one\ntwo\nmine\n")]);
        assert_eq!(
            at.merge(&["d/f.txt"], &["stale.txt"]),
            WorkingCopyMerge::Declined(MergeDeclined::BehindLink(vec!["d/f.txt".to_owned()]))
        );
        assert_eq!(
            std::fs::read_to_string(at.outside.join("d/f.txt")).unwrap(),
            "one\ntwo\nmine\n"
        );
        assert_eq!(at.text("stale.txt"), "s\n");
    }

    #[cfg(unix)]
    #[test]
    fn a_write_is_never_made_through_a_link_that_appears_after_the_reading() {
        let at = Moved::new(
            LineEndings::Verbatim,
            &[("f.txt", THREE)],
            &[("f.txt", MOVED)],
        );
        write(&at.repo, &[("f.txt", "ONE\ntwo\nthree\n")]);
        std::fs::write(at.outside.join("held"), "precious\n").unwrap();
        let (repo, outside) = (at.repo.clone(), at.outside.clone());
        let merged = at.merge_with(&["f.txt"], &[], &mut || {
            std::fs::remove_file(repo.join("f.txt")).unwrap();
            std::os::unix::fs::symlink(outside.join("held"), repo.join("f.txt")).unwrap();
            Ok(BeforeWrite::Write { backup: None })
        });
        assert_eq!(
            merged,
            WorkingCopyMerge::Declined(MergeDeclined::Changed("f.txt".to_owned()))
        );
        assert_eq!(
            std::fs::read_to_string(at.outside.join("held")).unwrap(),
            "precious\n"
        );
        assert!(write_file(&at.repo, "f.txt", b"x\n", None).is_err());
        assert_eq!(
            std::fs::read_to_string(at.outside.join("held")).unwrap(),
            "precious\n"
        );
    }

    #[test]
    fn a_save_after_the_backup_declines_the_merge_and_puts_the_refresh_back() {
        let at = Moved::new(
            LineEndings::Verbatim,
            &[("f.txt", THREE), ("stale.txt", "s\n")],
            &[("f.txt", MOVED), ("stale.txt", "s2\n")],
        );
        write(&at.repo, &[("f.txt", "ONE\ntwo\nthree\n")]);
        let repo = at.repo.clone();
        let merged = at.merge_with(&["f.txt"], &["stale.txt"], &mut || {
            write(&repo, &[("f.txt", "ONE\ntwo\nthree\nLATE\n")]);
            Ok(BeforeWrite::Write { backup: None })
        });
        assert_eq!(
            merged,
            WorkingCopyMerge::Declined(MergeDeclined::Changed("f.txt".to_owned()))
        );
        assert_eq!(at.text("f.txt"), "ONE\ntwo\nthree\nLATE\n");
        assert_eq!(at.text("stale.txt"), "s\n");
        assert_eq!(
            run(&at.repo, &["show", ":stale.txt"]),
            "s",
            "the stale index entry is put back"
        );
        assert!(!at.journal_left());
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_write_puts_back_every_path_written_before_it() {
        use std::os::unix::fs::PermissionsExt;
        let at = Moved::new(
            LineEndings::Verbatim,
            &[("a.txt", THREE), ("sub/b.txt", THREE), ("stale.txt", "s\n")],
            &[
                ("a.txt", MOVED),
                ("sub/b.txt", MOVED),
                ("stale.txt", "s2\n"),
            ],
        );
        write(
            &at.repo,
            &[
                ("a.txt", "ONE\ntwo\nthree\n"),
                ("sub/b.txt", "ONE\ntwo\nthree\n"),
            ],
        );
        let before = (at.status(), run(&at.repo, &["ls-files", "-s"]));
        std::fs::set_permissions(at.repo.join("sub"), std::fs::Permissions::from_mode(0o555))
            .unwrap();
        let merged = at.merge(&["a.txt", "sub/b.txt"], &["stale.txt"]);
        std::fs::set_permissions(at.repo.join("sub"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        assert!(
            matches!(
                merged,
                WorkingCopyMerge::Declined(MergeDeclined::WriteFailed(_))
            ),
            "{merged:?}"
        );
        assert_eq!((at.status(), run(&at.repo, &["ls-files", "-s"])), before);
        assert_eq!(at.text("a.txt"), "ONE\ntwo\nthree\n");
        assert_eq!(at.text("stale.txt"), "s\n");
        assert!(!at.journal_left());
    }

    #[test]
    fn a_journal_left_by_an_interrupted_run_puts_every_path_back() {
        let at = Moved::new(
            LineEndings::Verbatim,
            &[("f.txt", THREE), ("stale.txt", "s\n")],
            &[("f.txt", MOVED), ("stale.txt", "s2\n")],
        );
        write(&at.repo, &[("f.txt", "ONE\ntwo\nthree\n")]);
        let before = (at.status(), run(&at.repo, &["ls-files", "-s"]));
        let mut held = Vec::new();
        for path in ["f.txt", "stale.txt"] {
            held.push(held_now(&at.repo, path).unwrap());
        }
        Journal {
            backup: Some("proj--me/0123".to_owned()),
            held,
        }
        .write(&at.repo)
        .unwrap();
        write(
            &at.repo,
            &[("f.txt", "<<<<<<< half\n"), ("stale.txt", "s2\n")],
        );
        run(&at.repo, &["add", "f.txt", "stale.txt"]);
        assert_eq!(
            interrupted(&at.repo).unwrap(),
            Some(InterruptedMerge {
                backup: Some("proj--me/0123".to_owned()),
                paths: vec!["f.txt".to_owned(), "stale.txt".to_owned()],
            })
        );
        assert_eq!(
            at.merge(&["f.txt"], &["stale.txt"]),
            WorkingCopyMerge::Declined(MergeDeclined::Interrupted)
        );
        put_back(&at.repo).unwrap();
        assert_eq!((at.status(), run(&at.repo, &["ls-files", "-s"])), before);
        assert_eq!(at.text("f.txt"), "ONE\ntwo\nthree\n");
        assert!(!at.journal_left());
    }

    #[test]
    fn a_path_declared_unmergeable_is_left_conflicted_with_the_edit_kept() {
        let at = Moved::new(
            LineEndings::Verbatim,
            &[(".gitattributes", "data.tbl -merge\n"), ("data.tbl", THREE)],
            &[("data.tbl", MOVED)],
        );
        write(&at.repo, &[("data.tbl", "ONE\ntwo\nthree\n")]);
        assert_eq!(
            at.merge(&["data.tbl"], &[]),
            WorkingCopyMerge::Merged {
                clean: vec![],
                took_incoming: vec![],
                conflicted: vec!["data.tbl".to_owned()],
                staged_apart: vec![],
            }
        );
        assert_eq!(at.text("data.tbl"), "ONE\ntwo\nthree\n");
        assert_eq!(at.stages("data.tbl"), ["1", "2", "3"]);
    }

    #[test]
    fn a_path_naming_a_defined_merge_driver_is_declined_by_name() {
        let at = Moved::shaped(
            LineEndings::Verbatim,
            &[
                (".gitattributes", "data.tbl merge=mine\n"),
                ("data.tbl", THREE),
            ],
            &[("data.tbl", MOVED)],
            |repo| {
                run(repo, &["config", "merge.mine.driver", "true"]);
            },
        );
        write(&at.repo, &[("data.tbl", "ONE\ntwo\nthree\n")]);
        assert_eq!(
            at.merge(&["data.tbl"], &[]),
            WorkingCopyMerge::Declined(MergeDeclined::Driver {
                path: "data.tbl".to_owned(),
                driver: "mine".to_owned(),
            })
        );
        assert_eq!(at.text("data.tbl"), "ONE\ntwo\nthree\n");
    }

    #[test]
    fn derived_is_read_from_the_moved_tip_and_its_edit_is_reported_taken() {
        let at = Moved::new(
            LineEndings::Verbatim,
            &[
                (".gitattributes", "NOTES merge=rwv-ours\n"),
                ("NOTES", THREE),
                ("GEN", "base\n"),
            ],
            &[
                (".gitattributes", "GEN merge=rwv-ours\n"),
                ("NOTES", MOVED),
                ("GEN", "moved\n"),
            ],
        );
        write(
            &at.repo,
            &[
                ("NOTES", "ONE hand-written\ntwo\nthree\n"),
                ("GEN", "mine\n"),
            ],
        );
        assert_eq!(
            at.merge(&["GEN", "NOTES"], &[]),
            WorkingCopyMerge::Merged {
                clean: vec!["NOTES".to_owned()],
                took_incoming: vec!["GEN".to_owned()],
                conflicted: vec![],
                staged_apart: vec![],
            }
        );
        assert_eq!(at.text("GEN"), "moved\n");
        assert_eq!(at.text("NOTES"), "ONE hand-written\ntwo\nTHREE\n");
    }

    #[test]
    fn a_delete_on_either_side_is_left_conflicted_and_nothing_is_lost() {
        let at = Moved::shaped(
            LineEndings::Verbatim,
            &[("gone-there.txt", THREE), ("gone-here.txt", THREE)],
            &[("gone-here.txt", MOVED)],
            |repo| std::fs::remove_file(repo.join("gone-there.txt")).unwrap(),
        );
        write(&at.repo, &[("gone-there.txt", "ONE\ntwo\nthree\n")]);
        std::fs::remove_file(at.repo.join("gone-here.txt")).unwrap();
        assert_eq!(
            at.merge(&["gone-here.txt", "gone-there.txt"], &[]),
            WorkingCopyMerge::Merged {
                clean: vec![],
                took_incoming: vec![],
                conflicted: vec!["gone-here.txt".to_owned(), "gone-there.txt".to_owned()],
                staged_apart: vec![],
            }
        );
        assert_eq!(at.text("gone-there.txt"), "ONE\ntwo\nthree\n");
        assert_eq!(at.stages("gone-there.txt"), ["1", "2"]);
        assert_eq!(at.text("gone-here.txt"), MOVED);
        assert_eq!(at.stages("gone-here.txt"), ["1", "3"]);
    }

    #[test]
    fn a_file_only_intended_for_adding_where_the_move_adds_one_is_merged_as_added_on_both_sides() {
        let at = Moved::new(
            LineEndings::Verbatim,
            &[("keep.txt", "k\n")],
            &[("f.txt", "theirs\n")],
        );
        write(&at.repo, &[("f.txt", "mine\n")]);
        run(&at.repo, &["add", "-N", "f.txt"]);
        assert_eq!(
            at.merge(&["f.txt"], &[]),
            WorkingCopyMerge::Merged {
                clean: vec![],
                took_incoming: vec![],
                conflicted: vec!["f.txt".to_owned()],
                staged_apart: vec![],
            }
        );
        assert_eq!(at.stages("f.txt"), ["2", "3"]);
        let text = at.text("f.txt");
        assert!(
            text.contains("mine\n") && text.contains("theirs\n"),
            "{text}"
        );
        run(&at.repo, &["checkout", "--ours", "--", "f.txt"]);
        assert_eq!(at.text("f.txt"), "mine\n");
    }

    #[test]
    fn a_submodule_the_move_adds_is_declined_and_nothing_is_written() {
        let at = Moved::shaped(LineEndings::Verbatim, &[("keep.txt", "k\n")], &[], |repo| {
            let head = run(repo, &["rev-parse", "HEAD"]);
            std::fs::create_dir_all(repo.join("sub")).unwrap();
            run(
                repo,
                &[
                    "update-index",
                    "--add",
                    "--cacheinfo",
                    &format!("160000,{head},sub"),
                ],
            );
        });
        write(&at.repo, &[("sub", "mine\n")]);
        let before = at.status();
        assert_eq!(
            at.merge(&["sub"], &[]),
            WorkingCopyMerge::Declined(MergeDeclined::Submodule(vec!["sub".to_owned()]))
        );
        assert_eq!(at.status(), before);
        assert_eq!(at.text("sub"), "mine\n");
        assert!(!at.journal_left());
    }

    crate::git::under_both_line_endings!(
        a_clean_merge_stands_as_an_edit_on_the_moved_tips_entry,
        a_conflict_is_left_with_its_three_stages,
    );
}
