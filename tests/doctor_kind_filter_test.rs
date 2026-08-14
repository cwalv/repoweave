//! `rwv doctor --kind` — the report filter.
//!
//! The filter narrows the report to named finding kinds, text and `--json`
//! both. Three pins, each an executed outcome:
//!
//!   1. The text view shows only the named kind, ITEMIZED — the filter is
//!      the drill-down that replaces `--json | jq`, so a kind whose class
//!      normally collapses to a per-class count line renders its records.
//!      Findings of other kinds are absent.
//!   2. `--json` carries the subset: the same records it would carry
//!      unfiltered, minus the other kinds — record shape untouched.
//!   3. An unknown kind name refuses, naming the valid set — a typo must
//!      not produce an empty report that reads as "clean".
//!
//! The fixture holds findings of two kinds at once: two redundant orphaned
//! savepoints (a collapsed reclamation class) and one misnamed workweave
//! directory (an itemized tree-integrity finding), so every assertion about
//! presence has a live absence to check against.

use std::path::{Path, PathBuf};

mod common;

fn make_workspace(parent: &Path, name: &str) -> PathBuf {
    let root = parent.join(name);
    std::fs::create_dir_all(root.join("github")).unwrap();
    std::fs::create_dir_all(root.join("projects")).unwrap();
    root
}

fn init_git_repo(path: &Path) -> String {
    std::fs::create_dir_all(path).unwrap();
    let run = |args: &[&str], dir: &Path| {
        let out = common::git()
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@test.com")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@test.com")
            .output()
            .expect("git command failed to start");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    run(&["init", "--initial-branch=main", "-q"], path);
    std::fs::write(path.join("README.md"), "init\n").unwrap();
    run(&["add", "README.md"], path);
    run(&["commit", "-q", "-m", "init"], path);
    run(&["rev-parse", "HEAD"], path)
}

fn write_manifest(project_dir: &Path, repos: &[(&str, &str)]) {
    std::fs::create_dir_all(project_dir).unwrap();
    let mut manifest = String::new();
    for (path, url) in repos {
        manifest.push_str(&format!(
            "[repositories.\"{path}\"]\ntype = \"git\"\nurl = \"{url}\"\nversion = \"main\"\nrole = \"owned\"\n"
        ));
    }
    std::fs::write(project_dir.join("rwv.toml"), manifest).unwrap();
}

fn add_savepoint(repo: &Path, op_id: &str, sha: &str) {
    let out = common::git()
        .args(["update-ref", &format!("refs/rwv/pre-op/{op_id}"), sha])
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(out.status.success());
}

/// Two redundant savepoints + one misnamed workweave dir, two kinds live.
fn two_kind_fixture(parent: &Path) -> PathBuf {
    let root = make_workspace(parent, "ws");
    let repo_rel = "github/acme/server";
    let repo_abs = root.join(repo_rel);
    let head_sha = init_git_repo(&repo_abs);
    write_manifest(
        &root.join("projects").join("my-app"),
        &[(repo_rel, "https://github.com/acme/server.git")],
    );
    add_savepoint(&repo_abs, "511111111111111111", &head_sha);
    add_savepoint(&repo_abs, "522222222222222222", &head_sha);

    // Misnamed dir: valid marker naming this primary, project half of the
    // basename disagreeing with it.
    let ww_dir = root
        .parent()
        .expect("workspace root has a parent")
        .join(".workweaves")
        .join("other--feat-x");
    std::fs::create_dir_all(&ww_dir).unwrap();
    let primary = root.canonicalize().unwrap();
    std::fs::write(
        ww_dir.join(".rwv-workweave"),
        format!(
            "{{\"primary\":\"{}\",\"project\":\"my-app\",\"parent\":\"{}\"}}",
            primary.display(),
            primary.display()
        ),
    )
    .unwrap();
    root
}

fn rwv() -> assert_cmd::Command {
    common::rwv()
}

/// Pin 1: the text view of `--kind` shows only the named kind, itemized.
///
/// **Mutation evidence**, each an actual revert in `run_check` (check.rs):
/// dropping the `filter.admits` retain reddens the absence assertion (the
/// misnamed-dir line comes back); rendering the filtered set through
/// `violations_to_issues` instead of `itemized_violations_to_issues`
/// reddens the itemized assertions (the count line reappears and the
/// per-item lines vanish).
#[test]
fn kind_filter_text_shows_only_the_named_kind_itemized() {
    let tmp = common::tempdir().unwrap();
    let root = two_kind_fixture(tmp.path());

    let out = rwv()
        .args(["doctor", "--kind", "orphaned-savepoint"])
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(out.status.success(), "warnings only — exit 0");
    let stdout = String::from_utf8_lossy(&out.stdout);

    // Itemized: two per-item savepoint lines, not one count line.
    let item_lines = stdout
        .lines()
        .filter(|l| l.contains("orphaned-savepoint"))
        .count();
    assert_eq!(
        item_lines, 2,
        "the filter is the drill-down: each record renders its own line; \
         got:\n{stdout}"
    );
    assert!(
        !stdout.contains("redundant orphaned-savepoint findings"),
        "the count line must not render under the filter; got:\n{stdout}"
    );
    // Other kinds absent — including itemized ones and integration noise.
    assert!(
        !stdout.contains("disagrees with its records"),
        "a finding of another kind must be absent; got:\n{stdout}"
    );
    assert!(
        !stdout.contains("vscode-workspace") && !stdout.contains("rwv-ours"),
        "integration issues are not violations of any kind and are out of a \
         filtered view; got:\n{stdout}"
    );
}

/// Pin 2: `--json --kind` carries the subset — same records, fewer of them.
///
/// **Mutation evidence**: dropping the `filter.admits` retain in
/// `run_check_json` reddens the subset assertion (the tree-integrity
/// record comes back).
#[test]
fn kind_filter_json_carries_the_subset_with_unchanged_records() {
    let tmp = common::tempdir().unwrap();
    let root = two_kind_fixture(tmp.path());

    let parse = |raw: &[u8]| -> serde_json::Value {
        serde_json::from_slice(raw).expect("doctor --json parses")
    };

    let unfiltered = parse(
        &rwv()
            .args(["doctor", "--json"])
            .current_dir(&root)
            .output()
            .unwrap()
            .stdout,
    );
    let filtered = parse(
        &rwv()
            .args(["doctor", "--json", "--kind", "orphaned-savepoint"])
            .current_dir(&root)
            .output()
            .unwrap()
            .stdout,
    );

    let of_kind = |doc: &serde_json::Value, kind: &str| -> Vec<serde_json::Value> {
        doc["violations"]
            .as_array()
            .expect("violations is an array")
            .iter()
            .filter(|v| v["kind"] == kind)
            .cloned()
            .collect()
    };

    // The unfiltered run carries both kinds (the fixture's premise). The
    // misnamed dir yields two tree-integrity records: `misnamed-dir` and,
    // being unregistered, `unregistered-workweave`.
    assert_eq!(of_kind(&unfiltered, "orphaned-savepoint").len(), 2);
    assert_eq!(of_kind(&unfiltered, "workweave-tree-integrity").len(), 2);

    // The filtered run carries the named kind's records BYTE-IDENTICAL to
    // their unfiltered selves, and nothing of any other kind.
    assert_eq!(
        of_kind(&filtered, "orphaned-savepoint"),
        of_kind(&unfiltered, "orphaned-savepoint"),
        "filtering selects a subset; it must not reshape the records"
    );
    let filtered_total = filtered["violations"].as_array().unwrap().len();
    assert_eq!(
        filtered_total, 2,
        "no record of another kind survives the filter; got: {filtered}"
    );
}

/// Pin 3: an unknown kind refuses, naming the valid set. Silently-empty
/// output would read as "clean", which is the one thing a typo must never
/// produce.
///
/// **Mutation evidence**: skipping the validation in `KindFilter::new`
/// (accepting unknown names) reddens the exit-status assertion — the run
/// succeeds with an empty report instead of refusing.
#[test]
fn kind_filter_refuses_an_unknown_kind_naming_the_valid_set() {
    let tmp = common::tempdir().unwrap();
    let root = two_kind_fixture(tmp.path());

    let out = rwv()
        .args(["doctor", "--kind", "no-such-kind"])
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "an unknown kind must refuse, not render an empty (clean-looking) \
         report"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no-such-kind"),
        "the refusal names the offending value; got:\n{stderr}"
    );
    assert!(
        stderr.contains("orphaned-savepoint") && stderr.contains("branch-discipline"),
        "the refusal lists the valid set the operator can pick from; \
         got:\n{stderr}"
    );
}

/// The register agreement behind the filter: `CheckViolation::wire_kind`
/// must equal the `kind` tag serialization actually emits, for every corpus
/// specimen — and every wire kind must be admissible by name.
#[test]
fn wire_kind_agrees_with_the_serialized_tag_for_the_whole_corpus() {
    use common::doctor_corpus::corpus;
    use repoweave::check::{build_doctor_json, KindFilter};
    use std::collections::HashMap;

    let valid: std::collections::BTreeSet<String> = KindFilter::valid_kinds().into_iter().collect();
    assert!(
        valid.len() >= 30,
        "the schema walk recovered only {} kinds — it has stopped reading \
         the register",
        valid.len()
    );

    for v in corpus() {
        let declared = v.wire_kind().to_string();
        let doc = serde_json::to_value(build_doctor_json(
            vec![v],
            Vec::new(),
            Path::new("/ws"),
            &HashMap::new(),
            None,
            Vec::new(),
            vec![],
        ))
        .expect("doctor payload serializes");
        let emitted = doc["violations"][0]["kind"]
            .as_str()
            .expect("a violation serializes a kind tag")
            .to_string();
        assert_eq!(
            declared, emitted,
            "wire_kind must state the tag serialization emits"
        );
        assert!(
            valid.contains(&declared),
            "every emitted kind is admissible by name: `{declared}` missing \
             from the schema-derived set {valid:?}"
        );
    }
}
