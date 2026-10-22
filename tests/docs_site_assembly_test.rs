//! `scripts/assemble-docs-site.py` publishes release tags and nothing else.
//!
//! Driven against a fixture repository whose tags were made out of version
//! order, with a prerelease above every stable version, a tag on a commit no
//! release names, and a working tree that differs from every tag. mdbook and
//! mdbook-mermaid are stand-ins on PATH: the stand-in mdbook copies the tree
//! it is handed to its destination and records the scripts it was told to add,
//! so what each version's directory holds is what the assembler fed it.
//!
//! What this does not reach: the real mdbook, and how the selector renders in a
//! page. The stand-ins are sh scripts, hence `cfg(unix)`; the assembler runs in
//! the Pages job on Linux. `tests/docs_site_workflow_test.rs` holds the
//! workflow half.
#![cfg(unix)]

use std::collections::BTreeSet;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

mod common;

const ASSEMBLER: &str = "scripts/assemble-docs-site.py";
const SELECTOR: &str = "scripts/docs-site-version-selector.js";

const MDBOOK: &str = r#"#!/bin/sh
set -eu
[ "$1" = build ] && [ "$3" = --dest-dir ] || { echo "unexpected mdbook argv: $*" >&2; exit 64; }
if [ -e "$2/docs/FAIL" ]; then echo "this book does not build" >&2; exit 3; fi
mkdir -p "$4"
cp -R "$2/." "$4/"
printf '%s' "$MDBOOK_OUTPUT__HTML__ADDITIONAL_JS" > "$4/additional-js.json"
"#;

const MDBOOK_MERMAID: &str = r#"#!/bin/sh
set -eu
[ "$1" = install ] || { echo "unexpected mdbook-mermaid argv: $*" >&2; exit 64; }
touch "$2/mermaid-installed"
"#;

fn member_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn python_with_tomllib() -> bool {
    if common::skip_without_tool("python3") {
        return false;
    }
    let tomllib = Command::new("python3")
        .args(["-c", "import tomllib"])
        .status()
        .unwrap();
    if !tomllib.success() {
        common::report_skip("python3 has no tomllib (needs 3.11+), which the assembler reads book.toml with");
        return false;
    }
    true
}

fn write(path: &Path, contents: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

fn git(repo: &Path, date: &str, args: &[&str]) -> String {
    let out = common::git()
        .args(["-c", "commit.gpgSign=false", "-c", "tag.gpgSign=false"])
        .args(args)
        .current_dir(repo)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@test.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@test.com")
        .env("GIT_AUTHOR_DATE", date)
        .env("GIT_COMMITTER_DATE", date)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?} failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

fn page(label: &str) -> String {
    format!("page of {label}\n")
}

fn install_script(label: &str) -> String {
    format!("#!/bin/sh\necho installing {label}\n")
}

/// Commit a book whose every file names `label`, and tag it `label` unless
/// `label` is not a tag.
fn commit_book(repo: &Path, label: &str, date: &str, mermaid: bool, tag: bool) {
    let book = if mermaid {
        "[book]\nsrc = \"docs\"\n\n[preprocessor.mermaid]\ncommand = \"mdbook-mermaid\"\n\n\
         [output.html]\nadditional-js = [\"mermaid.min.js\", \"mermaid-init.js\"]\n"
    } else {
        "[book]\nsrc = \"docs\"\n"
    };
    write(&repo.join("book.toml"), book);
    write(&repo.join("docs/page.md"), &page(label));
    write(&repo.join("docs/install.sh"), &install_script(label));
    git(repo, date, &["add", "-A"]);
    git(repo, date, &["commit", "-q", "-m", label]);
    if tag {
        git(repo, date, &["tag", "-a", label, "-m", label]);
    }
}

struct Site {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    tools: PathBuf,
    out: PathBuf,
}

/// Releases in the order they were tagged, oldest first:
///
///   v0.9.0       2026-01   stable, no mermaid
///   v0.10.0      2026-02   stable, mermaid — the highest stable version
///   v1.0.0-rc.1  2026-03   prerelease above every stable version, mermaid
///   v0.9.1       2026-04   a patch on the 0.9 line, tagged last, no mermaid
///
/// then an untagged commit on main carrying two tags that are not releases,
/// and a working tree that differs from that commit.
fn site(failing: Option<&str>) -> Site {
    let tmp = common::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, "2026-01-01T00:00:00Z", &["init", "-q"]);

    let release = |label: &str, date: &str, mermaid: bool| {
        if failing == Some(label) {
            write(&repo.join("docs/FAIL"), "");
        }
        commit_book(&repo, label, date, mermaid, true);
        if failing == Some(label) {
            std::fs::remove_file(repo.join("docs/FAIL")).unwrap();
        }
    };
    release("v0.9.0", "2026-01-01T00:00:00Z", false);
    release("v0.10.0", "2026-02-01T00:00:00Z", true);
    release("v1.0.0-rc.1", "2026-03-01T00:00:00Z", true);
    git(&repo, "2026-04-01T00:00:00Z", &["switch", "-q", "-c", "maint-0.9", "v0.9.0"]);
    release("v0.9.1", "2026-04-01T00:00:00Z", false);
    git(&repo, "2026-04-01T00:00:00Z", &["switch", "-q", "main"]);

    commit_book(&repo, "untagged", "2026-05-01T00:00:00Z", true, false);
    git(&repo, "2026-05-01T00:00:00Z", &["tag", "tests/fixtures/go/x/v1.0.0"]);
    git(&repo, "2026-05-01T00:00:00Z", &["tag", "release-candidate"]);
    write(&repo.join("docs/page.md"), &page("the working tree"));
    write(&repo.join("docs/install.sh"), &install_script("the working tree"));
    write(&repo.join("docs/extra.md"), "an untracked page\n");

    let tools = tmp.path().join("tools");
    for (name, body) in [("mdbook", MDBOOK), ("mdbook-mermaid", MDBOOK_MERMAID)] {
        let path = tools.join(name);
        write(&path, body);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let out = tmp.path().join("site");
    Site {
        _tmp: tmp,
        repo,
        tools,
        out,
    }
}

impl Site {
    fn assemble_from(&self, repo: &Path) -> Output {
        let path = std::env::join_paths(
            std::iter::once(self.tools.clone()).chain(std::env::split_paths(
                &std::env::var_os("PATH").unwrap_or_default(),
            )),
        )
        .unwrap();
        Command::new("python3")
            .arg(member_root().join(ASSEMBLER))
            .arg("--repo")
            .arg(repo)
            .arg("--out")
            .arg(&self.out)
            .env("PATH", path)
            .output()
            .unwrap()
    }

    fn assemble(&self) -> Output {
        self.assemble_from(&self.repo)
    }

    fn assembled(&self) {
        let out = self.assemble();
        assert!(
            out.status.success(),
            "the assembler failed:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn read(&self, rel: &str) -> String {
        let path = self.out.join(rel);
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
    }

    fn versions(&self) -> serde_json::Value {
        serde_json::from_str(&self.read("versions.json")).unwrap()
    }
}

const RELEASES: [&str; 4] = ["v1.0.0-rc.1", "v0.10.0", "v0.9.1", "v0.9.0"];

#[test]
fn the_site_root_goes_to_the_highest_stable_release_whatever_order_the_tags_were_made_in() {
    if !python_with_tomllib() {
        return;
    }
    let site = site(None);
    let newest_first = git(
        &site.repo,
        "2026-06-01T00:00:00Z",
        &[
            "for-each-ref",
            "--sort=-creatordate",
            "--format=%(refname:short)",
            "refs/tags/v*",
        ],
    );
    assert_eq!(
        newest_first.lines().collect::<Vec<_>>(),
        ["v0.9.1", "v1.0.0-rc.1", "v0.10.0", "v0.9.0"],
        "the fixture's tag dates must disagree with version order, or a rule keyed on \
         date would pass this test"
    );

    site.assembled();
    let versions = site.versions();
    assert_eq!(versions["default"], "v0.10.0", "{versions:#}");
    let listed: Vec<(&str, bool)> = versions["versions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| {
            (
                v["version"].as_str().unwrap(),
                v["prerelease"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        listed,
        [
            ("v1.0.0-rc.1", true),
            ("v0.10.0", false),
            ("v0.9.1", false),
            ("v0.9.0", false),
        ],
        "versions.json lists every release highest first"
    );

    let index = site.read("index.html");
    assert!(
        index.contains("url=v0.10.0/\"") && index.contains("href=\"v0.10.0/\""),
        "the site root should redirect to v0.10.0/:\n{index}"
    );
}

#[test]
fn only_release_tags_are_published_each_from_its_own_tree() {
    if !python_with_tomllib() {
        return;
    }
    let site = site(None);
    site.assembled();

    let entries: BTreeSet<String> = std::fs::read_dir(&site.out)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    let expected: BTreeSet<String> = RELEASES
        .iter()
        .copied()
        .chain(["index.html", "versions.json", "install.sh"])
        .map(str::to_owned)
        .collect();
    assert_eq!(entries, expected, "the site root holds one directory per release tag");

    for tag in RELEASES {
        assert_eq!(site.read(&format!("{tag}/docs/page.md")), page(tag));
    }

    let mut leaked = Vec::new();
    let mut stack = vec![site.out.clone()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let bytes = std::fs::read(&path).unwrap();
            let text = String::from_utf8_lossy(&bytes);
            if text.contains("untagged")
                || text.contains("the working tree")
                || path.ends_with("docs/extra.md")
            {
                leaked.push(path.strip_prefix(&site.out).unwrap().display().to_string());
            }
        }
    }
    assert!(
        leaked.is_empty(),
        "the site carries content from the checkout or from a commit no release tag names: \
         {leaked:?}"
    );
}

#[test]
fn the_root_install_script_is_the_default_releases_byte_for_byte() {
    if !python_with_tomllib() {
        return;
    }
    let site = site(None);
    site.assembled();
    assert_eq!(site.read("install.sh"), install_script("v0.10.0"));
}

#[test]
fn every_book_gets_the_selector_after_its_own_scripts() {
    if !python_with_tomllib() {
        return;
    }
    let site = site(None);
    site.assembled();

    let selector = std::fs::read_to_string(member_root().join(SELECTOR)).unwrap();
    for tag in RELEASES {
        let mermaid = tag == "v0.10.0" || tag == "v1.0.0-rc.1";
        let scripts: Vec<String> =
            serde_json::from_str(&site.read(&format!("{tag}/additional-js.json"))).unwrap();
        let expected: &[&str] = if mermaid {
            &["mermaid.min.js", "mermaid-init.js", "version-selector.js"]
        } else {
            &["version-selector.js"]
        };
        assert_eq!(scripts, expected, "{tag}'s book scripts");
        assert_eq!(
            site.out.join(tag).join("mermaid-installed").exists(),
            mermaid,
            "{tag}: mermaid assets are installed exactly where the tag's book.toml asks for them"
        );
        assert_eq!(site.read(&format!("{tag}/version-selector.js")), selector);
    }
}

#[test]
fn a_release_whose_book_does_not_build_fails_the_assembly_naming_it() {
    if !python_with_tomllib() {
        return;
    }
    let site = site(Some("v0.9.1"));
    let out = site.assemble();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success() && stderr.contains("v0.9.1 does not build"),
        "a release whose book fails must fail the assembly and name it; exit {:?}, stderr:\n{stderr}",
        out.status.code()
    );
}

#[test]
fn a_v_tag_that_is_not_a_release_version_is_refused() {
    if !python_with_tomllib() {
        return;
    }
    let site = site(None);
    for bad in ["v1.2", "v1.0.0+build.5", "v01.0.0", "v2-wip"] {
        git(&site.repo, "2026-06-01T00:00:00Z", &["tag", bad, "v0.9.0"]);
        let out = site.assemble();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            !out.status.success() && stderr.contains(&format!("'{bad}'")),
            "tag {bad} should be refused by name; exit {:?}, stderr:\n{stderr}",
            out.status.code()
        );
        assert!(!site.out.exists(), "a refused tag set wrote {}", site.out.display());
        git(&site.repo, "2026-06-01T00:00:00Z", &["tag", "-d", bad]);
    }
}

#[test]
fn a_shallow_clone_is_refused_because_it_can_be_missing_releases() {
    if !python_with_tomllib() {
        return;
    }
    let site = site(None);
    let shallow = site.repo.with_file_name("shallow");
    git(
        site.repo.parent().unwrap(),
        "2026-06-01T00:00:00Z",
        &[
            "clone",
            "-q",
            "--depth",
            "1",
            &common::file_url(&site.repo),
            shallow.to_str().unwrap(),
        ],
    );
    let out = site.assemble_from(&shallow);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success() && stderr.contains("shallow clone"),
        "exit {:?}, stderr:\n{stderr}",
        out.status.code()
    );
}

#[test]
fn a_tag_set_with_no_stable_release_is_refused() {
    if !python_with_tomllib() {
        return;
    }
    let site = site(None);
    for tag in ["v0.9.0", "v0.10.0", "v0.9.1"] {
        git(&site.repo, "2026-06-01T00:00:00Z", &["tag", "-d", tag]);
    }
    let out = site.assemble();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success() && stderr.contains("no stable release tag"),
        "exit {:?}, stderr:\n{stderr}",
        out.status.code()
    );
}

#[test]
fn an_existing_output_directory_is_refused() {
    if !python_with_tomllib() {
        return;
    }
    let site = site(None);
    write(&site.out.join("stale.html"), "from an earlier build\n");
    let out = site.assemble();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success() && stderr.contains("already exists"),
        "exit {:?}, stderr:\n{stderr}",
        out.status.code()
    );
    assert_eq!(
        std::fs::read_dir(&site.out).unwrap().count(),
        1,
        "the refused run wrote into {}",
        site.out.display()
    );
}
