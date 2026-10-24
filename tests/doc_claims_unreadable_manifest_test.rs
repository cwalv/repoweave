//! A project whose `rwv.toml` does not parse: every verb that reads the
//! manifest refuses, naming the key, the table and the line, except
//! `rwv status` and `rwv prime`, which report on the whole workspace and skip
//! the project with a warning instead. `rwv workweave <project> list` reads
//! only the workweave registry, so a broken manifest does not reach it.

use std::path::{Path, PathBuf};
use std::process::Output;

mod common;

const KEY: &str = "`enabled` in `[integrations.gita]`";
const MEMBER: &str = "local/origin/alpha";

fn run(args: &[&str], dir: &Path) -> Output {
    common::rwv().args(args).current_dir(dir).output().unwrap()
}

fn text(out: &Output) -> String {
    format!(
        "exit {:?}\nstdout:\n{}\nstderr:\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn succeed(args: &[&str], dir: &Path) {
    let out = run(args, dir);
    assert!(
        out.status.success(),
        "`rwv {}`: {}",
        args.join(" "),
        text(&out)
    );
}

fn origin(tmp: &Path, name: &str) -> String {
    let bare = tmp.join("origin").join(format!("{name}.git"));
    std::fs::create_dir_all(bare.parent().unwrap()).unwrap();
    common::init_bare_repo_with_commit(&bare);
    common::file_url(&bare)
}

struct Weave {
    tmp: tempfile::TempDir,
    ws: PathBuf,
    workweave: PathBuf,
}

impl Weave {
    /// A weave whose project `app` holds `alpha`, locked and committed, with
    /// a workweave `one`; then every copy of the manifest gains an `enabled`
    /// that is not a boolean.
    fn broken() -> Self {
        let tmp = common::tempdir().unwrap();
        let ws = tmp.path().join("ws");
        std::fs::create_dir_all(ws.join("projects")).unwrap();
        succeed(&["init", "app"], &ws);
        succeed(&["add", &origin(tmp.path(), "alpha")], &ws);
        let project = ws.join("projects/app");
        common::git_in(&project, &["config", "user.email", "test@test.com"]);
        common::git_in(&project, &["config", "user.name", "Test"]);
        succeed(&["lock"], &ws);
        common::git_in(&project, &["add", "-A"]);
        common::git_in(&project, &["commit", "-qm", "alpha"]);
        succeed(&["workweave", "app", "create", "one"], &ws);
        let workweave = common::under(tmp.path(), ".workweaves/app--one");
        for root in [&ws, &workweave] {
            let manifest = root.join("projects/app/rwv.toml");
            let mut body = std::fs::read_to_string(&manifest).unwrap();
            body.push_str("\n[integrations.gita]\nenabled = \"false\"\n");
            std::fs::write(&manifest, body).unwrap();
        }
        Weave { tmp, ws, workweave }
    }
}

fn names_the_line(stream: &str) -> bool {
    stream.contains(KEY) && stream.contains("line ")
}

#[test]
fn every_verb_that_reads_the_manifest_refuses_naming_the_key_and_line() {
    let weave = Weave::broken();
    let beta = origin(weave.tmp.path(), "beta");
    let ws = weave.ws.as_path();
    let workweave_dir = weave.workweave.to_str().unwrap().to_owned();
    let primary_dir = ws.to_str().unwrap().to_owned();
    let cases: Vec<(Vec<&str>, &Path)> = vec![
        (vec!["lock"], ws),
        (vec!["fetch"], ws),
        (vec!["update"], ws),
        (vec!["materialize"], ws),
        (vec!["activate", "app"], ws),
        (vec!["add", &beta], ws),
        (vec!["push", "--dry-run"], ws),
        (vec!["role", MEMBER, "reference"], ws),
        (vec!["remove", MEMBER], ws),
        (vec!["workweave", "app", "create", "two"], ws),
        (vec!["workweave", "app", "delete", "one"], ws),
        (vec!["sync", &workweave_dir], ws),
        (vec!["sync", &primary_dir], &weave.workweave),
        (vec!["sync-to"], &weave.workweave),
    ];
    for (args, dir) in cases {
        let out = run(&args, dir);
        assert!(
            !out.status.success() && names_the_line(&String::from_utf8_lossy(&out.stderr)),
            "`rwv {}` in {} must refuse naming the key and its line: {}",
            args.join(" "),
            dir.display(),
            text(&out)
        );
    }
}

#[test]
fn status_and_prime_skip_the_project_with_a_warning_in_either_checkout() {
    let weave = Weave::broken();
    for dir in [&weave.ws, &weave.workweave] {
        for args in [&["status"][..], &["status", "--json"], &["prime"]] {
            let out = run(args, dir);
            let stderr = String::from_utf8_lossy(&out.stderr);
            assert!(
                out.status.success()
                    && stderr.contains("warning: skipping project app: manifest unreadable")
                    && names_the_line(&stderr),
                "`rwv {}` in {} must warn and exit 0: {}",
                args.join(" "),
                dir.display(),
                text(&out)
            );
        }

        let status: serde_json::Value =
            serde_json::from_slice(&run(&["status", "--json"], dir).stdout).unwrap();
        assert_eq!(
            status["unreadable_projects"][0]["project"], "app",
            "{status}"
        );
        assert!(
            names_the_line(status["unreadable_projects"][0]["error"].as_str().unwrap()),
            "{status}"
        );

        let prime = run(&["prime"], dir);
        let context = String::from_utf8_lossy(&prime.stdout);
        assert!(
            context.contains("`rwv.toml` did not load") && names_the_line(&context),
            "the context an agent reads says the repository table is missing, and why: {}",
            text(&prime)
        );
    }
}

#[test]
fn prime_reads_the_manifest_of_the_checkout_it_runs_in() {
    let weave = Weave::broken();
    let manifest = weave.ws.join("projects/app/rwv.toml");
    let body = std::fs::read_to_string(&manifest).unwrap();
    std::fs::write(
        &manifest,
        body.replace("\n[integrations.gita]\nenabled = \"false\"\n", ""),
    )
    .unwrap();

    let primary = run(&["prime"], &weave.ws);
    assert!(
        String::from_utf8_lossy(&primary.stdout).contains(MEMBER),
        "{}",
        text(&primary)
    );
    let workweave = run(&["prime"], &weave.workweave);
    let stderr = String::from_utf8_lossy(&workweave.stderr);
    assert!(
        stderr.contains(
            &common::under(&weave.workweave, "projects/app/rwv.toml")
                .display()
                .to_string()
        ) && names_the_line(&stderr),
        "{}",
        text(&workweave)
    );
}

#[test]
fn workweave_list_reads_no_manifest_and_lists_what_the_registry_holds() {
    let weave = Weave::broken();
    let out = run(&["workweave", "app", "list"], &weave.ws);
    assert!(
        out.status.success() && String::from_utf8_lossy(&out.stdout).trim() == "one",
        "{}",
        text(&out)
    );
}
