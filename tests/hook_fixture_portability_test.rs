//! Pins the two decisions that let a refusing-git-hook fixture run on Windows,
//! neither of which a green suite reports.
//!
//! A hook that refuses a `git worktree add` is planted by writing a `#!/bin/sh`
//! script into `.git/hooks/`. Nothing about that needs a Unix host: git reads
//! the `#!` line and looks the interpreter up itself, and its `access()` on
//! Windows masks off the execute bit, so a hook file that exists is one git
//! runs. The mode this repository sets is therefore Unix ceremony, and gating a
//! whole fixture on it costs the platform the test.
//!
//! What such a test cannot afford to lose is its proof that the hook fired. Each
//! one asserts the operation FAILED before it asserts anything about what the
//! failure left behind. Drop that assertion and the residue checks hold just as
//! well against an operation that succeeded — green, and green forever, on any
//! platform where the hook silently does not run. That is the one failure this
//! suite exists to make loud, because it is the one that does not announce
//! itself.
//!
//! So two pins, over the source text of `src/` and `tests/`:
//!
//!   - every site that plants or drives a refusing hook asserts the failure;
//!   - no such site is `#[cfg(unix)]`-gated.
//!
//! Residue, and it is the half worth knowing. The corpus is keyed on one hook
//! body, `exit 1` and nothing else, so a test that plants a hook doing
//! something more elaborate is outside this scan entirely — which is where
//! `cleanup_failure_preserves_original_error_with_manual_note` sits, gated for
//! a reason this suite does not adjudicate and states at its own site. Item
//! bodies are cut at the first
//! line that is a lone `}` at the item's own indentation, which is true of
//! rustfmt output and not of Rust in general — `cargo fmt --check` is what makes
//! it true here. The call graph is one hop: a test that reaches a planting
//! fixture through an intermediate helper is not seen. Assertion ORDER is not
//! checked, only presence, so a failure assertion moved below the residue
//! checks still passes here. And this file excludes itself from its own scan,
//! because it quotes every needle it searches for — the same reason the
//! citation gate stops at its own test boundary. Nothing outside that exclusion
//! is read by anything else.

use std::path::{Path, PathBuf};

/// The body of a hook planted to refuse, as it is spelled in source.
const REFUSING_HOOK: &str = "#!/bin/sh\\nexit 1\\n";

/// This file, which quotes every needle below and must not read itself.
const SELF: &str = "hook_fixture_portability_test.rs";

/// Ways a test says the operation it drove did not succeed.
const FAILURE_ASSERTIONS: &[&str] = &[".failure()", "expect_err(", "is_err()"];

/// One `fn` item: where it is, what sits above it, and what is inside it.
struct Item {
    file: String,
    line: usize,
    name: String,
    attrs: Vec<String>,
    body: String,
}

impl Item {
    fn site(&self) -> String {
        format!("{}:{} {}", self.file, self.line, self.name)
    }

    fn has_attr(&self, needle: &str) -> bool {
        self.attrs.iter().any(|a| a.contains(needle))
    }

    fn asserts_failure(&self) -> bool {
        FAILURE_ASSERTIONS.iter().any(|a| self.body.contains(a))
            || self
                .body
                .lines()
                .any(|l| l.contains('!') && l.contains("status.success()"))
    }
}

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read dir").flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// Every `fn` item under `src/` and `tests/`, this file excluded.
fn items() -> Vec<Item> {
    let root = crate_dir();
    let mut files = Vec::new();
    rust_files(&root.join("src"), &mut files);
    rust_files(&root.join("tests"), &mut files);
    files.sort();

    let mut out = Vec::new();
    for file in &files {
        let name = file
            .file_name()
            .expect("file has a name")
            .to_string_lossy()
            .into_owned();
        if name == SELF {
            continue;
        }
        let rel = file
            .strip_prefix(&root)
            .expect("walked path is under the crate")
            .to_string_lossy()
            .replace('\\', "/");
        let text = std::fs::read_to_string(file).expect("read source file");
        collect(&rel, &text, &mut out);
    }
    out
}

fn collect(rel: &str, text: &str, out: &mut Vec<Item>) {
    let lines: Vec<&str> = text.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        let indent = line.len() - line.trim_start().len();
        let trimmed = line.trim_start();
        let after_vis = trimmed.strip_prefix("pub ").unwrap_or(trimmed);
        let Some(rest) = after_vis.strip_prefix("fn ") else {
            continue;
        };
        let Some(name) = rest.split(['(', '<']).next() else {
            continue;
        };

        let mut attrs = Vec::new();
        let mut k = i;
        while k > 0 {
            let above = lines[k - 1].trim_start();
            if above.starts_with("#[") || above.starts_with("///") {
                attrs.push(above.to_string());
                k -= 1;
            } else {
                break;
            }
        }

        let closer = format!("{}}}", " ".repeat(indent));
        let end = lines[i + 1..]
            .iter()
            .position(|l| *l == closer)
            .map(|p| i + 1 + p)
            .unwrap_or(lines.len());

        out.push(Item {
            file: rel.to_string(),
            line: i + 1,
            name: name.to_string(),
            attrs,
            body: lines[i..end].join("\n"),
        });
    }
}

/// Items that plant a refusing hook, and items that call one that does.
fn hook_refusal_sites(all: &[Item]) -> Vec<&Item> {
    let planters: Vec<&str> = all
        .iter()
        .filter(|it| it.body.contains(REFUSING_HOOK))
        .map(|it| it.name.as_str())
        .collect();

    all.iter()
        .filter(|it| {
            it.body.contains(REFUSING_HOOK)
                || planters
                    .iter()
                    .any(|p| *p != it.name && it.body.contains(&format!("{p}(")))
        })
        .collect()
}

#[test]
fn the_scan_finds_the_hook_fixtures_it_claims_to_pin() {
    let all = items();
    assert!(
        all.len() >= 2_000,
        "expected at least 2000 fn items under src/ and tests/, got {} — the \
         walk or the item matcher broke, and every assertion below would then \
         hold over an empty corpus",
        all.len()
    );

    let sites = hook_refusal_sites(&all);
    assert!(
        sites.len() >= 7,
        "expected at least 7 refusing-hook sites, found {}: {:?}. The needle is \
         the hook body as it is spelled in source; if a fixture rewrites it, \
         this suite stops seeing the thing it exists to pin and says nothing",
        sites.len(),
        sites.iter().map(|it| it.site()).collect::<Vec<_>>()
    );
}

#[test]
fn every_refusing_hook_site_asserts_the_operation_failed() {
    let all = items();
    let unarmoured: Vec<String> = hook_refusal_sites(&all)
        .iter()
        .filter(|it| it.has_attr("#[test]"))
        .filter(|it| !it.asserts_failure())
        .map(|it| it.site())
        .collect();

    assert!(
        unarmoured.is_empty(),
        "these tests plant a hook that refuses, then check what the refusal \
         left behind, without ever asserting that the operation failed: {}. \
         Every residue check in them holds just as well against an operation \
         that succeeded, so on a platform where the hook does not run they pass \
         while testing nothing, and they do it silently",
        unarmoured.join(", ")
    );
}

#[test]
fn no_refusing_hook_site_is_unix_gated() {
    let all = items();
    let gated: Vec<String> = hook_refusal_sites(&all)
        .iter()
        .filter(|it| it.has_attr("cfg(unix)"))
        .map(|it| it.site())
        .collect();

    assert!(
        gated.is_empty(),
        "these sites plant or drive a refusing hook and are gated to Unix: {}. \
         git runs a shebang hook on Windows too — it resolves the interpreter \
         itself, and its access() masks off the execute bit — so the gate does \
         not name a capability Windows lacks. What it does is delete the test \
         there, where it neither fails nor reports. Set the mode inside a cfg \
         block and leave the item reachable. If a site here has grown a second \
         fixture that Windows really cannot build, that is a different \
         decision from this one and wants saying out loud rather than spelling \
         as a cfg",
        gated.join(", ")
    );
}
