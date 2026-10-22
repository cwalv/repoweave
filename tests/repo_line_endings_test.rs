//! Every text file this repository tracks checks out LF on a host whose git
//! converts line endings, and the only paths kept from that are named here.
//!
//! What reads this repository's bytes is not only a compiler: the suite scans
//! its own sources and docs, `include_str!` embeds the docs in the binary, and
//! a compile probe edits a copy of a module by matching `\n`. A checkout that
//! converted them would hand all of those CRLF. `.gitattributes` declares
//! `* text=auto eol=lf`, which outranks every host's `core.autocrlf` and
//! `core.eol`; this pins that declaration by checking out HEAD the way Git for
//! Windows' default config would.
//!
//! Scope: HEAD's tree, read into an index of the test's own and checked out
//! into a directory of its own, with the repository's working tree, index and
//! object store untouched, and with no host-level attributes file consulted.
//! So an edit to `.gitattributes` is invisible here until it is committed. Not
//! covered: an attributes file in the repository's own `info/` directory, and a
//! working-tree file someone converted to CRLF by hand, which git reads as
//! unchanged.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

mod common;

/// The tracked paths that hold CRLF on purpose.
const KEPT_FROM_LF: &[&str] = &[
    "tests/fixtures/gita/groups.csv",
    "tests/fixtures/gita/repos.csv",
];

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// HEAD's tree in an index of its own, read by git as a host converting line
/// endings would: `core.autocrlf=true` and no attributes beyond the tree's.
struct HeadCheckout {
    tmp: tempfile::TempDir,
    index: PathBuf,
    empty_work_tree: PathBuf,
    host: PathBuf,
}

impl HeadCheckout {
    fn new() -> Self {
        let tmp = common::tempdir().unwrap();
        let index = tmp.path().join("index");
        let empty_work_tree = tmp.path().join("empty");
        std::fs::create_dir(&empty_work_tree).unwrap();
        let host = tmp.path().join("host.gitattributes");
        std::fs::write(&host, "").unwrap();
        let checkout = HeadCheckout {
            tmp,
            index,
            empty_work_tree,
            host,
        };
        checkout.git(&["read-tree", "HEAD"]);
        checkout
    }

    /// git against the repository, with this index, an empty working tree
    /// so a `.gitattributes` there cannot stand in for the tree's, and the
    /// converting host's config.
    fn command(&self) -> Command {
        let mut command = common::git();
        command
            .current_dir(repo_root())
            .env("GIT_INDEX_FILE", &self.index)
            .env("GIT_ATTR_NOSYSTEM", "1")
            .arg("-c")
            .arg(format!("core.attributesFile={}", self.host.display()))
            .args(["-c", "core.autocrlf=true"])
            .arg("--work-tree")
            .arg(&self.empty_work_tree);
        command
    }

    fn git(&self, args: &[&str]) -> Vec<u8> {
        self.git_with_stdin(args, &[])
    }

    fn git_with_stdin(&self, args: &[&str], stdin: &[u8]) -> Vec<u8> {
        use std::io::Write;
        let mut child = self
            .command()
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let mut pipe = child.stdin.take().unwrap();
        let input = stdin.to_vec();
        let writer = std::thread::spawn(move || pipe.write_all(&input));
        let out = child.wait_with_output().unwrap();
        writer.join().unwrap().unwrap();
        assert!(
            out.status.success(),
            "git {args:?} failed:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        out.stdout
    }

    fn tracked(&self) -> Vec<String> {
        String::from_utf8(self.git(&["ls-files", "-z"]))
            .unwrap()
            .split_terminator('\0')
            .map(str::to_owned)
            .collect()
    }

    /// Check out `paths` under `dest`, returning each one's bytes.
    fn check_out(&self, dest: &Path, paths: &[String]) -> Vec<(String, Vec<u8>)> {
        let input: Vec<u8> = paths.iter().flat_map(|p| p.bytes().chain([0])).collect();
        self.git_with_stdin(
            &[
                "checkout-index",
                "-z",
                "--stdin",
                &format!("--prefix={}/", dest.display()),
            ],
            &input,
        );
        paths
            .iter()
            .map(|p| {
                let bytes = std::fs::read(common::under(dest, p))
                    .unwrap_or_else(|e| panic!("{p} was not checked out: {e}"));
                (p.clone(), bytes)
            })
            .collect()
    }
}

fn holds_cr(bytes: &[u8]) -> bool {
    bytes.contains(&b'\r')
}

#[test]
fn every_text_file_checks_out_lf_on_a_host_that_converts_line_endings() {
    let head = HeadCheckout::new();
    let tracked = head.tracked();
    assert!(
        tracked.len() > 100,
        "HEAD tracks {} paths, which is not this repository: a checkout of \
         nothing holds no CR",
        tracked.len()
    );

    let dest = head.tmp.path().join("checkout");
    let with_cr: BTreeSet<String> = head
        .check_out(&dest, &tracked)
        .into_iter()
        .filter(|(_, bytes)| holds_cr(bytes))
        .map(|(path, _)| path)
        .collect();
    let kept: BTreeSet<String> = KEPT_FROM_LF.iter().map(|p| (*p).to_owned()).collect();
    assert_eq!(
        with_cr, kept,
        "a checkout under core.autocrlf=true wrote CR into the left-hand paths; \
         every text file must check out LF, and a file that must hold CRLF is \
         named in .gitattributes with -text and in KEPT_FROM_LF"
    );
}

/// Without the tree's `.gitattributes`, the same checkout converts: the host
/// config does engage, so the LF above is the declaration's doing.
#[test]
fn the_same_checkout_without_the_declaration_converts() {
    let head = HeadCheckout::new();
    head.git(&["update-index", "--force-remove", ".gitattributes"]);
    let dest = head.tmp.path().join("checkout");
    let checked = head.check_out(&dest, &["Cargo.toml".to_owned()]);
    let bytes = &checked[0].1;
    assert!(
        bytes.windows(2).any(|w| w == b"\r\n"),
        "Cargo.toml checked out under core.autocrlf=true with no attributes holds no CRLF"
    );
}

#[test]
fn the_only_paths_kept_from_lf_are_the_named_crlf_fixtures() {
    let head = HeadCheckout::new();
    let tracked = head.tracked();
    let input: Vec<u8> = tracked.iter().flat_map(|p| p.bytes().chain([0])).collect();
    let report = String::from_utf8(head.git_with_stdin(
        &["check-attr", "--cached", "-z", "--stdin", "text", "eol"],
        &input,
    ))
    .unwrap();
    let fields: Vec<&str> = report.split_terminator('\0').collect();
    assert_eq!(
        fields.len(),
        tracked.len() * 2 * 3,
        "check-attr should report text and eol for every tracked path"
    );
    let mut kept = BTreeSet::new();
    for record in fields.chunks(3) {
        let [path, attr, value] = record else {
            unreachable!()
        };
        let lf = match *attr {
            "text" => matches!(*value, "set" | "auto"),
            "eol" => *value == "lf",
            _ => panic!("check-attr reported {attr}, which was not asked for"),
        };
        if !lf {
            kept.insert((*path).to_owned());
        }
    }
    let named: BTreeSet<String> = KEPT_FROM_LF.iter().map(|p| (*p).to_owned()).collect();
    assert_eq!(
        kept, named,
        "the paths .gitattributes keeps from `text eol=lf` must be exactly the \
         named CRLF fixtures"
    );
}
