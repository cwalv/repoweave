//! Every source file under `tests/` runs in exactly one test binary.
//!
//! A test file no binary declares is not red, it is unrun: cargo compiles
//! what a binary's module tree reaches and nothing else. So the layout is a
//! prohibition over an enumerable population — every `.rs` file under
//! `tests/` — and is held here.
//!
//! The binaries are what cargo discovers: `tests/<area>/main.rs` each, and a
//! file directly under `tests/` would be one more, of its own. [`walk`]
//! follows each binary's `mod` declarations, a `#[path]` attribute included,
//! with rustc's rules for where a declared module's file lives. A file is
//! reported when no binary reaches it, when a binary of its own is a file
//! directly under `tests/`, when two binaries reach it, or when a
//! declaration names a file that does not exist. `tests/common/` is the one
//! tree every binary may reach, through `mod common`. `tests/fixtures/` is
//! data a test copies, never compiled.
//!
//! What the walk reads is a line-shaped `mod <name>;`: a declaration inside
//! an inline `mod <name> { }` block resolves relative to the wrong directory
//! here, and is reported as naming a missing file rather than passing. The
//! walk is held equal to rustc's own account of the binary this test runs
//! in — the dep-info file cargo writes beside it, listing every source file
//! that binary compiled — and must find this file.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

fn member_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn rel(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// `path` with its `.` and `..` components folded away, by spelling alone:
/// the same file reached through `tests/<area>/../common` and through
/// `tests/common` is one entry.
fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// A `mod <name>;` line and the `#[path]` attribute above it, if any.
fn declarations(text: &str) -> Vec<(String, Option<String>)> {
    let decl = regex::Regex::new(r"^\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+(\w+)\s*;").unwrap();
    let path_attr = regex::Regex::new(r#"^\s*#\[path\s*=\s*"([^"]+)"\]"#).unwrap();
    let mut out = Vec::new();
    let mut pending_path = None;
    for line in text.lines() {
        if let Some(c) = path_attr.captures(line) {
            pending_path = Some(c[1].to_string());
            continue;
        }
        if let Some(c) = decl.captures(line) {
            out.push((c[1].to_string(), pending_path.take()));
            continue;
        }
        if !line.trim_start().starts_with("#[") {
            pending_path = None;
        }
    }
    out
}

/// Where rustc looks for `mod name;` declared in `file`: beside a `main.rs`
/// or `mod.rs`, in a directory named after any other file, and relative to
/// the declaring file's own directory for a `#[path]`.
fn resolve(file: &Path, name: &str, path_attr: Option<&str>) -> Vec<PathBuf> {
    let dir = file.parent().expect("a source file has a directory");
    if let Some(p) = path_attr {
        return vec![dir.join(p)];
    }
    let stem = file.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    let base = if stem == "main" || stem == "mod" {
        dir.to_path_buf()
    } else {
        dir.join(stem)
    };
    vec![
        base.join(format!("{name}.rs")),
        base.join(name).join("mod.rs"),
    ]
}

#[derive(Default)]
struct Walk {
    /// Each file a binary reaches, and the binaries that reach it.
    reached: BTreeMap<PathBuf, BTreeSet<String>>,
    missing: Vec<String>,
}

fn walk_binary(binary: &str, root_file: &Path, walk: &mut Walk, root: &Path) {
    let mut stack = vec![root_file.to_path_buf()];
    let mut seen = BTreeSet::new();
    while let Some(file) = stack.pop() {
        let file = lexical(&file);
        if !seen.insert(file.clone()) {
            continue;
        }
        walk.reached
            .entry(file.clone())
            .or_default()
            .insert(binary.to_string());
        let text = std::fs::read_to_string(&file)
            .unwrap_or_else(|e| panic!("{} is readable: {e}", file.display()));
        for (name, path_attr) in declarations(&text) {
            let candidates = resolve(&file, &name, path_attr.as_deref());
            match candidates.iter().find(|c| c.is_file()) {
                Some(found) => stack.push(found.clone()),
                None => walk.missing.push(format!(
                    "{}: `mod {name};` names no file (looked for {})",
                    rel(root, &file),
                    candidates
                        .iter()
                        .map(|c| rel(root, c))
                        .collect::<Vec<_>>()
                        .join(", ")
                )),
            }
        }
    }
}

/// The binaries cargo discovers under `tests/`: `(name, root file)`.
fn binaries(tests: &Path) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(tests)
        .expect("tests/ is readable")
        .flatten()
    {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_dir() && path.join("main.rs").is_file() {
            out.push((name, path.join("main.rs")));
        } else if path.is_file() && name.ends_with(".rs") {
            out.push((name.trim_end_matches(".rs").to_string(), path));
        }
    }
    out.sort();
    out
}

fn walk(root: &Path) -> Walk {
    let tests = root.join("tests");
    let mut walk = Walk::default();
    for (name, file) in binaries(&tests) {
        walk_binary(&name, &file, &mut walk, root);
    }
    walk
}

fn findings(root: &Path) -> Vec<String> {
    let tests = root.join("tests");
    let common = tests.join("common");
    let fixtures = tests.join("fixtures");
    let walk = walk(root);
    let mut out = walk.missing.clone();
    for (_, file) in binaries(&tests) {
        if file.parent() == Some(tests.as_path()) {
            out.push(format!(
                "{}: a test binary of its own; every binary is tests/<area>/main.rs, \
                 and a test file is a module of one",
                rel(root, &file)
            ));
        }
    }
    let mut files = Vec::new();
    rust_files(&tests, &mut files);
    for file in files {
        let file = lexical(&file);
        if file.strip_prefix(&fixtures).is_ok() {
            continue;
        }
        let shown = rel(root, &file);
        match walk.reached.get(&file) {
            None => out.push(format!(
                "{shown}: declared by no test binary, so it never runs"
            )),
            Some(bins) if bins.len() > 1 && file.strip_prefix(&common).is_err() => {
                out.push(format!(
                    "{shown}: declared by more than one test binary ({})",
                    bins.iter().cloned().collect::<Vec<_>>().join(", ")
                ))
            }
            Some(_) => {}
        }
    }
    out
}

#[test]
fn every_test_source_runs_in_exactly_one_binary() {
    let root = member_root();
    let found = findings(&root);
    assert!(
        found.is_empty(),
        "test sources outside the layout:\n  {}",
        found.join("\n  ")
    );
}

/// The `.rs` files under `tests/` rustc compiled into `binary`, the one
/// running this test, from the dep-info file cargo keeps beside it.
///
/// rustc writes each path as it was handed one, relative to the directory it
/// ran in — the workspace root, which is this package's root only when the
/// package builds alone. The first source listed is the binary's own
/// `main.rs`, whose absolute path is known, so that directory is what the
/// first entry's spelling leaves when cut from it.
fn compiled_into_this_binary(root: &Path, binary: &str) -> BTreeSet<PathBuf> {
    let exe = std::env::current_exe().expect("this test binary's own path");
    let dep_info = exe.with_extension("d");
    let text = std::fs::read_to_string(&dep_info)
        .unwrap_or_else(|e| panic!("{} is readable: {e}", dep_info.display()));
    let first = text.lines().next().unwrap_or("");
    let (_, deps) = first
        .split_once(": ")
        .unwrap_or_else(|| panic!("{} opens with no dependency line", dep_info.display()));
    let deps: Vec<&str> = deps.split(' ').filter(|d| !d.is_empty()).collect();
    let main = root.join("tests").join(binary).join("main.rs");
    let spelled_main = Path::new(deps.first().expect("dep-info lists the binary's main.rs"));
    let base = if spelled_main.is_absolute() {
        PathBuf::new()
    } else {
        let main_text = main.to_string_lossy().replace('\\', "/");
        let spelled = spelled_main.to_string_lossy().replace('\\', "/");
        let cut = main_text.strip_suffix(spelled.as_str()).unwrap_or_else(|| {
            panic!("dep-info's first source {spelled} is not this binary's main.rs, {main_text}")
        });
        PathBuf::from(cut)
    };
    let tests = root.join("tests");
    deps.iter()
        .filter(|d| d.ends_with(".rs"))
        .map(|d| lexical(&base.join(d)))
        .filter(|d| d.strip_prefix(&tests).is_ok())
        .collect()
}

#[test]
fn the_walk_agrees_with_what_rustc_compiled_into_this_binary() {
    let root = member_root();
    let (binary, module) = module_path!()
        .split_once("::")
        .expect("this test is a module of an area binary");
    let this_file = root.join("tests").join(binary).join(format!("{module}.rs"));
    let walk = walk(&root);
    let walked: BTreeSet<PathBuf> = walk
        .reached
        .iter()
        .filter(|(_, bins)| bins.contains(binary))
        .map(|(f, _)| f.clone())
        .collect();
    assert!(
        walked.contains(&this_file),
        "the walk never reached {}",
        this_file.display()
    );
    let compiled = compiled_into_this_binary(&root, binary);
    assert_eq!(
        walked.iter().map(|p| rel(&root, p)).collect::<Vec<_>>(),
        compiled.iter().map(|p| rel(&root, p)).collect::<Vec<_>>(),
        "the module walk and rustc disagree about what the {binary} binary compiles"
    );
}

fn plant(root: &Path, rel_path: &str, text: &str) {
    let path = root.join(rel_path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn seeded_layout() -> tempfile::TempDir {
    let tmp = crate::common::tempdir().expect("tempdir");
    let root = tmp.path();
    plant(root, "tests/common/mod.rs", "pub mod cli;\n");
    plant(root, "tests/common/cli.rs", "");
    plant(
        root,
        "tests/alpha/main.rs",
        "#[path = \"../common/mod.rs\"]\nmod common;\n\nmod one_test;\n",
    );
    plant(
        root,
        "tests/alpha/one_test.rs",
        "#[path = \"one_test/inner.rs\"]\nmod inner;\n",
    );
    plant(root, "tests/alpha/one_test/inner.rs", "");
    plant(
        root,
        "tests/beta/main.rs",
        "#[path = \"../common/mod.rs\"]\nmod common;\n\nmod two_test;\n",
    );
    plant(root, "tests/beta/two_test.rs", "mod nested;\n");
    plant(root, "tests/beta/two_test/nested.rs", "");
    plant(root, "tests/fixtures/crate/src/lib.rs", "");
    tmp
}

#[test]
fn a_well_formed_layout_has_no_findings() {
    let tmp = seeded_layout();
    assert_eq!(findings(tmp.path()), Vec::<String>::new());
}

#[test]
fn each_way_out_of_the_layout_is_reported() {
    let tmp = seeded_layout();
    let root = tmp.path();
    plant(root, "tests/alpha/undeclared_test.rs", "");
    plant(root, "tests/loose_test.rs", "");
    plant(root, "tests/beta/three_test.rs", "");
    plant(
        root,
        "tests/beta/main.rs",
        "#[path = \"../common/mod.rs\"]\nmod common;\n\nmod two_test;\nmod gone_test;\n#[path = \"../alpha/one_test.rs\"]\nmod borrowed;\n",
    );
    let found = findings(root);
    for needle in [
        "tests/alpha/undeclared_test.rs: declared by no test binary",
        "tests/loose_test.rs: a test binary of its own",
        "tests/beta/three_test.rs: declared by no test binary",
        "tests/beta/main.rs: `mod gone_test;` names no file",
        "tests/alpha/one_test.rs: declared by more than one test binary (alpha, beta)",
        "tests/alpha/one_test/inner.rs: declared by more than one test binary (alpha, beta)",
    ] {
        assert!(
            found.iter().any(|f| f.starts_with(needle)),
            "expected a finding starting {needle:?} in:\n{found:#?}"
        );
    }
    assert!(
        !found
            .iter()
            .any(|f| f.contains("fixtures") || f.contains("common/")),
        "fixtures and the shared common tree are not findings:\n{found:#?}"
    );
}
