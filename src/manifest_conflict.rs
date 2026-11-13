//! An `rwv.toml` a merge or a replay left unmerged, read from the sides the
//! VCS recorded for it, and the check of a text merge of it the VCS made
//! with no conflict. Each key of a member, of an integration's table or of
//! `[workweave]`, where every side holds that table, is one entry; a table
//! some side lacks and every other top-level key are each one, whole. An
//! entry the two sides changed, added or removed differently is in conflict.

use crate::manifest::Manifest;
use crate::op_state::OpVerb;
use crate::vcs::{CommitId, ConflictSides, ResolvedRevisionId, Vcs, VcsAction, VcsError};
use anyhow::Context;
use std::path::Path;

/// The side `--continue --keep` takes for every entry in conflict: the
/// branch checked out, or the side it takes in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Side {
    Yours,
    Incoming,
}

impl Side {
    fn word(self) -> &'static str {
        match self {
            Self::Yours => "yours",
            Self::Incoming => "incoming",
        }
    }

    /// During a replay the side the VCS records as the checkout's is the one
    /// being taken in.
    fn is_recorded_theirs(self, replaying: bool) -> bool {
        (self == Self::Incoming) != replaying
    }
}

/// Top-level tables whose keys are each one entry.
const KEYED_TABLES: [&str; 2] = ["repositories", "integrations"];

const TEXT_MERGE_MARK: &str = "<<<<<<< the VCS's text merge of rwv.toml";
const VALUES_MARK: &str = ">>>>>>> the merge of both sides' values";

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct EntryConflict {
    pub(crate) path: Vec<String>,
    pub(crate) ours: Option<toml::Value>,
    pub(crate) theirs: Option<toml::Value>,
}

#[derive(Debug)]
pub(crate) enum Reading {
    /// Every side reads as a manifest. `text_merge` says how a text merge the
    /// VCS made with no conflict differs from the merge of the values, where
    /// the working file holds one.
    Entries {
        conflicts: Vec<EntryConflict>,
        text_merge: Vec<String>,
    },
    Unreadable {
        side: &'static str,
        why: String,
    },
    /// Nothing is left unmerged, and the file as it stands does not read as a
    /// manifest.
    Unparsed(String),
}

struct Sides {
    base: Option<Manifest>,
    ours: Manifest,
    theirs: Manifest,
    ours_text: String,
}

fn read(sides: &ConflictSides) -> Result<Sides, (&'static str, String)> {
    let parse = |side: &'static str, text: &Option<String>| -> Result<Manifest, _> {
        let text = text
            .as_deref()
            .ok_or((side, "it holds no rwv.toml".to_owned()))?;
        Manifest::from_toml_str(text).map_err(|e| (side, format!("{e:#}")))
    };
    Ok(Sides {
        base: sides
            .base
            .is_some()
            .then(|| parse("base", &sides.base))
            .transpose()?,
        ours: parse("ours", &sides.ours)?,
        theirs: parse("theirs", &sides.theirs)?,
        ours_text: sides.ours.clone().unwrap_or_default(),
    })
}

impl Sides {
    /// A side with no file reads as no table.
    fn tables(&self) -> anyhow::Result<[toml::Table; 3]> {
        Ok([
            self.base
                .as_ref()
                .map(Manifest::declared)
                .transpose()?
                .unwrap_or_default(),
            self.ours.declared()?,
            self.theirs.declared()?,
        ])
    }

    fn conflicts(&self) -> anyhow::Result<Vec<EntryConflict>> {
        let [base, ours, theirs] = self.tables()?;
        Ok(entries(&[&base, &ours, &theirs])
            .into_iter()
            .filter_map(|path| {
                let (b, o, t) = (at(&base, &path), at(&ours, &path), at(&theirs, &path));
                (o != t && t != b && o != b).then(|| EntryConflict {
                    path,
                    ours: o.cloned(),
                    theirs: t.cloned(),
                })
            })
            .collect())
    }

    /// Ours, with each entry only theirs changed taken from theirs, and each
    /// entry in conflict too where `take_theirs`.
    fn merged(&self, take_theirs: bool) -> anyhow::Result<Manifest> {
        let [base, ours, theirs] = self.tables()?;
        let mut merged = ours.clone();
        for path in entries(&[&base, &ours, &theirs]) {
            let (b, o, t) = (at(&base, &path), at(&ours, &path), at(&theirs, &path));
            if o == b || (take_theirs && t != b) {
                put(&mut merged, &path, t.cloned());
            }
        }
        let text = toml::to_string(&merged).context("failed to serialize the merged rwv.toml")?;
        Manifest::from_toml_str(&text)
    }

    /// `merged` written as an edit of the recorded checkout side's text.
    fn text(&self, merged: Manifest) -> anyhow::Result<String> {
        Ok(Manifest::edit_text(&self.ours_text, |m| *m = merged)?.0)
    }
}

fn entries(sides: &[&toml::Table]) -> Vec<Vec<String>> {
    let mut keys: Vec<&String> = sides.iter().flat_map(|side| side.keys()).collect();
    keys.sort();
    keys.dedup();
    let mut paths = Vec::new();
    for key in keys {
        let keyed = KEYED_TABLES.contains(&key.as_str())
            && sides
                .iter()
                .all(|side| side.get(key).is_none_or(toml::Value::is_table));
        let tables: Vec<Vec<String>> = if keyed {
            let mut subs: Vec<&String> = sides
                .iter()
                .filter_map(|side| side.get(key).and_then(toml::Value::as_table))
                .flat_map(toml::Table::keys)
                .collect();
            subs.sort();
            subs.dedup();
            subs.into_iter()
                .map(|sub| vec![key.clone(), sub.clone()])
                .collect()
        } else {
            vec![vec![key.clone()]]
        };
        for table in tables {
            match fields(sides, &table) {
                Some(fields) => paths.extend(fields.into_iter().map(|field| {
                    let mut path = table.clone();
                    path.push(field.clone());
                    path
                })),
                None => paths.push(table),
            }
        }
    }
    paths
}

/// The keys of the table at `entry`, a member, an integration or
/// `[workweave]`, where every side holds it as a table; `None` otherwise,
/// and the entry is judged whole.
fn fields<'a>(sides: &[&'a toml::Table], entry: &[String]) -> Option<Vec<&'a String>> {
    if entry.len() == 1 && entry[0] != "workweave" {
        return None;
    }
    let tables = sides
        .iter()
        .map(|side| at(side, entry).and_then(toml::Value::as_table))
        .collect::<Option<Vec<_>>>()?;
    let mut fields: Vec<&String> = tables.iter().flat_map(|table| table.keys()).collect();
    fields.sort();
    fields.dedup();
    Some(fields)
}

fn at<'a>(table: &'a toml::Table, path: &[String]) -> Option<&'a toml::Value> {
    let (last, parents) = path.split_last()?;
    let mut table = table;
    for key in parents {
        table = table.get(key)?.as_table()?;
    }
    table.get(last)
}

fn put(table: &mut toml::Table, path: &[String], value: Option<toml::Value>) {
    let Some((last, parents)) = path.split_last() else {
        return;
    };
    let mut table = table;
    for key in parents {
        let Some(inner) = table
            .entry(key.clone())
            .or_insert_with(|| toml::Value::Table(toml::Table::new()))
            .as_table_mut()
        else {
            return;
        };
        table = inner;
    }
    match value {
        Some(value) => table.insert(last.clone(), value),
        None => table.remove(last),
    };
}

/// How `rwv.toml` in `dir` reads, `None` where it is not unmerged there.
pub(crate) fn reading(vcs: &dyn Vcs, dir: &Path) -> anyhow::Result<Option<Reading>> {
    let Some(recorded) = vcs.conflict_sides(dir, Manifest::FILE_NAME)? else {
        return Ok(None);
    };
    Ok(Some(match read(&recorded) {
        Ok(sides) => Reading::Entries {
            conflicts: sides.conflicts()?,
            text_merge: text_merge_left(dir, &sides)?,
        },
        Err((side, why)) => Reading::Unreadable { side, why },
    }))
}

/// How the text merge a check wrote into `dir`'s working `rwv.toml`, above
/// the merge of the values, differs from it; nothing where the file holds
/// none.
fn text_merge_left(dir: &Path, sides: &Sides) -> anyhow::Result<Vec<String>> {
    let Ok(working) = std::fs::read_to_string(dir.join(Manifest::FILE_NAME)) else {
        return Ok(Vec::new());
    };
    let Some(text_merge) = working
        .strip_prefix(TEXT_MERGE_MARK)
        .and_then(|rest| rest.strip_prefix('\n'))
        .and_then(|rest| rest.split_once("=======\n"))
        .map(|(text_merge, _)| text_merge)
    else {
        return Ok(Vec::new());
    };
    differences(text_merge, sides)
}

/// How `text_merge`, a text merge of `rwv.toml`, differs from the merge of
/// the values of `sides`; nothing where it declares that merge.
fn differences(text_merge: &str, sides: &Sides) -> anyhow::Result<Vec<String>> {
    let head = "rwv.toml: the VCS merged its text with no conflict, into";
    let text_merge = match Manifest::from_toml_str(text_merge) {
        Ok(manifest) => manifest,
        Err(e) => return Ok(vec![format!("{head} a file that does not parse ({e:#})")]),
    };
    let conflicts = sides.conflicts()?;
    let merged = sides.merged(false)?;
    if conflicts.is_empty() && text_merge.declares_same(&merged) {
        return Ok(Vec::new());
    }
    let (text_merge, values) = (text_merge.declared()?, merged.declared()?);
    let mut lines = vec![format!(
        "{head} a manifest that differs from the merge of both sides' values:"
    )];
    for path in entries(&[&text_merge, &values]) {
        if conflicts.iter().any(|c| path.starts_with(&c.path)) {
            continue;
        }
        let (t, v) = (at(&text_merge, &path), at(&values, &path));
        if t != v {
            lines.push(format!(
                "  {}: {} in the text merge, {} in the merge of the values",
                entry_name(&path),
                shown(t),
                shown(v)
            ));
        }
    }
    if !conflicts.is_empty() {
        lines.push("  and both sides changed the entries below differently".to_owned());
    }
    Ok(lines)
}

/// Check `text_merge`, the text the VCS merged `rwv.toml` into in `dir` with
/// no conflict, against the merge of the values of `sides`. Where they
/// differ, record `rwv.toml` as unmerged from `sides` and write both into its
/// working file between conflict markers; return whether it did.
pub(crate) fn check_text_merge(
    vcs: &dyn Vcs,
    dir: &Path,
    sides: &ConflictSides,
    text_merge: &str,
) -> anyhow::Result<bool> {
    if sides.ours == sides.base || sides.theirs == sides.base {
        return Ok(false);
    }
    let Ok(read) = read(sides) else {
        return Ok(false);
    };
    if differences(text_merge, &read)?.is_empty() {
        return Ok(false);
    }
    let values = read.text(read.merged(false)?)?;
    let newline = |text: &str| if text.ends_with('\n') { "" } else { "\n" };
    let working = format!(
        "{TEXT_MERGE_MARK}\n{text_merge}{}=======\n{values}{}{VALUES_MARK}\n",
        newline(text_merge),
        newline(&values)
    );
    vcs.record_conflict(dir, Manifest::FILE_NAME, sides)?;
    crate::durable_file::replace(&dir.join(Manifest::FILE_NAME), working.as_bytes())
        .context("failed to write rwv.toml")?;
    Ok(true)
}

/// The sides of `rwv.toml` a merge of `theirs` into `ours` over `base`
/// records.
pub(crate) fn sides_at(
    vcs: &dyn Vcs,
    dir: &Path,
    base: Option<&CommitId>,
    ours: &CommitId,
    theirs: &CommitId,
) -> ConflictSides {
    let at = |commit: &CommitId| {
        vcs.read_file_at_revision(
            dir,
            &ResolvedRevisionId::from(commit.clone()),
            Path::new(Manifest::FILE_NAME),
        )
        .ok()
    };
    ConflictSides {
        base: base.and_then(at),
        ours: at(ours),
        theirs: at(theirs),
    }
}

/// Check the commit a replay of `dir` stopped at, where its pick left
/// `rwv.toml` merged: committed, at a pause after a pick with no conflict
/// where `paused`, or staged beside another path's conflict. The text is checked against the
/// merge of the values of the commit's parent, the commit the replay built
/// on, and the commit. Return whether it recorded `rwv.toml` unmerged.
pub(crate) fn check_replayed(vcs: &dyn Vcs, dir: &Path, paused: bool) -> anyhow::Result<bool> {
    let Some(picked) = vcs.rebase_replaying(dir)? else {
        return Ok(false);
    };
    let unmerged = vcs.unmerged_paths(dir)?;
    if unmerged.iter().any(|path| path == Manifest::FILE_NAME) {
        return Ok(false);
    }
    let Some(text_merge) = vcs.staged_file(dir, Manifest::FILE_NAME)? else {
        return Ok(false);
    };
    let head = vcs.head_revision(dir)?.id().clone();
    let built_on = if paused {
        match vcs.parents(dir, &head)?.into_iter().next() {
            Some(parent) => parent,
            None => return Ok(false),
        }
    } else {
        head
    };
    let base = vcs.parents(dir, &picked)?.into_iter().next();
    let sides = sides_at(vcs, dir, base.as_ref(), &built_on, &picked);
    check_text_merge(vcs, dir, &sides, &text_merge)
}

/// Whether `text_merge`, the text the VCS merged `rwv.toml` into with no
/// conflict, differs from the merge of the values of `sides`.
pub(crate) fn text_merge_differs(sides: &ConflictSides, text_merge: &str) -> anyhow::Result<bool> {
    if sides.ours == sides.base || sides.theirs == sides.base {
        return Ok(false);
    }
    match read(sides) {
        Ok(read) => Ok(!differences(text_merge, &read)?.is_empty()),
        Err(_) => Ok(false),
    }
}

/// Check the merge in flight in `dir`, where it left `rwv.toml` merged,
/// against the merge of the values of its sides; return whether it recorded
/// `rwv.toml` unmerged.
pub(crate) fn check_merge_in_flight(vcs: &dyn Vcs, dir: &Path) -> anyhow::Result<bool> {
    let Some(theirs) = vcs.merge_in_flight(dir)? else {
        return Ok(false);
    };
    if vcs
        .unmerged_paths(dir)?
        .iter()
        .any(|path| path == Manifest::FILE_NAME)
    {
        return Ok(false);
    }
    let Some(text_merge) = vcs.staged_file(dir, Manifest::FILE_NAME)? else {
        return Ok(false);
    };
    let ours = vcs.head_revision(dir)?.id().clone();
    let base = vcs.merge_bases(dir, &ours, &theirs)?.into_iter().next();
    let sides = sides_at(vcs, dir, base.as_ref(), &ours, &theirs);
    check_text_merge(vcs, dir, &sides, &text_merge)
}

/// Replay the project repo in `dir` onto `onto`, pausing after each commit
/// that changes `rwv.toml` where `onto` changed it too, then drive it with
/// [`drive_replay`].
pub(crate) fn replay(
    vcs: &dyn Vcs,
    dir: &Path,
    onto: &ResolvedRevisionId,
    derived: crate::vcs::DerivedContentPolicy,
) -> anyhow::Result<Result<(), VcsError>> {
    let head = vcs.head_revision(dir)?.id().clone();
    let base = vcs.merge_bases(dir, &head, onto.id())?.into_iter().next();
    let sides = sides_at(vcs, dir, base.as_ref(), onto.id(), onto.id());
    let outcome = if sides.base == sides.ours {
        vcs.rebase(dir, onto, onto, derived)
    } else {
        vcs.rebase_pausing(dir, onto, onto, derived, Manifest::FILE_NAME)
    };
    drive_replay(vcs, dir, outcome, derived, None)
}

/// Drive the project replay in `dir` from `outcome` to its end or to a stop.
/// At each pause whose commit holds `rwv.toml` as the pick left it, the text
/// is checked and a right one goes on; at a stop on a conflict, a pick other
/// than `resumed`, the one a continue went on from, is checked too. A check
/// that records `rwv.toml` unmerged stops the replay there.
pub(crate) fn drive_replay(
    vcs: &dyn Vcs,
    dir: &Path,
    mut outcome: Result<(), VcsError>,
    derived: crate::vcs::DerivedContentPolicy,
    resumed: Option<CommitId>,
) -> anyhow::Result<Result<(), VcsError>> {
    let stopped = || VcsError::RebaseConflict {
        repo: dir.to_path_buf(),
        op: crate::vcs::ConflictOp::Rebase,
    };
    loop {
        let replaying = vcs.rebase_replaying(dir)?;
        match outcome {
            Ok(()) if replaying.is_none() => return Ok(Ok(())),
            Ok(()) => {
                crate::test_pause::point("replay-paused");
                let head = vcs.head_revision(dir)?;
                let committed = vcs
                    .read_file_at_revision(dir, &head, Path::new(Manifest::FILE_NAME))
                    .ok();
                let as_picked = vcs.staged_file(dir, Manifest::FILE_NAME)? == committed;
                if as_picked && check_replayed(vcs, dir, true)? {
                    return Ok(Err(stopped()));
                }
                outcome = vcs.rebase_continue(dir, derived);
            }
            Err(VcsError::RebaseConflict { .. }) => {
                if replaying.is_some() && replaying != resumed {
                    check_replayed(vcs, dir, false)?;
                }
                return Ok(Err(stopped()));
            }
            Err(e) => return Ok(Err(e)),
        }
    }
}

/// Go on with the project replay stopped in `dir`, from a pause as from a
/// conflict, through [`drive_replay`].
pub(crate) fn continue_replay(
    vcs: &dyn Vcs,
    dir: &Path,
    derived: crate::vcs::DerivedContentPolicy,
) -> anyhow::Result<Result<(), VcsError>> {
    let resumed = vcs.rebase_replaying(dir)?;
    let outcome = if vcs.rebase_paused(dir)? {
        Ok(())
    } else {
        vcs.rebase_continue(dir, derived)
    };
    drive_replay(vcs, dir, outcome, derived, resumed)
}

fn kept_lines(vcs: &dyn Vcs, dir: &Path, kept: &crate::vcs::BackupWitness) -> [String; 2] {
    [
        format!(
            "rwv.toml as it stood before is kept in backup {}; to read it:",
            kept.backup().name
        ),
        format!(
            "  {}",
            vcs.hint(VcsAction::ShowKeptFile)
                .replace("<checkout>", &crate::workspace::quoted(dir))
                .replace("<revision>", kept.backup().tip.id().as_str())
                .replace("<paths>", Manifest::FILE_NAME)
        ),
    ]
}

/// At the `typed` continue of the operation stopped in the active project,
/// `keep` naming the side to take for every entry in conflict: where
/// `rwv.toml` is unmerged, write and stage the merge of its sides' values,
/// keeping the checkout's edits as a backup first, or stage a working file
/// that already declares it. Refuse where an entry is in conflict and no
/// side is named, where the working file holds another manifest, and where
/// the merge or replay in flight is not the operation's.
pub(crate) fn continue_at_stop(
    ctx: &crate::workspace::WorkspaceContext,
    typed: OpVerb,
    keep: Option<Side>,
) -> anyhow::Result<()> {
    let Some(project) = ctx.active_project() else {
        return Ok(());
    };
    let dir = crate::workspace::project_dir(ctx.active_path(), project);
    let vcs = crate::vcs::project_vcs();
    let vcs = vcs.as_ref();
    if !vcs.is_repo(&dir) {
        return Ok(());
    }
    let Some(record) = crate::op_state::read_owner(ctx.active_path())? else {
        crate::op_state::resume(ctx.active_path())?;
        return Ok(());
    };
    if record.verb == OpVerb::CherryPick {
        return Ok(());
    }
    let workspace = ctx.active_path();
    let resume = crate::op_state::resume_command_at(record.verb, workspace);
    let refused = |head: &str, lines: Vec<String>| {
        let mut text = format!(
            "{head} in {} at {}:\n",
            crate::workspace::project_rel_path(project),
            dir.display()
        );
        for line in lines {
            text.push_str(&line);
            text.push('\n');
        }
        text.push_str(&format!(
            "then run `{resume}`, or `{}` to put everything back as it was",
            crate::op_state::abort_command_at(workspace)
        ));
        crate::refusal::refusal(crate::refusal::RefusalKind::OpParked, text)
    };
    let path = dir.join(Manifest::FILE_NAME);
    let working = std::fs::read_to_string(&path).ok();
    let Some(recorded) = vcs.conflict_sides(&dir, Manifest::FILE_NAME)? else {
        if vcs.rebase_paused(&dir)? {
            let head = vcs.head_revision(&dir)?;
            let committed = vcs
                .read_file_at_revision(&dir, &head, Path::new(Manifest::FILE_NAME))
                .ok();
            let as_picked = vcs.staged_file(&dir, Manifest::FILE_NAME)? == committed;
            if !(as_picked && check_replayed(vcs, &dir, true)?) {
                return Ok(());
            }
            return Err(refused(
                "rwv.toml is still in conflict",
                stop_lines(vcs, &dir, &resume),
            ));
        }
        if let Some(Err(e)) = working.as_deref().map(Manifest::from_toml_str) {
            let reading = Reading::Unparsed(format!("{e:#}"));
            return Err(refused(
                "rwv.toml is still in conflict",
                reading_lines(vcs, &dir, &reading, Some(&resume)),
            ));
        }
        if let (Some(side), true) = (keep, record.verb == typed) {
            eprintln!(
                "{}: rwv.toml is not unmerged, so `--keep {}` takes nothing",
                crate::op_state::resume_command(typed),
                side.word()
            );
        }
        return Ok(());
    };
    let sides = match read(&recorded) {
        Ok(sides) => sides,
        Err((side, why)) => {
            let reading = Reading::Unreadable { side, why };
            return Err(refused(
                "rwv.toml is still in conflict",
                reading_lines(vcs, &dir, &reading, Some(&resume)),
            ));
        }
    };
    let conflicts = sides.conflicts()?;
    if record.verb != typed || (keep.is_none() && !conflicts.is_empty()) {
        let reading = Reading::Entries {
            text_merge: text_merge_left(&dir, &sides)?,
            conflicts,
        };
        return Err(refused(
            "rwv.toml is still in conflict",
            reading_lines(vcs, &dir, &reading, Some(&resume)),
        ));
    }
    if !in_flight_is_the_operations(vcs, &dir, &record)? {
        return Err(refused(
            "the merge or replay in progress is not the one the stopped operation started",
            vec![format!(
                "conclude or abandon it as you started it, in {}",
                dir.display()
            )],
        ));
    }
    let replaying = vcs.rebase_in_flight(&dir)?.is_some();
    let merged = sides.merged(keep.is_some_and(|side| side.is_recorded_theirs(replaying)))?;
    let done = crate::op_state::resume_command(typed);
    if let Some(working) = working
        .as_deref()
        .and_then(|text| Manifest::from_toml_str(text).ok())
    {
        if working.declares_same(&merged) {
            vcs.stage_paths(&dir, &[Manifest::FILE_NAME])?;
            eprintln!("{done}: rwv.toml as it stands declares the merge of both sides; staged it");
            return Ok(());
        }
        return Err(refused(
            "rwv.toml as it stands reads as a manifest other than the merge of both sides",
            vec![
                "to keep it as it stands, stage it, then run the continue again:".to_owned(),
                format!("  {}", mark_resolved(vcs, &dir)),
            ],
        ));
    }
    let text = sides.text(merged)?;
    let kept = vcs.backup_edits(&dir)?;
    crate::durable_file::replace(&path, text.as_bytes()).context("failed to write rwv.toml")?;
    vcs.stage_paths(&dir, &[Manifest::FILE_NAME])?;
    let took = match keep {
        Some(side) if conflicts.is_empty() => format!(
            "; no entry was in conflict, so `--keep {}` took nothing",
            side.word()
        ),
        Some(side) => format!(", taking {} for each entry in conflict", side.word()),
        None => String::new(),
    };
    eprintln!("{done}: wrote and staged the merge of both sides of rwv.toml{took}");
    for line in kept.iter().flat_map(|kept| kept_lines(vcs, &dir, kept)) {
        eprintln!("{line}");
    }
    Ok(())
}

/// Whether the merge or replay in flight in `dir` takes in a commit the
/// operation `record` journaled taking in there.
fn in_flight_is_the_operations(
    vcs: &dyn Vcs,
    dir: &Path,
    record: &crate::op_state::OwnerRecord,
) -> anyhow::Result<bool> {
    let incoming = match vcs.merge_in_flight(dir)? {
        Some(merging) => merging,
        None => match vcs.rebase_in_flight(dir)? {
            Some(onto) => onto,
            None => return Ok(true),
        },
    };
    let project = crate::manifest::RepoKey::Project;
    Ok(record.take_in.as_ref().is_some_and(|take_in| {
        take_in
            .merges
            .iter()
            .any(|m| m.repo == project && m.source == incoming)
            || take_in
                .rebases
                .iter()
                .any(|r| r.repo == project && r.onto == incoming)
    }))
}

fn mark_resolved(vcs: &dyn Vcs, dir: &Path) -> String {
    vcs.hint(VcsAction::MarkResolved)
        .replace("<checkout>", &crate::workspace::quoted(dir))
        .replace("<paths>", Manifest::FILE_NAME)
}

fn entry_name(path: &[String]) -> String {
    match path {
        [table, member] if table == "repositories" => member.clone(),
        [table, member, field] if table == "repositories" => format!("{member} {field}"),
        [table, name] if table == "integrations" => format!("[integrations.{name}]"),
        [table, name, field] if table == "integrations" => {
            format!("[integrations.{name}] {field}")
        }
        [table, field] if table == "workweave" => format!("[workweave] {field}"),
        [table] if table == "workweave" => "[workweave]".to_owned(),
        [key] => key.clone(),
        _ => path.join("."),
    }
}

/// The table holding the entry at `path` where the entry is one of its keys.
fn keyed_in(path: &[String]) -> Option<&[String]> {
    match path {
        [_, _, _] => Some(&path[..2]),
        [table, _] if table == "workweave" => Some(&path[..1]),
        _ => None,
    }
}

/// The lines a stop prints for an unmerged `rwv.toml` in `dir`: each entry in
/// conflict with both sides' values, and the ways on. `resume` is the
/// continue of the operation stopped there, spelled to run against its
/// workspace.
pub(crate) fn stop_lines(vcs: &dyn Vcs, dir: &Path, resume: &str) -> Vec<String> {
    match reading(vcs, dir) {
        Ok(Some(reading)) => reading_lines(vcs, dir, &reading, Some(resume)),
        _ => Vec::new(),
    }
}

/// `resume`: `None` where no operation of rwv's is stopped in `dir`.
pub(crate) fn reading_lines(
    vcs: &dyn Vcs,
    dir: &Path,
    reading: &Reading,
    resume: Option<&str>,
) -> Vec<String> {
    let by_hand = "edit rwv.toml into what you mean, then stage it";
    let replaying = matches!(vcs.rebase_in_flight(dir), Ok(Some(_)));
    let label = |side: &str| match (side, replaying) {
        ("ours", false) | ("theirs", true) => "yours",
        ("ours", true) | ("theirs", false) => "incoming",
        _ => "common base",
    };
    let mut lines = Vec::new();
    match reading {
        Reading::Unreadable { side, why } => lines.push(format!(
            "rwv.toml's {} side does not read as a manifest ({why}); {by_hand}:",
            label(side)
        )),
        Reading::Unparsed(why) => lines.push(format!(
            "rwv.toml does not read as a manifest ({why}); {by_hand}:"
        )),
        Reading::Entries {
            conflicts,
            text_merge,
        } => {
            lines.extend(text_merge.iter().cloned());
            let mut heading: Option<&[String]> = None;
            for conflict in conflicts {
                let [yours, incoming] = yours_and_incoming(conflict, replaying);
                let shown_pair = format!("{} in yours, {} incoming", shown(yours), shown(incoming));
                match keyed_in(&conflict.path) {
                    Some(table) => {
                        if heading != Some(table) {
                            heading = Some(table);
                            lines.push(format!(
                                "rwv.toml entry {} is in conflict:",
                                entry_name(table)
                            ));
                        }
                        let field = conflict.path.last().map_or("", String::as_str);
                        lines.push(format!("  {field}: {shown_pair}"));
                    }
                    None => {
                        heading = None;
                        lines.push(format!(
                            "rwv.toml entry {} is in conflict: {shown_pair}",
                            entry_name(&conflict.path)
                        ));
                        if let [Some(toml::Value::Table(yours)), Some(toml::Value::Table(theirs))] =
                            [yours, incoming]
                        {
                            let mut keys: Vec<&String> =
                                yours.keys().chain(theirs.keys()).collect();
                            keys.sort();
                            keys.dedup();
                            for key in keys.into_iter().filter(|k| yours.get(*k) != theirs.get(*k))
                            {
                                lines.push(format!(
                                    "  {key}: {} in yours, {} incoming",
                                    shown(yours.get(key)),
                                    shown(theirs.get(key))
                                ));
                            }
                        }
                    }
                }
            }
            if !conflicts.is_empty() {
                lines.push(format!(
                    "(yours: the side of the branch checked out in {}; incoming: the side it \
                     takes in)",
                    dir.display()
                ));
            }
            match resume {
                Some(resume) if conflicts.is_empty() => lines.extend([
                    format!(
                        "rwv.toml: no entry is in conflict; `{resume}` writes and stages the \
                         merge of both sides"
                    ),
                    format!("or {by_hand}:"),
                ]),
                Some(resume) => {
                    lines.push(
                        "to take one side for every entry in conflict, and write and stage the \
                         merge of both sides:"
                            .to_owned(),
                    );
                    for side in [Side::Yours, Side::Incoming] {
                        lines.push(format!("  {resume} --keep {}", side.word()));
                    }
                    lines.push(format!("or {by_hand}:"));
                }
                None => lines.push(format!("rwv.toml: {by_hand}:")),
            }
        }
    }
    if lines.last().is_some_and(|line| line.ends_with(':')) {
        lines.push(format!("  {}", mark_resolved(vcs, dir)));
    }
    lines
}

fn yours_and_incoming(conflict: &EntryConflict, replaying: bool) -> [Option<&toml::Value>; 2] {
    if replaying {
        [conflict.theirs.as_ref(), conflict.ours.as_ref()]
    } else {
        [conflict.ours.as_ref(), conflict.theirs.as_ref()]
    }
}

fn shown(value: Option<&toml::Value>) -> String {
    match value {
        Some(toml::Value::Table(_)) => "set".to_owned(),
        Some(value) => value.to_string(),
        None => "absent".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{RepoPath, Role};

    fn member(name: &str, branch: &str) -> String {
        format!(
            "[repositories.\"local/{name}\"]\ntype = \"git\"\nurl = \"https://example.com/{name}.git\"\nbranch = \"{branch}\"\nrole = \"owned\"\n"
        )
    }

    fn sides(base: &str, ours: &str, theirs: &str) -> Sides {
        read(&ConflictSides {
            base: Some(base.to_owned()),
            ours: Some(ours.to_owned()),
            theirs: Some(theirs.to_owned()),
        })
        .unwrap()
    }

    fn paths(sides: &Sides) -> Vec<String> {
        sides
            .conflicts()
            .unwrap()
            .iter()
            .map(|c| entry_name(&c.path))
            .collect()
    }

    fn branch_of(merged: &Manifest, name: &str) -> String {
        merged
            .get_entry(&RepoPath::new(format!("local/{name}")).unwrap())
            .unwrap()
            .branch
            .local_counterpart()
            .as_str()
            .to_owned()
    }

    #[test]
    fn different_fields_of_one_member_do_not_conflict_and_merge_both() {
        let base = member("lib", "main");
        let ours = base.replace("role = \"owned\"", "role = \"fork\"");
        let theirs = member("lib", "dev");
        let sides = sides(&base, &ours, &theirs);
        assert!(paths(&sides).is_empty());
        let merged = sides.merged(false).unwrap();
        let lib = merged
            .get_entry(&RepoPath::new("local/lib").unwrap())
            .unwrap();
        assert_eq!(lib.role, Role::Fork);
        assert_eq!(branch_of(&merged, "lib"), "dev");
    }

    #[test]
    fn one_field_both_sides_changed_differently_conflicts_alone() {
        let base = member("lib", "main");
        let ours = member("lib", "x").replace("role = \"owned\"", "role = \"fork\"");
        let theirs = member("lib", "y");
        assert_eq!(paths(&sides(&base, &ours, &theirs)), ["local/lib branch"]);
    }

    #[test]
    fn keeping_a_side_takes_it_for_each_entry_in_conflict_and_the_other_side_elsewhere() {
        let base = format!("{}{}", member("a", "main"), member("b", "main"));
        let ours = format!(
            "{}{}",
            member("a", "x"),
            member("b", "main").replace("role = \"owned\"", "role = \"fork\"")
        );
        let theirs = format!("{}{}", member("a", "y"), member("b", "z"));
        let sides = sides(&base, &ours, &theirs);
        let kept_ours = sides.merged(false).unwrap();
        let kept_theirs = sides.merged(true).unwrap();
        assert_eq!(branch_of(&kept_ours, "a"), "x");
        assert_eq!(branch_of(&kept_theirs, "a"), "y");
        for merged in [&kept_ours, &kept_theirs] {
            assert_eq!(branch_of(merged, "b"), "z");
            let b = merged
                .get_entry(&RepoPath::new("local/b").unwrap())
                .unwrap();
            assert_eq!(b.role, Role::Fork);
        }
    }

    #[test]
    fn a_member_both_sides_added_differently_conflicts_whole() {
        let base = member("a", "main");
        let ours = format!("{base}{}", member("lib", "x"));
        let theirs = format!(
            "{base}{}",
            member("lib", "x").replace("role = \"owned\"", "role = \"fork\"")
        );
        assert_eq!(paths(&sides(&base, &ours, &theirs)), ["local/lib"]);
    }

    #[test]
    fn members_changed_on_one_side_each_do_not_conflict_and_merge_both() {
        let base = format!("{}{}", member("a", "main"), member("b", "main"));
        let ours = format!("{}{}", member("a", "x"), member("b", "main"));
        let theirs = format!("{}{}", member("a", "main"), member("b", "y"));
        let sides = sides(&base, &ours, &theirs);
        assert!(paths(&sides).is_empty());
        let merged = sides.merged(false).unwrap();
        assert_eq!(
            (branch_of(&merged, "a"), branch_of(&merged, "b")),
            ("x".to_owned(), "y".to_owned())
        );
    }

    #[test]
    fn removed_against_changed_conflicts_either_way_and_keeping_a_side_keeps_its_outcome() {
        let base = format!("{}{}", member("a", "main"), member("b", "main"));
        let changed = format!("{}{}", member("a", "x"), member("b", "main"));
        let removed = member("b", "main");
        assert_eq!(paths(&sides(&base, &changed, &removed)), ["local/a"]);
        let theirs_changed = sides(&base, &removed, &changed);
        assert_eq!(paths(&theirs_changed), ["local/a"]);
        let a = RepoPath::new("local/a").unwrap();
        assert!(!theirs_changed.merged(false).unwrap().contains_repo(&a));
        assert_eq!(branch_of(&theirs_changed.merged(true).unwrap(), "a"), "x");
    }

    #[test]
    fn workweave_keys_and_history_are_entries() {
        let base = format!("{}[workweave]\ncopy = [\"b\"]\n", member("a", "main"));
        let ours = format!(
            "history = \"merge\"\n{}[workweave]\ncopy = [\"o\"]\n",
            member("a", "main")
        );
        let theirs = format!(
            "history = \"linear\"\n{}[workweave]\ncopy = [\"t\"]\nlink = [\"l\"]\n",
            member("a", "main")
        );
        let sides = sides(&base, &ours, &theirs);
        assert_eq!(paths(&sides), ["history", "[workweave] copy"]);
        let merged = sides.merged(true).unwrap();
        let workweave = merged.workweave.unwrap();
        assert_eq!(workweave.copy, ["t"]);
        assert_eq!(workweave.link, ["l"]);
    }

    #[test]
    fn a_side_that_is_not_a_manifest_is_named() {
        let base = member("a", "main");
        let err = read(&ConflictSides {
            base: Some(base.clone()),
            ours: Some("<<<<<<< nope".to_owned()),
            theirs: Some(base),
        })
        .err()
        .unwrap();
        assert_eq!(err.0, "ours");
    }

    #[test]
    fn a_text_merge_that_moved_a_setting_differs_and_one_that_did_not_does_not() {
        let base = format!("{}{}", member("a", "main"), member("b", "main"));
        let ours = format!(
            "{}history = \"merge\"\n{}",
            member("a", "main"),
            member("b", "main")
        );
        let theirs = format!("{}{}", member("a", "main"), member("b", "x"));
        let sides = sides(&base, &ours, &theirs);
        let right = format!(
            "{}history = \"merge\"\n{}",
            member("a", "main"),
            member("b", "x")
        );
        assert!(differences(&right, &sides).unwrap().is_empty());
        let moved = format!(
            "{}{}history = \"merge\"\n",
            member("a", "main"),
            member("b", "x")
        );
        let lines = differences(&moved, &sides).unwrap();
        assert!(lines.iter().any(|line| line.contains("local/a history")));
        assert!(lines.iter().any(|line| line.contains("local/b history")));
        let duplicated = format!("{right}{}", member("b", "x"));
        assert!(differences(&duplicated, &sides).unwrap()[0].contains("does not parse"));
    }
}
