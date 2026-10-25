use crate::invariants::LockSite;
use crate::runner::{Claim, Exit, Forge, MergeHow, MissingVerb, Scenario, Step};
use crate::sandbox::{Machine, Place, Repo, SecondProject};

fn a() -> Place {
    Place::Primary(Machine::A)
}

fn ww(name: &str) -> Place {
    Place::ww(Machine::A, name)
}

fn create(name: &str) -> Step {
    Step::rwv(&a(), &["workweave", "proj", "create", name]).expect(Exit::Succeeds)
}

fn scenario(name: &'static str, steps: Vec<Step>) -> Scenario {
    Scenario {
        name,
        second_project: None,
        steps,
    }
}

pub fn solo_workweave() -> Scenario {
    scenario(
        "solo workweave",
        vec![
            create("solo"),
            Step::commit(&ww("solo"), Repo::App, "solo-app"),
            Step::rwv(&ww("solo"), &["sync-to", "--retire"])
                .expect(Exit::Succeeds)
                .claim(Claim::WorkweaveGone(ww("solo")))
                .claim(Claim::Contains {
                    at: a(),
                    repo: Repo::App,
                    rev: "main",
                    mark: "solo-app",
                })
                .claim(Claim::PinContains {
                    site: LockSite::Checkout(a(), Repo::Proj, "main".to_owned()),
                    member: Repo::App,
                    mark: "solo-app",
                }),
            Step::rwv(&a(), &["push"])
                .expect(Exit::Succeeds)
                .claim(Claim::RemoteContains {
                    repo: Repo::App,
                    branch: "main",
                    mark: "solo-app",
                })
                .claim(Claim::PinContains {
                    site: LockSite::RemoteMain(Repo::Proj),
                    member: Repo::App,
                    mark: "solo-app",
                }),
        ],
    )
}

fn two_member_feature(name: &str) -> Vec<Step> {
    vec![
        create(name),
        Step::commit(&ww(name), Repo::App, "pr-app"),
        Step::commit(&ww(name), Repo::Lib, "pr-lib"),
    ]
}

pub fn cross_repo_pr() -> Scenario {
    let mut steps = two_member_feature("pr");
    steps.push(Step::pending(
        "publish app and lib for review",
        MissingVerb::Publish,
    ));
    scenario("cross-repo PR from a workweave", steps)
}

pub fn cross_repo_pr_fork() -> Scenario {
    let mut steps = two_member_feature("pr");
    steps.push(Step::pending(
        "publish to the fork remote",
        MissingVerb::Publish,
    ));
    scenario("cross-repo PR, fork variant", steps)
}

pub fn resume_on_machine_b() -> Scenario {
    let mut steps = two_member_feature("pr");
    steps.push(Step::pending(
        "publish before resuming elsewhere",
        MissingVerb::Publish,
    ));
    scenario("resume on machine B", steps)
}

pub fn feature_in_primary_while_main_moves() -> Scenario {
    scenario(
        "feature in the primary while main moves",
        vec![Step::pending(
            "start a feature branch in the primary",
            MissingVerb::WeaveBranch,
        )],
    )
}

pub fn workweave_without_b_then_pr() -> Scenario {
    scenario(
        "workweave without -b, then a PR",
        vec![
            create("anon"),
            Step::commit(&ww("anon"), Repo::App, "anon-app"),
            Step::pending("name the workweave's branch", MissingVerb::Rename),
        ],
    )
}

pub fn workweave_without_b_given_up_before_publishing() -> Scenario {
    scenario(
        "workweave without -b, given up before publishing",
        vec![
            create("anon"),
            Step::commit(&ww("anon"), Repo::App, "anon-app"),
            Step::rwv(
                &a(),
                &[
                    "workweave",
                    "proj",
                    "delete",
                    "anon",
                    "--discard-unmerged-commits",
                ],
            ),
        ],
    )
}

pub fn workweave_without_b_given_up_after_publishing() -> Scenario {
    scenario(
        "workweave without -b, given up after publishing",
        vec![
            create("anon"),
            Step::commit(&ww("anon"), Repo::App, "anon-app"),
            Step::pending("publish before giving up", MissingVerb::Publish),
        ],
    )
}

pub fn feature_off_stable() -> Scenario {
    scenario(
        "feature off stable",
        vec![Step::pending(
            "create stable holding lib behind",
            MissingVerb::WeaveBranch,
        )],
    )
}

pub fn permanent_seat() -> Scenario {
    scenario(
        "permanent seat",
        vec![
            create("seat"),
            Step::commit(&ww("seat"), Repo::App, "seat-1"),
            Step::rwv(&ww("seat"), &["sync-to"])
                .expect(Exit::Succeeds)
                .claim(Claim::Contains {
                    at: a(),
                    repo: Repo::App,
                    rev: "main",
                    mark: "seat-1",
                }),
            create("other"),
            Step::commit(&ww("other"), Repo::App, "elsewhere-1"),
            Step::rwv(&ww("other"), &["sync-to", "--retire"]).expect(Exit::Succeeds),
            Step::commit(&ww("seat"), Repo::App, "seat-2"),
            Step::rwv(&ww("seat"), &["lock"]).expect(Exit::Succeeds),
            Step::git(&ww("seat"), Repo::Proj, &["add", "rwv.lock"]),
            Step::commit(&ww("seat"), Repo::Proj, "seat-notes"),
            Step::rwv(&ww("seat"), &["sync-to"])
                .expect(Exit::Succeeds)
                .claim(Claim::Contains {
                    at: a(),
                    repo: Repo::App,
                    rev: "main",
                    mark: "elsewhere-1",
                })
                .claim(Claim::Contains {
                    at: ww("seat"),
                    repo: Repo::App,
                    rev: "HEAD",
                    mark: "elsewhere-1",
                }),
            Step::pending(
                "land a declaration change from the seat",
                MissingVerb::Declare,
            ),
        ],
    )
}

pub fn review_colleague_fix() -> Scenario {
    scenario(
        "review a colleague's fix-123",
        vec![
            Step::forge(Forge::Commit {
                repo: Repo::App,
                branch: "fix-123",
                mark: "colleague-fix",
            }),
            Step::pending("build fix-123 in a review workweave", MissingVerb::Review),
        ],
    )
}

pub fn stable_end_to_end() -> Scenario {
    scenario(
        "stable end to end",
        vec![Step::pending(
            "create stable holding lib behind",
            MissingVerb::WeaveBranch,
        )],
    )
}

pub fn main_change_into_stable() -> Scenario {
    scenario(
        "bring a main change into stable",
        vec![Step::pending("create stable", MissingVerb::WeaveBranch)],
    )
}

fn shared_member(name: &'static str, branch: &'static str) -> Scenario {
    let side = Place::Workweave(Machine::A, Repo::Proj2, "side".to_owned());
    Scenario {
        name,
        second_project: Some(SecondProject { branch }),
        steps: vec![
            Step::fetch_project(Machine::A, Repo::Proj2).expect(Exit::Undecided),
            Step::commit(&a(), Repo::Lib, "lib-pending"),
            Step::rwv(&a(), &["activate", "proj2"]),
            Step::rwv(&a(), &["activate", "proj"]),
            Step::rwv(&a(), &["workweave", "proj2", "create", "side"]).expect(Exit::Succeeds),
            Step::commit(&side, Repo::Lib, "side-lib"),
            Step::rwv(&side, &["sync-to", "--retire"]),
        ],
    }
}

pub fn shared_member_declared_dev() -> Scenario {
    shared_member("shared member, proj2 declares lib on dev", "dev")
}

pub fn shared_member_pinned_behind() -> Scenario {
    shared_member("shared member, proj2 pins lib behind on main", "main")
}

pub fn nested_workweaves() -> Scenario {
    scenario(
        "nested workweaves",
        vec![Step::pending(
            "create w1 with -b w1",
            MissingVerb::WeaveBranch,
        )],
    )
}

pub fn nested_workweaves_without_naming() -> Scenario {
    let w1_root = "../proj--w1";
    scenario(
        "nested workweaves, unnamed: sync both ways, retire child, land parent",
        vec![
            create("w1"),
            Step::rwv(
                &ww("w1"),
                &["workweave", "proj", "create", "w2", "--from", "."],
            )
            .expect(Exit::Succeeds),
            Step::commit(&ww("w2"), Repo::App, "w2-app"),
            Step::rwv(&ww("w2"), &["sync-to"])
                .expect(Exit::Succeeds)
                .claim(Claim::Contains {
                    at: ww("w1"),
                    repo: Repo::App,
                    rev: "HEAD",
                    mark: "w2-app",
                }),
            Step::commit(&ww("w1"), Repo::Lib, "w1-lib"),
            Step::rwv(&ww("w1"), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::rwv(&ww("w2"), &["sync", w1_root])
                .expect(Exit::Succeeds)
                .claim(Claim::Contains {
                    at: ww("w2"),
                    repo: Repo::Lib,
                    rev: "HEAD",
                    mark: "w1-lib",
                }),
            Step::rwv(&ww("w2"), &["sync-to", "--retire"])
                .expect(Exit::Succeeds)
                .claim(Claim::WorkweaveGone(ww("w2"))),
            Step::rwv(&ww("w1"), &["sync-to", "--retire"])
                .expect(Exit::Succeeds)
                .claim(Claim::WorkweaveGone(ww("w1")))
                .claim(Claim::Contains {
                    at: a(),
                    repo: Repo::App,
                    rev: "main",
                    mark: "w2-app",
                })
                .claim(Claim::Contains {
                    at: a(),
                    repo: Repo::Lib,
                    rev: "main",
                    mark: "w1-lib",
                }),
        ],
    )
}

pub fn origin_and_base_edge_cases() -> Scenario {
    scenario(
        "origin and base edge cases",
        vec![Step::pending(
            "read the weave branch's origin record",
            MissingVerb::BaseRecord,
        )],
    )
}

fn feature_branch_in_primary() -> Vec<Step> {
    vec![
        Step::git(&a(), Repo::App, &["switch", "-c", "feat-x"]),
        Step::commit(&a(), Repo::App, "feat-app"),
        Step::git(&a(), Repo::Proj, &["switch", "-c", "feat-x"]),
        Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
    ]
}

fn raw_merge(name: &'static str, main_moves: bool) -> Scenario {
    let mut steps = feature_branch_in_primary();
    steps.extend([
        Step::git(&a(), Repo::App, &["push", "origin", "feat-x"]),
        Step::git(&a(), Repo::Proj, &["push", "origin", "feat-x"]),
        Step::git(&a(), Repo::App, &["switch", "main"]),
        Step::git(&a(), Repo::Proj, &["switch", "main"]),
    ]);
    if main_moves {
        steps.extend([
            Step::commit(&a(), Repo::App, "main-moves"),
            Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
        ]);
    }
    steps.extend([
        Step::git(&a(), Repo::Proj, &["merge", "--no-edit", "feat-x"]),
        Step::git(&a(), Repo::App, &["merge", "--no-edit", "feat-x"]),
        Step::rwv(&a(), &["lock", "--commit"]),
        Step::rwv(&a(), &["push"])
            .expect(Exit::Succeeds)
            .claim(Claim::RemoteContains {
                repo: Repo::App,
                branch: "main",
                mark: "feat-app",
            })
            .claim(Claim::PinContains {
                site: LockSite::RemoteMain(Repo::Proj),
                member: Repo::App,
                mark: "feat-app",
            }),
        Step::forge(Forge::DeleteBranch {
            repo: Repo::App,
            branch: "feat-x",
        }),
        Step::forge(Forge::DeleteBranch {
            repo: Repo::Proj,
            branch: "feat-x",
        }),
    ]);
    scenario(name, steps)
}

pub fn raw_merge_real() -> Scenario {
    raw_merge("raw merge, real merge", true)
}

pub fn raw_merge_fast_forward() -> Scenario {
    raw_merge("raw merge, fast-forward", false)
}

pub fn detached_head_in_workweave() -> Scenario {
    scenario(
        "detached HEAD with two commits in a workweave",
        vec![
            create("det"),
            Step::git(&ww("det"), Repo::App, &["switch", "--detach"]),
            Step::commit(&ww("det"), Repo::App, "det-1"),
            Step::commit(&ww("det"), Repo::App, "det-2"),
            Step::rwv(&ww("det"), &["sync-to"]),
        ],
    )
}

pub fn switch_c_wip_in_workweave() -> Scenario {
    scenario(
        "git switch -c wip in a workweave member",
        vec![
            create("wip"),
            Step::git(&ww("wip"), Repo::App, &["switch", "-c", "wip"]),
            Step::commit(&ww("wip"), Repo::App, "wip-1"),
            Step::rwv(&ww("wip"), &["lock"]),
            Step::rwv(&ww("wip"), &["push"]).expect(Exit::Fails),
            Step::rwv(&ww("wip"), &["sync-to"]),
        ],
    )
}

pub fn raw_switch_in_primary_project() -> Scenario {
    let mut steps = feature_branch_in_primary();
    steps.extend([
        Step::git(&a(), Repo::App, &["switch", "main"]),
        Step::git(&a(), Repo::Proj, &["switch", "main"]),
        Step::git(&a(), Repo::Proj, &["switch", "feat-x"]),
        Step::rwv(&a(), &["status"]),
    ]);
    scenario("raw git switch feat-x in the primary's project repo", steps)
}

pub fn project_pr_merged_before_members() -> Scenario {
    let mut steps = feature_branch_in_primary();
    steps.extend([
        Step::git(&a(), Repo::App, &["push", "origin", "feat-x"]),
        Step::git(&a(), Repo::Proj, &["push", "origin", "feat-x"]),
        Step::forge(Forge::Merge {
            repo: Repo::Proj,
            branch: "feat-x",
            how: MergeHow::Merge,
        }),
        Step::new_machine(Machine::C),
        Step::forge(Forge::Merge {
            repo: Repo::App,
            branch: "feat-x",
            how: MergeHow::Squash,
        }),
        Step::rwv(&Place::Primary(Machine::C), &["update"]),
    ]);
    scenario("project PR merged before its member PRs", steps)
}

pub fn hand_edit_declaration_to_existing_branch() -> Scenario {
    scenario(
        "hand edit: lib declared on a branch that exists",
        vec![
            Step::hand_edit(
                &a(),
                Repo::Proj,
                "rwv.toml",
                "lib.git\"\nversion = \"main\"",
                "lib.git\"\nversion = \"dev\"",
            ),
            Step::rwv(&a(), &["update"]),
        ],
    )
}

pub fn hand_edit_declaration_to_missing_branch() -> Scenario {
    scenario(
        "hand edit: lib declared on a branch that exists nowhere",
        vec![
            Step::hand_edit(
                &a(),
                Repo::Proj,
                "rwv.toml",
                "lib.git\"\nversion = \"main\"",
                "lib.git\"\nversion = \"nowhere\"",
            ),
            Step::rwv(&a(), &["update"]),
        ],
    )
}

pub fn hand_edit_origin_record() -> Scenario {
    scenario(
        "hand edit: the weave branch's origin record",
        vec![Step::pending(
            "hand-edit the origin record",
            MissingVerb::BaseRecord,
        )],
    )
}

pub fn worktree_add_force_on_weave_branch() -> Scenario {
    let stray = "../../../../stray-app";
    scenario(
        "git worktree add --force on a member's weave branch",
        vec![
            create("wt"),
            Step::git(
                &a(),
                Repo::App,
                &["worktree", "add", "--force", stray, "proj--wt"],
            ),
            Step::git(
                &a(),
                Repo::App,
                &["-C", stray, "commit", "--allow-empty", "-m", "stray-1"],
            ),
            Step::rwv(&ww("wt"), &["sync-to", "--retire"]),
        ],
    )
}

pub fn cherry_pick_onto_stable() -> Scenario {
    scenario(
        "cherry-pick onto stable a commit adding a member",
        vec![Step::pending("create stable", MissingVerb::WeaveBranch)],
    )
}

pub fn pr_closed_unmerged() -> Scenario {
    scenario(
        "a PR closed unmerged",
        vec![
            create("closed"),
            Step::commit(&ww("closed"), Repo::App, "closed-app"),
            Step::pending("publish for review", MissingVerb::Publish),
        ],
    )
}
