//! An `rwv.toml` a merge or a replay left unmerged, read from the sides the
//! VCS recorded for it, and the check of a text merge of it the VCS made
//! with no conflict. Each key of a member, of an integration's table or of
//! `[workweave]`, where every side holds that table, is one entry; a table
//! some side lacks and every other top-level key are each one, whole. An
//! entry the two sides changed, added or removed differently is in conflict
//! where the VCS left it inside a region in conflict.

use crate::manifest::Manifest;
use crate::op_state::OpVerb;
use crate::vcs::{CommitId, ConflictSides, ResolvedRevisionId, TakingSide, Vcs, VcsAction};
use anyhow::Context;
use std::path::Path;

/// What `--continue --keep` keeps: in each region of a conflict the VCS
/// left, the branch checked out's side or the side it takes in, or, where
/// the check stopped a text merge that differs from the merge of the values,
/// that text merge.
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
}

/// Top-level tables whose keys are each one entry.
const KEYED_TABLES: [&str; 2] = ["repositories", "integrations"];

const BY_HAND: &str = "edit rwv.toml into what you mean, then mark it resolved";

pub(crate) const KEEP_TEXT_MERGE: &str = "keep the text merge as the VCS made it";
pub(crate) const STAGE_OWN: &str =
    "edit rwv.toml into what you mean and mark it resolved, then continue";

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct EntryConflict {
    pub(crate) path: Vec<String>,
    pub(crate) yours: Option<toml::Value>,
    pub(crate) incoming: Option<toml::Value>,
}

#[derive(Debug)]
pub(crate) enum Reading {
    /// The VCS merged the text with no conflict and the check stopped it:
    /// the text merge, and how it differs from the merge of the values.
    Checked {
        text_merge: String,
        lines: Vec<String>,
    },
    /// The VCS left regions in conflict: each entry in them both sides
    /// changed differently, and for each side the file that takes it there,
    /// or why there is none.
    Conflicted {
        conflicts: Vec<EntryConflict>,
        taking: [Result<String, String>; 2],
    },
    Unreadable {
        what: String,
    },
}

struct Sides {
    base: Option<Manifest>,
    yours: Manifest,
    incoming: Manifest,
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
            .then(|| parse("common base", &sides.base))
            .transpose()?,
        yours: parse("yours", &sides.yours)?,
        incoming: parse("incoming", &sides.incoming)?,
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
            self.yours.declared()?,
            self.incoming.declared()?,
        ])
    }

    /// The merge of the values with each entry both sides changed
    /// differently as `text_merge` holds it: that entry is the text merge's
    /// to make.
    fn merged_over(&self, text_merge: &Manifest) -> anyhow::Result<Manifest> {
        let text_merge = text_merge.declared()?;
        let [_, yours, _] = self.tables()?;
        self.merged_with(yours, None, |path, _, _| at(&text_merge, path).cloned())
    }

    /// `regions`, the file taking `taking`'s side in each region in
    /// conflict, with each entry the two sides' files differ in settled by
    /// its values: the side that changed it, or `taking`'s where both did.
    fn settled(
        &self,
        regions: &toml::Table,
        other: &toml::Table,
        taking: TakingSide,
    ) -> anyhow::Result<Manifest> {
        let in_regions = self.in_regions(regions, other)?;
        self.merged_with(regions.clone(), Some(&in_regions), |_, yours, incoming| {
            match taking {
                TakingSide::Yours => yours,
                TakingSide::Incoming => incoming,
            }
            .cloned()
        })
    }

    /// `on`, with each entry of `only` (every entry where it is `None`) set
    /// to the merge of its values: the side that changed it, or
    /// `in_conflict` where both changed it differently.
    fn merged_with(
        &self,
        mut on: toml::Table,
        only: Option<&[Vec<String>]>,
        in_conflict: impl Fn(
            &[String],
            Option<&toml::Value>,
            Option<&toml::Value>,
        ) -> Option<toml::Value>,
    ) -> anyhow::Result<Manifest> {
        let [base, yours, incoming] = self.tables()?;
        let paths = match only {
            Some(only) => only.to_vec(),
            None => entries(&[&base, &yours, &incoming]),
        };
        for path in paths {
            let (b, o, t) = (at(&base, &path), at(&yours, &path), at(&incoming, &path));
            let value = if o == b {
                t.cloned()
            } else if t == b || t == o {
                o.cloned()
            } else {
                in_conflict(&path, o, t)
            };
            put(&mut on, &path, value);
        }
        let text = toml::to_string(&on).context("failed to serialize the merged rwv.toml")?;
        Manifest::from_toml_str(&text)
    }

    /// Each entry whose value differs between `regions` and `other`, the
    /// files taking each side in the regions in conflict.
    fn in_regions(
        &self,
        regions: &toml::Table,
        other: &toml::Table,
    ) -> anyhow::Result<Vec<Vec<String>>> {
        let [base, yours, incoming] = self.tables()?;
        Ok(entries(&[&base, &yours, &incoming, regions, other])
            .into_iter()
            .filter(|path| at(regions, path) != at(other, path))
            .collect())
    }

    /// Each entry both sides changed differently whose value differs
    /// between `regions` and `other`.
    fn in_conflict(
        &self,
        regions: &toml::Table,
        other: &toml::Table,
    ) -> anyhow::Result<Vec<EntryConflict>> {
        let [base, yours, incoming] = self.tables()?;
        Ok(self
            .in_regions(regions, other)?
            .into_iter()
            .filter_map(|path| {
                let (b, o, t) = (at(&base, &path), at(&yours, &path), at(&incoming, &path));
                (o != t && t != b && o != b).then(|| EntryConflict {
                    path,
                    yours: o.cloned(),
                    incoming: t.cloned(),
                })
            })
            .collect())
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
    let [yours, incoming] = [TakingSide::Yours, TakingSide::Incoming]
        .map(|taking| vcs.merge_taking(dir, &recorded, taking));
    let (yours, incoming) = (yours?, incoming?);
    let sides = match read(&recorded) {
        Ok(sides) => sides,
        Err((side, why)) => {
            return Ok(Some(Reading::Unreadable {
                what: format!("rwv.toml's {side} side does not read as a manifest ({why})"),
            }))
        }
    };
    if yours == incoming {
        return Ok(Some(Reading::Checked {
            lines: differences(&yours, &sides)?,
            text_merge: yours,
        }));
    }
    let parsed = |side: &str, text: &str| {
        Manifest::from_toml_str(text)
            .and_then(|m| m.declared())
            .map_err(|e| {
                format!(
                    "rwv.toml taking {side} in each region in conflict does not read as a \
                     manifest ({e:#})"
                )
            })
    };
    let (yours_table, incoming_table) =
        match (parsed("yours", &yours), parsed("incoming", &incoming)) {
            (Ok(y), Ok(i)) => (y, i),
            (Err(what), _) | (_, Err(what)) => return Ok(Some(Reading::Unreadable { what })),
        };
    let settle = |taking, text: &str, regions, other| {
        sides
            .settled(regions, other, taking)
            .and_then(|merged| Ok(Manifest::edit_text(text, |m| *m = merged)?.0))
            .map_err(|e| format!("{e:#}"))
    };
    Ok(Some(Reading::Conflicted {
        conflicts: sides.in_conflict(&yours_table, &incoming_table)?,
        taking: [
            settle(TakingSide::Yours, &yours, &yours_table, &incoming_table),
            settle(
                TakingSide::Incoming,
                &incoming,
                &incoming_table,
                &yours_table,
            ),
        ],
    }))
}

/// How `text_merge`, a text merge of `rwv.toml`, differs from the merge of
/// the values of `sides` in an entry at most one side changed, then each
/// entry both sides changed that it holds as one side's value; nothing where
/// no entry at most one side changed differs, or where it does not parse,
/// which the stop for an unparseable result answers.
fn differences(text_merge: &str, sides: &Sides) -> anyhow::Result<Vec<String>> {
    let Ok(text_merge) = Manifest::from_toml_str(text_merge) else {
        return Ok(Vec::new());
    };
    let values = sides.merged_over(&text_merge)?.declared()?;
    let text_merge = text_merge.declared()?;
    let differing: Vec<String> = entries(&[&text_merge, &values])
        .into_iter()
        .filter_map(|path| {
            let (t, v) = (at(&text_merge, &path), at(&values, &path));
            (t != v).then(|| {
                format!(
                    "  {}: {} in the text merge, {} in the merge of the values",
                    entry_name(&path),
                    shown(t),
                    shown(v)
                )
            })
        })
        .collect();
    if differing.is_empty() {
        return Ok(Vec::new());
    }
    let [base, yours, incoming] = sides.tables()?;
    let both_changed: Vec<String> = entries(&[&base, &yours, &incoming])
        .into_iter()
        .filter_map(|path| {
            let (b, o, t) = (at(&base, &path), at(&yours, &path), at(&incoming, &path));
            let held = at(&text_merge, &path);
            (o != b && t != b && o != t && (held == o || held == t)).then(|| {
                format!(
                    "  {}: {} in yours, {} incoming; the text merge holds {}",
                    entry_name(&path),
                    shown(o),
                    shown(t),
                    shown(held)
                )
            })
        })
        .collect();
    let mut lines = vec![
        "rwv.toml: the VCS merged its text with no conflict, into a manifest that differs from \
         the merge of both sides' values:"
            .to_owned(),
    ];
    lines.extend(differing);
    if !both_changed.is_empty() {
        lines.push(
            "both sides changed these entries, and the text merge holds one side's value:"
                .to_owned(),
        );
        lines.extend(both_changed);
    }
    Ok(lines)
}

/// The sides of `sides`, read, where `text_merge` differs from the merge of
/// their values and both sides changed `rwv.toml`.
fn differing(sides: &ConflictSides, text_merge: &str) -> anyhow::Result<Option<Sides>> {
    if sides.yours == sides.base || sides.incoming == sides.base {
        return Ok(None);
    }
    let Ok(read) = read(sides) else {
        return Ok(None);
    };
    Ok((!differences(text_merge, &read)?.is_empty()).then_some(read))
}

/// How `tip`, the replayed tip's `rwv.toml`, differs from the merge of the
/// values of `sides`: the merge base, the branch's tip before the replay and
/// the commit replayed onto. A tip that does not parse is not judged.
pub(crate) fn check_tip(sides: &ConflictSides, tip: &str) -> anyhow::Result<Option<Vec<String>>> {
    Ok(match differing(sides, tip)? {
        Some(read) => Some(differences(tip, &read)?),
        None => None,
    })
}

/// Check `text_merge`, the text the VCS merged `rwv.toml` into in `dir` with
/// no conflict, against the merge of the values of `sides`. Where they
/// differ, record `rwv.toml` as unmerged from `sides`, its working file left
/// as the VCS merged it; return whether it did.
pub(crate) fn check_text_merge(
    vcs: &dyn Vcs,
    dir: &Path,
    sides: &ConflictSides,
    text_merge: &str,
) -> anyhow::Result<bool> {
    if differing(sides, text_merge)?.is_none() {
        return Ok(false);
    }
    vcs.record_conflict(dir, Manifest::FILE_NAME, sides)?;
    Ok(true)
}

/// Check `text_merge`, the text the VCS merged `rwv.toml` into in `dir` with
/// no conflict, for a port that carries the tables of `rwv.toml` and keeps
/// `ours`' entries and `history`: it is judged against the merge of the
/// values of `sides` with those standing on every side. Where it does not
/// parse or differs, record `rwv.toml` as unmerged from `sides`, its working
/// file left as the VCS merged it, and return the lines saying how.
pub(crate) fn check_ported_text_merge(
    vcs: &dyn Vcs,
    dir: &Path,
    sides: &ConflictSides,
    text_merge: &str,
    ours: &Manifest,
) -> anyhow::Result<Option<Vec<String>>> {
    let hold = |text: &str| -> anyhow::Result<String> {
        Ok(Manifest::edit_text(text, |m| {
            m.history = ours.history;
            m.repositories = ours.repositories.clone();
        })?
        .0)
    };
    let lines = match Manifest::from_toml_str(text_merge) {
        Err(error) => std::iter::once(
            "rwv.toml: the VCS merged its text with no conflict, into a file that does not parse:"
                .to_owned(),
        )
        .chain(format!("{error:#}").lines().map(|line| format!("  {line}")))
        .collect(),
        Ok(_) => {
            let held = ConflictSides {
                base: sides.base.as_deref().map(hold).transpose()?,
                yours: sides.yours.as_deref().map(hold).transpose()?,
                incoming: sides.incoming.as_deref().map(hold).transpose()?,
            };
            let Ok(read) = read(&held) else {
                return Ok(None);
            };
            differences(&hold(text_merge)?, &read)?
        }
    };
    if lines.is_empty() {
        return Ok(None);
    }
    vcs.record_conflict(dir, Manifest::FILE_NAME, sides)?;
    Ok(Some(lines))
}

/// The sides of `rwv.toml` a merge or replay of `incoming` into `yours` over
/// `base` records.
pub(crate) fn sides_at(
    vcs: &dyn Vcs,
    dir: &Path,
    base: Option<&CommitId>,
    yours: &CommitId,
    incoming: &CommitId,
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
        yours: at(yours),
        incoming: at(incoming),
    }
}

/// The sides of `rwv.toml` a merge of `incoming` into `yours` records, where the
/// two have at most one merge base. With several, no one base is both sides'
/// common one, and the VCS's own merge stands unchecked.
pub(crate) fn sides_to_check(
    vcs: &dyn Vcs,
    dir: &Path,
    yours: &CommitId,
    incoming: &CommitId,
) -> anyhow::Result<Option<ConflictSides>> {
    let bases = vcs.merge_bases(dir, yours, incoming)?;
    Ok((bases.len() <= 1).then(|| sides_at(vcs, dir, bases.first(), yours, incoming)))
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
    let Some(incoming) = vcs.merge_in_flight(dir)? else {
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
    let yours = vcs.head_revision(dir)?.id().clone();
    let Some(sides) = sides_to_check(vcs, dir, &yours, &incoming)? else {
        return Ok(false);
    };
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
/// `keep` naming what to take. Where `rwv.toml` is unmerged with regions in
/// conflict, write the file taking `keep`'s side in them and mark it
/// resolved, keeping the checkout's edits as a backup first; where the check
/// stopped a text merge, `--keep text-merge` marks the working file resolved
/// while it holds that text merge, and nothing is written. Where the replayed
/// tip's `rwv.toml` differs from the merge of the values, `--keep text-merge`
/// keeps it. Refuse a bare continue while `rwv.toml` is unmerged, a side
/// named where no region is in conflict, and a merge or replay in flight that
/// is not the operation's.
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
            "then run `{resume}`, or `{}` to put everything back as it was; edits to tracked files \
             not yet committed are not kept, as the VCS's own abort does not keep them",
            crate::op_state::abort_command_at(workspace)
        ));
        crate::refusal::refusal(crate::refusal::RefusalKind::OpParked, text)
    };
    let path = dir.join(Manifest::FILE_NAME);
    let done = crate::op_state::resume_command(typed);
    let Some(reading) = reading(vcs, &dir)? else {
        if record.verb != typed {
            return Ok(());
        }
        if let Some((rebase, _)) = crate::project_merge::replayed_tip_differs(vcs, &dir, workspace)?
        {
            return settle_tip(vcs, &dir, workspace, rebase, keep, &done, &refused, &resume);
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
                "{done}: rwv.toml is not conflicted, so `--keep {}` takes nothing",
                side.word()
            ),
            None => {}
        }
        return Ok(());
    };
    let still = |reading: &Reading| {
        refused(
            "rwv.toml is still in conflict",
            reading_lines(vcs, &dir, reading, Some(&resume)),
        )
    };
    if record.verb != typed {
        return Err(still(&reading));
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
    let (conflicts, taking) = match (&reading, keep) {
        (Reading::Conflicted { conflicts, taking }, Some(Side::Yours | Side::Incoming) | None) => {
            (conflicts, taking)
        }
        (Reading::Checked { text_merge, .. }, Some(Side::TextMerge)) => {
            if std::fs::read_to_string(&path).ok().as_ref() != Some(text_merge) {
                return Err(refused(
                    "rwv.toml was edited since the stop, so it no longer holds the text merge \
                     as the VCS made it",
                    vec![
                        "to take it as it stands, mark it resolved, then run the continue again:"
                            .to_owned(),
                        format!("  {}", mark_resolved(vcs, &dir)),
                    ],
                ));
            }
            vcs.stage_paths(&dir, &[Manifest::FILE_NAME])?;
            eprintln!(
                "{done}: kept the text merge of rwv.toml as the VCS made it, marked resolved"
            );
            return Ok(());
        }
        (Reading::Checked { .. }, Some(side)) => {
            return Err(refused(
                &format!(
                    "the VCS merged rwv.toml with no conflict, so `--keep {}` has no side to take",
                    side.word()
                ),
                reading_lines(vcs, &dir, &reading, Some(&resume)),
            ))
        }
        (Reading::Conflicted { .. }, Some(Side::TextMerge)) => {
            return Err(refused(
                "rwv.toml is in conflict in the VCS's own text merge, so there is no text merge \
                 to keep",
                reading_lines(vcs, &dir, &reading, Some(&resume)),
            ))
        }
        (Reading::Checked { .. } | Reading::Unreadable { .. }, _) => return Err(still(&reading)),
    };
    let side = match keep {
        Some(side) => side,
        None if conflicts.is_empty() => Side::Yours,
        None => return Err(still(&reading)),
    };
    let text = match &taking[usize::from(side == Side::Incoming)] {
        Ok(text) => text,
        Err(why) => {
            return Err(refused(
                &format!(
                    "taking {} in each region of rwv.toml in conflict leaves no manifest ({why})",
                    side.word()
                ),
                vec![
                    format!("{BY_HAND}:"),
                    format!("  {}", mark_resolved(vcs, &dir)),
                ],
            ))
        }
    };
    if let Some(working) = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| Manifest::from_toml_str(&text).ok())
    {
        if working.declares_same(&Manifest::from_toml_str(text)?) {
            vcs.stage_paths(&dir, &[Manifest::FILE_NAME])?;
            eprintln!(
                "{done}: rwv.toml as it stands declares the merge of both sides; marked it resolved"
            );
            return Ok(());
        }
        return Err(refused(
            "rwv.toml as it stands reads as a manifest other than the merge of both sides",
            vec![
                "to keep it as it stands, mark it resolved, then run the continue again:"
                    .to_owned(),
                format!("  {}", mark_resolved(vcs, &dir)),
            ],
        ));
    }
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
    eprintln!("{done}: wrote the merge of both sides of rwv.toml{took}, marked resolved");
    for line in kept.iter().flat_map(|kept| kept_lines(vcs, &dir, kept)) {
        eprintln!("{line}");
    }
    Ok(())
}

/// At the stop where the replayed tip's `rwv.toml` in `dir` differs from the
/// merge of the values: `--keep text-merge` keeps the tip as the VCS made it
/// and settles it for the rest of the operation, refused where another
/// `rwv.toml` is marked resolved. A bare continue leaves the stop to the
/// operation, which takes an `rwv.toml` marked resolved that parses, and
/// stops again without one.
#[allow(clippy::too_many_arguments)]
fn settle_tip(
    vcs: &dyn Vcs,
    dir: &Path,
    workspace: &Path,
    rebase: crate::op_state::Rebasing,
    keep: Option<Side>,
    done: &str,
    refused: &dyn Fn(&str, Vec<String>) -> anyhow::Error,
    resume: &str,
) -> anyhow::Result<()> {
    let Some(keep) = keep else {
        return Ok(());
    };
    if keep != Side::TextMerge {
        return Err(refused(
            &format!(
                "the VCS merged the replayed tip's rwv.toml with no conflict, so `--keep {}` \
                 has no side to take",
                keep.word()
            ),
            vec![
                format!("to {KEEP_TEXT_MERGE}:"),
                format!("  {resume} --keep text-merge"),
                format!("or {STAGE_OWN}:"),
                format!("  {}", mark_resolved(vcs, dir)),
            ],
        ));
    }
    let head = vcs.head_revision(dir)?;
    let committed = vcs
        .read_file_at_revision(dir, &head, Path::new(Manifest::FILE_NAME))
        .ok();
    if vcs.staged_file(dir, Manifest::FILE_NAME)? != committed {
        return Err(refused(
            "an rwv.toml other than the replayed tip's is marked resolved",
            vec![
                format!("to take it, run `{resume}` without `--keep`"),
                "to keep the replayed tip's text merge, withdraw it first:".to_owned(),
                format!(
                    "  {}",
                    vcs.hint(VcsAction::Unstage)
                        .replace("<checkout>", &crate::workspace::quoted(dir))
                        .replace("<paths>", Manifest::FILE_NAME)
                ),
            ],
        ));
    }
    eprintln!("{done}: kept the replayed tip's rwv.toml as the VCS merged it");
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

pub(crate) fn mark_resolved(vcs: &dyn Vcs, dir: &Path) -> String {
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
    let mut lines = Vec::new();
    match reading {
        Reading::Unreadable { what } => lines.push(format!("{what}; {BY_HAND}:")),
        Reading::Checked { lines: listed, .. } => {
            lines.extend(listed.iter().cloned());
            match resume {
                Some(resume) => lines.extend([
                    format!("to {KEEP_TEXT_MERGE}:"),
                    format!("  {resume} --keep text-merge"),
                    format!("or {STAGE_OWN}:"),
                ]),
                None => lines.push(format!("rwv.toml: {BY_HAND}:")),
            }
        }
        Reading::Conflicted { conflicts, taking } => {
            let mut heading: Option<&[String]> = None;
            for conflict in conflicts {
                let [yours, incoming] = [conflict.yours.as_ref(), conflict.incoming.as_ref()];
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
                Some(resume) if conflicts.is_empty() && taking[0].is_ok() => lines.extend([
                    format!(
                        "rwv.toml: no entry is in conflict; `{resume}` writes the merge of both \
                         sides, marked resolved"
                    ),
                    format!("or {BY_HAND}:"),
                ]),
                Some(resume) => {
                    lines.push(
                        "to take one side for every entry in conflict, and write the merge of \
                         both sides, marked resolved:"
                            .to_owned(),
                    );
                    for (side, taken) in [Side::Yours, Side::Incoming].into_iter().zip(taking) {
                        lines.push(match taken {
                            Ok(_) => format!("  {resume} --keep {}", side.word()),
                            Err(why) => {
                                format!("  (taking {} leaves no manifest: {why})", side.word())
                            }
                        });
                    }
                    lines.push(format!("or {BY_HAND}:"));
                }
                None => lines.push(format!("rwv.toml: {BY_HAND}:")),
            }
        }
    }
    if lines.last().is_some_and(|line| line.ends_with(':')) {
        lines.push(format!("  {}", mark_resolved(vcs, dir)));
    }
    lines
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
            yours: Some(ours.to_owned()),
            incoming: Some(theirs.to_owned()),
        })
        .unwrap()
    }

    /// Each side's file standing whole for the region in conflict.
    fn whole(sides: &Sides) -> [toml::Table; 2] {
        let [_, yours, incoming] = sides.tables().unwrap();
        [yours, incoming]
    }

    fn paths(sides: &Sides) -> Vec<String> {
        let [yours, incoming] = whole(sides);
        sides
            .in_conflict(&yours, &incoming)
            .unwrap()
            .iter()
            .map(|c| entry_name(&c.path))
            .collect()
    }

    impl Sides {
        fn merged(&self, take_incoming: bool) -> anyhow::Result<Manifest> {
            let [yours, incoming] = whole(self);
            if take_incoming {
                self.settled(&incoming, &yours, TakingSide::Incoming)
            } else {
                self.settled(&yours, &incoming, TakingSide::Yours)
            }
        }
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
    fn an_entry_both_sides_changed_the_regions_agree_on_is_not_in_conflict_and_kept() {
        let base = format!("{}[workweave]\ncopy = [\"b\"]\n", member("a", "main"));
        let ours = base
            .replace("[\"b\"]", "[\"b\", \"o\"]")
            .replace("main", "x");
        let theirs = base
            .replace("[\"b\"]", "[\"t\", \"b\"]")
            .replace("main", "y");
        let sides = sides(&base, &ours, &theirs);
        let merged_list = base
            .replace("[\"b\"]", "[\"t\", \"b\", \"o\"]")
            .replace("main", "x");
        let regions = Manifest::from_toml_str(&merged_list)
            .unwrap()
            .declared()
            .unwrap();
        let other = Manifest::from_toml_str(&merged_list.replace("\"x\"", "\"y\""))
            .unwrap()
            .declared()
            .unwrap();
        let listed: Vec<String> = sides
            .in_conflict(&regions, &other)
            .unwrap()
            .iter()
            .map(|c| entry_name(&c.path))
            .collect();
        assert_eq!(listed, ["local/a branch"]);
        let settled = toml::to_string(
            &sides
                .settled(&regions, &other, TakingSide::Yours)
                .unwrap()
                .declared()
                .unwrap(),
        )
        .unwrap();
        assert!(settled.contains("\"t\", \"b\", \"o\""), "{settled}");
        assert!(settled.contains("branch = \"x\""), "{settled}");
    }

    #[test]
    fn a_side_that_is_not_a_manifest_is_named() {
        let base = member("a", "main");
        let err = read(&ConflictSides {
            base: Some(base.clone()),
            yours: Some("<<<<<<< nope".to_owned()),
            incoming: Some(base),
        })
        .err()
        .unwrap();
        assert_eq!(err.0, "yours");
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
