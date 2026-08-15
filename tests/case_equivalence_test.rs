//! The filesystem is consulted about its own case equivalence; rwv models
//! none of it.
//!
//! Three mechanisms, and one prohibition running under all of them.
//!
//!   1. **Mint onto disk.** A directory whose final component is an identity
//!      is created with `create_dir`, never `create_dir_all` — `mkdir -p`
//!      reports success for a directory that already exists, which on a
//!      folding filesystem silently adopts one the operator spelled another
//!      way. The refusal names the occupant as the parent directory lists it.
//!   2. **Steady state.** A directory whose spelling has drifted from the
//!      record is reported by the existing misnamed-dir finding, byte-wise
//!      against the parent listing.
//!   3. **Confusable-sibling lint.** Two recorded siblings differing only by
//!      ASCII case warn, at doctor and at mint, on every host — including the
//!      case-sensitive ones where both are legal and nothing else notices.
//!
//! The prohibition: **no fold decides an identity.** The lint folds to
//! compare and throws the fold away; nothing stores it, nothing resolves
//! through it, and two names differing by case remain two identities.
//!
//! Mechanisms 1 and 3 answer differently depending on whether the filesystem
//! under the fixture folds case, so the tests that reach them ask it —
//! [`filesystem_folds_case`] creates a directory and its case twin and reads
//! what the second create says — and assert the arm that answer names. A
//! `cfg(target_os)` would model the filesystem instead of consulting it, and
//! would be wrong on both of the hosts it looks certain about: APFS ships
//! case-insensitive but formats case-sensitive on request, and ext4 folds in
//! any directory carrying `casefold`.
//!
//! WHAT A CASE-SENSITIVE HOST CANNOT REACH, stated because it changes how to
//! read every test below. Where nothing folds, a collision always means the
//! requested spelling exists as an entry, so the occupant's listed name always
//! equals the requested one. The folding arms — the refusal at the second
//! `create_dir`, and the occupant named by filesystem identity in a spelling
//! the operator never asked for — execute only where the filesystem folds; on
//! a case-sensitive host only the mechanism that would find the divergence is
//! pinned, not its result.

use assert_cmd::Command;
use repoweave::manifest::{Manifest, ProjectName, RepoPath, WorkweaveName};
use repoweave::path_spelling::operator_path;
use repoweave::workspace::{confusable_siblings, describe_existing, diverged_occupant};
use std::path::{Path, PathBuf};
use std::process;

mod common;

use common::src_scan::production_lines;

fn rwv() -> Command {
    common::rwv()
}

fn doctor_output(ws: &Path) -> String {
    let out = rwv().args(["doctor"]).current_dir(ws).output().unwrap();
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// Whether `dir` holds one entry for two spellings that differ only by ASCII
/// case, asked of `dir` itself.
///
/// The twin is derived from the one base name rather than written twice, so no
/// edit can leave a pair that is not a pair and answer "does not fold" for
/// that reason. `dir` is left as it was found.
fn filesystem_folds_case(dir: &Path) -> bool {
    const PROBE: &str = "rwv-case-probe";
    let one = dir.join(PROBE);
    let twin = dir.join(PROBE.to_ascii_uppercase());

    std::fs::create_dir(&one).expect("the probe must be creatable");
    let twin_created = std::fs::create_dir(&twin);
    let folds = match &twin_created {
        Ok(()) => false,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => true,
        Err(e) => panic!("the probe must be answered, not refused for another reason: {e}"),
    };

    std::fs::remove_dir(&one).unwrap();
    if twin_created.is_ok() {
        std::fs::remove_dir(&twin).unwrap();
    }
    folds
}

/// The probe decides which arm every test below runs, so its answer is checked
/// against a second reading of the same directory: a filesystem that refused
/// the twin lists one entry afterwards, and one that took it lists two. A
/// probe that answered without asking, or that left its own directories
/// behind, disagrees with the count.
#[test]
fn the_case_probe_agrees_with_what_the_directory_lists() {
    let tmp = common::tempdir().unwrap();
    let dir = tmp.path().join("probe-parent");
    std::fs::create_dir(&dir).unwrap();

    let folds = filesystem_folds_case(&dir);

    std::fs::create_dir(dir.join("twin")).unwrap();
    let twin_taken = std::fs::create_dir(dir.join("TWIN")).is_ok();
    assert_eq!(
        folds, !twin_taken,
        "the probe must report what a create of an independent twin reports"
    );
    assert_eq!(
        std::fs::read_dir(&dir).unwrap().count(),
        if folds { 1 } else { 2 },
        "and the listing must hold those entries and nothing the probe left"
    );
}

// ---------------------------------------------------------------------------
// The prohibition: a fold compares, and decides nothing
// ---------------------------------------------------------------------------

/// Two identities differing only by case are two identities. This is the claim
/// every mechanism below is built not to violate: introduce a fold into any of
/// these comparisons and the lint stops being a lint and becomes an
/// equivalence.
#[test]
fn identity_comparison_stays_byte_exact() {
    let upper = ProjectName::new("Chatly").expect("both spellings must be mintable");
    let lower = ProjectName::new("chatly").expect("both spellings must be mintable");
    assert_ne!(upper, lower, "project names must not fold");

    let ww_upper = WorkweaveName::new("Feat").unwrap();
    let ww_lower = WorkweaveName::new("feat").unwrap();
    assert_ne!(ww_upper, ww_lower, "workweave names must not fold");

    let repo_upper = RepoPath::new("github/acme/Server").unwrap();
    let repo_lower = RepoPath::new("github/acme/server").unwrap();
    assert_ne!(repo_upper, repo_lower, "repo paths must not fold");
}

/// The lint reports the spellings the record holds, never the folded key it
/// grouped them by. Nothing downstream can store or resolve through a fold it
/// is never handed.
#[test]
fn the_lint_reports_recorded_spellings_and_not_the_fold() {
    let names = vec!["Chatly".to_owned(), "chatly".to_owned()];
    let found = confusable_siblings("projects", &names);
    assert_eq!(found.len(), 1, "the pair must be found: {found:?}");
    assert_eq!(found[0].first, "Chatly");
    assert_eq!(found[0].second, "chatly");
    assert_ne!(
        found[0].first, found[0].second,
        "reporting the fold twice would be reporting a name nothing recorded"
    );
}

/// Non-vacuity, and the negative arm: names that are not case twins are not
/// reported, so the check above is not passing because everything is.
#[test]
fn the_lint_does_not_report_names_that_merely_resemble_each_other() {
    let names = vec![
        "chatly".to_owned(),
        "chatly-web".to_owned(),
        "web-app".to_owned(),
    ];
    assert!(
        confusable_siblings("projects", &names).is_empty(),
        "distinct names must not be reported"
    );
    assert!(
        confusable_siblings("projects", &[]).is_empty(),
        "an empty namespace has no pairs"
    );
    // Three spellings of one fold are three pairs, not one.
    let three = vec!["a".to_owned(), "A".to_owned(), "a".to_owned()];
    assert_eq!(
        confusable_siblings("projects", &three).len(),
        1,
        "a repeated spelling is one identity, not two"
    );
}

// ---------------------------------------------------------------------------
// Mechanism 1: minting onto disk
// ---------------------------------------------------------------------------

fn make_weave(tmp: &Path) -> PathBuf {
    let ws = tmp.join("ws");
    std::fs::create_dir_all(&ws).unwrap();
    ws
}

/// Minting a project onto a path that is already occupied refuses, and the
/// refusal names what is there.
///
/// `create_dir_all` at this component reports success instead, adopting the
/// existing directory — which is the whole defect on a folding filesystem.
#[test]
fn minting_a_project_over_an_occupied_path_refuses() {
    let tmp = common::tempdir().unwrap();
    let ws = make_weave(tmp.path());

    rwv()
        .args(["init", "chatly"])
        .current_dir(&ws)
        .assert()
        .success();

    let out = rwv()
        .args(["init", "chatly"])
        .current_dir(&ws)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        !out.status.success(),
        "the second mint must refuse: {stderr}"
    );
    assert!(
        stderr.contains("already exists"),
        "the refusal must say what happened: {stderr}"
    );
    assert!(
        stderr.contains(&operator_path(&ws.join("projects").join("chatly"))),
        "the refusal must name the occupied path: {stderr}"
    );
}

/// The same at the workweave mint, which has its own idempotent-reuse path and
/// so must be checked separately rather than assumed to follow.
#[test]
fn minting_a_workweave_over_a_foreign_directory_refuses() {
    let tmp = common::tempdir().unwrap();
    let ws = make_weave(tmp.path());
    rwv()
        .args(["init", "web-app"])
        .current_dir(&ws)
        .assert()
        .success();
    let project_dir = ws.join("projects/web-app");
    git(&["init", "--initial-branch=main"], &project_dir);
    git(&["config", "user.email", "t@t"], &project_dir);
    git(&["config", "user.name", "T"], &project_dir);
    git(&["add", "-A"], &project_dir);
    git(&["commit", "-m", "initial"], &project_dir);

    // A directory already sitting where the workweave would be minted, with
    // no marker: not this workweave, and not rwv's to adopt.
    let container = tmp.path().join(".workweaves");
    let squatter = container.join("web-app--feat");
    std::fs::create_dir_all(squatter.join("some-work")).unwrap();

    let out = rwv()
        .args(["workweave", "web-app", "create", "feat"])
        .current_dir(&ws)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        !out.status.success(),
        "creating over an occupied path must refuse: {stderr}"
    );
    assert!(
        squatter.join("some-work").exists(),
        "the refusal must leave what was there alone"
    );
}

/// The occupant is named from the parent's listing, not from `canonicalize` —
/// which on a folding filesystem echoes back the spelling it was asked with
/// and so can never report the divergence.
///
/// A source pin because the divergence is unreachable on this host: with no
/// folding, the listed name always equals the requested one, so no fixture can
/// tell the two mechanisms apart by their output.
#[test]
fn occupant_naming_reads_the_listing_and_not_a_canonicalized_echo() {
    let lines = production_lines();
    let start = lines
        .iter()
        .position(|l| l.file == "workspace.rs" && l.text.contains("fn listed_occupant("))
        .expect("the occupant lookup must exist");
    let mut body = Vec::new();
    for line in &lines[start..] {
        body.push(line.text.clone());
        if line.text == "}" && body.len() > 1 {
            break;
        }
    }
    let body = body.join("\n");
    assert!(
        body.ends_with('}') && body.lines().count() >= 3,
        "the slicer must yield a whole body: {body}"
    );
    assert!(
        body.contains("read_dir("),
        "the occupant must come from the parent's listing: {body}"
    );
    assert!(
        !body.contains("canonicalize"),
        "canonicalize echoes the queried spelling on a folding filesystem: {body}"
    );
}

/// With the spellings in agreement there is no divergence to report — the
/// ordinary case, and the one this host can actually produce.
#[test]
fn an_agreeing_spelling_is_not_reported_as_divergence() {
    let tmp = common::tempdir().unwrap();
    let dir = tmp.path().join("chatly");
    std::fs::create_dir(&dir).unwrap();
    assert_eq!(
        diverged_occupant(&dir),
        None,
        "the name on disk is the name that was asked for"
    );
    assert!(
        describe_existing(&dir).contains("already exists"),
        "the sentence still reports the collision"
    );
    assert_eq!(
        diverged_occupant(&tmp.path().join("absent")),
        None,
        "nothing there is not a divergence"
    );
}

// ---------------------------------------------------------------------------
// Mechanism 3: the lint, at mint and at doctor
// ---------------------------------------------------------------------------

/// Where the filesystem distinguishes the two spellings, mint WARNS and does
/// not refuse. That was an operator fork and warn is what ships: a warning can
/// tighten into a refusal later, while a refusal shipped wrongly needs an
/// escape hatch. Turn the warning into a `bail!` and this fails on the exit
/// status.
///
/// Where the filesystem folds them there is no pair to lint and never will be:
/// the create that asks the question is refused by the filesystem itself,
/// before the lint runs, and the refusal names the occupant in the spelling
/// the parent lists rather than the one the operator typed.
#[test]
fn a_confusable_sibling_warns_at_mint_and_is_still_created() {
    let tmp = common::tempdir().unwrap();
    let ws = make_weave(tmp.path());

    rwv()
        .args(["init", "chatly"])
        .current_dir(&ws)
        .assert()
        .success();
    let folds = filesystem_folds_case(&ws.join("projects"));

    let out = rwv()
        .args(["init", "Chatly"])
        .current_dir(&ws)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();

    if folds {
        assert!(
            !out.status.success(),
            "one entry cannot answer to two spellings, so the mint must refuse: {stderr}"
        );
        assert!(
            stderr.contains(&operator_path(&ws.join("projects").join("Chatly"))),
            "the refusal must name the path that was asked for: {stderr}"
        );
        assert!(
            stderr.contains("lists it as `chatly`"),
            "and must name the occupant as the parent lists it, not as it was \
             requested — the whole reason the lookup is not a canonicalize: {stderr}"
        );
        assert!(
            !stderr.contains("differ only by ASCII case"),
            "the create is the consult and it refused; the lint never ran: {stderr}"
        );
        assert!(
            ws.join("projects/chatly").is_dir(),
            "the refusal must leave the occupant where it was"
        );
    } else {
        assert!(
            out.status.success(),
            "the mint must succeed — this is a warning, not a refusal: {stderr}"
        );
        assert!(
            ws.join("projects/Chatly").is_dir(),
            "the project must exist afterwards"
        );
        assert!(
            ws.join("projects/chatly").is_dir(),
            "and so must the one it resembles"
        );
        assert!(
            stderr.contains("differ only by ASCII case"),
            "the pair must be reported at mint: {stderr}"
        );
    }
}

/// And at doctor. Where the filesystem distinguishes the two spellings both
/// names are legal and nothing is broken — which is exactly when saying so is
/// useful.
///
/// Where it folds them the pair cannot reach `projects/` at all, so the
/// namespace the lint still reaches is the record: repository paths sharing
/// one parent, which the manifest holds and no filesystem gets a vote on. The
/// two doctor runs on that arm are one seeded pair — silence while the record
/// holds no twins, the finding once it does.
#[test]
fn a_confusable_sibling_is_reported_by_doctor() {
    let tmp = common::tempdir().unwrap();
    let ws = make_weave(tmp.path());
    rwv()
        .args(["init", "chatly"])
        .current_dir(&ws)
        .assert()
        .success();
    let folds = filesystem_folds_case(&ws.join("projects"));

    if !folds {
        rwv()
            .args(["init", "Chatly"])
            .current_dir(&ws)
            .assert()
            .success();
        let combined = doctor_output(&ws);
        assert!(
            combined.contains("`Chatly` and `chatly`"),
            "doctor must name both spellings: {combined}"
        );
        assert!(
            combined.contains("differ only by ASCII case"),
            "doctor must say what the relation is: {combined}"
        );
        return;
    }

    let out = rwv()
        .args(["init", "Chatly"])
        .current_dir(&ws)
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "the pair cannot reach the disk here: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let unseeded = doctor_output(&ws);
    assert!(
        !unseeded.contains("differ only by ASCII case"),
        "the filesystem refused the pair, so there is none to report: {unseeded}"
    );

    std::fs::write(
        ws.join("projects/chatly").join(Manifest::FILE_NAME),
        r#"[repositories."github/acme/Server"]
type = "git"
url = "https://example.com/acme/Server.git"
version = "main"
role = "owned"

[repositories."github/acme/server"]
type = "git"
url = "https://example.com/acme/server.git"
version = "main"
role = "owned"
"#,
    )
    .unwrap();
    let seeded = doctor_output(&ws);
    assert!(
        seeded.contains("`Server` and `server`"),
        "doctor must name both spellings: {seeded}"
    );
    assert!(
        seeded.contains("differ only by ASCII case"),
        "doctor must say what the relation is: {seeded}"
    );
}

/// A weave with no confusable pair produces no finding — the check reports
/// what it finds rather than firing on every workspace.
#[test]
fn distinct_project_names_produce_no_finding() {
    let tmp = common::tempdir().unwrap();
    let ws = make_weave(tmp.path());
    rwv()
        .args(["init", "chatly"])
        .current_dir(&ws)
        .assert()
        .success();
    rwv()
        .args(["init", "web-app"])
        .current_dir(&ws)
        .assert()
        .success();

    let combined = doctor_output(&ws);
    assert!(
        !combined.contains("differ only by ASCII case"),
        "no pair exists here: {combined}"
    );
}

fn git(args: &[&str], dir: &Path) {
    let status = common::git()
        .args(args)
        .current_dir(dir)
        .stdout(process::Stdio::null())
        .stderr(process::Stdio::null())
        .status()
        .expect("git should be available");
    assert!(status.success(), "git {args:?} in {} failed", dir.display());
}

// ---------------------------------------------------------------------------
// Mechanism 2: case drift in steady state
// ---------------------------------------------------------------------------

/// A workweave directory whose spelling drifted from the record — differing
/// only by case — is reported through the existing misnamed-dir finding.
///
/// The comparison is byte-wise between the record and the parent's LISTING.
/// That is why this is reachable on a case-sensitive host at all: the drifted
/// spelling is a genuinely different entry here, and byte-wise comparison sees
/// it for the same reason it would see a fold-induced one.
#[test]
fn case_drift_between_the_record_and_the_disk_is_reported() {
    let tmp = common::tempdir().unwrap();
    let ws = make_weave(tmp.path());
    rwv()
        .args(["init", "web-app"])
        .current_dir(&ws)
        .assert()
        .success();
    let project_dir = ws.join("projects/web-app");
    git(&["init", "--initial-branch=main"], &project_dir);
    git(&["config", "user.email", "t@t"], &project_dir);
    git(&["config", "user.name", "T"], &project_dir);
    git(&["add", "-A"], &project_dir);
    git(&["commit", "-m", "initial"], &project_dir);

    rwv()
        .args(["workweave", "web-app", "create", "feat"])
        .current_dir(&ws)
        .assert()
        .success();

    // Drift the directory's spelling by case alone, and re-point the recorded
    // path at it — what an out-of-band rename plus a re-adopt would leave, and
    // what a folding filesystem produces without anyone renaming anything.
    let container = tmp.path().join(".workweaves");
    let recorded_spelling = container.join("web-app--feat");
    let drifted = container.join("web-app--Feat");
    std::fs::rename(&recorded_spelling, &drifted).unwrap();

    let index_path = ws.join("projects/web-app/.rwv-workweave-index");
    let mut index: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&index_path).unwrap()).unwrap();
    index["workweaves"]["feat"] = serde_json::json!(drifted.canonicalize().unwrap());
    std::fs::write(&index_path, serde_json::to_string(&index).unwrap()).unwrap();

    let combined = doctor_output(&ws);
    assert!(
        combined.contains("disagrees with its records"),
        "the drift must reach the misnamed-dir finding: {combined}"
    );
    assert!(
        combined.contains("web-app--feat"),
        "the finding must name the spelling the records expect: {combined}"
    );
}
