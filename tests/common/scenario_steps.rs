//! Step builders the scenarios and the kill matrix share.

use super::scenario_runner::{Exit, Step};
use super::scenario_sandbox::{self, Machine, Place, Remote, Repo};

pub const LIB: &str = "local/remotes/lib";
pub const APP: &str = "local/remotes/app";
pub const DOCS: &str = "local/remotes/docs";

pub fn a() -> Place {
    Place::Primary(Machine::A)
}

pub fn b() -> Place {
    Place::Primary(Machine::B)
}

pub fn ww(name: &str) -> Place {
    Place::ww(Machine::A, name)
}

pub fn create(name: &str) -> Step {
    Step::rwv(&a(), &["workweave", "create", "--project", "proj", name]).expect(Exit::Succeeds)
}

pub fn publish_app_and_project(mark: &'static str) -> Vec<Step> {
    vec![
        Step::commit(&a(), Repo::App, mark),
        Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::git(&a(), Repo::App, &["push", "origin", "main"])
            .pushes(Remote::origin(Repo::App), "main"),
        Step::git(&a(), Repo::Proj, &["push", "origin", "main"])
            .pushes(Remote::origin(Repo::Proj), "main"),
    ]
}

/// The primary leaves every branch of `main`'s: its project repo and members
/// detached, so `main` and each member's `main` are checked out nowhere.
pub fn main_checked_out_nowhere() -> Vec<Step> {
    [Repo::Proj, Repo::App, Repo::Lib]
        .into_iter()
        .map(|repo| Step::git(&a(), repo, &["switch", "-q", "--detach"]))
        .collect()
}

pub fn stack_on_a_seat_under(history: &'static str) -> Vec<Step> {
    vec![
        Step::rwv(&a(), &["branch", "set-history", history]).expect(Exit::Succeeds),
        create("w1"),
        Step::commit(&ww("w1"), Repo::App, "w1-app"),
        Step::rwv(&ww("w1"), &["lock", "--commit"]).expect(Exit::Succeeds),
        dependent_of(&ww("w1"), "w2", "proj--w1"),
        Step::commit(&ww("w1"), Repo::App, "w2-app"),
        Step::rwv(&ww("w1"), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::rwv(&ww("w1"), &["switch", "proj--w1"]).expect(Exit::Succeeds),
    ]
}

/// In the workspace `at`, `name` created as a feature onto `onto`, the
/// workspace switching to it.
pub fn dependent_of(at: &Place, name: &'static str, onto: &'static str) -> Step {
    Step::rwv(at, &["branch", "create", name, "--target", onto]).expect(Exit::Succeeds)
}

pub fn main_moves_app(mark: &'static str) -> Vec<Step> {
    vec![
        Step::commit(&a(), Repo::App, mark),
        Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
    ]
}

/// `stable`, declared from `main` with its own `stable-app` and `stable-lib`
/// lines, and the primary back on `main`.
pub fn stable_lines() -> Vec<Step> {
    vec![
        Step::rwv(
            &a(),
            &[
                "branch",
                "create",
                "stable",
                "--declared",
                "--set-new-branch",
                "local/remotes/app=stable-app",
                "--set-new-branch",
                "local/remotes/lib=stable-lib",
            ],
        )
        .expect(Exit::Succeeds),
        Step::rwv(&a(), &["switch", "main"]).expect(Exit::Succeeds),
    ]
}

/// One project commit on `main`, `port-me`: `app` two commits forward, `lib`
/// one, the `dependency` `docs` re-pinned one forward, `lib`'s entry and the
/// top-level `history` changed, and a project file added.
pub fn port_me_on_main() -> Vec<Step> {
    let lib_main = scenario_sandbox::declared_branch_text(Repo::Lib, "main");
    vec![
        Step::commit(&a(), Repo::App, "fix-app-1"),
        Step::commit(&a(), Repo::App, "fix-app-2"),
        Step::commit(&a(), Repo::Lib, "fix-lib"),
        Step::commit(&a(), Repo::Docs, "docs-bump"),
        Step::hand_edit(
            &a(),
            Repo::Proj,
            "rwv.toml",
            "[integrations.vscode-workspace]",
            "history = \"linear\"\n\n[integrations.vscode-workspace]",
        ),
        Step::hand_edit(
            &a(),
            Repo::Proj,
            "rwv.toml",
            &lib_main,
            &format!("{lib_main}\nrecurse-submodules = false"),
        ),
        Step::rwv(&a(), &["lock"]).expect(Exit::Succeeds),
        Step::git(&a(), Repo::Proj, &["add", "rwv.toml", "rwv.lock"]),
        Step::commit(&a(), Repo::Proj, "port-me"),
    ]
}

/// The primary on `stable`, and a workweave `p` holding a feature onto it.
pub fn feature_onto_stable() -> Vec<Step> {
    vec![
        Step::rwv(&a(), &["switch", "stable"]).expect(Exit::Succeeds),
        create("p"),
    ]
}

/// The operator's settling of a replayed tip whose text merge put `app`'s
/// `recurse-submodules` under `docs`: the line moved back by hand, then the
/// printed marking of it resolved.
pub fn recurse_moved_back(at: &Place) -> [Step; 3] {
    [
        Step::hand_edit(
            at,
            Repo::Proj,
            "rwv.toml",
            "role = \"owned\"\nrecurse-submodules = false\n",
            "role = \"owned\"\n",
        ),
        Step::hand_edit(
            at,
            Repo::Proj,
            "rwv.toml",
            "role = \"fork\"\n\n[repositories.\"local/remotes/docs\"]",
            "role = \"fork\"\nrecurse-submodules = false\n\n[repositories.\"local/remotes/docs\"]",
        ),
        Step::rwv_printed_holding(at, "git -C ", "add -- rwv.toml").expect(Exit::Succeeds),
    ]
}
