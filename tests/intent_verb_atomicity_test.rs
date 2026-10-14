//! An intent verb's manifest edit and the regeneration it drives land
//! together or not at all, and re-running `rwv add` for a member the manifest
//! already names converges the weave instead of reporting success over it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

mod common;

/// A weave whose active project `demo` was made by `rwv init`.
fn weave() -> (tempfile::TempDir, PathBuf) {
    let tmp = common::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    std::fs::create_dir_all(ws.join("projects")).unwrap();
    common::rwv()
        .args(["init", "demo"])
        .current_dir(&ws)
        .assert()
        .success();
    (tmp, ws)
}

/// A bare repo at `bare` whose one commit holds `files`, and its `file://` URL.
fn origin_with(bare: &Path, files: &[(&str, &str)]) -> String {
    let parent = bare.parent().unwrap();
    std::fs::create_dir_all(parent).unwrap();
    common::git_in(
        parent,
        &[
            "init",
            "--bare",
            "--initial-branch=main",
            bare.to_str().unwrap(),
        ],
    );
    let seed = parent.join(format!(
        "__seed_{}",
        bare.file_stem().unwrap().to_string_lossy()
    ));
    common::git_in(
        parent,
        &["clone", bare.to_str().unwrap(), seed.to_str().unwrap()],
    );
    common::git_in(&seed, &["config", "user.email", "test@test.com"]);
    common::git_in(&seed, &["config", "user.name", "Test"]);
    std::fs::write(seed.join("README"), "seed").unwrap();
    for (path, content) in files {
        let file = common::under(&seed, path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, content).unwrap();
    }
    common::git_in(&seed, &["add", "."]);
    common::git_in(&seed, &["commit", "-m", "initial"]);
    common::git_in(&seed, &["push", "origin", "main"]);
    common::file_url(bare)
}

/// Every file under `dir` but its `.git`, with its bytes.
fn tree(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn walk(dir: &Path, base: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.file_name().is_some_and(|n| n == ".git") {
                continue;
            }
            let meta = path.symlink_metadata().unwrap();
            if meta.is_dir() {
                walk(&path, base, out);
            } else if meta.is_file() {
                out.insert(
                    path.strip_prefix(base).unwrap().to_path_buf(),
                    std::fs::read(&path).unwrap(),
                );
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

/// The weave root's top-level symlinks and what each reads.
fn root_links(ws: &Path) -> BTreeMap<String, PathBuf> {
    std::fs::read_dir(ws)
        .unwrap()
        .flatten()
        .filter_map(|entry| {
            let target = std::fs::read_link(entry.path()).ok()?;
            Some((entry.file_name().to_string_lossy().into_owned(), target))
        })
        .collect()
}

/// The refusal's route line is the one line naming a token, and it is last.
fn assert_routes_to(stderr: &str, token: &str) {
    let routes: Vec<&str> = stderr
        .lines()
        .filter_map(|l| l.strip_prefix("rwv explain "))
        .collect();
    assert_eq!(routes, vec![token], "stderr was:\n{stderr}");
    assert!(
        stderr.ends_with(&format!("\n\nrwv explain {token}\n")),
        "the route line must be the last line, after a blank one, stderr was:\n{stderr}"
    );
}

fn add(ws: &Path, url: &str) -> std::process::Output {
    common::rwv()
        .args(["add", url])
        .current_dir(ws)
        .output()
        .unwrap()
}

#[test]
fn an_add_refused_by_activation_leaves_the_project_as_it_was() {
    // A repo that is its own cargo workspace cannot be a member of the weave's.
    // Every other integration has already authored by the time that refusal
    // fires, and the manifest was written before any of them ran.
    let (tmp, ws) = weave();
    let url = origin_with(
        &tmp.path().join("origin/acme/nested.git"),
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"nested\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
                 [workspace]\nmembers = []\n",
            ),
            ("src/lib.rs", ""),
        ],
    );
    let project = ws.join("projects/demo");
    let project_before = tree(&project);
    let links_before = root_links(&ws);

    let out = add(&ws, &url);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(!out.status.success(), "the add must refuse: {stderr}");
    assert!(
        stderr.contains("declares its own [workspace]"),
        "the cause must still reach the operator: {stderr}"
    );
    assert!(
        stderr.contains("rwv.toml is unchanged"),
        "the operator must be told the manifest is untouched: {stderr}"
    );
    assert_eq!(
        tree(&project),
        project_before,
        "rwv.toml and every regenerated file must be as they were"
    );
    assert_eq!(root_links(&ws), links_before);
}

#[test]
fn an_add_stopped_by_a_surfacing_occupant_moves_no_link_and_lands_nothing() {
    // A real file where the new member's lock link belongs: the shape a tool
    // leaves when it replaces a surfaced link instead of writing through it.
    let (tmp, ws) = weave();
    let plain = origin_with(&tmp.path().join("origin/acme/plain.git"), &[]);
    let added = add(&ws, &plain);
    assert!(
        added.status.success(),
        "fixture add: {}",
        String::from_utf8_lossy(&added.stderr)
    );
    let crate_url = origin_with(
        &tmp.path().join("origin/acme/server.git"),
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"server\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
            ),
            ("src/lib.rs", ""),
        ],
    );
    std::fs::write(ws.join("Cargo.lock"), "# not rwv's\n").unwrap();
    let project = ws.join("projects/demo");
    let project_before = tree(&project);
    let links_before = root_links(&ws);
    assert!(
        !links_before.is_empty(),
        "fixture: the first add must have surfaced something for this to guard"
    );

    let out = add(&ws, &crate_url);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(!out.status.success(), "the add must refuse: {stderr}");
    assert!(
        stderr.contains("Cargo.lock") && stderr.contains("rwv.toml is unchanged"),
        "the refusal must name the occupant and say nothing landed: {stderr}"
    );
    assert_routes_to(&stderr, "surfacing-path-occupied");
    assert_eq!(tree(&project), project_before);
    assert_eq!(root_links(&ws), links_before);
    assert_eq!(
        std::fs::read_to_string(ws.join("Cargo.lock")).unwrap(),
        "# not rwv's\n"
    );
}

#[test]
fn re_running_add_for_a_registered_repo_repairs_its_surfacing() {
    let (tmp, ws) = weave();
    let url = origin_with(&tmp.path().join("origin/acme/plain.git"), &[]);
    let first = add(&ws, &url);
    assert!(
        first.status.success(),
        "fixture add: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    let surfaced = root_links(&ws);
    assert!(
        !surfaced.is_empty(),
        "fixture: the add must have surfaced something to lose"
    );
    for name in surfaced.keys() {
        std::fs::remove_file(ws.join(name)).unwrap();
    }
    let manifest_before = std::fs::read(ws.join("projects/demo/rwv.toml")).unwrap();

    let again = add(&ws, &url);
    let stderr = String::from_utf8_lossy(&again.stderr);

    assert!(again.status.success(), "{stderr}");
    assert!(stderr.contains("already exists in manifest"), "{stderr}");
    assert_eq!(
        root_links(&ws),
        surfaced,
        "re-running the add must put back what the weave lost"
    );
    assert_eq!(
        std::fs::read(ws.join("projects/demo/rwv.toml")).unwrap(),
        manifest_before
    );
}

#[test]
fn a_remove_refused_by_activation_leaves_the_project_and_the_clone() {
    let (tmp, ws) = weave();
    let url = origin_with(&tmp.path().join("origin/acme/plain.git"), &[]);
    let added = add(&ws, &url);
    assert!(
        added.status.success(),
        "fixture add: {}",
        String::from_utf8_lossy(&added.stderr)
    );
    let surfaced = root_links(&ws);
    let occupied = surfaced
        .keys()
        .next()
        .expect("fixture: the add must have surfaced a link to occupy")
        .clone();
    std::fs::remove_file(ws.join(&occupied)).unwrap();
    std::fs::write(ws.join(&occupied), "mine\n").unwrap();
    let project = ws.join("projects/demo");
    let project_before = tree(&project);
    let clone = ws.join("local/acme/plain");
    assert!(clone.is_dir(), "fixture: the add must have cloned");

    let out = common::rwv()
        .args(["remove", "local/acme/plain", "--delete"])
        .current_dir(&ws)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(!out.status.success(), "the remove must refuse: {stderr}");
    assert!(stderr.contains("rwv.toml is unchanged"), "{stderr}");
    assert_eq!(tree(&project), project_before);
    assert!(clone.is_dir(), "a refused remove must not delete the clone");
}
