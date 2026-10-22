//! After a verb moves a member, the files the move rewrote are cloned again
//! from a workspace that already holds them, and the verb says how each was
//! placed.

use std::path::{Path, PathBuf};

mod common;

use common::LineEndings;

const MEMBER: &str = "github/org/reference";
const PROJECT: &str = "app";

fn init_repo(path: &Path) -> String {
    std::fs::create_dir_all(path).unwrap();
    common::git_in(path, &["init", "-b", "main"]);
    std::fs::write(path.join("README"), "readme\n").unwrap();
    common::git_in(path, &["add", "."]);
    common::git_in(path, &["commit", "-m", "initial"]);
    common::git_in(path, &["rev-parse", "HEAD"])
}

/// A primary whose one project holds [`MEMBER`] as a reference member
/// checked out under `endings`, and a seat created from it. Returns
/// `(primary, member's canonical clone, seat)`.
fn weave_and_seat(parent: &Path, endings: LineEndings) -> (PathBuf, PathBuf, PathBuf) {
    let ws = parent.join("primary");
    let member = ws.join(MEMBER);
    let sha = init_repo(&member);
    endings.pin(&member);
    let project_dir = ws.join("projects").join(PROJECT);
    init_repo(&project_dir);
    std::fs::write(
        project_dir.join(".gitattributes"),
        "rwv.lock merge=rwv-ours\n",
    )
    .unwrap();
    std::fs::write(
        project_dir.join("rwv.toml"),
        format!(
            "[repositories.\"{MEMBER}\"]\ntype = \"git\"\nurl = \"file://{}\"\n\
             version = \"main\"\nrole = \"reference\"\n",
            common::url_path(&member)
        ),
    )
    .unwrap();
    common::fixture_lock(
        &project_dir,
        &[(MEMBER, common::file_url(&member).as_str(), sha.as_str())],
    );
    common::git_in(&project_dir, &["add", "."]);
    common::git_in(&project_dir, &["commit", "-m", "lock: initial"]);
    std::fs::write(ws.join(".rwv-active"), format!("{PROJECT}\n")).unwrap();
    common::rwv()
        .args(["workweave", PROJECT, "create", "feat"])
        .current_dir(&ws)
        .assert()
        .success();
    let seat = parent.join(".workweaves").join(format!("{PROJECT}--feat"));
    (ws, member, seat)
}

fn declared_to_clone(dir: &Path) -> Option<bool> {
    if cfg!(target_os = "macos") {
        return Some(true);
    }
    if !cfg!(target_os = "linux") {
        return None;
    }
    let out = std::process::Command::new("stat")
        .args(["-f", "-c", "%T"])
        .arg(dir)
        .output()
        .ok()?;
    match String::from_utf8_lossy(&out.stdout).trim() {
        "ext2/ext3" | "tmpfs" => Some(false),
        "btrfs" => Some(true),
        _ => None,
    }
}

fn sync_reports_how_the_files_it_moved_were_placed(endings: LineEndings) {
    let tmp = common::tempdir().unwrap();
    let (primary, member, seat) = weave_and_seat(tmp.path(), endings);
    for (file, body) in [("NEXT", "n\n"), ("README", "readme, revised\n")] {
        std::fs::write(member.join(file), body).unwrap();
    }
    common::git_in(&member, &["add", "."]);
    common::git_in(&member, &["commit", "-m", "advance"]);
    common::rwv()
        .args(["lock", "--commit"])
        .current_dir(&primary)
        .assert()
        .success();

    let output = common::rwv()
        .args(["sync", "primary"])
        .current_dir(&seat)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success(),
        "sync failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let line = stdout
        .lines()
        .find_map(|l| l.trim().strip_prefix(&format!("{MEMBER}: files ")))
        .unwrap_or_else(|| panic!("sync reported no file placement for {MEMBER}:\n{stdout}"));
    let expected = match (endings, declared_to_clone(tmp.path())) {
        (LineEndings::Converted, _) | (_, Some(false)) => {
            Some("0 reflinked, 0 hardlinked, 2 written")
        }
        (LineEndings::Verbatim, Some(true)) => Some("2 reflinked, 0 hardlinked, 0 written"),
        (LineEndings::Verbatim, None) => None,
    };
    if let Some(expected) = expected {
        assert_eq!(line, expected, "on {}", std::env::consts::OS);
    }
    let checkout = seat.join(MEMBER);
    assert_eq!(common::git_in(&checkout, &["status", "--porcelain"]), "");
    let readme = match endings {
        LineEndings::Verbatim => "readme, revised\n",
        LineEndings::Converted => "readme, revised\r\n",
    };
    assert_eq!(
        std::fs::read_to_string(checkout.join("README")).unwrap(),
        readme
    );
}

fn sync_json_reports_the_placement_as_its_own_record(endings: LineEndings) {
    let tmp = common::tempdir().unwrap();
    let (primary, member, seat) = weave_and_seat(tmp.path(), endings);
    std::fs::write(member.join("NEXT"), "n\n").unwrap();
    common::git_in(&member, &["add", "."]);
    common::git_in(&member, &["commit", "-m", "advance"]);
    common::rwv()
        .args(["lock", "--commit"])
        .current_dir(&primary)
        .assert()
        .success();

    let output = common::rwv()
        .args(["sync", "primary", "--json"])
        .current_dir(&seat)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(output.status.success(), "sync failed:\n{stdout}");

    let envelope: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let outcome = envelope["outcomes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["path"] == MEMBER)
        .unwrap_or_else(|| panic!("no outcome for {MEMBER}:\n{stdout}"));
    assert!(outcome.get("files").is_none(), "{stdout}");
    let placements = envelope["placements"].as_array().unwrap();
    assert_eq!(placements.len(), 1, "{stdout}");
    let placement = &placements[0];
    assert_eq!(placement["path"], MEMBER);
    assert_eq!(
        repoweave::path_spelling::resolve(Path::new(placement["absolute_path"].as_str().unwrap()))
            .unwrap(),
        repoweave::path_spelling::resolve(&seat.join(MEMBER)).unwrap()
    );
    let files = &placement["files"];
    let count = |key: &str| files[key].as_u64().unwrap();
    assert_eq!(count("reflinked") + count("written"), 1, "{stdout}");
    assert_eq!(count("hardlinked"), 0);
    match (endings, declared_to_clone(tmp.path())) {
        (LineEndings::Converted, _) | (_, Some(false)) => assert_eq!(count("written"), 1),
        (LineEndings::Verbatim, Some(true)) => {
            assert_eq!(count("reflinked"), 1, "on {}", std::env::consts::OS)
        }
        (LineEndings::Verbatim, None) => {}
    }
}

mod verbatim {
    use super::LineEndings;

    #[test]
    fn sync_reports_how_the_files_it_moved_were_placed() {
        super::sync_reports_how_the_files_it_moved_were_placed(LineEndings::Verbatim)
    }

    #[test]
    fn sync_json_reports_the_placement_as_its_own_record() {
        super::sync_json_reports_the_placement_as_its_own_record(LineEndings::Verbatim)
    }
}

mod converted {
    use super::LineEndings;

    #[test]
    fn sync_reports_how_the_files_it_moved_were_placed() {
        super::sync_reports_how_the_files_it_moved_were_placed(LineEndings::Converted)
    }

    #[test]
    fn sync_json_reports_the_placement_as_its_own_record() {
        super::sync_json_reports_the_placement_as_its_own_record(LineEndings::Converted)
    }
}
