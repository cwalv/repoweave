//! The one writer of `rwv.toml` text: a change to the typed manifest becomes an
//! edit of only the entries and keys it changes, every other byte left as it
//! stands.

use super::{History, IntegrationConfig, Manifest, RepoEntry, RepoPath, WorkweaveConfig};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap};
use toml_edit::{DocumentMut, InlineTable, Item, Key, Table, Value};

#[derive(Serialize)]
struct Fields<'a> {
    #[serde(skip_serializing_if = "is_unset")]
    history: &'a Option<History>,
    repositories: &'a BTreeMap<RepoPath, RepoEntry>,
    integrations: &'a BTreeMap<String, IntegrationConfig>,
    #[serde(skip_serializing_if = "is_unset")]
    workweave: &'a Option<WorkweaveConfig>,
}

fn is_unset<T>(field: &&Option<T>) -> bool {
    field.is_none()
}

impl<'a> Fields<'a> {
    fn of(manifest: &'a Manifest) -> Self {
        Fields {
            history: &manifest.history,
            repositories: &manifest.repositories,
            integrations: &manifest.integrations,
            workweave: &manifest.workweave,
        }
    }
}

pub(super) fn declared(manifest: &Manifest) -> anyhow::Result<toml::Table> {
    toml::Table::try_from(Fields::of(manifest)).map_err(|e| {
        crate::untokened!(InternalInvariant, "failed to serialize the manifest: {e:#}")
    })
}

fn serializer_text(manifest: &Manifest) -> anyhow::Result<String> {
    toml::to_string(&Fields::of(manifest)).map_err(|e| {
        crate::untokened!(InternalInvariant, "failed to serialize the manifest: {e:#}")
    })
}

/// `text`, which declares `before`, edited to declare `after`.
pub(super) fn edit(text: &str, before: &Manifest, after: &Manifest) -> anyhow::Result<String> {
    let was = declared(before)?;
    let wanted = declared(after)?;
    if was == wanted {
        return Ok(text.to_owned());
    }
    let canon: DocumentMut = serializer_text(after)?.parse().map_err(|e| {
        crate::untokened!(
            InternalInvariant,
            "the serialized manifest does not parse: {e:#}"
        )
    })?;
    let (header, rest) = split_header(text.strip_prefix('\u{feff}').unwrap_or(text));
    let mut doc: DocumentMut = format!("\n{rest}")
        .parse()
        .map_err(|e| crate::untokened!(InternalInvariant, "rwv.toml does not parse: {e:#}"))?;

    let mut plan = Plan::default();
    edit_table(
        Tbl::Std(doc.as_table_mut()),
        &mut Vec::new(),
        &was,
        &wanted,
        canon.as_table(),
        &mut plan,
        None,
    );
    plan.renumber(doc.as_table_mut());

    let edited = in_the_form_of(
        text,
        format!("{header}{}", doc.to_string().trim_start_matches('\n')),
    );
    let reread = Manifest::from_toml_str(&edited).map_err(|e| {
        crate::untokened!(
            InternalInvariant,
            "the edited rwv.toml does not parse: {e:#}"
        )
    })?;
    if declared(&reread)? != wanted {
        return Err(crate::untokened!(
            InternalInvariant,
            "the edited rwv.toml does not declare the change made to it"
        ));
    }
    Ok(edited)
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Seg {
    Key(String),
    Index(usize),
}

enum Tbl<'a> {
    Std(&'a mut Table),
    Inline(&'a mut InlineTable),
}

impl Tbl<'_> {
    fn remove(&mut self, key: &str) -> Option<Item> {
        match self {
            Tbl::Std(t) => t.remove(key),
            Tbl::Inline(t) => t.remove(key).map(Item::Value),
        }
    }
}

/// Where each new standard table goes: after the table holding position `.1`,
/// in the order the tables were planned.
#[derive(Default)]
struct Plan {
    placed: Vec<(Vec<Seg>, usize)>,
    revealed: Vec<(Vec<Seg>, usize)>,
}

impl Plan {
    fn renumber(&self, root: &mut Table) {
        let new: HashMap<&[Seg], (usize, usize)> = self
            .placed
            .iter()
            .enumerate()
            .map(|(seq, (path, after))| (path.as_slice(), (*after, seq)))
            .chain(
                self.revealed
                    .iter()
                    .map(|(path, at)| (path.as_slice(), (*at, 0))),
            )
            .collect();
        let mut order: Vec<((usize, usize, usize), Vec<Seg>)> = Vec::new();
        visit_tables(root, &mut Vec::new(), &mut |path, table| {
            let key = match new.get(path) {
                Some((after, seq)) => (*after, 1, *seq),
                None => match table.position() {
                    Some(pos) => (pos, 0, 0),
                    None => return,
                },
            };
            order.push((key, path.to_vec()));
        });
        order.sort();
        for (n, (_, path)) in order.into_iter().enumerate() {
            if let Some(table) = table_at(root, &path) {
                table.set_position(n + 1);
            }
        }
    }
}

fn visit_tables(table: &Table, path: &mut Vec<Seg>, f: &mut impl FnMut(&[Seg], &Table)) {
    for (key, item) in table.iter() {
        path.push(Seg::Key(key.to_owned()));
        match item {
            Item::Table(sub) => {
                if !sub.is_dotted() && !sub.is_implicit() {
                    f(path, sub);
                }
                visit_tables(sub, path, f);
            }
            Item::ArrayOfTables(array) => {
                for (i, sub) in array.iter().enumerate() {
                    path.push(Seg::Index(i));
                    f(path, sub);
                    visit_tables(sub, path, f);
                    path.pop();
                }
            }
            _ => {}
        }
        path.pop();
    }
}

fn table_at<'a>(root: &'a mut Table, path: &[Seg]) -> Option<&'a mut Table> {
    let mut table = root;
    let mut segs = path.iter().peekable();
    while let Some(seg) = segs.next() {
        let Seg::Key(key) = seg else { return None };
        table = match table.get_mut(key)? {
            Item::Table(sub) => sub,
            Item::ArrayOfTables(array) => match segs.next() {
                Some(Seg::Index(i)) => array.get_mut(*i)?,
                _ => return None,
            },
            _ => return None,
        };
    }
    Some(table)
}

/// The lowest and highest position among the standard tables of `item`.
fn extent(item: &Item) -> Option<(usize, usize)> {
    let mut span: Option<(usize, usize)> = None;
    let mut widen = |pos: usize| {
        span = Some(match span {
            Some((lo, hi)) => (lo.min(pos), hi.max(pos)),
            None => (pos, pos),
        });
    };
    match item {
        Item::Table(table) => {
            if let (false, false, Some(pos)) =
                (table.is_dotted(), table.is_implicit(), table.position())
            {
                widen(pos);
            }
            visit_tables(table, &mut Vec::new(), &mut |_, sub| {
                if let Some(pos) = sub.position() {
                    widen(pos);
                }
            });
        }
        Item::ArrayOfTables(array) => {
            for sub in array.iter() {
                if let Some(pos) = sub.position() {
                    widen(pos);
                }
                visit_tables(sub, &mut Vec::new(), &mut |_, nested| {
                    if let Some(pos) = nested.position() {
                        widen(pos);
                    }
                });
            }
        }
        _ => {}
    }
    span
}

fn max_position(root: &Table) -> usize {
    let mut max = 0;
    visit_tables(root, &mut Vec::new(), &mut |_, t| {
        max = max.max(t.position().unwrap_or(0));
    });
    max
}

fn is_header_table(item: &Item) -> bool {
    matches!(item, Item::Table(t) if !t.is_dotted()) || item.is_array_of_tables()
}

fn is_line_item(item: &Item) -> bool {
    matches!(item, Item::Value(_)) || matches!(item, Item::Table(t) if t.is_dotted())
}

fn rank(canon: &Table, key: &str) -> Option<usize> {
    canon.iter().position(|(k, _)| k == key)
}

/// `parent_position` is the position of `doc`'s own header when it has one.
fn edit_table(
    mut doc: Tbl<'_>,
    path: &mut Vec<Seg>,
    was: &toml::Table,
    wanted: &toml::Table,
    canon: &Table,
    plan: &mut Plan,
    parent_position: Option<usize>,
) {
    for key in was.keys().filter(|k| !wanted.contains_key(*k)) {
        if let Some(removed) = doc.remove(key) {
            reveal_emptied(&mut doc, path, removed, plan);
        }
    }
    let empty = toml::Table::new();
    for (key, canon_item) in canon.iter() {
        let Some(after) = wanted.get(key) else {
            continue;
        };
        let before = was.get(key);
        if before == Some(after) {
            continue;
        }
        path.push(Seg::Key(key.to_owned()));
        match &mut doc {
            Tbl::Std(table) => {
                let own_position = match table.get(key) {
                    Some(Item::Table(t)) if !t.is_implicit() && !t.is_dotted() => t.position(),
                    _ => parent_position,
                };
                match (table.get_mut(key), after, canon_item) {
                    (Some(Item::Table(sub)), toml::Value::Table(after), Item::Table(canon_sub)) => {
                        let before = before.and_then(toml::Value::as_table).unwrap_or(&empty);
                        edit_table(
                            Tbl::Std(sub),
                            path,
                            before,
                            after,
                            canon_sub,
                            plan,
                            own_position,
                        );
                    }
                    (
                        Some(Item::Value(Value::InlineTable(sub))),
                        toml::Value::Table(after),
                        Item::Table(canon_sub),
                    ) => {
                        let before = before.and_then(toml::Value::as_table).unwrap_or(&empty);
                        edit_table(
                            Tbl::Inline(sub),
                            path,
                            before,
                            after,
                            canon_sub,
                            plan,
                            own_position,
                        );
                    }
                    (Some(Item::Value(value)), _, Item::Value(canon_value)) => {
                        replace_value(value, canon_value);
                    }
                    (Some(_), _, _) => {
                        let at = table.get(key).and_then(extent).map(|(lo, _)| lo - 1);
                        table.remove(key);
                        insert_std(
                            table,
                            key,
                            canon,
                            canon_item,
                            path,
                            plan,
                            at,
                            parent_position,
                        );
                    }
                    (None, _, _) => {
                        insert_std(
                            table,
                            key,
                            canon,
                            canon_item,
                            path,
                            plan,
                            None,
                            parent_position,
                        );
                    }
                }
            }
            Tbl::Inline(table) => match (table.get_mut(key), after, canon_item) {
                (
                    Some(Value::InlineTable(sub)),
                    toml::Value::Table(after),
                    Item::Table(canon_sub),
                ) => {
                    let before = before.and_then(toml::Value::as_table).unwrap_or(&empty);
                    edit_table(
                        Tbl::Inline(sub),
                        path,
                        before,
                        after,
                        canon_sub,
                        plan,
                        parent_position,
                    );
                }
                (Some(value), _, Item::Value(canon_value)) => replace_value(value, canon_value),
                (_, _, _) => insert_inline(table, key, canon_item),
            },
        }
        path.pop();
    }
}

/// An implicit table left with nothing under it would vanish from the text,
/// taking the key with it; it gets a header where its last table stood.
fn reveal_emptied(doc: &mut Tbl<'_>, path: &[Seg], removed: Item, plan: &mut Plan) {
    let Tbl::Std(parent) = doc else { return };
    if path.is_empty() || !parent.is_implicit() || !parent.is_empty() {
        return;
    }
    let Item::Table(removed) = removed else {
        return;
    };
    parent.set_implicit(false);
    parent.decor_mut().set_prefix("\n");
    if let Some(pos) = extent(&Item::Table(removed)).map(|(lo, _)| lo) {
        parent.set_position(pos);
        plan.revealed.push((path.to_vec(), pos));
    }
}

fn replace_value(value: &mut Value, canon: &Value) {
    let decor = value.decor().clone();
    *value = canon.clone();
    *value.decor_mut() = decor;
}

fn insert_inline(table: &mut InlineTable, key: &str, canon_item: &Item) {
    let mut value = match canon_item.clone() {
        Item::Value(value) => value,
        Item::Table(sub) => {
            let mut inline = sub.into_inline_table();
            inline.fmt();
            Value::InlineTable(inline)
        }
        _ => return,
    };
    let trailing = table.iter_mut().last().and_then(|(_, last)| {
        let suffix = last.decor().suffix().cloned();
        last.decor_mut().set_suffix("");
        suffix
    });
    value.decor_mut().set_prefix(" ");
    if let Some(suffix) = trailing {
        value.decor_mut().set_suffix(suffix);
    }
    table.insert(key, value);
}

#[allow(clippy::too_many_arguments)]
fn insert_std(
    table: &mut Table,
    key: &str,
    canon: &Table,
    canon_item: &Item,
    path: &[Seg],
    plan: &mut Plan,
    at: Option<usize>,
    parent_position: Option<usize>,
) {
    let canon_key = canon.key(key).cloned().unwrap_or_else(|| Key::new(key));
    match canon_item {
        Item::Table(canon_sub) => {
            let siblings: Vec<&Item> = table
                .iter()
                .filter(|(k, _)| *k != key)
                .map(|(_, item)| item)
                .collect();
            let inline_siblings = !siblings.is_empty()
                && siblings
                    .iter()
                    .all(|item| matches!(item, Item::Value(Value::InlineTable(_))));
            let dotted_siblings = table.is_dotted()
                || (!siblings.is_empty()
                    && siblings
                        .iter()
                        .all(|item| matches!(item, Item::Table(t) if t.is_dotted())));
            if inline_siblings {
                let mut inline = canon_sub.clone().into_inline_table();
                inline.fmt();
                insert_ordered(
                    table,
                    &Key::new(key),
                    Item::Value(Value::InlineTable(inline)),
                    canon,
                );
            } else if dotted_siblings {
                let mut dotted = canon_sub.clone();
                make_dotted(&mut dotted);
                insert_ordered(table, &Key::new(key), Item::Table(dotted), canon);
            } else {
                let after = at.unwrap_or_else(|| anchor(table, key, canon, parent_position));
                let mut new = canon_sub.clone();
                new.decor_mut().set_prefix("\n");
                let mut sub_path = path.to_vec();
                plan_tables(&new, &mut sub_path, after, plan);
                table.insert_formatted(&canon_key, Item::Table(new));
            }
        }
        item => insert_ordered(table, &canon_key, item.clone(), canon),
    }
}

fn make_dotted(table: &mut Table) {
    table.set_dotted(true);
    for (_, item) in table.iter_mut() {
        if let Item::Table(sub) = item {
            make_dotted(sub);
        }
    }
}

fn plan_tables(table: &Table, path: &mut Vec<Seg>, after: usize, plan: &mut Plan) {
    if !table.is_implicit() {
        plan.placed.push((path.clone(), after));
    }
    let mut subs: Vec<(Option<usize>, &str, &Table)> = table
        .iter()
        .filter_map(|(k, item)| match item {
            Item::Table(sub) if !sub.is_dotted() => Some((sub.position(), k, sub)),
            _ => None,
        })
        .collect();
    subs.sort_by_key(|(pos, _, _)| *pos);
    for (_, key, sub) in subs {
        path.push(Seg::Key(key.to_owned()));
        plan_tables(sub, path, after, plan);
        path.pop();
    }
}

/// The position a new table under `table` goes after: among its siblings at
/// its serializer place when they stand in serializer order, else after the
/// last of them.
fn anchor(table: &Table, key: &str, canon: &Table, parent_position: Option<usize>) -> usize {
    let mut siblings: Vec<(usize, usize, Option<usize>)> = table
        .iter()
        .filter(|(k, item)| *k != key && is_header_table(item))
        .filter_map(|(k, item)| extent(item).map(|(lo, hi)| (lo, hi, rank(canon, k))))
        .collect();
    if siblings.is_empty() {
        return parent_position.unwrap_or_else(|| max_position(table));
    }
    siblings.sort();
    let ranks: Vec<Option<usize>> = siblings.iter().map(|(_, _, r)| *r).collect();
    let in_order = ranks.iter().all(Option::is_some) && ranks.windows(2).all(|w| w[0] < w[1]);
    let last = siblings.iter().map(|(_, hi, _)| *hi).max().unwrap_or(0);
    let Some(own) = rank(canon, key).filter(|_| in_order) else {
        return last;
    };
    match siblings
        .iter()
        .rev()
        .find(|(_, _, r)| r.is_some_and(|r| r < own))
    {
        Some((_, hi, _)) => *hi,
        None => siblings[0].0 - 1,
    }
}

/// Inserts a line item at its serializer place when the table's line items
/// already stand in serializer order, else after them.
fn insert_ordered(table: &mut Table, key: &Key, item: Item, canon: &Table) {
    let lines: Vec<String> = table
        .iter()
        .filter(|(_, item)| is_line_item(item))
        .map(|(k, _)| k.to_owned())
        .collect();
    let ranks: Vec<Option<usize>> = lines.iter().map(|k| rank(canon, k)).collect();
    let in_order = ranks.iter().all(Option::is_some) && ranks.windows(2).all(|w| w[0] < w[1]);
    let own = rank(canon, key.get());
    let later: Vec<&String> = match own.filter(|_| in_order) {
        Some(own) => lines
            .iter()
            .zip(&ranks)
            .filter(|(_, r)| r.is_some_and(|r| r > own))
            .map(|(k, _)| k)
            .collect(),
        None => Vec::new(),
    };
    let moved: Vec<(Key, Item)> = later.iter().filter_map(|k| table.remove_entry(k)).collect();
    table.insert_formatted(key, item);
    for (k, item) in moved {
        table.insert_formatted(&k, item);
    }
}

/// The comments and blank lines at the top of `text` up to its last blank
/// line, which are the file's header, and the rest. A comment with no blank
/// line between it and what follows belongs to what follows.
fn split_header(text: &str) -> (&str, &str) {
    let (mut seen, mut end) = (0, 0);
    for line in text.split_inclusive('\n') {
        let blank = line.trim().is_empty();
        if !blank && !line.trim_start().starts_with('#') {
            break;
        }
        seen += line.len();
        if blank && line.ends_with('\n') {
            end = seen;
        }
    }
    text.split_at(end)
}

fn in_the_form_of(original: &str, mut edited: String) -> String {
    if !original.is_empty() && !original.ends_with('\n') && edited.ends_with('\n') {
        edited.pop();
    }
    let crlf = original.matches("\r\n").count();
    let lf = original.matches('\n').count() - crlf;
    let body = if crlf > lf {
        edited.replace("\r\n", "\n").replace('\n', "\r\n")
    } else {
        edited
    };
    match original.starts_with('\u{feff}') {
        true => format!("\u{feff}{body}"),
        false => body,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{IntegrationConfig, Role, SubmoduleRecursion, WorkweaveConfig};

    const APP: &str = "github/a/app";
    const DOCS: &str = "github/a/docs";
    const LIB: &str = "github/a/lib";

    fn path(p: &str) -> RepoPath {
        RepoPath::new(p).unwrap()
    }

    fn entry(name: &str) -> RepoEntry {
        Manifest::from_toml_str(&format!(
            "[repositories.\"github/a/{name}\"]\ntype = \"git\"\nurl = \"https://example.com/{name}.git\"\nbranch = \"main\"\nrole = \"owned\"\n"
        ))
        .unwrap()
        .get_entry(&path(&format!("github/a/{name}")))
        .unwrap()
        .clone()
    }

    fn change_entry(member: &str, f: impl FnOnce(&mut RepoEntry)) -> impl FnOnce(&mut Manifest) {
        let member = path(member);
        move |m: &mut Manifest| {
            let mut e = m.get_entry(&member).unwrap().clone();
            f(&mut e);
            m.insert_repo(member, e);
        }
    }

    type Change = (&'static str, Box<dyn Fn(&mut Manifest)>);

    fn changes() -> Vec<Change> {
        let set = |member: &'static str, f: fn(&mut RepoEntry)| -> Box<dyn Fn(&mut Manifest)> {
            Box::new(move |m: &mut Manifest| change_entry(member, f)(m))
        };
        let add = |name: &'static str| -> Box<dyn Fn(&mut Manifest)> {
            Box::new(move |m: &mut Manifest| {
                m.insert_repo(path(&format!("github/a/{name}")), entry(name));
            })
        };
        vec![
            ("set-role", set(APP, |e| e.role = Role::Fork)),
            (
                "set-url",
                set(DOCS, |e| {
                    e.url = "https://example.com/moved.git".parse().unwrap()
                }),
            ),
            (
                "set-branch",
                set(APP, |e| e.branch = entry("trunk").branch.clone()),
            ),
            (
                "set-history",
                set(DOCS, |e| e.history = Some(History::Linear)),
            ),
            ("unset-history", set(LIB, |e| e.history = None)),
            (
                "change-history",
                set(LIB, |e| e.history = Some(History::FfOnly)),
            ),
            (
                "skip-submodules",
                set(LIB, |e| e.recurse_submodules = SubmoduleRecursion::Skip),
            ),
            ("add-first", add("aaa")),
            ("add-middle", add("cat")),
            ("add-last", add("zed")),
            (
                "remove-app",
                Box::new(|m: &mut Manifest| {
                    m.remove_repo(&path(APP));
                }),
            ),
            (
                "remove-lib",
                Box::new(|m: &mut Manifest| {
                    m.remove_repo(&path(LIB));
                }),
            ),
            (
                "remove-all",
                Box::new(|m: &mut Manifest| m.retain_repos(|_, _| false)),
            ),
            (
                "set-top-history",
                Box::new(|m: &mut Manifest| m.history = Some(History::Merge)),
            ),
            (
                "add-integration",
                Box::new(|m: &mut Manifest| {
                    m.integrations.insert(
                        "static-files".to_owned(),
                        IntegrationConfig::from_toml("enabled = true\nfiles = [\"a.md\"]"),
                    );
                }),
            ),
            (
                "set-workweave",
                Box::new(|m: &mut Manifest| {
                    m.workweave = Some(WorkweaveConfig {
                        link: vec![".env".to_owned()],
                        copy: Vec::new(),
                    })
                }),
            ),
        ]
    }

    const SORTED: &str = r#"[repositories."github/a/app"]
type = "git"
url = "https://example.com/app.git"
branch = "main"
role = "owned"

[repositories."github/a/docs"]
type = "git"
url = "https://example.com/docs.git"
branch = "main"
role = "owned"

[repositories."github/a/lib"]
type = "git"
url = "https://example.com/lib.git"
branch = "main"
role = "owned"
history = "merge"

[integrations]
"#;

    const HAND: &str = r#"# The members.
[repositories."github/a/lib"]
type = "git"
url = "https://example.com/lib.git" # upstream
branch = "main"
role = "owned"
history = "merge"

# The app.
[repositories."github/a/app"]
role = "owned"
type = "git"
url = "https://example.com/app.git"
branch = "main"

[integrations.static-files]
enabled = false

[repositories."github/a/docs"]
type = "git"
url = "https://example.com/docs.git"
branch = "main" # tracked
role = "owned"
# end of file"#;

    const INLINE: &str = r#"[repositories]
"github/a/app" = { type = "git", url = "https://example.com/app.git", branch = "main", role = "owned" }
"github/a/docs" = { type = "git", url = "https://example.com/docs.git", branch = "main", role = "owned" }
"github/a/lib" = { type = "git", url = "https://example.com/lib.git", branch = "main", role = "owned", history = "merge" }
"#;

    const DOTTED: &str = r#"[repositories]
"github/a/app".type = "git"
"github/a/app".url = "https://example.com/app.git"
"github/a/app".branch = "main"
"github/a/app".role = "owned"
"github/a/docs".type = "git"
"github/a/docs".url = "https://example.com/docs.git"
"github/a/docs".branch = "main"
"github/a/docs".role = "owned"
"github/a/lib".type = "git"
"github/a/lib".url = "https://example.com/lib.git"
"github/a/lib".branch = "main"
"github/a/lib".role = "owned"
"github/a/lib".history = "merge"
"#;

    const MIXED: &str = r#"# Repoweave manifest
[repositories]
"github/a/app" = { type = "git", url = "https://example.com/app.git", branch = "main", role = "owned" }

[repositories."github/a/lib"]
type = "git"
url = "https://example.com/lib.git"
branch = "main"
role = "owned"
history = "merge"

[repositories."github/a/docs"]
type = "git"
url = "https://example.com/docs.git"
branch = "main"
role = "owned"
"#;

    const ROOT_DOTTED: &str = r#"repositories."github/a/app".type = "git"
repositories."github/a/app".url = "https://example.com/app.git"
repositories."github/a/app".branch = "main"
repositories."github/a/app".role = "owned"
repositories."github/a/docs".type = "git"
repositories."github/a/docs".url = "https://example.com/docs.git"
repositories."github/a/docs".branch = "main"
repositories."github/a/docs".role = "owned"
repositories."github/a/lib".type = "git"
repositories."github/a/lib".url = "https://example.com/lib.git"
repositories."github/a/lib".branch = "main"
repositories."github/a/lib".role = "owned"
repositories."github/a/lib".history = "merge"
"#;

    fn layouts() -> Vec<(&'static str, String)> {
        vec![
            ("root-dotted", ROOT_DOTTED.to_owned()),
            ("sorted", SORTED.to_owned()),
            ("hand", HAND.to_owned()),
            ("inline", INLINE.to_owned()),
            ("dotted", DOTTED.to_owned()),
            ("mixed", MIXED.to_owned()),
            ("crlf", HAND.replace('\n', "\r\n")),
            ("bom", format!("\u{feff}{HAND}")),
            ("final-newline", format!("{HAND}\n")),
        ]
    }

    fn lines(text: &str) -> Vec<&str> {
        text.lines().collect()
    }

    fn headers(text: &str) -> Vec<String> {
        text.lines()
            .filter(|l| l.trim_start().starts_with('['))
            .map(|l| l.trim_end_matches('\r').to_owned())
            .collect()
    }

    #[test]
    fn every_change_on_every_layout_declares_what_the_change_made() {
        for (layout, text) in layouts() {
            for (name, change) in changes() {
                let before = Manifest::from_toml_str(&text).unwrap();
                let mut wanted = before.clone();
                change(&mut wanted);
                let (edited, _) = Manifest::edit_text(&text, |m| change(m))
                    .unwrap_or_else(|e| panic!("{layout}/{name}: {e:#}"));
                let reread = Manifest::from_toml_str(&edited)
                    .unwrap_or_else(|e| panic!("{layout}/{name}: {e:#}\n{edited}"));
                assert!(reread.declares_same(&wanted), "{layout}/{name}:\n{edited}");
            }
        }
    }

    #[test]
    fn no_table_moves() {
        for (layout, text) in layouts() {
            for (name, change) in changes() {
                let (edited, _) = Manifest::edit_text(&text, |m| change(m)).unwrap();
                let was = headers(&text);
                let now = headers(&edited);
                let kept_was: Vec<&String> = was.iter().filter(|h| now.contains(h)).collect();
                let kept_now: Vec<&String> = now.iter().filter(|h| was.contains(h)).collect();
                assert_eq!(kept_was, kept_now, "{layout}/{name}:\n{edited}");
            }
        }
    }

    #[test]
    fn comments_and_untouched_lines_are_kept() {
        for (layout, text) in layouts() {
            for (name, change) in changes() {
                if name.starts_with("remove") {
                    continue;
                }
                let (edited, _) = Manifest::edit_text(&text, |m| change(m)).unwrap();
                let now = lines(&edited);
                let gone: Vec<&str> = lines(&text)
                    .into_iter()
                    .filter(|l| !now.contains(l))
                    .collect();
                assert!(
                    gone.len() <= 1,
                    "{layout}/{name}: at most the one line the change set is replaced, \
                     gone: {gone:?}\n{edited}"
                );
                assert!(
                    gone.iter().all(|l| !l.trim_start().starts_with('#')),
                    "{layout}/{name}: a comment line went: {gone:?}"
                );
            }
        }
    }

    #[test]
    fn line_endings_and_the_byte_order_mark_are_kept() {
        for (name, change) in changes() {
            let crlf = HAND.replace('\n', "\r\n");
            let (edited, _) = Manifest::edit_text(&crlf, |m| change(m)).unwrap();
            assert!(
                !edited.replace("\r\n", "").contains('\n'),
                "{name}: every line ends as the file's do:\n{edited:?}"
            );
            let (edited, _) =
                Manifest::edit_text(&format!("\u{feff}{SORTED}"), |m| change(m)).unwrap();
            assert!(edited.starts_with('\u{feff}'), "{name}");
        }
    }

    #[test]
    fn on_a_file_in_serializer_form_the_edit_is_what_the_serializer_writes() {
        let sorted = serializer_text(&Manifest::from_toml_str(SORTED).unwrap()).unwrap();
        assert_eq!(
            sorted, SORTED,
            "fixture: SORTED is the serializer's own form"
        );
        for (name, change) in changes() {
            let mut text = SORTED.to_owned();
            for (_, then) in changes().iter().filter(|(n, _)| *n != name).take(3) {
                text = Manifest::edit_text(&text, |m| then(m)).unwrap().0;
            }
            let (edited, after) = Manifest::edit_text(&text, |m| change(m)).unwrap();
            let serialized = serializer_text(&after).unwrap();
            if name == "add-integration" {
                assert_eq!(
                    edited,
                    serialized.replace("\n[integrations.", "\n[integrations]\n\n[integrations."),
                    "the serializer drops the header of a table that gains a sub-table; \
                     the edit leaves it"
                );
            } else {
                assert_eq!(edited, serialized, "{name}");
            }
        }
    }

    #[test]
    fn a_setter_on_a_hand_ordered_file_changes_only_its_own_line() {
        let (edited, _) =
            Manifest::edit_text(HAND, change_entry(APP, |e| e.role = Role::Fork)).unwrap();
        assert_eq!(
            edited,
            HAND.replace(
                "# The app.\n[repositories.\"github/a/app\"]\nrole = \"owned\"",
                "# The app.\n[repositories.\"github/a/app\"]\nrole = \"fork\""
            )
        );
        let (edited, _) = Manifest::edit_text(
            HAND,
            change_entry(LIB, |e| {
                e.url = "https://example.com/l2.git".parse().unwrap()
            }),
        )
        .unwrap();
        assert_eq!(
            edited,
            HAND.replace(
                "url = \"https://example.com/lib.git\" # upstream",
                "url = \"https://example.com/l2.git\" # upstream"
            ),
            "the trailing comment stays"
        );
    }

    #[test]
    fn a_key_added_to_a_table_in_serializer_order_goes_at_its_place_else_at_the_end() {
        let (edited, _) = Manifest::edit_text(
            HAND,
            change_entry(LIB, |e| e.recurse_submodules = SubmoduleRecursion::Skip),
        )
        .unwrap();
        assert_eq!(
            edited,
            HAND.replace(
                "role = \"owned\"\nhistory = \"merge\"",
                "role = \"owned\"\nrecurse-submodules = false\nhistory = \"merge\""
            )
        );
        let (edited, _) = Manifest::edit_text(
            HAND,
            change_entry(APP, |e| e.history = Some(History::Linear)),
        )
        .unwrap();
        assert_eq!(
            edited,
            HAND.replace(
                "url = \"https://example.com/app.git\"\nbranch = \"main\"\n",
                "url = \"https://example.com/app.git\"\nbranch = \"main\"\nhistory = \"linear\"\n"
            ),
            "app's keys are out of serializer order, so the key goes last"
        );
    }

    #[test]
    fn a_new_table_goes_at_its_sorted_place_among_sorted_siblings_else_after_the_last() {
        let (edited, _) = Manifest::edit_text(SORTED, |m| {
            m.insert_repo(path("github/a/cat"), entry("cat"));
        })
        .unwrap();
        let docs = edited.find("[repositories.\"github/a/docs\"]").unwrap();
        let cat = edited.find("[repositories.\"github/a/cat\"]").unwrap();
        let lib = edited.find("[repositories.\"github/a/lib\"]").unwrap();
        assert!(cat < docs && docs < lib, "{edited}");

        let (edited, _) = Manifest::edit_text(HAND, |m| {
            m.insert_repo(path("github/a/cat"), entry("cat"));
        })
        .unwrap();
        let block = "\n[repositories.\"github/a/cat\"]\ntype = \"git\"\nurl = \"https://example.com/cat.git\"\nbranch = \"main\"\nrole = \"owned\"\n";
        assert_eq!(
            edited,
            HAND.replace(
                "role = \"owned\"\n# end of file",
                &format!("role = \"owned\"\n{block}# end of file")
            ),
            "after docs, the last repository table, and before the comment that ends the file"
        );
    }

    #[test]
    fn removing_every_entry_leaves_the_table_header_where_the_last_one_stood() {
        let (edited, _) = Manifest::edit_text(HAND, |m| m.retain_repos(|_, _| false)).unwrap();
        assert_eq!(
            edited,
            "[repositories]\n\n[integrations.static-files]\nenabled = false\n# end of file"
        );
    }

    #[test]
    fn a_removed_entry_takes_only_its_own_lines() {
        let (edited, _) = Manifest::edit_text(HAND, |m| {
            m.remove_repo(&path(APP));
        })
        .unwrap();
        assert_eq!(
            edited,
            HAND.replace(
                "# The app.\n[repositories.\"github/a/app\"]\nrole = \"owned\"\ntype = \"git\"\nurl = \"https://example.com/app.git\"\nbranch = \"main\"\n\n",
                ""
            )
        );
    }

    #[test]
    fn a_project_made_by_verbs_keeps_the_skeleton_and_grows_by_its_entries() {
        let (one, _) = Manifest::edit_text(Manifest::SKELETON, |m| {
            m.insert_repo(path(LIB), entry("lib"));
        })
        .unwrap();
        let (two, _) = Manifest::edit_text(&one, |m| {
            m.insert_repo(path(APP), entry("app"));
        })
        .unwrap();
        let (three, _) =
            Manifest::edit_text(&two, change_entry(LIB, |e| e.role = Role::Fork)).unwrap();
        let (four, _) = Manifest::edit_text(&three, |m| {
            m.remove_repo(&path(APP));
        })
        .unwrap();
        let app = "\n[repositories.\"github/a/app\"]\ntype = \"git\"\nurl = \"https://example.com/app.git\"\nbranch = \"main\"\nrole = \"owned\"\n";
        let lib = "\n[repositories.\"github/a/lib\"]\ntype = \"git\"\nurl = \"https://example.com/lib.git\"\nbranch = \"main\"\nrole = \"fork\"\n";
        assert_eq!(three, format!("{}{app}{lib}", Manifest::SKELETON));
        assert_eq!(four, format!("{}{lib}", Manifest::SKELETON));
        let (empty, _) = Manifest::edit_text(&four, |m| m.retain_repos(|_, _| false)).unwrap();
        assert_eq!(empty, Manifest::SKELETON);
    }

    #[test]
    fn a_top_level_key_goes_above_the_tables_and_leaves_a_comment_touching_the_first_table_on_it() {
        let (set, _) = Manifest::edit_text(HAND, |m| m.history = Some(History::Merge)).unwrap();
        assert_eq!(set, format!("history = \"merge\"\n\n{HAND}"));
        let (unset, _) = Manifest::edit_text(&set, |m| m.history = None).unwrap();
        assert_eq!(unset, HAND);
        let (set, _) = Manifest::edit_text(SORTED, |m| m.history = Some(History::Merge)).unwrap();
        let (unset, _) = Manifest::edit_text(&set, |m| m.history = None).unwrap();
        assert_eq!(unset, SORTED);
    }

    #[test]
    fn an_edit_that_declares_something_else_is_refused_not_written() {
        let mut before = Manifest::from_toml_str(SORTED).unwrap();
        change_entry(LIB, |e| e.role = Role::Fork)(&mut before);
        let mut after = before.clone();
        change_entry(LIB, |e| e.history = Some(History::Linear))(&mut after);
        let err = edit(SORTED, &before, &after).unwrap_err();
        assert!(
            format!("{err:#}").contains("does not declare the change"),
            "{err:#}"
        );
    }

    #[test]
    fn a_key_added_to_a_table_whose_keys_are_out_of_order_goes_at_its_end() {
        let text = SORTED.replace(
            "type = \"git\"\nurl = \"https://example.com/lib.git\"\nbranch = \"main\"\nrole = \"owned\"\nhistory = \"merge\"\n",
            "history = \"merge\"\ntype = \"git\"\nurl = \"https://example.com/lib.git\"\nbranch = \"main\"\nrole = \"owned\"\n",
        );
        let (edited, _) = Manifest::edit_text(
            &text,
            change_entry(LIB, |e| e.recurse_submodules = SubmoduleRecursion::Skip),
        )
        .unwrap();
        assert_eq!(
            edited,
            text.replace(
                "role = \"owned\"\n\n[integrations]",
                "role = \"owned\"\nrecurse-submodules = false\n\n[integrations]"
            )
        );
    }

    #[test]
    fn a_new_member_among_inline_members_is_written_inline() {
        let (edited, _) = Manifest::edit_text(INLINE, |m| {
            m.insert_repo(path("github/a/zed"), entry("zed"));
        })
        .unwrap();
        assert_eq!(
            edited,
            format!(
                "{INLINE}\"github/a/zed\" = {{ type = \"git\", url = \"https://example.com/zed.git\", branch = \"main\", role = \"owned\" }}\n"
            )
        );
    }

    #[test]
    fn a_file_without_a_final_newline_keeps_none_when_its_last_line_is_the_changed_one() {
        let text = SORTED
            .trim_end()
            .trim_end_matches("[integrations]")
            .to_owned();
        let text = text.trim_end().to_owned();
        assert!(text.ends_with("history = \"merge\""), "{text}");
        let (edited, _) = Manifest::edit_text(
            &text,
            change_entry(LIB, |e| e.history = Some(History::Linear)),
        )
        .unwrap();
        assert_eq!(edited, text.replace("\"merge\"", "\"linear\""));
        for crlf in [false, true] {
            let text = if crlf {
                text.replace('\n', "\r\n")
            } else {
                text.clone()
            };
            let (edited, _) = Manifest::edit_text(
                &text,
                change_entry(LIB, |e| e.history = Some(History::Linear)),
            )
            .unwrap();
            assert_eq!(
                edited,
                text.replace("\"merge\"", "\"linear\""),
                "crlf {crlf}"
            );
        }
    }

    #[test]
    fn a_file_with_mixed_line_endings_is_written_in_the_ending_most_of_its_lines_have() {
        let mostly_crlf = SORTED.replace('\n', "\r\n").replacen("\r\n", "\n", 3);
        let mostly_lf = SORTED.replacen("\n", "\r\n", 3);
        for (text, ending) in [(mostly_crlf, "\r\n"), (mostly_lf, "\n")] {
            let (edited, after) =
                Manifest::edit_text(&text, change_entry(APP, |e| e.role = Role::Fork)).unwrap();
            assert!(after.declares_same(&Manifest::from_toml_str(&edited).unwrap()));
            assert_eq!(
                edited.matches("\r\n").count(),
                if ending == "\n" {
                    0
                } else {
                    edited.matches('\n').count()
                },
                "{edited:?}"
            );
        }
    }

    const HEADED: &str = "# Our weave.\n\n# lib: vendored\n[repositories.\"github/a/lib\"]\ntype = \"git\"\nurl = \"https://example.com/lib.git\"\nbranch = \"main\"\nrole = \"owned\"\n\n[repositories.\"github/a/app\"]\ntype = \"git\"\nurl = \"https://example.com/app.git\"\nbranch = \"main\"\nrole = \"owned\"\n";

    #[test]
    fn a_comment_above_a_blank_line_is_the_files_header_and_one_below_it_belongs_to_the_first_table(
    ) {
        let (set, _) = Manifest::edit_text(HEADED, |m| m.history = Some(History::Merge)).unwrap();
        assert_eq!(
            set,
            HEADED.replace(
                "# Our weave.\n\n",
                "# Our weave.\n\nhistory = \"merge\"\n\n"
            )
        );
        let (unset, _) = Manifest::edit_text(&set, |m| m.history = None).unwrap();
        assert_eq!(unset, HEADED);

        let (removed, _) = Manifest::edit_text(HEADED, |m| {
            m.remove_repo(&path(LIB));
        })
        .unwrap();
        assert_eq!(
            removed,
            "# Our weave.\n\n[repositories.\"github/a/app\"]\ntype = \"git\"\nurl = \"https://example.com/app.git\"\nbranch = \"main\"\nrole = \"owned\"\n"
        );
    }

    #[test]
    fn a_top_level_keys_own_comment_goes_with_it() {
        let text = HEADED.replace(
            "# lib: vendored\n",
            "# the default\nhistory = \"merge\"\n\n# lib: vendored\n",
        );
        let (unset, _) = Manifest::edit_text(&text, |m| m.history = None).unwrap();
        assert_eq!(unset, HEADED);
    }

    #[test]
    fn a_top_level_key_on_root_dotted_members_goes_below_the_header() {
        let text = format!("# Our weave.\n\n{ROOT_DOTTED}");
        let (set, _) = Manifest::edit_text(&text, |m| m.history = Some(History::Merge)).unwrap();
        assert!(
            set.starts_with("# Our weave.\n\n"),
            "the header stays on top:\n{set}"
        );
    }

    #[test]
    fn layouts_whose_spelling_is_normalized_still_declare_what_the_change_made() {
        let member = |name: &str| {
            [
                ("type", "\"git\"".to_owned()),
                ("url", format!("\"https://example.com/{name}.git\"")),
                ("branch", "\"main\"".to_owned()),
                ("role", "\"owned\"".to_owned()),
            ]
        };
        let spaced = ["app", "docs", "lib"]
            .iter()
            .enumerate()
            .map(|(n, name)| {
                let head = if n == 0 {
                    "repositories ."
                } else {
                    "repositories."
                };
                let body: String = member(name)
                    .iter()
                    .map(|(k, v)| format!("{k} = {v}\n"))
                    .collect();
                format!("[{head}\"github/a/{name}\"]\n{body}\n")
            })
            .collect::<String>();
        let interleaved = (0..4)
            .flat_map(|n| {
                ["app", "docs", "lib"].map(|name| {
                    let (k, v) = &member(name)[n];
                    format!("repositories.\"github/a/{name}\".{k} = {v}\n")
                })
            })
            .collect::<String>();
        for (layout, text) in [("spaced-header", spaced), ("interleaved", interleaved)] {
            for (name, change) in changes() {
                let before = Manifest::from_toml_str(&text).unwrap();
                let mut wanted = before.clone();
                change(&mut wanted);
                let (edited, _) = Manifest::edit_text(&text, |m| change(m))
                    .unwrap_or_else(|e| panic!("{layout}/{name}: {e:#}"));
                let reread = Manifest::from_toml_str(&edited)
                    .unwrap_or_else(|e| panic!("{layout}/{name}: {e:#}\n{edited}"));
                assert!(reread.declares_same(&wanted), "{layout}/{name}:\n{edited}");
            }
        }
    }
}
