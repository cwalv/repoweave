//! What each `--json` verb writes to stdout must satisfy the artifact its own
//! bytes name.
//!
//! `tests/schema_conformance_test.rs` validates serialized envelope types over
//! a wide corpus. That is the wrong instrument for one failure: a print path
//! that mints its own bytes. Serializing the envelope type and validating the
//! result is green whether or not any verb ever hands that type to
//! `serde_json`, so the shape of the regression the doctor harness was built
//! for — a hand-written `json!` literal beside a schema derived from a struct
//! nothing serialized — is invisible to it. This file runs the binary and
//! reads its stdout.
//!
//! The artifact each document is validated against is chosen by the `$schema`
//! URL in the document, not by the test. A verb that points a consumer at the
//! wrong file therefore fails here rather than being silently validated
//! against the file the test had in mind.
//!
//! # What one fixture can and cannot show
//!
//! Each test drives one real invocation, so it samples one outcome per verb —
//! the shape that fixture produces. Variant width is
//! `tests/schema_conformance_test.rs`'s job; the two are complements, and
//! neither alone is the pin. Every test here asserts its envelope is
//! non-empty, because an envelope carrying no records validates against
//! anything and would read as a pass over a verb that emitted nothing.
//!
//! # Residue
//!
//!   - `rwv doctor --json` exits non-zero when it finds anything. Exit status
//!     is not asserted here beyond "not a usage error": what is under test is
//!     the bytes, and doctor emits them on both paths.
//!
//! # NDJSON record conformance
//!
//! Verbs that support parallel mode (`-j N`, `N > 1`) stream one NDJSON line
//! per repo instead of a single envelope document. Each streamed line embeds
//! its own `$schema` URL pointing at a per-record artifact (e.g.
//! `fetch-record.json`, distinct from the envelope `fetch.json`). The
//! `*_ndjson_records_conform` tests drive `-j 2` and validate every emitted
//! line against the artifact its own `$schema` URL names, using the same
//! self-describing mechanism as the envelope tests above.

use serde_json::Value;
use std::path::{Path, PathBuf};

mod common;

use common::json_schema;

const MEMBER: &str = "github/example/server";
const PROJECT: &str = "web-app";

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

fn git(args: &[&str], dir: &Path) -> String {
    let out = common::git()
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@test.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@test.com")
        .output()
        .expect("git should be available");
    assert!(
        out.status.success(),
        "git {:?} in {} failed:\n{}",
        args,
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

/// A primary weave with one manifest repo and a project repo, both cloned from
/// local bare remotes so `rwv push` and `rwv update` have somewhere to go.
struct Weave {
    _root: tempfile::TempDir,
    parent: PathBuf,
    primary: PathBuf,
}

impl Weave {
    fn member(&self) -> PathBuf {
        self.primary.join(MEMBER)
    }

    fn project(&self) -> PathBuf {
        self.primary.join("projects").join(PROJECT)
    }
}

fn weave() -> Weave {
    let root = common::tempdir().expect("temp root");
    let parent = root.path().to_path_buf();
    let primary = parent.join("primary");
    let origin = parent.join("origin");
    std::fs::create_dir_all(&origin).unwrap();
    std::fs::create_dir_all(primary.join("github/example")).unwrap();
    std::fs::create_dir_all(primary.join("projects")).unwrap();

    let member_origin = origin.join("server.git");
    let project_origin = origin.join("web-app.git");
    for bare in [&member_origin, &project_origin] {
        git(
            &["init", "--bare", "-b", "main", &bare.to_string_lossy()],
            &origin,
        );
    }

    let member = primary.join(MEMBER);
    git(
        &[
            "clone",
            &member_origin.to_string_lossy(),
            &member.to_string_lossy(),
        ],
        &parent,
    );
    std::fs::write(member.join("README.md"), "init\n").unwrap();
    git(&["add", "."], &member);
    git(&["commit", "-m", "initial"], &member);
    git(&["push", "origin", "main"], &member);
    let sha = git(&["rev-parse", "HEAD"], &member);

    let project = primary.join("projects").join(PROJECT);
    git(
        &[
            "clone",
            &project_origin.to_string_lossy(),
            &project.to_string_lossy(),
        ],
        &parent,
    );
    let url = common::file_url(&member_origin);
    std::fs::write(
        project.join("rwv.toml"),
        format!(
            "[repositories]\n[repositories.\"{MEMBER}\"]\ntype = \"git\"\nurl = \"{url}\"\n\
             version = \"main\"\nrole = \"owned\"\n"
        ),
    )
    .unwrap();
    std::fs::write(project.join(".gitattributes"), "rwv.lock merge=rwv-ours\n").unwrap();
    write_lock(&project, &url, &sha);
    git(&["add", "-A"], &project);
    git(&["commit", "-m", "lock: initial"], &project);
    git(&["push", "origin", "main"], &project);

    // `rwv push` refuses a project repo whose remote has no recorded canonical
    // branch, which a clone of a then-empty bare repo does not have.
    for repo in [&member, &project] {
        git(&["remote", "set-head", "origin", "-a"], repo);
    }
    std::fs::write(primary.join(".rwv-active"), format!("{PROJECT}\n")).unwrap();

    Weave {
        _root: root,
        parent,
        primary,
    }
}

/// Round-tripped through the real parser and writer: a hand-formatted lock
/// that differs only in whitespace still diffs against a real relock.
fn write_lock(project: &Path, url: &str, sha: &str) {
    let raw = format!(
        "{{\"repositories\": {{{MEMBER:?}: {{\"type\": \"git\", \"url\": {url:?}, \
         \"version\": {sha:?}}}}}}}"
    );
    let lock = repoweave::manifest::LockFile::from_json_str(&raw).expect("lock parses");
    repoweave::lock::write_lock(&lock, &project.join("rwv.lock")).expect("lock writes");
}

/// A workweave forked from `weave`'s primary, placed explicitly so the test
/// does not have to guess the default container.
fn workweave(weave: &Weave) -> PathBuf {
    let dir = weave.parent.join("ww");
    let out = common::rwv()
        .args([
            "workweave",
            PROJECT,
            "create",
            "feat-a",
            "--dir",
            &dir.to_string_lossy(),
        ])
        .current_dir(&weave.primary)
        .output()
        .expect("rwv runs");
    assert!(
        out.status.success(),
        "rwv workweave create failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        dir.join(".rwv-workweave").is_file(),
        "no workweave marker at {}",
        dir.display()
    );
    dir
}

// ---------------------------------------------------------------------------
// Driving a verb and validating what it printed
// ---------------------------------------------------------------------------

/// Run `rwv <args>` in `cwd` and parse stdout as one JSON document.
fn emit(cwd: &Path, args: &[&str]) -> Value {
    let out = common::rwv()
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("rwv runs");
    let stdout = String::from_utf8(out.stdout).expect("stdout is UTF-8");
    assert_ne!(
        out.status.code(),
        Some(2),
        "rwv {args:?} in {} was a usage error, so nothing was emitted:\n{}",
        cwd.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !stdout.trim().is_empty(),
        "rwv {args:?} in {} printed no JSON (exit {:?}):\n{}",
        cwd.display(),
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!("rwv {args:?} printed something that is not one JSON document: {e}\n{stdout}")
    })
}

/// Validate `doc` against the committed artifact its own `$schema` names, and
/// assert that artifact is `verb`'s.
///
/// `records` is the envelope's array key; it must be non-empty, or the
/// document validates without saying anything about the records this verb
/// exists to emit.
fn assert_conforms(verb: &str, doc: &Value, records: &str) {
    let url = doc["$schema"]
        .as_str()
        .unwrap_or_else(|| panic!("rwv {verb} --json emitted no `$schema` string:\n{doc:#}"));
    let named = url
        .rsplit('/')
        .next()
        .and_then(|file| file.strip_suffix(".json"))
        .unwrap_or_else(|| panic!("rwv {verb} --json embedded `{url}`, which names no artifact"));
    assert_eq!(
        named,
        verb,
        "rwv {verb} --json points consumers at {}",
        json_schema::schema_path(named)
    );

    let entries = doc[records]
        .as_array()
        .unwrap_or_else(|| panic!("rwv {verb} --json emitted no `{records}` array:\n{doc:#}"));
    assert!(
        !entries.is_empty(),
        "rwv {verb} --json emitted an empty `{records}` — this fixture proves nothing about \
         the record shape"
    );

    let schema = json_schema::committed_schema(named);
    let (errors, walk) = json_schema::conform(doc, &schema);
    assert!(
        errors.is_empty(),
        "the bytes `rwv {verb} --json` wrote do not satisfy {}:\n  {}\n\nemitted:\n{doc:#}",
        json_schema::schema_path(named),
        errors.join("\n  ")
    );
    assert!(
        walk.properties_checked >= 3,
        "the walk never reached the envelope's properties, so the pass is vacuous: {walk:?}"
    );
}

// ---------------------------------------------------------------------------
// One verb, one invocation, one envelope
// ---------------------------------------------------------------------------

#[test]
fn status_json_wire_output_conforms() {
    let weave = weave();
    let doc = emit(&weave.primary, &["status", "--json"]);
    assert_conforms("status", &doc, "repos");
}

#[test]
fn doctor_json_wire_output_conforms() {
    let weave = weave();
    let doc = emit(&weave.primary, &["doctor", "--json"]);
    // A fresh weave raises findings on both channels; `issues` is what this
    // fixture reliably populates, and it is the array `violations` shares an
    // envelope with.
    assert_conforms("doctor", &doc, "issues");
}

#[test]
fn fetch_json_wire_output_conforms() {
    let weave = weave();
    let doc = emit(&weave.primary, &["fetch", "--json", "-j", "1"]);
    assert_conforms("fetch", &doc, "outcomes");
}

#[test]
fn update_json_wire_output_conforms() {
    let weave = weave();
    let doc = emit(&weave.primary, &["update", "--json", "-j", "1"]);
    assert_conforms("update", &doc, "repos");
}

#[test]
fn push_json_wire_output_conforms() {
    let weave = weave();
    let doc = emit(&weave.primary, &["push", "--json"]);
    assert_conforms("push", &doc, "outcomes");
}

#[test]
fn sync_json_wire_output_conforms() {
    let weave = weave();
    let ww = workweave(&weave);

    // Advance the primary and re-lock, so the workweave has something to
    // converge onto and the lock-freshness precondition holds.
    std::fs::write(weave.member().join("NOTES.md"), "advance\n").unwrap();
    git(&["add", "-A"], &weave.member());
    git(&["commit", "-m", "primary: advance"], &weave.member());
    let out = common::rwv()
        .args(["lock"])
        .current_dir(&weave.primary)
        .output()
        .expect("rwv runs");
    assert!(
        out.status.success(),
        "rwv lock failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    git(&["add", "-A"], &weave.project());
    git(&["commit", "-m", "lock: advance"], &weave.project());

    let primary = weave.primary.to_string_lossy().into_owned();
    let doc = emit(&ww, &["sync", &primary, "--json", "-j", "1"]);
    assert_conforms("sync", &doc, "outcomes");
}

#[test]
fn sync_to_json_wire_output_conforms() {
    let weave = weave();
    let ww = workweave(&weave);

    std::fs::write(ww.join(MEMBER).join("NOTES.md"), "workweave\n").unwrap();
    git(&["add", "-A"], &ww.join(MEMBER));
    git(&["commit", "-m", "ww: advance"], &ww.join(MEMBER));

    let doc = emit(&ww, &["sync-to", "--json"]);
    assert_conforms("sync-to", &doc, "outcomes");
}

// ---------------------------------------------------------------------------
// NDJSON record conformance (-j 2)
// ---------------------------------------------------------------------------

/// Run `rwv <args>` in `cwd` and parse stdout as NDJSON (one JSON object per
/// non-empty line). Returns the parsed lines.
fn emit_ndjson(cwd: &Path, args: &[&str]) -> Vec<Value> {
    let out = common::rwv()
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("rwv runs");
    let stdout = String::from_utf8(out.stdout).expect("stdout is UTF-8");
    assert_ne!(
        out.status.code(),
        Some(2),
        "rwv {args:?} in {} was a usage error:\n{}",
        cwd.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !stdout.trim().is_empty(),
        "rwv {args:?} in {} printed nothing (exit {:?}):\n{}",
        cwd.display(),
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|line| {
            serde_json::from_str(line).unwrap_or_else(|e| {
                panic!("rwv {args:?} printed a line that is not valid JSON: {e}\nline: {line}")
            })
        })
        .collect()
}

/// Validate each NDJSON record against the committed artifact its own
/// `$schema` URL names, asserting that artifact is `expected_record_artifact`.
///
/// A record URL pointing at a different artifact causes the `assert_eq` to
/// fire, naming both the expected and the actual artifact path so the failure
/// message is actionable without inspecting the binary.
fn assert_ndjson_conforms(verb: &str, records: &[Value], expected_record_artifact: &str) {
    assert!(
        !records.is_empty(),
        "rwv {verb} --json -j 2 emitted no records — fixture proves nothing"
    );
    for (i, record) in records.iter().enumerate() {
        let url = record["$schema"].as_str().unwrap_or_else(|| {
            panic!("rwv {verb} --json -j 2 record[{i}] has no `$schema` string:\n{record:#}")
        });
        let named = url
            .rsplit('/')
            .next()
            .and_then(|file| file.strip_suffix(".json"))
            .unwrap_or_else(|| {
                panic!(
                    "rwv {verb} --json -j 2 record[{i}] embedded `{url}`, which names no artifact"
                )
            });
        assert_eq!(
            named,
            expected_record_artifact,
            "rwv {verb} --json -j 2 record[{i}] points consumers at {}, expected {}",
            json_schema::schema_path(named),
            json_schema::schema_path(expected_record_artifact),
        );
        let schema = json_schema::committed_schema(named);
        let (errors, walk) = json_schema::conform(record, &schema);
        assert!(
            errors.is_empty(),
            "rwv {verb} --json -j 2 record[{i}] does not satisfy {}:\n  {}\n\nrecord:\n{record:#}",
            json_schema::schema_path(named),
            errors.join("\n  ")
        );
        assert!(
            walk.properties_checked >= 2,
            "walk on record[{i}] never reached the record's properties — pass is vacuous: \
             {walk:?}\nrecord:\n{record:#}"
        );
    }
}

#[test]
fn fetch_ndjson_records_conform() {
    let weave = weave();
    let records = emit_ndjson(&weave.primary, &["fetch", "--json", "-j", "2"]);
    assert_ndjson_conforms("fetch", &records, "fetch-record");
}

#[test]
fn update_ndjson_records_conform() {
    let weave = weave();
    let records = emit_ndjson(&weave.primary, &["update", "--json", "-j", "2"]);
    assert_ndjson_conforms("update", &records, "update-record");
}

#[test]
fn push_ndjson_records_conform() {
    let weave = weave();
    let records = emit_ndjson(&weave.primary, &["push", "--json", "-j", "2"]);
    assert_ndjson_conforms("push", &records, "push-record");
}

#[test]
fn sync_ndjson_records_conform() {
    let weave = weave();
    let ww = workweave(&weave);

    std::fs::write(weave.member().join("NOTES.md"), "advance\n").unwrap();
    git(&["add", "-A"], &weave.member());
    git(&["commit", "-m", "primary: advance"], &weave.member());
    let out = common::rwv()
        .args(["lock"])
        .current_dir(&weave.primary)
        .output()
        .expect("rwv runs");
    assert!(
        out.status.success(),
        "rwv lock failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    git(&["add", "-A"], &weave.project());
    git(&["commit", "-m", "lock: advance"], &weave.project());

    let primary = weave.primary.to_string_lossy().into_owned();
    let records = emit_ndjson(&ww, &["sync", &primary, "--json", "-j", "2"]);
    assert_ndjson_conforms("sync", &records, "sync-record");
}

#[test]
fn sync_to_ndjson_records_conform() {
    let weave = weave();
    let ww = workweave(&weave);

    std::fs::write(ww.join(MEMBER).join("NOTES.md"), "workweave\n").unwrap();
    git(&["add", "-A"], &ww.join(MEMBER));
    git(&["commit", "-m", "ww: advance"], &ww.join(MEMBER));

    let records = emit_ndjson(&ww, &["sync-to", "--json", "-j", "2"]);
    assert_ndjson_conforms("sync-to", &records, "sync-to-record");
}
