//! Every `--json` field whose value is a filesystem absolute path is minted by
//! one function, and the inventory of those fields is derived rather than
//! typed.
//!
//! The derivation matters more than the count. A by-name sweep for
//! `absolute_path` returns confident hits and misses `Resolution.workspace` —
//! a path published on every `--json` surface, held as a `String` minted at
//! construction precisely so that serde cannot spell it instead. The same
//! sweep would also over-select `path`, whose nine other arms are
//! manifest-relative. Field names decide nothing here; the constructing code
//! does.

use std::path::{Path, PathBuf};

mod common;

use common::src_scan::production_lines;

/// Assignments to a wire path field that do not go through the mint.
///
/// The scan is over field-assignment syntax rather than over a list of file
/// names, so a new `--json` record type is covered the day it is written.
#[test]
fn every_wire_path_field_is_minted_by_the_seam() {
    // Field names whose value is a filesystem absolute path, from walking the
    // twelve committed schemas. `path` is deliberately absent: its arms are
    // manifest-relative except one, and a name-based rule cannot tell them
    // apart — that one is pinned by its own construction site below.
    const WIRE_PATH_FIELDS: &[&str] = &[
        "absolute_path",
        "actual_store_path",
        "canonical_path",
        "expected_store_path",
        "index_path",
        "marker_path",
        "missing_path",
        "other_store_path",
        "parent_path",
        "recorded_owner",
        "recorded_path",
        "repo_path",
        "store_path",
        "weave_store_path",
        "workspace_dir",
        "workweave_dir",
    ];

    // rustfmt breaks a long assignment across lines, so the value is read as
    // a window rather than as the rest of one line. Four lines is what the
    // longest formatted call in the tree occupies; a value spread wider than
    // that is this instrument's residue, and the non-vacuity floor below is
    // what would notice the window going blind.
    const VALUE_WINDOW: usize = 4;
    let lines = production_lines();
    let mut unminted = Vec::new();
    let mut minted = 0usize;
    for (i, line) in lines.iter().enumerate() {
        let text = line.text.trim();
        let Some((field, _)) = text.split_once(':') else {
            continue;
        };
        if !WIRE_PATH_FIELDS.contains(&field.trim()) {
            continue;
        }
        // Bounded by this assignment's own end, not by a fixed offset: a
        // fixed window lets the NEXT field's mint call answer for this one,
        // which is how a scan reports clean over an unrouted field. Found by
        // mutating a single field off the seam and watching this test pass.
        let mut window = String::new();
        for l in lines[i..(i + VALUE_WINDOW).min(lines.len())].iter() {
            window.push(' ');
            window.push_str(&l.text);
            if l.text.trim_end().ends_with(',') {
                break;
            }
        }
        if window.contains("wire_path") {
            minted += 1;
        } else if window.contains("to_string_lossy") || window.contains(".display()") {
            unminted.push(format!("{}: {}", line.site(), text));
        }
    }

    assert!(
        unminted.is_empty(),
        "these wire path fields are spelled without the mint — a raw \
         stringification publishes the Windows verbatim prefix and backslashes \
         that the documented `jq | xargs` recipe cannot carry:\n{unminted:#?}"
    );
    // A floor on what the scan observes, not a total: an assignment whose
    // value is a local that was minted earlier (`absolute_path: abs_path,`)
    // is correctly neither counted nor flagged, so the real number of minted
    // fields is higher. The floor exists to notice the scan going blind — the
    // failure mode where it walks a corpus, finds nothing, and reports clean.
    assert!(
        minted >= 18,
        "the scan found only {minted} minted wire path fields; it has stopped \
         seeing the assignments it is supposed to police"
    );
}

/// The field a by-name sweep cannot reach. Held as a `String` rather than a
/// `PathBuf` so that its spelling is decided where it is built: a `PathBuf`
/// field is serialized by serde, which knows nothing about which audience the
/// value is owed to.
#[test]
fn the_resolution_workspace_field_is_minted_not_serialized() {
    let decl = production_lines()
        .into_iter()
        .find(|l| l.file == "workspace.rs" && l.text.trim() == "pub workspace: String,")
        .expect("Resolution::workspace must be a String minted at construction");
    assert!(decl.line > 0);

    let built = production_lines().into_iter().any(|l| {
        l.file == "workspace.rs"
            && l.text
                .contains("let workspace = crate::path_spelling::wire_path(")
    });
    assert!(
        built,
        "Resolution::workspace must be built by the wire mint"
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
            repoweave::path_spelling::wire_path(&p),
            raw,
            "the wire mint must not alter a Unix path"
        );
        assert_eq!(
            repoweave::path_spelling::operator_path(&p),
            raw,
            "the operator render must not alter a Unix path"
        );
    }
}

/// Wire and operator spellings are two functions on purpose. They agree here
/// and must not be collapsed into one on that basis — the divergence is on
/// Windows, which this host cannot produce.
#[test]
fn the_two_renders_are_separate_seams() {
    let lines = production_lines();
    let has_wire = lines
        .iter()
        .any(|l| l.file == "path_spelling.rs" && l.text.contains("pub fn wire_path("));
    let has_operator = lines
        .iter()
        .any(|l| l.file == "path_spelling.rs" && l.text.contains("pub fn operator_path("));
    assert!(
        has_wire && has_operator,
        "both renders must exist as their own functions"
    );
    assert_eq!(
        repoweave::path_spelling::wire_path(Path::new("/a/b")),
        repoweave::path_spelling::operator_path(Path::new("/a/b")),
        "on this platform they agree — the difference under test lives on Windows"
    );
}
