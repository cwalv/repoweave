use crate::invariants::{LockSite, Mode, Record};
use crate::runner::{Claim, Exit, Forge, MergeHow, MissingVerb, Scenario, Step};
use crate::sandbox::{
    declared_branch_text, Machine, Place, Remote, Repo, SecondProject, Seed, RECORDS_BRANCH,
};

fn a() -> Place {
    Place::Primary(Machine::A)
}

fn ww(name: &str) -> Place {
    Place::ww(Machine::A, name)
}

fn create(name: &str) -> Step {
    Step::rwv(&a(), &["workweave", "create", "--project", "proj", name]).expect(Exit::Succeeds)
}

fn scenario(name: &'static str, steps: Vec<Step>) -> Scenario {
    Scenario {
        name,
        seed: Seed::default(),
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
                .pushes(Remote::origin(Repo::App), "main")
                .pushes(Remote::origin(Repo::Proj), "main")
                .expect(Exit::Succeeds)
                .claim(Claim::RemoteContains {
                    repo: Repo::App,
                    branch: "main",
                    mark: "solo-app",
                })
                .claim(Claim::PinContains {
                    site: LockSite::remote_main(Repo::Proj),
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
                    "delete",
                    "--project",
                    "proj",
                    "anon",
                    "--discard-unmerged-commits",
                ],
            )
            .expect(Exit::Succeeds)
            .claim(Claim::WorkweaveGone(ww("anon")))
            .claim(Claim::BackedUp {
                repo: Repo::App,
                mark: "anon-app",
                marked: None,
            }),
            Step::rwv(&a(), &["backup", "list"]).expect(Exit::Succeeds),
        ],
    )
}

/// dc10-store-destroy: a member's clone holding a backup is not destroyed by
/// `member remove --delete`, which names `rwv backup list` and refuses; the
/// operator's `backup drop` is what releases the commits.
pub fn dc10_store_destroy() -> Scenario {
    let app = Repo::App.rel_path();
    scenario(
        "dc10: a clone holding a backup is not destroyed",
        vec![
            create("anon"),
            Step::commit(&ww("anon"), Repo::App, "anon-app"),
            Step::rwv(
                &a(),
                &[
                    "workweave",
                    "delete",
                    "--project",
                    "proj",
                    "anon",
                    "--discard-unmerged-commits",
                ],
            )
            .expect(Exit::Succeeds)
            .claim(Claim::BackedUp {
                repo: Repo::App,
                mark: "anon-app",
                marked: None,
            }),
            Step::rwv(
                &a(),
                &["member", "remove", &app, "--delete", "--project", "proj"],
            )
            .expect(Exit::Fails)
            .claim(Claim::BackedUp {
                repo: Repo::App,
                mark: "anon-app",
                marked: None,
            }),
            Step::rwv(&a(), &["backup", "drop", &app, "--project", "proj"])
                .expect(Exit::Succeeds)
                .may_strand(&[Repo::App]),
        ],
    )
}

/// tombstone-to-backup: a savepoint an earlier release left behind, the only
/// ref to commits it discarded, becomes a backup under `doctor --fix`, and
/// `backup drop` releases it.
pub fn tombstone_to_backup() -> Scenario {
    scenario(
        "tombstone-to-backup",
        vec![
            Step::commit(&a(), Repo::Proj, "tombstoned"),
            Step::git(
                &a(),
                Repo::Proj,
                &["update-ref", "refs/rwv/pre-op/1779769917405921588", "HEAD"],
            ),
            Step::git(&a(), Repo::Proj, &["reset", "--hard", "HEAD~1"]).may_rewrite(&[Repo::Proj]),
            Step::rwv(&a(), &["doctor", "--fix"]).claim(Claim::BackedUp {
                repo: Repo::Proj,
                mark: "tombstoned",
                marked: None,
            }),
            Step::rwv(
                &a(),
                &["backup", "drop", "projects/proj", "--project", "proj"],
            )
            .expect(Exit::Succeeds)
            .may_strand(&[Repo::Proj]),
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
        seed: Seed::default().second_project(SecondProject { branch }),
        steps: vec![
            Step::clone_project(Machine::A, Repo::Proj2).expect(Exit::Undecided),
            Step::commit(&a(), Repo::Lib, "lib-pending"),
            Step::rwv(&a(), &["activate", "proj2"]),
            Step::rwv(&a(), &["activate", "proj"]),
            Step::rwv(&a(), &["workweave", "create", "--project", "proj2", "side"])
                .expect(Exit::Succeeds),
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
                &[
                    "workweave",
                    "create",
                    "--project",
                    "proj",
                    "w2",
                    "--from",
                    ".",
                ],
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

fn feature_branch_in_primary() -> Vec<Step> {
    vec![
        Step::git(&a(), Repo::App, &["switch", "-c", "feat-x"]),
        Step::commit(&a(), Repo::App, "feat-app"),
        Step::git(&a(), Repo::Proj, &["switch", "-c", "feat-x"]),
        Step::rwv(
            &a(),
            &[
                "branch", "set-mode", "feat-x", "feature", "--target", "main",
            ],
        )
        .expect(Exit::Succeeds),
        Step::rwv(&a(), &["update"]).expect(Exit::Succeeds),
        Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
    ]
}

fn push_feat_x(repo: Repo) -> Step {
    Step::git(&a(), repo, &["push", "origin", "feat-x"]).pushes(Remote::origin(repo), "feat-x")
}

fn raw_merge(name: &'static str, main_moves: bool) -> Scenario {
    let mut steps = feature_branch_in_primary();
    steps.extend([
        push_feat_x(Repo::App),
        push_feat_x(Repo::Proj),
        Step::git(&a(), Repo::App, &["switch", "main"]),
        Step::git(&a(), Repo::Proj, &["switch", "main"]),
        Step::rwv(&a(), &["update"]).expect(Exit::Succeeds),
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
            .pushes(Remote::origin(Repo::App), "main")
            .pushes(Remote::origin(Repo::Proj), "main")
            .expect(Exit::Succeeds)
            .claim(Claim::RemoteContains {
                repo: Repo::App,
                branch: "main",
                mark: "feat-app",
            })
            .claim(Claim::PinContains {
                site: LockSite::remote_main(Repo::Proj),
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
        push_feat_x(Repo::App),
        push_feat_x(Repo::Proj),
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
        Step::rwv(&Place::Primary(Machine::C), &["update", "--remote"]),
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
                &declared_branch_text(Repo::Lib, "main"),
                &declared_branch_text(Repo::Lib, "dev"),
            ),
            Step::rwv(&a(), &["update", "--remote"]),
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
                &declared_branch_text(Repo::Lib, "main"),
                &declared_branch_text(Repo::Lib, "nowhere"),
            ),
            Step::rwv(&a(), &["update", "--remote"]),
        ],
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

/// A feature on its own branch in the project repo, `app` and `lib`, whose
/// target `main` moved `app` and relocked after the fork. `docs` stays on
/// `main` and gains a commit nothing locks.
pub fn feature_whose_target_moved() -> Scenario {
    let mut steps = Vec::new();
    for repo in [Repo::App, Repo::Lib, Repo::Proj] {
        steps.push(Step::git(&a(), repo, &["branch", "feat-x"]));
    }
    steps.extend([
        Step::commit(&a(), Repo::App, "main-app"),
        Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
    ]);
    for repo in [Repo::App, Repo::Lib, Repo::Proj] {
        steps.push(Step::git(&a(), repo, &["switch", "feat-x"]));
    }
    steps.extend([
        Step::commit(&a(), Repo::App, "feat-app-1"),
        Step::commit(&a(), Repo::App, "feat-app-2"),
        Step::commit(&a(), Repo::Lib, "feat-lib"),
        Step::commit(&a(), Repo::Proj, "feat-notes"),
        Step::commit(&a(), Repo::Docs, "docs-unlocked"),
        Step::rwv(&a(), &["log", "--against", "main"]).expect(Exit::Succeeds),
        Step::rwv(&a(), &["log", "--against", "main", "--diff"]).expect(Exit::Succeeds),
    ]);
    scenario("a feature whose target moved", steps)
}

/// `feat-x` in `app`, `lib` and the project repo, each holding a commit
/// there; `docs` has no such branch. The weave is on `main`, which `app`
/// moved past the fork.
pub fn feature_branches_in_some_members() -> Scenario {
    let mut steps = Vec::new();
    for (repo, mark) in [
        (Repo::App, "fx-app"),
        (Repo::Lib, "fx-lib"),
        (Repo::Proj, "fx-proj"),
    ] {
        steps.extend([
            Step::git(&a(), repo, &["switch", "-c", "feat-x"]),
            Step::commit(&a(), repo, mark),
            Step::git(&a(), repo, &["switch", "main"]),
        ]);
    }
    steps.push(Step::commit(&a(), Repo::App, "main-after"));
    scenario("a feature branch in some members", steps)
}

fn forge_commits_everywhere() -> Vec<Step> {
    [
        (Repo::Proj, "forge-proj"),
        (Repo::App, "forge-app"),
        (Repo::Lib, "forge-lib"),
        (Repo::Docs, "forge-docs"),
    ]
    .into_iter()
    .map(|(repo, mark)| {
        Step::forge(Forge::Commit {
            repo,
            branch: "main",
            mark,
        })
    })
    .collect()
}

pub fn fetch_moves_nothing() -> Scenario {
    let mut steps = forge_commits_everywhere();
    let mut fetch = Step::rwv(&a(), &["fetch"])
        .expect(Exit::Succeeds)
        .claim(Claim::LocalStateUnchanged(a()));
    for (repo, mark) in [
        (Repo::Proj, "forge-proj"),
        (Repo::App, "forge-app"),
        (Repo::Lib, "forge-lib"),
        (Repo::Docs, "forge-docs"),
    ] {
        fetch = fetch.claim(Claim::Contains {
            at: a(),
            repo,
            rev: "origin/main",
            mark,
        });
    }
    steps.push(fetch);
    scenario("fetch moves nothing", steps)
}

fn project_with_an_incomplete_lock() -> Vec<Step> {
    vec![
        Step::commit(&a(), Repo::App, "unpublished-app"),
        Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::drop_lock_entry(&a(), Repo::Proj, Repo::Docs),
        Step::git(
            &a(),
            Repo::Proj,
            &["commit", "-a", "-m", "lock: without docs"],
        ),
        Step::git(&a(), Repo::Proj, &["push", "origin", "main"])
            .pushes(Remote::origin(Repo::Proj), "main")
            .may_leave_unresolved(&[Repo::App]),
    ]
}

pub fn clone_exit_status() -> Scenario {
    let mut steps = project_with_an_incomplete_lock();
    steps.push(
        Step::new_machine(Machine::B)
            .expect(Exit::Code(3))
            .may_leave_unresolved(&[Repo::App])
            .claim(Claim::OutputContains("local/remotes/docs: pin none"))
            .claim(Claim::OutputContains("local/remotes/app: pin unreachable"))
            .claim(Claim::OnBranch {
                at: Place::Primary(Machine::B),
                repo: Repo::Docs,
                branch: "main",
            }),
    );
    scenario("clone lists what the lock cannot place and exits 3", steps)
}

/// The remote moved past the pins before the clone: every member is born
/// at its pin, and the branch reads as behind the one it takes in from.
pub fn fresh_take_at_pin() -> Scenario {
    let b = Place::Primary(Machine::B);
    let mut steps = forge_commits_everywhere();
    steps.extend([
        Step::new_machine(Machine::B)
            .expect(Exit::Code(0))
            .claim(Claim::AtPin {
                at: b.clone(),
                repo: Repo::App,
            })
            .claim(Claim::AtPin {
                at: b.clone(),
                repo: Repo::Lib,
            })
            .claim(Claim::OnBranch {
                at: b.clone(),
                repo: Repo::App,
                branch: "main",
            }),
        Step::rwv(&b, &["status"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputContains(
                "local/remotes/app: push-branch behind",
            ))
            .claim(Claim::OutputContains(
                "rwv update --remote --repo local/remotes/app",
            )),
    ]);
    scenario(
        "a fresh take is at its pins while the remote is ahead",
        steps,
    )
}

pub fn clone_with_a_complete_lock() -> Scenario {
    scenario(
        "clone with a complete lock exits 0",
        vec![Step::new_machine(Machine::B)
            .expect(Exit::Code(0))
            .claim(Claim::OnBranch {
                at: Place::Primary(Machine::B),
                repo: Repo::App,
                branch: "main",
            })],
    )
}

fn feature_onto(target: &str, as_mode: Mode) -> Record {
    Record::Feature {
        target: target.to_owned(),
        as_mode,
    }
}

const FEATURE_ONTO_ELSEWHERE: &str =
    "mode = \"feature\"\ntarget = \"elsewhere\"\nas = \"declared\"\n";

/// A colleague's remote, added only to fetch their branches, holds a records
/// branch whose entry for `main` clashes with this machine's: it is read as
/// nothing until the remote becomes a push target of some branch here.
pub fn records_foreign_remote() -> Scenario {
    let records_remote_tip = "refs/remotes/colleague/rwv-records";
    scenario(
        "records: a remote added only to fetch",
        vec![
            Step::hand_commit(
                &a(),
                Repo::Proj,
                "colleague-records",
                &[("main", FEATURE_ONTO_ELSEWHERE)],
            ),
            Step::git(
                &a(),
                Repo::Proj,
                &["remote", "add", "colleague", "file:///colleague/proj.git"],
            ),
            Step::git(
                &a(),
                Repo::Proj,
                &[
                    "update-ref",
                    records_remote_tip,
                    "refs/heads/colleague-records",
                ],
            ),
            Step::rwv(&a(), &["branch", "set-mode", "main", "declared"])
                .expect(Exit::Succeeds)
                .claim(Claim::RecordIs {
                    machine: Machine::A,
                    branch: "main",
                    record: Some(Record::Declared),
                }),
            Step::git(
                &a(),
                Repo::Proj,
                &["config", "branch.main.pushRemote", "colleague"],
            ),
            Step::rwv(&a(), &["branch", "list"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains("conflict")),
            Step::git(&a(), Repo::Proj, &["branch", "other"]),
            Step::rwv(&a(), &["branch", "set-mode", "other", "declared"])
                .expect(Exit::Succeeds)
                .claim(Claim::RecordIs {
                    machine: Machine::A,
                    branch: "main",
                    record: Some(Record::Conflict(vec![
                        Record::Declared,
                        feature_onto("elsewhere", Mode::Declared),
                    ])),
                }),
        ],
    )
}

/// A raw commit on the records branch changes one entry: it reads as that
/// entry, and the conflict a later merge stores for it is settled by
/// `branch set-mode`.
pub fn records_hand_commit() -> Scenario {
    scenario(
        "records: a commit by hand",
        vec![
            Step::git(&a(), Repo::Proj, &["branch", "feat-x"]),
            Step::rwv(
                &a(),
                &[
                    "branch", "set-mode", "feat-x", "feature", "--target", "main",
                ],
            )
            .expect(Exit::Succeeds)
            .claim(Claim::RecordIs {
                machine: Machine::A,
                branch: "feat-x",
                record: Some(feature_onto("main", Mode::Declared)),
            }),
            Step::hand_commit(
                &a(),
                Repo::Proj,
                RECORDS_BRANCH,
                &[("feat-x", "mode = \"declared\"\n")],
            ),
            Step::rwv(&a(), &["branch", "list"])
                .expect(Exit::Succeeds)
                .claim(Claim::RecordIs {
                    machine: Machine::A,
                    branch: "feat-x",
                    record: Some(Record::Declared),
                }),
            Step::hand_commit(
                &a(),
                Repo::Proj,
                "their-records",
                &[("feat-x", FEATURE_ONTO_ELSEWHERE)],
            ),
            Step::git(
                &a(),
                Repo::Proj,
                &[
                    "update-ref",
                    "refs/remotes/origin/rwv-records",
                    "refs/heads/their-records",
                ],
            ),
            Step::rwv(&a(), &["branch", "set-mode", "main", "declared"])
                .expect(Exit::Succeeds)
                .claim(Claim::RecordIs {
                    machine: Machine::A,
                    branch: "feat-x",
                    record: Some(Record::Conflict(vec![
                        Record::Declared,
                        feature_onto("elsewhere", Mode::Declared),
                    ])),
                }),
            Step::rwv(&a(), &["branch", "set-mode", "feat-x", "declared"])
                .expect(Exit::Succeeds)
                .claim(Claim::RecordIs {
                    machine: Machine::A,
                    branch: "feat-x",
                    record: Some(Record::Declared),
                }),
        ],
    )
}

/// A raw commit on the records branch writes a field this release does not
/// read for one workweave's branch: only that branch's verbs refuse, and
/// `branch set-mode` settles it.
pub fn records_unreadable_entry() -> Scenario {
    scenario(
        "records: an entry this release cannot read",
        vec![
            create("w"),
            Step::hand_commit(
                &a(),
                Repo::Proj,
                RECORDS_BRANCH,
                &[(
                    "proj--w",
                    "mode = \"declared\"\nwritten-by-a-later-release = 1\n",
                )],
            ),
            Step::rwv(&ww("w"), &["lock"])
                .expect(Exit::Fails)
                .claim(Claim::OutputContains("target unreadable (proj--w)"))
                .claim(Claim::OutputContains("rwv branch set-mode proj--w"))
                .claim(Claim::OutputContains("its commit names no rwv release")),
            Step::rwv(&a(), &["lock"]).expect(Exit::Succeeds),
            Step::rwv(&ww("w"), &["branch", "set-mode", "proj--w", "declared"])
                .expect(Exit::Succeeds)
                .claim(Claim::RecordIs {
                    machine: Machine::A,
                    branch: "proj--w",
                    record: Some(Record::Declared),
                }),
            Step::rwv(&ww("w"), &["update"]).expect(Exit::Succeeds),
            Step::rwv(&ww("w"), &["lock"]).expect(Exit::Succeeds),
        ],
    )
}

pub fn workspace_state_corrupt() -> Scenario {
    scenario(
        "workspace state: a file that does not parse",
        vec![
            Step::truncate_workspace_state(&a()),
            Step::rwv(&a(), &["status"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains(
                    "projects/proj aligned for no branch (workspace state reads as absent",
                ))
                .claim(Claim::OutputContains("alignment project unaligned"))
                .claim(Claim::OutputContains("rwv update")),
            Step::rwv(&a(), &["update"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains("reads as absent")),
            Step::rwv(&a(), &["status"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains("projects/proj aligned for main"))
                .claim(Claim::OutputLacks("reads as absent"))
                .claim(Claim::OutputLacks("project unaligned")),
        ],
    )
}

pub fn status_operator_branches() -> Scenario {
    scenario(
        "status lists the branches nothing accounts for",
        vec![
            Step::git(&a(), Repo::App, &["switch", "-c", "wip"]),
            Step::commit(&a(), Repo::App, "wip-one"),
            Step::commit(&a(), Repo::App, "wip-two"),
            Step::git(&a(), Repo::App, &["switch", "main"]),
            create("feat"),
            Step::commit(&ww("feat"), Repo::App, "feat-app"),
            Step::commit(&a(), Repo::Lib, "lib-main"),
            Step::git(&a(), Repo::Lib, &["switch", "-c", "elsewhere"]),
            Step::rwv(&a(), &["status"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains(
                    "operator branch local/remotes/app  wip  2 unpushed",
                ))
                .claim(Claim::OutputLacks(
                    "operator branch local/remotes/app  proj--feat",
                ))
                .claim(Claim::OutputLacks(
                    "operator branch local/remotes/app  main",
                ))
                .claim(Claim::OutputLacks("operator branch local/remotes/lib")),
            Step::rwv(&a(), &["status", "--json"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains("\"operator_branches\""))
                .claim(Claim::OutputContains("wip-two")),
        ],
    )
}

pub fn activate_no_checkout() -> Scenario {
    let refused_with = |step: Step| {
        step.expect(Exit::Fails)
            .claim(Claim::LocalStateUnchanged(a()))
            .claim(Claim::Presents {
                at: a(),
                project: Repo::Proj,
            })
    };
    Scenario {
        name: "activate moves no checkout",
        seed: Seed::default().second_project(SecondProject { branch: "dev" }),
        steps: vec![
            Step::clone_project(Machine::A, Repo::Proj2).expect(Exit::Succeeds),
            refused_with(Step::rwv(&a(), &["activate", "proj2"]))
                .claim(Claim::OutputContains(
                    "local/remotes/lib: alignment on branch (main)",
                ))
                .claim(Claim::OutputContains("rwv activate proj2 --update"))
                .claim(Claim::OutputLacks("rwv update"))
                .claim(Claim::OnBranch {
                    at: a(),
                    repo: Repo::Lib,
                    branch: "main",
                }),
            Step::hand_edit(&a(), Repo::Lib, "README", "lib\n", "lib edited\n"),
            refused_with(Step::rwv(&a(), &["activate", "proj2"]))
                .claim(Claim::OutputContains("local/remotes/lib: edits dirty"))
                .claim(Claim::OutputContains("README"))
                .claim(Claim::OutputContains("stash push"))
                .claim(Claim::OutputLacks("--update"))
                .claim(Claim::OutputLacks("alignment")),
            refused_with(Step::rwv(&a(), &["activate", "proj2", "--update"]))
                .claim(Claim::OutputContains("local/remotes/lib: edits dirty"))
                .claim(Claim::OutputLacks("alignment")),
            Step::git(&a(), Repo::Lib, &["checkout", "--", "README"]),
            Step::rwv(&a(), &["activate", "proj2", "--update"])
                .expect(Exit::Succeeds)
                .claim(Claim::OnBranch {
                    at: a(),
                    repo: Repo::Lib,
                    branch: "dev",
                })
                .claim(Claim::Presents {
                    at: a(),
                    project: Repo::Proj2,
                }),
        ],
    }
}

pub fn activate_leaves_what_the_outgoing_project_holds() -> Scenario {
    let hand_declared = |from: &str, to: &str| {
        Step::hand_edit(
            &a(),
            Repo::Proj,
            "rwv.toml",
            &declared_branch_text(Repo::Lib, from),
            &declared_branch_text(Repo::Lib, to),
        )
    };
    Scenario {
        name: "activate --update moves a member the outgoing project is aligned on",
        seed: Seed::default().second_project(SecondProject { branch: "dev" }),
        steps: vec![
            Step::clone_project(Machine::A, Repo::Proj2).expect(Exit::Succeeds),
            Step::git(&a(), Repo::Lib, &["switch", "-c", "wip"]),
            Step::commit(&a(), Repo::Lib, "wip-one"),
            hand_declared("main", "wip"),
            Step::rwv(&a(), &["update"]).expect(Exit::Succeeds),
            hand_declared("wip", "main"),
            Step::rwv(&a(), &["activate", "proj2"])
                .expect(Exit::Fails)
                .claim(Claim::LocalStateUnchanged(a()))
                .claim(Claim::OutputContains(
                    "local/remotes/lib: alignment on branch with stray commits (wip)",
                ))
                .claim(Claim::OutputContains("wip-one"))
                .claim(Claim::OutputContains("rwv activate proj2 --update")),
            Step::rwv(&a(), &["activate", "proj2", "--update"])
                .expect(Exit::Succeeds)
                .claim(Claim::OnBranch {
                    at: a(),
                    repo: Repo::Lib,
                    branch: "dev",
                })
                .claim(Claim::Contains {
                    at: a(),
                    repo: Repo::Lib,
                    rev: "wip",
                    mark: "wip-one",
                })
                .claim(Claim::Presents {
                    at: a(),
                    project: Repo::Proj2,
                }),
        ],
    }
}

pub fn activate_over_aligned_shared_member() -> Scenario {
    Scenario {
        name: "activate over a shared member aligned for the project",
        seed: Seed::default().second_project(SecondProject { branch: "main" }),
        steps: vec![
            Step::clone_project(Machine::A, Repo::Proj2).expect(Exit::Succeeds),
            Step::rwv(&a(), &["update", "--project", "proj2"])
                .expect(Exit::Succeeds)
                .claim(Claim::LocalStateUnchanged(a())),
            Step::rwv(&a(), &["activate", "proj2"])
                .expect(Exit::Succeeds)
                .claim(Claim::LocalStateUnchanged(a()))
                .claim(Claim::Presents {
                    at: a(),
                    project: Repo::Proj2,
                }),
        ],
    }
}

pub fn inactive_project_moves_no_shared_checkout() -> Scenario {
    let refused = |args: &[&str]| {
        Step::rwv(&a(), args)
            .expect(Exit::Fails)
            .claim(Claim::LocalStateUnchanged(a()))
            .claim(Claim::Presents {
                at: a(),
                project: Repo::Proj,
            })
    };
    Scenario {
        name: "a verb for an inactive project moves no shared checkout",
        seed: Seed::default().second_project(SecondProject { branch: "dev" }),
        steps: vec![
            Step::clone_project(Machine::A, Repo::Proj2).expect(Exit::Succeeds),
            refused(&["update", "--project", "proj2"])
                .claim(Claim::OutputContains(
                    "local/remotes/lib: alignment on branch (main)",
                ))
                .claim(Claim::OutputContains("rwv activate proj2 --update"))
                .claim(Claim::OutputLacks("\n    rwv update\n")),
            refused(&["update", "--remote", "--project", "proj2"])
                .claim(Claim::OutputContains(
                    "local/remotes/lib: alignment on branch (main)",
                ))
                .claim(Claim::OutputContains("rwv activate proj2 --update")),
            refused(&["pull", "--project", "proj2"])
                .claim(Claim::OutputContains(
                    "local/remotes/lib: alignment on branch (main)",
                ))
                .claim(Claim::OutputContains("rwv activate proj2 --update")),
            Step::hand_edit(&a(), Repo::Lib, "README", "lib\n", "lib edited\n"),
            refused(&["update", "--project", "proj2"])
                .claim(Claim::OutputContains("local/remotes/lib: edits dirty"))
                .claim(Claim::OutputLacks("--update")),
        ],
    }
}
