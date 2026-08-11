//! Pins the prohibitions behind rwv's symlink strategy, which prose alone
//! cannot hold.
//!
//! The strategy is full symlink parity on every platform: no junction, no copy
//! fallback, no hardlink, and a refusal rather than a warning when a link
//! cannot be made. Three of those four are prohibitions on code that does not
//! exist, and the fourth is a rule about where a decision is written. Nothing
//! about a green suite reports their violation — the defect that started this
//! was a `#[cfg(unix)]` block with no `#[cfg(windows)]` arm, which compiles
//! everywhere, passes every test on Unix, and creates no link at all on
//! Windows while reporting success.
//!
//! So the scan is the gate. What it pins:
//!
//!   - the platform symlink constructors have exactly one caller, so a new
//!     site cannot reintroduce a one-armed `cfg` block;
//!   - that caller creates a link under both `cfg`s, and picks between
//!     Windows' two constructors;
//!   - no call site hardcodes a file link, which is the shape of the bug that
//!     was live at the surfacing site;
//!   - `hard_link` stays confined to the atomic-publish path it belongs to.
//!
//! Residue, and it is the interesting half. This reads lines, so it cannot
//! see a caller that wraps the link creation in a recovery — a
//! `.or_else(copy)` around a refused link would pass every assertion here. It
//! inherits `src_scan`'s line-leading `//` filter and its `#[cfg(test)]` skip,
//! so a needle in a block comment or a trailing comment reads as production
//! and one in an inline test module reads as absent. It says nothing about
//! `tests/`, where fixtures create links directly on purpose. And none of it
//! is runtime evidence on Windows: no test in this repository has ever run
//! there.

mod common;

use common::src_scan::{production_lines, SourceLine};

/// The module that owns platform link creation.
const OWNER: &str = "symlink.rs";

/// The atomic exclusive publish, which links a staged temp into place and
/// removes the temp in the same call. It is the one hardlink rwv makes, and
/// no second name survives it.
const HARDLINK_OWNER: &str = "durable_file.rs";

/// The platform constructors, each of which is a place a link's kind is
/// decided — implicitly on Unix, explicitly on Windows.
const PLATFORM_CALLS: &[&str] = &[
    "os::unix::fs::symlink",
    "os::windows::fs::symlink_dir",
    "os::windows::fs::symlink_file",
];

fn sites<'a>(lines: &'a [SourceLine], needle: &str) -> Vec<&'a SourceLine> {
    lines.iter().filter(|l| l.text.contains(needle)).collect()
}

fn lines_of<'a>(lines: &'a [SourceLine], file: &str) -> Vec<&'a SourceLine> {
    lines.iter().filter(|l| l.file == file).collect()
}

#[test]
fn the_scan_is_pointed_at_a_whole_source_tree() {
    let lines = production_lines();
    assert!(
        lines.len() >= 10_000,
        "expected at least 10000 production lines under src/, got {} — every \
         absence asserted below would hold on an empty corpus",
        lines.len()
    );
}

#[test]
fn platform_symlink_calls_have_one_owner() {
    let lines = production_lines();
    for call in PLATFORM_CALLS {
        let found = sites(&lines, call);
        assert!(
            !found.is_empty(),
            "`{call}` was not found anywhere under src/. Each constructor has \
             to be called somewhere — a zero-site scan means the needle stopped \
             matching and this assertion is vacuous"
        );
        let strays: Vec<String> = found
            .iter()
            .filter(|l| l.file != OWNER)
            .map(|l| l.site())
            .collect();
        assert!(
            strays.is_empty(),
            "`{call}` is called outside {OWNER}: {}. A second caller is a \
             second place the directory-vs-file decision is made, and a \
             `#[cfg(unix)]`-only one creates nothing on Windows while \
             reporting success",
            strays.join(", ")
        );
    }
}

#[test]
fn the_owner_creates_a_link_on_both_platforms() {
    let lines = production_lines();
    let owner = lines_of(&lines, OWNER);
    assert!(
        !owner.is_empty(),
        "{OWNER} has no production lines — it was renamed or removed, and \
         every assertion here is then about a file that does not exist"
    );
    let has = |needle: &str| owner.iter().any(|l| l.text.contains(needle));

    assert!(
        has("#[cfg(unix)]") && has("#[cfg(windows)]"),
        "{OWNER} must define a link constructor under both cfgs. One arm \
         without the other is not a compile error: the call vanishes and the \
         caller's result stays Ok"
    );
    assert!(
        has("os::windows::fs::symlink_dir") && has("os::windows::fs::symlink_file"),
        "{OWNER} must reach both Windows constructors. Calling only one makes \
         every link of the other kind broken there, invisibly from here"
    );
}

#[test]
fn no_call_site_hardcodes_a_file_link() {
    let lines = production_lines();
    let hardcoded: Vec<String> = sites(&lines, "LinkTarget::File")
        .iter()
        .filter(|l| l.file != OWNER)
        .map(|l| l.site())
        .collect();
    assert!(
        hardcoded.is_empty(),
        "a call site names LinkTarget::File directly: {}. This is the bug that \
         was live at the surfacing site — an unconditional file link whose \
         target can be a directory (`.beads` and `.claude` are surfaced \
         directories). Classify the source with LinkTarget::on_disk, or come \
         back to docs/explanation/joints/symlinks-as-structure.md if the kind \
         really is known",
        hardcoded.join(", ")
    );
}

#[test]
fn hardlinks_are_confined_to_the_atomic_publish() {
    let lines = production_lines();
    let found = sites(&lines, "hard_link(");
    assert!(
        !found.is_empty(),
        "`hard_link(` was not found anywhere under src/. The atomic exclusive \
         publish uses it, so a zero-site scan means the needle stopped \
         matching and this assertion is vacuous"
    );
    let strays: Vec<String> = found
        .iter()
        .filter(|l| l.file != HARDLINK_OWNER)
        .map(|l| l.site())
        .collect();
    assert!(
        strays.is_empty(),
        "hard_link is called outside {HARDLINK_OWNER}: {}. rwv publishes owned \
         files by renaming over them, and rename replaces the directory entry \
         rather than writing through the inode — a hardlink that outlives one \
         call keeps the previous contents, silently. Hardlinks also cannot \
         name directories at all",
        strays.join(", ")
    );
}

/// The copy-fallback prohibition rests on this: rwv records nothing about a
/// checkout being a shared alias, so a copy is read back as a workspace the
/// user asked to work in. Changing that classification is the only way to make
/// a copy fallback safe, and this failing is the conversation about it.
#[test]
fn a_copied_checkout_is_indistinguishable_from_a_worktree() {
    use repoweave::workweave::{classify_checkout, CheckoutKind};

    let tmp = tempfile::tempdir().unwrap();
    let canonical = tmp.path().join("canonical");
    std::fs::create_dir_all(canonical.join(".git")).unwrap();

    let alias = tmp.path().join("alias");
    repoweave::symlink::create(
        &canonical,
        &alias,
        repoweave::symlink::LinkTarget::Directory,
    )
    .unwrap();
    assert_eq!(classify_checkout(&alias), CheckoutKind::ReferenceAlias);

    let copy = tmp.path().join("copy");
    std::fs::create_dir_all(copy.join(".git")).unwrap();
    assert_eq!(
        classify_checkout(&copy),
        CheckoutKind::Worktree,
        "a copy of the canonical store classifies as a worktree, so sync \
         advances it, orphan pruning hands it to `git worktree remove`, and \
         doctor reports it as a standalone store inside a workweave"
    );
}
