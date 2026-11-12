use crate::invariants::{LockSite, Mode, Record};
use crate::runner::{
    Claim, Exit, Forge, IndexAt, MarkerState, MergeHow, MissingVerb, Scenario, Step, Tamper,
};
use crate::sandbox::{
    declared_branch_text, Machine, Place, Remote, Repo, SecondProject, Seed, RECORDS_BRANCH,
};
use repoweave::manifest::Role;

fn a() -> Place {
    Place::Primary(Machine::A)
}

fn ww(name: &str) -> Place {
    Place::ww(Machine::A, name)
}

fn create(name: &str) -> Step {
    Step::rwv(&a(), &["workweave", "create", "--project", "proj", name]).expect(Exit::Succeeds)
}

/// A push of the weave branch the project repo is on publishes its record to
/// the project's origin, creating the records branch there on the first.
fn publishes_records(step: Step) -> Step {
    step.pushes(Remote::origin(Repo::Proj), RECORDS_BRANCH)
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
            publishes_records(Step::rwv(&a(), &["push"]))
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

/// `feat-x` in the primary holding a commit in `app` and one in `lib`, locked.
fn two_member_feature_in_primary() -> Vec<Step> {
    vec![
        Step::git(&a(), Repo::App, &["switch", "-c", "feat-x"]),
        Step::commit(&a(), Repo::App, "pr-app"),
        Step::git(&a(), Repo::Lib, &["switch", "-c", "feat-x"]),
        Step::commit(&a(), Repo::Lib, "pr-lib"),
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

fn push_feat_x_everywhere(step: Step) -> Step {
    publishes_records(step)
        .pushes(Remote::origin(Repo::App), "feat-x")
        .pushes(Remote::origin(Repo::Lib), "feat-x")
        .pushes(Remote::origin(Repo::Proj), "feat-x")
}

fn feat_x_record() -> Option<Record> {
    Some(feature_onto("main", Mode::Declared))
}

/// A cross-repo PR: one `push` publishes the members' branches, the record and
/// the project branch; an amend of `app` and `lib` is republished only by a
/// named, leased `--overwrite` per repo, which keeps the replaced tip as a
/// backup.
pub fn cross_repo_pr() -> Scenario {
    let mut steps = two_member_feature_in_primary();
    steps.extend([push_feat_x_everywhere(Step::rwv(&a(), &["push"]))
        .expect(Exit::Succeeds)
        .claim(Claim::RemoteContains {
            repo: Repo::App,
            branch: "feat-x",
            mark: "pr-app",
        })
        .claim(Claim::RemoteContains {
            repo: Repo::Lib,
            branch: "feat-x",
            mark: "pr-lib",
        })
        .claim(Claim::RemoteRecordIs {
            branch: "feat-x",
            record: feat_x_record(),
        })]);
    steps.extend(amend_both());
    steps.extend([
        Step::rwv(&a(), &["push"])
            .expect(Exit::Fails)
            .claim(Claim::OutputContains(
                "local/remotes/app: push-branch diverged",
            ))
            .claim(Claim::OutputContains(
                "local/remotes/lib: push-branch diverged",
            ))
            .claim(Claim::OutputLacks("--overwrite")),
        Step::rwv(&a(), &["push", "--overwrite", "local/remotes/app"])
            .expect(Exit::Fails)
            .claim(Claim::OutputContains(
                "local/remotes/lib: push-branch diverged",
            ))
            .claim(Claim::OutputLacks(
                "local/remotes/app: push-branch diverged",
            )),
        Step::rwv(
            &a(),
            &[
                "push",
                "--overwrite",
                "local/remotes/app",
                "--overwrite",
                "local/remotes/lib",
            ],
        )
        .overwrites(Remote::origin(Repo::App), "feat-x")
        .overwrites(Remote::origin(Repo::Lib), "feat-x")
        .pushes(Remote::origin(Repo::Proj), "feat-x")
        .expect(Exit::Succeeds)
        .claim(Claim::BackedUp {
            repo: Repo::App,
            mark: "pr-app",
            marked: None,
        })
        .claim(Claim::BackedUp {
            repo: Repo::Lib,
            mark: "pr-lib",
            marked: None,
        }),
    ]);
    scenario("cross-repo PR from a workweave", steps)
}

/// `app` and `lib` each amended after review, and their pins re-pinned by name.
fn amend_both() -> Vec<Step> {
    let mut steps = Vec::new();
    for repo in [Repo::App, Repo::Lib] {
        steps.push(Step::git(
            &a(),
            repo,
            &["commit", "--amend", "-m", "amended after review"],
        ));
        steps.push(
            Step::rwv(&a(), &["lock", "--repo", &repo.rel_path(), "--commit"])
                .expect(Exit::Succeeds)
                .may_rewind(&[repo]),
        );
    }
    steps
}

/// The fork variant: the members publish to a `fork` remote their push target
/// names, and the pin gate judges each pin by that remote's branches.
pub fn cross_repo_pr_fork() -> Scenario {
    let mut steps = vec![
        Step::add_remote(&a(), Repo::App, "fork", Remote::fork(Repo::App)),
        Step::add_remote(&a(), Repo::Lib, "fork", Remote::fork(Repo::Lib)),
        Step::git(&a(), Repo::App, &["config", "remote.pushDefault", "fork"]),
        Step::git(&a(), Repo::Lib, &["config", "remote.pushDefault", "fork"]),
    ];
    steps.extend(two_member_feature_in_primary());
    steps.push(
        publishes_records(Step::rwv(&a(), &["push"]))
            .pushes(Remote::fork(Repo::App), "feat-x")
            .pushes(Remote::fork(Repo::Lib), "feat-x")
            .pushes(Remote::origin(Repo::Proj), "feat-x")
            .may_leave_unresolved(&[Repo::App, Repo::Lib])
            .expect(Exit::Succeeds)
            .claim(Claim::RemoteLacks {
                repo: Repo::App,
                branch: "feat-x",
            }),
    );
    Scenario {
        name: "cross-repo PR, fork variant",
        seed: Seed::default().forks(),
        steps,
    }
}

/// The published branch is what machine B resumes from; opening it there is
/// the `workweave create --branch` of a later release.
pub fn resume_on_machine_b() -> Scenario {
    let mut steps = two_member_feature_in_primary();
    steps.extend([
        push_feat_x_everywhere(Step::rwv(&a(), &["push"])).expect(Exit::Succeeds),
        Step::new_machine(Machine::B),
        Step::pending("open feat-x on machine B", MissingVerb::WeaveBranch),
    ]);
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
            Step::rwv(&ww("anon"), &["push"])
                .expect(Exit::Fails)
                .claim(Claim::OutputContains("rwv branch rename <new>")),
            Step::rwv(&ww("anon"), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::rwv(&ww("anon"), &["branch", "rename", "fix-anon"])
                .expect(Exit::Succeeds)
                .claim(Claim::OnBranch {
                    at: ww("anon"),
                    repo: Repo::App,
                    branch: "fix-anon",
                })
                .claim(Claim::RecordIs {
                    machine: Machine::A,
                    branch: "proj--anon",
                    record: None,
                })
                .claim(Claim::RecordIs {
                    machine: Machine::A,
                    branch: "fix-anon",
                    record: Some(feature_onto("main", Mode::Declared)),
                }),
            publishes_records(Step::rwv(&ww("anon"), &["push"]))
                .pushes(Remote::origin(Repo::App), "fix-anon")
                .pushes(Remote::origin(Repo::Proj), "fix-anon")
                .expect(Exit::Succeeds)
                .claim(Claim::RemoteContains {
                    repo: Repo::App,
                    branch: "fix-anon",
                    mark: "anon-app",
                })
                .claim(Claim::RemoteRecordIs {
                    branch: "fix-anon",
                    record: Some(feature_onto("main", Mode::Declared)),
                })
                .claim(Claim::RemoteRecordIs {
                    branch: "proj--anon",
                    record: None,
                }),
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
                    "--discard-unlanded",
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
                    "--discard-unlanded",
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

/// A workweave made without a branch of its own carries a minted name that is
/// never published: `push` refuses it, offering the rename that names it; once
/// named and published, giving the workweave up takes the published branches
/// with it by the named repos.
pub fn workweave_without_b_given_up_after_publishing() -> Scenario {
    scenario(
        "workweave without -b, given up after publishing",
        vec![
            create("anon"),
            Step::commit(&ww("anon"), Repo::App, "anon-app"),
            Step::rwv(&ww("anon"), &["push"])
                .expect(Exit::Fails)
                .claim(Claim::OutputContains(
                    "projects/proj: name minted (proj--anon)",
                ))
                .claim(Claim::OutputContains("rwv branch rename <new>")),
            Step::rwv(&ww("anon"), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::rwv(&ww("anon"), &["branch", "rename", "fix-anon"]).expect(Exit::Succeeds),
            publishes_records(Step::rwv(&ww("anon"), &["push"]))
                .pushes(Remote::origin(Repo::App), "fix-anon")
                .pushes(Remote::origin(Repo::Proj), "fix-anon")
                .expect(Exit::Succeeds),
            Step::rwv(
                &a(),
                &[
                    "workweave",
                    "delete",
                    "--project",
                    "proj",
                    "anon",
                    "--discard-unlanded",
                    "--delete-remote-unlanded",
                    "local/remotes/app",
                    "--delete-remote-unlanded",
                    "projects/proj",
                ],
            )
            .deletes(Remote::origin(Repo::App), "fix-anon")
            .deletes(Remote::origin(Repo::Proj), "fix-anon")
            .expect(Exit::Succeeds)
            .claim(Claim::WorkweaveGone(ww("anon")))
            .claim(Claim::RemoteLacks {
                repo: Repo::App,
                branch: "fix-anon",
            })
            .claim(Claim::RemoteLacks {
                repo: Repo::Proj,
                branch: "fix-anon",
            })
            .claim(Claim::BackedUp {
                repo: Repo::App,
                mark: "anon-app",
                marked: None,
            }),
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
        publishes_records(Step::rwv(&a(), &["push"]))
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

/// A PR closed unmerged: the feature was published, then its remote branches
/// were deleted; the next `push` reports each branch gone and publishes none
/// of them again.
pub fn pr_closed_unmerged() -> Scenario {
    let mut steps = two_member_feature_in_primary();
    steps.extend([
        push_feat_x_everywhere(Step::rwv(&a(), &["push"])).expect(Exit::Succeeds),
        Step::forge(Forge::DeleteBranch {
            repo: Repo::App,
            branch: "feat-x",
        })
        .may_leave_unresolved(&[Repo::App]),
        Step::forge(Forge::DeleteBranch {
            repo: Repo::Lib,
            branch: "feat-x",
        })
        .may_leave_unresolved(&[Repo::Lib]),
        Step::forge(Forge::DeleteBranch {
            repo: Repo::Proj,
            branch: "feat-x",
        }),
        Step::rwv(&a(), &["push"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputContains("push-branch gone"))
            .claim(Claim::RemoteLacks {
                repo: Repo::App,
                branch: "feat-x",
            })
            .claim(Claim::RemoteLacks {
                repo: Repo::Proj,
                branch: "feat-x",
            }),
    ]);
    scenario("a PR closed unmerged", steps)
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

/// `proj` holds a `lib` commit no lock pins and nothing remote holds; `proj2`
/// locks over it. The relock says so to `proj2`, and `proj`'s own status then
/// reads the commit as held by `proj2`'s lock, not as unpinned.
pub fn proj2_relock_over_pending() -> Scenario {
    Scenario {
        name: "proj2 relocks over another project's pending commit",
        seed: Seed::default().second_project(SecondProject { branch: "main" }),
        steps: vec![
            Step::clone_project(Machine::A, Repo::Proj2).expect(Exit::Succeeds),
            Step::commit(&a(), Repo::Lib, "lib-pending"),
            Step::rwv(&a(), &["status"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains("local/remotes/lib: pin unpinned"))
                .claim(Claim::OutputLacks("pinned-by")),
            Step::rwv(&a(), &["lock", "--commit", "--project", "proj2"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains(
                    "local/remotes/lib: pins 1 unpushed commit(s) beyond",
                ))
                .claim(Claim::OutputContains(
                    "which the lock of project proj does not pin",
                ))
                .claim(Claim::PinContains {
                    site: LockSite::Checkout(a(), Repo::Proj2, "main".to_owned()),
                    member: Repo::Lib,
                    mark: "lib-pending",
                }),
            Step::rwv(&a(), &["status"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains("local/remotes/lib: pin ahead"))
                .claim(Claim::OutputContains("pinned-by: proj2 main"))
                .claim(Claim::OutputLacks("pin unpinned")),
        ],
    }
}

/// A `lib` commit only a fork remote holds is pinned, and the pin is
/// published without it. A clone elsewhere cannot reach the pin, and the
/// remote it lacks is the first thing the report offers.
pub fn fork_remote_missing() -> Scenario {
    Scenario {
        name: "a pin only a fork remote holds, that remote absent",
        seed: Seed::default().forks(),
        steps: vec![
            Step::add_remote(&a(), Repo::Lib, "fork", Remote::fork(Repo::Lib)),
            Step::commit(&a(), Repo::Lib, "fork-only"),
            Step::git(&a(), Repo::Lib, &["push", "fork", "main"])
                .pushes(Remote::fork(Repo::Lib), "main"),
            Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::git(&a(), Repo::Proj, &["push", "origin", "main"])
                .pushes(Remote::origin(Repo::Proj), "main")
                .may_leave_unresolved(&[Repo::Lib]),
            Step::new_machine(Machine::B)
                .expect(Exit::Code(3))
                .may_leave_unresolved(&[Repo::Lib])
                .claim(Claim::OutputContains("local/remotes/lib: pin unreachable"))
                .claim(Claim::OutputContains(
                    "choices:\n  choose where it pushes and pulls",
                )),
        ],
    }
}

fn b() -> Place {
    Place::Primary(Machine::B)
}

fn origin_records() -> Remote {
    Remote::origin(Repo::Proj)
}

/// dc03-roles-bound-push: a `dependency` member holding a commit its lock
/// pins is pushed only when named, and the project branch waits for the pin
/// the url remote's branches do not yet reach.
pub fn dc03_roles_bound_push() -> Scenario {
    let docs = Repo::Docs.rel_path();
    Scenario {
        name: "dc03: roles bound push",
        seed: Seed::default().role(Repo::Docs, Role::Dependency),
        steps: vec![
            Step::commit(&a(), Repo::Docs, "docs-dep"),
            Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::rwv(&a(), &["push"])
                .expect(Exit::Fails)
                .claim(Claim::OutputContains(
                    "local/remotes/docs: publish withheld",
                ))
                .claim(Claim::OutputContains(
                    "reached by no remote-tracking branch",
                ))
                .claim(Claim::OutputContains("rwv push --repo local/remotes/docs"))
                .claim(Claim::RemoteLacks {
                    repo: Repo::Proj,
                    branch: "rwv-records",
                }),
            Step::rwv(&a(), &["push", "--repo", &docs])
                .pushes(Remote::origin(Repo::Docs), "main")
                .expect(Exit::Succeeds)
                .claim(Claim::RemoteContains {
                    repo: Repo::Docs,
                    branch: "main",
                    mark: "docs-dep",
                }),
            publishes_records(Step::rwv(&a(), &["push"]))
                .pushes(Remote::origin(Repo::Proj), "main")
                .expect(Exit::Succeeds)
                .claim(Claim::PinContains {
                    site: LockSite::remote_main(Repo::Proj),
                    member: Repo::Docs,
                    mark: "docs-dep",
                }),
        ],
    }
}

/// dc13-push-no-lock: a member holding a commit no lock pins refuses the push,
/// naming the verb that records it, and the lock is not written.
pub fn dc13_push_no_lock() -> Scenario {
    scenario(
        "dc13: push never writes the lock",
        vec![
            Step::commit(&a(), Repo::App, "unpinned"),
            Step::rwv(&a(), &["push"])
                .expect(Exit::Fails)
                .claim(Claim::OutputContains("local/remotes/app: pin unpinned"))
                .claim(Claim::OutputContains("rwv lock --commit"))
                .claim(Claim::LocalStateUnchanged(a())),
        ],
    )
}

/// dc22-raw-project-switch: a raw `switch main` of the project repo while the
/// members stay on `feat-x` is no project-level refusal; each member refuses,
/// naming `update`, which moves them with their commits kept.
pub fn dc22_raw_project_switch() -> Scenario {
    let per_member = |step: Step| {
        step.expect(Exit::Fails)
            .claim(Claim::OutputContains(
                "local/remotes/app: alignment on branch with stray commits (feat-x)",
            ))
            .claim(Claim::OutputContains(
                "local/remotes/lib: alignment on branch (feat-x)",
            ))
            .claim(Claim::OutputContains("rwv update"))
            .claim(Claim::OutputLacks("projects/proj: alignment"))
            .claim(Claim::LocalStateUnchanged(a()))
    };
    let mut steps = feature_branch_in_primary();
    steps.extend([
        Step::git(&a(), Repo::Proj, &["switch", "main"]),
        per_member(Step::rwv(&a(), &["lock"])),
        per_member(Step::rwv(&a(), &["push"])),
        Step::rwv(&a(), &["update"])
            .expect(Exit::Succeeds)
            .claim(Claim::OnBranch {
                at: a(),
                repo: Repo::App,
                branch: "main",
            })
            .claim(Claim::Contains {
                at: a(),
                repo: Repo::App,
                rev: "feat-x",
                mark: "feat-app",
            }),
    ]);
    scenario("dc22: a raw project switch", steps)
}

/// records-conflict: two machines record `feat-x` differently; A publishes, B
/// fetches and meets the conflict alone, never publishes it, and settles it
/// with `branch set-mode`.
pub fn records_conflict() -> Scenario {
    scenario(
        "records: a conflict on the second to record",
        vec![
            Step::new_machine(Machine::B),
            Step::git(&a(), Repo::Proj, &["branch", "feat-x"]),
            Step::git(&b(), Repo::Proj, &["branch", "feat-x"]),
            Step::rwv(
                &a(),
                &[
                    "branch", "set-mode", "feat-x", "feature", "--target", "main",
                ],
            )
            .expect(Exit::Succeeds),
            Step::rwv(&b(), &["branch", "set-mode", "feat-x", "declared"]).expect(Exit::Succeeds),
            publishes_records(Step::rwv(&a(), &["push", "--branch", "feat-x"]))
                .pushes(origin_records(), "feat-x")
                .expect(Exit::Succeeds)
                .claim(Claim::RemoteRecordIs {
                    branch: "feat-x",
                    record: feat_x_record(),
                }),
            publishes_records(Step::rwv(&b(), &["push"]))
                .expect(Exit::Succeeds)
                .claim(Claim::RecordIs {
                    machine: Machine::B,
                    branch: "feat-x",
                    record: Some(Record::Conflict(vec![
                        Record::Declared,
                        feature_onto("main", Mode::Declared),
                    ])),
                })
                .claim(Claim::RemoteRecordIs {
                    branch: "feat-x",
                    record: feat_x_record(),
                }),
            Step::rwv(&b(), &["push"])
                .expect(Exit::Succeeds)
                .claim(Claim::RemoteRecordIs {
                    branch: "feat-x",
                    record: feat_x_record(),
                }),
            Step::rwv(&b(), &["branch", "set-mode", "feat-x", "declared"])
                .expect(Exit::Succeeds)
                .claim(Claim::RecordIs {
                    machine: Machine::B,
                    branch: "feat-x",
                    record: Some(Record::Declared),
                })
                .claim(Claim::RecordIs {
                    machine: Machine::A,
                    branch: "feat-x",
                    record: feat_x_record(),
                }),
        ],
    )
}

/// records-rejected: a remote refusing the records branch withholds a
/// feature's project branch and loses nothing, and publishes the trunk, whose
/// record is `declared`, reporting the rejection as a fact.
pub fn records_rejected() -> Scenario {
    let mut steps = feature_branch_in_primary();
    steps.extend([
        Step::rwv(&a(), &["push"])
            .pushes(Remote::origin(Repo::App), "feat-x")
            .expect(Exit::Fails)
            .claim(Claim::OutputContains(
                "projects/proj: publish records rejected",
            ))
            .claim(Claim::OutputContains("projects/proj feat-x was not pushed"))
            .claim(Claim::OutputContains("choose where it pushes"))
            .claim(Claim::RemoteContains {
                repo: Repo::App,
                branch: "feat-x",
                mark: "feat-app",
            })
            .claim(Claim::RemoteLacks {
                repo: Repo::Proj,
                branch: "feat-x",
            })
            .claim(Claim::RemoteRecordIs {
                branch: "feat-x",
                record: None,
            }),
        Step::git(&a(), Repo::Proj, &["switch", "main"]),
        Step::rwv(&a(), &["update"]).expect(Exit::Succeeds),
        Step::commit(&a(), Repo::App, "trunk-app"),
        Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::rwv(&a(), &["push"])
            .pushes(Remote::origin(Repo::App), "main")
            .pushes(Remote::origin(Repo::Proj), "main")
            .expect(Exit::Succeeds)
            .claim(Claim::OutputContains(
                "projects/proj: publish records rejected",
            ))
            .claim(Claim::PinContains {
                site: LockSite::remote_main(Repo::Proj),
                member: Repo::App,
                mark: "trunk-app",
            }),
    ]);
    Scenario {
        name: "records: a remote that refuses the records branch",
        seed: Seed::default().refusing_records(Remote::origin(Repo::Proj)),
        steps,
    }
}

/// records-first-push: the first push creates the records branch on a remote
/// that has none, and no other verb writes it there.
pub fn records_first_push() -> Scenario {
    scenario(
        "records: only the first push creates the branch",
        vec![
            Step::git(&a(), Repo::Proj, &["branch", "feat-z"]),
            Step::rwv(&a(), &["branch", "set-mode", "feat-z", "declared"]).expect(Exit::Succeeds),
            Step::rwv(&a(), &["fetch"]).expect(Exit::Succeeds),
            Step::rwv(&a(), &["update"]).expect(Exit::Succeeds),
            Step::rwv(&a(), &["lock", "--commit"])
                .expect(Exit::Succeeds)
                .claim(Claim::RemoteLacks {
                    repo: Repo::Proj,
                    branch: "rwv-records",
                }),
            publishes_records(Step::rwv(&a(), &["push"]))
                .expect(Exit::Succeeds)
                .claim(Claim::RemoteRecordIs {
                    branch: "main",
                    record: Some(Record::Declared),
                })
                .claim(Claim::RemoteRecordIs {
                    branch: "feat-z",
                    record: None,
                }),
            Step::rwv(&a(), &["push"]).expect(Exit::Succeeds),
        ],
    )
}

/// records-drop-per-machine: A deletes `feat-y` everywhere while B holds a
/// local `feat-y`; B keeps its record, never republishes it, and the records
/// branch reaches a fixed point.
pub fn records_drop_per_machine() -> Scenario {
    scenario(
        "records: a drop meets a branch kept on another machine",
        vec![
            Step::new_machine(Machine::B),
            Step::git(&a(), Repo::Proj, &["branch", "feat-y"]),
            Step::rwv(&a(), &["branch", "set-mode", "feat-y", "declared"]).expect(Exit::Succeeds),
            publishes_records(Step::rwv(&a(), &["push", "--branch", "feat-y"]))
                .pushes(Remote::origin(Repo::Proj), "feat-y")
                .expect(Exit::Succeeds)
                .claim(Claim::RemoteRecordIs {
                    branch: "feat-y",
                    record: Some(Record::Declared),
                }),
            Step::git(&b(), Repo::Proj, &["fetch", "origin"]),
            Step::git(&b(), Repo::Proj, &["branch", "feat-y", "origin/feat-y"]),
            Step::rwv(&b(), &["update"])
                .expect(Exit::Succeeds)
                .claim(Claim::RecordIs {
                    machine: Machine::B,
                    branch: "feat-y",
                    record: Some(Record::Declared),
                }),
            Step::rwv(
                &a(),
                &[
                    "branch",
                    "delete",
                    "feat-y",
                    "--target",
                    "main",
                    "--delete-remote",
                ],
            )
            .deletes(Remote::origin(Repo::Proj), "feat-y")
            .expect(Exit::Succeeds),
            publishes_records(Step::rwv(&a(), &["push"]))
                .expect(Exit::Succeeds)
                .claim(Claim::RemoteRecordIs {
                    branch: "feat-y",
                    record: None,
                }),
            Step::rwv(&b(), &["push"])
                .expect(Exit::Succeeds)
                .claim(Claim::RecordIs {
                    machine: Machine::B,
                    branch: "feat-y",
                    record: Some(Record::Declared),
                })
                .claim(Claim::RemoteRecordIs {
                    branch: "feat-y",
                    record: None,
                }),
            Step::rwv(&a(), &["push"]).expect(Exit::Succeeds),
            Step::rwv(&b(), &["push"]).expect(Exit::Succeeds),
            Step::rwv(&a(), &["push"]).expect(Exit::Succeeds),
            Step::rwv(&b(), &["push"])
                .expect(Exit::Succeeds)
                .claim(Claim::RemoteRecordIs {
                    branch: "feat-y",
                    record: None,
                })
                .claim(Claim::RecordIs {
                    machine: Machine::B,
                    branch: "feat-y",
                    record: Some(Record::Declared),
                }),
        ],
    )
}

/// records-offline-write: a record written with the remote unreachable stays
/// local, nothing is dropped meanwhile, a push is refused while the remote is
/// unconfirmed, and the next push online publishes it.
pub fn records_offline_write() -> Scenario {
    scenario(
        "records: a write made offline is published by the next push",
        vec![
            Step::git(&a(), Repo::Proj, &["branch", "feat-w"]),
            Step::rwv(&a(), &["branch", "set-mode", "feat-w", "declared"]).expect(Exit::Succeeds),
            publishes_records(Step::rwv(&a(), &["push", "--branch", "feat-w"]))
                .pushes(Remote::origin(Repo::Proj), "feat-w")
                .expect(Exit::Succeeds),
            Step::unreachable(origin_records()),
            Step::git(&a(), Repo::Proj, &["branch", "-D", "feat-w"]),
            Step::git(&a(), Repo::Proj, &["branch", "feat-z"]),
            Step::rwv(&a(), &["branch", "set-mode", "feat-z", "declared"])
                .expect(Exit::Succeeds)
                .claim(Claim::RecordIs {
                    machine: Machine::A,
                    branch: "feat-w",
                    record: Some(Record::Declared),
                })
                .claim(Claim::RecordIs {
                    machine: Machine::A,
                    branch: "feat-z",
                    record: Some(Record::Declared),
                }),
            Step::rwv(&a(), &["push", "--branch", "feat-z"])
                .expect(Exit::Fails)
                .claim(Claim::OutputContains("push-branch unconfirmed"))
                .claim(Claim::OutputContains("rwv fetch")),
            Step::reachable(origin_records()),
            Step::rwv(&a(), &["push", "--branch", "feat-z"])
                .pushes(Remote::origin(Repo::Proj), "feat-z")
                .pushes(Remote::origin(Repo::Proj), RECORDS_BRANCH)
                .expect(Exit::Succeeds)
                .claim(Claim::RemoteRecordIs {
                    branch: "feat-z",
                    record: Some(Record::Declared),
                }),
        ],
    )
}

/// records-unpushed-stay-local: two machines each hold a minted seat and an
/// unpushed `feat-y` recorded differently; each pushes `main`, and no remote
/// ever holds either entry.
pub fn records_unpushed_stay_local() -> Scenario {
    let seat = |machine: Machine| {
        Step::rwv(
            &Place::Primary(machine),
            &["workweave", "create", "--project", "proj", "seat"],
        )
        .expect(Exit::Succeeds)
    };
    let nothing_unpushed_is_held = |step: Step| {
        step.expect(Exit::Succeeds)
            .claim(Claim::RemoteRecordIs {
                branch: "feat-y",
                record: None,
            })
            .claim(Claim::RemoteRecordIs {
                branch: "proj--seat",
                record: None,
            })
    };
    scenario(
        "records: unpushed branches stay on their machine",
        vec![
            Step::new_machine(Machine::B),
            seat(Machine::A),
            seat(Machine::B),
            Step::git(&a(), Repo::Proj, &["branch", "feat-y"]),
            Step::git(&b(), Repo::Proj, &["branch", "feat-y"]),
            Step::rwv(
                &a(),
                &[
                    "branch", "set-mode", "feat-y", "feature", "--target", "main",
                ],
            )
            .expect(Exit::Succeeds),
            Step::rwv(&b(), &["branch", "set-mode", "feat-y", "declared"]).expect(Exit::Succeeds),
            nothing_unpushed_is_held(publishes_records(Step::rwv(&a(), &["push"]))).claim(
                Claim::RemoteRecordIs {
                    branch: "main",
                    record: Some(Record::Declared),
                },
            ),
            nothing_unpushed_is_held(Step::rwv(&b(), &["push"])),
            nothing_unpushed_is_held(Step::rwv(&a(), &["push"])),
            nothing_unpushed_is_held(Step::rwv(&b(), &["push"])),
            Step::rwv(&b(), &["branch", "list"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputLacks("conflict")),
            Step::rwv(&a(), &["branch", "list"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputLacks("conflict")),
        ],
    )
}

/// waiver-per-merge: a raw merge that keeps this side's lock leaves the other
/// side's `app` commit unjoined, which `push` refuses naming the merge; a
/// later backward re-pin by name abandons that commit, and the push then
/// passes listing the one merge it waived.
pub fn waiver_per_merge() -> Scenario {
    let app = Repo::App.rel_path();
    let mut steps = feature_branch_in_primary();
    steps.extend([
        Step::git(&a(), Repo::Proj, &["switch", "main"]),
        Step::rwv(&a(), &["update"]).expect(Exit::Succeeds),
        Step::commit(&a(), Repo::App, "main-app"),
        Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::git(&a(), Repo::Proj, &["merge", "--no-edit", "feat-x"]),
        Step::rwv(&a(), &["push", "--repo", &app])
            .pushes(Remote::origin(Repo::App), "main")
            .expect(Exit::Succeeds),
        Step::rwv(&a(), &["push"])
            .expect(Exit::Fails)
            .claim(Claim::OutputContains("local/remotes/app: pin unmerged"))
            .claim(Claim::OutputContains("merge --no-ff"))
            .claim(Claim::OutputLacks("pin waived"))
            .claim(Claim::RemoteLacks {
                repo: Repo::Proj,
                branch: "feat-x",
            }),
        Step::rwv(&a(), &["member", "pin", &app, "main~1", "--commit"])
            .expect(Exit::Succeeds)
            .may_rewind(&[Repo::App]),
        publishes_records(Step::rwv(&a(), &["push"]))
            .pushes(Remote::origin(Repo::Proj), "main")
            .expect(Exit::Succeeds)
            .claim(Claim::OutputContains("local/remotes/app: pin waived")),
    ]);
    scenario("waiver per merge", steps)
}

/// records-push-race: two machines publish records at once; the loser's
/// compare-and-swap on the remote loses, it fetches, merges and retries, and
/// no entry is lost.
pub fn records_push_race() -> Vec<Step> {
    vec![
        Step::new_machine(Machine::B),
        Step::git(&a(), Repo::Proj, &["branch", "feat-a"]),
        Step::git(&b(), Repo::Proj, &["branch", "feat-b"]),
        Step::rwv(&a(), &["branch", "set-mode", "feat-a", "declared"]).expect(Exit::Succeeds),
        Step::rwv(&b(), &["branch", "set-mode", "feat-b", "declared"]).expect(Exit::Succeeds),
        Step::rwv_paused(
            &a(),
            &["push", "--branch", "feat-a"],
            "records-publish",
            "loser",
        ),
        publishes_records(Step::rwv(&b(), &["push", "--branch", "feat-b"]))
            .pushes(Remote::origin(Repo::Proj), "feat-b")
            .expect(Exit::Succeeds)
            .claim(Claim::RemoteRecordIs {
                branch: "feat-a",
                record: None,
            }),
        publishes_records(Step::release("loser"))
            .pushes(Remote::origin(Repo::Proj), "feat-a")
            .expect(Exit::Succeeds)
            .claim(Claim::RemoteRecordIs {
                branch: "feat-a",
                record: Some(Record::Declared),
            })
            .claim(Claim::RemoteRecordIs {
                branch: "feat-b",
                record: Some(Record::Declared),
            }),
    ]
}

/// An overwrite is leased to the tip the push read: the remote moves while the
/// push is held before its member pushes, so the replacement is refused and the
/// remote keeps the move.
pub fn overwrite_lease_refuses_a_moved_remote() -> Vec<Step> {
    let mut steps = two_member_feature_in_primary();
    steps.push(push_feat_x_everywhere(Step::rwv(&a(), &["push"])).expect(Exit::Succeeds));
    steps.extend(amend_both());
    steps.extend([
        Step::rwv_paused(
            &a(),
            &[
                "push",
                "--overwrite",
                "local/remotes/app",
                "--overwrite",
                "local/remotes/lib",
            ],
            "push-members",
            "replacing",
        ),
        Step::forge(Forge::Commit {
            repo: Repo::App,
            branch: "feat-x",
            mark: "colleague-app",
        }),
        Step::release("replacing")
            .overwrites(Remote::origin(Repo::Lib), "feat-x")
            .may_leave_unresolved(&[Repo::Lib])
            .expect(Exit::Fails)
            .claim(Claim::OutputContains("rejected"))
            .claim(Claim::RemoteContains {
                repo: Repo::App,
                branch: "feat-x",
                mark: "colleague-app",
            }),
    ]);
    steps
}

pub fn dc20_state_location() -> Scenario {
    let aligning = |args: &[&str]| {
        Step::rwv(&ww("w"), args)
            .expect(Exit::Succeeds)
            .claim(Claim::WorkspaceStateKept(ww("w")))
            .claim(Claim::LocalStateUnchanged(ww("w")))
    };
    scenario(
        "workspace state: where each record lives, and when the stored one is written",
        vec![
            create("w"),
            Step::git(&a(), Repo::Proj, &["clean", "-xfd"]),
            Step::rwv(&a(), &["workweave", "list", "--project", "proj"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains("w"))
                .claim(Claim::IndexFile {
                    machine: Machine::A,
                    at: IndexAt::Common,
                    present: true,
                })
                .claim(Claim::IndexFile {
                    machine: Machine::A,
                    at: IndexAt::Old,
                    present: false,
                }),
            Step::rwv(&a(), &["status"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputLacks("reads as absent"))
                .claim(Claim::OutputLacks("project unaligned")),
            Step::rwv(&ww("w"), &["status"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputLacks("reads as absent"))
                .claim(Claim::OutputLacks("project unaligned"))
                .claim(Claim::WorkspaceStatesApart(a(), ww("w"))),
            Step::rwv(&a(), &["branch", "list"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains("proj--w"))
                .claim(Claim::OutputLacks("unrecorded")),
            aligning(&["update"]),
            aligning(&["update"]),
            aligning(&["update", "--repo", "app"]),
            aligning(&["update", "--repo", "lib"]),
            aligning(&["update", "--repo", "docs"]),
            aligning(&["update", "--role", "owned"]),
            aligning(&["update", "--repo", "app", "--repo", "lib"]),
            aligning(&["update"]),
            aligning(&["status"]),
            aligning(&["update"]),
        ],
    )
}

pub fn dc21_marker_disagreement() -> Scenario {
    let refused_naming_the_repair = |step: Step| {
        step.expect(Exit::Fails)
            .claim(Claim::OutputContains("rwv doctor --fix"))
    };
    scenario(
        "marker disagreement: destroying verbs refuse, every other verb restores",
        vec![
            create("w"),
            Step::tamper("corrupt w's marker", Tamper::CorruptMarker(ww("w"))),
            refused_naming_the_repair(Step::rwv(&a(), &["workweave", "delete", "w"]))
                .claim(Claim::DirStands(Machine::A, ".workweaves/proj--w"))
                .claim(Claim::MarkerIs(ww("w"), MarkerState::Corrupt)),
            refused_naming_the_repair(Step::rwv(&ww("w"), &["sync-to", "--retire"]))
                .claim(Claim::DirStands(Machine::A, ".workweaves/proj--w"))
                .claim(Claim::MarkerIs(ww("w"), MarkerState::Corrupt)),
            Step::rwv(&ww("w"), &["status"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains("restored"))
                .claim(Claim::MarkerParent {
                    at: ww("w"),
                    parent: a(),
                }),
            Step::tamper("delete w's marker", Tamper::DeleteMarker(ww("w"))),
            refused_naming_the_repair(Step::rwv(&ww("w"), &["sync-to", "--retire"]))
                .claim(Claim::MarkerIs(ww("w"), MarkerState::Absent)),
            Step::rwv(&ww("w"), &["doctor", "--fix"])
                .expect(Exit::Succeeds)
                .claim(Claim::MarkerParent {
                    at: ww("w"),
                    parent: a(),
                }),
            Step::tamper(
                "point w's marker at a parent the index does not record",
                Tamper::RepointMarkerParent(ww("w")),
            ),
            refused_naming_the_repair(Step::rwv(&a(), &["workweave", "delete", "w"]))
                .claim(Claim::DirStands(Machine::A, ".workweaves/proj--w"))
                .claim(Claim::MarkerIs(ww("w"), MarkerState::ParentElsewhere)),
            refused_naming_the_repair(Step::rwv(&ww("w"), &["sync-to", "--retire"]))
                .claim(Claim::DirStands(Machine::A, ".workweaves/proj--w"))
                .claim(Claim::MarkerIs(ww("w"), MarkerState::ParentElsewhere)),
            Step::rwv(&ww("w"), &["status"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains("restored"))
                .claim(Claim::MarkerParent {
                    at: ww("w"),
                    parent: a(),
                }),
            Step::tamper("delete w's marker", Tamper::DeleteMarker(ww("w"))),
            refused_naming_the_repair(Step::rwv(
                &a(),
                &["-w", "proj--w", "workweave", "delete", "w"],
            ))
            .claim(Claim::MarkerIs(ww("w"), MarkerState::Absent)),
            refused_naming_the_repair(Step::rwv(&a(), &["-w", "proj--w", "sync-to", "--retire"]))
                .claim(Claim::MarkerIs(ww("w"), MarkerState::Absent)),
            Step::rwv(&a(), &["-w", "proj--w", "status"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains("restored"))
                .claim(Claim::MarkerParent {
                    at: ww("w"),
                    parent: a(),
                }),
            Step::tamper("delete the index", Tamper::DeleteIndex(Machine::A)),
            Step::rwv(&ww("w"), &["status"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains("listed"))
                .claim(Claim::IndexLists {
                    machine: Machine::A,
                    name: "w",
                    parent: a(),
                }),
        ],
    )
}

pub fn index_parent() -> Scenario {
    scenario(
        "the index keeps each workweave's parent, so a marker restores whole",
        vec![
            create("outer"),
            Step::rwv(
                &ww("outer"),
                &["workweave", "create", "--project", "proj", "inner"],
            )
            .expect(Exit::Succeeds)
            .claim(Claim::IndexLists {
                machine: Machine::A,
                name: "inner",
                parent: ww("outer"),
            })
            .claim(Claim::IndexLists {
                machine: Machine::A,
                name: "outer",
                parent: a(),
            }),
            Step::tamper("delete inner's marker", Tamper::DeleteMarker(ww("inner"))),
            Step::rwv(&ww("inner"), &["status"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains("restored"))
                .claim(Claim::MarkerParent {
                    at: ww("inner"),
                    parent: ww("outer"),
                }),
            Step::tamper("delete the index", Tamper::DeleteIndex(Machine::A)),
            Step::rwv(&ww("inner"), &["status"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains("listed"))
                .claim(Claim::IndexLists {
                    machine: Machine::A,
                    name: "inner",
                    parent: ww("outer"),
                }),
            Step::rwv(&ww("outer"), &["status"])
                .expect(Exit::Succeeds)
                .claim(Claim::IndexLists {
                    machine: Machine::A,
                    name: "outer",
                    parent: a(),
                }),
        ],
    )
}

pub fn index_old_path() -> Scenario {
    let refused_as_old_path = |step: Step| {
        step.expect(Exit::Fails)
            .claim(Claim::OutputContains("legacy-workweave-index"))
            .claim(Claim::OutputContains("rwv doctor --fix"))
            .claim(Claim::IndexFile {
                machine: Machine::A,
                at: IndexAt::Old,
                present: true,
            })
            .claim(Claim::IndexFile {
                machine: Machine::A,
                at: IndexAt::Common,
                present: false,
            })
    };
    scenario(
        "an index at the old path refuses its readers until doctor converts it",
        vec![
            create("w1"),
            Step::rwv(
                &a(),
                &[
                    "workweave",
                    "create",
                    "--project",
                    "proj",
                    "far",
                    "--dir",
                    "../far-ww",
                ],
            )
            .expect(Exit::Succeeds),
            Step::tamper(
                "write the index as an earlier release kept it",
                Tamper::MoveIndexToOldPath(Machine::A),
            ),
            refused_as_old_path(Step::rwv(&a(), &["workweave", "list", "--project", "proj"])),
            refused_as_old_path(Step::rwv(
                &a(),
                &["workweave", "create", "--project", "proj", "w2"],
            ))
            .claim(Claim::DirStands(Machine::A, ".workweaves/proj--w1"))
            .claim(Claim::LocalStateUnchanged(a())),
            refused_as_old_path(Step::rwv(
                &a(),
                &[
                    "workweave",
                    "set-default-create-basedir",
                    "../elsewhere",
                    "--project",
                    "proj",
                ],
            )),
            refused_as_old_path(Step::rwv(&a(), &["workweave", "delete", "w1"]))
                .claim(Claim::DirStands(Machine::A, ".workweaves/proj--w1")),
            Step::rwv(&a(), &["doctor", "--fix"])
                .expect(Exit::Succeeds)
                .claim(Claim::IndexFile {
                    machine: Machine::A,
                    at: IndexAt::Old,
                    present: false,
                })
                .claim(Claim::IndexFile {
                    machine: Machine::A,
                    at: IndexAt::Common,
                    present: true,
                })
                .claim(Claim::IndexBasedir {
                    machine: Machine::A,
                    dir: Some("old-container"),
                })
                .claim(Claim::IndexLists {
                    machine: Machine::A,
                    name: "w1",
                    parent: a(),
                }),
            Step::rwv(&a(), &["workweave", "list", "--project", "proj"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains("w1"))
                .claim(Claim::OutputContains("far")),
        ],
    )
}

/// raw-rename-published-feature: a raw `git branch -m` of a pushed `feat-x` in
/// the project repo leaves its members on `feat-x`, the old record and the old
/// remote branches in place, and the workspace unaligned.
pub fn raw_rename_published_feature() -> Scenario {
    let mut steps = two_member_feature_in_primary();
    steps.extend([
        push_feat_x_everywhere(Step::rwv(&a(), &["push"])).expect(Exit::Succeeds),
        Step::git(&a(), Repo::Proj, &["branch", "-m", "feat-x", "feat-y"]),
        Step::rwv(&a(), &["status"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputContains("local/remotes/app   feat-x"))
            .claim(Claim::OutputContains("local/remotes/lib   feat-x"))
            .claim(Claim::OutputContains("record: proj feat-y: unrecorded"))
            .claim(Claim::OutputContains("projects/proj aligned for feat-x"))
            .claim(Claim::OnBranch {
                at: a(),
                repo: Repo::App,
                branch: "feat-x",
            })
            .claim(Claim::RemoteContains {
                repo: Repo::App,
                branch: "feat-x",
                mark: "pr-app",
            })
            .claim(Claim::RemoteContains {
                repo: Repo::Lib,
                branch: "feat-x",
                mark: "pr-lib",
            })
            .claim(Claim::RemoteRecordIs {
                branch: "feat-x",
                record: feat_x_record(),
            }),
    ]);
    scenario("a raw rename of a published feature", steps)
}

/// records-rename-adopt: `stable`, recorded `declared`, is renamed `release`
/// on A and pushed; B, with its own `stable`, adopts the new name; both
/// records and the dependent's target move with no conflict, and the landed
/// `origin/stable` is deleted by a named target.
pub fn records_rename_adopt() -> Scenario {
    scenario(
        "records: a declared branch renamed on one machine, adopted on another",
        vec![
            Step::new_machine(Machine::B),
            Step::git(&a(), Repo::Proj, &["branch", "stable"]),
            Step::git(&a(), Repo::Proj, &["branch", "stacked"]),
            Step::rwv(&a(), &["branch", "set-mode", "stable", "declared"]).expect(Exit::Succeeds),
            Step::rwv(
                &a(),
                &[
                    "branch", "set-mode", "stacked", "feature", "--target", "stable",
                ],
            )
            .expect(Exit::Succeeds),
            publishes_records(Step::rwv(&a(), &["push", "--branch", "stable"]))
                .pushes(Remote::origin(Repo::Proj), "stable")
                .expect(Exit::Succeeds),
            publishes_records(Step::rwv(&a(), &["push", "--branch", "stacked"]))
                .pushes(Remote::origin(Repo::Proj), "stacked")
                .expect(Exit::Succeeds),
            Step::git(&b(), Repo::Proj, &["fetch", "origin"]),
            Step::git(&b(), Repo::Proj, &["branch", "stable", "origin/stable"]),
            Step::rwv(&b(), &["branch", "set-mode", "stable", "declared"]).expect(Exit::Succeeds),
            Step::git(&a(), Repo::Proj, &["switch", "stable"]),
            Step::rwv(&a(), &["update"]).expect(Exit::Succeeds),
            Step::rwv(&a(), &["branch", "rename", "release"])
                .expect(Exit::Succeeds)
                .claim(Claim::RecordIs {
                    machine: Machine::A,
                    branch: "stable",
                    record: None,
                })
                .claim(Claim::RecordIs {
                    machine: Machine::A,
                    branch: "release",
                    record: Some(Record::Declared),
                })
                .claim(Claim::RecordIs {
                    machine: Machine::A,
                    branch: "stacked",
                    record: Some(feature_onto("release", Mode::Declared)),
                }),
            publishes_records(Step::rwv(&a(), &["push"]))
                .pushes(Remote::origin(Repo::Proj), "release")
                .expect(Exit::Succeeds)
                .claim(Claim::RemoteRecordIs {
                    branch: "stable",
                    record: None,
                })
                .claim(Claim::RemoteRecordIs {
                    branch: "release",
                    record: Some(Record::Declared),
                }),
            publishes_records(Step::rwv(&a(), &["push", "--branch", "stacked"]))
                .expect(Exit::Succeeds)
                .claim(Claim::RemoteRecordIs {
                    branch: "stacked",
                    record: Some(feature_onto("release", Mode::Declared)),
                }),
            Step::git(&b(), Repo::Proj, &["switch", "stable"]),
            Step::rwv(&b(), &["fetch"]).expect(Exit::Succeeds),
            Step::rwv(&b(), &["branch", "rename", "release", "--adopt"])
                .expect(Exit::Succeeds)
                .claim(Claim::RecordIs {
                    machine: Machine::B,
                    branch: "stable",
                    record: None,
                })
                .claim(Claim::RecordIs {
                    machine: Machine::B,
                    branch: "release",
                    record: Some(Record::Declared),
                })
                .claim(Claim::RecordIs {
                    machine: Machine::B,
                    branch: "stacked",
                    record: Some(feature_onto("release", Mode::Declared)),
                }),
            Step::rwv(
                &b(),
                &[
                    "branch",
                    "delete",
                    "stable",
                    "--target",
                    "release",
                    "--delete-remote",
                ],
            )
            .deletes(Remote::origin(Repo::Proj), "stable")
            .expect(Exit::Succeeds)
            .claim(Claim::RemoteLacks {
                repo: Repo::Proj,
                branch: "stable",
            }),
        ],
    )
}

pub fn basedir_default() -> Scenario {
    scenario(
        "the basedir is the default until it is set, and is kept in the index",
        vec![
            Step::rwv(&a(), &["workweave", "create", "--project", "proj", "ww"])
                .expect(Exit::Succeeds)
                .claim(Claim::DirStands(Machine::A, ".workweaves/proj--ww"))
                .claim(Claim::IndexBasedir {
                    machine: Machine::A,
                    dir: None,
                }),
            Step::rwv(
                &a(),
                &[
                    "workweave",
                    "set-default-create-basedir",
                    "../chosen",
                    "--project",
                    "proj",
                ],
            )
            .expect(Exit::Succeeds)
            .claim(Claim::IndexBasedir {
                machine: Machine::A,
                dir: Some("chosen"),
            }),
            Step::rwv(&a(), &["workweave", "create", "--project", "proj", "ww2"])
                .expect(Exit::Succeeds)
                .claim(Claim::DirStands(Machine::A, "chosen/proj--ww2")),
            Step::rwv(&a(), &["workweave", "list", "--project", "proj"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains("ww2"))
                .claim(Claim::OutputContains("ww")),
        ],
    )
}

/// no-superseded-on-prune: a pull request merged and its branch deleted is a
/// prune, which records no superseded tip, leaves the unpushed commits listed
/// and prints the rename without a name of its own to offer.
pub fn no_superseded_on_prune() -> Scenario {
    let mut steps = two_member_feature_in_primary();
    steps.extend([
        push_feat_x_everywhere(Step::rwv(&a(), &["push"])).expect(Exit::Succeeds),
        Step::forge(Forge::Merge {
            repo: Repo::App,
            branch: "feat-x",
            how: MergeHow::Squash,
        })
        .may_leave_unresolved(&[Repo::App]),
        Step::forge(Forge::DeleteBranch {
            repo: Repo::App,
            branch: "feat-x",
        })
        .may_leave_unresolved(&[Repo::App]),
        Step::forge(Forge::DeleteBranch {
            repo: Repo::Lib,
            branch: "feat-x",
        })
        .may_leave_unresolved(&[Repo::Lib]),
        Step::forge(Forge::Merge {
            repo: Repo::Proj,
            branch: "feat-x",
            how: MergeHow::Squash,
        })
        .may_leave_unresolved(&[Repo::App, Repo::Lib]),
        Step::forge(Forge::DeleteBranch {
            repo: Repo::Proj,
            branch: "feat-x",
        }),
        Step::rwv(&a(), &["fetch"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputContains("pruned"))
            .claim(Claim::OutputLacks("rewritten")),
        Step::git(&a(), Repo::App, &["for-each-ref", "refs/rwv/superseded"])
            .claim(Claim::OutputLacks("refs/rwv/superseded")),
        Step::git(&a(), Repo::Proj, &["for-each-ref", "refs/rwv/superseded"])
            .claim(Claim::OutputLacks("refs/rwv/superseded")),
        Step::rwv(&a(), &["status"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputContains("push-branch gone"))
            .claim(Claim::OutputContains("pr-app")),
        Step::rwv(&a(), &["push"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputContains("push-branch gone"))
            .claim(Claim::OutputContains("rwv branch rename <new> --adopt")),
    ]);
    scenario("a merged pull request's pruned branch", steps)
}

/// declared-mode-rename-retire (GP-1): a declared-mode project branch is
/// protected only against rewrites. `stable` is renamed `release` and pushed,
/// `--overwrite` and `--delete-remote-unlanded` refuse it `protected`, the
/// landed `origin/release` is deleted by a named target, and the forge's rename
/// of `main` to `trunk` takes the rename that adopts it.
pub fn declared_mode_rename_retire() -> Scenario {
    let mut steps = vec![
        Step::git(&a(), Repo::Proj, &["branch", "stable"]),
        Step::rwv(&a(), &["branch", "set-mode", "stable", "declared"]).expect(Exit::Succeeds),
        Step::git(&a(), Repo::Proj, &["switch", "stable"]),
        Step::rwv(&a(), &["update"]).expect(Exit::Succeeds),
        Step::rwv(&a(), &["branch", "rename", "release"])
            .expect(Exit::Succeeds)
            .claim(Claim::RecordIs {
                machine: Machine::A,
                branch: "stable",
                record: None,
            })
            .claim(Claim::RecordIs {
                machine: Machine::A,
                branch: "release",
                record: Some(Record::Declared),
            }),
        publishes_records(Step::rwv(&a(), &["push"]))
            .pushes(Remote::origin(Repo::Proj), "release")
            .expect(Exit::Succeeds)
            .claim(Claim::RemoteRecordIs {
                branch: "release",
                record: Some(Record::Declared),
            }),
        Step::rwv(&a(), &["push", "--overwrite", "projects/proj"])
            .expect(Exit::Fails)
            .claim(Claim::OutputContains("protected declared mode (release)")),
        Step::git(&a(), Repo::Proj, &["switch", "main"]),
        Step::rwv(&a(), &["update"]).expect(Exit::Succeeds),
        Step::rwv(
            &a(),
            &[
                "branch",
                "delete",
                "release",
                "--target",
                "main",
                "--discard-unlanded",
                "--delete-remote-unlanded",
                "projects/proj",
            ],
        )
        .expect(Exit::Fails)
        .claim(Claim::OutputContains("protected declared mode (release)")),
        Step::rwv(
            &a(),
            &[
                "branch",
                "delete",
                "release",
                "--target",
                "main",
                "--delete-remote",
            ],
        )
        .deletes(Remote::origin(Repo::Proj), "release")
        .expect(Exit::Succeeds)
        .claim(Claim::RemoteLacks {
            repo: Repo::Proj,
            branch: "release",
        }),
        Step::commit(&a(), Repo::App, "main-work"),
        Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
    ];
    steps.extend(Step::forge_rename_branch(Repo::Proj, "main", "trunk"));
    steps.extend([
        Step::rwv(&a(), &["fetch"]).expect(Exit::Succeeds),
        publishes_records(Step::rwv(&a(), &["push"]))
            .pushes(Remote::origin(Repo::App), "main")
            .claim(Claim::OutputContains("push-branch gone"))
            .claim(Claim::OutputContains("rwv branch rename <new> --adopt")),
        Step::rwv(&a(), &["branch", "rename", "trunk", "--adopt"])
            .expect(Exit::Succeeds)
            .claim(Claim::RecordIs {
                machine: Machine::A,
                branch: "main",
                record: None,
            })
            .claim(Claim::RecordIs {
                machine: Machine::A,
                branch: "trunk",
                record: Some(Record::Declared),
            }),
    ]);
    scenario("a declared-mode branch is renamed and retired", steps)
}

pub fn dc25_url_provenance() -> Scenario {
    Scenario {
        name: "dc25: a mirror url on another branch is no placement disagreement",
        seed: Seed::default().mirrors(),
        steps: vec![
            Step::git(&a(), Repo::Proj, &["switch", "-q", "-c", "stable"]),
            Step::rwv(&a(), &["branch", "set-mode", "stable", "declared"]).expect(Exit::Succeeds),
            Step::rwv(
                &a(),
                &[
                    "member",
                    "set-url",
                    "local/remotes/lib",
                    "url:mirror-keeping:lib",
                ],
            )
            .expect(Exit::Succeeds),
            Step::rwv(&a(), &["doctor", "--all"])
                .claim(Claim::OutputLacks("placement-disagreement")),
        ],
    }
}

pub fn url_change_raw_merge() -> Scenario {
    Scenario {
        name: "a raw merge carrying lib's url change is reported until published",
        seed: Seed::default().mirrors(),
        steps: vec![
            Step::rwv(&a(), &["update"]).expect(Exit::Succeeds),
            Step::git(&a(), Repo::Proj, &["switch", "-q", "-c", "mirror-url"]),
            Step::rwv(&a(), &["branch", "set-mode", "mirror-url", "declared"])
                .expect(Exit::Succeeds),
            Step::rwv(
                &a(),
                &[
                    "member",
                    "set-url",
                    "local/remotes/lib",
                    "url:mirror-keeping:lib",
                ],
            )
            .expect(Exit::Succeeds),
            Step::git(&a(), Repo::Proj, &["switch", "-q", "main"]),
            Step::git(
                &a(),
                Repo::Proj,
                &["merge", "-q", "--no-ff", "-m", "merge", "mirror-url"],
            ),
            Step::rwv(&a(), &["update"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains("member-changed url:")),
            Step::rwv(&a(), &["update"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains("member-changed url:")),
        ],
    }
}

pub fn set_branch_new() -> Scenario {
    scenario(
        "set-branch: a typo and an unfetched branch refuse, --new creates and pins",
        vec![
            Step::rwv(&a(), &["member", "set-branch", "local/remotes/lib", "dve"])
                .expect(Exit::Fails)
                .claim(Claim::OutputContains("name no-such-branch"))
                .claim(Claim::OutputContains("--new"))
                .claim(Claim::LocalStateUnchanged(a())),
            Step::rwv(
                &a(),
                &["member", "set-branch", "local/remotes/lib", "hold", "--new"],
            )
            .expect(Exit::Succeeds)
            .claim(Claim::OnBranch {
                at: a(),
                repo: Repo::Lib,
                branch: "hold",
            }),
            Step::rwv(
                &a(),
                &["member", "set-branch", "local/remotes/lib", "main", "--new"],
            )
            .expect(Exit::Fails)
            .claim(Claim::OutputContains("name exists")),
        ],
    )
}

pub fn member_pin_behind() -> Scenario {
    scenario(
        "member pin behind: reported until the next lock records the tip forward",
        vec![
            Step::commit(&a(), Repo::Lib, "lib-two"),
            Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::commit(&a(), Repo::Lib, "lib-three"),
            Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::git(&a(), Repo::Proj, &["branch", "other"]),
            Step::rwv(&a(), &["branch", "set-mode", "other", "declared"]).expect(Exit::Succeeds),
            Step::rwv(
                &a(),
                &["member", "pin", "local/remotes/lib", "HEAD~1", "--commit"],
            )
            .expect(Exit::Succeeds)
            .may_rewind(&[Repo::Lib]),
            Step::rwv(&a(), &["status"]).claim(Claim::OutputContains("pin ahead")),
            Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
        ],
    )
}
