//! Every `--json` field whose value is a filesystem absolute path is a
//! `WirePath`, whose one mint spells it the way the published schemas say.
//!
//! The type holds the spelling wherever a wire field is declared as one. What
//! it cannot hold is a wire field declared as something else, which still
//! compiles: a `PathBuf`, published in whatever spelling the host wrote, or a
//! `String` filled from `.display()`. The two scans below hold that residue,
//! one keyed on the field's type and one on its name, because a field name
//! alone does not say whether a value is a path (`path` is manifest-relative
//! on most arms) and a type alone does not catch a path held in a `String`.

use std::collections::BTreeSet;
#[cfg(not(windows))]
use std::path::PathBuf;

mod common;

use common::src_scan::{production_lines, SourceLine};

/// The fields of every `JsonSchema` item in `src/`, as `(line, name, type)`.
fn wire_item_fields() -> (usize, Vec<(SourceLine, String, String)>) {
    let lines = production_lines();
    let mut fields = Vec::new();
    let mut wire_items = 0usize;
    let mut i = 0;
    while i < lines.len() {
        if !lines[i].text.trim_start().starts_with("#[") {
            i += 1;
            continue;
        }
        let mut attributes = String::new();
        while i < lines.len() && lines[i].text.trim_start().starts_with("#[") {
            attributes.push_str(&lines[i].text);
            i += 1;
        }
        let Some(header) = lines.get(i) else { break };
        let head = header.text.trim_start();
        let declares_item = ["struct ", "enum "]
            .iter()
            .any(|kw| head.starts_with(kw) || head.contains(&format!("pub {kw}")));
        if !declares_item || !attributes.contains("JsonSchema") {
            continue;
        }
        wire_items += 1;

        let mut depth = 0i32;
        let mut opened = false;
        while i < lines.len() {
            let text = lines[i].text.clone();
            depth += text.matches('{').count() as i32 - text.matches('}').count() as i32;
            opened |= text.contains('{');
            let field = text
                .trim()
                .trim_start_matches("pub ")
                .strip_suffix(',')
                .and_then(|decl| decl.split_once(": "));
            if let Some((name, ty)) = field {
                if name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                    fields.push((lines[i].clone(), name.to_owned(), ty.trim().to_owned()));
                }
            }
            i += 1;
            if opened && depth <= 0 {
                break;
            }
            if !opened && text.trim_end().ends_with(';') {
                break;
            }
        }
    }
    (wire_items, fields)
}

/// A `PathBuf` reaches `--json` in the host's spelling: on Windows the
/// verbatim prefix and backslashes the documented `jq | xargs` recipe cannot
/// carry. A sub-kind generic over its path representation is held here too,
/// since naming its `PathBuf` instantiation in a wire type spells `PathBuf`.
#[test]
fn no_wire_type_holds_a_path_buf() {
    let (wire_items, fields) = wire_item_fields();
    let raw: Vec<String> = fields
        .iter()
        .filter(|(_, _, ty)| ty.contains("PathBuf"))
        .map(|(line, name, ty)| format!("{}: {name}: {ty}", line.site()))
        .collect();
    assert!(
        raw.is_empty(),
        "these `--json` fields hold a `PathBuf`; declare them `WirePath` and mint \
         them with `WirePath::of`:\n{raw:#?}"
    );
    assert!(
        wire_items >= 50,
        "the scan found only {wire_items} `--json` types; it has stopped seeing the \
         population it is supposed to police"
    );
}

/// A field the wire publishes under a path's name is declared a `WirePath`,
/// or the path parameter of a sub-kind whose wire instantiation is one.
#[test]
fn every_wire_path_field_is_declared_a_wire_path() {
    // Every name a `--json` type declares a `WirePath` under, less `path`,
    // which is manifest-relative on most arms and a location on a few.
    const WIRE_PATH_FIELDS: &[&str] = &[
        "absolute_path",
        "actual_store_path",
        "canonical_path",
        "expected_store_path",
        "index_path",
        "legacy_path",
        "manifest_path",
        "marker_path",
        "marker_primary",
        "member_config",
        "missing_dir",
        "other_store_path",
        "owner",
        "parent_path",
        "primary",
        "recorded_owner",
        "recorded_path",
        "repo",
        "repo_path",
        "root",
        "shadowed_by",
        "source",
        "state_path",
        "store_path",
        "target",
        "weave_config",
        "weave_store_path",
        "workspace",
        "workspace_dir",
        "workweave_dir",
        "worktree_path",
    ];
    const DECLARED_AS_A_PATH: &[&str] = &[
        "WirePath",
        "Option<WirePath>",
        "crate::path_spelling::WirePath",
        "Option<crate::path_spelling::WirePath>",
        "P",
    ];
    let (_, fields) = wire_item_fields();
    let mut misdeclared = Vec::new();
    let mut declared = 0usize;
    let mut live = BTreeSet::new();
    for (line, name, ty) in &fields {
        if !WIRE_PATH_FIELDS.contains(&name.as_str()) {
            continue;
        }
        live.insert(name.clone());
        if DECLARED_AS_A_PATH.contains(&ty.as_str()) {
            declared += 1;
        } else {
            misdeclared.push(format!("{}: {name}: {ty}", line.site()));
        }
    }
    let dead: Vec<&&str> = WIRE_PATH_FIELDS
        .iter()
        .filter(|f| !live.contains(**f))
        .collect();

    assert!(
        misdeclared.is_empty(),
        "these `--json` fields carry a path under a path's name without being a \
         `WirePath`, so nothing holds their spelling:\n{misdeclared:#?}"
    );
    assert!(
        declared >= 60,
        "the scan found only {declared} wire path fields; it has stopped seeing the \
         declarations it is supposed to police"
    );
    assert!(
        dead.is_empty(),
        "these entries name no field of a `--json` type — the field was renamed or \
         removed and the list still counts it as covered:\n{dead:#?}"
    );
}

/// On this platform the mint is the identity, so the wire output is byte-for-
/// byte what it was before the seam existed. That is the claim that makes the
/// decision free where the users are, and it is asserted rather than assumed.
#[cfg(not(windows))]
#[test]
fn unix_wire_output_is_unchanged_by_the_seam() {
    for raw in [
        "/weave/github/acme/server",
        "/weave/projects/chatly/web-app",
        "/tmp/.tmpABC/ws",
    ] {
        let p = PathBuf::from(raw);
        assert_eq!(
            repoweave::path_spelling::WirePath::of(&p).as_str(),
            raw,
            "the wire mint must not alter a Unix path"
        );
    }
}
