//! An `rwv.toml` a merge or a replay left conflicted, read from the sides
//! the VCS recorded for it. A member's whole table is one entry, an
//! integration's table is one, and every other top-level key is one; an
//! entry the two sides changed, added or removed differently is in conflict.
//! An entry's setter, run there, settles it on both sides and stages
//! nothing else; the file is staged once no entry is left in conflict.

use crate::manifest::{History, Manifest, RepoPath, RepoUrl, Role, SubmoduleRecursion};
use crate::vcs::{ConflictSides, TrackingRef, Vcs, VcsAction};
use anyhow::Context;
use std::path::Path;

/// Top-level tables whose keys are each one entry.
const KEYED_TABLES: [&str; 2] = ["repositories", "integrations"];

/// One entry the two sides changed differently, at its key path, with each
/// side's value; `None` where that side has no such entry.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct EntryConflict {
    pub(crate) path: Vec<String>,
    pub(crate) ours: Option<toml::Value>,
    pub(crate) theirs: Option<toml::Value>,
}

/// What the sides recorded for a conflicted `rwv.toml` hold.
#[derive(Debug)]
pub(crate) enum Reading {
    /// Every side reads as a manifest; these entries conflict, none where
    /// only the lines do.
    Entries(Vec<EntryConflict>),
    /// This side has no file, or one that does not read as a manifest.
    Unreadable { side: &'static str, why: String },
    /// Nothing is left unmerged, and the file as it stands does not read as a
    /// manifest.
    Unparsed(String),
}

/// What a setter writes: one field of one member, the removal of a member,
/// or a `history`, a member's or the top-level one.
#[derive(Debug, Clone)]
pub(crate) enum Setting {
    Branch(RepoPath, TrackingRef),
    Url(RepoPath, RepoUrl),
    Role(RepoPath, Role),
    RecurseSubmodules(RepoPath, SubmoduleRecursion),
    Remove(RepoPath),
    History(Option<RepoPath>, Option<History>),
}

impl Setting {
    fn entry(&self) -> Vec<String> {
        match self {
            Self::Branch(member, _)
            | Self::Url(member, _)
            | Self::Role(member, _)
            | Self::RecurseSubmodules(member, _)
            | Self::Remove(member)
            | Self::History(Some(member), _) => {
                vec!["repositories".to_owned(), member.as_str().to_owned()]
            }
            Self::History(None, _) => vec!["history".to_owned()],
        }
    }

    fn apply(&self, manifest: &mut Manifest) {
        let member = match self {
            Self::History(None, history) => {
                manifest.history = *history;
                return;
            }
            Self::Remove(member) => {
                manifest.remove_repo(member);
                return;
            }
            Self::Branch(member, _)
            | Self::Url(member, _)
            | Self::Role(member, _)
            | Self::RecurseSubmodules(member, _)
            | Self::History(Some(member), _) => member,
        };
        let Some(mut entry) = manifest.get_entry(member).cloned() else {
            return;
        };
        match self {
            Self::Branch(_, branch) => entry.branch = branch.clone(),
            Self::Url(_, url) => entry.url = url.clone(),
            Self::Role(_, role) => entry.role = *role,
            Self::RecurseSubmodules(_, recurse) => entry.recurse_submodules = *recurse,
            Self::History(_, history) => entry.history = *history,
            Self::Remove(_) => {}
        }
        manifest.insert_repo(member.clone(), entry);
    }
}

/// What a setter run at a conflicted `rwv.toml` did.
#[derive(Debug)]
pub(crate) struct Settled {
    pub(crate) entry: String,
    pub(crate) left: Vec<EntryConflict>,
    /// The commit holding the checkout's edits as they stood before the
    /// rewrite, `rwv.toml` with any unstaged edit of it among them.
    pub(crate) kept: Option<crate::vcs::BackupWitness>,
}

struct Sides {
    base: Option<Manifest>,
    ours: Manifest,
    theirs: Manifest,
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
    })
}

fn table(manifest: &Manifest) -> toml::Table {
    toml::Table::try_from(manifest).unwrap_or_default()
}

impl Sides {
    fn tables(&self) -> [toml::Table; 3] {
        [
            self.base.as_ref().map(table).unwrap_or_default(),
            table(&self.ours),
            table(&self.theirs),
        ]
    }

    fn conflicts(&self) -> Vec<EntryConflict> {
        let [base, ours, theirs] = self.tables();
        entries(&base, &ours, &theirs)
            .into_iter()
            .filter_map(|path| {
                let (b, o, t) = (at(&base, &path), at(&ours, &path), at(&theirs, &path));
                (o != t && t != b && o != b).then(|| EntryConflict {
                    path,
                    ours: o.cloned(),
                    theirs: t.cloned(),
                })
            })
            .collect()
    }

    /// Ours, with each entry only theirs changed taken from theirs.
    fn merged(&self) -> anyhow::Result<Manifest> {
        let [base, ours, theirs] = self.tables();
        let mut merged = ours.clone();
        for path in entries(&base, &ours, &theirs) {
            if at(&ours, &path) == at(&base, &path) {
                put(&mut merged, &path, at(&theirs, &path).cloned());
            }
        }
        let text = toml::to_string(&merged).context("failed to serialize the merged rwv.toml")?;
        Manifest::from_toml_str(&text)
    }
}

fn entries(base: &toml::Table, ours: &toml::Table, theirs: &toml::Table) -> Vec<Vec<String>> {
    let sides = [base, ours, theirs];
    let mut keys: Vec<&String> = sides.iter().flat_map(|side| side.keys()).collect();
    keys.sort();
    keys.dedup();
    let mut paths = Vec::new();
    for key in keys {
        let keyed = KEYED_TABLES.contains(&key.as_str())
            && sides
                .iter()
                .all(|side| side.get(key).is_none_or(toml::Value::is_table));
        if !keyed {
            paths.push(vec![key.clone()]);
            continue;
        }
        let mut subs: Vec<&String> = sides
            .iter()
            .filter_map(|side| side.get(key).and_then(toml::Value::as_table))
            .flat_map(toml::Table::keys)
            .collect();
        subs.sort();
        subs.dedup();
        paths.extend(subs.into_iter().map(|sub| vec![key.clone(), sub.clone()]));
    }
    paths
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

/// How `rwv.toml` in `dir` reads, `None` where it is not conflicted there.
pub(crate) fn reading(vcs: &dyn Vcs, dir: &Path) -> anyhow::Result<Option<Reading>> {
    let Some(sides) = vcs.conflict_sides(dir, Manifest::FILE_NAME)? else {
        return Ok(None);
    };
    Ok(Some(match read(&sides) {
        Ok(sides) => Reading::Entries(sides.conflicts()),
        Err((side, why)) => Reading::Unreadable { side, why },
    }))
}

/// Settle `setting`'s entry of the conflicted `rwv.toml` in `dir` on both
/// sides. With no entry left in conflict, the merged manifest is written
/// and staged; otherwise the rewritten sides are recorded and the VCS
/// rewrites the working file from them. `None`, with nothing written, where
/// `rwv.toml` is not conflicted there, a side does not read as a manifest,
/// or the entry is not among those in conflict.
pub(crate) fn settle(
    vcs: &dyn Vcs,
    dir: &Path,
    setting: &Setting,
) -> anyhow::Result<Option<Settled>> {
    let Some(recorded) = vcs.conflict_sides(dir, Manifest::FILE_NAME)? else {
        return Ok(None);
    };
    let Ok(mut sides) = read(&recorded) else {
        return Ok(None);
    };
    let entry = setting.entry();
    if !sides.conflicts().iter().any(|c| c.path == entry) {
        return Ok(None);
    }
    setting.apply(&mut sides.ours);
    setting.apply(&mut sides.theirs);
    let kept = vcs.backup_edits(dir)?;
    let left = sides.conflicts();
    if left.is_empty() {
        sides.merged()?.write(&dir.join(Manifest::FILE_NAME))?;
        vcs.stage_paths(dir, &[Manifest::FILE_NAME])?;
    } else {
        let text = |manifest: &Manifest| {
            toml::to_string(manifest).context("failed to serialize a side of rwv.toml")
        };
        vcs.rewrite_conflict(
            dir,
            Manifest::FILE_NAME,
            &ConflictSides {
                base: sides.base.as_ref().map(text).transpose()?,
                ours: Some(text(&sides.ours)?),
                theirs: Some(text(&sides.theirs)?),
            },
        )?;
    }
    Ok(Some(Settled {
        entry: entry_name(&entry),
        left,
        kept,
    }))
}

/// Run at a stop where the active project's `rwv.toml` is conflicted and
/// `setting`'s entry is in conflict: settle it, say what is left, and
/// return `true`. `false`, having done nothing, everywhere else.
pub(crate) fn settle_at_stop(
    ctx: &crate::workspace::WorkspaceContext,
    verb: &str,
    setting: &Setting,
) -> anyhow::Result<bool> {
    let Some(project) = ctx.active_project() else {
        return Ok(false);
    };
    let dir = crate::workspace::project_dir(ctx.active_path(), project);
    let vcs = crate::vcs::project_vcs();
    let vcs = vcs.as_ref();
    if !vcs.is_repo(&dir) {
        return Ok(false);
    }
    let recorded = crate::op_state::read_owner_header(ctx.active_path())
        .ok()
        .flatten()
        .map(|header| header.verb);
    if recorded == Some(crate::op_state::OpVerb::CherryPick) {
        return Ok(false);
    }
    let Some(settled) = settle(vcs, &dir, setting)? else {
        return Ok(false);
    };
    let resume = recorded.map(crate::op_state::resume_command);
    eprintln!(
        "rwv {verb}: settled rwv.toml entry {} on both sides",
        settled.entry
    );
    if let Some(kept) = &settled.kept {
        eprintln!(
            "rwv.toml as it stood before is kept in backup {}; to read it:",
            kept.backup().name
        );
        eprintln!(
            "  {}",
            vcs.hint(VcsAction::ShowKeptFile)
                .replace("<checkout>", &dir.display().to_string())
                .replace("<revision>", kept.backup().tip.id().as_str())
                .replace("<paths>", Manifest::FILE_NAME)
        );
    }
    if settled.left.is_empty() {
        eprintln!("rwv.toml has no entry left in conflict, and is staged");
    } else {
        eprintln!("rwv.toml still has entries in conflict:");
        for line in reading_lines(vcs, &dir, &Reading::Entries(settled.left)) {
            eprintln!("{line}");
        }
    }
    if let Some(resume) = resume {
        eprintln!("then run `{resume}` once nothing is left in conflict");
    }
    Ok(true)
}

/// Refuse an operation's continue while the active project's `rwv.toml` is
/// conflicted, naming what is left to settle.
pub(crate) fn refuse_continue_while_conflicted(
    ctx: &crate::workspace::WorkspaceContext,
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
    let Some(header) = crate::op_state::read_owner_header(ctx.active_path())? else {
        return Ok(());
    };
    if header.verb == crate::op_state::OpVerb::CherryPick {
        return Ok(());
    }
    let reading = match reading(vcs, &dir)? {
        Some(reading) => reading,
        None => match std::fs::read_to_string(dir.join(Manifest::FILE_NAME)) {
            Ok(text) => match Manifest::from_toml_str(&text) {
                Ok(_) => return Ok(()),
                Err(e) => Reading::Unparsed(format!("{e:#}")),
            },
            Err(_) => return Ok(()),
        },
    };
    let mut text = format!(
        "rwv.toml is still in conflict in {} at {}:\n",
        crate::workspace::project_rel_path(project),
        dir.display()
    );
    for line in reading_lines(vcs, &dir, &reading) {
        text.push_str(&line);
        text.push('\n');
    }
    text.push_str(&format!(
        "then run `{}`, or `rwv abort` to put everything back as it was",
        crate::op_state::resume_command(header.verb)
    ));
    Err(crate::refusal::refusal(
        crate::refusal::RefusalKind::OpParked,
        text,
    ))
}

fn entry_name(path: &[String]) -> String {
    match path {
        [table, member] if table == "repositories" => member.clone(),
        [table, name] => format!("[{table}.{name}]"),
        [key] => key.clone(),
        _ => path.join("."),
    }
}

/// The lines a stop prints for a conflicted `rwv.toml` in `dir`: each
/// entry in conflict, both sides' values, and the setter that keeps each;
/// where no verb settles it, the hand edit and the staging that do.
pub(crate) fn stop_lines(vcs: &dyn Vcs, dir: &Path) -> Vec<String> {
    match reading(vcs, dir) {
        Ok(Some(reading)) => reading_lines(vcs, dir, &reading),
        _ => Vec::new(),
    }
}

pub(crate) fn reading_lines(vcs: &dyn Vcs, dir: &Path, reading: &Reading) -> Vec<String> {
    let by_hand = "no verb settles it: edit rwv.toml, then stage it";
    let replaying = matches!(vcs.rebase_in_flight(dir), Ok(Some(_)));
    let label = |side: &str| match (side, replaying) {
        ("ours", false) | ("theirs", true) => "yours",
        ("ours", true) | ("theirs", false) => "incoming",
        _ => "common base",
    };
    let mut lines = match reading {
        Reading::Unreadable { side, why } => vec![format!(
            "rwv.toml's {} side does not read as a manifest ({why}); {by_hand}",
            label(side)
        )],
        Reading::Unparsed(why) => vec![format!(
            "rwv.toml does not read as a manifest ({why}); {by_hand}"
        )],
        Reading::Entries(conflicts) if conflicts.is_empty() => vec![format!(
            "rwv.toml: no entry is in conflict, only lines are; {by_hand}"
        )],
        Reading::Entries(conflicts) => conflicts
            .iter()
            .flat_map(|conflict| conflict_lines(conflict, replaying, by_hand))
            .collect(),
    };
    if lines.iter().any(|line| line.contains(" yours")) {
        lines.push(format!(
            "(yours: the side of the branch checked out in {}; incoming: the side it takes in)",
            dir.display()
        ));
    }
    if lines.iter().any(|line| line.contains(by_hand)) {
        lines.push("to stage rwv.toml once it is edited:".to_owned());
        lines.push(format!(
            "  {}",
            vcs.hint(VcsAction::MarkResolved)
                .replace("<checkout>", &dir.display().to_string())
                .replace("<paths>", Manifest::FILE_NAME)
        ));
    }
    lines
}

/// `replaying`: the checkout is replaying its branch's commits, so the side
/// the VCS records as its own is the one taken in, and the other is the
/// branch's own.
fn conflict_lines(conflict: &EntryConflict, replaying: bool, by_hand: &str) -> Vec<String> {
    let name = entry_name(&conflict.path);
    let member = match conflict.path.as_slice() {
        [table, member] if table == "repositories" => Some(member.as_str()),
        _ => None,
    };
    let (yours, incoming) = if replaying {
        (&conflict.theirs, &conflict.ours)
    } else {
        (&conflict.ours, &conflict.theirs)
    };
    let mut lines = Vec::new();
    match (yours, incoming) {
        (Some(toml::Value::Table(yours)), Some(toml::Value::Table(incoming))) => {
            lines.push(format!("rwv.toml entry {name} is in conflict:"));
            let mut keys: Vec<&String> = yours.keys().chain(incoming.keys()).collect();
            keys.sort();
            keys.dedup();
            for key in keys {
                let (y, i) = (yours.get(key), incoming.get(key));
                if y == i {
                    continue;
                }
                lines.push(format!(
                    "  {key}: {} in yours, {} incoming",
                    shown(y),
                    shown(i)
                ));
                match &member {
                    Some(member) => {
                        for (side, value) in [("yours", y), ("incoming", i)] {
                            let setter = field_setter(member, key, value);
                            lines.push(format!(
                                "    keep {side}: {}",
                                setter.as_deref().unwrap_or(by_hand)
                            ));
                        }
                    }
                    None => lines.push(format!("    {by_hand}")),
                }
            }
        }
        (yours, incoming) => {
            lines.push(format!(
                "rwv.toml entry {name} is in conflict: {} in yours, {} incoming",
                shown(yours.as_ref()),
                shown(incoming.as_ref())
            ));
            let top_history = conflict.path == ["history"];
            for (side, value) in [("yours", yours), ("incoming", incoming)] {
                let setter = match (&member, value) {
                    (Some(member), None) => Some(format!("rwv member remove {member}")),
                    (None, value) if top_history => Some(history_setter(value.as_ref(), None)),
                    _ => None,
                };
                lines.push(format!(
                    "  keep {side}: {}",
                    setter.as_deref().unwrap_or(by_hand)
                ));
            }
        }
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

fn history_setter(value: Option<&toml::Value>, member: Option<&str>) -> String {
    let repo = member.map_or(String::new(), |member| format!(" --repo {member}"));
    match value.and_then(toml::Value::as_str) {
        Some(history) => format!("rwv branch set-history {history}{repo}"),
        None => format!("rwv branch set-history --unset{repo}"),
    }
}

fn field_setter(member: &str, field: &str, value: Option<&toml::Value>) -> Option<String> {
    if field == "history" {
        return Some(history_setter(value, Some(member)));
    }
    if field == "recurse-submodules" {
        let recurse = value.and_then(toml::Value::as_bool).unwrap_or(true);
        return Some(format!(
            "rwv member set-recurse-submodules {member} {recurse}"
        ));
    }
    let verb = match field {
        "branch" => "set-branch",
        "url" => "set-url",
        "role" => "set-role",
        _ => return None,
    };
    Some(format!("rwv member {verb} {member} {}", value?.as_str()?))
}

#[cfg(test)]
mod tests {
    use super::*;

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
            .iter()
            .map(|c| entry_name(&c.path))
            .collect()
    }

    #[test]
    fn different_fields_of_one_member_conflict_as_the_whole_member() {
        let base = member("lib", "main");
        let ours = base.replace("role = \"owned\"", "role = \"fork\"");
        let theirs = member("lib", "dev");
        assert_eq!(paths(&sides(&base, &ours, &theirs)), ["local/lib"]);
    }

    #[test]
    fn members_changed_on_one_side_each_do_not_conflict_and_merge_both() {
        let base = format!("{}{}", member("a", "main"), member("b", "main"));
        let ours = format!("{}{}", member("a", "x"), member("b", "main"));
        let theirs = format!("{}{}", member("a", "main"), member("b", "y"));
        let sides = sides(&base, &ours, &theirs);
        assert!(paths(&sides).is_empty());
        let merged = sides.merged().unwrap();
        let branch = |m: &str| {
            let path = RepoPath::new(format!("local/{m}")).unwrap();
            merged
                .get_entry(&path)
                .unwrap()
                .branch
                .local_counterpart()
                .as_str()
                .to_owned()
        };
        assert_eq!((branch("a"), branch("b")), ("x".to_owned(), "y".to_owned()));
    }

    #[test]
    fn removed_against_changed_conflicts_either_way() {
        let base = format!("{}{}", member("a", "main"), member("b", "main"));
        let changed = format!("{}{}", member("a", "x"), member("b", "main"));
        let removed = member("b", "main");
        assert_eq!(paths(&sides(&base, &changed, &removed)), ["local/a"]);
        assert_eq!(paths(&sides(&base, &removed, &changed)), ["local/a"]);
    }

    #[test]
    fn workweave_and_history_are_entries() {
        let base = member("a", "main");
        let ours = format!("history = \"merge\"\n{base}[workweave]\ncopy = [\"o\"]\n");
        let theirs = format!("history = \"linear\"\n{base}[workweave]\ncopy = [\"t\"]\n");
        assert_eq!(
            paths(&sides(&base, &ours, &theirs)),
            ["history", "workweave"]
        );
    }

    #[test]
    fn a_setting_on_both_sides_settles_its_entry_and_leaves_the_other() {
        let base = format!("{}{}", member("a", "main"), member("b", "main"));
        let ours = format!("{}{}", member("a", "x"), member("b", "x"));
        let theirs = format!("{}{}", member("a", "y"), member("b", "y"));
        let mut sides = sides(&base, &ours, &theirs);
        let a = RepoPath::new("local/a").unwrap();
        let branch = TrackingRef::parse(crate::vcs::RawRefName::new("x".to_owned())).unwrap();
        let setting = Setting::Branch(a, branch);
        setting.apply(&mut sides.ours);
        setting.apply(&mut sides.theirs);
        assert_eq!(paths(&sides), ["local/b"]);
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
}
