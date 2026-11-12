//! Pins the tracked `Cargo.lock` against `Cargo.toml`'s own dependency
//! tables: every crate name `Cargo.toml` requires must have a `[[package]]`
//! entry in the lock.
//!
//! `CARGO_MANIFEST_DIR` names this crate's own directory on disk regardless
//! of which workspace resolved the build that is running this test, so the
//! two files read here are the ones actually committed — no cargo resolution
//! runs, and no ancestor workspace lock enters the picture.
//!
//! Residue: this checks *presence* only. A dependency whose version
//! requirement narrowed or widened, without adding or removing a crate name,
//! is not caught here.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_toml(path: &Path) -> toml_edit::DocumentMut {
    std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
        .parse()
        .unwrap_or_else(|e| panic!("parsing {} as TOML: {e}", path.display()))
}

/// The crate name a `[dependencies]` entry actually pulls in: its `package`
/// field when the entry renames itself, otherwise the table key.
fn dep_crate_name(key: &str, item: &toml_edit::Item) -> String {
    let renamed = if let Some(inline) = item.as_inline_table() {
        inline.get("package").and_then(|v| v.as_str())
    } else if let Some(table) = item.as_table() {
        table.get("package").and_then(|i| i.as_str())
    } else {
        None
    };
    renamed.unwrap_or(key).to_string()
}

fn required_crate_names(manifest: &toml_edit::DocumentMut) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for section in ["dependencies", "dev-dependencies", "build-dependencies"] {
        let Some(table) = manifest.get(section).and_then(|i| i.as_table()) else {
            continue;
        };
        for (key, item) in table.iter() {
            names.insert(dep_crate_name(key, item));
        }
    }
    names
}

fn locked_crate_names(lock: &toml_edit::DocumentMut) -> BTreeSet<String> {
    lock.get("package")
        .and_then(|i| i.as_array_of_tables())
        .into_iter()
        .flatten()
        .filter_map(|t| t.get("name").and_then(|n| n.as_str()))
        .map(str::to_string)
        .collect()
}

#[test]
fn every_manifest_dependency_has_a_tracked_lock_entry() {
    let root = crate_root();
    let manifest = read_toml(&root.join("Cargo.toml"));
    let lock = read_toml(&root.join("Cargo.lock"));

    let required = required_crate_names(&manifest);
    assert!(
        !required.is_empty(),
        "found no [dependencies]/[dev-dependencies]/[build-dependencies] \
         entries in Cargo.toml — the parse is pointed at the wrong file or \
         the table names above no longer match its shape"
    );

    let locked = locked_crate_names(&lock);
    assert!(
        !locked.is_empty(),
        "found no [[package]] entries in Cargo.lock — the parse is pointed \
         at the wrong file or its shape changed"
    );

    let missing: Vec<&String> = required.difference(&locked).collect();
    assert!(
        missing.is_empty(),
        "Cargo.toml requires {missing:?} but the tracked Cargo.lock has no \
         [[package]] entry for it. Inside a workweave this crate builds \
         against an ancestor workspace's lock, so `cargo check`/`cargo test` \
         stay green while the committed lock — the one a standalone build or \
         release uses — falls out of date.\n\
         \n\
         Do not fix this with `cargo generate-lockfile`: run from inside a \
         workweave it re-resolves the whole graph against the ancestor \
         workspace, and even run outside one it discards existing pins \
         rather than extending them. Regenerate by resolving the manifest \
         on its own, outside any workspace, keeping the current lock as a \
         starting point:\n\
         \n\
         \tgit archive HEAD | tar -x -C <scratch-dir-outside-any-weave>\n\
         \tcd <scratch-dir> && cargo metadata --format-version 1 >/dev/null\n\
         \n\
         then copy the regenerated Cargo.lock back and diff it against the \
         original — only the crates you touched should move."
    );
}
