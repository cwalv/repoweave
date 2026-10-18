//! cargo-workspace reports `managed-file-user-held` on `Cargo.toml` in two
//! states, and an authoring verb treats them oppositely. With `resolver` and
//! no `members` — what disabling and re-enabling the integration leaves — the
//! next authoring verb writes `members` under the marker. With an unmarked
//! `members`, every authoring verb defers to it, so the finding stands until
//! the operator hands the key over. `.cargo/config.toml` splits the same way:
//! an unmarked key rwv would write is deferred to, and a file holding none of
//! rwv's keys is written by the next authoring verb.
//!
//! The defects pinned: a message naming a remedy that does nothing in the
//! state it is printed for, and either half of a take-over/defer split moving
//! without the other. Each remedy a message names is driven here to a cleared
//! finding, through the shipped binary.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

mod common;

const RESOLVER_ONLY: &str = "[workspace]\nresolver = \"2\"\n";

/// `members` names one of the two crates, so handing it over shows as the
/// second crate arriving; `resolver` is not the value rwv seeds, so a remedy
/// that keeps it is told apart from one that writes it back.
const UNMARKED_MEMBERS: &str = "[workspace]\n\
                                members = [\"github/acme/alpha\"]\n\
                                resolver = \"1\"\n\
                                \n\
                                [profile.release]\n\
                                opt-level = 3\n";

fn git_init_with_commit(dir: &Path) {
    common::git_in(dir, &["init", "--initial-branch=main"]);
    common::git_in(dir, &["config", "user.email", "test@test.com"]);
    common::git_in(dir, &["config", "user.name", "Test"]);
    common::git_in(dir, &["add", "-A"]);
    common::git_in(dir, &["commit", "-m", "init"]);
}

fn command(ws: &Path, path: Option<&OsStr>, args: &[&str]) -> std::process::Output {
    let mut cmd = common::rwv();
    cmd.args(args).current_dir(ws);
    if let Some(path) = path {
        cmd.env("PATH", path);
    }
    cmd.output().expect("rwv should run")
}

fn rwv(ws: &Path, path: Option<&OsStr>, args: &[&str]) {
    let output = command(ws, path, args);
    assert!(
        output.status.success(),
        "`rwv {}` failed:\n{}\n{}",
        args.join(" "),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Two owned crates in the active project `app`, nothing authored yet.
fn weave(root: &Path, cargo_settings: &str) -> PathBuf {
    let ws = root.join("ws");
    let project_dir = ws.join("projects/app");
    std::fs::create_dir_all(&project_dir).unwrap();

    let mut manifest = String::new();
    for crate_name in ["alpha", "beta"] {
        let member = ws.join("github/acme").join(crate_name);
        std::fs::create_dir_all(member.join("src")).unwrap();
        std::fs::write(
            member.join("Cargo.toml"),
            format!(
                "[package]\nname = \"{crate_name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"
            ),
        )
        .unwrap();
        std::fs::write(member.join("src/lib.rs"), "").unwrap();
        git_init_with_commit(&member);
        manifest.push_str(&format!(
            "[repositories.\"github/acme/{crate_name}\"]\n\
             type = \"git\"\n\
             url = \"https://github.com/acme/{crate_name}.git\"\n\
             version = \"main\"\n\
             role = \"owned\"\n\n"
        ));
    }
    manifest.push_str("[integrations.cargo-workspace]\nenabled = true\n");
    manifest.push_str(cargo_settings);
    std::fs::write(project_dir.join("rwv.toml"), manifest).unwrap();
    git_init_with_commit(&project_dir);
    std::fs::write(ws.join(".rwv-active"), "app\n").unwrap();
    ws
}

fn set_cargo_enabled(ws: &Path, enabled: bool) {
    let manifest = ws.join("projects/app/rwv.toml");
    let text = std::fs::read_to_string(&manifest).unwrap();
    let updated = text.replace(
        &format!("enabled = {}", !enabled),
        &format!("enabled = {enabled}"),
    );
    assert_ne!(updated, text, "fixture: cargo-workspace was not toggled");
    std::fs::write(&manifest, updated).unwrap();
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

/// Every cargo-workspace `managed-file-*` finding `rwv doctor --json` reports
/// on `file`, as `(kind, message)`.
fn findings_on(ws: &Path, path: Option<&OsStr>, file: &str) -> Vec<(String, String)> {
    let output = command(ws, path, &["doctor", "--json"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("doctor --json produced invalid JSON: {e}\n{stdout}"));
    json["issues"]
        .as_array()
        .expect("issues is an array")
        .iter()
        .filter(|issue| issue["integration"] == "cargo-workspace")
        .map(|issue| {
            (
                issue["kind"].as_str().unwrap().to_string(),
                issue["message"].as_str().unwrap().to_string(),
            )
        })
        .filter(|(kind, message)| kind.starts_with("managed-file-") && message.contains(file))
        .collect()
}

fn user_held_message(ws: &Path, path: Option<&OsStr>, file: &str, state: &str) -> String {
    let findings = findings_on(ws, path, file);
    match findings.as_slice() {
        [(kind, message)] if kind == "managed-file-user-held" => message.clone(),
        _ => panic!("{state}: expected one managed-file-user-held finding, got {findings:?}"),
    }
}

fn assert_cleared(ws: &Path, path: Option<&OsStr>, file: &str, remedy: &str) {
    let findings = findings_on(ws, path, file);
    assert!(
        findings.is_empty(),
        "{remedy} must clear the finding, got {findings:?}"
    );
}

fn assert_members_handed_over(cargo_toml: &Path, remedy: &str) -> String {
    let text = read(cargo_toml);
    assert!(
        text.contains("# managed by rwv\nmembers = [\"github/acme/alpha\", \"github/acme/beta\"]"),
        "{remedy} must leave both crates in a marked `members`:\n{text}"
    );
    text
}

#[test]
fn an_authoring_verb_takes_over_a_resolver_only_cargo_toml_and_defers_to_unmarked_members() {
    let tmp = common::tempdir().unwrap();
    let ws = weave(tmp.path(), "");
    let cargo_toml = ws.join("projects/app/Cargo.toml");
    let file = "Cargo.toml";

    rwv(&ws, None, &["doctor", "--fix"]);
    set_cargo_enabled(&ws, false);
    rwv(&ws, None, &["materialize"]);
    assert_eq!(
        read(&cargo_toml),
        RESOLVER_ONLY,
        "disabling must strip `members` and leave the seeded resolver"
    );
    set_cargo_enabled(&ws, true);

    let held = user_held_message(&ws, None, file, "re-enabled over the seeded resolver");
    assert!(
        held.contains("The next authoring verb") && held.contains("writes `members`"),
        "a resolver-only file's finding must name the authoring verb that takes it over: {held}"
    );
    for verb in [&["doctor", "--fix"][..], &["materialize"]] {
        rwv(&ws, None, verb);
        assert_eq!(
            read(&cargo_toml),
            RESOLVER_ONLY,
            "`rwv {}` must leave a resolver-only file to the authoring verb",
            verb.join(" ")
        );
    }

    rwv(
        &ws,
        None,
        &["role", "github/acme/beta", "fork", "--no-materialize"],
    );
    let taken = assert_members_handed_over(&cargo_toml, "the authoring verb");
    assert!(taken.contains("resolver = \"2\""), "{taken}");
    assert_cleared(&ws, None, file, "the authoring verb");

    std::fs::write(&cargo_toml, RESOLVER_ONLY.replace("resolver = \"2\"\n", "")).unwrap();
    rwv(&ws, None, &["doctor", "--fix"]);
    let remedy = "deleting `resolver`, then `rwv doctor --fix`";
    assert_members_handed_over(&cargo_toml, remedy);
    assert_cleared(&ws, None, file, remedy);

    std::fs::write(&cargo_toml, UNMARKED_MEMBERS).unwrap();
    let held = user_held_message(&ws, None, file, "an unmarked `members`");
    assert!(
        held.contains("An authoring verb leaves an unmarked `members` as it is")
            && held.contains("directly above `members`")
            && held.contains("delete `members` and `resolver`"),
        "an unmarked `members` finding must name the remedies that hand it over: {held}"
    );
    rwv(
        &ws,
        None,
        &["role", "github/acme/beta", "owned", "--no-materialize"],
    );
    assert_eq!(
        read(&cargo_toml),
        UNMARKED_MEMBERS,
        "an authoring verb must defer to an unmarked `members`"
    );
    user_held_message(
        &ws,
        None,
        file,
        "an unmarked `members` after an authoring verb",
    );

    std::fs::write(
        &cargo_toml,
        UNMARKED_MEMBERS.replace("members =", "# managed by rwv\nmembers ="),
    )
    .unwrap();
    rwv(&ws, None, &["doctor", "--fix"]);
    let remedy = "the marker line, then `rwv doctor --fix`";
    let marked = assert_members_handed_over(&cargo_toml, remedy);
    assert!(
        marked.contains("resolver = \"1\"") && marked.contains("[profile.release]"),
        "{remedy} must keep the operator's resolver and tables:\n{marked}"
    );
    assert_cleared(&ws, None, file, remedy);

    std::fs::write(
        &cargo_toml,
        UNMARKED_MEMBERS
            .replace("members = [\"github/acme/alpha\"]\n", "")
            .replace("resolver = \"1\"\n", ""),
    )
    .unwrap();
    rwv(&ws, None, &["doctor", "--fix"]);
    let remedy = "deleting `members` and `resolver`, then `rwv doctor --fix`";
    let rewritten = assert_members_handed_over(&cargo_toml, remedy);
    assert!(
        rewritten.contains("resolver = \"2\"") && rewritten.contains("[profile.release]"),
        "{remedy} must write both keys back and keep the operator's tables:\n{rewritten}"
    );
    assert_cleared(&ws, None, file, remedy);
}

/// Unix only: the `cargo` and `sccache` stand-ins are shebang scripts a child
/// resolves off `PATH`, the boundary `common::cargo_stand_in_path` states.
#[cfg(unix)]
#[test]
fn an_unmarked_wrapper_key_is_deferred_to_and_a_config_without_rwv_keys_is_taken_over() {
    use std::os::unix::fs::PermissionsExt;

    const UNMARKED_WRAPPER: &str = "[net]\n\
                                    git-fetch-with-cli = true\n\
                                    \n\
                                    [build]\n\
                                    rustc-wrapper = \"other\"\n";
    const MARKED_WRAPPER: &str = "# managed by rwv\nrustc-wrapper = \"sccache\"";

    let tmp = common::tempdir().unwrap();
    let ws = weave(tmp.path(), "rustc-wrapper = \"sccache\"\n");
    let bin = tmp.path().join("bin");
    let stand_in_path = common::cargo_stand_in_path(&bin);
    let sccache = bin.join("sccache");
    std::fs::write(&sccache, "#!/bin/sh\nexec \"$@\"\n").unwrap();
    std::fs::set_permissions(&sccache, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = Some(stand_in_path.as_os_str());
    let config = ws.join("projects/app/.cargo/config.toml");
    let file = "config.toml";

    rwv(&ws, path, &["doctor", "--fix"]);
    std::fs::write(&config, UNMARKED_WRAPPER).unwrap();
    let held = user_held_message(&ws, path, file, "an unmarked wrapper key");
    assert!(
        held.contains("An authoring verb leaves an unmarked key as it is")
            && held.contains("directly above the conflicting")
            && held.contains("delete the key and run an authoring verb"),
        "an unmarked key's finding must name the remedies that hand it over: {held}"
    );
    for verb in [
        &["role", "github/acme/beta", "fork", "--no-materialize"][..],
        &["materialize"],
    ] {
        rwv(&ws, path, verb);
        assert_eq!(
            read(&config),
            UNMARKED_WRAPPER,
            "`rwv {}` must defer to an unmarked wrapper key",
            verb.join(" ")
        );
    }
    user_held_message(&ws, path, file, "an unmarked wrapper key after the verbs");

    std::fs::write(
        &config,
        UNMARKED_WRAPPER.replace("rustc-wrapper", "# managed by rwv\nrustc-wrapper"),
    )
    .unwrap();
    rwv(&ws, path, &["doctor", "--fix"]);
    let remedy = "the marker line, then `rwv doctor --fix`";
    let marked = read(&config);
    assert!(
        marked.contains(MARKED_WRAPPER) && marked.contains("[net]"),
        "{remedy} must correct the key and keep the operator's tables:\n{marked}"
    );
    assert_cleared(&ws, path, file, remedy);

    let keyless = UNMARKED_WRAPPER.replace("rustc-wrapper = \"other\"\n", "");
    std::fs::write(&config, &keyless).unwrap();
    let held = user_held_message(&ws, path, file, "a config holding none of rwv's keys");
    assert!(
        held.contains("none of the keys rwv writes there")
            && held.contains("the next authoring verb"),
        "a config holding none of rwv's keys must name the authoring verb that writes them: {held}"
    );
    rwv(&ws, path, &["doctor", "--fix"]);
    assert_eq!(
        read(&config),
        keyless,
        "`rwv doctor --fix` must leave a config holding none of rwv's keys to the authoring verb"
    );
    rwv(
        &ws,
        path,
        &["role", "github/acme/beta", "owned", "--no-materialize"],
    );
    let remedy = "deleting the key, then an authoring verb";
    let written = read(&config);
    assert!(
        written.contains(MARKED_WRAPPER) && written.contains("[net]"),
        "{remedy} must write the key under the marker and keep the operator's tables:\n{written}"
    );
    assert_cleared(&ws, path, file, remedy);
}
