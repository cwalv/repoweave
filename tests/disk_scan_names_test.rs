//! The disk scan names each checkout by the names the host's path library
//! splits its location into, joined with `/`, and nothing else.

use std::path::Path;

use repoweave::manifest::RepoPath;
use repoweave::workspace::scan_repos_on_disk;

mod common;

fn weave(tmp: &tempfile::TempDir) -> std::path::PathBuf {
    let ws = tmp.path().join("ws");
    std::fs::create_dir_all(ws.join("projects")).unwrap();
    ws
}

fn plant_repo(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    common::git_in(dir, &["init", "-q"]);
}

fn scanned(ws: &Path) -> Vec<String> {
    scan_repos_on_disk(ws, repoweave::git::git_vcs().as_ref())
        .iter()
        .map(RepoPath::to_string)
        .collect()
}

#[test]
fn a_checkout_is_named_by_its_three_names() {
    let tmp = common::tempdir().unwrap();
    let ws = weave(&tmp);
    plant_repo(&ws.join("github").join("o").join("r"));
    assert_eq!(scanned(&ws), ["github/o/r"]);
}

#[cfg(unix)]
#[test]
fn a_backslash_in_a_unix_name_stays_part_of_the_name() {
    let tmp = common::tempdir().unwrap();
    let ws = weave(&tmp);
    plant_repo(&ws.join("github").join("o").join("a\\b"));
    assert_eq!(
        scanned(&ws),
        Vec::<String>::new(),
        "`a\\b` is one name, which no member path holds; read as `a/b` it names \
         a directory that is not there"
    );
}
