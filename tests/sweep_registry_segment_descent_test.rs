//! `rwv activate`'s symlink-removal sweep must not descend into a member
//! checkout sitting under a registry segment no builtin names.
//!
//! The sweep's descent-refusal set used to be the same hand-enumerated list
//! that decided weave-root containment: the three builtins plus `projects`.
//! `local/` — where `rwv add` places a `file://` clone — was never in it, so
//! the sweep walked straight into a member's own working tree. Its unguarded
//! effect at the bottom of the recursion is `std::fs::remove_dir` on every
//! directory it finishes visiting, which succeeds silently on anything
//! empty. Git tracks no empty directory, so a member checkout can hold one
//! as genuine, uncommitted user state (a scratch output directory, an empty
//! `dist/`) that the sweep would delete on the next `rwv activate`.

use std::path::{Path, PathBuf};

mod common;

/// A weave holding one project and one member cloned under `local/`, the way
/// `rwv add <file:// source>` places it — the segment no builtin registry
/// names.
fn weave_with_a_local_member() -> (tempfile::TempDir, PathBuf) {
    let tmp = common::tempdir().unwrap();
    let origin = tmp.path().join("origin").join("acme").join("widgets.git");
    std::fs::create_dir_all(origin.parent().unwrap()).unwrap();
    common::init_bare_repo_with_commit(&origin);

    let ws = tmp.path().join("ws");
    std::fs::create_dir_all(ws.join("projects")).unwrap();

    common::rwv()
        .args(["init", "demo"])
        .current_dir(&ws)
        .assert()
        .success();
    common::rwv()
        .args(["add", &common::file_url(&origin)])
        .current_dir(&ws)
        .assert()
        .success();

    assert!(
        ws.join("local/acme/widgets").is_dir(),
        "fixture: `rwv add` from a file:// source should place the clone \
         under local/"
    );
    (tmp, ws)
}

/// The gap: an empty, git-untracked directory inside the member checkout
/// survives `rwv activate`.
///
/// The sweep runs on every activation (`remove_activation_symlinks` is step 2
/// of `run_activations`, unconditionally), so a single `activate` after the
/// scratch directory exists is enough to expose the pre-fix behaviour.
#[test]
fn a_member_checkouts_empty_directory_survives_activation() {
    let (_tmp, ws) = weave_with_a_local_member();
    let scratch = ws.join("local/acme/widgets/scratch-output");
    std::fs::create_dir_all(&scratch).unwrap();

    common::rwv()
        .args(["activate", "demo"])
        .current_dir(&ws)
        .assert()
        .success();

    assert!(
        scratch.is_dir(),
        "activation's symlink-removal sweep deleted an empty directory \
         inside the member checkout at {} — it must never descend past \
         local/acme/widgets in the first place",
        scratch.display()
    );
}

/// Same gap, the other sweep: `rwv doctor`'s undeclared-root-links scan must
/// not walk into the member checkout either.
///
/// A plain file deep inside the checkout would survive regardless of walk
/// depth — `collect_undeclared_links_in` only ever looks at symlinks — so
/// that alone would not prove the walk stopped short. This plants a symlink
/// at the owner-scoped shape the sweep's predicate accepts everywhere else
/// (`target_resolves_to_projects` / `surfacing_owner_dir`): a link whose
/// target, read as `projects/<project>/<the link's own root-relative path>`,
/// makes it look exactly like rwv's own surfacing of a file the project no
/// longer declares. If the walk reaches it, it is reported; if the walk
/// stops at `local/`, as it must, it is never seen.
#[test]
fn doctor_does_not_walk_into_a_local_member_checkout() {
    let (_tmp, ws) = weave_with_a_local_member();
    let link_rel = "local/acme/widgets/mystery.md";
    let target = "projects/demo/local/acme/widgets/mystery.md";
    repoweave::symlink::create(
        Path::new(target),
        &ws.join(link_rel),
        repoweave::symlink::LinkTarget::File,
    )
    .unwrap();

    common::rwv()
        .args(["activate", "demo"])
        .current_dir(&ws)
        .assert()
        .success();

    let out = common::rwv()
        .args(["doctor"])
        .current_dir(&ws)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        ws.join(link_rel).symlink_metadata().is_ok(),
        "the planted symlink inside the member checkout must survive doctor \
         untouched (doctor never repairs this finding either way, so its \
         mere presence proves nothing was removed)"
    );
    assert!(
        !stdout.contains("mystery.md"),
        "a symlink inside a member's own working tree is not a weave-root \
         concern even when it happens to match rwv's surfacing shape, and \
         must not be reported:\n{stdout}"
    );
}
