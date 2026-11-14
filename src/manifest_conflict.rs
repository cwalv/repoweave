//! An `rwv.toml` a merge or a replay left unmerged, read from the sides the
//! VCS recorded for it, and the check of a text merge of it the VCS made
//! with no conflict. Each key of a member, of an integration's table or of
//! `[workweave]`, where every side holds that table, is one entry; a table
//! some side lacks and every other top-level key are each one, whole. An
//! entry the two sides changed, added or removed differently is in conflict.

use crate::manifest::Manifest;
use crate::op_state::OpVerb;
use crate::vcs::{CommitId, ConflictSides, ResolvedRevisionId, Vcs, VcsAction};
use anyhow::Context;
use std::path::Path;

/// What `--continue --keep` keeps: for every entry in conflict the branch
/// checked out's side or the side it takes in, or, where the check stopped a
/// text merge that differs from the merge of the values, that text merge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Side {
    Yours,
    Incoming,
    TextMerge,
}

impl Side {
    fn word(self) -> &'static str {
        match self {
            Self::Yours => "yours",
            Self::Incoming => "incoming",
            Self::TextMerge => "text-merge",
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

const BY_HAND: &str = "edit rwv.toml into what you mean, then stage it";

pub(crate) const WRITE_VALUES: &str =
    "write the merge of both sides' values in place of the text merge";
pub(crate) const KEEP_TEXT_MERGE: &str = "keep the text merge as the VCS made it";
pub(crate) const STAGE_OWN: &str = "edit rwv.toml into what you mean and stage it, then continue";

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
    let Some(text_merge) = text_merge_written(&working) else {
        return Ok(Vec::new());
    };
    differences(text_merge, sides)
}

/// The text merge a check wrote into `working`, above the merge of the
/// values.
fn text_merge_written(working: &str) -> Option<&str> {
    working
        .strip_prefix(TEXT_MERGE_MARK)
        .and_then(|rest| rest.strip_prefix('\n'))
        .and_then(|rest| rest.split_once("=======\n"))
        .map(|(text_merge, _)| text_merge)
}

/// How `text_merge`, a text merge of `rwv.toml`, differs from the merge of
/// the values of `sides`; nothing where it declares that merge, or does not
/// parse, which the stop for an unparseable result answers.
fn differences(text_merge: &str, sides: &Sides) -> anyhow::Result<Vec<String>> {
    let Ok(text_merge) = Manifest::from_toml_str(text_merge) else {
        return Ok(Vec::new());
    };
    let conflicts = sides.conflicts()?;
    let merged = sides.merged(false)?;
    if conflicts.is_empty() && text_merge.declares_same(&merged) {
        return Ok(Vec::new());
    }
    let mut lines = listing(&text_merge.declared()?, &merged.declared()?, &conflicts);
    if !conflicts.is_empty() {
        lines.push("  and both sides changed the entries below differently".to_owned());
    }
    Ok(lines)
}

/// The lines naming each entry, outside `conflicts`, where `text_merge` and
/// `values` differ.
fn listing(
    text_merge: &toml::Table,
    values: &toml::Table,
    conflicts: &[EntryConflict],
) -> Vec<String> {
    let mut lines = vec![
        "rwv.toml: the VCS merged its text with no conflict, into a manifest that differs from \
         the merge of both sides' values:"
            .to_owned(),
    ];
    for path in entries(&[text_merge, values]) {
        if conflicts.iter().any(|c| path.starts_with(&c.path)) {
            continue;
        }
        let (t, v) = (at(text_merge, &path), at(values, &path));
        if t != v {
            lines.push(format!(
                "  {}: {} in the text merge, {} in the merge of the values",
                entry_name(&path),
                shown(t),
                shown(v)
            ));
        }
    }
    lines
}

/// The sides of `sides`, read, where `text_merge` differs from the merge of
/// their values and both sides changed `rwv.toml`.
fn differing(sides: &ConflictSides, text_merge: &str) -> anyhow::Result<Option<Sides>> {
    if sides.ours == sides.base || sides.theirs == sides.base {
        return Ok(None);
    }
    let Ok(read) = read(sides) else {
        return Ok(None);
    };
    Ok((!differences(text_merge, &read)?.is_empty()).then_some(read))
}

/// How the replayed tip's `rwv.toml`, `tip`, differs from the merge of the
/// values of `sides`: the merge base, the commit replayed onto, and the
/// branch's tip before the replay.
pub(crate) struct TipDiffers {
    pub(crate) lines: Vec<String>,
    /// The merge of the values written as an edit of `tip`.
    pub(crate) values: String,
}

/// Judge `tip` against the merge of the values of `sides`; a tip that does
/// not parse is not judged.
pub(crate) fn check_tip(sides: &ConflictSides, tip: &str) -> anyhow::Result<Option<TipDiffers>> {
    if sides.ours == sides.base || sides.theirs == sides.base {
        return Ok(None);
    }
    let (Ok(read), Ok(at_tip)) = (read(sides), Manifest::from_toml_str(tip)) else {
        return Ok(None);
    };
    let merged = read.merged(false)?;
    if at_tip.declares_same(&merged) {
        return Ok(None);
    }
    Ok(Some(TipDiffers {
        lines: listing(&at_tip.declared()?, &merged.declared()?, &[]),
        values: Manifest::edit_text(tip, |m| *m = merged)?.0,
    }))
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
    let Some(read) = differing(sides, text_merge)? else {
        return Ok(false);
    };
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

/// Whether `text_merge`, the text the VCS merged `rwv.toml` into with no
/// conflict, differs from the merge of the values of `sides`.
pub(crate) fn text_merge_differs(sides: &ConflictSides, text_merge: &str) -> anyhow::Result<bool> {
    Ok(differing(sides, text_merge)?.is_some())
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
    let done = crate::op_state::resume_command(typed);
    let Some(recorded) = vcs.conflict_sides(&dir, Manifest::FILE_NAME)? else {
        if record.verb != typed {
            return Ok(());
        }
        if let Some((rebase, differs)) =
            crate::project_merge::replayed_tip_differs(vcs, &dir, workspace)?
        {
            return settle_tip(vcs, &dir, workspace, (rebase, differs), keep, &done);
        }
        match keep {
            Some(Side::TextMerge) => {
                return Err(refused(
                    "no text merge of rwv.toml is stopped for a check here",
                    vec![format!(
                        "`--keep text-merge` keeps a text merge the check stopped; run \
                         `{resume}` without it"
                    )],
                ))
            }
            Some(side) => eprintln!(
                "{done}: rwv.toml is not unmerged, so `--keep {}` takes nothing",
                side.word()
            ),
            None => {}
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
    if keep == Some(Side::TextMerge) {
        let Some(text_merge) = working.as_deref().and_then(text_merge_written) else {
            return Err(refused(
                "rwv.toml is in conflict in the VCS's own text merge, so there is no text merge \
                 to keep",
                vec![
                    format!(
                    "take a side with `{resume} --keep yours` or `--keep incoming`, or {BY_HAND}:"
                ),
                    format!("  {}", mark_resolved(vcs, &dir)),
                ],
            ));
        };
        crate::durable_file::replace(&path, text_merge.as_bytes())
            .context("failed to write rwv.toml")?;
        vcs.stage_paths(&dir, &[Manifest::FILE_NAME])?;
        eprintln!("{done}: kept and staged the text merge of rwv.toml as the VCS made it");
        return Ok(());
    }
    let replaying = vcs.rebase_in_flight(&dir)?.is_some();
    let merged = sides.merged(keep.is_some_and(|side| side.is_recorded_theirs(replaying)))?;
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
    let (text, dropped) = match working.as_deref().and_then(text_merge_written) {
        Some(text_merge) => (Manifest::edit_text(text_merge, |m| *m = merged)?.0, None),
        None => (
            sides.text(merged)?,
            Some(if replaying { "yours" } else { "incoming" }),
        ),
    };
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
    if let Some(dropped) = dropped {
        eprintln!(
            "comments and layout the {dropped} side changed in rwv.toml are not carried; the \
             file as the VCS left it is in the backup below"
        );
    }
    for line in kept.iter().flat_map(|kept| kept_lines(vcs, &dir, kept)) {
        eprintln!("{line}");
    }
    Ok(())
}

/// At the stop where the replayed tip's `rwv.toml` in `dir` differs from the
/// merge of the values: a staged `rwv.toml` is the operator's and is taken
/// whole; `--keep text-merge` keeps the tip as the VCS made it; otherwise
/// the merge of the values is written and staged, the file as it stood kept
/// in a backup first. Each settles the tip for the rest of the operation.
fn settle_tip(
    vcs: &dyn Vcs,
    dir: &Path,
    workspace: &Path,
    (rebase, differs): (crate::op_state::Rebasing, TipDiffers),
    keep: Option<Side>,
    done: &str,
) -> anyhow::Result<()> {
    let head = vcs.head_revision(dir)?;
    let committed = vcs
        .read_file_at_revision(dir, &head, Path::new(Manifest::FILE_NAME))
        .ok();
    let staged = vcs.staged_file(dir, Manifest::FILE_NAME)?;
    match keep {
        _ if staged != committed => {}
        Some(Side::TextMerge) => {
            eprintln!("{done}: kept the replayed tip's rwv.toml as the VCS merged it");
        }
        Some(side) => {
            eprintln!(
                "{done}: no entry of rwv.toml is in conflict, so `--keep {}` takes nothing",
                side.word()
            );
            return Ok(());
        }
        None => {
            let kept = vcs.backup_edits(dir)?;
            crate::durable_file::replace(&dir.join(Manifest::FILE_NAME), differs.values.as_bytes())
                .context("failed to write rwv.toml")?;
            vcs.stage_paths(dir, &[Manifest::FILE_NAME])?;
            eprintln!(
                "{done}: wrote and staged the merge of both sides' values in place of the text \
                 merge of rwv.toml"
            );
            for line in kept.iter().flat_map(|kept| kept_lines(vcs, dir, kept)) {
                eprintln!("{line}");
            }
        }
    }
    crate::op_state::journal_tip_settled(workspace, rebase)
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
    let by_hand = BY_HAND;
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
                Some(resume) if conflicts.is_empty() && !text_merge.is_empty() => lines.extend([
                    format!("to {WRITE_VALUES}:"),
                    format!("  {resume}"),
                    format!("to {KEEP_TEXT_MERGE}:"),
                    format!("  {resume} --keep text-merge"),
                    format!("or {STAGE_OWN}:"),
                ]),
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
    fn an_integrations_keys_are_each_an_entry() {
        let table = |enabled: bool, files: &str| {
            format!(
                "{}[integrations.static-files]\nenabled = {enabled}\nfiles = [{files}]\n",
                member("a", "main")
            )
        };
        let sides = sides(
            &table(true, "\"a\""),
            &table(false, "\"a\""),
            &table(true, "\"b\""),
        );
        assert!(paths(&sides).is_empty());
        let merged = toml::to_string(&sides.merged(false).unwrap().declared().unwrap()).unwrap();
        assert!(merged.contains("enabled = false"), "{merged}");
        assert!(merged.contains("\"b\""), "{merged}");
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
        assert!(differences(&duplicated, &sides).unwrap().is_empty());
    }
}
