//! Tests for clone-topology integrity checks (`rwv doctor`).
//!
//! Exercises the four sub-kinds of `CloneTopology` violations defined in
//! `docs/explanation/joints/clone-topology.md`:
//!
//! 1. `standalone-in-workweave` — a workweave hosts a full clone of a
//!    manifest repo (an inverted primary).
//! 2. `disconnected-weave-clone` — the canonical slot is a full clone, but
//!    the workweave checkouts of the same repo use a *different* canonical
//!    store; the weave-path clone publishes an unread object DAG.
//! 3. `wrong-parent-worktree` — a workweave checkout is a linked worktree
//!    whose canonical store is not the weave canonical (cross-DAG silent
//!    failure surface).
//! 4. `weave-clone-is-worktree` — the weave-path slot itself is a linked
//!    worktree of some other clone (full inversion: canonical has migrated
//!    out of the manifest slot).
//!
//! A clean weave (with healthy nested workweaves linked into the canonical
//! store) must produce zero clone-topology violations — that's the
//! regression gate against false positives.

use std::path::{Path, PathBuf};

mod common;

// ---------------------------------------------------------------------------
// Helpers (kept self-contained; this test file owns its own fixture
// scaffolding so it stays additive against parallel siblings.)
// ---------------------------------------------------------------------------

/// Build a primary workspace under `parent/ws/` with `projects/` and
/// `github/acme/` ready. Returns the workspace root.
fn make_primary(parent: &Path) -> PathBuf {
    let ws = parent.join("ws");
    std::fs::create_dir_all(ws.join("github/acme")).unwrap();
    std::fs::create_dir_all(ws.join("projects/app")).unwrap();
    ws
}

/// Run `git <args>` in `dir`, panicking on failure.
fn git(args: &[&str], dir: &Path) {
    let out = common::git()
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@test.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@test.com")
        .output()
        .expect("git command failed to start");
    assert!(
        out.status.success(),
        "git {:?} in {} failed: {}",
        args,
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Initialize a git repo at `path` with one commit so HEAD/main exist.
fn init_repo_with_commit(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    git(&["init", "--initial-branch=main"], path);
    std::fs::write(path.join("README.md"), "init\n").unwrap();
    git(&["add", "."], path);
    git(&["commit", "-m", "initial"], path);
}

/// Write an `rwv.yaml` with a single owned repo `github/acme/widget`.
fn write_manifest(ws: &Path) {
    let yaml = "\
repositories:
  github/acme/widget:
    type: git
    url: https://example.test/acme/widget.git
    version: main
    role: owned
";
    std::fs::write(ws.join("projects/app/rwv.yaml"), yaml).unwrap();
}

/// Write a healthy `.rwv-workweave` marker.
fn write_marker(ww_dir: &Path, primary: &Path, project: &str, parent: &Path) {
    std::fs::create_dir_all(ww_dir).unwrap();
    let primary_canon = primary
        .canonicalize()
        .unwrap_or_else(|_| primary.to_path_buf());
    let parent_canon = parent
        .canonicalize()
        .unwrap_or_else(|_| parent.to_path_buf());
    let content = format!(
        "{{\"primary\":\"{}\",\"project\":\"{}\",\"parent\":\"{}\"}}",
        primary_canon.display(),
        project,
        parent_canon.display(),
    );
    std::fs::write(ww_dir.join(".rwv-workweave"), content).unwrap();
}

/// Run `rwv doctor --json` against `ws` and return the parsed violations array.
fn doctor_violations(ws: &Path) -> Vec<serde_json::Value> {
    let out = common::rwv()
        .args(["doctor", "--json"])
        .current_dir(ws)
        .output()
        .expect("rwv doctor failed to start");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!(
            "rwv doctor --json did not produce JSON ({e}).\nstdout:\n{stdout}\nstderr:\n{}",
            String::from_utf8_lossy(&out.stderr)
        )
    });
    json["violations"].as_array().cloned().unwrap_or_default()
}

/// True iff `violations` contains a clone-topology violation whose sub_kind
/// is the variant named `sub_kind_tag` (kebab-cased name from `CloneTopologyKind`).
///
/// `CloneTopologyKind` derives a default serde enum-as-object representation:
/// each variant becomes a single key on the `sub_kind` object whose value is
/// the struct payload. So we look for that key.
fn has_clone_topology_sub_kind(violations: &[serde_json::Value], sub_kind_tag: &str) -> bool {
    violations.iter().any(|v| {
        v["kind"] == "clone-topology"
            && v["sub_kind"]
                .as_object()
                .map(|o| o.contains_key(sub_kind_tag))
                .unwrap_or(false)
    })
}

/// Count clone-topology violations of any sub-kind.
fn count_clone_topology(violations: &[serde_json::Value]) -> usize {
    violations
        .iter()
        .filter(|v| v["kind"] == "clone-topology")
        .count()
}

/// Build a healthy primary + canonical clone for the manifest repo so the
/// canonical store sits at `<ws>/github/acme/widget/.git`. Returns the
/// canonical clone path.
fn build_primary_with_canonical(parent: &Path) -> (PathBuf, PathBuf) {
    let ws = make_primary(parent);
    write_manifest(&ws);
    let canon = ws.join("github/acme/widget");
    init_repo_with_commit(&canon);
    (ws, canon)
}

// ===========================================================================
// Sub-kind 1: standalone-in-workweave
// ===========================================================================

/// A workweave's checkout of the manifest repo is itself a full clone (its
/// own .git/ directory), not a linked worktree of the canonical store: an
/// inverted primary.
#[test]
fn standalone_in_workweave_is_reported() {
    let tmp = common::tempdir().unwrap();
    let (ws, _canon) = build_primary_with_canonical(tmp.path());

    // Synthesize a workweave whose `github/acme/widget` is a *full clone*
    // (its own .git dir), not a worktree linked to the canonical.
    let ww_dir = tmp.path().join(".workweaves/app--inverted");
    std::fs::create_dir_all(ww_dir.join("github/acme")).unwrap();
    write_marker(&ww_dir, &ws, "app", &ws);

    let ww_repo = ww_dir.join("github/acme/widget");
    init_repo_with_commit(&ww_repo);

    let violations = doctor_violations(&ws);
    assert!(
        has_clone_topology_sub_kind(&violations, "standalone-in-workweave"),
        "expected standalone-in-workweave violation; got: {:#?}",
        violations
    );
}

// ===========================================================================
// Sub-kind 2: disconnected-weave-clone
// ===========================================================================

/// The canonical slot at `<ws>/<repo>` is a healthy full clone (its own
/// .git), but at least one workweave checkout uses a *different* canonical
/// store. The canonical's DAG is islanded.
#[test]
fn disconnected_weave_clone_is_reported() {
    let tmp = common::tempdir().unwrap();
    let (ws, _canon) = build_primary_with_canonical(tmp.path());

    // Build a workweave whose `widget` is a full clone of its own (separate
    // object DAG from the canonical). That makes the canonical a publisher
    // nobody reads.
    let ww_dir = tmp.path().join(".workweaves/app--disconnected");
    std::fs::create_dir_all(ww_dir.join("github/acme")).unwrap();
    write_marker(&ww_dir, &ws, "app", &ws);
    let ww_repo = ww_dir.join("github/acme/widget");
    init_repo_with_commit(&ww_repo);

    let violations = doctor_violations(&ws);
    assert!(
        has_clone_topology_sub_kind(&violations, "disconnected-weave-clone"),
        "expected disconnected-weave-clone violation; got: {:#?}",
        violations
    );
}

// ===========================================================================
// Sub-kind 3: wrong-parent-worktree
// ===========================================================================

/// A workweave checkout is a linked worktree, but its canonical store is
/// not the weave canonical. Cross-DAG `is_ancestor` lies silently.
///
/// Construction:
///   - canonical store at `<ws>/github/acme/widget/.git` (healthy).
///   - a *separate* full clone (the "wrong parent") at
///     `<ws>/.workweaves/app--rogue-store/github/acme/widget/` (a standalone
///     under .workweaves/ — this triggers standalone-in-workweave for
///     itself).
///   - a second workweave whose checkout is a linked worktree of the rogue
///     store, not the canonical. That's the wrong-parent-worktree case.
#[test]
fn wrong_parent_worktree_is_reported() {
    let tmp = common::tempdir().unwrap();
    let (ws, _canon) = build_primary_with_canonical(tmp.path());

    // Rogue full clone under .workweaves/
    let rogue_ww = tmp.path().join(".workweaves/app--rogue-store");
    std::fs::create_dir_all(rogue_ww.join("github/acme")).unwrap();
    write_marker(&rogue_ww, &ws, "app", &ws);
    let rogue_repo = rogue_ww.join("github/acme/widget");
    init_repo_with_commit(&rogue_repo);

    // Linked worktree of the rogue store, in a different workweave dir.
    let victim_ww = tmp.path().join(".workweaves/app--victim");
    std::fs::create_dir_all(victim_ww.join("github/acme")).unwrap();
    write_marker(&victim_ww, &ws, "app", &ws);
    let victim_repo = victim_ww.join("github/acme/widget");
    git(
        &[
            "worktree",
            "add",
            "-b",
            "victim/main",
            victim_repo.to_str().unwrap(),
        ],
        &rogue_repo,
    );

    let violations = doctor_violations(&ws);
    assert!(
        has_clone_topology_sub_kind(&violations, "wrong-parent-worktree"),
        "expected wrong-parent-worktree violation; got: {:#?}",
        violations
    );
}

// ===========================================================================
// Sub-kind 4: weave-clone-is-worktree
// ===========================================================================

/// The weave-path slot at `<ws>/<repo>` itself is a linked worktree of some
/// other clone. Total inversion.
///
/// Construction: a source clone outside the manifest tree, then
/// `git worktree add` the canonical slot path off it. The slot looks like a
/// workspace, but its canonical store is elsewhere.
#[test]
fn weave_clone_is_worktree_is_reported() {
    let tmp = common::tempdir().unwrap();
    let ws = make_primary(tmp.path());
    write_manifest(&ws);

    // Source clone (where the actual canonical store lives) outside the
    // manifest tree.
    let outsider = tmp.path().join("outsider-clone");
    init_repo_with_commit(&outsider);

    // Carve the canonical slot as a linked worktree of the outsider.
    let canonical_slot = ws.join("github/acme/widget");
    // The parent of the slot must exist (it does — `make_primary` makes
    // `github/acme`), but the slot itself must not.
    git(
        &[
            "worktree",
            "add",
            "-b",
            "canonical/main",
            canonical_slot.to_str().unwrap(),
        ],
        &outsider,
    );

    let violations = doctor_violations(&ws);
    assert!(
        has_clone_topology_sub_kind(&violations, "weave-clone-is-worktree"),
        "expected weave-clone-is-worktree violation; got: {:#?}",
        violations
    );
}

// ===========================================================================
// Clean: healthy nested workweaves linked into the canonical → zero
// clone-topology violations
// ===========================================================================

/// A clean weave with the canonical at `<ws>/<repo>` and a workweave checkout
/// that is a *real* `git worktree add` of the canonical produces zero
/// clone-topology violations.
#[test]
fn healthy_canonical_plus_linked_workweave_is_clean() {
    let tmp = common::tempdir().unwrap();
    let (ws, canon) = build_primary_with_canonical(tmp.path());

    // Create a workweave directory with a healthy marker.
    let ww_dir = tmp.path().join(".workweaves/app--healthy");
    std::fs::create_dir_all(ww_dir.join("github/acme")).unwrap();
    write_marker(&ww_dir, &ws, "app", &ws);

    // Link the workweave's checkout into the canonical store via
    // `git worktree add` — the I2 invariant.
    let ww_repo = ww_dir.join("github/acme/widget");
    git(
        &[
            "worktree",
            "add",
            "-b",
            "healthy/main",
            ww_repo.to_str().unwrap(),
        ],
        &canon,
    );

    let violations = doctor_violations(&ws);
    assert_eq!(
        count_clone_topology(&violations),
        0,
        "healthy canonical + linked workweave should produce zero \
         clone-topology violations; got: {:#?}",
        violations
    );
}

/// A weave with no `.workweaves/` directory at all and a canonical-only repo
/// produces zero clone-topology violations.
#[test]
fn lone_canonical_with_no_workweaves_is_clean() {
    let tmp = common::tempdir().unwrap();
    let (ws, _canon) = build_primary_with_canonical(tmp.path());

    let violations = doctor_violations(&ws);
    assert_eq!(
        count_clone_topology(&violations),
        0,
        "lone canonical with no workweaves should produce zero \
         clone-topology violations; got: {:#?}",
        violations
    );
}

// ===========================================================================
// Reference-alias carve-out
//
// A `reference` repo is materialized as a *symlink* to the single canonical
// weave-root clone, not a worktree. `git rev-parse --git-common-dir` follows
// the symlink and resolves to `<weave>/<repo>/.git`, so the workweave
// checkout's self-store equals its resolved store — the exact shape that
// triggers `standalone-in-workweave`. The scan must exclude the symlink
// (a `CheckoutKind::ReferenceAlias`) *before* that check, because the symlink
// IS the canonical store viewed through a link — it upholds the
// single-canonical-store invariant by identity rather than violating it.
//
// The adversarial requirement: the carve-out must distinguish a symlink-alias
// (valid) from a *real* standalone store (the genuine inversion case).
// If the skip is too broad, it blinds the scanner to real corruption.
// ===========================================================================

/// Create a symlinked reference checkout at `ww_repo` pointing at the
/// canonical clone `canon` — the on-disk shape `rwv workweave create`
/// produces for a `role: reference` repo. The parent directory is created
/// first; the symlink is the leaf.
#[cfg(unix)]
fn symlink_reference_checkout(ww_repo: &Path, canon: &Path) {
    std::fs::create_dir_all(ww_repo.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(canon, ww_repo).unwrap();
    assert!(
        ww_repo.is_symlink(),
        "fixture must produce a symlink at {}",
        ww_repo.display()
    );
}

/// A workweave holding a *symlinked* reference checkout of the manifest repo
/// produces zero clone-topology violations. The symlink resolves through to
/// the canonical store (its self-store equals its resolved store), which would
/// look identical to `standalone-in-workweave` — but it is the canonical store
/// viewed through a symlink, which upholds I1 by identity. The
/// `ReferenceAlias` carve-out must skip it.
#[cfg(unix)]
#[test]
fn symlinked_reference_in_workweave_is_clean() {
    let tmp = common::tempdir().unwrap();
    let (ws, canon) = build_primary_with_canonical(tmp.path());

    let ww_dir = tmp.path().join(".workweaves/app--ref");
    write_marker(&ww_dir, &ws, "app", &ws);

    // Symlink the workweave's `github/acme/widget` at the canonical clone —
    // the reference-repo materialization mode.
    let ww_repo = ww_dir.join("github/acme/widget");
    symlink_reference_checkout(&ww_repo, &canon);

    let violations = doctor_violations(&ws);
    assert_eq!(
        count_clone_topology(&violations),
        0,
        "a symlinked reference checkout must produce zero clone-topology \
         violations (it is the canonical store viewed through a symlink, \
         not a standalone store); got: {:#?}",
        violations
    );
}

/// THE CRITICAL ADVERSARIAL TEST: a *real* inversion — an actual
/// standalone clone (a real `.git` directory, NOT a symlink) living inside a
/// workweave — STILL fires `standalone-in-workweave`, even with a symlinked
/// reference present in a sibling workweave. The carve-out must distinguish
/// the symlink-alias (valid) from the real standalone store (a violation).
/// If the skip is too broad and blinds the scanner to genuine corruption,
/// that is a failure of the carve-out.
#[cfg(unix)]
#[test]
fn real_standalone_still_fires_alongside_symlinked_reference() {
    let tmp = common::tempdir().unwrap();
    let (ws, canon) = build_primary_with_canonical(tmp.path());

    // Workweave A: a valid symlinked reference checkout (must be skipped).
    let ref_ww = tmp.path().join(".workweaves/app--ref");
    write_marker(&ref_ww, &ws, "app", &ws);
    symlink_reference_checkout(&ref_ww.join("github/acme/widget"), &canon);

    // Workweave B: a REAL standalone clone — its own `.git` directory, a real
    // directory (not a symlink). This is the genuine inversion the scan must
    // still catch.
    let bad_ww = tmp.path().join(".workweaves/app--inverted");
    std::fs::create_dir_all(bad_ww.join("github/acme")).unwrap();
    write_marker(&bad_ww, &ws, "app", &ws);
    let bad_repo = bad_ww.join("github/acme/widget");
    init_repo_with_commit(&bad_repo);
    assert!(
        !bad_repo.is_symlink() && bad_repo.join(".git").is_dir(),
        "fixture: the standalone checkout must be a real clone, not a symlink"
    );

    let violations = doctor_violations(&ws);
    assert!(
        has_clone_topology_sub_kind(&violations, "standalone-in-workweave"),
        "a REAL standalone clone inside a workweave must STILL fire \
         standalone-in-workweave even when a symlinked reference is present; \
         the carve-out must not blind the scanner to genuine corruption. \
         got: {:#?}",
        violations
    );

    // The standalone-in-workweave finding must name the REAL inversion's
    // checkout (`app--inverted`), not the symlinked reference (`app--ref`).
    let standalone_paths: Vec<&str> = violations
        .iter()
        .filter(|v| {
            v["kind"] == "clone-topology"
                && v["sub_kind"]
                    .as_object()
                    .map(|o| o.contains_key("standalone-in-workweave"))
                    .unwrap_or(false)
        })
        .filter_map(|v| v["absolute_path"].as_str())
        .collect();
    assert!(
        standalone_paths.iter().any(|p| p.contains("app--inverted")),
        "the standalone-in-workweave finding must name the real inversion; \
         got paths: {standalone_paths:?}"
    );

    // The carve-out must produce NO finding of any kind pointing at the
    // symlinked reference checkout — it is excluded before every sub-check.
    let ref_checkout = ref_ww.join("github/acme/widget");
    let ref_checkout_str = ref_checkout.to_string_lossy().into_owned();
    let mentions_reference = violations.iter().any(|v| {
        [v["absolute_path"].as_str(), v["repo_path"].as_str()]
            .into_iter()
            .flatten()
            .any(|p| p == ref_checkout_str)
    });
    assert!(
        !mentions_reference,
        "no violation may point at the symlinked reference checkout {}; \
         the carve-out excludes it before every sub-check. got: {:#?}",
        ref_checkout.display(),
        violations
    );
}
