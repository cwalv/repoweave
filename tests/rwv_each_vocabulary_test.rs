//! `rwv each` reads `rwv status --json` through its own closed copies of the
//! role and relation vocabularies, because a bundled plugin never names the
//! library. The copies must accept every token the library can report, and
//! this is where that is asked: the roles are the library's, the relations are
//! the ones its published schema declares, and the plugin is run against a
//! stand-in `rwv` that reports one repo for each.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::process::Command;

use repoweave::manifest::Role;
use repoweave::status::LockRelation;
use serde_json::{json, Value};

mod common;

fn tokens<T: serde::Serialize>(values: &[T]) -> Vec<String> {
    values
        .iter()
        .map(|v| {
            serde_json::to_value(v)
                .unwrap()
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect()
}

fn run_each_against(status: &Value) -> (bool, String, String) {
    let tmp = common::tempdir().unwrap();
    let bin = tmp.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::copy(env!("CARGO_BIN_EXE_rwv-each"), bin.join("rwv-each")).unwrap();
    let standin = bin.join("rwv");
    std::fs::write(
        &standin,
        format!("#!/bin/sh\ncat <<'STATUS'\n{status}\nSTATUS\n"),
    )
    .unwrap();
    std::fs::set_permissions(&standin, std::fs::Permissions::from_mode(0o755)).unwrap();

    // A sibling test's fork can still hold a just-written file open for
    // writing; exec then fails with ETXTBSY, here or in the plugin's own spawn
    // of the stand-in, until that child execs or exits.
    for _ in 0..100 {
        let out = Command::new(bin.join("rwv-each"))
            .args(["--dry-run", "--role", "reference", "--", "true"])
            .env("RWV_WORKSPACE", tmp.path())
            .env("RWV_PROJECT", "demo")
            .env_remove("RWV_WORKWEAVE")
            .env_remove("RWV_WORKWEAVE_UNREGISTERED")
            .output();
        let busy = match &out {
            Err(e) => e.kind() == std::io::ErrorKind::ExecutableFileBusy,
            Ok(out) => String::from_utf8_lossy(&out.stderr).contains("Text file busy"),
        };
        if busy {
            std::thread::sleep(std::time::Duration::from_millis(20));
            continue;
        }
        let out = out.expect("rwv-each runs");
        return (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        );
    }
    panic!("rwv-each stayed busy (ETXTBSY) for 2s")
}

fn repo(path: &str, role: &str, relation: &str) -> Value {
    json!({
        "path": path,
        "role": role,
        "absolute_path": format!("/ws/{path}"),
        "relation": relation,
    })
}

#[test]
fn the_plugin_reads_every_role_and_relation_the_library_reports() {
    let roles = tokens(Role::ALL);
    let schema = serde_json::to_value(schemars::schema_for!(LockRelation)).unwrap();
    let relations: Vec<String> = common::json_schema::enum_member_tokens(&schema)
        .into_iter()
        .collect();
    assert!(roles.len() > 1 && relations.len() > 1);

    let n = roles.len().max(relations.len());
    let paths: Vec<String> = (0..n).map(|i| format!("github/org/r{i}")).collect();
    let repos: Vec<Value> = paths
        .iter()
        .enumerate()
        .map(|(i, path)| {
            repo(
                path,
                &roles[i % roles.len()],
                &relations[i % relations.len()],
            )
        })
        .collect();

    let (ok, stdout, stderr) = run_each_against(&json!({ "repos": repos }));
    assert!(ok, "the plugin refused library output: {stderr}");
    for path in &paths {
        assert!(stdout.contains(path.as_str()), "{path} missing:\n{stdout}");
    }
}

#[test]
fn a_role_or_relation_the_library_does_not_define_is_refused_naming_it() {
    for (role, relation, unknown) in [
        ("owned", "no_lock", "no_lock"),
        ("owned", "stale", "stale"),
        ("primary", "ok", "primary"),
    ] {
        let (ok, stdout, stderr) =
            run_each_against(&json!({ "repos": [repo("github/org/a", role, relation)] }));
        assert!(!ok, "{unknown} was read as a repo:\n{stdout}");
        assert!(stderr.contains(unknown), "{unknown}: {stderr}");
    }
}
