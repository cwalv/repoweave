use crate::invariants::{LockSite, Mode, Record};
use crate::runner::{
    Claim, Exit, Forge, IndexAt, MarkerState, MergeHow, MissingVerb, Scenario, Step, Tamper,
};
use crate::sandbox::{
    declared_branch_text, Machine, Place, Remote, Repo, SecondProject, Seed, RECORDS_BRANCH,
};
use repoweave::diagnosis::State;
use repoweave::manifest::Role;
use repoweave::refusal::RefusalKind;

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
            Step::rwv(&ww("solo"), &["land", "--retire"])
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
                    remote: Remote::origin(Repo::App),
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
            remote: Remote::origin(Repo::App),
            branch: "feat-x",
            mark: "pr-app",
        })
        .claim(Claim::RemoteContains {
            remote: Remote::origin(Repo::Lib),
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
            .expect(Exit::refuses(Repo::App, State::PushBranchDiverged))
            .claim(Claim::Judged(Repo::Lib, State::PushBranchDiverged))
            .claim(Claim::OutputLacks("--overwrite")),
        Step::rwv(&a(), &["push", "--overwrite", "local/remotes/app"])
            .expect(Exit::refuses(Repo::Lib, State::PushBranchDiverged))
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
            Step::rwv(
                &a(),
                &["member", "pin", &repo.rel_path(), "HEAD", "--commit"],
            )
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
            .claim(Claim::RemoteContains {
                remote: Remote::fork(Repo::App),
                branch: "feat-x",
                mark: "pr-app",
            })
            .claim(Claim::RemoteContains {
                remote: Remote::fork(Repo::Lib),
                branch: "feat-x",
                mark: "pr-lib",
            })
            .claim(Claim::RemoteLacks {
                repo: Repo::App,
                branch: "feat-x",
            })
            .claim(Claim::RemoteLacks {
                repo: Repo::Lib,
                branch: "feat-x",
            })
            .claim(Claim::RemoteRecordIs {
                branch: "feat-x",
                record: feat_x_record(),
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
        Step::rwv(&b(), &["switch", "feat-x"])
            .expect(Exit::Succeeds)
            .claim(Claim::OnBranch {
                at: b(),
                repo: Repo::App,
                branch: "feat-x",
            })
            .claim(Claim::OnBranch {
                at: b(),
                repo: Repo::Lib,
                branch: "feat-x",
            })
            .claim(Claim::AtPin {
                at: b(),
                repo: Repo::App,
            })
            .claim(Claim::AtPin {
                at: b(),
                repo: Repo::Lib,
            }),
        Step::pending(
            "commit in lib on B, publish, then converge or land from A",
            MissingVerb::Land,
        ),
    ]);
    scenario("resume on machine B", steps)
}

pub fn feature_in_primary_while_main_moves() -> Scenario {
    scenario(
        "feature in the primary while main moves",
        vec![
            Step::rwv(&a(), &["branch", "create", "feat-x"]).expect(Exit::Succeeds),
            Step::commit(&a(), Repo::App, "feat-app"),
            Step::rwv(&a(), &["switch", "main"])
                .expect(Exit::Succeeds)
                .claim(Claim::OnBranch {
                    at: a(),
                    repo: Repo::App,
                    branch: "main",
                }),
            Step::rwv(&a(), &["switch", "feat-x"])
                .expect(Exit::Succeeds)
                .claim(Claim::OnBranch {
                    at: a(),
                    repo: Repo::App,
                    branch: "feat-x",
                })
                .claim(Claim::Contains {
                    at: a(),
                    repo: Repo::App,
                    rev: "HEAD",
                    mark: "feat-app",
                }),
            Step::rwv(&a(), &["switch", "main"]).expect(Exit::Succeeds),
            Step::rwv(&a(), &["switch", "feat-x"]).expect(Exit::Succeeds),
            Step::commit(&a(), Repo::Lib, "main-lib"),
            Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::rwv(&a(), &["switch", "feat-x"]).expect(Exit::Succeeds),
            Step::rwv(&a(), &["sync", "--strategy", "rebase"])
                .expect(Exit::Succeeds)
                .claim(Claim::Contains {
                    at: a(),
                    repo: Repo::Lib,
                    rev: "HEAD",
                    mark: "main-lib",
                })
                .claim(Claim::Contains {
                    at: a(),
                    repo: Repo::App,
                    rev: "HEAD",
                    mark: "feat-app",
                }),
            Step::pending("land feat-x into main", MissingVerb::Land),
        ],
    )
}

pub fn workweave_without_b_then_pr() -> Scenario {
    scenario(
        "workweave without -b, then a PR",
        vec![
            create("anon"),
            Step::commit(&ww("anon"), Repo::App, "anon-app"),
            Step::rwv(&ww("anon"), &["push"])
                .expect(Exit::refuses(Repo::Proj, State::NameMinted))
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
                    remote: Remote::origin(Repo::App),
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
            .expect(Exit::refuses_kind(RefusalKind::DroppedRepoHasUniqueCommits))
            .claim(Claim::OutputContains("rwv backup list"))
            .claim(Claim::BackedUp {
                repo: Repo::App,
                mark: "anon-app",
                marked: None,
            }),
            Step::rwv(
                &a(),
                &["backup", "drop", &app, "--all", "--project", "proj"],
            )
            .expect(Exit::Succeeds)
            .may_strand(&[Repo::App]),
            Step::rwv(
                &a(),
                &["member", "remove", &app, "--delete", "--project", "proj"],
            )
            .expect(Exit::Succeeds)
            .claim(Claim::DirGone(Machine::A, "weave/local/remotes/app")),
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
            Step::rwv(&a(), &["doctor", "--fix"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains(
                    "orphaned-savepoint for projects/proj: kept as a backup",
                ))
                .claim(Claim::BackedUp {
                    repo: Repo::Proj,
                    mark: "tombstoned",
                    marked: None,
                }),
            Step::rwv(&a(), &["backup", "list", "--project", "proj"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputLine("projects/proj"))
                .claim(Claim::OutputContains(" tombstoned")),
            Step::rwv(
                &a(),
                &[
                    "backup",
                    "drop",
                    "projects/proj",
                    "--all",
                    "--project",
                    "proj",
                ],
            )
            .expect(Exit::Succeeds)
            .may_strand(&[Repo::Proj]),
            Step::rwv(&a(), &["backup", "list", "--project", "proj"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputLine("no backups")),
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
                .expect(Exit::refuses(Repo::Proj, State::NameMinted))
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

/// `stable` created declared, holding `lib` one commit behind `main`'s pin.
fn stable_holding_lib_behind() -> Vec<Step> {
    vec![
        Step::commit(&a(), Repo::Lib, "lib-on-main"),
        Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::rwv(&a(), &["branch", "create", "stable", "--declared"])
            .expect(Exit::Succeeds)
            .claim(Claim::OnBranch {
                at: a(),
                repo: Repo::Proj,
                branch: "stable",
            }),
        Step::rwv(
            &a(),
            &["member", "pin", "local/remotes/lib", "HEAD~1", "--commit"],
        )
        .expect(Exit::Succeeds)
        .may_rewind(&[Repo::Lib])
        .claim(Claim::Judged(Repo::Lib, State::PinAhead)),
    ]
}

pub fn feature_off_stable() -> Scenario {
    let mut steps = stable_holding_lib_behind();
    steps.push(Step::pending(
        "a feature off stable, landed into it",
        MissingVerb::Land,
    ));
    scenario("feature off stable", steps)
}

pub fn permanent_seat() -> Scenario {
    scenario(
        "permanent seat",
        vec![
            create("seat"),
            Step::commit(&ww("seat"), Repo::App, "seat-1"),
            Step::rwv(&ww("seat"), &["land"])
                .expect(Exit::Succeeds)
                .claim(Claim::Contains {
                    at: a(),
                    repo: Repo::App,
                    rev: "main",
                    mark: "seat-1",
                }),
            create("other"),
            Step::commit(&ww("other"), Repo::App, "elsewhere-1"),
            Step::rwv(&ww("other"), &["land", "--retire"]).expect(Exit::Succeeds),
            Step::commit(&ww("seat"), Repo::App, "seat-2"),
            Step::rwv(&ww("seat"), &["lock"]).expect(Exit::Succeeds),
            Step::git(&ww("seat"), Repo::Proj, &["add", "rwv.lock"]),
            Step::commit(&ww("seat"), Repo::Proj, "seat-notes"),
            Step::rwv(&ww("seat"), &["land"])
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
            Step::rwv(
                &ww("seat"),
                &["member", "set-branch", "local/remotes/lib", "dev"],
            )
            .expect(Exit::Succeeds)
            .claim(Claim::OutputContains(
                "local/remotes/lib: branch main -> dev",
            )),
            Step::pending(
                "land the declaration change with --allow-member-change lib",
                MissingVerb::Land,
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
    let mut steps = stable_holding_lib_behind();
    steps.push(Step::pending(
        "a hotfix landed into stable, then a bump",
        MissingVerb::Land,
    ));
    scenario("stable end to end", steps)
}

pub fn main_change_into_stable() -> Scenario {
    scenario(
        "bring a main change into stable",
        vec![
            Step::rwv(&a(), &["branch", "create", "stable", "--declared"]).expect(Exit::Succeeds),
            Step::pending("bring a main change into stable", MissingVerb::Land),
        ],
    )
}

fn shared_member(name: &'static str, branch: &'static str) -> Scenario {
    let side = Place::Workweave(Machine::A, Repo::Proj2, "side".to_owned());
    let activate_proj2 = Step::rwv(&a(), &["activate", "proj2"]);
    let activate_proj2 = if branch == "main" {
        activate_proj2
            .expect(Exit::Succeeds)
            .claim(Claim::Presents {
                at: a(),
                project: Repo::Proj2,
            })
    } else {
        activate_proj2
            .expect(Exit::refuses(
                Repo::Lib,
                State::AlignmentOnBranchWithStrayCommits,
            ))
            .claim(Claim::OutputContains("rwv activate proj2 --update"))
            .claim(Claim::LocalStateUnchanged(a()))
            .claim(Claim::Presents {
                at: a(),
                project: Repo::Proj,
            })
    };
    Scenario {
        name,
        seed: Seed::default().second_project(SecondProject { branch }),
        steps: vec![
            Step::clone_project(Machine::A, Repo::Proj2).expect(Exit::Succeeds),
            Step::commit(&a(), Repo::Lib, "lib-pending"),
            activate_proj2,
            Step::rwv(&a(), &["activate", "proj"])
                .expect(Exit::Succeeds)
                .claim(Claim::Presents {
                    at: a(),
                    project: Repo::Proj,
                }),
            Step::rwv(&a(), &["workweave", "create", "--project", "proj2", "side"])
                .expect(Exit::Succeeds),
            Step::commit(&side, Repo::Lib, "side-lib"),
            Step::pending(
                "land side into proj2's lib while proj is active",
                MissingVerb::Land,
            ),
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
        "nested workweaves, each on a branch it names",
        vec![
            Step::rwv(
                &a(),
                &[
                    "workweave",
                    "create",
                    "--project",
                    "proj",
                    "w1",
                    "--new-branch",
                    "w1",
                ],
            )
            .expect(Exit::Succeeds),
            Step::rwv(
                &ww("w1"),
                &[
                    "workweave",
                    "create",
                    "--project",
                    "proj",
                    "w2",
                    "--new-branch",
                    "w2",
                ],
            )
            .expect(Exit::Succeeds),
            Step::commit(&ww("w2"), Repo::App, "w2-app"),
            Step::rwv(&ww("w2"), &["land"])
                .expect(Exit::Succeeds)
                .claim(Claim::Contains {
                    at: ww("w1"),
                    repo: Repo::App,
                    rev: "HEAD",
                    mark: "w2-app",
                }),
            Step::rwv(&ww("w2"), &["sync"]).expect(Exit::Succeeds),
            Step::rwv(&ww("w2"), &["land", "--retire"])
                .expect(Exit::Succeeds)
                .claim(Claim::WorkweaveGone(ww("w2"))),
        ],
    )
}

pub fn nested_workweaves_without_naming() -> Scenario {
    scenario(
        "nested workweaves, unnamed: sync both ways, retire child, land parent",
        vec![
            create("w1"),
            Step::rwv(
                &ww("w1"),
                &["workweave", "create", "--project", "proj", "w2"],
            )
            .expect(Exit::Succeeds),
            Step::commit(&ww("w2"), Repo::App, "w2-app"),
            Step::rwv(&ww("w2"), &["land"])
                .expect(Exit::Succeeds)
                .claim(Claim::Contains {
                    at: ww("w1"),
                    repo: Repo::App,
                    rev: "HEAD",
                    mark: "w2-app",
                }),
            Step::commit(&ww("w1"), Repo::Lib, "w1-lib"),
            Step::rwv(&ww("w1"), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::rwv(&ww("w2"), &["sync"])
                .expect(Exit::Succeeds)
                .claim(Claim::Contains {
                    at: ww("w2"),
                    repo: Repo::Lib,
                    rev: "HEAD",
                    mark: "w1-lib",
                }),
            Step::rwv(&ww("w2"), &["land", "--retire"])
                .expect(Exit::Succeeds)
                .claim(Claim::WorkweaveGone(ww("w2"))),
            Step::rwv(&ww("w1"), &["land", "--retire"])
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
    if main_moves {
        steps.extend(lock_conflict_resolved_by_hand("feat-x"));
    } else {
        steps.push(Step::git(
            &a(),
            Repo::Proj,
            &["merge", "--no-edit", "feat-x"],
        ));
    }
    steps.extend([
        Step::git(&a(), Repo::App, &["merge", "--no-edit", "feat-x"]),
        Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
        publishes_records(Step::rwv(&a(), &["push"]))
            .pushes(Remote::origin(Repo::App), "main")
            .pushes(Remote::origin(Repo::Proj), "main")
            .expect(Exit::Succeeds)
            .claim(Claim::RemoteContains {
                remote: Remote::origin(Repo::App),
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

/// A raw merge of `branch` into the project's `main` whose pins diverged under
/// one branch: the lock merge driver leaves `rwv.lock` conflicted, every verb
/// refuses it, and the operator keeps `main`'s pins by hand and concludes it.
fn lock_conflict_resolved_by_hand(branch: &'static str) -> Vec<Step> {
    vec![
        Step::git(&a(), Repo::Proj, &["merge", "--no-edit", branch]).expect(Exit::Code(1)),
        Step::rwv(&a(), &["status"]).expect(Exit::refuses(Repo::Proj, State::EditsLockUnresolved)),
        Step::git(&a(), Repo::Proj, &["checkout", "--ours", "rwv.lock"]),
        Step::git(&a(), Repo::Proj, &["add", "rwv.lock"]),
        Step::git(&a(), Repo::Proj, &["commit", "--no-edit"]),
    ]
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
            Step::rwv(&ww("det"), &["lock"])
                .expect(Exit::refuses(Repo::App, State::AlignmentDetachedUnpushed)),
            Step::pending("land the workweave", MissingVerb::Land),
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
            Step::rwv(&ww("wip"), &["lock"])
                .expect(Exit::refuses(
                    Repo::App,
                    State::AlignmentOnBranchWithStrayCommits,
                ))
                .claim(Claim::OutputContains("copy these commits onto `"))
                .claim(Claim::OutputLacks("`<b>`"))
                .claim(Claim::OutputLine("rwv explain alignment")),
            Step::rwv(&ww("wip"), &["push"]).expect(Exit::refuses(Repo::Proj, State::NameMinted)),
            Step::pending("land the workweave", MissingVerb::Land),
        ],
    )
}

pub fn raw_switch_in_primary_project() -> Scenario {
    let mut steps = feature_branch_in_primary();
    steps.extend([
        Step::git(&a(), Repo::App, &["switch", "main"]),
        Step::git(&a(), Repo::Proj, &["switch", "main"]),
        Step::git(&a(), Repo::Proj, &["switch", "feat-x"]),
        Step::rwv(&a(), &["status"])
            .expect(Exit::Succeeds)
            .claim(Claim::Judged(Repo::App, State::AlignmentOnBranch))
            .claim(Claim::OutputContains(
                "local/remotes/app: alignment on branch (main)",
            ))
            .claim(Claim::OutputContains("rwv update\n"))
            .claim(Claim::OutputLacks("projects/proj: alignment")),
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
        Step::rwv(&Place::Primary(Machine::C), &["update", "--remote"])
            .expect(Exit::refuses(Repo::App, State::PinBackwards))
            .claim(Claim::OutputContains("rwv member pin local/remotes/app "))
            .claim(Claim::LocalStateUnchanged(Place::Primary(Machine::C))),
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
            Step::rwv(&a(), &["update", "--remote"])
                .expect(Exit::refuses(Repo::Lib, State::PinBackwards))
                .claim(Claim::OutputContains("rwv member pin local/remotes/lib "))
                .claim(Claim::LocalStateUnchanged(a())),
            Step::rwv(&a(), &["lock", "--commit"])
                .expect(Exit::Succeeds)
                .may_rewind(&[Repo::Lib])
                .claim(Claim::OutputContains("member-changed branch: main -> dev"))
                .claim(Claim::OutputContains(
                    "local/remotes/lib: branch main -> dev",
                ))
                .claim(Claim::OutputContains("local/remotes/lib: pinned at "))
                .claim(Claim::OutputContains(
                    "rwv lock: local/remotes/lib: re-pinned ",
                )),
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
            Step::rwv(&a(), &["update", "--remote"])
                .expect(Exit::refuses(Repo::Lib, State::NameNoSuchBranch))
                .claim(Claim::OutputContains("rwv fetch"))
                .claim(Claim::LocalStateUnchanged(a())),
            Step::rwv(&a(), &["lock", "--commit"])
                .expect(Exit::refuses(Repo::Lib, State::NameNoSuchBranch))
                .claim(Claim::OutputContains("rwv fetch"))
                .claim(Claim::OutputContains(
                    "rwv member set-branch local/remotes/lib nowhere --new",
                ))
                .claim(Claim::LocalStateUnchanged(a())),
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
            Step::pending("land and retire the workweave", MissingVerb::Land),
        ],
    )
}

pub fn cherry_pick_onto_stable() -> Scenario {
    scenario(
        "cherry-pick onto stable a commit adding a member",
        vec![
            Step::rwv(&a(), &["branch", "create", "stable", "--declared"]).expect(Exit::Succeeds),
            Step::pending(
                "cherry-pick a commit adding a member",
                MissingVerb::CherryPick,
            ),
        ],
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
                .claim(Claim::OutputContains(
                    "conflict: declared | feature onto elsewhere (as declared)",
                )),
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
                .expect(Exit::refuses(Repo::Proj, State::TargetUnreadable))
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
    let refused_with = |step: Step, state: State| {
        step.expect(Exit::refuses(Repo::Lib, state))
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
            refused_with(
                Step::rwv(&a(), &["activate", "proj2"]),
                State::AlignmentOnBranch,
            )
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
            refused_with(Step::rwv(&a(), &["activate", "proj2"]), State::EditsDirty)
                .claim(Claim::OutputContains("README"))
                .claim(Claim::OutputContains("stash push"))
                .claim(Claim::OutputLacks("--update"))
                .claim(Claim::OutputLacks("alignment")),
            refused_with(
                Step::rwv(&a(), &["activate", "proj2", "--update"]),
                State::EditsDirty,
            )
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
                .expect(Exit::refuses(
                    Repo::Lib,
                    State::AlignmentOnBranchWithStrayCommits,
                ))
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
    let refused = |args: &[&str], state: State| {
        Step::rwv(&a(), args)
            .expect(Exit::refuses(Repo::Lib, state))
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
            refused(&["update", "--project", "proj2"], State::AlignmentOnBranch)
                .claim(Claim::OutputContains(
                    "local/remotes/lib: alignment on branch (main)",
                ))
                .claim(Claim::OutputContains("rwv activate proj2 --update"))
                .claim(Claim::OutputLacks("\n    rwv update\n")),
            refused(
                &["update", "--remote", "--project", "proj2"],
                State::AlignmentOnBranch,
            )
            .claim(Claim::OutputContains(
                "local/remotes/lib: alignment on branch (main)",
            ))
            .claim(Claim::OutputContains("rwv activate proj2 --update")),
            refused(&["pull", "--project", "proj2"], State::AlignmentOnBranch)
                .claim(Claim::OutputContains(
                    "local/remotes/lib: alignment on branch (main)",
                ))
                .claim(Claim::OutputContains("rwv activate proj2 --update")),
            Step::hand_edit(&a(), Repo::Lib, "README", "lib\n", "lib edited\n"),
            refused(&["update", "--project", "proj2"], State::EditsDirty)
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
                .expect(Exit::refuses(Repo::Docs, State::PublishWithheld))
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
                    remote: Remote::origin(Repo::Docs),
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
                .expect(Exit::refuses(Repo::App, State::PinUnpinned))
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
        step.expect(Exit::refuses(
            Repo::App,
            State::AlignmentOnBranchWithStrayCommits,
        ))
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
            Step::rwv(&b(), &["push", "--branch", "feat-x"])
                .expect(Exit::refuses(Repo::Proj, State::TargetConflict))
                .claim(Claim::OutputContains("rwv branch set-mode feat-x"))
                .claim(Claim::RemoteRecordIs {
                    branch: "feat-x",
                    record: feat_x_record(),
                })
                .claim(Claim::RecordIs {
                    machine: Machine::B,
                    branch: "feat-x",
                    record: Some(Record::Conflict(vec![
                        Record::Declared,
                        feature_onto("main", Mode::Declared),
                    ])),
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
            publishes_records(Step::rwv(&b(), &["push", "--branch", "feat-x"]))
                .pushes(origin_records(), "feat-x")
                .expect(Exit::Succeeds)
                .claim(Claim::RemoteRecordIs {
                    branch: "feat-x",
                    record: Some(Record::Declared),
                }),
        ],
    )
}

/// records-rejected: a remote refusing the records branch takes a feature's
/// project branch and the trunk all the same, the rejection reported as a
/// fact; another machine opening the feature finds it unrecorded and is asked
/// once how to treat it.
pub fn records_rejected() -> Scenario {
    let mut steps = feature_branch_in_primary();
    steps.extend([
        Step::rwv(&a(), &["push"])
            .pushes(Remote::origin(Repo::App), "feat-x")
            .pushes(Remote::origin(Repo::Proj), "feat-x")
            .expect(Exit::Succeeds)
            .claim(Claim::OutputContains(
                "projects/proj: publish records rejected",
            ))
            .claim(Claim::OutputContains("is asked once how to treat it"))
            .claim(Claim::RemoteContains {
                remote: Remote::origin(Repo::App),
                branch: "feat-x",
                mark: "feat-app",
            })
            .claim(Claim::RemoteRecordIs {
                branch: "feat-x",
                record: None,
            }),
        Step::new_machine(Machine::B),
        Step::rwv(&b(), &["switch", "feat-x"])
            .expect(Exit::refuses(Repo::Proj, State::TargetUnrecorded))
            .claim(Claim::OutputContains("rwv fetch"))
            .claim(Claim::OutputContains("rwv branch set-mode")),
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
                .expect(Exit::refuses(Repo::Proj, State::PushBranchUnconfirmed))
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
                .claim(Claim::OutputLacks("conflict:")),
            Step::rwv(&a(), &["branch", "list"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputLacks("conflict:")),
        ],
    )
}

/// waiver-per-merge: a raw merge that keeps this side's lock leaves the other
/// side's `app` commit unjoined, which `push` refuses naming the merge; a
/// later backward re-pin by name, which its lock commit records, abandons
/// that commit, and the push then passes listing the one merge it waived and
/// the verb that waived it.
pub fn waiver_per_merge() -> Scenario {
    let app = Repo::App.rel_path();
    let mut steps = feature_branch_in_primary();
    steps.extend([
        Step::git(&a(), Repo::Proj, &["switch", "main"]),
        Step::rwv(&a(), &["update"]).expect(Exit::Succeeds),
        Step::commit(&a(), Repo::App, "main-app"),
        Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
    ]);
    steps.extend(lock_conflict_resolved_by_hand("feat-x"));
    steps.extend([
        Step::rwv(&a(), &["push", "--repo", &app])
            .pushes(Remote::origin(Repo::App), "main")
            .expect(Exit::Succeeds),
        Step::rwv(&a(), &["push"])
            .expect(Exit::refuses(Repo::App, State::PinUnmerged))
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
            .claim(Claim::OutputContains("local/remotes/app: pin waived"))
            .claim(Claim::OutputContains("by `rwv member pin`")),
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
            .expect(Exit::refuses(Repo::App, State::PublishWithheld))
            .claim(Claim::OutputContains("(stale info)"))
            .claim(Claim::RemoteContains {
                remote: Remote::origin(Repo::App),
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
                .claim(Claim::OutputLine("w"))
                .claim(Claim::IndexLists {
                    machine: Machine::A,
                    name: "w",
                    parent: a(),
                })
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
        step.expect(Exit::refuses_kind(RefusalKind::StaleRegistryEntry))
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
            refused_naming_the_repair(Step::rwv(&ww("w"), &["land", "--retire"]))
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
            refused_naming_the_repair(Step::rwv(&ww("w"), &["land", "--retire"]))
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
            refused_naming_the_repair(Step::rwv(&ww("w"), &["land", "--retire"]))
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
            refused_naming_the_repair(Step::rwv(&a(), &["-w", "proj--w", "land", "--retire"]))
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
        step.expect(Exit::refuses(
            Repo::Proj,
            State::LegacyWorkweaveIndexOldPath,
        ))
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
                .claim(Claim::OutputLine("w1"))
                .claim(Claim::OutputLine("far")),
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
                remote: Remote::origin(Repo::App),
                branch: "feat-x",
                mark: "pr-app",
            })
            .claim(Claim::RemoteContains {
                remote: Remote::origin(Repo::Lib),
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
                .claim(Claim::OutputLine("ww2"))
                .claim(Claim::OutputLine("ww")),
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
            .expect(Exit::refuses(Repo::Proj, State::ProtectedDeclaredMode))
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
        .expect(Exit::refuses(Repo::Proj, State::ProtectedDeclaredMode))
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
            .expect(Exit::Succeeds)
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
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains(
                    "local/remotes/lib: origin URL mismatch",
                ))
                .claim(Claim::OutputContains("mirror-keeping/lib.git"))
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
                .claim(Claim::OutputContains("member-changed url:"))
                .claim(Claim::UpstreamOn {
                    at: a(),
                    repo: Repo::Lib,
                    branch: "main",
                    remote: Remote::origin(Repo::Lib),
                })
                .claim(Claim::OutputContains("'main' relinks to "))
                .claim(Claim::OutputContains("rwv fetch")),
            Step::rwv(&a(), &["fetch"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains(
                    "local/remotes/lib: mirror-keeping/main created",
                )),
            Step::rwv(&a(), &["update"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains("member-changed url:"))
                .claim(Claim::UpstreamOn {
                    at: a(),
                    repo: Repo::Lib,
                    branch: "main",
                    remote: Remote::keeping_mirror(Repo::Lib),
                }),
            Step::rwv(&a(), &["update"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains("member-changed url:")),
            publishes_records(Step::rwv(&a(), &["push"]))
                .pushes(Remote::origin(Repo::Proj), "main")
                .expect(Exit::Succeeds),
            Step::rwv(&a(), &["update"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputLacks("member-changed url:")),
        ],
    }
}

pub fn set_branch_new() -> Scenario {
    let lib = "local/remotes/lib";
    scenario(
        "set-branch: a typo and an unfetched branch refuse, --new creates and pins",
        vec![
            Step::rwv(&a(), &["member", "set-branch", lib, "dve"])
                .expect(Exit::refuses(Repo::Lib, State::NameNoSuchBranch))
                .claim(Claim::OutputContains("rwv fetch"))
                .claim(Claim::OutputContains("--new"))
                .claim(Claim::LocalStateUnchanged(a())),
            Step::forge(Forge::Commit {
                repo: Repo::Lib,
                branch: "next",
                mark: "next-unfetched",
            }),
            Step::rwv(&a(), &["member", "set-branch", lib, "next"])
                .expect(Exit::refuses(Repo::Lib, State::NameNoSuchBranch))
                .claim(Claim::OutputContains("rwv fetch"))
                .claim(Claim::OutputContains("--new"))
                .claim(Claim::LocalStateUnchanged(a())),
            Step::rwv(&a(), &["member", "set-branch", lib, "hold", "--new"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains("created hold at"))
                .claim(Claim::OnBranch {
                    at: a(),
                    repo: Repo::Lib,
                    branch: "hold",
                })
                .claim(Claim::AtPin {
                    at: a(),
                    repo: Repo::Lib,
                })
                .claim(Claim::OutputContains("Committed rwv.toml, rwv.lock")),
            Step::rwv(
                &a(),
                &[
                    "branch",
                    "create",
                    "dec",
                    "--declared",
                    "--set-new-branch",
                    "local/remotes/lib=hold-too",
                ],
            )
            .expect(Exit::Succeeds)
            .claim(Claim::OnBranch {
                at: a(),
                repo: Repo::Lib,
                branch: "hold-too",
            })
            .claim(Claim::AtPin {
                at: a(),
                repo: Repo::Lib,
            }),
        ],
    )
}

/// `lib-three` is unpushed, so the branch `keeps-lib-three` locks it: the tip
/// beyond the held pin is then pinned elsewhere, `pin` ahead, not unpinned.
pub fn member_pin_behind() -> Scenario {
    scenario(
        "member pin behind: reported until the next lock records the tip forward",
        vec![
            Step::commit(&a(), Repo::Lib, "lib-two"),
            Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::commit(&a(), Repo::Lib, "lib-three"),
            Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::git(&a(), Repo::Proj, &["branch", "keeps-lib-three"]),
            Step::rwv(&a(), &["branch", "set-mode", "keeps-lib-three", "declared"])
                .expect(Exit::Succeeds),
            Step::rwv(
                &a(),
                &["member", "pin", "local/remotes/lib", "HEAD~1", "--commit"],
            )
            .expect(Exit::Succeeds)
            .may_rewind(&[Repo::Lib])
            .claim(Claim::Judged(Repo::Lib, State::PinAhead))
            .claim(Claim::OutputContains("pinned-by: proj keeps-lib-three"))
            .claim(Claim::OutputContains("rwv lock --commit"))
            .claim(Claim::PinContains {
                site: LockSite::Checkout(a(), Repo::Proj, "main".to_owned()),
                member: Repo::Lib,
                mark: "lib-two",
            })
            .claim(Claim::Contains {
                at: a(),
                repo: Repo::Lib,
                rev: "HEAD",
                mark: "lib-three",
            }),
            Step::rwv(&a(), &["status"])
                .expect(Exit::Succeeds)
                .claim(Claim::Judged(Repo::Lib, State::PinAhead))
                .claim(Claim::OutputContains("pinned-by: proj keeps-lib-three"))
                .claim(Claim::OutputContains("record it: the lock"))
                .claim(Claim::OutputLine("    rwv lock --commit"))
                .claim(Claim::OutputLine("    rwv push --repo local/remotes/lib")),
            Step::rwv(&a(), &["switch", "--detach", "HEAD"])
                .expect(Exit::Succeeds)
                .claim(Claim::AtPin {
                    at: a(),
                    repo: Repo::Lib,
                })
                .claim(Claim::OutputContains("every repo is at its pin")),
            Step::rwv(&a(), &["switch", "main"])
                .expect(Exit::Succeeds)
                .claim(Claim::OnBranch {
                    at: a(),
                    repo: Repo::Lib,
                    branch: "main",
                })
                .claim(Claim::Contains {
                    at: a(),
                    repo: Repo::Lib,
                    rev: "HEAD",
                    mark: "lib-three",
                }),
            Step::rwv(&a(), &["lock", "--commit"])
                .expect(Exit::Succeeds)
                .claim(Claim::PinContains {
                    site: LockSite::Checkout(a(), Repo::Proj, "main".to_owned()),
                    member: Repo::Lib,
                    mark: "lib-three",
                }),
        ],
    )
}

/// records-reserved-name: the records branch's name and every name below it
/// are no weave branch's; `branch create` refuses each and creates nothing.
pub fn records_reserved_name() -> Scenario {
    let refused = |name: &str| {
        Step::rwv(&a(), &["branch", "create", name, "--declared"])
            .expect(Exit::refuses(Repo::Proj, State::NameReserved))
            .claim(Claim::LocalStateUnchanged(a()))
    };
    scenario(
        "records: a reserved name is no weave branch",
        vec![
            refused("rwv-records"),
            refused("rwv-records/x"),
            Step::rwv(&a(), &["branch", "create", "rwv-records"])
                .expect(Exit::refuses(Repo::Proj, State::NameReserved))
                .claim(Claim::LocalStateUnchanged(a())),
        ],
    )
}

/// update-remote-writes-lock: `update --remote` writes the lock and leaves it
/// uncommitted; `switch` refuses until `lock --commit` records it.
pub fn update_remote_writes_lock() -> Scenario {
    scenario(
        "update --remote writes the lock uncommitted and switch refuses until it is committed",
        vec![
            Step::rwv(&a(), &["branch", "create", "feat-z"]).expect(Exit::Succeeds),
            Step::rwv(&a(), &["switch", "main"]).expect(Exit::Succeeds),
            Step::forge(Forge::Commit {
                repo: Repo::App,
                branch: "main",
                mark: "upstream-app",
            }),
            Step::rwv(&a(), &["update", "--remote"])
                .expect(Exit::Succeeds)
                .claim(Claim::Contains {
                    at: a(),
                    repo: Repo::App,
                    rev: "HEAD",
                    mark: "upstream-app",
                }),
            Step::rwv(&a(), &["switch", "feat-z"])
                .expect(Exit::refuses(Repo::Proj, State::EditsLockFilesUncommitted))
                .claim(Claim::OutputContains("rwv lock --commit"))
                .claim(Claim::OutputLacks("rwv branch create"))
                .claim(Claim::LocalStateUnchanged(a())),
            Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::rwv(&a(), &["switch", "feat-z"])
                .expect(Exit::Succeeds)
                .claim(Claim::OnBranch {
                    at: a(),
                    repo: Repo::App,
                    branch: "feat-z",
                })
                .claim(Claim::OnBranch {
                    at: a(),
                    repo: Repo::Proj,
                    branch: "feat-z",
                }),
        ],
    )
}

/// detached-create-start: from a detached workspace holding an uncommitted
/// `rwv.toml`, `branch create --declared` starts at HEAD and carries the file.
pub fn detached_create_start() -> Scenario {
    let declared = declared_branch_text(Repo::Lib, "main");
    scenario(
        "a create from a detached workspace starts at its HEAD and carries rwv.toml",
        vec![
            Step::commit(&a(), Repo::Proj, "detached-start"),
            Step::rwv(&a(), &["switch", "--detach", "HEAD"]).expect(Exit::Succeeds),
            Step::hand_edit(
                &a(),
                Repo::Proj,
                "rwv.toml",
                &declared,
                &format!("{declared} # edited while detached"),
            ),
            Step::rwv(&a(), &["branch", "create", "w"])
                .expect(Exit::refuses_kind(RefusalKind::HeadNotOnBranch))
                .claim(Claim::OutputContains("--declared")),
            Step::rwv(&a(), &["branch", "create", "w", "--declared"])
                .expect(Exit::Succeeds)
                .claim(Claim::OnBranch {
                    at: a(),
                    repo: Repo::Proj,
                    branch: "w",
                })
                .claim(Claim::Contains {
                    at: a(),
                    repo: Repo::Proj,
                    rev: "w",
                    mark: "detached-start",
                })
                .claim(Claim::OnBranch {
                    at: a(),
                    repo: Repo::App,
                    branch: "main",
                }),
            Step::hand_edit(
                &a(),
                Repo::Proj,
                "rwv.toml",
                "# edited while detached",
                "# edited while detached",
            ),
        ],
    )
}

fn publish_app_and_project(mark: &'static str) -> Vec<Step> {
    vec![
        Step::commit(&a(), Repo::App, mark),
        Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::git(&a(), Repo::App, &["push", "origin", "main"])
            .pushes(Remote::origin(Repo::App), "main"),
        Step::git(&a(), Repo::Proj, &["push", "origin", "main"])
            .pushes(Remote::origin(Repo::Proj), "main"),
    ]
}

pub fn three_way_pins() -> Scenario {
    three_way_pins_on(Seed::default())
}

#[cfg(unix)]
pub fn three_way_pins_on_old_git() -> Scenario {
    three_way_pins_on(Seed::default().old_git())
}

fn three_way_pins_on(seed: Seed) -> Scenario {
    let mut steps = publish_app_and_project("app-one");
    steps.extend(publish_app_and_project("app-two"));
    steps.extend([
        Step::new_machine(Machine::B),
        Step::rwv(
            &b(),
            &["member", "pin", "local/remotes/app", "HEAD~1", "--commit"],
        )
        .expect(Exit::Succeeds)
        .may_rewind(&[Repo::App]),
        Step::git(&b(), Repo::Proj, &["push", "origin", "main"])
            .pushes(Remote::origin(Repo::Proj), "main")
            .may_rewind(&[Repo::App]),
        Step::commit(&a(), Repo::Proj, "a-notes"),
        Step::rwv(&a(), &["pull", "--merge-upstream"])
            .expect(Exit::Succeeds)
            .may_rewind(&[Repo::App])
            .claim(Claim::PinContains {
                site: LockSite::Checkout(a(), Repo::Proj, "main".to_owned()),
                member: Repo::App,
                mark: "app-one",
            })
            .claim(Claim::Contains {
                at: a(),
                repo: Repo::Proj,
                rev: "HEAD",
                mark: "a-notes",
            }),
    ]);
    Scenario {
        name: "three-way pins: a backward pin by name survives the merge",
        seed,
        steps,
    }
}

pub fn dc14_c3_under_pin_ahead() -> Scenario {
    scenario(
        "dc14: C3 under an unpinned commit whose push branch sits at the pin",
        vec![
            Step::commit(&a(), Repo::App, "app-local"),
            Step::rwv(&a(), &["push"])
                .expect(Exit::refuses(Repo::App, State::PinUnpinned))
                .claim(Claim::OutputContains(
                    "rwv update --remote --reset-to-upstream local/remotes/app",
                )),
            Step::rwv(
                &a(),
                &[
                    "update",
                    "--remote",
                    "--reset-to-upstream",
                    "local/remotes/app",
                ],
            )
            .expect(Exit::Succeeds)
            .may_rewrite(&[Repo::App])
            .claim(Claim::BackedUp {
                repo: Repo::App,
                mark: "app-local",
                marked: None,
            }),
        ],
    )
}

pub fn dc09_integrations_settle(seed: Seed) -> Scenario {
    let docs_url = "url:origin:docs";
    Scenario {
        name: "dc09: a merge adding a member settles its integration files",
        seed,
        steps: vec![
            Step::rwv(&a(), &["member", "remove", "local/remotes/docs"]).expect(Exit::Succeeds),
            Step::git(&a(), Repo::Proj, &["push", "origin", "main"])
                .pushes(Remote::origin(Repo::Proj), "main"),
            Step::new_machine(Machine::B),
            Step::rwv(&a(), &["member", "add", docs_url]).expect(Exit::Succeeds),
            Step::git(&a(), Repo::Proj, &["push", "origin", "main"])
                .pushes(Remote::origin(Repo::Proj), "main"),
            Step::commit(&b(), Repo::Proj, "b-notes"),
            Step::rwv(&b(), &["pull", "--merge-upstream"])
                .expect(Exit::Succeeds)
                .claim(Claim::Contains {
                    at: b(),
                    repo: Repo::Proj,
                    rev: "HEAD",
                    mark: "b-notes",
                }),
        ],
    }
}

pub fn both_sides_move_app_conflictingly() -> Vec<Step> {
    vec![
        Step::new_machine(Machine::B),
        Step::rwv(&a(), &["member", "remove", "local/remotes/docs"]).expect(Exit::Succeeds),
        Step::hand_edit(&a(), Repo::App, "README", "app", "alpha"),
        Step::git(&a(), Repo::App, &["commit", "-qam", "alpha"]),
        Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::git(&a(), Repo::App, &["push", "origin", "main"])
            .pushes(Remote::origin(Repo::App), "main"),
        Step::git(&a(), Repo::Proj, &["push", "origin", "main"])
            .pushes(Remote::origin(Repo::Proj), "main"),
        Step::hand_edit(&b(), Repo::App, "README", "app", "beta"),
        Step::git(&b(), Repo::App, &["commit", "-qam", "beta"]),
        Step::rwv(&b(), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::rwv(&b(), &["pull", "--merge-upstream"])
            .expect(Exit::refuses_kind(RefusalKind::OpParked))
            .claim(Claim::OutputContains(
                "merge conflict in local/remotes/app (README)",
            ))
            .claim(Claim::OutputContains("rwv pull --continue"))
            .claim(Claim::OutputContains("rwv abort")),
        Step::git(&b(), Repo::Proj, &["rev-parse", "--verify", "-q", "HEAD^2"])
            .expect(Exit::Code(1)),
        Step::git(&b(), Repo::Docs, &["status", "--porcelain"]).expect(Exit::Succeeds),
    ]
}

pub fn merge_conflict_continues() -> Scenario {
    let mut steps = both_sides_move_app_conflictingly();
    steps.extend([
        Step::git(&b(), Repo::App, &["checkout", "--theirs", "README"]),
        Step::git(&b(), Repo::App, &["add", "README"]),
        Step::rwv(&b(), &["pull", "--continue"]).expect(Exit::Succeeds),
    ]);
    scenario(
        "pull --merge-upstream: a member conflict stops, continue finishes",
        steps,
    )
}

pub fn merge_conflict_aborts() -> Scenario {
    let mut steps = both_sides_move_app_conflictingly();
    steps.extend([
        Step::rwv(&b(), &["abort"]).expect(Exit::Succeeds),
        Step::git(
            &b(),
            Repo::App,
            &["rev-parse", "-q", "--verify", "MERGE_HEAD"],
        )
        .expect(Exit::Code(1)),
        Step::git(&b(), Repo::App, &["diff", "--quiet", "HEAD"]).expect(Exit::Succeeds),
        Step::git(&b(), Repo::Docs, &["status", "--porcelain"]).expect(Exit::Succeeds),
    ]);
    scenario(
        "pull --merge-upstream: a member conflict stops, abort restores",
        steps,
    )
}

pub fn resolve_app_conflict_and_continue() -> Vec<Step> {
    vec![
        Step::git(&b(), Repo::App, &["checkout", "--theirs", "README"]),
        Step::git(&b(), Repo::App, &["add", "README"]),
        Step::rwv(&b(), &["pull", "--continue"]).expect(Exit::Succeeds),
        Step::git(&b(), Repo::Proj, &["rev-parse", "--verify", "-q", "HEAD^2"])
            .expect(Exit::Succeeds),
    ]
}

pub fn merge_upstream_as_refs() -> Scenario {
    scenario(
        "update --remote --merge-upstream: a branch checked out nowhere merges as refs",
        vec![
            Step::commit(&a(), Repo::Lib, "lib-local"),
            Step::git(&a(), Repo::Lib, &["switch", "-q", "-c", "side"]),
            Step::forge(Forge::Commit {
                repo: Repo::Lib,
                branch: "main",
                mark: "lib-remote",
            }),
            Step::rwv(&a(), &["update", "--remote", "--repo", "local/remotes/lib"])
                .expect(Exit::refuses(Repo::Lib, State::PushBranchDiverged))
                .claim(Claim::OutputContains("--merge-upstream")),
            Step::rwv(
                &a(),
                &[
                    "update",
                    "--remote",
                    "--merge-upstream",
                    "--repo",
                    "local/remotes/lib",
                ],
            )
            .expect(Exit::Succeeds)
            .claim(Claim::OnBranch {
                at: a(),
                repo: Repo::Lib,
                branch: "side",
            })
            .claim(Claim::Contains {
                at: a(),
                repo: Repo::Lib,
                rev: "main",
                mark: "lib-remote",
            })
            .claim(Claim::Contains {
                at: a(),
                repo: Repo::Lib,
                rev: "main",
                mark: "lib-local",
            }),
            Step::git(&a(), Repo::Lib, &["rev-parse", "--verify", "-q", "main^2"])
                .expect(Exit::Succeeds),
            Step::git(&a(), Repo::Lib, &["diff", "--quiet", "HEAD"]).expect(Exit::Succeeds),
            Step::git(
                &a(),
                Repo::Lib,
                &["ls-files", "--error-unmatch", "lib-remote.txt"],
            )
            .expect(Exit::Code(1)),
        ],
    )
}

pub fn reset_acts_only_on_the_named_repo() -> Scenario {
    let mut steps = Vec::new();
    for (repo, local, remote) in [
        (Repo::App, "app-local", "app-remote"),
        (Repo::Lib, "lib-local", "lib-remote"),
    ] {
        steps.extend([
            Step::commit(&a(), repo, local),
            Step::forge(Forge::Commit {
                repo,
                branch: "main",
                mark: remote,
            }),
        ]);
    }
    steps.extend([
        Step::rwv(
            &a(),
            &[
                "update",
                "--remote",
                "--merge-upstream",
                "--reset-to-upstream",
                "local/remotes/app",
            ],
        )
        .expect(Exit::Succeeds)
        .may_rewrite(&[Repo::App])
        .claim(Claim::BackedUp {
            repo: Repo::App,
            mark: "app-local",
            marked: None,
        })
        .claim(Claim::Contains {
            at: a(),
            repo: Repo::App,
            rev: "HEAD",
            mark: "app-remote",
        }),
        Step::git(&a(), Repo::Lib, &["rev-parse", "--verify", "-q", "HEAD^2"])
            .expect(Exit::Succeeds),
        Step::git(&a(), Repo::App, &["rev-parse", "--verify", "-q", "HEAD^2"])
            .expect(Exit::Code(1))
            .claim(Claim::Contains {
                at: a(),
                repo: Repo::Lib,
                rev: "HEAD",
                mark: "lib-remote",
            }),
    ]);
    scenario(
        "update --remote --reset-to-upstream resets only the repo it names, --merge-upstream merges the rest",
        steps,
    )
}

/// dc15-moved-reports: `update` moving a member off a pushed `wip` with no
/// stray commits reports where it moved it from; a member on a branch a
/// project declares, and one on a recorded weave branch, are moved with no
/// such report.
pub fn dc15_moved_reports() -> Scenario {
    Scenario {
        name: "dc15: update reports a member moved off a branch nothing accounts for",
        seed: Seed::default().second_project(SecondProject { branch: "dev" }),
        steps: vec![
            Step::clone_project(Machine::A, Repo::Proj2).expect(Exit::Succeeds),
            Step::git(&a(), Repo::App, &["switch", "-c", "wip"]),
            Step::git(&a(), Repo::App, &["push", "-u", "origin", "wip"])
                .pushes(Remote::origin(Repo::App), "wip"),
            Step::git(&a(), Repo::Lib, &["switch", "dev"]),
            Step::git(&a(), Repo::Proj, &["branch", "recorded"]),
            Step::rwv(&a(), &["branch", "set-mode", "recorded", "declared"]).expect(Exit::Succeeds),
            Step::git(&a(), Repo::Docs, &["switch", "-c", "recorded"]),
            Step::rwv(&a(), &["update"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputLine(
                    "rwv update: local/remotes/app: moved from wip",
                ))
                .claim(Claim::OutputLacks("local/remotes/lib: moved from"))
                .claim(Claim::OutputLacks("local/remotes/docs: moved from"))
                .claim(Claim::OnBranch {
                    at: a(),
                    repo: Repo::App,
                    branch: "main",
                })
                .claim(Claim::OnBranch {
                    at: a(),
                    repo: Repo::Lib,
                    branch: "main",
                })
                .claim(Claim::OnBranch {
                    at: a(),
                    repo: Repo::Docs,
                    branch: "main",
                }),
        ],
    }
}

fn delete_with(name: &str, flags: &[&str]) -> Step {
    let mut args = vec!["workweave", "delete", "--project", "proj", name];
    args.extend_from_slice(flags);
    Step::rwv(&a(), &args)
}

/// dc16-destroys-name-repos: `--delete-remote` deletes only push branches
/// that landed; an unlanded one is deleted only in the repo
/// `--delete-remote-unlanded` names, after a backup; `backup drop` needs a
/// repo.
pub fn dc16_destroys_name_repos() -> Scenario {
    let pushed = |at: &Place, repo: Repo, branch: &'static str| {
        Step::git(at, repo, &["push", "origin", branch]).pushes(Remote::origin(repo), branch)
    };
    scenario(
        "dc16: a delete destroys only what landed, or the repos it names",
        vec![
            create("feat"),
            Step::commit(&ww("feat"), Repo::Lib, "feat-lib"),
            pushed(&ww("feat"), Repo::Lib, "proj--feat"),
            pushed(&ww("feat"), Repo::App, "proj--feat"),
            pushed(&ww("feat"), Repo::Proj, "proj--feat"),
            delete_with("feat", &["--discard-unlanded", "--delete-remote"])
                .deletes(Remote::origin(Repo::App), "proj--feat")
                .deletes(Remote::origin(Repo::Proj), "proj--feat")
                .expect(Exit::Succeeds)
                .claim(Claim::WorkweaveGone(ww("feat")))
                .claim(Claim::RemoteLacks {
                    repo: Repo::App,
                    branch: "proj--feat",
                })
                .claim(Claim::RemoteContains {
                    remote: Remote::origin(Repo::Lib),
                    branch: "proj--feat",
                    mark: "feat-lib",
                })
                .claim(Claim::BackedUp {
                    repo: Repo::Lib,
                    mark: "feat-lib",
                    marked: None,
                }),
            create("feat2"),
            Step::commit(&ww("feat2"), Repo::Lib, "feat2-lib"),
            Step::commit(&ww("feat2"), Repo::App, "feat2-app"),
            pushed(&ww("feat2"), Repo::Lib, "proj--feat2"),
            pushed(&ww("feat2"), Repo::App, "proj--feat2"),
            delete_with(
                "feat2",
                &[
                    "--discard-unlanded",
                    "--delete-remote-unlanded",
                    "local/remotes/lib",
                ],
            )
            .deletes(Remote::origin(Repo::Lib), "proj--feat2")
            .expect(Exit::Succeeds)
            .claim(Claim::WorkweaveGone(ww("feat2")))
            .claim(Claim::RemoteLacks {
                repo: Repo::Lib,
                branch: "proj--feat2",
            })
            .claim(Claim::RemoteContains {
                remote: Remote::origin(Repo::App),
                branch: "proj--feat2",
                mark: "feat2-app",
            })
            .claim(Claim::BackedUp {
                repo: Repo::Lib,
                mark: "feat2-lib",
                marked: None,
            })
            .claim(Claim::BackedUp {
                repo: Repo::App,
                mark: "feat2-app",
                marked: None,
            }),
            Step::rwv(&a(), &["backup", "drop", "--project", "proj"])
                .expect(Exit::Code(2))
                .claim(Claim::OutputContains("<REPO>"))
                .claim(Claim::BackedUp {
                    repo: Repo::Lib,
                    mark: "feat2-lib",
                    marked: None,
                }),
        ],
    )
}

/// dc18-partial-delete-status: a delete that leaves an unlanded member
/// branch exits with its own status and names each survivor and why.
pub fn dc18_partial_delete_status() -> Scenario {
    scenario(
        "dc18: a partial delete exits 3 naming its survivors",
        vec![
            create("feat"),
            Step::commit(&ww("feat"), Repo::Lib, "unlanded-lib"),
            delete_with("feat", &["--json"])
                .expect(Exit::Code(3))
                .claim(Claim::WorkweaveGone(ww("feat")))
                .claim(Claim::Contains {
                    at: a(),
                    repo: Repo::Lib,
                    rev: "proj--feat",
                    mark: "unlanded-lib",
                })
                .claim(Claim::OutputContains("\"reason\": \"unlanded\""))
                .claim(Claim::OutputContains("\"reason\": \"members\""))
                .claim(Claim::OutputContains("unlanded-lib")),
            create("feat2"),
            Step::commit(&ww("feat2"), Repo::Lib, "unlanded-lib2"),
            delete_with("feat2", &[])
                .expect(Exit::Code(3))
                .claim(Claim::OutputContains(
                    "left: local/remotes/lib proj--feat2: unlanded",
                )),
        ],
    )
}

/// A workweave's feature, its target `main` moved in `app` and locked; `sync`
/// rebases the feature's `app` onto `main`'s and relocks.
pub fn sync_takes_in_the_target() -> Scenario {
    scenario(
        "sync takes in the record's target",
        vec![
            create("w"),
            Step::commit(&ww("w"), Repo::App, "w-app"),
            Step::rwv(&ww("w"), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::commit(&a(), Repo::App, "main-app"),
            Step::commit(&a(), Repo::Lib, "main-lib"),
            Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::rwv(&ww("w"), &["sync"])
                .expect(Exit::refuses_kind(RefusalKind::NotFastForwardable))
                .claim(Claim::OutputContains("--strategy rebase")),
            Step::rwv(&ww("w"), &["sync", "--strategy", "rebase"])
                .expect(Exit::Succeeds)
                .may_rewrite(&[Repo::App])
                .claim(Claim::OutputContains("local/remotes/app: rebased onto"))
                .claim(Claim::Contains {
                    at: ww("w"),
                    repo: Repo::App,
                    rev: "proj--w",
                    mark: "main-app",
                })
                .claim(Claim::Contains {
                    at: ww("w"),
                    repo: Repo::Lib,
                    rev: "proj--w",
                    mark: "main-lib",
                })
                .claim(Claim::PinContains {
                    site: LockSite::Checkout(ww("w"), Repo::Proj, "proj--w".to_owned()),
                    member: Repo::App,
                    mark: "main-app",
                })
                .claim(Claim::PinContains {
                    site: LockSite::Checkout(ww("w"), Repo::Proj, "proj--w".to_owned()),
                    member: Repo::Lib,
                    mark: "main-lib",
                }),
        ],
    )
}

fn b_ww(name: &str) -> Place {
    Place::ww(Machine::B, name)
}

pub fn records_bare_open_second_machine() -> Scenario {
    let mut steps = two_member_feature_in_primary();
    steps.extend([
        push_feat_x_everywhere(Step::rwv(&a(), &["push"])).expect(Exit::Succeeds),
        Step::new_machine(Machine::B),
        Step::rwv(
            &b(),
            &[
                "workweave",
                "create",
                "--project",
                "proj",
                "ww",
                "--branch",
                "feat-x",
            ],
        )
        .expect(Exit::Succeeds)
        .claim(Claim::OnBranch {
            at: b_ww("ww"),
            repo: Repo::Proj,
            branch: "feat-x",
        })
        .claim(Claim::OnBranch {
            at: b_ww("ww"),
            repo: Repo::App,
            branch: "feat-x",
        })
        .claim(Claim::OnBranch {
            at: b_ww("ww"),
            repo: Repo::Lib,
            branch: "feat-x",
        })
        .claim(Claim::AtPin {
            at: b_ww("ww"),
            repo: Repo::App,
        })
        .claim(Claim::RecordIs {
            machine: Machine::B,
            branch: "feat-x",
            record: feat_x_record(),
        })
        .claim(Claim::MarkerParent {
            at: b_ww("ww"),
            parent: b(),
        }),
    ]);
    scenario("a second machine opens a published branch bare", steps)
}

pub fn records_raw_push_unrecorded() -> Scenario {
    scenario(
        "a raw-pushed branch opens only once recorded, and the next push publishes the record",
        vec![
            Step::git(&a(), Repo::Proj, &["switch", "-c", "feat-x"]),
            Step::commit(&a(), Repo::Proj, "raw-feat"),
            Step::git(&a(), Repo::Proj, &["push", "origin", "feat-x"])
                .pushes(Remote::origin(Repo::Proj), "feat-x"),
            Step::git(&a(), Repo::Proj, &["switch", "main"]),
            Step::new_machine(Machine::B),
            Step::rwv(
                &b(),
                &[
                    "workweave",
                    "create",
                    "--project",
                    "proj",
                    "ww",
                    "--branch",
                    "feat-x",
                ],
            )
            .expect(Exit::refuses(Repo::Proj, State::TargetUnrecorded))
            .claim(Claim::OutputContains("unrecorded"))
            .claim(Claim::OutputContains("rwv fetch"))
            .claim(Claim::OutputContains("rwv branch set-mode feat-x declared"))
            .claim(Claim::RecordIs {
                machine: Machine::B,
                branch: "feat-x",
                record: None,
            }),
            Step::rwv(&b(), &["fetch"]).expect(Exit::Succeeds),
            Step::rwv(
                &b(),
                &[
                    "branch", "set-mode", "feat-x", "feature", "--target", "main",
                ],
            )
            .expect(Exit::Succeeds)
            .claim(Claim::RecordIs {
                machine: Machine::B,
                branch: "feat-x",
                record: Some(feature_onto("main", Mode::Declared)),
            }),
            Step::rwv(
                &b(),
                &[
                    "workweave",
                    "create",
                    "--project",
                    "proj",
                    "ww",
                    "--branch",
                    "feat-x",
                ],
            )
            .expect(Exit::Succeeds)
            .claim(Claim::OnBranch {
                at: b_ww("ww"),
                repo: Repo::Proj,
                branch: "feat-x",
            }),
            publishes_records(Step::rwv(&b_ww("ww"), &["push", "--branch", "feat-x"]))
                .pushes(Remote::origin(Repo::Proj), "feat-x")
                .expect(Exit::Succeeds)
                .claim(Claim::RemoteRecordIs {
                    branch: "feat-x",
                    record: Some(feature_onto("main", Mode::Declared)),
                }),
        ],
    )
}

pub fn project_branch_elsewhere() -> Scenario {
    scenario(
        "a project branch checked out in another workspace is not taken, and can be looked at detached",
        vec![
            Step::rwv(
                &a(),
                &[
                    "workweave",
                    "create",
                    "--project",
                    "proj",
                    "ww1",
                    "--new-branch",
                    "feat-x",
                ],
            )
            .expect(Exit::Succeeds),
            Step::rwv(&a(), &["switch", "feat-x"])
                .expect(Exit::refuses(Repo::Proj, State::CheckoutInAnotherWorkspace))
                .claim(Claim::OutputContains("checkout"))
                .claim(Claim::OutputContains("proj--ww1"))
                .claim(Claim::LocalStateUnchanged(a())),
            Step::rwv(
                &a(),
                &[
                    "workweave",
                    "create",
                    "--project",
                    "proj",
                    "ww2",
                    "--branch",
                    "feat-x",
                ],
            )
            .expect(Exit::refuses(Repo::Proj, State::CheckoutInAnotherWorkspace))
            .claim(Claim::OutputContains("checkout"))
            .claim(Claim::OutputContains("proj--ww1"))
            .claim(Claim::OutputContains(
                "rwv workweave create <new> --detach feat-x",
            ))
            .claim(Claim::LocalStateUnchanged(a())),
            Step::rwv(
                &a(),
                &[
                    "workweave",
                    "create",
                    "--project",
                    "proj",
                    "ww2",
                    "--detach",
                    "feat-x",
                ],
            )
            .expect(Exit::Succeeds)
            .claim(Claim::LocalStateUnchanged(a())),
        ],
    )
}

pub fn create_diverged_at() -> Scenario {
    scenario(
        "a create at a branch that diverged from its upstream refuses and starts at the local branch with --at",
        vec![
            Step::commit(&a(), Repo::Proj, "local-landing"),
            Step::forge(Forge::Commit {
                repo: Repo::Proj,
                branch: "main",
                mark: "teammate",
            }),
            Step::rwv(&a(), &["fetch"]).expect(Exit::Succeeds),
            Step::rwv(&a(), &["workweave", "create", "--project", "proj", "ww"])
                .expect(Exit::refuses(Repo::Proj, State::PushBranchDiverged))
                .claim(Claim::OutputContains("diverged"))
                .claim(Claim::OutputContains("--at main"))
                .claim(Claim::LocalStateUnchanged(a())),
            Step::rwv(
                &a(),
                &[
                    "workweave",
                    "create",
                    "--project",
                    "proj",
                    "ww",
                    "--at",
                    "main",
                ],
            )
            .expect(Exit::Succeeds)
            .claim(Claim::Contains {
                at: ww("ww"),
                repo: Repo::Proj,
                rev: "HEAD",
                mark: "local-landing",
            }),
        ],
    )
}

pub fn workweave_create_dirty_parent() -> Scenario {
    let declared = declared_branch_text(Repo::Lib, "main");
    scenario(
        "a create leaves the parent's uncommitted manifest where it is, reported",
        vec![
            Step::hand_edit(
                &a(),
                Repo::Proj,
                "rwv.toml",
                &declared,
                &format!("{declared} # edited"),
            ),
            Step::rwv(&a(), &["workweave", "create", "--project", "proj", "ww"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains("rwv.toml"))
                .claim(Claim::OutputContains("stay there"))
                .claim(Claim::OnBranch {
                    at: a(),
                    repo: Repo::Proj,
                    branch: "main",
                }),
        ],
    )
}

pub fn dc19_idempotent_create() -> Scenario {
    scenario(
        "dc19: the same create twice returns the workweave; a different spelling refuses",
        vec![
            create("ww"),
            Step::rwv(&a(), &["workweave", "create", "--project", "proj", "ww"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains("returned as it is"))
                .claim(Claim::LocalStateUnchanged(ww("ww"))),
            Step::rwv(
                &a(),
                &[
                    "workweave",
                    "create",
                    "--project",
                    "proj",
                    "ww",
                    "--new-branch",
                    "other",
                ],
            )
            .expect(Exit::refuses(Repo::Proj, State::NameTaken))
            .claim(Claim::OutputContains("name taken"))
            .claim(Claim::LocalStateUnchanged(ww("ww"))),
        ],
    )
}

pub fn dc08_set_mode_own_workspace() -> Scenario {
    scenario(
        "dc08: set-mode in the primary leaves a workweave on the branch as it is",
        vec![
            Step::rwv(
                &a(),
                &[
                    "workweave",
                    "create",
                    "--project",
                    "proj",
                    "ww1",
                    "--new-branch",
                    "feat-x",
                ],
            )
            .expect(Exit::Succeeds),
            Step::rwv(&a(), &["branch", "set-mode", "feat-x", "declared"])
                .expect(Exit::Succeeds)
                .claim(Claim::OnBranch {
                    at: ww("ww1"),
                    repo: Repo::Proj,
                    branch: "feat-x",
                })
                .claim(Claim::OnBranch {
                    at: ww("ww1"),
                    repo: Repo::App,
                    branch: "feat-x",
                }),
            Step::rwv(&ww("ww1"), &["status"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains("alignment"))
                .claim(Claim::OutputContains("rwv update")),
        ],
    )
}

pub fn detached_review_lib_fix() -> Scenario {
    scenario(
        "detached review workspace: an unrelated lib commit is kept on a branch, then update returns lib to its pin",
        vec![
            Step::rwv(
                &a(),
                &[
                    "workweave",
                    "create",
                    "--project",
                    "proj",
                    "rv",
                    "--detach",
                    "main",
                ],
            )
            .expect(Exit::Succeeds)
            .claim(Claim::AtPin {
                at: ww("rv"),
                repo: Repo::Lib,
            }),
            Step::commit(&ww("rv"), Repo::Lib, "lib-fix"),
            Step::rwv(&ww("rv"), &["update"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains("detached with unpushed commits"))
                .claim(Claim::OutputContains("branch <name> <sha>")),
            Step::git(&ww("rv"), Repo::Lib, &["branch", "lib-fix-branch"]),
            Step::rwv(&ww("rv"), &["update"])
                .expect(Exit::Succeeds)
                .claim(Claim::AtPin {
                    at: ww("rv"),
                    repo: Repo::Lib,
                })
                .claim(Claim::Contains {
                    at: ww("rv"),
                    repo: Repo::Lib,
                    rev: "lib-fix-branch",
                    mark: "lib-fix",
                }),
            Step::git(&ww("rv"), Repo::Lib, &["push", "origin", "lib-fix-branch"])
                .pushes(Remote::origin(Repo::Lib), "lib-fix-branch"),
        ],
    )
}

/// `set-history merge --repo lib`, then `--unset`: each commits, and the
/// committed `rwv.toml` returns to what it held before.
pub fn history_set_unset() -> Scenario {
    let lib = "local/remotes/lib";
    scenario(
        "history: set a member's setting, then unset it",
        vec![
            Step::rwv(&a(), &["branch", "set-history", "merge", "--repo", lib])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains(
                    "rwv branch set-history: local/remotes/lib: history unset -> merge",
                ))
                .claim(Claim::OutputContains("Committed rwv.toml")),
            Step::git(&a(), Repo::Proj, &["show", "HEAD:rwv.toml"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains("history = \"merge\"")),
            Step::rwv(&a(), &["branch", "set-history", "--unset", "--repo", lib])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains(
                    "rwv branch set-history: local/remotes/lib: history merge -> unset",
                ))
                .claim(Claim::OutputContains("Committed rwv.toml")),
            Step::git(&a(), Repo::Proj, &["show", "HEAD:rwv.toml"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputLacks("history")),
            Step::rwv(&a(), &["branch", "set-history", "--unset", "--repo", lib])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains("nothing to change"))
                .claim(Claim::LocalStateUnchanged(a())),
        ],
    )
}

/// `proj` and `proj2` record different `history` for `lib`: `doctor`
/// reports it. How each project's landings follow its own setting needs
/// `land`.
pub fn history_disagreement() -> Scenario {
    let lib = "local/remotes/lib";
    Scenario {
        name: "history: two projects record different settings for a shared member",
        seed: Seed::default().second_project(SecondProject { branch: "main" }),
        steps: vec![
            Step::clone_project(Machine::A, Repo::Proj2).expect(Exit::Succeeds),
            Step::rwv(&a(), &["branch", "set-history", "merge", "--repo", lib])
                .expect(Exit::Succeeds),
            Step::rwv(&a(), &["doctor"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputLacks("history disagreement")),
            Step::rwv(
                &a(),
                &[
                    "branch",
                    "set-history",
                    "--project",
                    "proj2",
                    "linear",
                    "--repo",
                    lib,
                ],
            )
            .expect(Exit::Succeeds),
            Step::rwv(&a(), &["doctor"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains(
                    "history disagreement: local/remotes/lib",
                ))
                .claim(Claim::OutputContains("proj main: merge"))
                .claim(Claim::OutputContains("proj2 main: linear")),
            Step::pending(
                "land into proj's main by merge and into proj2's by rebase",
                MissingVerb::Land,
            ),
        ],
    }
}

/// Top-level `linear`, `lib` overridden to `merge`: what is stored. That one
/// landing rebases `app` and merges `lib` needs `land`.
pub fn history_member_override() -> Scenario {
    let lib = "local/remotes/lib";
    scenario(
        "history: a member's own setting beside the top-level one",
        vec![
            Step::rwv(&a(), &["branch", "set-history", "linear"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains(
                    "rwv branch set-history: projects/proj: history unset -> linear",
                )),
            Step::rwv(&a(), &["branch", "set-history", "merge", "--repo", lib])
                .expect(Exit::Succeeds),
            Step::git(&a(), Repo::Proj, &["show", "HEAD:rwv.toml"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains("history = \"linear\""))
                .claim(Claim::OutputContains("history = \"merge\"")),
            create("over"),
            Step::commit(&ww("over"), Repo::App, "over-app"),
            Step::commit(&ww("over"), Repo::Lib, "over-lib"),
            Step::pending("land, rebasing app and merging lib", MissingVerb::Land),
        ],
    )
}

/// Hand edits of `[integrations.cargo-workspace]`, `[workweave]` and the
/// top-level `history`: `lock --commit` commits all three in one commit,
/// running set-history's step, and a create then runs.
pub fn lock_commit_whole_file() -> Scenario {
    scenario(
        "lock --commit: the whole hand-edited rwv.toml in one commit",
        vec![
            Step::hand_edit(
                &a(),
                Repo::Proj,
                "rwv.toml",
                "[integrations.vscode-workspace]\nenabled = false\n\n[repositories]\n",
                "history = \"merge\"\n\n[integrations.vscode-workspace]\nenabled = false\n\n\
                 [integrations.cargo-workspace]\nenabled = false\n\n[workweave]\n\
                 copy = [\"README\"]\n\n[repositories]\n",
            ),
            Step::rwv(&a(), &["lock", "--commit"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains(
                    "rwv lock: projects/proj: history unset -> merge, committed as the file has it",
                ))
                .claim(Claim::OutputContains(
                    "the other tables of rwv.toml changed",
                )),
            Step::git(&a(), Repo::Proj, &["show", "HEAD:rwv.toml"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains("history = \"merge\""))
                .claim(Claim::OutputContains("[integrations.cargo-workspace]"))
                .claim(Claim::OutputContains("[workweave]")),
            Step::git(
                &a(),
                Repo::Proj,
                &["diff", "--exit-code", "HEAD", "--", "rwv.toml"],
            )
            .expect(Exit::Succeeds),
            create("seat"),
        ],
    )
}

/// `feat-x` in A's primary, onto `main`, published with its members; `stable`
/// recorded `declared` and published from B.
fn feat_x_published_beside_stable() -> Vec<Step> {
    let mut steps = vec![Step::new_machine(Machine::B)];
    steps.extend(feature_branch_in_primary());
    steps.extend([
        publishes_records(Step::rwv(&a(), &["push"]))
            .pushes(Remote::origin(Repo::App), "feat-x")
            .pushes(Remote::origin(Repo::Proj), "feat-x")
            .expect(Exit::Succeeds),
        Step::rwv(&a(), &["update"]).expect(Exit::Succeeds),
        Step::git(&b(), Repo::Proj, &["branch", "stable"]),
        Step::rwv(&b(), &["branch", "set-mode", "stable", "declared"]).expect(Exit::Succeeds),
        publishes_records(Step::rwv(&b(), &["push", "--branch", "stable"]))
            .pushes(Remote::origin(Repo::Proj), "stable")
            .expect(Exit::Succeeds),
    ]);
    steps
}

/// B retargets `feat-x` onto `stable` and publishes the record; A fetches it.
fn feat_x_retargeted_on_b() -> Vec<Step> {
    vec![
        Step::rwv(&b(), &["fetch"]).expect(Exit::Succeeds),
        Step::git(&b(), Repo::Proj, &["branch", "feat-x", "origin/feat-x"]),
        Step::rwv(
            &b(),
            &[
                "branch", "set-mode", "feat-x", "feature", "--target", "stable",
            ],
        )
        .expect(Exit::Succeeds),
        publishes_records(Step::rwv(&b(), &["push", "--branch", "feat-x"])).expect(Exit::Succeeds),
        Step::rwv(&a(), &["fetch"]).expect(Exit::Succeeds),
    ]
}

/// A record retargeted on another machine reaches this one by fetch: `status`
/// reports it, and `sync` refuses before anything moves, offering to accept
/// it or to restore the old target, until one of them is taken.
pub fn record_retargeted_elsewhere_is_reported_and_refused() -> Scenario {
    let refused = |step: Step| {
        step.expect(Exit::refuses(Repo::Proj, State::TargetRecordChanged))
            .claim(Claim::OutputContains(
                "projects/proj: target record changed (feat-x)",
            ))
            .claim(Claim::OutputContains("rwv update"))
            .claim(Claim::OutputContains(
                "rwv branch set-mode feat-x feature --target main",
            ))
    };
    let mut steps = feat_x_published_beside_stable();
    steps.extend(feat_x_retargeted_on_b());
    steps.extend([
        Step::rwv(&a(), &["status"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputContains(
                "projects/proj: target record changed (feat-x)",
            ))
            .claim(Claim::OutputContains(
                "was: feature onto main (as declared)",
            ))
            .claim(Claim::OutputContains(
                "now: feature onto stable (as declared)",
            ))
            .claim(Claim::OutputContains("arrived by fetch")),
        refused(Step::rwv(&a(), &["sync"])).claim(Claim::OnBranch {
            at: a(),
            repo: Repo::App,
            branch: "feat-x",
        }),
        refused(Step::rwv(&a(), &["sync"])).claim(Claim::LocalStateUnchanged(a())),
    ]);
    scenario("a record retargeted on another machine", steps)
}

/// `rwv update` accepts a record changed elsewhere: it reports the change once
/// and stores it, and the next `sync` takes in the new target.
pub fn record_retargeted_elsewhere_accepted_by_update() -> Scenario {
    let mut steps = feat_x_published_beside_stable();
    steps.extend(feat_x_retargeted_on_b());
    steps.extend([
        Step::rwv(&a(), &["update"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputContains(
                "projects/proj: target record changed (feat-x)",
            )),
        Step::rwv(&a(), &["status"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputLacks("record changed")),
        Step::rwv(&a(), &["sync"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputLacks("record changed"))
            .claim(Claim::OutputContains(
                "projects/proj: already contains stable",
            )),
    ]);
    scenario("a record changed elsewhere, accepted", steps)
}

/// A record this workspace writes is never reported to it.
pub fn record_retargeted_here_is_not_reported() -> Scenario {
    let mut steps = feature_branch_in_primary();
    steps.extend([
        Step::git(&a(), Repo::Proj, &["branch", "stable", "main"]),
        Step::rwv(&a(), &["branch", "set-mode", "stable", "declared"]).expect(Exit::Succeeds),
        Step::rwv(
            &a(),
            &[
                "branch", "set-mode", "feat-x", "feature", "--target", "stable",
            ],
        )
        .expect(Exit::Succeeds),
        Step::rwv(&a(), &["status"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputLacks("record changed")),
        Step::rwv(&a(), &["sync"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputLacks("record changed"))
            .claim(Claim::OutputContains(
                "projects/proj: already contains stable",
            )),
    ]);
    scenario("a record changed here", steps)
}

/// A workspace that stored no record takes the one `sync` reads without a
/// word, and a later change elsewhere is reported against it.
pub fn record_unstored_is_stored_by_sync() -> Scenario {
    let mut steps = feat_x_published_beside_stable();
    steps.extend([
        Step::delete_workspace_state(&a()),
        Step::rwv(&a(), &["sync"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputLacks("record changed")),
    ]);
    steps.extend(feat_x_retargeted_on_b());
    steps.push(
        Step::rwv(&a(), &["status"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputContains(
                "was: feature onto main (as declared)",
            )),
    );
    scenario("no stored record", steps)
}

/// `feat-x` in the primary, its `app` and `lib` commits published.
fn published_feature() -> Vec<Step> {
    let mut steps = two_member_feature_in_primary();
    steps.push(push_feat_x_everywhere(Step::rwv(&a(), &["push"])).expect(Exit::Succeeds));
    steps
}

/// follow-never-backward: `app`'s PR merges on the forge by a merge commit;
/// `sync` moves `app` forward to it and pins it, and a second `sync` finds
/// nothing to do.
pub fn follow_never_backward() -> Scenario {
    let mut steps = published_feature();
    steps.extend([
        Step::forge(Forge::Merge {
            repo: Repo::App,
            branch: "feat-x",
            how: MergeHow::Merge,
        }),
        Step::rwv(&a(), &["sync"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputContains("local/remotes/app: fast-forwarded"))
            .claim(Claim::PinContains {
                site: LockSite::Checkout(a(), Repo::Proj, "feat-x".to_owned()),
                member: Repo::App,
                mark: "pr-app",
            })
            .claim(Claim::PinContains {
                site: LockSite::Checkout(a(), Repo::Proj, "feat-x".to_owned()),
                member: Repo::Lib,
                mark: "pr-lib",
            }),
        Step::rwv(&a(), &["sync"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputContains("already contains main")),
    ]);
    scenario("follow-never-backward", steps)
}

/// no-c3-after-reset: `app`'s PR is squashed on the forge; `sync
/// --reset-to-target` moves `app` to `main` keeping a backup and listing the
/// work it drops; the reset branch, diverged from its push branch with nothing
/// unpushed, is a fact to the next `sync`, and `push` skips it with no C3.
pub fn no_c3_after_reset() -> Scenario {
    let mut steps = published_feature();
    steps.extend([
        Step::forge(Forge::Merge {
            repo: Repo::App,
            branch: "feat-x",
            how: MergeHow::Squash,
        }),
        Step::rwv(&a(), &["sync", "--reset-to-target", "local/remotes/app"])
            .expect(Exit::Succeeds)
            .may_rewrite(&[Repo::App])
            .may_rewind(&[Repo::App])
            .claim(Claim::BackedUp {
                repo: Repo::App,
                mark: "pr-app",
                marked: None,
            })
            .claim(Claim::OutputContains(
                "new work the reset leaves in the backup",
            ))
            .claim(Claim::OutputContains("cherry-pick"))
            .claim(Claim::OutputContains("merge --no-ff")),
        Step::rwv(&a(), &["sync"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputContains("(landed)")),
        push_feat_x_everywhere(Step::rwv(&a(), &["push"]))
            .expect(Exit::Succeeds)
            .claim(Claim::OutputLacks("--reset-to-upstream"))
            .claim(Claim::OutputLacks("pin unmerged")),
    ]);
    scenario("no-c3-after-reset", steps)
}

/// no-empty-merge: `sync --from main` in a workweave whose `docs` already
/// holds `main`'s pin merges `app` and the project and leaves `docs`.
pub fn no_empty_merge() -> Scenario {
    scenario(
        "no-empty-merge",
        vec![
            create("w"),
            Step::commit(&ww("w"), Repo::App, "w-app"),
            Step::rwv(&ww("w"), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::commit(&a(), Repo::App, "main-app"),
            Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::rwv(&ww("w"), &["sync", "--from", "main"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains("local/remotes/app: merged"))
                .claim(Claim::OutputLacks("local/remotes/docs: merged"))
                .claim(Claim::OutputLacks("local/remotes/lib: merged"))
                .claim(Claim::Contains {
                    at: ww("w"),
                    repo: Repo::App,
                    rev: "proj--w",
                    mark: "main-app",
                })
                .claim(Claim::Contains {
                    at: ww("w"),
                    repo: Repo::App,
                    rev: "proj--w",
                    mark: "w-app",
                }),
            Step::rwv(&ww("w"), &["sync", "--from", "main"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains("already contains main")),
        ],
    )
}

/// The lineage rule: `main`'s `app` is rewritten under the feature's base, so
/// `sync` refuses before anything moves; the reset to target clears it.
pub fn sync_refuses_off_lineage() -> Scenario {
    scenario(
        "sync refuses work built on another lineage",
        vec![
            Step::git(&a(), Repo::App, &["branch", "other"]),
            Step::commit(&a(), Repo::App, "cut-app"),
            Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
            create("w"),
            Step::commit(&ww("w"), Repo::App, "w-app"),
            Step::rwv(&ww("w"), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::rwv(
                &a(),
                &["member", "set-branch", "local/remotes/app", "other"],
            )
            .expect(Exit::Succeeds)
            .may_rewind(&[Repo::App]),
            Step::rwv(&ww("w"), &["sync", "--strategy", "rebase"])
                .expect(Exit::refuses(Repo::App, State::PinOffLineage))
                .claim(Claim::OutputContains("local/remotes/app: pin off lineage"))
                .claim(Claim::OutputContains(
                    "rwv sync --reset-to-target local/remotes/app",
                ))
                .claim(Claim::LocalStateUnchanged(ww("w"))),
            Step::rwv(
                &ww("w"),
                &[
                    "sync",
                    "--strategy",
                    "rebase",
                    "--reset-to-target",
                    "local/remotes/app",
                ],
            )
            .expect(Exit::Succeeds)
            .may_rewrite(&[Repo::App])
            .may_rewind(&[Repo::App])
            .claim(Claim::BackedUp {
                repo: Repo::App,
                mark: "w-app",
                marked: None,
            }),
        ],
    )
}

/// A range holding a merge is never rebased: `sync --strategy rebase` refuses
/// it and names the merge that keeps it.
pub fn sync_never_rebases_a_merge() -> Scenario {
    scenario(
        "sync never rebases a range holding a merge",
        vec![
            create("w"),
            Step::git(&ww("w"), Repo::App, &["switch", "-c", "side"]),
            Step::commit(&ww("w"), Repo::App, "side-app"),
            Step::git(&ww("w"), Repo::App, &["switch", "proj--w"]),
            Step::commit(&ww("w"), Repo::App, "w-app"),
            Step::git(
                &ww("w"),
                Repo::App,
                &["merge", "--no-ff", "-m", "bump", "side"],
            ),
            Step::rwv(&ww("w"), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::commit(&a(), Repo::App, "main-app"),
            Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::rwv(&ww("w"), &["sync", "--strategy", "rebase"])
                .expect(Exit::refuses_kind(RefusalKind::NotFastForwardable))
                .claim(Claim::OutputContains("rwv sync --from main"))
                .claim(Claim::LocalStateUnchanged(ww("w"))),
            Step::rwv(&ww("w"), &["sync", "--from", "main"])
                .expect(Exit::Succeeds)
                .claim(Claim::Contains {
                    at: ww("w"),
                    repo: Repo::App,
                    rev: "proj--w",
                    mark: "side-app",
                }),
        ],
    )
}

/// A rebase that would rewrite a branch other features land into refuses,
/// naming them; a fast-forward does not.
pub fn sync_refuses_to_rewrite_under_dependents() -> Scenario {
    scenario(
        "sync refuses to rewrite a branch with dependents",
        vec![
            create("w"),
            Step::rwv(
                &ww("w"),
                &["workweave", "create", "--project", "proj", "w2"],
            )
            .expect(Exit::Succeeds),
            Step::commit(&a(), Repo::Lib, "main-lib"),
            Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::rwv(&ww("w"), &["sync"]).expect(Exit::Succeeds),
            Step::commit(&ww("w"), Repo::App, "w-app"),
            Step::commit(&a(), Repo::App, "main-app"),
            Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::rwv(&ww("w"), &["sync", "--strategy", "rebase"])
                .expect(Exit::refuses(Repo::Proj, State::TargetDependents))
                .claim(Claim::OutputContains("target dependents"))
                .claim(Claim::LocalStateUnchanged(ww("w"))),
        ],
    )
}

/// The three ways a member branch stands against a push branch it diverged
/// from: rewritten upstream (C3, C2), unpushed commits (C2, C3); the third,
/// landed, is `no_c3_after_reset`'s.
pub fn sync_judges_a_diverged_push_branch(rewritten: bool) -> Scenario {
    let mut steps = published_feature();
    if rewritten {
        steps.extend(
            Step::forge_force_push(Repo::App, "feat-x", "rewritten-app")
                .into_iter()
                .map(|step| step.may_leave_unresolved(&[Repo::App])),
        );
        steps.push(Step::rwv(&a(), &["fetch"]).expect(Exit::Succeeds));
    } else {
        steps.extend([
            Step::forge(Forge::Commit {
                repo: Repo::App,
                branch: "feat-x",
                mark: "theirs-app",
            }),
            Step::rwv(&a(), &["fetch"]).expect(Exit::Succeeds),
            Step::commit(&a(), Repo::App, "mine-app"),
        ]);
    }
    let mut refusal = Step::rwv(&a(), &["sync"])
        .expect(Exit::refuses(Repo::App, State::PushBranchDiverged))
        .claim(Claim::OutputContains(
            "local/remotes/app: push-branch diverged",
        ))
        .claim(Claim::OutputContains("--merge-upstream"));
    if rewritten {
        refusal = refusal.claim(Claim::OutputContains("rewritten"));
    }
    steps.push(refusal);
    Scenario {
        name: if rewritten {
            "sync refuses a member whose upstream was rewritten"
        } else {
            "sync refuses a member holding unpushed commits beside its upstream's"
        },
        seed: Seed::default(),
        steps,
    }
}

/// absent-pin-unmerged: `main`'s tip pins a `lib` commit this clone lacks;
/// `sync` refuses with C17, C25, C19 rather than judge on it.
pub fn absent_pin_unmerged() -> Scenario {
    scenario(
        "absent-pin-unmerged",
        vec![
            create("w"),
            Step::new_machine(Machine::B),
            Step::commit(&b(), Repo::Lib, "b-lib"),
            Step::rwv(&b(), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::git(&b(), Repo::Proj, &["push", "origin", "main"])
                .pushes(Remote::origin(Repo::Proj), "main")
                .may_leave_unresolved(&[Repo::Lib]),
            Step::rwv(&ww("w"), &["sync"])
                .may_leave_unresolved(&[Repo::Lib])
                .expect(Exit::refuses(Repo::Lib, State::PinUnreachable))
                .claim(Claim::OutputContains("local/remotes/lib: pin unreachable"))
                .claim(Claim::OutputContains("rwv member pin"))
                .claim(Claim::LocalStateUnchanged(ww("w"))),
        ],
    )
}

/// A target whose live tip names the member-entry keys an earlier release
/// wrote refuses `sync` at that branch, naming the repair where it is checked
/// out; that repair clears it.
pub fn sync_refuses_a_pre_schema_target_tip() -> Scenario {
    scenario(
        "sync refuses a pre-schema target tip",
        vec![
            create("w"),
            Step::hand_edit(
                &a(),
                Repo::Proj,
                "rwv.toml",
                &declared_branch_text(Repo::App, "main"),
                "app.git\"\nversion = \"main\"",
            ),
            Step::git(&a(), Repo::Proj, &["commit", "-am", "old keys"]),
            Step::rwv(&ww("w"), &["sync"])
                .expect(Exit::refuses(Repo::Proj, State::UnparseableProjectAt))
                .claim(Claim::OutputContains("only that, in "))
                .claim(Claim::OutputContains(
                    "machine-a/weave\n    rwv doctor --fix",
                ))
                .claim(Claim::LocalStateUnchanged(ww("w"))),
            Step::rwv(&a(), &["doctor", "--fix"]).expect(Exit::Succeeds),
            Step::rwv(&ww("w"), &["sync"]).expect(Exit::Succeeds),
        ],
    )
}

/// `w`'s `app` published, then rebased by `sync` onto `main`'s newer pin: its
/// base is that pin, which the published `app` and the published lock's pin
/// of it lack.
fn feature_rebased_past_its_push_branch() -> Vec<Step> {
    vec![
        Step::rwv(
            &a(),
            &[
                "workweave",
                "create",
                "--project",
                "proj",
                "w",
                "--new-branch",
                "feat-w",
            ],
        )
        .expect(Exit::Succeeds),
        Step::commit(&ww("w"), Repo::App, "w-app"),
        Step::rwv(&ww("w"), &["lock", "--commit"]).expect(Exit::Succeeds),
        publishes_records(Step::rwv(&ww("w"), &["push"]))
            .pushes(Remote::origin(Repo::App), "feat-w")
            .pushes(Remote::origin(Repo::Proj), "feat-w")
            .expect(Exit::Succeeds),
        Step::commit(&a(), Repo::App, "main-app"),
        Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::rwv(&ww("w"), &["sync", "--strategy", "rebase"])
            .expect(Exit::Succeeds)
            .may_rewrite(&[Repo::App, Repo::Proj])
            .claim(Claim::Contains {
                at: ww("w"),
                repo: Repo::App,
                rev: "feat-w",
                mark: "main-app",
            }),
    ]
}

/// The lineage rule on `pull --merge-upstream`: the project branch and its
/// upstream both re-pinned `app`, so the merge would merge `app` with the pin
/// the upstream holds, which lacks the base `w`'s `app` was rebased onto;
/// `pull` refuses before anything moves and names C3, since that pin's own
/// base is on `main`. C3, run as printed, takes the upstream's lock.
pub fn merge_upstream_off_lineage() -> Scenario {
    let mut steps = feature_rebased_past_its_push_branch();
    steps.extend([
        Step::rwv(&ww("w"), &["pull", "--merge-upstream"])
            .expect(Exit::refuses(Repo::App, State::PinOffLineage))
            .claim(Claim::OutputContains(
                "rwv pull --reset-to-upstream projects/proj",
            ))
            .claim(Claim::LocalStateUnchanged(ww("w"))),
        Step::rwv(&ww("w"), &["pull", "--reset-to-upstream", "projects/proj"])
            .expect(Exit::Succeeds)
            .may_rewrite(&[Repo::Proj])
            .may_rewind(&[Repo::Proj, Repo::App])
            .claim(Claim::OutputContains("to its upstream"))
            .claim(Claim::OutputContains("in backup")),
    ]);
    scenario("merge-upstream off lineage", steps)
}

/// `main` moved `app`, and `stable`, a branch `w` does not land into, is cut
/// from it.
fn stable_ahead_of_w() -> Vec<Step> {
    vec![
        create("w"),
        Step::commit(&a(), Repo::App, "main-app"),
        Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::git(&a(), Repo::Proj, &["branch", "stable", "main"]),
        Step::rwv(&a(), &["branch", "set-mode", "stable", "declared"]).expect(Exit::Succeeds),
    ]
}

/// A merge naming another branch than the record's target is a merge commit
/// even where a fast-forward would do: `w`'s `app` holds nothing `stable`'s
/// pin lacks, and `sync --from stable` still merges it.
pub fn sync_from_another_branch_always_merges() -> Scenario {
    let mut steps = stable_ahead_of_w();
    steps.extend([
        Step::rwv(&ww("w"), &["sync", "--from", "stable"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputContains("local/remotes/app: merged"))
            .claim(Claim::Contains {
                at: ww("w"),
                repo: Repo::App,
                rev: "proj--w",
                mark: "main-app",
            }),
        Step::git(
            &ww("w"),
            Repo::App,
            &["rev-parse", "--verify", "--quiet", "proj--w^2"],
        ),
    ]);
    scenario("sync --from another branch always merges", steps)
}

/// `sync` reads the record and never writes it, a one-run `--target`
/// included.
pub fn sync_writes_no_record() -> Scenario {
    let mut steps = stable_ahead_of_w();
    steps.extend([
        Step::rwv(&ww("w"), &["sync", "--target", "stable"])
            .expect(Exit::Succeeds)
            .claim(Claim::Contains {
                at: ww("w"),
                repo: Repo::App,
                rev: "proj--w",
                mark: "main-app",
            })
            .claim(Claim::RecordIs {
                machine: Machine::A,
                branch: "proj--w",
                record: Some(feature_onto("main", Mode::Declared)),
            }),
        Step::rwv(&ww("w"), &["sync"])
            .expect(Exit::Succeeds)
            .claim(Claim::RecordIs {
                machine: Machine::A,
                branch: "proj--w",
                record: Some(feature_onto("main", Mode::Declared)),
            }),
    ]);
    scenario("sync writes no record", steps)
}

/// waiver-own-lock: a raw merge into `feat-w`, its lock resolved by hand to
/// its own side, leaves `w2`'s `app` commit unjoined, which `push` refuses;
/// `sync --reset-to-target` then re-pins `app` backward by name in sync's own
/// lock commit, to `main`'s pin, which lacks that commit, and the push passes
/// listing the merge it waived and the verb that waived it.
pub fn waiver_own_lock() -> Scenario {
    let app = Repo::App.rel_path();
    scenario(
        "waiver-own-lock",
        vec![
            Step::rwv(
                &a(),
                &[
                    "workweave",
                    "create",
                    "--project",
                    "proj",
                    "w",
                    "--new-branch",
                    "feat-w",
                ],
            )
            .expect(Exit::Succeeds),
            create("w2"),
            Step::commit(&ww("w"), Repo::App, "w-app"),
            Step::rwv(&ww("w"), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::commit(&ww("w2"), Repo::App, "w2-app"),
            Step::rwv(&ww("w2"), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::git(&ww("w"), Repo::Proj, &["merge", "--no-edit", "proj--w2"])
                .expect(Exit::Code(1)),
            Step::git(&ww("w"), Repo::Proj, &["checkout", "--ours", "rwv.lock"]),
            Step::git(&ww("w"), Repo::Proj, &["add", "rwv.lock"]),
            Step::git(&ww("w"), Repo::Proj, &["commit", "--no-edit"]),
            publishes_records(Step::rwv(&ww("w"), &["push"]))
                .pushes(Remote::origin(Repo::App), "feat-w")
                .expect(Exit::refuses(Repo::App, State::PinUnmerged))
                .claim(Claim::OutputLacks("pin waived"))
                .claim(Claim::RemoteLacks {
                    repo: Repo::Proj,
                    branch: "feat-w",
                }),
            Step::rwv(&ww("w"), &["sync", "--reset-to-target", &app])
                .expect(Exit::Succeeds)
                .may_rewrite(&[Repo::App])
                .may_rewind(&[Repo::App])
                .claim(Claim::BackedUp {
                    repo: Repo::App,
                    mark: "w-app",
                    marked: None,
                }),
            publishes_records(Step::rwv(&ww("w"), &["push"]))
                .pushes(Remote::origin(Repo::Proj), "feat-w")
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains("local/remotes/app: pin waived"))
                .claim(Claim::OutputContains("by `rwv sync`")),
        ],
    )
}

/// An uncommitted `rwv.toml` and a member on a branch of its own never refuse
/// each other's only choice: `lock --commit` refuses the member, naming
/// `update`, and `update` aligns it under the uncommitted file, leaving the
/// file as it stands, so `lock --commit` then commits it.
pub fn uncommitted_manifest_beside_an_unaligned_member() -> Scenario {
    let declared = declared_branch_text(Repo::App, "main");
    let edited = format!("{declared}\nrecurse-submodules = false");
    scenario(
        "an uncommitted rwv.toml beside an unaligned member",
        vec![
            Step::git(&a(), Repo::Lib, &["switch", "-c", "side"]),
            Step::hand_edit(&a(), Repo::Proj, "rwv.toml", &declared, &edited),
            Step::rwv(&a(), &["lock", "--commit"])
                .expect(Exit::refuses(Repo::Lib, State::AlignmentOnBranch))
                .claim(Claim::OutputContains("rwv update"))
                .claim(Claim::LocalStateUnchanged(a())),
            Step::rwv(&a(), &["update"])
                .expect(Exit::Succeeds)
                .claim(Claim::OnBranch {
                    at: a(),
                    repo: Repo::Lib,
                    branch: "main",
                }),
            Step::git(
                &a(),
                Repo::Proj,
                &["diff", "--quiet", "HEAD", "--", "rwv.lock"],
            ),
            Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::git(&a(), Repo::Proj, &["diff", "--quiet", "HEAD"]),
        ],
    )
}

/// `app`'s PR squashed on the forge and its branch deleted: the feature's
/// pin is off every remote, and its change matches the squash on `main`, so
/// `fetch` names the match and C22, which, run as printed, resets `app`.
pub fn squashed_pin_off_remote_offers_the_reset() -> Scenario {
    let mut steps = published_feature();
    steps.extend([
        Step::forge(Forge::Merge {
            repo: Repo::App,
            branch: "feat-x",
            how: MergeHow::Squash,
        }),
        Step::forge(Forge::DeleteBranch {
            repo: Repo::App,
            branch: "feat-x",
        })
        .may_leave_unresolved(&[Repo::App]),
        Step::rwv(&a(), &["fetch"])
            .may_leave_unresolved(&[Repo::App])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputContains("local/remotes/app: pin off remote"))
            .claim(Claim::OutputContains("patch-equivalent: "))
            .claim(Claim::OutputContains(
                "rwv sync --reset-to-target local/remotes/app",
            ))
            .claim(Claim::OutputLacks("rwv push --repo local/remotes/app")),
        Step::rwv(&a(), &["sync", "--reset-to-target", "local/remotes/app"])
            .may_leave_unresolved(&[Repo::App])
            .expect(Exit::Succeeds)
            .may_rewrite(&[Repo::App])
            .may_rewind(&[Repo::App])
            .claim(Claim::BackedUp {
                repo: Repo::App,
                mark: "pr-app",
                marked: None,
            }),
    ]);
    scenario("a squashed pin off remote offers the reset", steps)
}

/// `w` took in `main`'s `app`, then its `app` was rebuilt by hand on the old
/// base with the same change: the pin is landed and the checkout diverges
/// from it, so `status` names the match and C22, which, run as printed,
/// resets `app` to `main`'s.
pub fn landed_pin_diverged_offers_the_reset() -> Scenario {
    scenario(
        "a landed pin a checkout diverged from offers the reset",
        vec![
            create("w"),
            Step::commit(&a(), Repo::App, "main-app"),
            Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::rwv(&ww("w"), &["sync"]).expect(Exit::Succeeds),
            Step::git(&ww("w"), Repo::App, &["reset", "-q", "--hard", "HEAD~1"])
                .may_rewind(&[Repo::App]),
            Step::git(&ww("w"), Repo::App, &["cherry-pick", "main"]),
            Step::rwv(&ww("w"), &["status"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains("local/remotes/app: pin diverged"))
                .claim(Claim::OutputContains("pin-landed"))
                .claim(Claim::OutputContains("patch-equivalent: "))
                .claim(Claim::OutputContains(
                    "rwv sync --reset-to-target local/remotes/app",
                )),
            Step::rwv(
                &ww("w"),
                &["sync", "--reset-to-target", "local/remotes/app"],
            )
            .expect(Exit::Succeeds)
            .may_rewrite(&[Repo::App])
            .may_rewind(&[Repo::App])
            .claim(Claim::AtPin {
                at: ww("w"),
                repo: Repo::App,
            }),
        ],
    )
}

/// A landing that would run into nothing moves nothing: the target checked out
/// in no workspace refuses until the landing as refs exists, naming the
/// workweave that opens it.
pub fn land_into_a_target_checked_out_nowhere() -> Scenario {
    scenario(
        "land into a target checked out nowhere",
        vec![
            create("w"),
            Step::commit(&ww("w"), Repo::App, "w-app"),
            Step::git(&a(), Repo::Proj, &["switch", "--detach"]),
            Step::rwv(&ww("w"), &["land"])
                .expect(Exit::refuses(Repo::Proj, State::TargetNotCheckedOut))
                .claim(Claim::OutputContains(
                    "rwv workweave create <ww> --branch main",
                ))
                .claim(Claim::LocalStateUnchanged(ww("w"))),
        ],
    )
}

/// Every refusal a landing owes is reported in one run before anything moves:
/// an edit in a repo it composes, and a change to the target's entry, which
/// no landing makes until it is named.
pub fn land_reports_every_refusal_together() -> Scenario {
    scenario(
        "land reports every refusal together",
        vec![
            create("w"),
            Step::commit(&ww("w"), Repo::App, "w-app"),
            Step::rwv(
                &ww("w"),
                &["member", "set-role", "local/remotes/lib", "reference"],
            )
            .expect(Exit::Succeeds),
            Step::hand_edit(&ww("w"), Repo::App, "w-app.txt", "w-app", "uncommitted"),
            Step::rwv(&ww("w"), &["land"])
                .expect(Exit::refuses(Repo::Lib, State::TargetMemberChanges))
                .claim(Claim::Judged(Repo::App, State::EditsDirty))
                .claim(Claim::OutputContains("role: owned → reference"))
                .claim(Claim::LocalStateUnchanged(ww("w")))
                .claim(Claim::LocalStateUnchanged(a())),
        ],
    )
}

/// A weave branch that lands into the feature refuses its landing until
/// dependents are carried along.
pub fn land_refuses_a_dependent() -> Scenario {
    scenario(
        "land refuses a dependent",
        vec![
            create("w1"),
            Step::rwv(
                &ww("w1"),
                &["workweave", "create", "--project", "proj", "w2"],
            )
            .expect(Exit::Succeeds),
            Step::commit(&ww("w1"), Repo::App, "w1-app"),
            Step::rwv(&ww("w1"), &["land"])
                .expect(Exit::refuses(Repo::Proj, State::TargetDependents))
                .claim(Claim::OutputContains("proj--w2 lands into proj--w1"))
                .claim(Claim::LocalStateUnchanged(a())),
        ],
    )
}

/// `target-mode-changed`: the record says the target was `declared`, and the
/// target is now a feature. The landing refuses, never redirects, and offers
/// the rebind and the restore; it lands after either.
pub fn target_mode_changed() -> Scenario {
    let refused = |step: Step, feature: &'static str| {
        step.expect(Exit::refuses(Repo::Proj, State::TargetModeChanged))
            .claim(Claim::OutputContains(feature))
            .claim(Claim::OutputContains("rwv branch set-mode stable declared"))
    };
    let to_feature = || {
        Step::rwv(
            &a(),
            &[
                "branch", "set-mode", "stable", "feature", "--target", "main",
            ],
        )
        .expect(Exit::Succeeds)
    };
    scenario(
        "target mode changed",
        vec![
            Step::rwv(&a(), &["branch", "create", "stable", "--declared"]).expect(Exit::Succeeds),
            create("w1"),
            create("w2"),
            Step::commit(&ww("w1"), Repo::App, "w1-app"),
            Step::commit(&ww("w2"), Repo::Lib, "w2-lib"),
            to_feature(),
            refused(
                Step::rwv(&ww("w1"), &["land"]),
                "rwv branch set-mode proj--w1 feature --target stable",
            ),
            Step::rwv(&a(), &["branch", "set-mode", "stable", "declared"]).expect(Exit::Succeeds),
            Step::rwv(&ww("w1"), &["land", "--retire"])
                .expect(Exit::Succeeds)
                .claim(Claim::Contains {
                    at: a(),
                    repo: Repo::App,
                    rev: "main",
                    mark: "w1-app",
                }),
            to_feature(),
            refused(
                Step::rwv(&ww("w2"), &["land"]),
                "rwv branch set-mode proj--w2 feature --target stable",
            ),
            Step::rwv(
                &ww("w2"),
                &[
                    "branch", "set-mode", "proj--w2", "feature", "--target", "stable",
                ],
            )
            .expect(Exit::Succeeds),
            Step::rwv(&ww("w2"), &["land", "--retire"])
                .expect(Exit::Succeeds)
                .claim(Claim::Contains {
                    at: a(),
                    repo: Repo::Lib,
                    rev: "stable",
                    mark: "w2-lib",
                }),
        ],
    )
}

/// `land` refuses a record that changed on another machine since this
/// workspace last saw it, before anything moves, with every other refusal of
/// the same run.
pub fn land_refuses_a_record_retargeted_elsewhere() -> Scenario {
    let mut steps = feat_x_published_beside_stable();
    steps.extend(feat_x_retargeted_on_b());
    let refused = |step: Step| {
        step.expect(Exit::refuses(Repo::Proj, State::TargetRecordChanged))
            .claim(Claim::Judged(Repo::Proj, State::TargetNotCheckedOut))
            .claim(Claim::OutputContains("rwv update"))
            .claim(Claim::OutputContains(
                "rwv branch set-mode feat-x feature --target main",
            ))
    };
    steps.extend([
        refused(Step::rwv(&a(), &["land"])),
        refused(Step::rwv(&a(), &["land"])).claim(Claim::LocalStateUnchanged(a())),
    ]);
    scenario("land refuses a record retargeted elsewhere", steps)
}

/// A record is published only with its target: a feature onto a branch with
/// no push branch on the push target is withheld, naming the push of the
/// target first and the rebind.
pub fn push_withholds_a_feature_whose_target_is_unpublished() -> Scenario {
    scenario(
        "push withholds a feature whose target is unpublished",
        vec![
            Step::git(&a(), Repo::Proj, &["branch", "stable"]),
            Step::rwv(&a(), &["branch", "set-mode", "stable", "declared"]).expect(Exit::Succeeds),
            Step::rwv(&a(), &["branch", "create", "feat-x", "--target", "stable"])
                .expect(Exit::Succeeds),
            Step::commit(&a(), Repo::App, "feat-app"),
            Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::rwv(&a(), &["push"])
                .expect(Exit::refuses(Repo::Proj, State::PublishTargetUnpublished))
                .claim(Claim::OutputContains("rwv push --branch stable"))
                .claim(Claim::RemoteLacks {
                    repo: Repo::Proj,
                    branch: "feat-x",
                }),
        ],
    )
}

/// A record another machine published names its target as that remote's
/// branch: a local branch of that name that does not take in from there is
/// another branch, and the landing refuses it as the target gone rather than
/// landing into it.
pub fn land_never_resolves_a_published_record_to_an_unrelated_branch() -> Scenario {
    let mut steps = vec![Step::new_machine(Machine::B)];
    steps.extend(feature_branch_in_primary());
    steps.extend([
        publishes_records(Step::rwv(&a(), &["push"]))
            .pushes(Remote::origin(Repo::App), "feat-x")
            .pushes(Remote::origin(Repo::Proj), "feat-x")
            .expect(Exit::Succeeds),
        Step::rwv(&b(), &["fetch"]).expect(Exit::Succeeds),
        Step::git(&b(), Repo::Proj, &["branch", "--unset-upstream", "main"]),
        Step::rwv(
            &b(),
            &[
                "workweave",
                "create",
                "--project",
                "proj",
                "r",
                "--branch",
                "feat-x",
            ],
        )
        .expect(Exit::Succeeds),
        Step::rwv(&b_ww("r"), &["land"])
            .expect(Exit::refuses(Repo::Proj, State::TargetGone))
            .claim(Claim::OutputContains("does not take in"))
            .claim(Claim::LocalStateUnchanged(b())),
    ]);
    scenario(
        "land never resolves a published record to an unrelated branch",
        steps,
    )
}

/// The lineage rule at landing: work built on a pin the branch it lands on
/// no longer holds is refused before anything moves.
pub fn land_refuses_work_built_on_another_lineage() -> Scenario {
    scenario(
        "land refuses work built on another lineage",
        vec![
            Step::commit(&a(), Repo::App, "cut-app"),
            Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
            create("w"),
            Step::commit(&ww("w"), Repo::App, "w-app"),
            Step::git(&a(), Repo::App, &["reset", "-q", "--hard", "HEAD~1"])
                .may_rewind(&[Repo::App])
                .may_rewrite(&[Repo::App]),
            Step::rwv(&ww("w"), &["land"])
                .expect(Exit::refuses(Repo::App, State::PinOffLineage))
                .claim(Claim::LocalStateUnchanged(a())),
        ],
    )
}

/// A landing lands a feature only: a declared-mode project branch is never
/// rebased onto another, `--target` or not.
pub fn land_never_rewrites_a_declared_mode_branch() -> Scenario {
    scenario(
        "land never rewrites a declared-mode branch",
        vec![
            Step::rwv(&a(), &["branch", "create", "stable", "--declared"]).expect(Exit::Succeeds),
            Step::commit(&a(), Repo::App, "stable-app"),
            Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::rwv(&a(), &["land", "--target", "main"])
                .expect(Exit::refuses(Repo::Proj, State::TargetNoTarget))
                .claim(Claim::LocalStateUnchanged(a())),
        ],
    )
}
