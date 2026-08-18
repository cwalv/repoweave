//! The concurrency boundaries rwv declares it does NOT cross.
//!
//! `docs/internals/concurrency.md` puts two things out of scope on purpose:
//! universal per-verb mutual exclusion, and preventing other processes from
//! touching the working tree. Both are decisions, not gaps — and a decision
//! written only in prose decays the first time a reader tidies the asymmetry.
//! What is here fails when either boundary is crossed, so crossing one becomes
//! a conversation with the ruling rather than a quiet widening.
//!
//! Neither test asserts that the boundary is a good idea. They assert it is
//! where the ruling says it is.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

mod common;

// ---------------------------------------------------------------------------
// Boundary 1: exclusion is sync's tool, not a verb-surface mutex
// ---------------------------------------------------------------------------

/// The one module allowed to acquire the op lease, and the one call site
/// outside it.
///
/// `op_state.rs` is the lease's own implementation, so its internal uses are
/// not the question; the question is which VERBS take it.
const LEASE_ACQUIRING_SITES: [(&str, usize); 1] = [("sync.rs", 1)];

fn src_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read src dir").flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// Source lines with comment lines dropped, so prose naming a call is not
/// counted as one.
fn code_lines(text: &str) -> Vec<&str> {
    text.lines()
        .map(str::trim_start)
        .filter(|line| !line.starts_with("//"))
        .collect()
}

/// Call sites of `needle` per file under `src/`, excluding `op_state.rs`.
fn call_sites(needle: &str) -> Vec<(String, usize)> {
    let src = src_dir();
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    files.sort();

    let mut found = Vec::new();
    for file in files {
        let rel = file
            .strip_prefix(&src)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        if rel == "op_state.rs" {
            continue;
        }
        let text = std::fs::read_to_string(&file).expect("read source file");
        let count = code_lines(&text)
            .iter()
            .filter(|line| line.contains(needle))
            .count();
        if count > 0 {
            found.push((rel, count));
        }
    }
    found
}

/// **Boundary: rwv does not serialize its verbs against each other.** The op
/// lease is `sync`'s tool. Git does not serialize its commands either, and a
/// lease across the whole verb surface would install the wedged-workspace
/// failure mode everywhere — a stale claim needs a human, which is a cost worth
/// paying once, at the one place a half-applied multi-repo operation would
/// otherwise corrupt.
///
/// STRUCTURAL PIN, licensed as a prohibition over an enumerable population:
/// "no verb acquires" is a claim about source, and the behavioural form —
/// driving every verb concurrently against every other — is a product of the
/// verb surface with itself.
///
/// SCOPE, and therefore what is invisible here: this reads `acquire_op(` on
/// non-comment lines of `src/**/*.rs`, excluding `op_state.rs` itself. A verb
/// that reached exclusion by some other route — its own `create_new` on a file
/// of its own, an `flock`, a lease taken through a helper this does not name —
/// is not caught. What narrows that gap is not here but in
/// `tests/state_file_publish_audit_test.rs`, which holds `EXCLUSIVE_CREATE` —
/// the closed, derived set of names rwv publishes by exclusive create — to its
/// declared constants in both directions. A new exclusion mechanism has to
/// register a name there to be published at all, and that file reddens when one
/// appears.
#[test]
fn the_op_lease_is_syncs_tool_and_not_a_verb_surface_mutex() {
    let sites = call_sites("acquire_op(");

    assert!(
        !sites.is_empty(),
        "non-vacuity: the scan must find the acquisition it exists to bound. \
         An empty result means the spelling moved and this pin is measuring \
         nothing."
    );

    let expected: BTreeSet<(String, usize)> = LEASE_ACQUIRING_SITES
        .iter()
        .map(|&(file, count)| (file.to_string(), count))
        .collect();
    let actual: BTreeSet<(String, usize)> = sites.iter().cloned().collect();

    assert_eq!(
        actual, expected,
        "the op lease is acquired outside `sync` (or has moved). Extending \
         exclusive leases across the verb surface is out of scope by ruling, \
         not by omission: read docs/internals/concurrency.md before widening \
         this list, and if the ruling has changed, change it there first."
    );
}

// ---------------------------------------------------------------------------
// Boundary 2: the working tree is shared, unlocked space
// ---------------------------------------------------------------------------

/// **Boundary: rwv never locks the working tree, and interference with it is a
/// detection problem.** Git cannot stop an editor writing during `git add`, and
/// rwv does not try either: another process writing a member checkout while a
/// verb runs SUCCEEDS, and what rwv owes is an honest record afterwards, not a
/// blocked write.
///
/// Driven, because "the write is not prevented" is observable: a `cargo` shim
/// stands inside the running verb and writes into a member checkout, and the
/// bytes are there afterwards. The pin fails if some later change starts
/// locking member checkouts for the duration of a verb — at which point the
/// write would fail or be reverted, and this test would say so.
///
/// UNIX ONLY, at the test rather than the file: the instrument is a `#!/bin/sh`
/// script dispatched as `cargo` off PATH, and Windows resolves executables by
/// PATHEXT with no executable bit. The boundary is not platform-specific.
#[test]
#[cfg(unix)]
fn a_write_into_a_member_while_a_verb_runs_is_not_prevented() {
    use std::os::unix::fs::PermissionsExt;

    let Ok(real_cargo) = which::which("cargo") else {
        eprintln!("skipping: `cargo` not found on PATH");
        return;
    };
    let tmp = common::tempdir().unwrap();
    let ws = weave_with_a_generator(tmp.path());
    let member_file = ws.join("github/acme/lib/src/lib.rs");

    let (ok, first) = rwv(&["materialize"], &ws);
    assert!(ok, "precondition: the weave materializes cleanly:\n{first}");

    let shim_dir = tmp.path().join("shim");
    std::fs::create_dir_all(&shim_dir).unwrap();
    let shim = shim_dir.join("cargo");
    std::fs::write(
        &shim,
        format!(
            "#!/bin/sh\n\
             printf 'pub const WRITTEN_MID_VERB: u8 = 1;\\n' > '{member}'\n\
             exec '{real}' \"$@\"\n",
            member = member_file.display(),
            real = real_cargo.display(),
        ),
    )
    .unwrap();
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();

    let (ok, ran) = rwv_with_path_prefix(&["materialize"], &ws, &shim_dir);
    assert!(ok, "{ran}");

    assert_eq!(
        std::fs::read_to_string(&member_file).unwrap(),
        "pub const WRITTEN_MID_VERB: u8 = 1;\n",
        "the foreign write must stand: rwv holds no lock on a member checkout \
         while a verb runs, and taking one is out of scope by ruling. If this \
         fails because the write was blocked or reverted, the boundary moved \
         and docs/internals/concurrency.md is the place to argue it."
    );
}

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

const EMPTY_LOCK: &str = "{\n  \"repositories\": {}\n}\n";

fn rwv(args: &[&str], cwd: &Path) -> (bool, String) {
    let output = common::rwv()
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("rwv should run");
    (
        output.status.success(),
        format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    )
}

#[cfg(unix)]
fn rwv_with_path_prefix(args: &[&str], cwd: &Path, prepend: &Path) -> (bool, String) {
    let inherited = std::env::var("PATH").unwrap_or_default();
    let output = common::rwv()
        .args(args)
        .current_dir(cwd)
        .env("PATH", format!("{}:{inherited}", prepend.display()))
        .output()
        .expect("rwv should run");
    (
        output.status.success(),
        format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    )
}

fn git_init_with_commit(dir: &Path) {
    common::git_in(dir, &["init", "--initial-branch=main"]);
    common::git_in(dir, &["config", "user.email", "test@test.com"]);
    common::git_in(dir, &["config", "user.name", "Test"]);
    common::git_in(dir, &["add", "-A"]);
    common::git_in(dir, &["commit", "-m", "init"]);
}

/// A weave with one Rust member, so `materialize` really runs a generator and
/// there is a subprocess to stand inside.
#[cfg(unix)]
fn weave_with_a_generator(root: &Path) -> PathBuf {
    let ws = root.join("ws");
    let project_dir = ws.join("projects/app");
    std::fs::create_dir_all(&project_dir).unwrap();

    let member = ws.join("github/acme/lib");
    std::fs::create_dir_all(member.join("src")).unwrap();
    std::fs::write(
        member.join("Cargo.toml"),
        "[package]\nname = \"lib\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(member.join("src/lib.rs"), "").unwrap();
    git_init_with_commit(&member);

    std::fs::write(
        project_dir.join("rwv.toml"),
        "[repositories.\"github/acme/lib\"]\ntype = \"git\"\nurl = \"https://github.com/acme/lib.git\"\nversion = \"main\"\nrole = \"owned\"\n",
    )
    .unwrap();
    std::fs::write(project_dir.join("rwv.lock"), EMPTY_LOCK).unwrap();
    git_init_with_commit(&project_dir);
    std::fs::write(ws.join(".rwv-active"), "app\n").unwrap();

    let ctx = repoweave::workspace::WorkspaceContext::resolve_invocation(&ws, None).unwrap();
    repoweave::activate::activate_intent_with_options(
        "app",
        &ctx,
        repoweave::activate::ActivateOptions {
            no_materialize: true,
        },
    )
    .expect("intent activation should author the workspace manifest");
    ws
}
