//! An argument's meaning comes from how it is spelled, and a project from
//! `--project` or `.rwv-active` — never from where CWD happens to be or what a
//! path happens to resolve to. Each refusal here is the binary's stderr, read
//! for its route line.

use std::path::{Path, PathBuf};

mod common;

fn refusal_stderr(args: &[&str], cwd: &Path) -> String {
    let output = common::rwv()
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("rwv should run");
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_eq!(
        output.status.code(),
        Some(1),
        "rwv {args:?} was expected to refuse with exit 1\n{stderr}"
    );
    stderr
}

fn assert_routes_to(stderr: &str, token: &str) {
    let routed: Vec<&str> = stderr
        .lines()
        .filter_map(|l| l.strip_prefix("rwv explain "))
        .collect();
    assert_eq!(
        routed,
        vec![token],
        "expected exactly one route line naming `{token}`, stderr was:\n{stderr}"
    );
}

fn init_project(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("rwv.toml"), "[repositories]\n").unwrap();
    common::fixture_lock(dir, &[]);
    common::git_in(dir, &["init", "--initial-branch=main"]);
    common::git_in(dir, &["config", "user.email", "test@test.com"]);
    common::git_in(dir, &["config", "user.name", "Test"]);
    common::git_in(dir, &["add", "-A"]);
    common::git_in(dir, &["commit", "-m", "init"]);
}

/// A weave holding `web-app` (active) and `mobile`, both with empty manifests.
fn two_project_weave(root: &Path) -> PathBuf {
    let ws = root.join("ws");
    init_project(&ws.join("projects/web-app"));
    init_project(&ws.join("projects/mobile"));
    std::fs::write(ws.join(".rwv-active"), "web-app\n").unwrap();
    ws
}

fn project_files(ws: &Path) -> Vec<(PathBuf, String)> {
    ["web-app", "mobile"]
        .iter()
        .flat_map(|p| ["rwv.toml", "rwv.lock"].map(|f| ws.join("projects").join(p).join(f)))
        .map(|f| {
            let body = std::fs::read_to_string(&f).unwrap();
            (f, body)
        })
        .collect()
}

#[test]
fn every_writing_verb_refuses_from_another_projects_directory() {
    let tmp = common::tempdir().unwrap();
    let ws = two_project_weave(tmp.path());
    let mobile = ws.join("projects/mobile");
    let before = project_files(&ws);

    let remote = common::file_url(tmp.path().join("nowhere.git"));
    let cases: &[&[&str]] = &[
        &["add", &remote],
        &["remove", "github/acme/server"],
        &["role", "github/acme/server", "owned"],
        &["lock"],
        &["update"],
        &["push"],
        &["sync", "primary"],
        &["sync-to", "primary"],
        &["fetch"],
        &["doctor", "--fix"],
    ];
    for args in cases {
        let stderr = refusal_stderr(args, &mobile);
        assert_routes_to(&stderr, "cwd-in-inactive-project");
        assert!(
            stderr.contains("projects/mobile/")
                && stderr.contains("web-app")
                && stderr.contains("--project mobile"),
            "rwv {args:?} must name both projects and `--project mobile`:\n{stderr}"
        );
    }
    assert_eq!(
        project_files(&ws),
        before,
        "a refused verb must not have written either project's files"
    );
}

#[test]
fn doctor_without_fix_reports_from_another_projects_directory() {
    let tmp = common::tempdir().unwrap();
    let ws = two_project_weave(tmp.path());

    let output = common::rwv()
        .arg("doctor")
        .current_dir(ws.join("projects/mobile"))
        .output()
        .expect("rwv should run");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("cwd-in-inactive-project"),
        "a report changes nothing, so it does not refuse:\n{stderr}"
    );
}

#[test]
fn fetch_refuses_to_guess_among_several_projects_and_names_the_project_flag() {
    let tmp = common::tempdir().unwrap();
    let ws = two_project_weave(tmp.path());
    std::fs::remove_file(ws.join(".rwv-active")).unwrap();

    let stderr = refusal_stderr(&["fetch"], &ws);
    assert_routes_to(&stderr, "no-active-project");
    assert!(stderr.contains("--project <name>"), "{stderr}");
    assert!(
        !ws.join(".rwv-active").exists(),
        "with two projects there is nothing to select"
    );
}

#[test]
fn fetch_project_flag_with_a_source_is_inapplicable() {
    let tmp = common::tempdir().unwrap();
    let ws = two_project_weave(tmp.path());
    let source = common::file_url(tmp.path().join("web-app.git"));

    let stderr = refusal_stderr(&["fetch", "--project", "mobile", &source], &ws);
    assert_routes_to(&stderr, "inapplicable-flag");
    assert!(stderr.contains("--project"), "{stderr}");
}

#[test]
fn add_refuses_a_plain_path_to_a_bare_remote_and_names_both_spellings() {
    let tmp = common::tempdir().unwrap();
    let ws = two_project_weave(tmp.path());
    let bare = tmp.path().join("remotes/lib.git");
    std::fs::create_dir_all(bare.parent().unwrap()).unwrap();
    common::init_bare_repo_with_commit(&bare);
    let before = project_files(&ws);
    let path = bare.to_str().unwrap();

    let stderr = refusal_stderr(&["add", path], &ws);
    assert_routes_to(&stderr, "unresolvable-repo-source");
    assert!(
        stderr.contains(&format!("`rwv add {}`", common::file_url(&bare)))
            && stderr.contains(&format!("`rwv add --adopt {path}`")),
        "the refusal must name the file:// spelling and --adopt:\n{stderr}"
    );
    assert_eq!(project_files(&ws), before);
    assert!(!ws.join("local").exists(), "nothing may have been cloned");

    let advised = common::advised_command(&stderr, "rwv add file://");
    let findings_before = common::doctor_findings(&ws);
    let followed = common::run_advised(&advised, &ws);
    assert!(
        followed.status.success(),
        "`{advised}`:\n{}",
        String::from_utf8_lossy(&followed.stderr)
    );
    assert!(ws.join("local/remotes/lib/.git").exists());
    common::assert_no_new_findings(&ws, &findings_before);
}

#[test]
fn add_no_longer_adopts_an_existing_clone_named_by_its_path() {
    let tmp = common::tempdir().unwrap();
    let ws = two_project_weave(tmp.path());
    let bare = tmp.path().join("other.git");
    common::init_bare_repo_with_commit(&bare);
    let clone = ws.join("vendor/other");
    std::fs::create_dir_all(clone.parent().unwrap()).unwrap();
    common::git_in(
        tmp.path(),
        &["clone", bare.to_str().unwrap(), clone.to_str().unwrap()],
    );
    let before = project_files(&ws);

    let stderr = refusal_stderr(&["add", "vendor/other"], &ws);
    assert_routes_to(&stderr, "unresolvable-repo-source");
    assert!(
        stderr.contains("`rwv add --adopt vendor/other`"),
        "{stderr}"
    );
    assert_eq!(
        project_files(&ws),
        before,
        "a path is not registered without --adopt"
    );

    let advised = common::advised_command(&stderr, "rwv add --adopt");
    let followed = common::run_advised(&advised, &ws);
    assert!(
        followed.status.success(),
        "`{advised}`:\n{}",
        String::from_utf8_lossy(&followed.stderr)
    );
    let manifest = std::fs::read_to_string(ws.join("projects/web-app/rwv.toml")).unwrap();
    assert!(manifest.contains("vendor/other"), "{manifest}");

    let fetched = common::rwv()
        .arg("fetch")
        .current_dir(&ws)
        .output()
        .unwrap();
    assert!(
        fetched.status.success(),
        "rwv fetch after the adopt:\n{}",
        String::from_utf8_lossy(&fetched.stderr)
    );
}

#[test]
fn adopt_refuses_a_path_with_no_clone_including_a_bare_remote() {
    let tmp = common::tempdir().unwrap();
    let ws = two_project_weave(tmp.path());
    let bare = ws.join("remotes/lib.git");
    std::fs::create_dir_all(bare.parent().unwrap()).unwrap();
    common::init_bare_repo_with_commit(&bare);
    let before = project_files(&ws);

    std::fs::create_dir_all(ws.join("vendor/plain")).unwrap();
    std::fs::write(ws.join("vendor/plain/notes.txt"), "not a clone\n").unwrap();
    for path in ["vendor/missing", "remotes/lib.git", "vendor/plain"] {
        let stderr = refusal_stderr(&["add", "--adopt", path], &ws);
        assert_routes_to(&stderr, "no-clone-to-adopt");
        assert!(stderr.contains("file://"), "{stderr}");
        assert!(stderr.contains("drop --adopt"), "{stderr}");
    }
    assert_eq!(project_files(&ws), before);

    let findings_before = common::doctor_findings(&ws);
    let followed = common::run_advised(&format!("rwv add {}", common::file_url(&bare)), &ws);
    assert!(
        followed.status.success(),
        "the URL spelling the refusal names:\n{}",
        String::from_utf8_lossy(&followed.stderr)
    );
    assert!(ws.join("local/remotes/lib/.git").exists());
    common::assert_no_new_findings(&ws, &findings_before);
}

#[test]
fn adopt_and_new_are_exclusive() {
    let tmp = common::tempdir().unwrap();
    let ws = two_project_weave(tmp.path());
    common::rwv()
        .args(["add", "--adopt", "--new", "github/acme/fresh"])
        .current_dir(&ws)
        .assert()
        .code(2);
}

/// `--param` and `--params-json` only have an effect under `--new`
/// (dispatch.rs passes creation params on the `new` branch only); without it
/// they were silently ignored. Explicit over guessed: clap refuses rather
/// than accept a flag that does nothing.
#[test]
fn param_and_params_json_require_new() {
    let tmp = common::tempdir().unwrap();
    let ws = two_project_weave(tmp.path());
    common::rwv()
        .args([
            "add",
            "https://github.com/acme/fresh.git",
            "--param",
            "root=/tmp",
        ])
        .current_dir(&ws)
        .assert()
        .code(2)
        .stderr(predicates::str::contains("--new"));
    common::rwv()
        .args([
            "add",
            "https://github.com/acme/fresh.git",
            "--params-json",
            "{\"root\": \"/tmp\"}",
        ])
        .current_dir(&ws)
        .assert()
        .code(2)
        .stderr(predicates::str::contains("--new"));
}

#[test]
fn fetch_refuses_a_plain_path_source_and_names_both_spellings() {
    let tmp = common::tempdir().unwrap();
    let target = tmp.path().join("fresh");
    std::fs::create_dir_all(&target).unwrap();
    let seed = tmp.path().join("seed");
    init_project(&seed);
    let bare = tmp.path().join("web-app.git");
    common::git_in(
        tmp.path(),
        &[
            "clone",
            "--bare",
            &seed.to_string_lossy(),
            &bare.to_string_lossy(),
        ],
    );
    let path = bare.to_str().unwrap();

    let stderr = refusal_stderr(&["fetch", path], &target);
    assert_routes_to(&stderr, "unresolvable-repo-source");
    assert!(
        stderr.contains(&format!("`rwv fetch {}`", common::file_url(&bare)))
            && stderr.contains("`rwv fetch --project <name>`"),
        "the refusal must name the file:// spelling and the in-place spelling:\n{stderr}"
    );
    assert!(
        !target.join("projects").exists(),
        "nothing may have been cloned"
    );

    let advised = common::advised_command(&stderr, "rwv fetch file://");
    let followed = common::run_advised(&advised, &target);
    assert!(
        followed.status.success(),
        "`{advised}`:\n{}",
        String::from_utf8_lossy(&followed.stderr)
    );
    assert!(
        target.join("projects").is_dir(),
        "the fetch cloned the project"
    );
    let findings = common::doctor_findings(&target);
    assert!(
        findings
            .iter()
            .all(|f| f.starts_with("missing-replay-exclusion")
                || f.starts_with("missing-merge-driver-config")),
        "{findings:?}"
    );
}

/// A manifest `url` that is a plain path names no remote to clone from; the
/// refusal on `rwv fetch` names the `file://` spelling to write instead.
#[test]
fn fetch_of_a_manifest_url_that_is_a_plain_path_clears_once_it_is_spelled_file() {
    let tmp = common::tempdir().unwrap();
    let ws = two_project_weave(tmp.path());
    let bare = tmp.path().join("remotes/lib.git");
    std::fs::create_dir_all(bare.parent().unwrap()).unwrap();
    common::init_bare_repo_with_commit(&bare);
    let manifest = ws.join("projects/web-app/rwv.toml");
    std::fs::write(
        &manifest,
        format!(
            "[repositories.\"local/remotes/lib\"]\ntype = \"git\"\nurl = \"{}\"\nversion = \"main\"\nrole = \"owned\"\n",
            bare.display()
        ),
    )
    .unwrap();

    let findings_before = common::doctor_findings(&ws);
    let out = common::rwv()
        .arg("fetch")
        .current_dir(&ws)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{stderr}");
    assert!(!ws.join("local/remotes/lib").exists());

    let spelled = stderr
        .split_whitespace()
        .find(|w| w.starts_with("file://"))
        .map(|w| w.trim_end_matches([')', '`', ',', '.']))
        .expect("the refusal names the file:// spelling");
    assert_eq!(spelled, common::file_url(&bare));
    let text = std::fs::read_to_string(&manifest).unwrap();
    std::fs::write(
        &manifest,
        text.replace(
            &format!("url = \"{}\"", bare.display()),
            &format!("url = \"{spelled}\""),
        ),
    )
    .unwrap();

    let followed = common::rwv()
        .arg("fetch")
        .current_dir(&ws)
        .output()
        .unwrap();
    assert!(
        followed.status.success(),
        "{}",
        String::from_utf8_lossy(&followed.stderr)
    );
    assert!(ws.join("local/remotes/lib/.git").exists());
    common::assert_no_new_findings(&ws, &findings_before);
}

/// Two bare remotes whose URLs place at the same path, `local/remotes/lib`.
fn two_remotes_one_placement(root: &Path) -> (PathBuf, PathBuf) {
    let ours = root.join("ours/remotes/lib.git");
    let theirs = root.join("theirs/remotes/lib.git");
    for bare in [&ours, &theirs] {
        std::fs::create_dir_all(bare.parent().unwrap()).unwrap();
        common::init_bare_repo_with_commit(bare);
    }
    (ours, theirs)
}

#[test]
fn add_refuses_a_clone_of_another_remote_already_at_the_placement() {
    let tmp = common::tempdir().unwrap();
    let ws = two_project_weave(tmp.path());
    let (ours, theirs) = two_remotes_one_placement(tmp.path());
    let placed = ws.join("local/remotes/lib");
    std::fs::create_dir_all(placed.parent().unwrap()).unwrap();
    common::git_in(
        tmp.path(),
        &["clone", theirs.to_str().unwrap(), placed.to_str().unwrap()],
    );
    let before = project_files(&ws);

    let stderr = refusal_stderr(&["add", &common::file_url(&ours)], &ws);
    assert_routes_to(&stderr, "foreign-clone-at-placement");
    assert!(
        stderr.contains("`rwv add --adopt local/remotes/lib`"),
        "the refusal must name --adopt as the way to keep the clone:\n{stderr}"
    );
    assert_eq!(
        project_files(&ws),
        before,
        "a clone of another remote is not registered under this URL"
    );
    assert!(
        placed.join(".git").exists(),
        "the existing clone is untouched"
    );
}

#[test]
fn add_registers_a_clone_of_the_same_remote_already_at_the_placement() {
    let tmp = common::tempdir().unwrap();
    let ws = two_project_weave(tmp.path());
    let (ours, _theirs) = two_remotes_one_placement(tmp.path());
    let placed = ws.join("local/remotes/lib");
    std::fs::create_dir_all(placed.parent().unwrap()).unwrap();
    common::git_in(
        tmp.path(),
        &["clone", ours.to_str().unwrap(), placed.to_str().unwrap()],
    );
    let url = common::file_url(&ours);

    let out = common::rwv()
        .args(["add", &url])
        .current_dir(&ws)
        .assert()
        .success();
    let stderr = String::from_utf8_lossy(&out.get_output().stderr).into_owned();
    assert!(stderr.contains("skipping clone"), "{stderr}");
    let manifest = std::fs::read_to_string(ws.join("projects/web-app/rwv.toml")).unwrap();
    assert!(
        manifest.contains("local/remotes/lib") && manifest.contains(&url),
        "a clone whose origin is the same repo, spelled as a bare path, is registered:\n{manifest}"
    );
}

/// Whatever sits at the placement without being a clone — a directory with
/// content, an empty one, a file — has no remote to compare, so the add
/// refuses, names the placement, and leaves it as it was.
#[test]
fn add_refuses_anything_but_a_clone_at_the_placement() {
    let tmp = common::tempdir().unwrap();
    let ws = two_project_weave(tmp.path());
    let (ours, _theirs) = two_remotes_one_placement(tmp.path());
    let placed = ws.join("local/remotes/lib");
    std::fs::create_dir_all(placed.parent().unwrap()).unwrap();
    let before = project_files(&ws);

    type Occupy = fn(&Path);
    let occupants: [(&str, Occupy); 3] = [
        ("directory", |p| {
            std::fs::create_dir_all(p).unwrap();
            std::fs::write(p.join("notes.txt"), "not a clone\n").unwrap();
        }),
        ("directory", |p| std::fs::create_dir_all(p).unwrap()),
        ("file", |p| std::fs::write(p, "not a clone\n").unwrap()),
    ];
    for (shape, occupy) in occupants {
        occupy(&placed);
        let stderr = refusal_stderr(&["add", &common::file_url(&ours)], &ws);
        assert_routes_to(&stderr, "non-clone-at-placement");
        assert!(
            stderr.contains("local/remotes/lib") && stderr.contains(shape),
            "{stderr}"
        );
        assert!(!stderr.contains("skipping clone"), "{stderr}");
        assert_eq!(project_files(&ws), before);

        let retry = common::advised_command(&stderr, "rwv add");
        std::fs::rename(&placed, placed.with_extension("aside")).unwrap();
        let findings_before = common::doctor_findings(&ws);
        let followed = common::run_advised(&retry, &ws);
        assert!(
            followed.status.success(),
            "`{retry}` after moving the {shape} aside:\n{}",
            String::from_utf8_lossy(&followed.stderr)
        );
        assert!(placed.join(".git").exists(), "the retry cloned into place");
        common::assert_no_new_findings(&ws, &findings_before);

        std::fs::remove_dir_all(&placed).unwrap();
        let aside = placed.with_extension("aside");
        if aside.is_dir() {
            std::fs::remove_dir_all(aside).unwrap();
        } else {
            std::fs::remove_file(aside).unwrap();
        }
        for (file, body) in &before {
            std::fs::write(file, body).unwrap();
        }
    }
}

/// A member the manifest already names, whose placement holds a directory
/// that is not a clone, refuses the same way rather than re-running its
/// activation over it.
#[test]
fn re_add_of_a_listed_member_refuses_a_non_clone_at_its_placement() {
    let tmp = common::tempdir().unwrap();
    let ws = two_project_weave(tmp.path());
    let (ours, _theirs) = two_remotes_one_placement(tmp.path());
    let url = common::file_url(&ours);
    common::rwv()
        .args(["add", &url])
        .current_dir(&ws)
        .assert()
        .success();
    let placed = ws.join("local/remotes/lib");
    std::fs::remove_dir_all(&placed).unwrap();
    std::fs::create_dir_all(&placed).unwrap();
    std::fs::write(placed.join("notes.txt"), "not a clone\n").unwrap();
    let before = project_files(&ws);

    let stderr = refusal_stderr(&["add", &url], &ws);
    assert_routes_to(&stderr, "non-clone-at-placement");
    assert!(stderr.contains("local/remotes/lib"), "{stderr}");
    assert_eq!(project_files(&ws), before);
    assert!(placed.join("notes.txt").is_file());
}
