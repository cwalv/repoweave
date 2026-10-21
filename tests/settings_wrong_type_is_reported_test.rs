//! A value of the wrong type for a setting an integration reads is reported as
//! `malformed-settings`, for every setting of every integration, and is never
//! read as the setting's default.
//!
//! The regression pinned here: `go-version = 122` on `go-work` was swallowed
//! where the hook read its settings, so the go directive was seeded from the
//! members as if the key were absent, with no finding. Its three siblings
//! report the same mistake.
//!
//! Each integration is enabled in its fixture, since one that defaults off is
//! not run, and not read, until it is.
//!
//! The population is derived: every built-in integration, and every key its
//! `settings_keys` declares, so a setting added later is held to the same
//! route without anyone remembering to list it. An integer is the probe value
//! because no setting reads one: they are strings, booleans, lists, tables, or
//! a boolean-or-name.
//!
//! Driven through `rwv doctor --json`, where each finding is one entry on the
//! integration channel carrying its integration and the deserializer's account
//! of the field. The second test holds the same population to the refusal
//! `rwv materialize` and the install hooks take from `check()`: an integration
//! that reports the finding only from other hooks is not in it. The third
//! drives a go-work whose settings do not parse, which authors nothing.

use std::path::{Path, PathBuf};

use repoweave::integrations::builtin_integrations;

mod common;

fn git_init_with_commit(dir: &Path) {
    common::git_in(dir, &["init", "--initial-branch=main"]);
    common::git_in(dir, &["config", "user.email", "test@test.com"]);
    common::git_in(dir, &["config", "user.name", "Test"]);
    common::git_in(dir, &["add", "-A"]);
    common::git_in(dir, &["commit", "-m", "init"]);
}

fn rwv(args: &[&str], cwd: &Path) -> (bool, String, String) {
    let output = common::rwv()
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("rwv should run");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

fn weave_with_setting(root: &Path, integration: &str, key: &str) -> PathBuf {
    let ws = root.join("ws");
    let project_dir = ws.join("projects/app");
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(
        project_dir.join("rwv.toml"),
        format!("[repositories]\n\n[integrations.{integration}]\nenabled = true\n{key} = 7\n"),
    )
    .unwrap();
    std::fs::write(ws.join(".rwv-active"), "app\n").unwrap();
    ws
}

#[test]
fn a_setting_of_the_wrong_type_is_reported_for_every_key_of_every_integration() {
    let builtin = builtin_integrations();
    let mut probed = 0;
    let mut failures = Vec::new();

    for integration in &builtin {
        for key in integration.settings_keys() {
            probed += 1;
            let tmp = common::tempdir().unwrap();
            let ws = weave_with_setting(tmp.path(), integration.name(), key);

            let (_, stdout, stderr) = rwv(&["doctor", "--json"], &ws);
            let report: serde_json::Value = serde_json::from_str(&stdout)
                .unwrap_or_else(|e| panic!("doctor --json must parse ({e}):\n{stdout}\n{stderr}"));
            let malformed: Vec<&serde_json::Value> = report["issues"]
                .as_array()
                .expect("report carries issues")
                .iter()
                .filter(|i| {
                    i["kind"] == "malformed-settings" && i["integration"] == integration.name()
                })
                .collect();

            match malformed.as_slice() {
                [only] if only["message"].as_str().is_some_and(|m| m.contains(key)) => {}
                [only] => failures.push(format!(
                    "{} `{key} = 7`: the finding does not name the key: {}",
                    integration.name(),
                    only["message"]
                )),
                other => failures.push(format!(
                    "{} `{key} = 7`: expected one malformed-settings finding, got {}",
                    integration.name(),
                    other.len()
                )),
            }
        }
    }

    assert!(
        probed
            >= builtin
                .iter()
                .filter(|i| !i.settings_keys().is_empty())
                .count(),
        "the probe must reach every integration that reads settings"
    );
    assert!(
        failures.is_empty(),
        "a wrong-typed setting must be reported, and name its key:\n{}",
        failures.join("\n")
    );
}

#[test]
fn materialize_refuses_over_a_setting_of_the_wrong_type_for_every_key_of_every_integration() {
    let builtin = builtin_integrations();
    let mut failures = Vec::new();

    for integration in &builtin {
        for key in integration.settings_keys() {
            let tmp = common::tempdir().unwrap();
            let ws = weave_with_setting(tmp.path(), integration.name(), key);

            let (ok, stdout, stderr) = rwv(&["materialize"], &ws);
            let out = format!("{stdout}\n{stderr}");

            let refused_naming_both = !ok
                && out.contains("nothing here was changed")
                && out.contains(&format!("{}: ", integration.name()))
                && out.contains(&format!("`{key}`"));
            if !refused_naming_both {
                failures.push(format!(
                    "{} `{key} = 7`: materialize must refuse, change nothing and name both; \
                     ok={ok}:\n{out}",
                    integration.name()
                ));
            }
        }
    }

    assert!(
        failures.is_empty(),
        "an integration whose settings do not parse is in the population materialize \
         and the install hooks refuse over:\n{}",
        failures.join("\n")
    );
}

#[test]
fn a_go_work_whose_settings_do_not_parse_authors_nothing() {
    let tmp = common::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    let project_dir = ws.join("projects/app");
    std::fs::create_dir_all(&project_dir).unwrap();

    let member = ws.join("github/acme/svc");
    std::fs::create_dir_all(&member).unwrap();
    std::fs::write(member.join("go.mod"), "module example.com/svc\n\ngo 1.22\n").unwrap();
    git_init_with_commit(&member);

    std::fs::write(
        project_dir.join("rwv.toml"),
        "[repositories.\"github/acme/svc\"]\n\
         type = \"git\"\n\
         url = \"https://github.com/acme/svc.git\"\n\
         version = \"main\"\n\
         role = \"owned\"\n\
         \n[integrations.go-work]\n\
         go-version = 122\n",
    )
    .unwrap();
    git_init_with_commit(&project_dir);
    std::fs::write(ws.join(".rwv-active"), "app\n").unwrap();

    let (ok, stdout, stderr) = rwv(&["materialize"], &ws);
    let out = format!("{stdout}\n{stderr}");

    assert!(
        !ok,
        "materialize must refuse over a go-version it cannot read:\n{out}"
    );
    assert!(
        out.contains("go-work") && out.contains("go-version"),
        "the refusal names the integration and the field:\n{out}"
    );
    assert!(
        !project_dir.join("go.work").exists(),
        "go.work must not be authored from a setting that did not parse:\n{out}"
    );
}
