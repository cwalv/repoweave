//! `WorkweaveMarker`'s three fields are private; `WorkweaveMarker::new` is
//! the only constructor, and it canonicalizes `parent` on the way in — a
//! step a field-by-field literal has no way to be forced through. Unlike
//! `WeaveRootIdentity`'s payloads (see `root_identity_seal_test.rs`), `new`
//! is meant to be called from many sites — production and tests alike — so
//! there is no "exactly one caller" invariant to pin here, only "every
//! caller goes through `new`, nobody reaches for the literal directly".
//!
//! Two halves, because neither is sufficient alone: the compile probes show
//! the literal and the field reads are unreachable from outside the crate;
//! privacy stops at the defining module, though, so a probe alone says
//! nothing about a second literal added lower down in `workspace.rs` itself
//! — the source scan is what watches that.
//!
//! The scan's needle, `WorkweaveMarker {`, has two false-positive shapes to
//! filter: `CheckViolation::LegacyWorkweaveMarker { .. }` in check.rs is an
//! unrelated enum variant that happens to share the trailing substring, and
//! `into_marker(self) -> WorkweaveMarker {` in workspace.rs is a function
//! signature, not a literal. [`is_construction_site`] excludes both: an
//! identifier character immediately before the needle (the `Legacy` case)
//! or a `-> ` immediately before it (the return-type case).

mod common;

use common::compile_probe::{assert_fails_with, compile};
use common::src_scan::{production_lines, struct_literal_needle};
use repoweave::workspace::WorkweaveMarker;

/// Whether `text` contains a genuine `WorkweaveMarker {` construction,
/// excluding the type's own declaration/impl header and the two
/// false-positive shapes described above.
fn is_construction_site(text: &str, needle: &str) -> bool {
    if text.contains("struct ") || text.trim_start().starts_with("impl ") {
        return false;
    }
    let bytes = text.as_bytes();
    let mut start = 0;
    while let Some(rel) = text[start..].find(needle) {
        let at = start + rel;
        let preceded_by_ident = at > 0 && {
            let c = bytes[at - 1];
            c.is_ascii_alphanumeric() || c == b'_'
        };
        let preceded_by_arrow = at >= 3 && &text[at - 3..at] == "-> ";
        if !preceded_by_ident && !preceded_by_arrow {
            return true;
        }
        start = at + 1;
    }
    false
}

#[test]
fn is_construction_site_ignores_the_known_false_positives() {
    let needle = struct_literal_needle::<WorkweaveMarker>();
    assert!(is_construction_site("        WorkweaveMarker {", &needle));
    assert!(!is_construction_site(
        "pub struct WorkweaveMarker {",
        &needle
    ));
    assert!(!is_construction_site("impl WorkweaveMarker {", &needle));
    assert!(!is_construction_site(
        "            CheckViolation::LegacyWorkweaveMarker { marker_path, .. } => (",
        &needle
    ));
    assert!(!is_construction_site(
        "    pub fn into_marker(self) -> WorkweaveMarker {",
        &needle
    ));
}

#[test]
fn the_harness_can_compile_a_legal_snippet() {
    // Control. Everything below asserts a failure, so a broken rustc
    // invocation would make them all pass for the wrong reason.
    let (compiled, stderr) = compile(
        r#"
        use repoweave::manifest::ProjectName;
        use repoweave::workspace::WorkweaveMarker;
        use std::path::{Path, PathBuf};
        pub fn legal(primary: PathBuf, project: ProjectName, parent: &Path) -> WorkweaveMarker {
            let marker = WorkweaveMarker::new(primary, project, parent);
            let _ = (marker.primary(), marker.project(), marker.parent());
            marker
        }
        "#,
    );
    assert!(compiled, "control snippet must compile; got:\n{stderr}");
}

#[test]
fn a_marker_cannot_be_assembled_field_by_field_from_outside_the_crate() {
    assert_fails_with(
        "E0451",
        "WorkweaveMarker's fields are private; only `new` builds one",
        r#"
        use repoweave::manifest::ProjectName;
        use repoweave::workspace::{CanonicalPath, WorkweaveMarker};
        use std::path::PathBuf;
        pub fn forge(
            primary: PathBuf,
            project: ProjectName,
            parent: CanonicalPath,
        ) -> WorkweaveMarker {
            WorkweaveMarker { primary, project, parent }
        }
        "#,
    );
}

#[test]
fn a_marker_field_cannot_be_read_directly_from_outside_the_crate() {
    assert_fails_with(
        "E0616",
        "reading `parent` bypasses the `parent()` accessor",
        r#"
        use repoweave::workspace::WorkweaveMarker;
        use std::path::Path;
        pub fn peek(marker: &WorkweaveMarker) -> &Path {
            &marker.parent
        }
        "#,
    );
}

#[test]
fn workspace_rs_still_constructs_the_needle_this_scan_looks_for() {
    let lines = production_lines();
    let needle = struct_literal_needle::<WorkweaveMarker>();
    let hits: Vec<_> = lines
        .iter()
        .filter(|l| l.file == "workspace.rs" && is_construction_site(&l.text, &needle))
        .collect();
    assert_eq!(
        hits.len(),
        1,
        "expected exactly one production `{needle}` site in workspace.rs \
         (inside `WorkweaveMarker::new`), found: {:?}",
        hits.iter().map(|l| l.site()).collect::<Vec<_>>()
    );
}

#[test]
fn no_module_outside_workspace_assembles_a_marker_field_by_field() {
    let lines = production_lines();
    assert!(
        lines.len() >= 20_000,
        "expected at least 20,000 production lines under src/, got {} — \
         this scan is pointed at the wrong corpus",
        lines.len()
    );

    let needle = struct_literal_needle::<WorkweaveMarker>();
    let hits: Vec<String> = lines
        .iter()
        .filter(|l| l.file != "workspace.rs" && is_construction_site(&l.text, &needle))
        .map(|l| format!("{} {}", l.site(), l.text.trim()))
        .collect();
    assert!(
        hits.is_empty(),
        "a WorkweaveMarker literal must appear only in workspace.rs, built by \
         `new`: {hits:#?}"
    );
}
