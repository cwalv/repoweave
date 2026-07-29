//! `WeaveRootIdentity` has exactly one producer: `require_exclusive`.
//!
//! The type is a projection of `RootObservation`, and the two observations
//! that projection refuses — a root carrying both identity files, and a
//! marker that cannot witness what it claims — have no representation in it.
//! That is only worth anything while the projection is the sole way to get
//! one. A consumer that could assemble the workweave arm from a marker it
//! read itself would be deciding, at its own site, the marker-versus-pointer
//! tiebreak the projection exists to refuse — and it would do it silently,
//! because the assembled value is indistinguishable from a projected one.
//!
//! Two halves, because neither is sufficient:
//!
//! 1. **Nothing outside `workspace.rs` can build one.** The payload structs
//!    carry private fields, so the literal does not compile. Pinned by
//!    diagnostic code rather than by "somehow fails", and pinned from an
//!    external crate — where every visibility narrower than `pub` looks
//!    alike, so the refusal an out-of-crate probe demonstrates is the same
//!    one `check.rs` or `dispatch.rs` would hit.
//! 2. **Inside `workspace.rs`, the one site is in `require_exclusive`.**
//!    Privacy stops at the module that defines the field; a second literal
//!    added a few hundred lines below the first would compile. The source
//!    scan is what notices.
//!
//! Residue: the scan reads line text, so a construction spelled across a
//! line break (`WorkweaveIdentity {` alone on its line is caught, but a
//! `Self {` inside an `impl WorkweaveIdentity` block is not) is invisible to
//! it. The compile probes are unaffected — such a site is still inside
//! `workspace.rs` — but a reviewer adding an `impl` block to either payload
//! is the person this note is for.

mod common;

use common::compile_probe::{assert_fails_with, compile};
use common::src_scan::{production_lines, src_dir, struct_literal_needle};
use repoweave::workspace::{PrimaryIdentity, WorkweaveIdentity};

#[test]
fn the_harness_can_compile_a_legal_snippet() {
    // Control. Everything below asserts a failure, so a broken rustc
    // invocation would make them all pass for the wrong reason.
    let (compiled, stderr) = compile(
        r#"
        use repoweave::workspace::{observe_root, WeaveRootIdentity};
        use std::path::Path;
        pub fn legal(dir: &Path) -> Option<WeaveRootIdentity> {
            observe_root(dir)?.require_exclusive().ok()
        }
        "#,
    );
    assert!(compiled, "control snippet must compile; got:\n{stderr}");
}

#[test]
fn the_workweave_arm_cannot_be_assembled_from_a_marker() {
    // This is the shape the guard is about: a caller that reads the marker
    // itself, sees a workweave, and declares one — never having looked for
    // the `.rwv-active` beside it.
    assert_fails_with(
        "E0451",
        "the workweave arm is projected from an observation, not declared",
        r#"
        use repoweave::workspace::{WeaveRootIdentity, WorkweaveIdentity, WorkweaveMarker};
        pub fn forge(marker: WorkweaveMarker) -> WeaveRootIdentity {
            WeaveRootIdentity::Workweave(WorkweaveIdentity { marker })
        }
        "#,
    );
}

#[test]
fn the_primary_arm_cannot_be_assembled_from_a_pointer() {
    // Every field is supplied. A literal that omits one fails for the
    // uninteresting reason instead, and would keep failing after the fields
    // it does name became public.
    assert_fails_with(
        "E0451",
        "the primary arm is projected from an observation, not declared",
        r#"
        use repoweave::manifest::ProjectName;
        use repoweave::workspace::{PrimaryIdentity, WeaveRootIdentity};
        use std::path::PathBuf;
        pub fn forge(root: PathBuf, selection: Option<ProjectName>) -> WeaveRootIdentity {
            WeaveRootIdentity::Primary(PrimaryIdentity { root, selection })
        }
        "#,
    );
}

#[test]
fn each_identity_arm_is_built_at_exactly_one_site_inside_require_exclusive() {
    let body = require_exclusive_lines();
    assert!(
        body.clone().count() >= 4,
        "require_exclusive's span came out as {body:?} — the slicer, not the \
         invariant, is what this test would be reporting on"
    );

    let lines = production_lines();
    for needle in [
        struct_literal_needle::<WorkweaveIdentity>(),
        struct_literal_needle::<PrimaryIdentity>(),
    ] {
        let sites: Vec<_> = lines
            .iter()
            .filter(|l| l.text.contains(&needle) && !declares(&l.text))
            .collect();

        assert_eq!(
            sites.len(),
            1,
            "`{needle}` must be built at exactly one production site. Found: {:?}",
            sites.iter().map(|l| l.site()).collect::<Vec<_>>()
        );
        assert_eq!(
            sites[0].file,
            "workspace.rs",
            "`{needle}`'s one site must be in workspace.rs; found {}",
            sites[0].site()
        );
        assert!(
            body.contains(&sites[0].line),
            "`{needle}` is built at {}, outside require_exclusive (lines \
             {body:?}) — a second collapse, deciding for itself what the \
             projection refuses to decide",
            sites[0].site()
        );
    }
}

/// Whether a line naming the type declares it rather than builds one: the
/// struct's own definition and an `impl` block header both carry the needle.
fn declares(text: &str) -> bool {
    text.contains("struct ") || text.trim_start().starts_with("impl ")
}

/// The 1-based line span of `require_exclusive`'s body, taken from the source
/// rather than typed here so it cannot drift.
fn require_exclusive_lines() -> std::ops::RangeInclusive<usize> {
    let source =
        std::fs::read_to_string(src_dir().join("workspace.rs")).expect("read workspace.rs");
    let lines: Vec<&str> = source.lines().collect();
    let start = lines
        .iter()
        .position(|l| l.contains("pub fn require_exclusive"))
        .expect("workspace.rs declares require_exclusive");
    let end = match lines[start + 1..]
        .iter()
        .position(|l| l.trim_start().starts_with("pub fn "))
    {
        Some(offset) => start + offset,
        None => lines.len() - 1,
    };
    (start + 1)..=(end + 1)
}
