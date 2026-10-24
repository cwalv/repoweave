//! A member whose `url` in `rwv.toml` is a plain path: no clone of it can be
//! made, `rwv fetch` names the refusal on the member's own line and in its
//! `--json` record, and `rwv doctor` names the one repair that works, writing
//! the `url` as a `file://` URL. Following that repair, `rwv fetch` clones it.

use std::path::{Path, PathBuf};
use std::process::Output;

mod common;

const MEMBER: &str = "local/remotes/alpha";

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

struct Weave {
    _tmp: tempfile::TempDir,
    ws: PathBuf,
    bare: PathBuf,
}

impl Weave {
    fn with_url(url_of: impl Fn(&Path) -> String) -> Self {
        let tmp = common::tempdir().unwrap();
        let bare = tmp.path().join("remotes/alpha.git");
        std::fs::create_dir_all(bare.parent().unwrap()).unwrap();
        common::init_bare_repo_with_commit(&bare);
        let ws = tmp.path().join("ws");
        std::fs::create_dir_all(ws.join("projects/app")).unwrap();
        std::fs::write(ws.join(".rwv-active"), "app\n").unwrap();
        let weave = Weave {
            _tmp: tmp,
            ws,
            bare,
        };
        weave.write_url(&url_of(&weave.bare));
        weave
    }

    fn write_url(&self, url: &str) {
        std::fs::write(
            self.ws.join("projects/app/rwv.toml"),
            format!(
                "[repositories.\"{MEMBER}\"]\ntype = \"git\"\nurl = \"{}\"\nversion = \"main\"\nrole = \"owned\"\n",
                url.replace('\\', "\\\\")
            ),
        )
        .unwrap();
    }

    fn doctor_kinds(&self) -> Vec<String> {
        let out = run(&["doctor", "--json"], &self.ws);
        let report: serde_json::Value =
            serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{e}: {}", text(&out)));
        report["violations"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|v| v["repo"] == MEMBER || v["path"] == MEMBER)
            .map(|v| match v["sub_kind"].as_object() {
                Some(sub) => sub.keys().next().unwrap().clone(),
                None => v["kind"].as_str().unwrap().to_owned(),
            })
            .collect()
    }
}

#[test]
fn fetch_names_the_refusal_on_the_members_line_and_in_its_record() {
    let weave = Weave::with_url(|bare| bare.to_str().unwrap().to_owned());

    let out = run(&["fetch"], &weave.ws);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let member_line = stderr
        .lines()
        .position(|l| l.starts_with(&format!("rwv fetch: error: {MEMBER}:")))
        .unwrap_or_else(|| panic!("{}", text(&out)));
    assert_eq!(
        stderr.lines().nth(member_line + 1),
        Some("rwv explain unresolvable-repo-source"),
        "{}",
        text(&out)
    );
    assert!(!out.status.success());

    let out = run(&["fetch", "--json", "-j", "1"], &weave.ws);
    let envelope: serde_json::Value =
        serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{e}: {}", text(&out)));
    let record = &envelope["outcomes"][0];
    assert_eq!(record["status"], "failed", "{envelope}");
    assert_eq!(record["refusal"], "unresolvable-repo-source", "{envelope}");
}

#[test]
fn doctor_names_only_the_respelling_and_following_it_lets_fetch_clone() {
    let weave = Weave::with_url(|bare| bare.to_str().unwrap().to_owned());

    assert_eq!(weave.doctor_kinds(), ["unresolvable-url"]);
    let out = run(&["doctor"], &weave.ws);
    let report = String::from_utf8_lossy(&out.stdout);
    let file_url = common::file_url(&weave.bare);
    assert!(
        report.contains(&format!("`{file_url}`")) && !report.contains("dangling reference"),
        "{}",
        text(&out)
    );

    weave.write_url(&file_url);
    let out = run(&["fetch"], &weave.ws);
    assert!(out.status.success(), "{}", text(&out));
    assert!(weave.ws.join(MEMBER).join(".git").exists());
    assert_eq!(weave.doctor_kinds(), Vec::<String>::new());
}

#[test]
fn a_resolvable_url_with_no_clone_is_still_a_dangling_reference() {
    let weave = Weave::with_url(|bare| common::file_url(bare));
    assert_eq!(weave.doctor_kinds(), ["dangling-reference"]);
}
