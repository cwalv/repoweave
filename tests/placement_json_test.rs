//! Under `--json`, file counts reach a consumer only in placement records
//! `{path, absolute_path, files}`, each naming the checkout it describes: the
//! envelope's `placements`, or under `-j N` a line of their own whose
//! `$schema` names `placement-record.json`. No outcome record carries them.

use serde_json::Value;
use std::path::{Path, PathBuf};

mod common;

use common::json_schema;

const MEMBER: &str = "github/org/lib";
const EXTRA: &str = "github/org/extra";
const PROJECT: &str = "app";

struct Weave {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    primary: PathBuf,
    origin: PathBuf,
}

impl Weave {
    fn member(&self) -> PathBuf {
        self.primary.join(MEMBER)
    }

    fn project(&self) -> PathBuf {
        self.primary.join("projects").join(PROJECT)
    }

    fn seat(&self, name: &str) -> PathBuf {
        succeed(&self.primary, &["workweave", PROJECT, "create", name]);
        self.root
            .join(".workweaves")
            .join(format!("{PROJECT}--{name}"))
    }

    /// Commit `files` to the member's origin from a scratch clone, leaving the
    /// weave's own clone where it was.
    fn advance_origin(&self, files: &[&str]) -> String {
        let scratch = self.root.join("scratch");
        common::git_in(
            &self.root,
            &[
                "clone",
                "-q",
                &self.origin.to_string_lossy(),
                &scratch.to_string_lossy(),
            ],
        );
        identify(&scratch);
        commit(&scratch, files, "upstream");
        common::git_in(&scratch, &["push", "-q", "origin", "main"]);
        let sha = common::git_in(&scratch, &["rev-parse", "HEAD"]);
        std::fs::remove_dir_all(&scratch).unwrap();
        sha
    }
}

fn identify(repo: &Path) {
    common::git_in(repo, &["config", "user.email", "test@test.com"]);
    common::git_in(repo, &["config", "user.name", "Test"]);
}

fn commit(repo: &Path, files: &[&str], message: &str) {
    for file in files {
        std::fs::write(repo.join(file), format!("{message} {file}\n")).unwrap();
    }
    common::git_in(repo, &["add", "."]);
    common::git_in(repo, &["commit", "-q", "-m", message]);
}

fn init_repo(path: &Path) -> String {
    std::fs::create_dir_all(path).unwrap();
    common::git_in(path, &["init", "-q", "-b", "main"]);
    identify(path);
    commit(path, &["README"], "initial");
    common::git_in(path, &["rev-parse", "HEAD"])
}

fn entry(path: &str, url: &str) -> String {
    entry_as(path, url, "owned")
}

fn entry_as(path: &str, url: &str, role: &str) -> String {
    format!(
        "[repositories.\"{path}\"]\ntype = \"git\"\nurl = \"{url}\"\nversion = \"main\"\n\
         role = \"{role}\"\n"
    )
}

/// A primary whose project `app` holds [`MEMBER`], an owned clone of a bare
/// origin, pinned and committed.
fn weave() -> Weave {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let primary = root.join("primary");
    let origin = root.join("origin").join("lib.git");
    std::fs::create_dir_all(origin.parent().unwrap()).unwrap();
    common::git_in(
        &root,
        &[
            "init",
            "-q",
            "--bare",
            "-b",
            "main",
            &origin.to_string_lossy(),
        ],
    );
    let member = primary.join(MEMBER);
    std::fs::create_dir_all(member.parent().unwrap()).unwrap();
    common::git_in(
        &root,
        &[
            "clone",
            "-q",
            &origin.to_string_lossy(),
            &member.to_string_lossy(),
        ],
    );
    identify(&member);
    commit(&member, &["README"], "initial");
    common::git_in(&member, &["push", "-q", "origin", "main"]);
    let sha = common::git_in(&member, &["rev-parse", "HEAD"]);

    let project = primary.join("projects").join(PROJECT);
    init_repo(&project);
    std::fs::write(project.join(".gitattributes"), "rwv.lock merge=rwv-ours\n").unwrap();
    let url = common::file_url(&origin);
    std::fs::write(project.join("rwv.toml"), entry(MEMBER, &url)).unwrap();
    common::fixture_lock(&project, &[(MEMBER, &url, &sha)]);
    common::git_in(&project, &["add", "-A"]);
    common::git_in(&project, &["commit", "-q", "-m", "lock: initial"]);
    std::fs::write(primary.join(".rwv-active"), format!("{PROJECT}\n")).unwrap();
    Weave {
        _tmp: tmp,
        root,
        primary,
        origin,
    }
}

fn succeed(dir: &Path, args: &[&str]) -> String {
    let out = common::rwv().args(args).current_dir(dir).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "`rwv {}` failed:\nstdout:\n{stdout}\nstderr:\n{}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr)
    );
    stdout
}

fn envelope(dir: &Path, args: &[&str]) -> Value {
    let stdout = succeed(dir, args);
    serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("not one JSON document ({e}):\n{stdout}"))
}

fn lines(dir: &Path, args: &[&str]) -> Vec<Value> {
    succeed(dir, args)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn is_placement_line(line: &Value) -> bool {
    line["$schema"]
        .as_str()
        .is_some_and(|url| url.ends_with("/placement-record.json"))
}

fn placed_total(placement: &Value) -> u64 {
    ["reflinked", "hardlinked", "written"]
        .iter()
        .map(|key| {
            placement["files"][key]
                .as_u64()
                .unwrap_or_else(|| panic!("no `files.{key}` count: {placement}"))
        })
        .sum()
}

fn same_checkout(reported: &Value, checkout: &Path) -> bool {
    reported
        .as_str()
        .and_then(|p| repoweave::path_spelling::resolve(Path::new(p)).ok())
        == repoweave::path_spelling::resolve(checkout).ok()
}

/// The one placement naming `checkout`, among `placements`.
fn placement_of<'a>(placements: &'a [Value], checkout: &Path, context: &str) -> &'a Value {
    let found: Vec<&Value> = placements
        .iter()
        .filter(|p| same_checkout(&p["absolute_path"], checkout))
        .collect();
    assert_eq!(
        found.len(),
        1,
        "expected one placement for {}:\n{context}",
        checkout.display()
    );
    found[0]
}

fn assert_no_outcome_carries_files(records: &[Value], context: &str) {
    for record in records.iter().filter(|r| !is_placement_line(r)) {
        assert!(
            record.get("files").is_none(),
            "an outcome record carries `files`:\n{record:#}\n{context}"
        );
    }
}

#[test]
fn sync_to_places_both_workspaces_each_under_its_own_path() {
    let weave = weave();
    let seat = weave.seat("feat");
    commit(&weave.member(), &["T1", "T2", "T3"], "target");
    succeed(&weave.primary, &["lock", "--commit"]);
    let seat_member = seat.join(MEMBER);
    identify(&seat_member);
    commit(&seat_member, &["S1"], "seat");
    succeed(&seat, &["lock", "--commit"]);

    let doc = envelope(
        &seat,
        &[
            "sync-to",
            &weave.primary.to_string_lossy(),
            "--strategy",
            "rebase",
            "--json",
        ],
    );
    let context = format!("{doc:#}");
    let placements = doc["placements"].as_array().unwrap();
    assert_eq!(placements.len(), 2, "{context}");
    let here = placement_of(placements, &seat_member, &context);
    let there = placement_of(placements, &weave.member(), &context);
    assert_eq!(here["path"], MEMBER);
    assert_eq!(there["path"], MEMBER);
    assert_eq!(
        placed_total(here),
        3,
        "the seat re-shares what replay brought in"
    );
    assert_eq!(
        placed_total(there),
        1,
        "the target re-shares what it was advanced over"
    );
    assert_no_outcome_carries_files(doc["outcomes"].as_array().unwrap(), &context);
}

#[test]
fn sync_ndjson_streams_the_end_of_op_reshare_as_its_own_line() {
    let weave = weave();
    let seat = weave.seat("feat");
    commit(&weave.member(), &["N1", "N2"], "primary");
    succeed(&weave.primary, &["lock", "--commit"]);

    let records = lines(&seat, &["sync", "primary", "--json", "-j", "2"]);
    let context = format!("{records:#?}");
    let outcome_at = records
        .iter()
        .position(|r| !is_placement_line(r) && r["path"] == MEMBER)
        .unwrap_or_else(|| panic!("no outcome line for {MEMBER}:\n{context}"));
    let placed: Vec<(usize, &Value)> = records
        .iter()
        .enumerate()
        .filter(|(_, r)| is_placement_line(r))
        .collect();
    assert_eq!(placed.len(), 1, "{context}");
    let (placed_at, placement) = placed[0];
    assert!(
        placed_at > outcome_at,
        "the re-share runs at the end of the op, after the outcome line:\n{context}"
    );
    assert_eq!(placement["path"], MEMBER);
    assert!(
        same_checkout(&placement["absolute_path"], &seat.join(MEMBER)),
        "{context}"
    );
    assert_eq!(placed_total(placement), 2, "{context}");
    assert_no_outcome_carries_files(&records, &context);
}

/// A seat made while the project held only [`MEMBER`], whose manifest and
/// lock then gained [`EXTRA`], which the primary holds a clone of.
fn seat_lacking_a_member(weave: &Weave) -> PathBuf {
    seat_named_lacking_a_member(weave, "feat")
}

fn seat_named_lacking_a_member(weave: &Weave, name: &str) -> PathBuf {
    seat_lacking_a_member_as(weave, name, "owned")
}

/// [`seat_named_lacking_a_member`], the member held in `role`.
fn seat_lacking_a_member_as(weave: &Weave, name: &str, role: &str) -> PathBuf {
    let seat = weave.seat(name);
    let extra = weave.primary.join(EXTRA);
    init_repo(&extra);
    commit(&extra, &["E1"], "extra");
    let extra_sha = common::git_in(&extra, &["rev-parse", "HEAD"]);
    let project = seat.join("projects").join(PROJECT);
    let member_url = common::file_url(&weave.origin);
    let extra_url = common::file_url(&extra);
    let member_sha = common::git_in(seat.join(MEMBER), &["rev-parse", "HEAD"]);
    std::fs::write(
        project.join("rwv.toml"),
        format!(
            "{}\n{}",
            entry(MEMBER, &member_url),
            entry_as(EXTRA, &extra_url, role)
        ),
    )
    .unwrap();
    common::fixture_lock(
        &project,
        &[
            (MEMBER, &member_url, &member_sha),
            (EXTRA, &extra_url, &extra_sha),
        ],
    );
    identify(&project);
    common::git_in(&project, &["add", "-A"]);
    common::git_in(&project, &["commit", "-q", "-m", "add extra"]);
    assert!(!seat.join(EXTRA).exists());
    seat
}

#[test]
fn sync_to_places_a_member_it_creates_in_the_target_in_both_modes() {
    for ndjson in [false, true] {
        let weave = weave();
        let target = weave.seat("target");
        let seat = seat_named_lacking_a_member(&weave, "feat");
        succeed(&seat, &["fetch", "--json", "-j", "1"]);
        assert!(seat.join(EXTRA).is_dir() && !target.join(EXTRA).exists());

        let target_arg = target.to_string_lossy();
        let (records, placements) = outcomes_and_placements(
            &seat,
            &["sync-to", &target_arg, "--json"],
            "outcomes",
            ndjson,
        );
        let context = format!("ndjson: {ndjson}\n{records:#?}\n{placements:#?}");
        assert!(target.join(EXTRA).is_dir(), "{context}");
        let placement = placement_of(&placements, &target.join(EXTRA), &context);
        assert_eq!(placement["path"], EXTRA);
        assert_eq!(placed_total(placement), 2, "README and E1\n{context}");
        assert_no_outcome_carries_files(&records, &context);
    }
}

#[test]
fn fetch_in_a_seat_places_the_worktree_it_creates_in_the_envelope() {
    let weave = weave();
    let seat = seat_lacking_a_member(&weave);

    let doc = envelope(&seat, &["fetch", "--json", "-j", "1"]);
    let context = format!("{doc:#}");
    let placements = doc["placements"].as_array().unwrap();
    assert_eq!(placements.len(), 1, "{context}");
    let placement = placement_of(placements, &seat.join(EXTRA), &context);
    assert_eq!(placement["path"], EXTRA);
    assert_eq!(placed_total(placement), 2, "README and E1");
    let outcomes = doc["outcomes"].as_array().unwrap();
    let aligned = outcomes.iter().find(|o| o["path"] == EXTRA).unwrap();
    assert!(
        same_checkout(&aligned["absolute_path"], &weave.primary.join(EXTRA)),
        "the outcome names the clone the fetch aligned, not the seat's worktree:\n{context}"
    );
    assert_no_outcome_carries_files(outcomes, &context);
}

/// A `reference` member is fetched into a seat like any other, and
/// `--no-reference` leaves it out of the seat as it leaves it out of the
/// primary.
#[test]
fn fetch_in_a_seat_creates_a_reference_member_unless_no_reference_leaves_it_out() {
    for (extra_args, created) in [(&[][..], true), (&["--no-reference"][..], false)] {
        let weave = weave();
        let seat = seat_lacking_a_member_as(&weave, "feat", "reference");
        let args: Vec<&str> = ["fetch", "--json", "-j", "1"]
            .into_iter()
            .chain(extra_args.iter().copied())
            .collect();

        let doc = envelope(&seat, &args);

        assert_eq!(
            seat.join(EXTRA).exists(),
            created,
            "args {args:?}: the reference member's worktree in the seat\n{doc:#}"
        );
    }
}

#[test]
fn fetch_in_a_seat_streams_the_worktree_it_creates_as_its_own_line() {
    let weave = weave();
    let seat = seat_lacking_a_member(&weave);

    let records = lines(&seat, &["fetch", "--json", "-j", "2"]);
    let context = format!("{records:#?}");
    let placed: Vec<&Value> = records.iter().filter(|r| is_placement_line(r)).collect();
    assert_eq!(placed.len(), 1, "{context}");
    assert_eq!(placed[0]["path"], EXTRA);
    assert!(
        same_checkout(&placed[0]["absolute_path"], &seat.join(EXTRA)),
        "{context}"
    );
    assert_eq!(placed_total(placed[0]), 2, "{context}");
    assert_no_outcome_carries_files(&records, &context);
}

/// A `--json` run's outcome records and placements: from one envelope's
/// `records_key` and `placements` arrays under `-j 1`, or from the lines
/// `-j 2` streams.
fn outcomes_and_placements(
    dir: &Path,
    args: &[&str],
    records_key: &str,
    ndjson: bool,
) -> (Vec<Value>, Vec<Value>) {
    let jobs = if ndjson { "2" } else { "1" };
    let args: Vec<&str> = args.iter().copied().chain(["-j", jobs]).collect();
    if ndjson {
        lines(dir, &args)
            .into_iter()
            .partition(|r| !is_placement_line(r))
    } else {
        let doc = envelope(dir, &args);
        (
            doc[records_key].as_array().unwrap().clone(),
            doc["placements"].as_array().unwrap().clone(),
        )
    }
}

#[test]
fn update_reports_the_advance_as_a_placement_in_both_modes() {
    for ndjson in [false, true] {
        let weave = weave();
        weave.advance_origin(&["U1", "U2"]);
        let (records, placements) =
            outcomes_and_placements(&weave.primary, &["update", "--json"], "repos", ndjson);
        let context = format!("ndjson: {ndjson}\n{records:#?}\n{placements:#?}");
        assert!(!records.is_empty(), "{context}");
        assert_eq!(placements.len(), 1, "{context}");
        let placement = placement_of(&placements, &weave.member(), &context);
        assert_eq!(placement["path"], MEMBER);
        assert_eq!(placed_total(placement), 2, "{context}");
        assert_no_outcome_carries_files(&records, &context);
    }
}

#[test]
fn fetch_reports_a_realigned_clone_as_a_placement_in_both_modes() {
    for ndjson in [false, true] {
        let weave = weave();
        let sha = weave.advance_origin(&["F1", "F2"]);
        common::git_in(weave.member(), &["fetch", "-q", "origin"]);
        let url = common::file_url(&weave.origin);
        common::fixture_lock(&weave.project(), &[(MEMBER, &url, &sha)]);
        common::git_in(weave.project(), &["commit", "-q", "-am", "lock: upstream"]);

        let (records, placements) =
            outcomes_and_placements(&weave.primary, &["fetch", "--json"], "outcomes", ndjson);
        let context = format!("ndjson: {ndjson}\n{records:#?}\n{placements:#?}");
        assert_eq!(
            common::git_in(weave.member(), &["rev-parse", "HEAD"]),
            sha,
            "fixture: fetch realigns the clone to the lock\n{context}"
        );
        assert_eq!(placements.len(), 1, "{context}");
        let placement = placement_of(&placements, &weave.member(), &context);
        assert_eq!(placement["path"], MEMBER);
        assert_eq!(placed_total(placement), 2, "{context}");
        assert_no_outcome_carries_files(&records, &context);
    }
}

fn references_file_placement(schema: &Value) -> bool {
    match schema {
        Value::Object(map) => {
            map.get("$ref")
                .and_then(Value::as_str)
                .is_some_and(|r| r.ends_with("/FilePlacement"))
                || map.values().any(references_file_placement)
        }
        Value::Array(items) => items.iter().any(references_file_placement),
        _ => false,
    }
}

/// Every object schema with a property whose value reaches `FilePlacement`.
fn objects_holding_a_placement(schema: &Value, out: &mut Vec<Value>) {
    match schema {
        Value::Object(map) => {
            if let Some(Value::Object(props)) = map.get("properties") {
                if props.values().any(references_file_placement) {
                    out.push(schema.clone());
                }
            }
            map.values()
                .for_each(|v| objects_holding_a_placement(v, out));
        }
        Value::Array(items) => items
            .iter()
            .for_each(|v| objects_holding_a_placement(v, out)),
        _ => {}
    }
}

#[test]
fn file_counts_sit_only_in_placement_records_in_every_committed_schema() {
    let mut holding = Vec::new();
    for artifact in json_schema::committed_verbs() {
        let mut found = Vec::new();
        objects_holding_a_placement(&json_schema::committed_schema(&artifact), &mut found);
        for object in &found {
            let mut props: Vec<&str> = object["properties"]
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .filter(|key| *key != "$schema")
                .collect();
            props.sort_unstable();
            assert_eq!(
                props,
                ["absolute_path", "files", "path"],
                "{} holds file counts in something other than a placement record:\n{object:#}",
                json_schema::schema_path(&artifact)
            );
        }
        if !found.is_empty() {
            holding.push(artifact);
        }
    }
    assert_eq!(
        holding,
        ["fetch", "placement-record", "sync", "sync-to", "update"],
        "every verb that places files reports placements, and the shared record is committed"
    );
}
