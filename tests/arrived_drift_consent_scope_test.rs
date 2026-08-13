//! Which verbs the drift consents bind.
//!
//! Amendment A2 made `rwv materialize` refuse on arriving at attested content
//! rwv never accepted, naming the two consents. The refusal reads on the
//! consent the verb carries, but the thing that settles drift is the install
//! hooks: they re-run each generator and record what it produces as accepted.
//! Every activating verb runs them, and only one of those verbs has a flag to
//! carry an answer.
//!
//! So these pin the prohibition rather than the verb: no run re-attests
//! arrived drift without being told which way. `rwv activate` is the headline
//! — it printed the finding naming both consents and then settled the drift in
//! the same run — and `add`, `remove` and `update` settled it while saying
//! nothing at all.
//!
//! Each drift arm is paired with a control on the same fixture without drift,
//! because "the generator did not run" is equally true of a build where it
//! never runs. The discriminator is a member the resolve can be watched
//! entering or leaving, not the attestation alone.

use std::path::{Path, PathBuf};

mod common;

fn git_run(cwd: &Path, args: &[&str]) -> String {
    let output = common::git()
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("git should be available");
    assert!(
        output.status.success(),
        "git {args:?} in {} failed: {}",
        cwd.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

/// A bare remote holding `files` on `main`.
///
/// `rwv add` reads `origin/HEAD` off the clone it is given and refuses without
/// it, so the member clones have to come from somewhere rather than be `git
/// init`ed in place.
fn init_bare_repo(bare: &Path, files: &[(&str, &str)]) {
    let parent = bare.parent().unwrap();
    std::fs::create_dir_all(parent).unwrap();
    let stem = bare.file_stem().unwrap().to_string_lossy().into_owned();
    git_run(
        parent,
        &[
            "init",
            "--bare",
            "--initial-branch=main",
            bare.to_str().unwrap(),
        ],
    );
    let seed = parent.join(format!("__seed_{stem}"));
    git_run(
        parent,
        &["clone", bare.to_str().unwrap(), seed.to_str().unwrap()],
    );
    git_run(&seed, &["config", "user.email", "test@test.com"]);
    git_run(&seed, &["config", "user.name", "Test"]);
    for (name, body) in files {
        let path = seed.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, body).unwrap();
    }
    git_run(&seed, &["add", "-A"]);
    git_run(&seed, &["commit", "-m", "initial"]);
    git_run(&seed, &["push", "origin", "main"]);
}

/// A two-version directory source, plus the `.cargo/config.toml` that puts it
/// in place of crates.io.
///
/// Two versions is what makes a resolve observable: a lock holding the older
/// one is a pin a fresh resolve would move.
fn write_local_crate_source(source_dir: &Path, weave_root: &Path, versions: &[&str]) {
    for version in versions {
        let pkg = source_dir.join(format!("pinnable-{version}"));
        std::fs::create_dir_all(pkg.join("src")).unwrap();
        std::fs::write(
            pkg.join("Cargo.toml"),
            format!(
                "[package]\nname = \"pinnable\"\nversion = \"{version}\"\nedition = \"2021\"\n"
            ),
        )
        .unwrap();
        std::fs::write(pkg.join("src/lib.rs"), "pub fn pinned() {}\n").unwrap();
        std::fs::write(pkg.join(".cargo-checksum.json"), r#"{"files":{}}"#).unwrap();
    }
    std::fs::create_dir_all(weave_root.join(".cargo")).unwrap();
    std::fs::write(
        weave_root.join(".cargo/config.toml"),
        format!(
            "[source.crates-io]\nreplace-with = \"local\"\n\n[source.local]\ndirectory = \"{}\"\n",
            source_dir.display()
        ),
    )
    .unwrap();
}

fn locked_pinnable_version(lock_text: &str) -> Option<String> {
    let mut lines = lock_text.lines();
    while let Some(line) = lines.next() {
        if line.trim() == r#"name = "pinnable""# {
            return lines
                .next()
                .and_then(|v| v.trim().strip_prefix(r#"version = ""#).map(str::to_string))
                .and_then(|v| v.strip_suffix('"').map(str::to_string));
        }
    }
    None
}

struct Fixture {
    _tmp: tempfile::TempDir,
    ws: PathBuf,
}

impl Fixture {
    fn rwv(&self, args: &[&str]) -> (bool, String) {
        let output = common::rwv()
            .args(args)
            .current_dir(&self.ws)
            .output()
            .expect("rwv should run");
        (
            output.status.success(),
            format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ),
        )
    }

    fn lock(&self) -> PathBuf {
        self.ws.join("projects/app/Cargo.lock")
    }

    fn lock_text(&self) -> String {
        std::fs::read_to_string(self.lock()).unwrap_or_default()
    }

    fn drift_is_unaccepted(&self) -> bool {
        self.rwv(&["doctor"])
            .1
            .contains("differs from the last rwv-accepted generation")
    }
}

/// A primary weave that SELECTS `app`, with two cargo members in the manifest
/// and a third clone sitting on disk that the manifest does not name.
///
/// Selecting `app` is the load-bearing part. Intent mode returns before the
/// hooks when the root presents some other project, and a fixture where it
/// does cannot reach the step under test. The unnamed third clone is what
/// `rwv add` names by path — the arm that needs no network.
fn fixture() -> Fixture {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let ws = root.join("ws");
    std::fs::create_dir_all(ws.join("projects")).unwrap();
    write_local_crate_source(&root.join("crate-source"), &ws, &["0.1.0", "0.1.1"]);

    let bares = root.join("upstream");
    init_bare_repo(
        &bares.join("server.git"),
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"server\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
                 [dependencies]\npinnable = \"0.1\"\n",
            ),
            ("src/lib.rs", ""),
        ],
    );
    for (repo, package) in [("lib", "acmelib"), ("extra", "acmeextra")] {
        init_bare_repo(
            &bares.join(format!("{repo}.git")),
            &[
                (
                    "Cargo.toml",
                    &format!(
                        "[package]\nname = \"{package}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"
                    ),
                ),
                ("src/lib.rs", ""),
            ],
        );
    }

    let mut manifest = String::new();
    let mut lock_entries = Vec::new();
    for name in ["server", "lib", "extra"] {
        let bare = bares.join(format!("{name}.git"));
        let canonical = ws.join(format!("github/acme/{name}"));
        std::fs::create_dir_all(canonical.parent().unwrap()).unwrap();
        git_run(
            &root,
            &["clone", bare.to_str().unwrap(), canonical.to_str().unwrap()],
        );
        git_run(&canonical, &["config", "user.email", "test@test.com"]);
        git_run(&canonical, &["config", "user.name", "Test"]);
        if name == "extra" {
            continue;
        }
        let sha = git_run(&canonical, &["rev-parse", "HEAD"]);
        manifest.push_str(&format!(
            "[repositories.\"github/acme/{name}\"]\ntype = \"git\"\nurl = \"{}\"\nversion = \"main\"\nrole = \"owned\"\n",
            bare.display()
        ));
        lock_entries.push(format!(
            "\"github/acme/{name}\": {{\"type\": \"git\", \"url\": {:?}, \"version\": {sha:?}}}",
            bare.to_str().unwrap()
        ));
    }

    let project_bare = bares.join("project.git");
    init_bare_repo(&project_bare, &[("README", "app")]);
    let project_dir = ws.join("projects/app");
    git_run(
        &root,
        &[
            "clone",
            project_bare.to_str().unwrap(),
            project_dir.to_str().unwrap(),
        ],
    );
    git_run(&project_dir, &["config", "user.email", "test@test.com"]);
    git_run(&project_dir, &["config", "user.name", "Test"]);
    std::fs::write(project_dir.join("rwv.toml"), &manifest).unwrap();
    std::fs::write(project_dir.join(".gitignore"), "/Cargo.lock\n").unwrap();
    let raw_lock = format!("{{\"repositories\": {{{}}}}}", lock_entries.join(","));
    let lock = repoweave::manifest::LockFile::from_json_str(&raw_lock).unwrap();
    repoweave::lock::write_lock(&lock, &project_dir.join("rwv.lock")).unwrap();
    std::fs::write(ws.join(".rwv-active"), "app\n").unwrap();

    let ctx = repoweave::workspace::WorkspaceContext::resolve(&ws, None).unwrap();
    repoweave::activate::activate_intent_with_options(
        "app",
        &ctx,
        repoweave::activate::ActivateOptions {
            no_materialize: true,
        },
    )
    .expect("fixture: intent activation should succeed");
    assert!(
        !project_dir.join("Cargo.lock").exists(),
        "fixture: the setup must not leave a lock behind"
    );
    git_run(&project_dir, &["add", "-A"]);
    git_run(&project_dir, &["commit", "-m", "manifest + managed files"]);
    git_run(&project_dir, &["push", "origin", "main"]);

    Fixture { _tmp: tmp, ws }
}

/// Materialize once so the lock exists and is attested. The fixture's clean
/// starting point, and the control arms stop here.
fn materialized(f: &Fixture) {
    let (ok, report) = f.rwv(&["materialize"]);
    assert!(ok, "fixture: first materialize should succeed:\n{report}");
    assert_eq!(
        locked_pinnable_version(&f.lock_text()).as_deref(),
        Some("0.1.1"),
        "fixture: a first resolve should pick the newest matching version"
    );
    assert!(
        !f.drift_is_unaccepted(),
        "fixture: a freshly materialized weave has no drift"
    );
}

/// …then pin the lock back to the older version and leave that unattested:
/// content rwv never accepted, holding a resolve a re-resolve would move.
fn materialized_then_pinned_back(f: &Fixture) {
    materialized(f);
    let generated = f.lock_text();
    let pinned = generated.replace(r#"version = "0.1.1""#, r#"version = "0.1.0""#);
    assert_ne!(
        pinned, generated,
        "fixture: the downgrade must change the lock"
    );
    std::fs::write(f.lock(), &pinned).unwrap();
    assert!(
        f.drift_is_unaccepted(),
        "fixture: the pinned-back lock must read as drift"
    );
}

fn assert_named_both_consents(report: &str, verb: &str) {
    assert!(
        report.contains("[withheld]"),
        "`rwv {verb}` must say what it declined to do, not pass over it in \
         silence:\n{report}"
    );
    assert!(
        report.contains("rwv materialize --adopt-drifted")
            && report.contains("rwv materialize --regenerate-drifted"),
        "and name both exits, spelled as they are invoked, so the operator can \
         take the one they mean:\n{report}"
    );
}

/// The headline. `rwv activate`'s verify pass prints the finding that names
/// both consents, and the hooks it runs afterwards then took one of them.
///
/// Which one is not even determined: cargo honours a lock that satisfies its
/// constraints, so these bytes survive either way and the operator's resolve is
/// still on disk afterwards. What moves is the attestation, and moving it is
/// `--adopt-drifted` with nobody's consent — which is why the assertion under
/// test is the doctor finding and not the pin.
#[test]
fn activate_does_not_settle_the_drift_it_just_named() {
    if which::which("cargo").is_err() {
        eprintln!("skipping: `cargo` not found on PATH");
        return;
    }
    let f = fixture();
    materialized_then_pinned_back(&f);

    let (ok, report) = f.rwv(&["activate", "app"]);
    assert!(ok, "activate must still select the project:\n{report}");
    assert!(
        report.contains("differs from the last rwv-accepted generation"),
        "premise, not the claim: the run must still report the drift:\n{report}"
    );
    assert!(
        f.drift_is_unaccepted(),
        "a verb that names both consents and then acts without one is the \
         laundering they exist to prevent — the drift must still be \
         UNACCEPTED afterwards:\n{report}"
    );
    assert_named_both_consents(&report, "activate");
    assert_eq!(
        locked_pinnable_version(&f.lock_text()).as_deref(),
        Some("0.1.0"),
        "and the operator's resolve must be left alone"
    );
}

/// The control for every arm below: with nothing unsettled, the same verb runs
/// the generator and the new member enters the resolve.
///
/// Without this, "the generator did not run" is a claim no arm can distinguish
/// from a fixture where it never runs.
#[test]
fn a_verb_with_no_drift_in_its_way_still_runs_the_generator() {
    if which::which("cargo").is_err() {
        eprintln!("skipping: `cargo` not found on PATH");
        return;
    }
    let f = fixture();
    materialized(&f);
    assert!(
        !f.lock_text().contains(r#"name = "acmeextra""#),
        "fixture: the unnamed clone is not in the resolve yet"
    );

    let (ok, report) = f.rwv(&["add", "github/acme/extra"]);
    assert!(ok, "`rwv add` should succeed:\n{report}");
    assert!(
        !report.contains("[withheld]"),
        "nothing is being withheld here:\n{report}"
    );
    assert!(
        f.lock_text().contains(r#"name = "acmeextra""#),
        "the added member must enter the resolve, which only the generator \
         can do:\n{report}"
    );
}

/// `rwv add` said nothing about drift at all and settled it anyway. The
/// manifest change is not what is withheld — it lands, because the verb wrote
/// it before it regenerated and un-writing it is not the repair.
#[test]
fn add_withholds_the_generator_over_unsettled_drift() {
    if which::which("cargo").is_err() {
        eprintln!("skipping: `cargo` not found on PATH");
        return;
    }
    let f = fixture();
    materialized_then_pinned_back(&f);

    let (ok, report) = f.rwv(&["add", "github/acme/extra"]);
    assert!(ok, "the manifest change still lands:\n{report}");
    assert!(
        f.drift_is_unaccepted(),
        "the drift must still be UNACCEPTED afterwards:\n{report}"
    );
    assert_named_both_consents(&report, "add");
    assert!(
        !f.lock_text().contains(r#"name = "acmeextra""#),
        "and the generator must not have run — the control shows this same \
         member entering the resolve when nothing is in the way:\n{report}"
    );
    let manifest = std::fs::read_to_string(f.ws.join("projects/app/rwv.toml")).unwrap();
    assert!(
        manifest.contains("github/acme/extra"),
        "withholding the regeneration is not withholding the verb:\n{manifest}"
    );
}

/// `rwv remove` reaches the same regeneration from the other direction: the
/// member leaves the resolve rather than entering it.
#[test]
fn remove_withholds_the_generator_over_unsettled_drift() {
    if which::which("cargo").is_err() {
        eprintln!("skipping: `cargo` not found on PATH");
        return;
    }
    let f = fixture();
    materialized_then_pinned_back(&f);
    assert!(
        f.lock_text().contains(r#"name = "acmelib""#),
        "fixture: the member being removed is in the resolve to start with"
    );

    let (ok, report) = f.rwv(&["remove", "github/acme/lib"]);
    assert!(ok, "the manifest change still lands:\n{report}");
    assert!(
        f.drift_is_unaccepted(),
        "the drift must still be UNACCEPTED afterwards:\n{report}"
    );
    assert_named_both_consents(&report, "remove");
    assert!(
        f.lock_text().contains(r#"name = "acmelib""#),
        "and the generator must not have run:\n{report}"
    );
}

/// `rwv update` regenerates after re-snapshotting the lock, and that
/// regeneration is the same one.
///
/// Its own discriminator is the attestation alone — an update that advances
/// nothing moves no bytes — so the control above carries the "the generator
/// runs when nothing is in the way" half for this arm.
#[test]
fn update_withholds_the_generator_over_unsettled_drift() {
    if which::which("cargo").is_err() {
        eprintln!("skipping: `cargo` not found on PATH");
        return;
    }
    let f = fixture();
    materialized_then_pinned_back(&f);

    let (ok, report) = f.rwv(&["update"]);
    assert!(ok, "the lock re-snapshot still lands:\n{report}");
    assert!(
        f.drift_is_unaccepted(),
        "the drift must still be UNACCEPTED afterwards:\n{report}"
    );
    assert_named_both_consents(&report, "update");
}

/// The consent-carrying verb is the one this must not touch: `materialize`
/// answers the fork before the generator is reached, so there is nothing left
/// to withhold and the run must go through.
#[test]
fn materialize_with_a_consent_still_reaches_the_generator() {
    if which::which("cargo").is_err() {
        eprintln!("skipping: `cargo` not found on PATH");
        return;
    }
    let f = fixture();
    materialized_then_pinned_back(&f);

    let (ok, report) = f.rwv(&["materialize", "--regenerate-drifted"]);
    assert!(ok, "the consent must be honoured:\n{report}");
    assert!(
        !report.contains("[withheld]"),
        "a verb that was told which way must not be withheld from:\n{report}"
    );
    assert_eq!(
        locked_pinnable_version(&f.lock_text()).as_deref(),
        Some("0.1.1"),
        "and discarding the operator's resolve then re-resolving is what \
         `--regenerate-drifted` means — a pin still at 0.1.0 here says the \
         generator never ran:\n{report}"
    );
}
