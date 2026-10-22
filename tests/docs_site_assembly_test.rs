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
const SITE_PATH: &str = "/repoweave/";

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
        common::report_skip(
            "python3 has no tomllib (needs 3.11+), which the assembler reads book.toml with",
        );
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
    git(
        &repo,
        "2026-04-01T00:00:00Z",
        &["switch", "-q", "-c", "maint-0.9", "v0.9.0"],
    );
    release("v0.9.1", "2026-04-01T00:00:00Z", false);
    git(&repo, "2026-04-01T00:00:00Z", &["switch", "-q", "main"]);

    commit_book(&repo, "untagged", "2026-05-01T00:00:00Z", true, false);
    git(
        &repo,
        "2026-05-01T00:00:00Z",
        &["tag", "tests/fixtures/go/x/v1.0.0"],
    );
    git(&repo, "2026-05-01T00:00:00Z", &["tag", "release-candidate"]);
    write(&repo.join("docs/page.md"), &page("the working tree"));
    write(
        &repo.join("docs/install.sh"),
        &install_script("the working tree"),
    );
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
    fn assembler(&self, repo: &Path) -> Command {
        let path = std::env::join_paths(std::iter::once(self.tools.clone()).chain(
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
        ))
        .unwrap();
        let mut command = Command::new("python3");
        command
            .arg(member_root().join(ASSEMBLER))
            .arg("--repo")
            .arg(repo)
            .args(["--site-path", SITE_PATH])
            .arg("--out")
            .arg(&self.out)
            .env("PATH", path);
        command
    }

    fn assemble_from(&self, repo: &Path) -> Output {
        self.assembler(repo).output().unwrap()
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
        .chain(["index.html", "404.html", "versions.json", "install.sh"])
        .map(str::to_owned)
        .collect();
    assert_eq!(
        entries, expected,
        "the site root holds one directory per release tag"
    );

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

/// Git for Windows installs `core.autocrlf=true`, under which a checkout
/// writes text with CRLF line endings. A site assembled on such a host
/// publishes what each tag records, not that host's rendering of it.
#[test]
fn a_host_that_converts_line_endings_on_checkout_publishes_each_tags_own_bytes() {
    if !python_with_tomllib() {
        return;
    }
    let site = site(None);
    let converting = site.repo.with_file_name("converting.gitconfig");
    write(&converting, "[core]\n\tautocrlf = true\n");
    let rendered = common::git()
        .args(["cat-file", "--filters", "v0.9.0:docs/page.md"])
        .current_dir(&site.repo)
        .env("GIT_CONFIG_GLOBAL", &converting)
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&rendered.stdout),
        page("v0.9.0").replace('\n', "\r\n"),
        "fixture check: a checkout under this config does not convert, so the \
         assembly below cannot tell a pinned export from an unpinned one"
    );

    let out = site
        .assembler(&site.repo)
        .env("GIT_CONFIG_GLOBAL", &converting)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "the assembler failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    for tag in RELEASES {
        for (file, recorded) in [
            ("docs/page.md", page(tag)),
            ("docs/install.sh", install_script(tag)),
        ] {
            let published = std::fs::read(site.out.join(tag).join(file)).unwrap();
            assert_eq!(
                String::from_utf8_lossy(&published),
                recorded,
                "{tag}/{file} should hold the bytes {tag} records"
            );
        }
    }
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
        assert!(
            !site.out.exists(),
            "a refused tag set wrote {}",
            site.out.display()
        );
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

/// Runs the 404 page's script under node with only what it touches stubbed:
/// `location`, and the one link it fills in.
const NOT_FOUND_DRIVER: &str = r#"
const vm = require("vm");
const { script, url } = JSON.parse(require("fs").readFileSync(0, "utf8"));
const u = new URL(url);
const home = { href: "", textContent: "" };
let redirect = null;
vm.runInNewContext(script, {
  location: {
    pathname: u.pathname,
    search: u.search,
    hash: u.hash,
    replace: (target) => { redirect = String(target); },
  },
  document: { getElementById: (id) => (id === "home" ? home : null) },
});
process.stdout.write(JSON.stringify({ redirect, home: home.href }));
"#;

const ORIGIN: &str = "https://docs.test";

#[derive(Debug, PartialEq)]
enum Visit {
    Page(String),
    NotFound { at: String, home: String },
}

impl Site {
    fn not_found_script(&self) -> String {
        let page = self.read("404.html");
        let start = page.find("<script>").expect("404.html has a script") + "<script>".len();
        let end = page[start..].find("</script>").unwrap() + start;
        page[start..end].to_owned()
    }

    /// Open `path` the way Pages serves it: a file under the site root is a
    /// page, and any other address gets the root 404.html, whose redirects are
    /// followed. Returns the redirects taken and where they ended.
    fn visit(&self, path: &str) -> (Vec<String>, Visit) {
        let script = self.not_found_script();
        let mut hops: Vec<String> = Vec::new();
        let mut at = path.to_owned();
        for _ in 0..4 {
            let file_path = at.split(['?', '#']).next().unwrap();
            if let Some(rel) = file_path.strip_prefix(SITE_PATH) {
                let file = self.out.join(rel);
                if file.is_file() || file.join("index.html").is_file() {
                    return (hops, Visit::Page(rel.to_owned()));
                }
            }
            let input = serde_json::json!({ "script": script, "url": format!("{ORIGIN}{at}") });
            let mut node = Command::new("node")
                .args(["-e", NOT_FOUND_DRIVER])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            use std::io::Write;
            node.stdin
                .take()
                .unwrap()
                .write_all(input.to_string().as_bytes())
                .unwrap();
            let out = node.wait_with_output().unwrap();
            assert!(
                out.status.success(),
                "the 404 script failed at {at}:\n{}",
                String::from_utf8_lossy(&out.stderr)
            );
            let served: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
            match served["redirect"].as_str() {
                Some(target) => {
                    assert_eq!(
                        served["home"], target,
                        "the 404 page links where it redirects"
                    );
                    hops.push(target.to_owned());
                    at = target.to_owned();
                }
                None => {
                    let home = served["home"].as_str().unwrap().to_owned();
                    return (hops, Visit::NotFound { at, home });
                }
            }
        }
        panic!("{path} is still redirecting after {hops:?}");
    }
}

#[test]
fn an_address_with_no_version_opens_the_default_version_and_a_versioned_one_never_redirects() {
    if !python_with_tomllib() || common::skip_without_tool("node") {
        return;
    }
    let site = site(None);
    site.assembled();

    assert_eq!(
        site.visit("/repoweave/docs/page.md?q=1#usage"),
        (
            vec!["/repoweave/v0.10.0/docs/page.md?q=1#usage".to_owned()],
            Visit::Page("v0.10.0/docs/page.md".to_owned())
        ),
        "an address with no version opens the same address in the default version, \
         query and fragment kept"
    );
    assert_eq!(
        site.visit("/repoweave/v0.9.0/missing.html"),
        (
            vec![],
            Visit::NotFound {
                at: "/repoweave/v0.9.0/missing.html".to_owned(),
                home: "/repoweave/v0.9.0/".to_owned(),
            }
        ),
        "a missing page under a version is not found there, with a link to that version"
    );
    assert_eq!(
        site.visit("/repoweave/missing.html"),
        (
            vec!["/repoweave/v0.10.0/missing.html".to_owned()],
            Visit::NotFound {
                at: "/repoweave/v0.10.0/missing.html".to_owned(),
                home: "/repoweave/v0.10.0/".to_owned(),
            }
        ),
        "a page the default version lacks ends on not found after one redirect"
    );
    assert_eq!(
        site.visit("/elsewhere/page.html"),
        (
            vec![],
            Visit::NotFound {
                at: "/elsewhere/page.html".to_owned(),
                home: SITE_PATH.to_owned(),
            }
        ),
        "an address outside the site root is not redirected"
    );
}

#[test]
fn a_site_path_that_is_not_slash_delimited_is_refused() {
    if !python_with_tomllib() {
        return;
    }
    let site = site(None);
    for bad in ["/repoweave", "repoweave/", ""] {
        let out = Command::new("python3")
            .arg(member_root().join(ASSEMBLER))
            .arg("--repo")
            .arg(&site.repo)
            .args(["--site-path", bad])
            .arg("--out")
            .arg(&site.out)
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            !out.status.success() && stderr.contains("must start and end with /"),
            "--site-path {bad:?}: exit {:?}, stderr:\n{stderr}",
            out.status.code()
        );
    }
}
