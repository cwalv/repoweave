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
    let mut doc: DocumentMut = text
        .parse()
        .map_err(|e| crate::untokened!(InternalInvariant, "rwv.toml does not parse: {e:#}"))?;

    let first_before = first_element(&doc);
    let lead = first_before.as_ref().and_then(|id| prefix_of(&mut doc, id));
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
    if let (Some(first_before), Some(lead)) = (first_before, lead) {
        keep_leading_decor(&mut doc, &first_before, lead);
    }

    let edited = splice(text, &doc.to_string());
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

#[derive(Debug, Clone, PartialEq, Eq)]
enum First {
    RootKey(String),
    Table(Vec<Seg>),
}

fn first_element(doc: &DocumentMut) -> Option<First> {
    if let Some((key, _)) = doc.as_table().iter().find(|(_, item)| item.is_value()) {
        return Some(First::RootKey(key.to_owned()));
    }
    let mut first: Option<(usize, Vec<Seg>)> = None;
    visit_tables(doc.as_table(), &mut Vec::new(), &mut |path, table| {
        if let Some(pos) = table.position() {
            if first.as_ref().is_none_or(|(lo, _)| pos < *lo) {
                first = Some((pos, path.to_vec()));
            }
        }
    });
    first.map(|(_, path)| First::Table(path))
}

fn prefix_of(doc: &mut DocumentMut, id: &First) -> Option<String> {
    let decor = match id {
        First::RootKey(key) => doc.as_table().key(key).map(Key::leaf_decor),
        First::Table(path) => table_at(doc.as_table_mut(), path).map(|t| &*t.decor_mut()),
    }?;
    Some(
        decor
            .prefix()
            .and_then(|p| p.as_str())
            .unwrap_or_default()
            .to_owned(),
    )
}

fn set_prefix(doc: &mut DocumentMut, id: &First, prefix: String) {
    match id {
        First::RootKey(key) => {
            if let Some(mut key) = doc.as_table_mut().key_mut(key) {
                key.leaf_decor_mut().set_prefix(prefix);
            }
        }
        First::Table(path) => {
            if let Some(table) = table_at(doc.as_table_mut(), path) {
                table.decor_mut().set_prefix(prefix);
            }
        }
    }
}

/// What stood at the top of the file before the edit stays there: a new first
/// element takes over the old one's leading comments, and the old one is set
/// off by a blank line; when the first element is removed, its successor
/// takes them over.
fn keep_leading_decor(doc: &mut DocumentMut, first_before: &First, lead: String) {
    let Some(first_after) = first_element(doc) else {
        return;
    };
    if &first_after == first_before {
        return;
    }
    if prefix_of(doc, first_before).is_some() {
        set_prefix(doc, first_before, "\n".to_owned());
        set_prefix(doc, &first_after, lead);
    } else {
        let own = prefix_of(doc, &first_after).unwrap_or_default();
        let kept = own
            .split_inclusive('\n')
            .skip_while(|line| line.trim().is_empty())
            .collect::<String>();
        set_prefix(doc, &first_after, format!("{lead}{kept}"));
    }
}

/// `edited`, toml_edit's rendering, laid back over `original` line by line:
/// every line the edit left keeps its own bytes and line ending, and the byte
/// order mark stays, neither of which the rendering carries.
fn splice(original: &str, edited: &str) -> String {
    let (bom, body) = match original.strip_prefix('\u{feff}') {
        Some(body) => ("\u{feff}", body),
        None => ("", original),
    };
    let old: Vec<(&str, &str)> = body
        .split_inclusive('\n')
        .map(|line| match line.strip_suffix("\r\n") {
            Some(content) => (content, "\r\n"),
            None => match line.strip_suffix('\n') {
                Some(content) => (content, "\n"),
                None => (line, ""),
            },
        })
        .collect();
    let new: Vec<&str> = edited.lines().collect();
    let first_ending = old
        .iter()
        .map(|(_, ending)| *ending)
        .find(|e| !e.is_empty())
        .unwrap_or("\n");
    let ends_open = old.last().is_some_and(|(_, ending)| ending.is_empty());

    let matched = common_lines(&old.iter().map(|(c, _)| *c).collect::<Vec<_>>(), &new);
    let mut out = String::from(bom);
    let mut ending = first_ending;
    for (j, line) in new.iter().enumerate() {
        let last = j + 1 == new.len();
        match matched[j] {
            Some(i) => {
                let (content, own) = old[i];
                if !own.is_empty() {
                    ending = own;
                }
                out.push_str(content);
                out.push_str(if last && own.is_empty() { "" } else { ending });
            }
            None => {
                out.push_str(line);
                out.push_str(if last && ends_open { "" } else { ending });
            }
        }
    }
    out
}

/// For each line of `new`, the line of `old` it is matched to by a longest
/// common subsequence.
fn common_lines(old: &[&str], new: &[&str]) -> Vec<Option<usize>> {
    let (n, m) = (old.len(), new.len());
    let mut len = vec![0u32; (n + 1) * (m + 1)];
    let at = |i: usize, j: usize| i * (m + 1) + j;
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            len[at(i, j)] = if old[i] == new[j] {
                len[at(i + 1, j + 1)] + 1
            } else {
                len[at(i + 1, j)].max(len[at(i, j + 1)])
            };
        }
    }
    let mut matched = vec![None; m];
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if old[i] == new[j] {
            matched[j] = Some(i);
            i += 1;
            j += 1;
        } else if len[at(i + 1, j)] >= len[at(i, j + 1)] {
            i += 1;
        } else {
            j += 1;
        }
    }
    matched
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
            "# The members.\n[repositories]\n\n[integrations.static-files]\nenabled = false\n# end of file"
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
    fn a_top_level_key_goes_above_the_tables_and_leaves_the_leading_comment_on_top() {
        let (set, _) = Manifest::edit_text(HAND, |m| m.history = Some(History::Merge)).unwrap();
        assert_eq!(
            set,
            HAND.replace(
                "# The members.\n",
                "# The members.\nhistory = \"merge\"\n\n"
            )
        );
        let (unset, _) = Manifest::edit_text(&set, |m| m.history = None).unwrap();
        assert_eq!(unset, HAND);
        let (set, _) = Manifest::edit_text(SORTED, |m| m.history = Some(History::Merge)).unwrap();
        let (unset, _) = Manifest::edit_text(&set, |m| m.history = None).unwrap();
        assert_eq!(unset, SORTED);
    }
}
