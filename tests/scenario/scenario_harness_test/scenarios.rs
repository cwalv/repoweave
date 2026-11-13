use super::invariants::{LockSite, Mode, Record};
use super::runner::{
    Claim, Defect, Exit, Forge, IndexAt, MarkerState, MergeHow, MissingVerb, Scenario, Step, Tamper,
};
use super::sandbox::{
    declared_branch_text, Machine, Place, Remote, Repo, SecondProject, Seed, Store, RECORDS_BRANCH,
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
            Step::rwv(&a(), &["sync"])
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
            Step::rwv(&ww("seat"), &["land", "--history", "linear"])
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
            Step::rwv(
                &ww("seat"),
                &["land", "--history", "linear", "--allow-member-change", LIB],
            )
            .expect(Exit::Succeeds)
            .may_rewind(&[Repo::Lib])
            .claim(Claim::OnBranch {
                at: a(),
                repo: Repo::Lib,
                branch: "dev",
            }),
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
    let landing = Step::rwv(&side, &["land", "--history", "linear", "--retire"]);
    let landing = if branch == "main" {
        landing
            .expect(Exit::refuses(Repo::Lib, State::PinUnpinned))
            .claim(Claim::LocalStateUnchanged(a()))
    } else {
        landing
            .expect(Exit::Succeeds)
            .may_strand(&[Repo::Lib])
            .claim(Claim::OutputContains(
                "rwv push --branch main --project proj2",
            ))
            .claim(Claim::Presents {
                at: a(),
                project: Repo::Proj,
            })
    };
    Scenario {
        name,
        seed: Seed::default().second_project(SecondProject { branch, also: &[] }),
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
        ]
        .into_iter()
        .chain(
            (branch != "main").then(|| Step::hand_edit(&a(), Repo::Lib, "README", "lib", "edited")),
        )
        .chain([landing])
        .chain((branch != "main").then(|| {
            Step::git(&a(), Repo::Lib, &["log", "--format=%s", branch])
                .claim(Claim::OutputLine("side-lib"))
        }))
        .collect(),
    }
}

/// `inactive-project-choices`: `proj2` inactive, `lib` shared with `proj`,
/// which the primary presents. A landing from `proj2`'s workweave composes
/// in the source and moves `proj2`'s `dev` as a ref, leaving the primary's
/// `lib` to `proj`; each choice it prints that must act on `proj2` carries
/// `--project proj2` or names `activate proj2 --update`, and each, run as
/// printed in the primary, passes.
/// `proj2`'s seat drops `lib`, which `proj` lists too, and lands while the
/// primary presents `proj`: the primary's `lib` is `proj`'s checkout, so the
/// landing leaves it.
pub fn an_inactive_projects_landing_leaves_a_dropped_member_the_presented_project_lists() -> Scenario
{
    let side = Place::Workweave(Machine::A, Repo::Proj2, "side".to_owned());
    Scenario {
        name: "an inactive project's landing leaves a dropped member the presented project lists",
        seed: Seed::default().second_project(SecondProject {
            branch: "dev",
            also: &[],
        }),
        steps: vec![
            Step::clone_project(Machine::A, Repo::Proj2).expect(Exit::Succeeds),
            Step::rwv(&a(), &["workweave", "create", "--project", "proj2", "side"])
                .expect(Exit::Succeeds),
            Step::rwv(&side, &["member", "remove", LIB, "--delete"]).expect(Exit::Succeeds),
            Step::rwv(&side, &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::rwv(
                &side,
                &[
                    "land",
                    "--history",
                    "linear",
                    "--retire",
                    "--allow-member-change",
                    LIB,
                ],
            )
            .expect(Exit::Succeeds)
            .claim(Claim::OutputLacks("pruned in the target's workspace"))
            .claim(Claim::OutputContains("which proj lists")),
            Step::git(&a(), Repo::Lib, &["status", "--porcelain"]),
            Step::rwv(&a(), &["status"]).expect(Exit::Succeeds),
        ],
    }
}

/// A verb run with `--project` for a project the primary does not present
/// prints its choices for that project, and they run as printed.
pub fn unpresented_project_choices_carry_the_project() -> Scenario {
    Scenario {
        name: "unpresented project choices carry the project",
        seed: Seed::default().second_project(SecondProject {
            branch: "dev",
            also: &[],
        }),
        steps: vec![
            Step::clone_project(Machine::A, Repo::Proj2).expect(Exit::Succeeds),
            Step::rwv(&a(), &["push", "--project", "proj2"])
                .expect(Exit::refuses(Repo::Lib, State::AlignmentOnBranch))
                .claim(Claim::OutputContains("rwv update --project proj2")),
            Step::rwv_printed(&a(), "rwv update --project proj2")
                .expect(Exit::refuses(Repo::Lib, State::AlignmentOnBranch))
                .claim(Claim::OutputContains("rwv activate proj2 --update")),
            Step::rwv_printed(&a(), "rwv activate proj2 --update")
                .expect(Exit::Succeeds)
                .claim(Claim::Presents {
                    at: a(),
                    project: Repo::Proj2,
                }),
        ],
    }
}

/// `proj2`, inactive, lists `docs`, which `proj` no longer does; the
/// primary's `docs` holds a commit `proj2`'s lock does not record. The
/// landing from `proj2`'s workweave refuses it unpinned in the primary and
/// spells the lock for `proj2`. Each printed choice run in turn moves the
/// state on: the lock and the update refuse the shared `lib` and name the
/// activation, after which the choices print bare and the landing proceeds.
pub fn inactive_landing_refusal_spelled_for_its_project() -> Scenario {
    let side = Place::Workweave(Machine::A, Repo::Proj2, "side".to_owned());
    Scenario {
        name: "inactive landing refusal spelled for its project",
        seed: Seed::default().second_project(SecondProject {
            branch: "dev",
            also: &[Repo::Docs],
        }),
        steps: vec![
            Step::clone_project(Machine::A, Repo::Proj2).expect(Exit::Succeeds),
            Step::rwv(&a(), &["member", "remove", DOCS]).expect(Exit::Succeeds),
            Step::rwv(&a(), &["workweave", "create", "--project", "proj2", "side"])
                .expect(Exit::Succeeds),
            Step::commit(&a(), Repo::Docs, "docs-unrecorded"),
            Step::commit(&side, Repo::Lib, "side-lib"),
            Step::rwv(&side, &["land", "--history", "linear"])
                .expect(Exit::refuses(Repo::Docs, State::PinUnpinned))
                .claim(Claim::OutputContains("rwv lock --commit --project proj2")),
            Step::rwv_printed(&a(), "rwv lock --commit")
                .expect(Exit::refuses(Repo::Lib, State::AlignmentOnBranch))
                .claim(Claim::OutputContains("rwv update --project proj2")),
            Step::rwv_printed(&a(), "rwv update --project proj2")
                .expect(Exit::refuses(Repo::Lib, State::AlignmentOnBranch))
                .claim(Claim::OutputContains("rwv activate proj2 --update")),
            Step::rwv_printed(&a(), "rwv activate proj2 --update")
                .expect(Exit::Succeeds)
                .claim(Claim::Presents {
                    at: a(),
                    project: Repo::Proj2,
                }),
            Step::rwv(&side, &["land", "--history", "linear"])
                .expect(Exit::refuses(Repo::Docs, State::PinUnpinned))
                .claim(Claim::OutputLacks("--project")),
            Step::rwv_printed(&a(), "rwv lock --commit").expect(Exit::Succeeds),
            Step::rwv(&side, &["land", "--history", "linear"])
                .expect(Exit::Succeeds)
                .claim(Claim::Contains {
                    at: a(),
                    repo: Repo::Lib,
                    rev: "dev",
                    mark: "side-lib",
                }),
        ],
    }
}

/// `proj2`, inactive, lists `docs`, which `proj` lists too, on `main`; the
/// primary's `docs` holds a commit under `proj` that no lock records. A
/// landing from `proj2`'s workweave that fast-forwards that checkout would pin
/// the commit: it refuses `pin` `unpinned` before anything moves.
pub fn a_landing_judges_the_pin_of_a_shared_member_it_fast_forwards() -> Scenario {
    let side = Place::Workweave(Machine::A, Repo::Proj2, "side".to_owned());
    Scenario {
        name: "a landing judges the pin of a shared member it fast-forwards",
        seed: Seed::default().second_project(SecondProject {
            branch: "dev",
            also: &[Repo::Docs],
        }),
        steps: vec![
            Step::clone_project(Machine::A, Repo::Proj2).expect(Exit::Succeeds),
            Step::rwv(&a(), &["workweave", "create", "--project", "proj2", "side"])
                .expect(Exit::Succeeds),
            Step::commit(&a(), Repo::Docs, "docs-unrecorded"),
            Step::commit(&side, Repo::Docs, "side-docs"),
            Step::rwv(&side, &["land", "--history", "linear"])
                .expect(Exit::refuses(Repo::Docs, State::PinUnpinned))
                .claim(Claim::LocalStateUnchanged(a()))
                .claim(Claim::LocalStateUnchanged(side.clone())),
        ],
    }
}

pub fn inactive_project_choices() -> Scenario {
    let side = Place::Workweave(Machine::A, Repo::Proj2, "side".to_owned());
    Scenario {
        name: "inactive project choices",
        seed: Seed::default().second_project(SecondProject {
            branch: "dev",
            also: &[],
        }),
        steps: vec![
            Step::clone_project(Machine::A, Repo::Proj2).expect(Exit::Succeeds),
            Step::rwv(&a(), &["workweave", "create", "--project", "proj2", "side"])
                .expect(Exit::Succeeds),
            Step::commit(&side, Repo::Lib, "side-lib"),
            Step::rwv(&side, &["land", "--history", "linear", "--retire"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains(
                    "rwv push --branch main --project proj2",
                ))
                .claim(Claim::OutputContains("rwv activate proj2 --update"))
                .claim(Claim::Contains {
                    at: a(),
                    repo: Repo::Lib,
                    rev: "dev",
                    mark: "side-lib",
                })
                .claim(Claim::Presents {
                    at: a(),
                    project: Repo::Proj,
                }),
            Step::rwv_printed(&a(), "rwv activate proj2 --update")
                .expect(Exit::Succeeds)
                .claim(Claim::Presents {
                    at: a(),
                    project: Repo::Proj2,
                })
                .claim(Claim::Contains {
                    at: a(),
                    repo: Repo::Lib,
                    rev: "HEAD",
                    mark: "side-lib",
                }),
            Step::rwv(&a(), &["push", "--branch", "main", "--project", "proj2"])
                .expect(Exit::Succeeds)
                .pushes(Remote::origin(Repo::Proj2), "main")
                .pushes(Remote::origin(Repo::Proj2), RECORDS_BRANCH)
                .pushes(Remote::origin(Repo::Lib), "dev"),
            Step::rwv(&a(), &["status"]).expect(Exit::Succeeds),
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
                .claim(Claim::OutputContains("keep this work on a branch"))
                .claim(Claim::OutputLacks("copy these commits onto"))
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
        seed: Seed::default().second_project(SecondProject {
            branch: "dev",
            also: &[],
        }),
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
        seed: Seed::default().second_project(SecondProject {
            branch: "dev",
            also: &[],
        }),
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
        seed: Seed::default().second_project(SecondProject {
            branch: "main",
            also: &[],
        }),
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
        seed: Seed::default().second_project(SecondProject {
            branch: "dev",
            also: &[],
        }),
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
        seed: Seed::default().second_project(SecondProject {
            branch: "main",
            also: &[],
        }),
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

/// `pull --merge-upstream` stopped on its conflict in `app`, its merge
/// having also brought in `a-clean.txt` cleanly; the operator edits that
/// file and does not stage it, so the VCS's own abort of the merge refuses.
/// `rwv abort` refuses with the VCS's message and the choices that clear
/// it; the printed `--abandon-foreign-tip` keeps the edit as a backup and
/// puts `app` back.
pub fn pull_abort_over_a_cancel_the_vcs_refuses() -> Scenario {
    scenario(
        "pull abort over a cancel the VCS refuses",
        vec![
            Step::new_machine(Machine::B),
            Step::rwv(&a(), &["member", "remove", "local/remotes/docs"]).expect(Exit::Succeeds),
            Step::hand_edit(&a(), Repo::App, "README", "app", "alpha"),
            Step::git(&a(), Repo::App, &["commit", "-qam", "alpha"]),
            Step::commit(&a(), Repo::App, "a-clean"),
            Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::git(&a(), Repo::App, &["push", "origin", "main"])
                .pushes(Remote::origin(Repo::App), "main"),
            Step::git(&a(), Repo::Proj, &["push", "origin", "main"])
                .pushes(Remote::origin(Repo::Proj), "main"),
            Step::hand_edit(&b(), Repo::App, "README", "app", "beta"),
            Step::git(&b(), Repo::App, &["commit", "-qam", "beta"]),
            Step::rwv(&b(), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::rwv(&b(), &["pull", "--merge-upstream"])
                .expect(Exit::refuses_kind(RefusalKind::OpParked)),
            Step::hand_edit(
                &b(),
                Repo::App,
                "a-clean.txt",
                "a-clean",
                "edited mid-merge",
            ),
            Step::rwv(&b(), &["abort"])
                .expect(Exit::refuses_kind(RefusalKind::ForeignTip))
                .claim(Claim::OutputContains("could not be canceled"))
                .claim(Claim::OutputLacks("abort stopped there"))
                .claim(Claim::OutputContains("not uptodate"))
                .claim(Claim::OutputContains("rwv abort --abandon-foreign-tip"))
                .claim(Claim::LocalStateUnchanged(b())),
            Step::git(
                &b(),
                Repo::App,
                &["rev-parse", "-q", "--verify", "MERGE_HEAD"],
            ),
            Step::rwv_printed(&b(), "rwv abort --abandon-foreign-tip")
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains(
                    "its uncommitted edits are kept as backup",
                )),
            Step::git(
                &b(),
                Repo::App,
                &["rev-parse", "-q", "--verify", "MERGE_HEAD"],
            )
            .expect(Exit::Code(1)),
            Step::git(
                &b(),
                Repo::App,
                &["log", "--all", "--format=kept %s", "-Sedited mid-merge"],
            )
            .claim(Claim::OutputContains("kept ")),
            Step::rwv(&b(), &["abort"]).expect(Exit::refuses_kind(RefusalKind::NoOpRecorded)),
        ],
    )
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

/// `app` declares `DERIVED` regenerated; machine A pushes a regeneration of
/// it, and of `OTHER` where `other` is set, while B commits its own. B's
/// `pull --merge-upstream` merges `app`'s pins: `DERIVED` takes the
/// upstream's content, as a rebase onto the upstream would, and a path
/// nobody declared that both sides changed still stops with B's commit kept,
/// which the operator resolves and `--continue` finishes.
fn pull_merge_deriving(other: bool) -> Scenario {
    let upstream: &[(&str, &str)] = if other {
        &[("DERIVED", "upstream\n"), ("OTHER", "upstream-other\n")]
    } else {
        &[("DERIVED", "upstream\n")]
    };
    let local: &[(&str, &str)] = if other {
        &[("DERIVED", "local\n"), ("OTHER", "local-other\n")]
    } else {
        &[("DERIVED", "local\n")]
    };
    let mut steps = vec![
        Step::rwv(&a(), &["member", "remove", "local/remotes/docs"]).expect(Exit::Succeeds),
        Step::hand_commit(
            &a(),
            Repo::App,
            "main",
            &[
                (".gitattributes", "DERIVED merge=rwv-ours\n"),
                ("DERIVED", "base\n"),
                ("OTHER", "base\n"),
            ],
        ),
        Step::git(&a(), Repo::App, &["reset", "-q", "--hard"]),
        Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::git(&a(), Repo::App, &["push", "origin", "main"])
            .pushes(Remote::origin(Repo::App), "main"),
        Step::git(&a(), Repo::Proj, &["push", "origin", "main"])
            .pushes(Remote::origin(Repo::Proj), "main"),
        Step::new_machine(Machine::B),
        Step::hand_commit(&a(), Repo::App, "main", upstream),
        Step::git(&a(), Repo::App, &["reset", "-q", "--hard"]),
        Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::git(&a(), Repo::App, &["push", "origin", "main"])
            .pushes(Remote::origin(Repo::App), "main"),
        Step::git(&a(), Repo::Proj, &["push", "origin", "main"])
            .pushes(Remote::origin(Repo::Proj), "main"),
        Step::hand_commit(&b(), Repo::App, "main", local),
        Step::git(&b(), Repo::App, &["reset", "-q", "--hard"]),
        Step::rwv(&b(), &["lock", "--commit"]).expect(Exit::Succeeds),
    ];
    let pull = Step::rwv(&b(), &["pull", "--merge-upstream"]);
    if other {
        steps.extend([
            pull.expect(Exit::refuses_kind(RefusalKind::OpParked))
                .claim(Claim::OutputContains(
                    "merge conflict in local/remotes/app (OTHER)",
                ))
                .claim(Claim::OutputContains("rwv pull --continue")),
            Step::git(&b(), Repo::App, &["show", "HEAD:OTHER"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains("local-other")),
            Step::git(&b(), Repo::App, &["show", "MERGE_HEAD:OTHER"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains("upstream-other")),
            Step::git(&b(), Repo::App, &["checkout", "--ours", "OTHER"]),
            Step::git(&b(), Repo::App, &["add", "OTHER"]),
            Step::rwv(&b(), &["pull", "--continue"]).expect(Exit::Succeeds),
            Step::git(&b(), Repo::App, &["show", "HEAD:OTHER"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains("local-other")),
        ]);
    } else {
        steps.push(pull.expect(Exit::Succeeds));
    }
    steps.extend([
        Step::git(&b(), Repo::App, &["rev-parse", "--verify", "-q", "HEAD^2"])
            .expect(Exit::Succeeds),
        Step::git(&b(), Repo::App, &["show", "HEAD:DERIVED"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputContains("upstream")),
    ]);
    scenario(
        if other {
            "pull --merge-upstream: an undeclared path both sides changed still stops"
        } else {
            "pull --merge-upstream: a derived path both sides regenerated takes the upstream's"
        },
        steps,
    )
}

pub fn pull_merge_resolves_derived_to_the_upstream() -> Scenario {
    pull_merge_deriving(false)
}

pub fn pull_merge_still_stops_on_an_undeclared_path() -> Scenario {
    pull_merge_deriving(true)
}

/// The project repo itself declares `DERIVED` regenerated, beside its lock's
/// own driver; both machines regenerate it, and B's pull takes the
/// upstream's.
pub fn pull_project_merge_resolves_derived_to_the_upstream() -> Scenario {
    scenario(
        "pull --merge-upstream: a derived path of the project repo takes the upstream's",
        vec![
            Step::hand_commit(
                &a(),
                Repo::Proj,
                "main",
                &[
                    (
                        ".gitattributes",
                        "rwv.lock merge=rwv-lock\nDERIVED merge=rwv-ours\n",
                    ),
                    ("DERIVED", "base\n"),
                ],
            ),
            Step::git(&a(), Repo::Proj, &["reset", "-q", "--hard"]),
            Step::git(&a(), Repo::Proj, &["push", "origin", "main"])
                .pushes(Remote::origin(Repo::Proj), "main"),
            Step::new_machine(Machine::B),
            Step::hand_commit(&a(), Repo::Proj, "main", &[("DERIVED", "upstream\n")]),
            Step::git(&a(), Repo::Proj, &["reset", "-q", "--hard"]),
            Step::git(&a(), Repo::Proj, &["push", "origin", "main"])
                .pushes(Remote::origin(Repo::Proj), "main"),
            Step::hand_commit(&b(), Repo::Proj, "main", &[("DERIVED", "local\n")]),
            Step::git(&b(), Repo::Proj, &["reset", "-q", "--hard"]),
            Step::rwv(&b(), &["pull", "--merge-upstream"]).expect(Exit::Succeeds),
            Step::git(&b(), Repo::Proj, &["rev-parse", "--verify", "-q", "HEAD^2"])
                .expect(Exit::Succeeds),
            Step::git(&b(), Repo::Proj, &["show", "HEAD:DERIVED"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains("upstream")),
        ],
    )
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
        seed: Seed::default().second_project(SecondProject {
            branch: "dev",
            also: &[],
        }),
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
                .expect(Exit::refuses(Repo::App, State::HistoryUnset))
                .claim(Claim::OutputContains("rwv branch set-history <h> --repo"))
                .claim(Claim::OutputContains("rerun with --history <h>"))
                .claim(Claim::LocalStateUnchanged(ww("w"))),
            Step::rwv(&ww("w"), &["sync", "--history", "linear"])
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
/// reports it, and each project's landing follows its own setting: `proj`'s
/// merges `lib`, `proj2`'s rebases it.
pub fn history_disagreement() -> Scenario {
    let lib = "local/remotes/lib";
    let w2 = Place::Workweave(Machine::A, Repo::Proj2, "w2".to_owned());
    Scenario {
        name: "history: two projects record different settings for a shared member",
        seed: Seed::default().second_project(SecondProject {
            branch: "main",
            also: &[],
        }),
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
            Step::rwv(&a(), &["branch", "set-history", "linear"]).expect(Exit::Succeeds),
            Step::rwv(
                &a(),
                &["branch", "set-history", "--project", "proj2", "linear"],
            )
            .expect(Exit::Succeeds),
            Step::commit(&a(), Repo::Lib, "main-lib"),
            Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
            create("w"),
            Step::commit(&ww("w"), Repo::Lib, "w-lib"),
            Step::rwv(&ww("w"), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::commit(&a(), Repo::Lib, "main-lib-2"),
            Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::rwv(&ww("w"), &["land"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputLacks("local/remotes/lib: rebased"))
                .claim(Claim::Contains {
                    at: a(),
                    repo: Repo::Lib,
                    rev: "main^1",
                    mark: "main-lib-2",
                })
                .claim(Claim::Contains {
                    at: a(),
                    repo: Repo::Lib,
                    rev: "main^2",
                    mark: "w-lib",
                }),
            Step::rwv(&a(), &["workweave", "create", "--project", "proj2", "w2"])
                .expect(Exit::Succeeds),
            Step::commit(&w2, Repo::Lib, "w2-lib"),
            Step::rwv(&w2, &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::rwv(&w2, &["land"])
                .expect(Exit::Succeeds)
                .may_strand(&[Repo::Lib, Repo::Proj2])
                .claim(Claim::OutputContains(
                    "local/remotes/lib: rebased; the tip it left",
                )),
        ],
    }
}

/// Top-level `linear`, `lib` overridden to `merge`: what is stored, and a
/// landing that only fast-forwards reads neither. `history_member_override_lands`
/// is the landing that needs both.
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
            Step::rwv(&ww("over"), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::rwv(&ww("over"), &["land"]).expect(Exit::Succeeds),
        ],
    )
}

/// Hand edits of `[integrations.cargo-workspace]`, `[workweave]` and the
/// top-level `history`: a create runs over the uncommitted file, which stays
/// in the parent, reported; `lock --commit` then commits all three in one
/// commit, running set-history's step, and a create runs again.
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
            create("early")
                .claim(Claim::OutputContains(
                    "holds uncommitted changes that stay there",
                ))
                .claim(Claim::OutputContains("projects/proj/rwv.toml")),
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
            Step::rwv(&ww("w"), &["sync"])
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
                    "--history",
                    "linear",
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

/// A range holding a merge is never rebased: `sync` under a linear setting
/// integrates it by a merge commit instead, first parent the feature's, and
/// reports it `merged`.
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
            Step::rwv(&ww("w"), &["sync", "--history", "linear"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains(
                    "local/remotes/app: merged: the commits a rebase would replay hold the merge",
                ))
                .claim(Claim::OutputLacks("local/remotes/app: rebased onto"))
                .claim(Claim::Contains {
                    at: ww("w"),
                    repo: Repo::App,
                    rev: "proj--w",
                    mark: "side-app",
                })
                .claim(Claim::Contains {
                    at: ww("w"),
                    repo: Repo::App,
                    rev: "proj--w",
                    mark: "main-app",
                })
                .claim(Claim::Contains {
                    at: ww("w"),
                    repo: Repo::App,
                    rev: "proj--w^1",
                    mark: "w-app",
                }),
        ],
    )
}

/// `sync --json` reports the range it integrated by a merge and the branch it
/// rebased, the way a landing does.
pub fn sync_json_reports_what_it_merged_and_rebased() -> Scenario {
    scenario(
        "sync --json reports a merged range and a rebased branch",
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
            Step::commit(&ww("w"), Repo::Lib, "w-lib"),
            Step::rwv(&ww("w"), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::commit(&a(), Repo::App, "main-app"),
            Step::commit(&a(), Repo::Lib, "main-lib"),
            Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::rwv(&ww("w"), &["sync", "--history", "linear", "--json"])
                .expect(Exit::Succeeds)
                .may_rewrite(&[Repo::Lib])
                .claim(Claim::OutputContains("\"kind\": \"merged\""))
                .claim(Claim::OutputContains("\"path\": \"local/remotes/app\""))
                .claim(Claim::OutputContains("\"kind\": \"rebased\""))
                .claim(Claim::OutputContains("\"path\": \"local/remotes/lib\"")),
        ],
    )
}

/// A rebase that would rewrite a branch another feature lands into, cut from
/// an older commit of it, refuses, naming it; a fast-forward does not.
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
            Step::rwv(&ww("w"), &["sync", "--history", "linear"])
                .expect(Exit::refuses(Repo::Proj, State::TargetDependentLacksTip))
                .claim(Claim::OutputContains("target dependent lacks the tip"))
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
        Step::rwv(&ww("w"), &["sync", "--history", "linear"])
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

/// The lineage rule on `update --remote --merge-upstream`: `w`'s `app` is
/// rebased onto `main`'s newer pin, so its upstream `feat-w` lacks that base;
/// the merge refuses before anything moves and names C3, since the
/// upstream's own base is on `main`. C3, run as printed, moves `feat-w` to
/// its upstream by name, keeping a backup, and re-pins `app` there.
pub fn update_remote_merge_upstream_off_lineage() -> Scenario {
    let mut steps = feature_rebased_past_its_push_branch();
    steps.extend([
        Step::rwv(&ww("w"), &["update", "--remote", "--merge-upstream"])
            .expect(Exit::refuses(Repo::App, State::PinOffLineage))
            .claim(Claim::OutputContains(
                "rwv update --remote --reset-to-upstream local/remotes/app",
            ))
            .claim(Claim::LocalStateUnchanged(ww("w"))),
        Step::rwv(
            &ww("w"),
            &[
                "update",
                "--remote",
                "--reset-to-upstream",
                "local/remotes/app",
            ],
        )
        .expect(Exit::Succeeds)
        .may_rewrite(&[Repo::App])
        .may_rewind(&[Repo::App])
        .claim(Claim::OutputContains("to its upstream"))
        .claim(Claim::OutputContains("in backup"))
        .claim(Claim::OutputContains("local/remotes/app: re-pinned "))
        .claim(Claim::OnBranch {
            at: ww("w"),
            repo: Repo::App,
            branch: "feat-w",
        })
        .claim(Claim::PinContains {
            site: LockSite::Checkout(ww("w"), Repo::Proj, "feat-w".to_owned()),
            member: Repo::App,
            mark: "w-app",
        }),
        Step::git(
            &ww("w"),
            Repo::App,
            &["ls-files", "--error-unmatch", "main-app.txt"],
        )
        .expect(Exit::Code(1)),
    ]);
    scenario("update --remote --merge-upstream off lineage", steps)
}

/// A feature's member branch that takes in from a branch of another name is
/// refused by `update --remote` before anything moves: it advances only from
/// its own upstream.
pub fn update_remote_refuses_a_foreign_feature_upstream() -> Scenario {
    scenario(
        "update --remote refuses a feature branch taking in from another branch",
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
            Step::git(
                &ww("w"),
                Repo::App,
                &["branch", "--set-upstream-to=origin/main", "feat-w"],
            ),
            Step::forge(Forge::Commit {
                repo: Repo::App,
                branch: "main",
                mark: "main-remote",
            }),
            Step::rwv(&ww("w"), &["update", "--remote"])
                .expect(Exit::refuses(Repo::App, State::PushBranchForeignUpstream))
                .claim(Claim::OutputContains("feat-w takes in from origin/main"))
                .claim(Claim::LocalStateUnchanged(ww("w"))),
        ],
    )
}

/// `update --remote` in a feature advances each `owned` member's feature
/// branch from that branch's own upstream: `app`'s `feat-w` fast-forwards to
/// the commit pushed onto it and takes in nothing of `main`'s newer upstream
/// tip. Once both sides of `feat-w` have moved, `--merge-upstream` merges the
/// upstream in, first parent the upstream, and the lock pins the merge; the
/// merge, ahead of the upstream, is no backward re-pin for the next run.
pub fn update_remote_advances_the_feature_branch() -> Scenario {
    scenario(
        "update --remote in a feature advances the feature branch from its upstream",
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
            Step::forge(Forge::Commit {
                repo: Repo::App,
                branch: "feat-w",
                mark: "colleague-app",
            }),
            Step::forge(Forge::Commit {
                repo: Repo::App,
                branch: "main",
                mark: "main-remote",
            }),
            Step::rwv(&ww("w"), &["update", "--remote"])
                .expect(Exit::Succeeds)
                .claim(Claim::OnBranch {
                    at: ww("w"),
                    repo: Repo::App,
                    branch: "feat-w",
                })
                .claim(Claim::Contains {
                    at: ww("w"),
                    repo: Repo::App,
                    rev: "feat-w",
                    mark: "colleague-app",
                })
                .claim(Claim::PinContains {
                    site: LockSite::Checkout(ww("w"), Repo::Proj, "feat-w".to_owned()),
                    member: Repo::App,
                    mark: "colleague-app",
                }),
            Step::git(
                &ww("w"),
                Repo::App,
                &["ls-files", "--error-unmatch", "main-remote.txt"],
            )
            .expect(Exit::Code(1)),
            Step::commit(&ww("w"), Repo::App, "w-app-2"),
            Step::rwv(&ww("w"), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::forge(Forge::Commit {
                repo: Repo::App,
                branch: "feat-w",
                mark: "colleague-app-2",
            }),
            Step::rwv(&ww("w"), &["update", "--remote", "--merge-upstream"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains("merged its upstream"))
                .claim(Claim::OnBranch {
                    at: ww("w"),
                    repo: Repo::App,
                    branch: "feat-w",
                })
                .claim(Claim::PinContains {
                    site: LockSite::Checkout(ww("w"), Repo::Proj, "feat-w".to_owned()),
                    member: Repo::App,
                    mark: "w-app-2",
                })
                .claim(Claim::PinContains {
                    site: LockSite::Checkout(ww("w"), Repo::Proj, "feat-w".to_owned()),
                    member: Repo::App,
                    mark: "colleague-app-2",
                }),
            Step::git(
                &ww("w"),
                Repo::App,
                &["rev-parse", "--verify", "-q", "feat-w^2"],
            )
            .expect(Exit::Succeeds),
            Step::git(
                &ww("w"),
                Repo::App,
                &["ls-files", "--error-unmatch", "main-remote.txt"],
            )
            .expect(Exit::Code(1)),
            Step::rwv(&ww("w"), &["update", "--remote"])
                .expect(Exit::Succeeds)
                .claim(Claim::PinContains {
                    site: LockSite::Checkout(ww("w"), Repo::Proj, "feat-w".to_owned()),
                    member: Repo::App,
                    mark: "w-app-2",
                }),
        ],
    )
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

/// A target checked out in no workspace lands as refs, so what refuses such a
/// landing is the source's own: its edits, what retiring it refuses under
/// `--retire`, and beside a target with no record or a project repo on no
/// branch too, each before anything moves.
pub fn land_into_a_target_checked_out_nowhere() -> Scenario {
    scenario(
        "land into a target checked out nowhere",
        vec![
            create("w"),
            Step::commit(&ww("w"), Repo::App, "w-app"),
            Step::git(&a(), Repo::Proj, &["switch", "--detach"]),
            Step::hand_edit(&ww("w"), Repo::Lib, "README", "lib", "lib edited"),
            Step::rwv(&ww("w"), &["land"])
                .expect(Exit::refuses(Repo::Lib, State::EditsDirty))
                .claim(Claim::LocalStateUnchanged(ww("w")))
                .claim(Claim::LocalStateUnchanged(a())),
            Step::git(
                &ww("w"),
                Repo::App,
                &["checkout-index", "--prefix=copy-", "README"],
            ),
            Step::rwv(&ww("w"), &["land", "--retire"])
                .expect(Exit::refuses(Repo::Lib, State::EditsDirty))
                .claim(Claim::Judged(Repo::App, State::EditsDirty))
                .claim(Claim::LocalStateUnchanged(ww("w"))),
            Step::rwv(&ww("w"), &["land", "--target", "nosuch"])
                .expect(Exit::refuses(Repo::Proj, State::TargetUnrecorded))
                .claim(Claim::Judged(Repo::Lib, State::EditsDirty))
                .claim(Claim::LocalStateUnchanged(ww("w"))),
            Step::git(&ww("w"), Repo::Proj, &["switch", "--detach"]),
            Step::rwv(&ww("w"), &["land"])
                .expect(Exit::refuses(Repo::Proj, State::AlignmentProjectDetached))
                .claim(Claim::Judged(Repo::Lib, State::EditsDirty))
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

/// A landing that only fast-forwards rewrites nothing of the feature, so a
/// weave branch landing into it, even one cut from an older commit and
/// checked out elsewhere, does not refuse it and is not carried.
pub fn land_refuses_a_dependent() -> Scenario {
    scenario(
        "a fast-forward landing leaves a dependent alone",
        vec![
            create("w1"),
            Step::rwv(
                &ww("w1"),
                &["workweave", "create", "--project", "proj", "w2"],
            )
            .expect(Exit::Succeeds),
            Step::commit(&ww("w1"), Repo::App, "w1-app"),
            Step::rwv(&ww("w1"), &["land", "--history", "linear"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputLacks("dependent"))
                .claim(Claim::OutputLacks("carried")),
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
            Step::rwv(&ww("w2"), &["land", "--history", "linear", "--retire"])
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
        Step::hand_edit(
            &b_ww("r"),
            Repo::App,
            "feat-app.txt",
            "feat-app",
            "uncommitted",
        ),
        Step::rwv(&b_ww("r"), &["land"])
            .expect(Exit::refuses(Repo::Proj, State::TargetGone))
            .claim(Claim::Judged(Repo::App, State::EditsDirty))
            .claim(Claim::OutputContains("neither takes in"))
            .claim(Claim::OutputContains("rwv branch set-mode feat-x declared"))
            .claim(Claim::OutputLacks("--target main"))
            .claim(Claim::LocalStateUnchanged(b())),
        Step::rwv(&b_ww("r"), &["branch", "set-mode", "feat-x", "declared"]).expect(Exit::Succeeds),
        Step::rwv(&b_ww("r"), &["land"])
            .expect(Exit::refuses(Repo::Proj, State::TargetNoTarget))
            .claim(Claim::LocalStateUnchanged(b())),
    ]);
    scenario(
        "land never resolves a published record to an unrelated branch",
        steps,
    )
}

/// A local branch named as a published record's target that takes in from
/// another branch on the remote holding the record is another branch, though
/// it publishes there: the landing refuses it as the target gone.
pub fn land_never_resolves_a_published_record_to_a_branch_with_a_foreign_upstream() -> Scenario {
    let mut steps = vec![Step::new_machine(Machine::B)];
    steps.extend(feature_branch_in_primary());
    steps.extend([
        publishes_records(Step::rwv(&a(), &["push"]))
            .pushes(Remote::origin(Repo::App), "feat-x")
            .pushes(Remote::origin(Repo::Proj), "feat-x")
            .expect(Exit::Succeeds),
        Step::rwv(&b(), &["fetch"]).expect(Exit::Succeeds),
        Step::git(
            &b(),
            Repo::Proj,
            &["branch", "--set-upstream-to", "origin/feat-x", "main"],
        ),
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
            .claim(Claim::OutputContains("neither takes in"))
            .claim(Claim::LocalStateUnchanged(b())),
    ]);
    scenario(
        "land never resolves a published record to a branch with a foreign upstream",
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
/// rebased onto another, `--target` or not; the workspace's own refusals come
/// in the same report, and the features landing into the branch, which such
/// a landing never moves, are no refusal of it.
pub fn land_never_rewrites_a_declared_mode_branch() -> Scenario {
    scenario(
        "land never rewrites a declared-mode branch",
        vec![
            Step::rwv(&a(), &["branch", "create", "stable", "--declared"]).expect(Exit::Succeeds),
            Step::commit(&a(), Repo::App, "stable-app"),
            Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
            create("s"),
            Step::hand_edit(&a(), Repo::Lib, "README", "lib", "lib edited"),
            Step::rwv(&a(), &["land", "--target", "main"])
                .expect(Exit::refuses(Repo::Proj, State::TargetNoTarget))
                .claim(Claim::Judged(Repo::Lib, State::EditsDirty))
                .claim(Claim::OutputLacks("target dependents"))
                .claim(Claim::LocalStateUnchanged(a())),
        ],
    )
}

/// `dc01-mid-operation`, for the landing: a member stopped mid-bisect refuses
/// `edits` mid-operation, offering only the VCS's own status, and moves
/// nothing.
pub fn dc01_mid_operation_land() -> Scenario {
    scenario(
        "dc01 a member mid-operation refuses the landing",
        vec![
            create("w"),
            Step::commit(&ww("w"), Repo::Lib, "w-lib"),
            Step::git(&ww("w"), Repo::Lib, &["bisect", "start"]),
            Step::rwv(&ww("w"), &["land"])
                .expect(Exit::refuses(Repo::Lib, State::EditsMidOperation))
                .claim(Claim::OutputLacks("commit your edits"))
                .claim(Claim::LocalStateUnchanged(a())),
        ],
    )
}

/// `deletion-offline-unpublished`: a minted seat never pushed, every remote
/// unreachable: `land --retire` lands, deletes the seat's branches by local
/// refs, and removes the workweave.
pub fn deletion_offline_unpublished() -> Scenario {
    scenario(
        "deletion offline, unpublished",
        vec![
            create("seat"),
            Step::commit(&ww("seat"), Repo::App, "seat-app"),
            Step::unreachable(Remote::origin(Repo::App)),
            Step::unreachable(Remote::origin(Repo::Lib)),
            Step::unreachable(Remote::origin(Repo::Docs)),
            Step::unreachable(Remote::origin(Repo::Proj)),
            Step::rwv(&ww("seat"), &["land", "--retire"])
                .expect(Exit::Succeeds)
                .claim(Claim::WorkweaveGone(ww("seat")))
                .claim(Claim::Contains {
                    at: a(),
                    repo: Repo::App,
                    rev: "main",
                    mark: "seat-app",
                }),
        ],
    )
}

/// How `dc01-mid-operation` stops a checkout.
#[derive(Debug, Clone, Copy)]
pub enum Stopped {
    /// A rebase whose last step failed, leaving the member detached at its
    /// branch's tip.
    Rebase,
    /// A rebase stopped part-way, leaving the member detached behind the
    /// lock's pin.
    RebaseBehindPin,
    /// A merge stopped on a conflict, on the member's branch.
    Merge,
    /// A cherry-pick stopped on a conflict, on the member's branch.
    CherryPick,
    /// A revert not yet committed, on the member's branch.
    Revert,
    /// A bisect, which leaves the member on its branch.
    Bisect,
    /// A bisect in the project repo.
    ProjectBisect,
}

impl Stopped {
    fn repo(self) -> Repo {
        match self {
            Stopped::ProjectBisect => Repo::Proj,
            _ => Repo::Lib,
        }
    }

    /// The steps after the workweave's first commit in `lib` that leave the
    /// checkout stopped.
    fn steps(self) -> Vec<Step> {
        let w = ww("w");
        let conflicting = || {
            vec![
                Step::git(&w, Repo::Lib, &["branch", "side", "HEAD~1"]),
                Step::hand_commit(&w, Repo::Lib, "side", &[("w-lib.txt", "side\n")]),
            ]
        };
        match self {
            Stopped::Rebase => vec![
                Step::git(&w, Repo::Lib, &["rebase", "-x", "false", "HEAD~1"])
                    .expect(Exit::Code(1)),
            ],
            Stopped::RebaseBehindPin => vec![
                Step::commit(&w, Repo::Lib, "w-lib-2"),
                Step::rwv(&w, &["lock", "--commit"]).expect(Exit::Succeeds),
                Step::git(&w, Repo::Lib, &["rebase", "-x", "false", "HEAD~2"])
                    .expect(Exit::Code(1)),
            ],
            Stopped::Merge => {
                let mut steps = conflicting();
                steps.push(
                    Step::git(&w, Repo::Lib, &["merge", "--no-edit", "side"]).expect(Exit::Code(1)),
                );
                steps
            }
            Stopped::CherryPick => {
                let mut steps = conflicting();
                steps
                    .push(Step::git(&w, Repo::Lib, &["cherry-pick", "side"]).expect(Exit::Code(1)));
                steps
            }
            Stopped::Revert => vec![Step::git(&w, Repo::Lib, &["revert", "--no-commit", "HEAD"])],
            Stopped::Bisect => vec![Step::git(&w, Repo::Lib, &["bisect", "start"])],
            Stopped::ProjectBisect => vec![Step::git(&w, Repo::Proj, &["bisect", "start"])],
        }
    }

    /// Whether `update` would move the stopped checkout, so reports it.
    fn update_moves_it(self) -> bool {
        matches!(self, Stopped::Rebase | Stopped::RebaseBehindPin)
    }
}

/// `dc01-mid-operation`, for the verbs that align, move or remove a
/// checkout: a checkout stopped in a VCS operation is judged on that alone,
/// offering only the VCS's own status, wherever its HEAD stands; `update`
/// reports it where it would move the member and leaves a member it would
/// not, `switch`, `land` and `workweave delete` refuse it, a discard flag
/// included, and nothing moves.
pub fn dc01_mid_operation(stopped: Stopped) -> Scenario {
    let repo = stopped.repo();
    let alone = |step: Step| {
        step.claim(Claim::JudgedOnly(repo, State::EditsMidOperation))
            .claim(Claim::OutputContains(
                "finish or abandon the operation in progress first",
            ))
            .claim(Claim::OutputLacks("commit your edits"))
            .claim(Claim::LocalStateUnchanged(ww("w")))
    };
    let update = Step::rwv(&ww("w"), &["update"])
        .expect(Exit::Succeeds)
        .claim(Claim::OutputLacks("alignment"))
        .claim(Claim::LocalStateUnchanged(ww("w")));
    let update = if stopped.update_moves_it() {
        update
            .claim(Claim::JudgedOnly(repo, State::EditsMidOperation))
            .claim(Claim::OutputContains(
                "finish or abandon the operation in progress first",
            ))
    } else {
        update.claim(Claim::OutputLacks("mid-operation"))
    };
    let mut steps = vec![create("w"), Step::commit(&ww("w"), Repo::Lib, "w-lib")];
    steps.extend(stopped.steps());
    steps.extend([
        update,
        alone(Step::rwv(&ww("w"), &["switch", "--detach", "HEAD"]))
            .expect(Exit::refuses(repo, State::EditsMidOperation)),
        alone(Step::rwv(&ww("w"), &["land"]))
            .expect(Exit::refuses(repo, State::EditsMidOperation))
            .claim(Claim::LocalStateUnchanged(a())),
        alone(Step::rwv(&a(), &["workweave", "delete", "w"]))
            .expect(Exit::refuses(repo, State::EditsMidOperation)),
        alone(Step::rwv(
            &a(),
            &["workweave", "delete", "w", "--discard-uncommitted"],
        ))
        .expect(Exit::refuses(repo, State::EditsMidOperation)),
        alone(Step::rwv(
            &a(),
            &["workweave", "delete", "w", "--discard-unlanded"],
        ))
        .expect(Exit::refuses(repo, State::EditsMidOperation)),
    ]);
    scenario(
        "dc01 a checkout mid-operation is judged on that alone",
        steps,
    )
}

/// `dc02-c14-keeps-work`: a member detached with two unpushed commits refuses
/// the landing with C14, which prints no bare return while they are
/// unpushed; following it keeps them on a branch, and the landing then
/// retires the workweave with the commits still on that branch.
pub fn dc02_c14_keeps_work() -> Scenario {
    scenario(
        "dc02 C14 keeps the work",
        vec![
            create("w"),
            Step::commit(&ww("w"), Repo::App, "w-app"),
            Step::git(&ww("w"), Repo::Lib, &["switch", "--detach"]),
            Step::commit(&ww("w"), Repo::Lib, "detached-1"),
            Step::commit(&ww("w"), Repo::Lib, "detached-2"),
            Step::rwv(&ww("w"), &["land", "--retire"])
                .expect(Exit::refuses(Repo::Lib, State::AlignmentDetachedUnpushed))
                .claim(Claim::OutputContains("branch <name> <sha>"))
                .claim(Claim::OutputLacks("switch"))
                .claim(Claim::LocalStateUnchanged(ww("w")))
                .claim(Claim::LocalStateUnchanged(a())),
            Step::git(&ww("w"), Repo::Lib, &["branch", "kept", "HEAD"]),
            Step::rwv(&ww("w"), &["update"])
                .expect(Exit::Succeeds)
                .claim(Claim::OnBranch {
                    at: ww("w"),
                    repo: Repo::Lib,
                    branch: "proj--w",
                }),
            Step::rwv(&ww("w"), &["land", "--retire"])
                .expect(Exit::Succeeds)
                .claim(Claim::WorkweaveGone(ww("w")))
                .claim(Claim::Contains {
                    at: a(),
                    repo: Repo::App,
                    rev: "main",
                    mark: "w-app",
                })
                .claim(Claim::Contains {
                    at: a(),
                    repo: Repo::Lib,
                    rev: "kept",
                    mark: "detached-2",
                }),
        ],
    )
}

/// `land --retire --target <T>` judges the deletion rule against `<T>`,
/// the branch it landed into, not the record's target: the retired feature
/// is landed there, so its branches and record go.
pub fn land_retire_judges_deletion_against_the_named_target() -> Scenario {
    scenario(
        "land --retire --target judges deletion against the named target",
        vec![
            Step::rwv(&a(), &["branch", "create", "stable", "--declared"]).expect(Exit::Succeeds),
            create("w"),
            Step::rwv(&a(), &["switch", "main"]).expect(Exit::Succeeds),
            Step::commit(&ww("w"), Repo::App, "w-app"),
            Step::rwv(&ww("w"), &["land", "--retire", "--target", "main"])
                .expect(Exit::Succeeds)
                .claim(Claim::WorkweaveGone(ww("w")))
                .claim(Claim::OutputLacks("left:"))
                .claim(Claim::Contains {
                    at: a(),
                    repo: Repo::App,
                    rev: "main",
                    mark: "w-app",
                })
                .claim(Claim::RecordIs {
                    machine: Machine::A,
                    branch: "proj--w",
                    record: None,
                }),
        ],
    )
}

/// `stable` cut from `main` at the commit pinning `cut-lib`, holding `lib`
/// on its own `stable-lib` one commit behind; a workweave `b` onto it.
fn lib_held_on_stable() -> Vec<Step> {
    vec![
        Step::commit(&a(), Repo::Lib, "cut-lib"),
        Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::rwv(
            &a(),
            &[
                "branch",
                "create",
                "stable",
                "--declared",
                "--set-new-branch",
                "local/remotes/lib=stable-lib",
                "--pin",
                "local/remotes/lib=HEAD~1",
            ],
        )
        .expect(Exit::Succeeds),
        create("b"),
    ]
}

/// The hold's `lib` fast-forwarded in `b` to `main`'s, which the landing
/// judges off lineage, naming C22.
fn hold_bumped_by_fast_forward() -> Vec<Step> {
    vec![
        Step::git(&ww("b"), Repo::Lib, &["merge", "--ff-only", "main"]),
        Step::rwv(&ww("b"), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::rwv(&ww("b"), &["land"])
            .expect(Exit::refuses(Repo::Lib, State::PinOffLineage))
            .claim(Claim::OutputContains(
                "rwv sync --reset-to-target local/remotes/lib",
            ))
            .claim(Claim::LocalStateUnchanged(a())),
    ]
}

fn reset_lib_to_target() -> Step {
    Step::rwv(
        &ww("b"),
        &["sync", "--reset-to-target", "local/remotes/lib"],
    )
    .expect(Exit::Succeeds)
    .may_rewind(&[Repo::Lib])
    .claim(Claim::BackedUp {
        repo: Repo::Lib,
        mark: "cut-lib",
        marked: None,
    })
    .claim(Claim::OutputContains(
        "merge the backup in as a merge commit, bringing",
    ))
    .claim(Claim::OutputLacks("<pin>"))
}

/// `bump-to-cut-pin`: the hold's `lib` bumped to exactly the cut's pin is
/// moved and judged, never silently dropped: off lineage, then C22, whose
/// report holds no new work and offers C26 alone, and after C26's merge the
/// bump lands.
pub fn bump_to_cut_pin() -> Scenario {
    let mut steps = lib_held_on_stable();
    steps.extend(hold_bumped_by_fast_forward());
    steps.extend([
        reset_lib_to_target()
            .claim(Claim::OutputContains("cut-lib"))
            .claim(Claim::OutputLacks("cherry-pick")),
        Step::git(
            &ww("b"),
            Repo::Lib,
            &["merge", "--no-ff", "--no-edit", ":/cut-lib"],
        ),
        Step::rwv(&ww("b"), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::rwv(&ww("b"), &["land", "--retire"])
            .expect(Exit::Succeeds)
            .claim(Claim::WorkweaveGone(ww("b")))
            .claim(Claim::Contains {
                at: a(),
                repo: Repo::Lib,
                rev: "stable-lib",
                mark: "cut-lib",
            }),
    ]);
    scenario("bump to the cut's pin", steps)
}

/// `c22-prints-both`: a hold bumped by fast-forward past the cut's pin; C22's
/// report prints C15 with the new work alone and C26 with every commit the
/// backup brings.
pub fn c22_prints_both() -> Scenario {
    let mut steps = lib_held_on_stable();
    steps.push(Step::hand_commit(
        &a(),
        Repo::Lib,
        "main",
        &[("past-cut.txt", "past the cut\n")],
    ));
    steps.extend(hold_bumped_by_fast_forward());
    steps.push(
        reset_lib_to_target()
            .claim(Claim::OutputContains(
                "new work the reset leaves in the backup: ",
            ))
            .claim(Claim::OutputContains("hand commit\n  copy these commits"))
            .claim(Claim::OutputContains("cherry-pick"))
            .claim(Claim::OutputContains("cut-lib")),
    );
    scenario("C22 prints both", steps)
}

/// `dc29-no-bring`: work left off lineage is ported by the cherry-pick C22's
/// report prints and recorded by C5; it lands and publishes with no porting
/// verb.
pub fn dc29_no_bring() -> Scenario {
    scenario(
        "dc29 no bring",
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
            Step::rwv(&ww("w"), &["land", "--history", "linear"])
                .expect(Exit::refuses(Repo::App, State::PinOffLineage))
                .claim(Claim::OutputContains(
                    "rwv sync --reset-to-target local/remotes/app",
                )),
            Step::rwv(
                &ww("w"),
                &[
                    "sync",
                    "--history",
                    "linear",
                    "--reset-to-target",
                    "local/remotes/app",
                ],
            )
            .expect(Exit::Succeeds)
            .may_rewrite(&[Repo::App])
            .may_rewind(&[Repo::App])
            .claim(Claim::OutputContains("copy these commits onto the branch"))
            .claim(Claim::OutputContains("w-app")),
            Step::git(&ww("w"), Repo::App, &["cherry-pick", ":/w-app"]),
            Step::rwv(&ww("w"), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::rwv(&ww("w"), &["land", "--history", "linear", "--retire"])
                .expect(Exit::Succeeds)
                .claim(Claim::WorkweaveGone(ww("w"))),
            Step::git(&a(), Repo::App, &["log", "-1", "--format=%s", "other"])
                .claim(Claim::OutputLine("w-app")),
            publishes_records(Step::rwv(&a(), &["push"]))
                .pushes(Remote::origin(Repo::App), "other")
                .pushes(Remote::origin(Repo::App), "main")
                .pushes(Remote::origin(Repo::Proj), "main")
                .expect(Exit::Succeeds),
        ],
    )
}

/// `concurrent-landing-merge-upstream`: two machines each land a workweave
/// into `release-1` locally, A publishes first; B's push refuses its diverged
/// branches, `pull --merge-upstream` merges `release-1` and its members with
/// A's, and B's push then passes with no lock merge left to redo.
pub fn concurrent_landing_merge_upstream() -> Scenario {
    let on_release = |at: Place, name: &str| {
        Step::rwv(&at, &["workweave", "create", "--project", "proj", name]).expect(Exit::Succeeds)
    };
    scenario(
        "concurrent landing, merge upstream",
        vec![
            Step::rwv(&a(), &["branch", "create", "release-1", "--declared"])
                .expect(Exit::Succeeds),
            publishes_records(Step::rwv(&a(), &["push"]))
                .pushes(Remote::origin(Repo::Proj), "release-1")
                .expect(Exit::Succeeds),
            Step::new_machine(Machine::B),
            Step::rwv(&b(), &["switch", "release-1"]).expect(Exit::Succeeds),
            on_release(a(), "wa"),
            on_release(b(), "wb"),
            Step::commit(&ww("wa"), Repo::App, "a-app"),
            Step::commit(&b_ww("wb"), Repo::App, "b-app"),
            Step::rwv(&ww("wa"), &["land", "--retire"]).expect(Exit::Succeeds),
            Step::rwv(&b_ww("wb"), &["land", "--retire"]).expect(Exit::Succeeds),
            publishes_records(Step::rwv(&a(), &["push"]))
                .pushes(Remote::origin(Repo::App), "main")
                .pushes(Remote::origin(Repo::Proj), "release-1")
                .expect(Exit::Succeeds),
            Step::rwv(&b(), &["fetch"]).expect(Exit::Succeeds),
            Step::rwv(&b(), &["push"])
                .expect(Exit::refuses(Repo::App, State::PushBranchDiverged))
                .claim(Claim::Judged(Repo::Proj, State::PushBranchDiverged))
                .claim(Claim::OutputContains("rwv pull --merge-upstream")),
            Step::rwv(&b(), &["pull", "--merge-upstream"])
                .expect(Exit::Succeeds)
                .claim(Claim::Contains {
                    at: b(),
                    repo: Repo::App,
                    rev: "main",
                    mark: "a-app",
                })
                .claim(Claim::Contains {
                    at: b(),
                    repo: Repo::App,
                    rev: "main",
                    mark: "b-app",
                }),
            publishes_records(Step::rwv(&b(), &["push"]))
                .pushes(Remote::origin(Repo::App), "main")
                .pushes(Remote::origin(Repo::Proj), "release-1")
                .expect(Exit::Succeeds)
                .claim(Claim::OutputLacks("pin unmerged"))
                .claim(Claim::OutputLacks("merge --no-ff"))
                .claim(Claim::RemoteContains {
                    remote: Remote::origin(Repo::App),
                    branch: "main",
                    mark: "b-app",
                })
                .claim(Claim::RemoteContains {
                    remote: Remote::origin(Repo::App),
                    branch: "main",
                    mark: "a-app",
                }),
        ],
    )
}

const SEATS: [&str; 8] = ["s1", "s2", "s3", "s4", "s5", "s6", "s7", "s8"];

fn seat_recorded(seat: &str, recorded: bool) -> Claim {
    let branch: &'static str = Box::leak(format!("proj--{seat}").into_boxed_str());
    Claim::RecordIs {
        machine: Machine::A,
        branch,
        record: recorded.then(|| feature_onto("main", Mode::Declared)),
    }
}

/// `records-local-writers`: eight seats created at once on one machine, all
/// but the last held before their records compare-and-swap, each losing it
/// and retrying, every record kept; then retires whose drops race a create,
/// the drop retried against the winner's write and dropping only what is
/// gone. Four seats then land `--retire` at once, three held in their
/// retires' record writes while the fourth lands and retires: each landing
/// waits only on the claim on `main`, which a landing gives up once `main`'s
/// workspace took its last fast-forward, so none refuses on another's lease,
/// and every landing and every drop is kept.
pub fn records_local_writers() -> Scenario {
    let (held, last) = SEATS.split_at(SEATS.len() - 1);
    let mut steps: Vec<Step> = held
        .iter()
        .map(|seat| {
            Step::rwv_paused(
                &a(),
                &["workweave", "create", "--project", "proj", seat],
                "records-write",
                Box::leak(format!("create-{seat}").into_boxed_str()),
            )
        })
        .collect();
    steps.push(create(last[0]));
    for seat in held {
        steps.push(
            Step::release(Box::leak(format!("create-{seat}").into_boxed_str()))
                .expect(Exit::Succeeds),
        );
    }
    let mut every = Step::rwv(&a(), &["branch", "list"]).expect(Exit::Succeeds);
    for seat in SEATS {
        every = every.claim(seat_recorded(seat, true));
    }
    steps.push(every);
    for (seat, mark) in [
        ("s1", "s1-app"),
        ("s2", "s2-lib"),
        ("s3", "s3-docs"),
        ("s4", "s4-app"),
    ] {
        let repo = match &mark[3..] {
            "app" => Repo::App,
            "lib" => Repo::Lib,
            _ => Repo::Docs,
        };
        steps.push(Step::commit(&ww(seat), repo, mark));
    }
    let retiring = ["s1", "s2", "s3"];
    for seat in retiring {
        steps.push(Step::rwv_paused(
            &ww(seat),
            &["land", "--history", "linear", "--retire"],
            "records-write",
            Box::leak(format!("retire-{seat}").into_boxed_str()),
        ));
        if seat == "s1" {
            steps.push(create("s9"));
        }
    }
    steps.push(
        Step::rwv(&ww("s4"), &["land", "--history", "linear", "--retire"])
            .expect(Exit::Succeeds)
            .claim(Claim::WorkweaveGone(ww("s4"))),
    );
    for seat in retiring {
        steps.push(
            Step::release(Box::leak(format!("retire-{seat}").into_boxed_str()))
                .expect(Exit::Succeeds)
                .claim(Claim::WorkweaveGone(ww(seat)))
                .claim(seat_recorded(seat, false)),
        );
    }
    let mut after = Step::rwv(&a(), &["branch", "list"]).expect(Exit::Succeeds);
    for seat in SEATS {
        after = after.claim(seat_recorded(
            seat,
            !["s1", "s2", "s3", "s4"].contains(&seat),
        ));
    }
    steps.push(after.claim(seat_recorded("s9", true)));
    for (repo, landed) in [
        (Repo::App, "main:s1-app.txt"),
        (Repo::Lib, "main:s2-lib.txt"),
        (Repo::Docs, "main:s3-docs.txt"),
        (Repo::App, "main:s4-app.txt"),
    ] {
        steps.push(Step::git(&a(), repo, &["cat-file", "-e", landed]));
    }
    scenario("records local writers", steps)
}

/// Two seats land into `main` at once: the first held before its
/// fast-forward, holding the claim on `main`, the second waiting on that
/// claim rather than refusing on the first's lease. Released, the first lands;
/// the second then reads the tips the first left and lands on them.
pub fn a_landing_waits_on_another_landings_claim() -> Scenario {
    scenario(
        "a landing waits on another landing's claim",
        vec![
            create("w1"),
            create("w2"),
            Step::commit(&ww("w1"), Repo::App, "w1-app"),
            Step::commit(&ww("w2"), Repo::Lib, "w2-lib"),
            Step::rwv_paused(
                &ww("w1"),
                &["land", "--retire"],
                "advance-target",
                "land-w1",
            ),
            Step::rwv_paused(
                &ww("w2"),
                &["land", "--history", "linear", "--retire"],
                "landing-claim-held",
                "land-w2",
            ),
            Step::release("land-w1")
                .expect(Exit::Succeeds)
                .claim(Claim::WorkweaveGone(ww("w1"))),
            Step::release("land-w2")
                .expect(Exit::Succeeds)
                .claim(Claim::WorkweaveGone(ww("w2")))
                .claim(Claim::Contains {
                    at: a(),
                    repo: Repo::App,
                    rev: "main",
                    mark: "w1-app",
                })
                .claim(Claim::Contains {
                    at: a(),
                    repo: Repo::Lib,
                    rev: "main",
                    mark: "w2-lib",
                })
                .claim(Claim::PinContains {
                    site: main_lock(),
                    member: Repo::App,
                    mark: "w1-app",
                })
                .claim(Claim::PinContains {
                    site: main_lock(),
                    member: Repo::Lib,
                    mark: "w2-lib",
                }),
        ],
    )
}

/// `records-local-writers`, a drop racing a create: `branch create` makes
/// its branch before it writes the record, so a drop that judges between
/// the two finds the branch; and a create that re-records a name whose stale
/// entry already holds the same value still commits, so a drop judged
/// before that branch existed loses its compare-and-swap and keeps the
/// entry.
pub fn records_drop_races_create() -> Scenario {
    let recorded = |branch: &'static str| Claim::RecordIs {
        machine: Machine::A,
        branch,
        record: Some(feature_onto("main", Mode::Declared)),
    };
    scenario(
        "records drop races create",
        vec![
            create("s2"),
            Step::rwv_paused(
                &ww("s2"),
                &["branch", "create", "fresh", "--target", "main"],
                "records-write",
                "create-fresh",
            ),
            Step::git(
                &ww("s2"),
                Repo::Proj,
                &["rev-parse", "--verify", "-q", "refs/heads/fresh"],
            ),
            Step::release("create-fresh")
                .expect(Exit::Succeeds)
                .claim(recorded("fresh")),
            Step::rwv(
                &a(),
                &[
                    "workweave",
                    "create",
                    "--project",
                    "proj",
                    "s1",
                    "--new-branch",
                    "feat-1",
                ],
            )
            .expect(Exit::Succeeds),
            Step::commit(&ww("s1"), Repo::App, "s1-app"),
            Step::rwv_paused(
                &ww("s1"),
                &["land", "--retire"],
                "records-write",
                "retire-s1",
            ),
            Step::rwv(
                &ww("s2"),
                &["branch", "create", "feat-1", "--target", "main"],
            )
            .expect(Exit::Succeeds),
            Step::release("retire-s1")
                .expect(Exit::Succeeds)
                .claim(Claim::WorkweaveGone(ww("s1")))
                .claim(recorded("feat-1")),
        ],
    )
}

/// A lock commit that pinned a member commit no ref keeps (locked, then
/// amended and re-pinned by name) lands into a target that did not move, and
/// is published. Machine B lacks that commit. A seat B cut before it fetched
/// lands: the lineage rule leaves the pin out, the later lock having
/// re-pinned it by name, and says so, since the seat had not taken that
/// re-pin in. A seat cut after it lands quietly. Machine A, which holds the
/// commit, then lands as well: both clones judge the history alike.
pub fn landed_history_pin_no_ref_keeps() -> Scenario {
    let app = Repo::App.rel_path();
    scenario(
        "a landed history pin no ref keeps",
        vec![
            Step::new_machine(Machine::B),
            Step::rwv(&b(), &["workweave", "create", "--project", "proj", "early"])
                .expect(Exit::Succeeds),
            Step::commit(&b_ww("early"), Repo::App, "early-app"),
            create("w"),
            Step::commit(&ww("w"), Repo::App, "w-app"),
            Step::rwv(&ww("w"), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::git(
                &ww("w"),
                Repo::App,
                &["commit", "--amend", "-q", "-m", "w-app amended"],
            )
            .may_strand(&[Repo::App])
            .may_leave_unresolved(&[Repo::App]),
            Step::rwv(&ww("w"), &["member", "pin", &app, "HEAD", "--commit"])
                .expect(Exit::Succeeds),
            Step::rwv(&ww("w"), &["land"]).expect(Exit::Succeeds),
            publishes_records(Step::rwv(&a(), &["push"]))
                .pushes(Remote::origin(Repo::App), "main")
                .pushes(Remote::origin(Repo::Proj), "main")
                .expect(Exit::Succeeds),
            Step::rwv(&b(), &["fetch"]).expect(Exit::Succeeds),
            Step::rwv(&b_ww("early"), &["land", "--history", "linear"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains(
                    "local/remotes/app: history pin skipped",
                ))
                .claim(Claim::OutputContains("by name later (member pin)")),
            Step::rwv(&b(), &["workweave", "create", "--project", "proj", "w2"])
                .expect(Exit::Succeeds),
            Step::commit(&b_ww("w2"), Repo::App, "w2-app"),
            Step::rwv(&b_ww("w2"), &["land"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputLacks("history pin skipped")),
            publishes_records(Step::rwv(&b(), &["push"]))
                .pushes(Remote::origin(Repo::App), "main")
                .pushes(Remote::origin(Repo::Proj), "main")
                .expect(Exit::Succeeds),
            Step::rwv(&a(), &["fetch"]).expect(Exit::Succeeds),
            create("w3"),
            Step::commit(&ww("w3"), Repo::App, "w3-app"),
            Step::rwv(&ww("w3"), &["land"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputLacks("history pin skipped")),
        ],
    )
}

/// [`landed_history_pin_no_ref_keeps`] taken in on machine B by `rwv sync
/// --json` from a seat cut before the re-pin: the skip is one of the sync's
/// reports, as it is a landing's.
pub fn a_sync_reports_a_skipped_history_pin_in_json() -> Scenario {
    let app = Repo::App.rel_path();
    scenario(
        "a sync reports a skipped history pin in json",
        vec![
            Step::new_machine(Machine::B),
            Step::rwv(&b(), &["workweave", "create", "--project", "proj", "early"])
                .expect(Exit::Succeeds),
            Step::commit(&b_ww("early"), Repo::App, "early-app"),
            create("w"),
            Step::commit(&ww("w"), Repo::App, "w-app"),
            Step::rwv(&ww("w"), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::git(
                &ww("w"),
                Repo::App,
                &["commit", "--amend", "-q", "-m", "w-app amended"],
            )
            .may_strand(&[Repo::App])
            .may_leave_unresolved(&[Repo::App]),
            Step::rwv(&ww("w"), &["member", "pin", &app, "HEAD", "--commit"])
                .expect(Exit::Succeeds),
            Step::rwv(&ww("w"), &["land"]).expect(Exit::Succeeds),
            publishes_records(Step::rwv(&a(), &["push"]))
                .pushes(Remote::origin(Repo::App), "main")
                .pushes(Remote::origin(Repo::Proj), "main")
                .expect(Exit::Succeeds),
            Step::rwv(&b(), &["fetch"]).expect(Exit::Succeeds),
            Step::rwv(&b_ww("early"), &["sync", "--history", "linear", "--json"])
                .expect(Exit::Succeeds)
                .may_rewrite(&[Repo::App])
                .claim(Claim::OutputContains("\"history-pin-skipped\"")),
        ],
    )
}

/// A seat locks `app`, whose `main` then moves upstream with nothing locking it, so
/// the project lands by fast-forward while `app` is rebased: the landing's
/// relock records the rebase's move of `app` by name, and machine B, which
/// never held the commit the seat locked, lands in `app`.
pub fn a_fast_forward_landing_records_a_member_it_rebased() -> Scenario {
    scenario(
        "a fast-forward landing records a member it rebased",
        vec![
            Step::new_machine(Machine::B),
            create("w"),
            Step::commit(&ww("w"), Repo::App, "w-app"),
            Step::rwv(&ww("w"), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::forge(Forge::Commit {
                repo: Repo::App,
                branch: "main",
                mark: "forge-app",
            }),
            Step::rwv(&a(), &["fetch"]).expect(Exit::Succeeds),
            Step::rwv(&ww("w"), &["land", "--history", "linear"])
                .expect(Exit::Succeeds)
                .may_rewrite(&[Repo::App]),
            Step::git(
                &a(),
                Repo::Proj,
                &[
                    "log",
                    "-1",
                    "--format=%(trailers:key=Rwv-Repin,valueonly)",
                    "main",
                ],
            )
            .claim(Claim::OutputContains(" rebase")),
            publishes_records(Step::rwv(&a(), &["push"]))
                .pushes(Remote::origin(Repo::App), "main")
                .pushes(Remote::origin(Repo::Proj), "main")
                .expect(Exit::Succeeds)
                .claim(Claim::OutputLacks("held by no branch")),
            Step::rwv(&b(), &["fetch"]).expect(Exit::Succeeds),
            Step::rwv(&b(), &["workweave", "create", "--project", "proj", "w2"])
                .expect(Exit::Succeeds),
            Step::commit(&b_ww("w2"), Repo::App, "w2-app"),
            Step::rwv(&b_ww("w2"), &["land", "--history", "linear"])
                .expect(Exit::Succeeds)
                .may_rewrite(&[Repo::App]),
        ],
    )
}

/// Which verb replays the seat's project commits onto a `main` that moved,
/// in [`a_re_pin_follows_its_lock_through_a_replay`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Replays {
    Landing,
    SyncFirst,
}

/// [`landed_history_pin_no_ref_keeps`] with `main` moved before the seat
/// lands by rebase: the replay re-maps the re-pinning lock commit's pins,
/// and its re-pin record with them, so machine B, which lacks the amended-
/// away commit, lands in `app`.
pub fn a_re_pin_follows_its_lock_through_a_replay(by: Replays) -> Scenario {
    let app = Repo::App.rel_path();
    let mut steps = vec![
        Step::new_machine(Machine::B),
        create("w"),
        Step::commit(&ww("w"), Repo::App, "w-app"),
        Step::rwv(&ww("w"), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::git(
            &ww("w"),
            Repo::App,
            &["commit", "--amend", "-q", "-m", "w-app amended"],
        )
        .may_strand(&[Repo::App])
        .may_leave_unresolved(&[Repo::App]),
        Step::rwv(&ww("w"), &["member", "pin", &app, "HEAD", "--commit"]).expect(Exit::Succeeds),
        Step::commit(&a(), Repo::App, "main-app"),
        Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
    ];
    if by == Replays::SyncFirst {
        steps.push(
            Step::rwv(&ww("w"), &["sync", "--history", "linear"])
                .expect(Exit::Succeeds)
                .may_rewrite(&[Repo::App]),
        );
    }
    steps.extend([
        Step::rwv(&ww("w"), &["land", "--history", "linear"])
            .expect(Exit::Succeeds)
            .may_rewrite(&[Repo::App]),
        publishes_records(Step::rwv(&a(), &["push"]))
            .pushes(Remote::origin(Repo::App), "main")
            .pushes(Remote::origin(Repo::Proj), "main")
            .expect(Exit::Succeeds)
            .claim(Claim::OutputLacks("held by no branch")),
        Step::rwv(&b(), &["fetch"]).expect(Exit::Succeeds),
        Step::rwv(&b(), &["workweave", "create", "--project", "proj", "w2"]).expect(Exit::Succeeds),
        Step::commit(&b_ww("w2"), Repo::App, "w2-app"),
        Step::rwv(&b_ww("w2"), &["land", "--history", "linear"])
            .expect(Exit::Succeeds)
            .may_rewrite(&[Repo::App]),
    ]);
    scenario("a re-pin follows its lock through a replay", steps)
}

/// A merge whose unjoined `app` pin a later backward re-pin by name waived
/// is published; machine B, which lacks the commit the merge left unjoined,
/// lands a seat: the waiver covers the pin, so it does not decide the base.
pub fn landed_history_waived_pin_on_a_second_machine() -> Scenario {
    let mut steps = waiver_per_merge().steps;
    steps.extend([
        Step::new_machine(Machine::B),
        Step::rwv(&b(), &["workweave", "create", "--project", "proj", "w2"]).expect(Exit::Succeeds),
        Step::commit(&b_ww("w2"), Repo::App, "w2-app"),
        Step::rwv(&b_ww("w2"), &["land", "--history", "linear"]).expect(Exit::Succeeds),
    ]);
    scenario("a waived history pin on a second machine", steps)
}

/// A project that lands by merge, its `app` by rebase: the merged range's
/// locks pin `app` commits the rebase replaced, which no ref keeps once the
/// landing drops its rebased backups. `main` publishes, and machine B, which
/// never held them, lands in `app`: the landing's merge commit re-pinned
/// `app` by name, so those pins do not decide the base.
pub fn merged_project_with_rebased_members_on_a_second_machine() -> Scenario {
    let app = Repo::App.rel_path();
    scenario(
        "a merged project with rebased members on a second machine",
        vec![
            Step::rwv(&a(), &["branch", "set-history", "merge"]).expect(Exit::Succeeds),
            Step::rwv(&a(), &["branch", "set-history", "linear", "--repo", &app])
                .expect(Exit::Succeeds),
            create("w"),
            Step::commit(&ww("w"), Repo::App, "w-app"),
            Step::rwv(&ww("w"), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::commit(&ww("w"), Repo::App, "w-app-2"),
            Step::rwv(&ww("w"), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::commit(&a(), Repo::App, "main-app"),
            Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::rwv(&ww("w"), &["land", "--retire"])
                .expect(Exit::Succeeds)
                .may_rewrite(&[Repo::App])
                .claim(Claim::WorkweaveGone(ww("w"))),
            publishes_records(Step::rwv(&a(), &["push"]))
                .pushes(Remote::origin(Repo::App), "main")
                .pushes(Remote::origin(Repo::Proj), "main")
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains("local/remotes/app: pin waived"))
                .claim(Claim::OutputContains("a rebase replaced it:"))
                .claim(Claim::OutputLacks("held by no branch")),
            Step::new_machine(Machine::B),
            Step::rwv(&b(), &["workweave", "create", "--project", "proj", "w2"])
                .expect(Exit::Succeeds),
            Step::commit(&b_ww("w2"), Repo::App, "w2-app"),
            Step::commit(&b(), Repo::App, "b-main-app"),
            Step::rwv(&b(), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::rwv(&b_ww("w2"), &["land"])
                .expect(Exit::Succeeds)
                .may_rewrite(&[Repo::App]),
            publishes_records(Step::rwv(&b(), &["push"]))
                .pushes(Remote::origin(Repo::App), "main")
                .pushes(Remote::origin(Repo::Proj), "main")
                .expect(Exit::Succeeds),
        ],
    )
}

/// A project that lands by merge, its `app` by `history` (`linear` or
/// `ff-only`): `rwv sync` rebases `app` and merges `main` into the seat's
/// project branch, the merge recording the rebase's move of `app` by name.
/// Under `ff-only` the landing first refuses, naming `rwv sync`. The landing
/// and push on A pass with the pre-rebase pins waived, and machine B lands
/// and pushes.
pub fn sync_rebased_members_in_a_merged_project_on_a_second_machine(
    history: &'static str,
) -> Scenario {
    let app = Repo::App.rel_path();
    let mut steps = vec![
        Step::rwv(&a(), &["branch", "set-history", "merge"]).expect(Exit::Succeeds),
        Step::rwv(&a(), &["branch", "set-history", history, "--repo", &app]).expect(Exit::Succeeds),
        create("w"),
        Step::commit(&ww("w"), Repo::App, "w-app"),
        Step::rwv(&ww("w"), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::commit(&ww("w"), Repo::App, "w-app-2"),
        Step::rwv(&ww("w"), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::commit(&a(), Repo::App, "main-app"),
        Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
    ];
    if history == "ff-only" {
        steps.push(
            Step::rwv(&ww("w"), &["land"])
                .expect(Exit::refuses(Repo::App, State::HistoryNotFastForward))
                .claim(Claim::OutputContains("rwv sync")),
        );
    }
    steps.extend([
        Step::rwv(&ww("w"), &["sync"])
            .expect(Exit::Succeeds)
            .may_rewrite(&[Repo::App]),
        Step::rwv(&ww("w"), &["land", "--retire"])
            .expect(Exit::Succeeds)
            .may_rewrite(&[Repo::App])
            .claim(Claim::WorkweaveGone(ww("w"))),
        publishes_records(Step::rwv(&a(), &["push"]))
            .pushes(Remote::origin(Repo::App), "main")
            .pushes(Remote::origin(Repo::Proj), "main")
            .expect(Exit::Succeeds)
            .claim(Claim::OutputContains("a rebase replaced it:"))
            .claim(Claim::OutputLacks("held by no branch")),
        Step::new_machine(Machine::B),
        Step::rwv(&b(), &["workweave", "create", "--project", "proj", "w2"]).expect(Exit::Succeeds),
        Step::commit(&b_ww("w2"), Repo::App, "w2-app"),
        Step::rwv(&b_ww("w2"), &["land"])
            .expect(Exit::Succeeds)
            .may_rewrite(&[Repo::App]),
        publishes_records(Step::rwv(&b(), &["push"]))
            .pushes(Remote::origin(Repo::App), "main")
            .pushes(Remote::origin(Repo::Proj), "main")
            .expect(Exit::Succeeds),
    ]);
    scenario(
        "sync-rebased members in a merged project on a second machine",
        steps,
    )
}

/// A merged project's landing whose member rebase was rewritten by hand
/// before the relock: no record says which commit became which, so the
/// landing writes no re-pin on the operator's behalf and stops before its
/// project commit.
pub fn a_landing_stops_before_its_project_commit_where_no_record_describes_a_member_rebase(
) -> Scenario {
    let app = Repo::App.rel_path();
    scenario(
        "a landing stops where no record describes a member rebase",
        vec![
            Step::rwv(&a(), &["branch", "set-history", "merge"]).expect(Exit::Succeeds),
            Step::rwv(&a(), &["branch", "set-history", "linear", "--repo", &app])
                .expect(Exit::Succeeds),
            create("w"),
            Step::commit(&ww("w"), Repo::App, "w-app"),
            Step::rwv(&ww("w"), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::commit(&a(), Repo::App, "main-app"),
            Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::rwv_paused(&ww("w"), &["land"], "relocking", "relocking"),
            Step::git(
                &ww("w"),
                Repo::App,
                &["commit", "--amend", "-q", "-m", "w-app by hand"],
            )
            .may_strand(&[Repo::App])
            .may_leave_unresolved(&[Repo::App]),
            Step::release("relocking")
                .expect(Exit::refuses_kind(RefusalKind::OpParked))
                .may_rewrite(&[Repo::App])
                .claim(Claim::OutputContains("no record of the rebase"))
                .claim(Claim::OutputLacks("re-locked in the merge")),
        ],
    )
}

/// How a landing composes the project repo in [`landing_writes_no_relock_of_its_own`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectLands {
    FastForward,
    Rebase,
    Merge,
}

/// A seat lands without `--retire`, then pushes from the seat. Its member
/// branch holds nothing beyond the target's pin, so the push leaves it alone,
/// and its pin is published only by publishing the target: the refusal names
/// that push, which runs from the seat, and the seat's push then publishes.
pub fn a_seat_pushes_after_landing_without_retire() -> Scenario {
    scenario(
        "a seat pushes after landing without retire",
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
            Step::commit(&ww("w"), Repo::App, "w-app-2"),
            Step::rwv(&ww("w"), &["land"]).expect(Exit::Succeeds),
            Step::rwv(&ww("w"), &["push"])
                .expect(Exit::refuses(Repo::App, State::PublishWithheld))
                .claim(Claim::OutputLacks("rwv push --repo")),
            publishes_records(Step::rwv_printed(&ww("w"), "rwv push --branch main"))
                .pushes(Remote::origin(Repo::App), "main")
                .pushes(Remote::origin(Repo::Proj), "main")
                .expect(Exit::Succeeds)
                .claim(Claim::RemoteContains {
                    remote: Remote::origin(Repo::App),
                    branch: "main",
                    mark: "w-app-2",
                }),
            publishes_records(Step::rwv(&ww("w"), &["push"]))
                .pushes(Remote::origin(Repo::Proj), "feat-w")
                .expect(Exit::Succeeds),
        ],
    )
}

/// A seat whose lock is behind its members' tips lands. The landing writes
/// no relock commit of the source's own: the target's history gains the
/// seat's commits and the landing's one project commit, which records the
/// tips, and the seat ends on the target's tip with nothing to relock.
pub fn landing_writes_no_relock_of_its_own(how: ProjectLands) -> Scenario {
    let mut steps = Vec::new();
    if how == ProjectLands::Merge {
        steps.push(Step::rwv(&a(), &["branch", "set-history", "merge"]).expect(Exit::Succeeds));
    }
    steps.extend([
        create("w"),
        Step::commit(&ww("w"), Repo::App, "w-app"),
        Step::rwv(&ww("w"), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::commit(&ww("w"), Repo::App, "w-app-2"),
    ]);
    if how != ProjectLands::FastForward {
        steps.extend([
            Step::commit(&a(), Repo::Lib, "main-lib"),
            Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
        ]);
    }
    let land: &[&str] = match how {
        ProjectLands::Rebase => &["land", "--history", "linear"],
        _ => &["land"],
    };
    steps.extend([
        Step::rwv(&ww("w"), land)
            .expect(Exit::Succeeds)
            .claim(Claim::OutputContains(
                "lock behind HEAD by 1 commits — the landing records",
            ))
            .claim(Claim::PinContains {
                site: main_lock(),
                member: Repo::App,
                mark: "w-app-2",
            }),
        Step::git(
            &a(),
            Repo::Proj,
            &["rev-list", "--count", "--grep=auto-relock", "main"],
        )
        .claim(Claim::OutputLine(if how == ProjectLands::Merge {
            "0"
        } else {
            "1"
        })),
        Step::rwv(&ww("w"), &["status"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputLacks("lock behind")),
        Step::git(&ww("w"), Repo::Proj, &["diff", "--quiet", "HEAD", "main"])
            .expect(Exit::Succeeds),
    ]);
    scenario("a landing writes no relock of its own", steps)
}

/// A seat's `lib` takes, by hand, a commit its declared branch's upstream
/// already holds, and nothing relocks it: `lib` holds no unlanded work, so the
/// landing does not move it, yet the tip it found when it began is what the
/// landed lock pins.
pub fn a_landing_records_the_tip_of_a_member_it_does_not_move() -> Scenario {
    scenario(
        "a landing records the tip of a member it does not move",
        vec![
            create("w"),
            Step::forge(Forge::Commit {
                repo: Repo::Lib,
                branch: "main",
                mark: "forge-lib",
            }),
            Step::git(&ww("w"), Repo::Lib, &["fetch", "-q", "origin"]),
            Step::git(
                &ww("w"),
                Repo::Lib,
                &["merge", "-q", "--ff-only", "origin/main"],
            ),
            Step::commit(&ww("w"), Repo::App, "w-app"),
            Step::rwv(&ww("w"), &["land"])
                .expect(Exit::Succeeds)
                .claim(Claim::PinContains {
                    site: main_lock(),
                    member: Repo::Lib,
                    mark: "forge-lib",
                }),
        ],
    )
}

/// A landing refused at its preconditions, with the seat's lock behind its
/// members and an uncommitted `rwv.lock` beside it, leaves the seat exactly
/// as it stood: no relock was written or committed.
pub fn a_refused_landing_leaves_a_lock_behind_as_it_stood() -> Scenario {
    scenario(
        "a refused landing leaves a lock behind as it stood",
        vec![
            create("w"),
            Step::commit(&ww("w"), Repo::App, "w-app"),
            Step::commit(&a(), Repo::App, "main-app"),
            Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::rwv(&ww("w"), &["lock"]).expect(Exit::Succeeds),
            Step::commit(&ww("w"), Repo::App, "w-app-2"),
            Step::rwv(&ww("w"), &["land"])
                .expect(Exit::refuses(Repo::App, State::HistoryUnset))
                .claim(Claim::OutputLacks("the landing records"))
                .claim(Claim::LocalStateUnchanged(ww("w"))),
        ],
    )
}

/// A lock commit of the target's history pins a member commit machine B
/// lacks, and no later lock re-pinned it by name: B's landing in that member
/// still refuses, and the printed fetch clears it once the commit is
/// published.
pub fn a_history_pin_not_repinned_still_refuses() -> Scenario {
    scenario(
        "a history pin not re-pinned still refuses",
        vec![
            Step::new_machine(Machine::B),
            Step::rwv(&b(), &["workweave", "create", "--project", "proj", "w2"])
                .expect(Exit::Succeeds),
            Step::commit(&b_ww("w2"), Repo::App, "w2-app"),
            Step::commit(&a(), Repo::App, "a-app"),
            Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::commit(&a(), Repo::App, "a-app-2"),
            Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::git(&a(), Repo::Proj, &["push", "origin", "main"])
                .pushes(Remote::origin(Repo::Proj), "main")
                .may_leave_unresolved(&[Repo::App]),
            Step::git(&b(), Repo::Proj, &["fetch", "origin"]).may_leave_unresolved(&[Repo::App]),
            Step::rwv(&b_ww("w2"), &["land"])
                .may_leave_unresolved(&[Repo::App])
                .expect(Exit::refuses(Repo::App, State::PinUnreachable))
                .claim(Claim::OutputContains("rwv fetch"))
                .claim(Claim::OutputLacks("history pin skipped"))
                .claim(Claim::LocalStateUnchanged(b_ww("w2"))),
            Step::rwv(&a(), &["push", "--repo", &Repo::App.rel_path()])
                .pushes(Remote::origin(Repo::App), "main")
                .expect(Exit::Succeeds),
            Step::rwv(&b(), &["fetch"]).expect(Exit::Succeeds),
            Step::rwv(&b_ww("w2"), &["land", "--history", "linear"]).expect(Exit::Succeeds),
        ],
    )
}

/// `parallel-retire-fetch`: four published seats retire in one clone. A
/// pruning fetch that meets the store's fetch claim held by a retiring seat
/// waits for it rather than failing on a ref lock, and each retire's
/// deletion completes. A second seat lands while the first retires, its
/// landing claim on `main` already given up, and its own retire's fetch then
/// waits on the first's, as a plain fetch does. `main` records `merge`, so
/// each later seat lands by a merge commit whose first parent is `main` and
/// which its published branch is an ancestor of: no published branch is
/// rewritten, and each retire deletes what landed.
pub fn parallel_retire_fetch() -> Scenario {
    let seats = [
        ("s1", "feat-1", "f1-app"),
        ("s2", "feat-2", "f2-app"),
        ("s3", "feat-3", "f3-app"),
        ("s4", "feat-4", "f4-app"),
    ];
    let mut steps =
        vec![Step::rwv(&a(), &["branch", "set-history", "merge"]).expect(Exit::Succeeds)];
    for (seat, branch, mark) in seats {
        steps.extend([
            Step::rwv(
                &a(),
                &[
                    "workweave",
                    "create",
                    "--project",
                    "proj",
                    seat,
                    "--new-branch",
                    branch,
                ],
            )
            .expect(Exit::Succeeds),
            Step::commit(&ww(seat), Repo::App, mark),
            Step::rwv(&ww(seat), &["lock", "--commit"]).expect(Exit::Succeeds),
            publishes_records(Step::rwv(&ww(seat), &["push"]))
                .pushes(Remote::origin(Repo::App), branch)
                .pushes(Remote::origin(Repo::Proj), branch)
                .expect(Exit::Succeeds),
        ]);
    }
    let retired = |step: Step, seat: &str, mark: &'static str| {
        step.expect(Exit::Succeeds)
            .claim(Claim::WorkweaveGone(ww(seat)))
            .claim(Claim::OutputLacks("left:"))
            .claim(Claim::Contains {
                at: a(),
                repo: Repo::App,
                rev: "main",
                mark,
            })
    };
    let after_s1 = |step: Step| {
        step.claim(Claim::OutputLacks("rebased"))
            .claim(Claim::Contains {
                at: a(),
                repo: Repo::App,
                rev: "main^1",
                mark: "f1-app",
            })
    };
    steps.extend([
        Step::rwv_paused(
            &ww("s1"),
            &["land", "--retire"],
            "fetch-under-claim",
            "retire-s1",
        ),
        Step::rwv_paused(&ww("s2"), &["land", "--retire"], "claim-held", "retire-s2"),
        Step::rwv_paused(&a(), &["fetch"], "claim-held", "fetch-a"),
        retired(Step::release("retire-s1"), "s1", "f1-app"),
        retired(after_s1(Step::release("retire-s2")), "s2", "f2-app"),
        Step::release("fetch-a").expect(Exit::Succeeds),
    ]);
    for (seat, _, mark) in &seats[2..] {
        steps.push(retired(
            after_s1(Step::rwv(&ww(seat), &["land", "--retire"])),
            seat,
            mark,
        ));
    }
    scenario("parallel retire fetch", steps)
}

/// `dc11-drift-before-dirt`: a member's branch moved under its worktree, by a
/// move made from the primary's checkout of the same store, so the seat's
/// index and files describe the old tip. Drift is refreshed and reported
/// before edits are judged, and the landing carries the moved tip. Beside an
/// edit to a file the move did not touch, the drift is still refreshed and the
/// edit alone is refused, so the commit C13 offers cannot carry the move's
/// reverse. A file both moved and edited is left as it stands; what the
/// landing then offers is undecided.
pub fn dc11_drift_before_dirt() -> Scenario {
    scenario(
        "dc11 drift before dirt",
        vec![
            create("w"),
            Step::commit(&ww("w"), Repo::Lib, "w-lib"),
            Step::hand_commit(&a(), Repo::Lib, "proj--w", &[("moved.txt", "moved\n")]),
            Step::rwv(&ww("w"), &["land"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains("refreshed drift"))
                .claim(Claim::OutputContains("moved.txt"))
                .claim(Claim::OutputLacks("commit your edits"))
                .claim(Claim::Contains {
                    at: a(),
                    repo: Repo::Lib,
                    rev: "main",
                    mark: "w-lib",
                }),
            Step::git(&a(), Repo::Lib, &["cat-file", "-e", "main:moved.txt"]),
            Step::git(&ww("w"), Repo::Lib, &["status", "--porcelain"])
                .claim(Claim::OutputLacks("moved.txt")),
            Step::hand_edit(&ww("w"), Repo::Lib, "w-lib.txt", "w-lib", "edited"),
            Step::hand_commit(&a(), Repo::Lib, "proj--w", &[("moved-again.txt", "x\n")]),
            Step::rwv(&ww("w"), &["land"])
                .expect(Exit::refuses(Repo::Lib, State::EditsDirty))
                .claim(Claim::OutputContains("refreshed drift"))
                .claim(Claim::OutputContains("moved-again.txt"))
                .claim(Claim::OutputLacks("    moved-again.txt")),
            Step::git(
                &ww("w"),
                Repo::Lib,
                &["grep", "-q", "edited", "--", "w-lib.txt"],
            ),
            Step::git(
                &ww("w"),
                Repo::Lib,
                &["diff", "--quiet", "HEAD", "--", "moved-again.txt"],
            ),
            Step::git(
                &ww("w"),
                Repo::Lib,
                &["commit", "-q", "-a", "-m", "w-lib edited"],
            ),
            Step::hand_edit(&ww("w"), Repo::Lib, "moved-again.txt", "x", "mine"),
            Step::hand_commit(&a(), Repo::Lib, "proj--w", &[("moved-again.txt", "y\n")]),
            Step::rwv(&ww("w"), &["land"]),
            Step::git(
                &ww("w"),
                Repo::Lib,
                &["grep", "-q", "mine", "--", "moved-again.txt"],
            ),
        ],
    )
}

const LIB: &str = "local/remotes/lib";
const APP: &str = "local/remotes/app";
const DOCS: &str = "local/remotes/docs";

/// `seat-follows-after-landing`: a permanent seat whose `lib` holds no work
/// of its own declares `lib` on `dev` and lands that change by name. The
/// seat's `lib` moves to `dev`'s tip, keeping the tip it left in a backup, and
/// the target's workspace follows `dev`.
pub fn seat_follows_after_landing() -> Scenario {
    scenario(
        "seat follows after landing",
        vec![
            create("seat"),
            Step::commit(&a(), Repo::Lib, "main-lib"),
            Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::rwv(&ww("seat"), &["sync"])
                .expect(Exit::Succeeds)
                .claim(Claim::Contains {
                    at: ww("seat"),
                    repo: Repo::Lib,
                    rev: "HEAD",
                    mark: "main-lib",
                }),
            Step::rwv(&ww("seat"), &["member", "set-branch", LIB, "dev"]).expect(Exit::Succeeds),
            Step::rwv(&ww("seat"), &["land"])
                .expect(Exit::refuses(Repo::Lib, State::TargetMemberChanges))
                .claim(Claim::OutputContains("branch: main → dev"))
                .claim(Claim::LocalStateUnchanged(ww("seat")))
                .claim(Claim::LocalStateUnchanged(a())),
            Step::rwv(&ww("seat"), &["land", "--allow-member-change", LIB])
                .expect(Exit::Succeeds)
                .may_rewind(&[Repo::Lib])
                .claim(Claim::BackedUp {
                    repo: Repo::Lib,
                    mark: "main-lib",
                    marked: None,
                })
                .claim(Claim::AtPin {
                    at: ww("seat"),
                    repo: Repo::Lib,
                })
                .claim(Claim::OnBranch {
                    at: a(),
                    repo: Repo::Lib,
                    branch: "dev",
                })
                .claim(Claim::AtPin {
                    at: a(),
                    repo: Repo::Lib,
                }),
        ],
    )
}

/// `member-change-race-listing`: a fix lands on `main`'s `lib` while a
/// feature cut before it moves `lib` to `dev`. The landing lists the fix as
/// a commit `dev` lacks, and `status` keeps listing it until the change is
/// published.
pub fn member_change_race_listing() -> Scenario {
    scenario(
        "member change race listing",
        vec![
            create("fix"),
            create("feat"),
            Step::commit(&ww("fix"), Repo::Lib, "lib-fix"),
            Step::rwv(&ww("fix"), &["land", "--retire"]).expect(Exit::Succeeds),
            Step::rwv(&ww("feat"), &["member", "set-branch", LIB, "dev"]).expect(Exit::Succeeds),
            Step::commit(&ww("feat"), Repo::App, "feat-app"),
            Step::rwv(
                &ww("feat"),
                &["land", "--history", "linear", "--allow-member-change", LIB],
            )
            .expect(Exit::Succeeds)
            .may_rewind(&[Repo::Lib])
            .claim(Claim::OutputInOrder(&[
                "the target's pin reaches commits 'dev' lacks",
                "lib-fix",
                "copy these commits onto `dev`",
            ]))
            .claim(Claim::Contains {
                at: a(),
                repo: Repo::App,
                rev: "main",
                mark: "feat-app",
            }),
            Step::rwv(&a(), &["status"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains("member-changed"))
                .claim(Claim::OutputContains("lib-fix")),
            Step::rwv(&a(), &["status"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains("lib-fix")),
            publishes_records(Step::rwv(&a(), &["push"]))
                .pushes(Remote::origin(Repo::Proj), "main")
                .pushes(Remote::origin(Repo::App), "main")
                .pushes(Remote::origin(Repo::Lib), "main")
                .pushes(Remote::origin(Repo::Lib), "dev")
                .may_rewind(&[Repo::Lib])
                .expect(Exit::Succeeds),
            Step::rwv(&a(), &["status"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputLacks("member-changed"))
                .claim(Claim::OutputLacks("lib-fix")),
        ],
    )
}

/// A member on a branch of its own with a commit nothing else reaches, its
/// expected branch checked out in the primary: the printed copy records its
/// source, so the commit counts as reached, the state moves to an empty stray
/// set, and its printed `rwv update` aligns the member.
pub fn stray_commits_copied_onto_the_checked_out_branch() -> Scenario {
    Scenario {
        name: "stray commits copied onto the checked-out branch",
        seed: Seed::default().role(Repo::Lib, Role::Dependency),
        steps: vec![
            create("w"),
            Step::git(&ww("w"), Repo::Lib, &["switch", "-q", "-c", "stray"]),
            Step::commit(&ww("w"), Repo::Lib, "stray-one"),
            Step::rwv(&ww("w"), &["lock"])
                .expect(Exit::refuses(
                    Repo::Lib,
                    State::AlignmentOnBranchWithStrayCommits,
                ))
                .claim(Claim::OutputContains("cherry-pick -x"))
                .claim(Claim::OutputContains("rwv lock --commit")),
            Step::rwv_printed_holding(&ww("w"), "git -C", "cherry-pick").expect(Exit::Succeeds),
            Step::git(&a(), Repo::Lib, &["cat-file", "-e", "main:stray-one.txt"])
                .expect(Exit::Succeeds),
            Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::rwv(&ww("w"), &["lock"])
                .expect(Exit::refuses(Repo::Lib, State::AlignmentOnBranch))
                .claim(Claim::OutputLacks("stray-one")),
            Step::rwv_printed_exactly(&ww("w"), "rwv update").expect(Exit::Succeeds),
            Step::rwv(&ww("w"), &["lock"]).expect(Exit::Succeeds),
        ],
    }
}

/// A seat lands `lib`'s move from `main` to `dev` while `main` holds
/// `alpha`, a commit its push branch lacks and `dev` does not hold.
fn member_change_leaves_main_unpushed() -> Vec<Step> {
    vec![
        Step::new_machine(Machine::B),
        create("seat"),
        Step::hand_edit(&a(), Repo::Lib, "README", "lib", "alpha"),
        Step::git(&a(), Repo::Lib, &["commit", "-qam", "alpha"]),
        Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::rwv(&ww("seat"), &["sync"]).expect(Exit::Succeeds),
        Step::rwv(&ww("seat"), &["member", "set-branch", LIB, "dev"]).expect(Exit::Succeeds),
        Step::commit(&ww("seat"), Repo::App, "feat-app"),
        Step::rwv(
            &ww("seat"),
            &["land", "--history", "linear", "--allow-member-change", LIB],
        )
        .expect(Exit::Succeeds)
        .may_rewind(&[Repo::Lib]),
    ]
}

/// The push publishing the range that declared `main` refuses `main`
/// diverged from its push branch, naming it.
fn push_refuses_main_diverged() -> Step {
    Step::rwv(&a(), &["push"])
        .expect(Exit::refuses(Repo::Lib, State::PushBranchDiverged))
        .claim(Claim::OutputContains(
            "'main' is declared by a commit being pushed",
        ))
        .claim(Claim::OutputContains(
            "rwv update --remote --merge-upstream --repo local/remotes/lib:main",
        ))
        .claim(Claim::OutputContains(
            "rwv update --remote --reset-to-upstream local/remotes/lib:main",
        ))
}

/// `main-diverged-after-member-change`, its push branch moved on elsewhere
/// without a conflict: the printed merge merges `main` as refs, checked out
/// nowhere, and the push then publishes it.
pub fn main_diverged_after_member_change() -> Scenario {
    let mut steps = member_change_leaves_main_unpushed();
    steps.extend([
        Step::forge(Forge::Commit {
            repo: Repo::Lib,
            branch: "main",
            mark: "lib-remote",
        }),
        push_refuses_main_diverged(),
        Step::rwv_printed(&a(), "rwv update --remote --merge-upstream")
            .expect(Exit::Succeeds)
            .claim(Claim::OnBranch {
                at: a(),
                repo: Repo::Lib,
                branch: "dev",
            })
            .claim(Claim::Contains {
                at: a(),
                repo: Repo::Lib,
                rev: "main",
                mark: "lib-remote",
            }),
        publishes_records(Step::rwv(&a(), &["push"]))
            .pushes(Remote::origin(Repo::Proj), "main")
            .pushes(Remote::origin(Repo::App), "main")
            .pushes(Remote::origin(Repo::Lib), "main")
            .may_rewind(&[Repo::Lib])
            .expect(Exit::Succeeds),
        Step::git(
            &a(),
            Repo::Lib,
            &["grep", "-q", "alpha", "origin/main", "--", "README"],
        )
        .expect(Exit::Succeeds),
    ]);
    scenario("main diverged after member change", steps)
}

/// `main-diverged-after-member-change` where `lib` declares `DERIVED`
/// regenerated: `main` and its push branch each changed it, and `OTHER` too
/// where `other` is set. The merge as refs resolves `DERIVED` to the
/// upstream's content, as it does in a checkout, and stops on `OTHER`.
fn main_diverged_deriving(other: bool) -> Scenario {
    let upstream: &'static [(&'static str, &'static str)] = if other {
        &[("DERIVED", "upstream\n"), ("OTHER", "upstream-other\n")]
    } else {
        &[("DERIVED", "upstream\n")]
    };
    let local: &'static [(&'static str, &'static str)] = if other {
        &[("DERIVED", "local\n"), ("OTHER", "local-other\n")]
    } else {
        &[("DERIVED", "local\n")]
    };
    let mut steps = vec![
        Step::new_machine(Machine::B),
        Step::hand_commit(
            &a(),
            Repo::Lib,
            "main",
            &[
                (".gitattributes", "DERIVED merge=rwv-ours\n"),
                ("DERIVED", "base\n"),
                ("OTHER", "base\n"),
            ],
        ),
        Step::git(&a(), Repo::Lib, &["reset", "-q", "--hard"]),
        Step::git(&a(), Repo::Lib, &["push", "-q", "origin", "main"])
            .pushes(Remote::origin(Repo::Lib), "main")
            .expect(Exit::Succeeds),
        Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
    ];
    steps.extend([
        create("seat"),
        Step::hand_commit(&a(), Repo::Lib, "main", local),
        Step::git(&a(), Repo::Lib, &["reset", "-q", "--hard"]),
        Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::rwv(&ww("seat"), &["sync"]).expect(Exit::Succeeds),
        Step::rwv(&ww("seat"), &["member", "set-branch", LIB, "dev"]).expect(Exit::Succeeds),
        Step::commit(&ww("seat"), Repo::App, "feat-app"),
        Step::rwv(
            &ww("seat"),
            &["land", "--history", "linear", "--allow-member-change", LIB],
        )
        .expect(Exit::Succeeds)
        .may_rewind(&[Repo::Lib]),
        Step::forge(Forge::Files {
            repo: Repo::Lib,
            branch: "main",
            files: upstream,
        }),
        push_refuses_main_diverged(),
    ]);
    let merge = Step::rwv_printed(&a(), "rwv update --remote --merge-upstream");
    if other {
        steps.extend([
            merge
                .expect(Exit::refuses(Repo::Lib, State::PushBranchDiverged))
                .claim(Claim::OutputContains(
                    "'main' is checked out nowhere and its merge as refs conflicts in OTHER",
                )),
            Step::git(&a(), Repo::Lib, &["show", "main:OTHER"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains("local-other")),
        ]);
    } else {
        steps.extend([
            merge.expect(Exit::Succeeds),
            Step::git(&a(), Repo::Lib, &["show", "main:DERIVED"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains("upstream")),
        ]);
    }
    scenario(
        if other {
            "main diverged after member change, an undeclared path both sides changed"
        } else {
            "main diverged after member change, a derived path both sides regenerated"
        },
        steps,
    )
}

pub fn main_diverged_resolves_derived_to_the_upstream() -> Scenario {
    main_diverged_deriving(false)
}

pub fn main_diverged_still_stops_on_an_undeclared_path() -> Scenario {
    main_diverged_deriving(true)
}

/// The same, settled by the printed reset: `main` takes its upstream, `alpha`
/// kept in a backup, and the push publishes nothing of `main`.
pub fn main_diverged_after_member_change_reset() -> Scenario {
    let mut steps = member_change_leaves_main_unpushed();
    steps.extend([
        Step::forge(Forge::Commit {
            repo: Repo::Lib,
            branch: "main",
            mark: "lib-remote",
        }),
        push_refuses_main_diverged(),
        Step::rwv_printed(&a(), "rwv update --remote --reset-to-upstream")
            .expect(Exit::Succeeds)
            .may_rewrite(&[Repo::Lib])
            .claim(Claim::Contains {
                at: a(),
                repo: Repo::Lib,
                rev: "main",
                mark: "lib-remote",
            }),
        Step::git(&a(), Repo::Lib, &["diff", "--quiet", "main", "origin/main"])
            .expect(Exit::Succeeds),
        publishes_records(Step::rwv(&a(), &["push"]))
            .pushes(Remote::origin(Repo::Proj), "main")
            .pushes(Remote::origin(Repo::App), "main")
            .may_rewind(&[Repo::Lib])
            .expect(Exit::Succeeds),
    ]);
    scenario("main diverged after member change, reset", steps)
}

/// A landing whose target's lock goes missing between its move and the
/// alignment that follows stops there, leaving its record, and `--continue`
/// finishes it once the lock is back.
pub fn landing_stops_where_the_landed_lock_does_not_read() -> Scenario {
    scenario(
        "landing stops where the landed lock does not read",
        vec![
            create("seat"),
            Step::commit(&ww("seat"), Repo::App, "feat-app"),
            Step::rwv_paused(
                &ww("seat"),
                &["land", "--history", "linear"],
                "aligning-target",
                "aligning",
            ),
            Step::git(&a(), Repo::Proj, &["mv", "rwv.lock", "rwv.lock.aside"]),
            Step::release("aligning")
                .expect(Exit::refuses_kind(RefusalKind::OpParked))
                .claim(Claim::OutputContains("has no rwv.lock"))
                .claim(Claim::OutputContains("rwv land --continue")),
            Step::git(&a(), Repo::Proj, &["mv", "rwv.lock.aside", "rwv.lock"]),
            Step::rwv(&ww("seat"), &["land", "--continue"])
                .expect(Exit::Succeeds)
                .claim(Claim::Contains {
                    at: a(),
                    repo: Repo::App,
                    rev: "main",
                    mark: "feat-app",
                }),
        ],
    )
}

/// `lib`'s `main` as a branch operand: the declared-elsewhere branch the
/// pushed range still declares.
const LIB_MAIN: &str = "local/remotes/lib:main";

/// `main-diverged-after-member-change` with `dev` holding a commit of its own
/// that its push branch lacks: the printed reset of `main` resets `main`
/// alone, and `dev` and the lock's pin on it stay.
pub fn main_diverged_reset_leaves_the_expected_branch() -> Scenario {
    let mut steps = member_change_leaves_main_unpushed();
    steps.extend([
        Step::commit(&a(), Repo::Lib, "dev-own"),
        Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::forge(Forge::Commit {
            repo: Repo::Lib,
            branch: "main",
            mark: "lib-remote",
        }),
        push_refuses_main_diverged(),
        Step::rwv_printed(&a(), "rwv update --remote --reset-to-upstream")
            .expect(Exit::Succeeds)
            .may_rewrite(&[Repo::Lib])
            .claim(Claim::Contains {
                at: a(),
                repo: Repo::Lib,
                rev: "main",
                mark: "lib-remote",
            })
            .claim(Claim::Contains {
                at: a(),
                repo: Repo::Lib,
                rev: "dev",
                mark: "dev-own",
            })
            .claim(Claim::AtPin {
                at: a(),
                repo: Repo::Lib,
            }),
        Step::rwv(
            &a(),
            &[
                "update",
                "--remote",
                "--reset-to-upstream",
                "local/remotes/lib:dev",
            ],
        )
        .expect(Exit::refuses_kind(RefusalKind::InapplicableFlag))
        .claim(Claim::OutputContains("name it `local/remotes/lib`")),
        Step::rwv(
            &a(),
            &[
                "update",
                "--remote",
                "--reset-to-upstream",
                "local/remotes/lib:nope",
            ],
        )
        .expect(Exit::refuses_kind(RefusalKind::InapplicableFlag))
        .claim(Claim::OutputContains("declares 'nope'")),
        Step::rwv(&a(), &["update", "--remote", "--repo", "glob:local/*:main"])
            .expect(Exit::refuses_kind(RefusalKind::InapplicableFlag))
            .claim(Claim::OutputContains("never one branch of a repo")),
    ]);
    scenario("main diverged, its reset leaves the expected branch", steps)
}

/// A branch named by the `<m>:<branch>` operand of `update --remote` cannot
/// be reset or merged under a landing that holds a claim on it: a landing
/// into `rel`, a project branch declaring `lib`'s `main`, stopped once it
/// composed, claims that `main`; `rwv abort` frees it.
pub fn a_branch_named_by_an_update_operand_is_refused_under_a_landing_claim(
    merge: bool,
) -> Scenario {
    let mut steps = vec![
        Step::rwv(&a(), &["branch", "create", "rel", "--declared"]).expect(Exit::Succeeds),
        Step::rwv(&a(), &["switch", "main"]).expect(Exit::Succeeds),
        create("relw"),
        Step::commit(&ww("relw"), Repo::Lib, "rel-lib"),
    ];
    steps.extend(member_change_leaves_main_unpushed());
    steps.extend([
        Step::rwv(
            &a(),
            &[
                "workweave",
                "create",
                "--project",
                "proj",
                "relb",
                "--branch",
                "rel",
            ],
        )
        .expect(Exit::Succeeds),
        Step::rwv(&ww("relb"), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::rwv(&a(), &["workweave", "delete", "--project", "proj", "relb"])
            .expect(Exit::Succeeds),
        Step::rwv_paused(
            &ww("relw"),
            &["land", "--target", "rel", "--history", "linear"],
            "composed",
            "landing",
        ),
        Step::kill("landing"),
        Step::forge(Forge::Commit {
            repo: Repo::Lib,
            branch: "main",
            mark: "lib-remote",
        }),
    ]);
    let flags: &[&str] = if merge {
        &["--merge-upstream", "--repo", LIB_MAIN]
    } else {
        &["--reset-to-upstream", LIB_MAIN]
    };
    let mut update = vec!["update", "--remote"];
    update.extend_from_slice(flags);
    steps.extend([
        Step::rwv(&a(), &update)
            .expect(Exit::refuses_kind(RefusalKind::StateClaimHeld))
            .claim(Claim::OutputContains("rwv abort"))
            .claim(Claim::LocalStateUnchanged(a())),
        Step::rwv(&ww("relw"), &["abort"])
            .expect(Exit::Succeeds)
            .may_rewind(&[Repo::Lib, Repo::Proj])
            .may_restore(&[Repo::Lib, Repo::Proj]),
        Step::rwv(&a(), &update)
            .expect(Exit::Succeeds)
            .may_rewrite(&[Repo::Lib]),
    ]);
    scenario(
        "a branch named by an update operand is refused under a landing claim",
        steps,
    )
}

/// A colleague's commit to `lib`'s `main` that conflicts with `alpha`, and
/// the push refusing `main` diverged, then the printed merge refusing: `main`
/// is checked out nowhere and its merge as refs would conflict.
fn main_diverged_conflicting() -> Vec<Step> {
    vec![
        Step::hand_edit(&b(), Repo::Lib, "README", "lib", "beta"),
        Step::git(&b(), Repo::Lib, &["commit", "-qam", "beta"]),
        Step::git(&b(), Repo::Lib, &["push", "-q", "origin", "main"])
            .pushes(Remote::origin(Repo::Lib), "main"),
        push_refuses_main_diverged(),
        Step::rwv_printed(&a(), "rwv update --remote --merge-upstream")
            .expect(Exit::refuses(Repo::Lib, State::PushBranchDiverged))
            .claim(Claim::OutputContains(
                "'main' is checked out nowhere and its merge as refs conflicts in README",
            ))
            .claim(Claim::OutputLacks("merged the upstream")),
    ]
}

/// `main-diverged-after-member-change`, the merge conflicting and `dev`
/// holding `alpha` by ancestry: the refusal offers the reset alone, saying
/// `dev` holds it; run as printed, the push publishes `dev` and every pin of
/// the range is reached through it.
pub fn main_diverged_conflicting_held() -> Scenario {
    let mut steps = member_change_leaves_main_unpushed();
    steps.extend([
        Step::git(&a(), Repo::Lib, &["merge", "-q", "--no-edit", "main"]),
        Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
    ]);
    steps.extend(main_diverged_conflicting());
    let refused = steps.len() - 1;
    steps[refused] = steps[refused]
        .clone()
        .claim(Claim::OutputInOrder(&[
            "`dev` holds each commit of 'main' its upstream lacks",
            "take theirs; yours is kept in a backup; `dev` already holds each commit only yours \
             has",
            "rwv update --remote --reset-to-upstream local/remotes/lib:main",
        ]))
        .claim(Claim::OutputLacks("cherry-pick"))
        .claim(Claim::OutputLacks("push origin"));
    steps.extend([
        Step::rwv_printed(&a(), "rwv update --remote --reset-to-upstream")
            .expect(Exit::Succeeds)
            .may_rewrite(&[Repo::Lib]),
        publishes_records(Step::rwv(&a(), &["push"]))
            .pushes(Remote::origin(Repo::Proj), "main")
            .pushes(Remote::origin(Repo::App), "main")
            .pushes(Remote::origin(Repo::Lib), "dev")
            .may_rewind(&[Repo::Lib])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputLacks("held by no branch")),
        Step::git(
            &a(),
            Repo::Lib,
            &["grep", "-q", "alpha", "origin/dev", "--", "README"],
        )
        .expect(Exit::Succeeds),
    ]);
    scenario(
        "main diverged after member change, conflicting, held",
        steps,
    )
}

/// `main-diverged-after-member-change`, the merge conflicting and `dev`
/// lacking `alpha`: the refusal offers copying `alpha` onto `dev` with its
/// source recorded, then the reset, naming the project commit whose pin only
/// the backup will hold; run as printed, the push publishes the range and
/// reports that pin, the tip's pin carrying its recorded copy.
pub fn main_diverged_conflicting_copied() -> Scenario {
    let mut steps = member_change_leaves_main_unpushed();
    steps.extend(main_diverged_conflicting());
    let refused = steps.len() - 1;
    steps[refused] = steps[refused]
        .clone()
        .claim(Claim::OutputInOrder(&[
            "alpha",
            "pins",
            "which after the reset only a backup holds; copied and reset, the tip's pin carries \
             a recorded copy of it",
            "copy these commits onto `dev`, then take theirs",
            "cherry-pick -x",
            "rwv lock --commit",
            "rwv update --remote --reset-to-upstream local/remotes/lib:main",
            "publish this commit under a branch name you choose, then take theirs",
            "push origin",
            "rwv update --remote --reset-to-upstream local/remotes/lib:main",
        ]))
        .claim(Claim::OutputLacks(
            "already holds each commit only yours has",
        ));
    steps.extend([
        Step::rwv_printed_holding(&a(), "git -C", "cherry-pick").expect(Exit::Succeeds),
        Step::git(
            &a(),
            Repo::Lib,
            &["grep", "-q", "alpha", "dev", "--", "README"],
        )
        .expect(Exit::Succeeds),
        Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::rwv(
            &a(),
            &["update", "--remote", "--reset-to-upstream", LIB_MAIN],
        )
        .expect(Exit::Succeeds)
        .may_rewrite(&[Repo::Lib]),
        publishes_records(Step::rwv(&a(), &["push"]))
            .pushes(Remote::origin(Repo::Proj), "main")
            .pushes(Remote::origin(Repo::App), "main")
            .pushes(Remote::origin(Repo::Lib), "dev")
            .may_rewind(&[Repo::Lib])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputInOrder(&[
                "local/remotes/lib: pin held by no branch",
                "pinned by projects/proj",
                "held by backup",
                "carries a recorded copy of it",
            ])),
        Step::git(
            &a(),
            Repo::Lib,
            &["grep", "-q", "alpha", "origin/dev", "--", "README"],
        )
        .expect(Exit::Succeeds),
    ]);
    scenario(
        "main diverged after member change, conflicting, copied",
        steps,
    )
}

/// The same, by the other printed route: `main`'s unpushed commits are
/// published under a name the operator chooses, then `main` is reset; the
/// push then reports nothing held by no branch, the remote keeping `alpha`.
pub fn main_diverged_conflicting_published() -> Scenario {
    let mut steps = member_change_leaves_main_unpushed();
    steps.extend(main_diverged_conflicting());
    steps.extend([
        Step::rwv_printed_filling(&a(), "git -C", "push", &[("<name>", "main-kept")])
            .expect(Exit::Succeeds)
            .pushes(Remote::origin(Repo::Lib), "main-kept"),
        Step::rwv(
            &a(),
            &["update", "--remote", "--reset-to-upstream", LIB_MAIN],
        )
        .expect(Exit::Succeeds)
        .may_rewrite(&[Repo::Lib]),
        publishes_records(Step::rwv(&a(), &["push"]))
            .pushes(Remote::origin(Repo::Proj), "main")
            .pushes(Remote::origin(Repo::App), "main")
            .may_rewind(&[Repo::Lib])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputLacks("held by no branch")),
        Step::git(
            &a(),
            Repo::Lib,
            &["grep", "-q", "alpha", "origin/main-kept", "--", "README"],
        )
        .expect(Exit::Succeeds),
    ]);
    scenario(
        "main diverged after member change, conflicting, published",
        steps,
    )
}

/// The member change race under `--json`: the envelope lists the fix as a
/// commit `dev` lacks, with the copy that ports it, and reports `lib`,
/// holding no new work, following `dev`.
pub fn member_change_left_behind_in_json() -> Scenario {
    scenario(
        "member change left behind in json",
        vec![
            create("fix"),
            create("feat"),
            Step::commit(&ww("fix"), Repo::Lib, "lib-fix"),
            Step::rwv(&ww("fix"), &["land", "--retire"]).expect(Exit::Succeeds),
            Step::rwv(&ww("feat"), &["member", "set-branch", LIB, "dev"]).expect(Exit::Succeeds),
            Step::commit(&ww("feat"), Repo::App, "feat-app"),
            Step::rwv(
                &ww("feat"),
                &[
                    "land",
                    "--history",
                    "linear",
                    "--allow-member-change",
                    LIB,
                    "--json",
                ],
            )
            .expect(Exit::Succeeds)
            .may_rewind(&[Repo::Lib])
            .claim(Claim::OutputInOrder(&[
                "\"left_behind\": [",
                "\"path\": \"local/remotes/lib\"",
                "\"was\": \"main\"",
                "\"now\": \"dev\"",
                "lib-fix",
                "copy these commits onto `dev`",
                "cherry-pick",
            ]))
            .claim(Claim::OutputInOrder(&[
                "\"reports\": [",
                "\"kind\": \"followed\"",
                "\"path\": \"local/remotes/lib\"",
                "\"backup\": ",
            ])),
        ],
    )
}

/// A dependency pinned backward in a feature and landed by name, under
/// `--json`: the envelope reports the re-pin.
pub fn backward_pin_reported_in_json() -> Scenario {
    Scenario {
        name: "backward pin reported in json",
        seed: Seed::default().role(Repo::Lib, Role::Dependency),
        steps: vec![
            create("back"),
            Step::rwv(&ww("back"), &["member", "pin", LIB, "HEAD~1", "--commit"])
                .expect(Exit::Succeeds)
                .may_rewind(&[Repo::Lib]),
            Step::commit(&a(), Repo::App, "main-app"),
            Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::rwv(
                &ww("back"),
                &[
                    "land",
                    "--history",
                    "linear",
                    "--allow-member-change",
                    LIB,
                    "--json",
                ],
            )
            .expect(Exit::Succeeds)
            .may_rewind(&[Repo::Lib])
            .claim(Claim::OutputInOrder(&[
                "\"reports\": [",
                "\"kind\": \"re-pinned\"",
                "\"path\": \"local/remotes/lib\"",
                "\"was\": ",
                "\"now\": ",
            ])),
        ],
    }
}

/// The member change race with the landing stopped on a conflict in `app`:
/// `--continue` lists the fix `dev` lacks again, with its copy, as the start
/// did, and reports again that `lib` followed `dev`, a move the stopped run
/// made.
pub fn member_change_left_behind_on_continue() -> Scenario {
    scenario(
        "member change left behind on continue",
        vec![
            create("fix"),
            create("feat"),
            Step::commit(&ww("fix"), Repo::Lib, "lib-fix"),
            Step::rwv(&ww("fix"), &["land", "--retire"]).expect(Exit::Succeeds),
            Step::hand_edit(&a(), Repo::App, "README", "app", "alpha"),
            Step::git(&a(), Repo::App, &["commit", "-qam", "alpha"]),
            Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::rwv(&ww("feat"), &["member", "set-branch", LIB, "dev"]).expect(Exit::Succeeds),
            Step::hand_edit(&ww("feat"), Repo::App, "README", "app", "beta"),
            Step::git(&ww("feat"), Repo::App, &["commit", "-qam", "beta"]),
            Step::rwv(&ww("feat"), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::rwv(
                &ww("feat"),
                &["land", "--history", "linear", "--allow-member-change", LIB],
            )
            .expect(Exit::refuses_kind(RefusalKind::OpParked))
            .may_rewind(&[Repo::Lib, Repo::App])
            .claim(Claim::OutputInOrder(&[
                "the target's pin reaches commits 'dev' lacks",
                "lib-fix",
                "copy these commits onto `dev`",
            ]))
            .claim(Claim::OutputContains(
                "local/remotes/lib: has no new work, so it takes",
            )),
            Step::git(&ww("feat"), Repo::App, &["checkout", "--theirs", "README"]),
            Step::git(&ww("feat"), Repo::App, &["add", "README"]),
            Step::rwv(&ww("feat"), &["land", "--continue"])
                .expect(Exit::Succeeds)
                .may_rewind(&[Repo::Lib])
                .may_strand(&[Repo::App])
                .claim(Claim::OutputInOrder(&[
                    "the target's pin reaches commits 'dev' lacks",
                    "lib-fix",
                    "copy these commits onto `dev`",
                ]))
                .claim(Claim::OutputContains(
                    "local/remotes/lib: has no new work, so it takes",
                )),
        ],
    )
}

/// `member-change-choice-order`: a hotfix cut from `stable` but spelled onto
/// `main` would change `main`'s entry for `lib`. The refusal prints the
/// rebind onto `stable` first, then the setter that keeps `main`'s value,
/// then the consent, which states what it changes; the rebind, run as
/// printed, lands the hotfix into `stable`, whose `app` is `main`, which the
/// primary holds: it fast-forwards there in place.
pub fn member_change_choice_order() -> Scenario {
    scenario(
        "member change choice order",
        vec![
            Step::rwv(
                &a(),
                &[
                    "branch",
                    "create",
                    "stable",
                    "--declared",
                    "--set-branch",
                    "local/remotes/lib=dev",
                ],
            )
            .expect(Exit::Succeeds)
            .may_rewind(&[Repo::Lib]),
            Step::rwv(&a(), &["switch", "main"]).expect(Exit::Succeeds),
            Step::rwv(
                &a(),
                &[
                    "workweave",
                    "create",
                    "--project",
                    "proj",
                    "st",
                    "--branch",
                    "stable",
                ],
            )
            .expect(Exit::Succeeds),
            Step::rwv(
                &a(),
                &[
                    "workweave",
                    "create",
                    "--project",
                    "proj",
                    "hotfix",
                    "--at",
                    "stable",
                ],
            )
            .expect(Exit::Succeeds),
            Step::commit(&ww("hotfix"), Repo::App, "hotfix-app"),
            Step::rwv(&ww("hotfix"), &["land"])
                .expect(Exit::refuses(Repo::Lib, State::TargetMemberChanges))
                .claim(Claim::OutputContains("branch: main → dev"))
                .claim(Claim::OutputInOrder(&[
                    "rwv branch set-mode proj--hotfix feature --target stable",
                    "rwv member set-branch local/remotes/lib main",
                    "land these changes too (this changes the target's entries, or its \
                     top-level setting)",
                    "rwv land --allow-member-change local/remotes/lib",
                ]))
                .claim(Claim::LocalStateUnchanged(a())),
            Step::rwv(
                &ww("hotfix"),
                &[
                    "branch",
                    "set-mode",
                    "proj--hotfix",
                    "feature",
                    "--target",
                    "stable",
                ],
            )
            .expect(Exit::Succeeds),
            Step::rwv(&ww("hotfix"), &["land", "--retire"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains("in place in"))
                .claim(Claim::Contains {
                    at: ww("st"),
                    repo: Repo::App,
                    rev: "HEAD",
                    mark: "hotfix-app",
                })
                .claim(Claim::Contains {
                    at: a(),
                    repo: Repo::App,
                    rev: "HEAD",
                    mark: "hotfix-app",
                }),
            Step::rwv(&a(), &["status"]).expect(Exit::Succeeds),
        ],
    )
}

/// `shared-checkout-detached`: the primary on `main` holds `docs`'s `main`;
/// a workweave on `stable`, which declares `main` for `docs` too, leaves
/// `docs` detached at its pin, stated as the `checkout` fact; a hotfix onto
/// `stable` moving `main` fast-forwards the primary's clean checkout of it in
/// place; then commits on the detached checkout take C14.
fn stable_holding_docs_detached() -> Vec<Step> {
    vec![
        Step::rwv(
            &a(),
            &[
                "branch",
                "create",
                "stable",
                "--declared",
                "--set-branch",
                "local/remotes/lib=dev",
            ],
        )
        .expect(Exit::Succeeds)
        .may_rewind(&[Repo::Lib]),
        Step::rwv(&a(), &["switch", "main"]).expect(Exit::Succeeds),
        Step::rwv(
            &a(),
            &[
                "workweave",
                "create",
                "--project",
                "proj",
                "st",
                "--branch",
                "stable",
            ],
        )
        .expect(Exit::Succeeds),
        Step::git(&ww("st"), Repo::Docs, &["symbolic-ref", "-q", "HEAD"]).expect(Exit::Code(1)),
        Step::rwv(&ww("st"), &["status"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputContains("checkout: main is checked out in"))
            .claim(Claim::OutputContains(
                "local/remotes/docs: alignment aligned",
            )),
        Step::rwv(
            &a(),
            &[
                "workweave",
                "create",
                "--project",
                "proj",
                "hotfix",
                "--at",
                "stable",
            ],
        )
        .expect(Exit::Succeeds),
        Step::rwv(
            &ww("hotfix"),
            &[
                "branch",
                "set-mode",
                "proj--hotfix",
                "feature",
                "--target",
                "stable",
            ],
        )
        .expect(Exit::Succeeds),
        Step::commit(&ww("hotfix"), Repo::Docs, "hotfix-docs"),
    ]
}

pub fn shared_checkout_detached() -> Scenario {
    let mut steps = stable_holding_docs_detached();
    steps.extend([
        Step::rwv(&ww("hotfix"), &["land", "--history", "linear", "--retire"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputContains("in place in"))
            .claim(Claim::Contains {
                at: a(),
                repo: Repo::Docs,
                rev: "main",
                mark: "hotfix-docs",
            })
            .claim(Claim::Contains {
                at: a(),
                repo: Repo::Docs,
                rev: "HEAD",
                mark: "hotfix-docs",
            })
            .claim(Claim::Contains {
                at: ww("st"),
                repo: Repo::Docs,
                rev: "HEAD",
                mark: "hotfix-docs",
            }),
        Step::git(&ww("st"), Repo::Docs, &["symbolic-ref", "-q", "HEAD"]).expect(Exit::Code(1)),
        Step::git(&a(), Repo::Docs, &["for-each-ref", "refs/rwv/pre-op/"])
            .claim(Claim::OutputLacks("-in-place")),
        Step::rwv(&a(), &["status"]).expect(Exit::Succeeds),
        Step::commit(&ww("st"), Repo::Docs, "st-docs"),
        Step::rwv(&ww("st"), &["lock"])
            .expect(Exit::refuses(Repo::Docs, State::AlignmentDetachedUnpushed))
            .claim(Claim::OutputContains("keep this work on a branch")),
    ]);
    scenario("shared checkout detached", steps)
}

/// `shared-checkout-detached` stopped once the landing landed: `docs` moved
/// both in the target's workspace, whose detached checkout aligning follows
/// to the new pin, and in place in the primary. `rwv abort` puts both back.
pub fn abort_restores_a_member_moved_in_the_target_and_in_place() -> Scenario {
    let mut steps = stable_holding_docs_detached();
    steps.extend([
        Step::rwv_paused(
            &ww("hotfix"),
            &["land", "--history", "linear"],
            "landed",
            "landing",
        )
        .claim(Claim::Contains {
            at: a(),
            repo: Repo::Docs,
            rev: "HEAD",
            mark: "hotfix-docs",
        })
        .claim(Claim::Contains {
            at: ww("st"),
            repo: Repo::Docs,
            rev: "HEAD",
            mark: "hotfix-docs",
        }),
        Step::kill("landing"),
        Step::rwv(&ww("hotfix"), &["abort"])
            .expect(Exit::Succeeds)
            .may_rewind(&[Repo::Docs, Repo::Proj])
            .may_restore(&[Repo::Docs, Repo::Proj]),
        Step::git(&a(), Repo::Docs, &["log", "--format=%s", "HEAD"])
            .claim(Claim::OutputLacks("hotfix-docs")),
        Step::git(&a(), Repo::Docs, &["log", "--format=%s", "main"])
            .claim(Claim::OutputLacks("hotfix-docs")),
        Step::git(&ww("st"), Repo::Docs, &["log", "--format=%s", "HEAD"])
            .claim(Claim::OutputLacks("hotfix-docs")),
    ]);
    scenario(
        "abort restores a member moved in the target and in place",
        steps,
    )
}

/// `removal-is-member-change`: a feature removing a member its target lists
/// refuses to land until `--allow-member-change` names the member, before
/// anything lands; the consent, run as the refusal prints it, lands it, and
/// the target's checkout of the member is pruned once the members have
/// merged; then the target no longer lists it.
pub fn removal_is_member_change() -> Scenario {
    scenario(
        "removal is a member change",
        vec![
            create("w"),
            Step::rwv(&ww("w"), &["member", "remove", LIB, "--delete"]).expect(Exit::Succeeds),
            Step::commit(&ww("w"), Repo::App, "w-app"),
            Step::rwv(&ww("w"), &["land", "--allow-member-change", DOCS])
                .expect(Exit::refuses(Repo::Lib, State::TargetMemberChanges))
                .claim(Claim::OutputContains(
                    "`--allow-member-change local/remotes/docs` names no change",
                ))
                .claim(Claim::LocalStateUnchanged(a())),
            Step::rwv(&ww("w"), &["land"])
                .expect(Exit::refuses(Repo::Lib, State::TargetMemberChanges))
                .claim(Claim::OutputContains("removed"))
                .claim(Claim::OutputContains("rwv member add "))
                .claim(Claim::LocalStateUnchanged(ww("w")))
                .claim(Claim::LocalStateUnchanged(a())),
            Step::rwv_paused(
                &ww("w"),
                &["land", "--allow-member-change", LIB],
                "advance-target",
                "landing",
            )
            .claim(Claim::DirStands(Machine::A, "weave/local/remotes/lib")),
            Step::release("landing")
                .expect(Exit::Succeeds)
                .claim(Claim::DirGone(Machine::A, "weave/local/remotes/lib"))
                .claim(Claim::Contains {
                    at: a(),
                    repo: Repo::App,
                    rev: "main",
                    mark: "w-app",
                }),
            Step::git(&a(), Repo::Proj, &["show", "main:rwv.toml"])
                .claim(Claim::OutputLacks(LIB))
                .claim(Claim::OutputContains(DOCS)),
            Step::git(&a(), Repo::Proj, &["show", "main:rwv.lock"]).claim(Claim::OutputLacks(LIB)),
        ],
    )
}

/// The re-add a removal's refusal prints, run as printed, clears the
/// refusal: the feature lists the member again as the target does.
pub fn the_printed_re_add_clears_a_removal() -> Scenario {
    scenario(
        "the printed re-add clears a removal",
        vec![
            create("w"),
            Step::rwv(&ww("w"), &["member", "remove", LIB]).expect(Exit::Succeeds),
            Step::commit(&ww("w"), Repo::App, "w-app"),
            Step::rwv(&ww("w"), &["land"])
                .expect(Exit::refuses(Repo::Lib, State::TargetMemberChanges)),
            Step::rwv_printed(&ww("w"), "rwv member add ").expect(Exit::Succeeds),
            Step::rwv(&ww("w"), &["land"])
                .expect(Exit::Succeeds)
                .claim(Claim::Contains {
                    at: a(),
                    repo: Repo::App,
                    rev: "main",
                    mark: "w-app",
                }),
            Step::git(&a(), Repo::Proj, &["show", "main:rwv.toml"])
                .claim(Claim::OutputContains(LIB)),
        ],
    )
}

/// `dc06-member-set-from-merge`: `main` adds `docs` while the feature
/// removes `lib` and adds a `[workweave]` table. Landing, with `lib` named,
/// leaves `main` listing exactly the merged set, pinning exactly it, the
/// `[workweave]` table landed unnamed, `main`'s checkout of `lib` pruned once
/// the members have merged, and the relock as the last commit.
pub fn dc06_member_set_from_merge() -> Scenario {
    scenario(
        "dc06 member set from merge",
        vec![
            Step::rwv(&a(), &["member", "remove", DOCS]).expect(Exit::Succeeds),
            create("w"),
            Step::rwv(&a(), &["member", "add", "url:origin:docs"]).expect(Exit::Succeeds),
            Step::rwv(&ww("w"), &["member", "remove", LIB, "--delete"]).expect(Exit::Succeeds),
            Step::hand_edit(
                &ww("w"),
                Repo::Proj,
                "rwv.toml",
                "[integrations.vscode-workspace]\nenabled = false\n",
                "[integrations.vscode-workspace]\nenabled = false\n\n[workweave]\ncopy = \
                 [\"notes.txt\"]\n",
            ),
            Step::rwv(&ww("w"), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::commit(&ww("w"), Repo::App, "w-app"),
            Step::rwv(&ww("w"), &["land", "--history", "linear"])
                .expect(Exit::refuses(Repo::Lib, State::TargetMemberChanges))
                .claim(Claim::OutputLacks("local/remotes/docs: target"))
                .claim(Claim::LocalStateUnchanged(a())),
            Step::rwv(
                &ww("w"),
                &["land", "--history", "linear", "--allow-member-change", LIB],
            )
            .expect(Exit::refuses_kind(RefusalKind::OpParked))
            .claim(Claim::OutputContains("rwv land --continue")),
            Step::git(&ww("w"), Repo::Proj, &["checkout", "--ours", "rwv.toml"]),
            Step::drop_manifest_entry(&ww("w"), Repo::Proj, Repo::Lib),
            Step::git(&ww("w"), Repo::Proj, &["add", "rwv.toml"]),
            Step::rwv(&ww("w"), &["land", "--continue"])
                .expect(Exit::Succeeds)
                .claim(Claim::Contains {
                    at: a(),
                    repo: Repo::App,
                    rev: "main",
                    mark: "w-app",
                })
                .claim(Claim::DirGone(Machine::A, "weave/local/remotes/lib")),
            Step::git(&a(), Repo::Proj, &["show", "main:rwv.toml"])
                .claim(Claim::OutputLacks(LIB))
                .claim(Claim::OutputContains(DOCS))
                .claim(Claim::OutputContains("local/remotes/app"))
                .claim(Claim::OutputContains("copy = [\"notes.txt\"]")),
            Step::git(&a(), Repo::Proj, &["show", "main:rwv.lock"])
                .claim(Claim::OutputLacks(LIB))
                .claim(Claim::OutputContains(DOCS))
                .claim(Claim::OutputContains("local/remotes/app")),
            Step::git(&a(), Repo::Proj, &["log", "-1", "--format=%s", "main"])
                .claim(Claim::OutputContains("lock: auto-relock after sync from")),
        ],
    )
}

/// `main` adds `docs` while the feature removes `lib`; the landing, `lib`
/// named, stops on `rwv.toml`, and the resolution keeps the feature's file,
/// which drops `docs` too.
fn stop_resolved_to_the_features_manifest() -> Vec<Step> {
    vec![
        Step::rwv(&a(), &["member", "remove", DOCS]).expect(Exit::Succeeds),
        create("w"),
        Step::rwv(&a(), &["member", "add", "url:origin:docs"]).expect(Exit::Succeeds),
        Step::rwv(&ww("w"), &["member", "remove", LIB]).expect(Exit::Succeeds),
        Step::hand_edit(
            &ww("w"),
            Repo::Proj,
            "rwv.toml",
            "[integrations.vscode-workspace]\nenabled = false\n",
            "[integrations.vscode-workspace]\nenabled = false\n\n[workweave]\ncopy = \
             [\"notes.txt\"]\n",
        ),
        Step::rwv(&ww("w"), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::commit(&ww("w"), Repo::App, "w-app"),
        Step::rwv(
            &ww("w"),
            &["land", "--history", "linear", "--allow-member-change", LIB],
        )
        .expect(Exit::refuses_kind(RefusalKind::OpParked))
        .claim(Claim::OutputContains(
            "local/remotes/lib: removed (the target's entry, named by \
                 --allow-member-change)",
        )),
        Step::git(&ww("w"), Repo::Proj, &["checkout", "--theirs", "rwv.toml"]),
        Step::git(&ww("w"), Repo::Proj, &["add", "rwv.toml"]),
        Step::rwv(&ww("w"), &["land", "--continue"])
            .expect(Exit::refuses(Repo::Docs, State::TargetMemberChanges))
            .claim(Claim::OutputContains("removed"))
            .claim(Claim::OutputInOrder(&[
                "rerun with --allow-member-change local/remotes/docs",
                "rwv abort",
            ]))
            .claim(Claim::LocalStateUnchanged(a())),
    ]
}

/// A `--continue` judges `rwv.toml` as the stop's resolution leaves it: a
/// change the landing's start did not name refuses, and the consent the
/// refusal prints, given to `--continue`, lands it and reports it.
pub fn continue_rejudges_the_resolved_manifest() -> Scenario {
    let mut steps = stop_resolved_to_the_features_manifest();
    steps.extend([
        Step::rwv(
            &ww("w"),
            &[
                "land",
                "--continue",
                "--allow-member-change",
                "local/remotes/app",
            ],
        )
        .expect(Exit::refuses_kind(RefusalKind::InapplicableFlag))
        .claim(Claim::LocalStateUnchanged(a())),
        Step::rwv(
            &ww("w"),
            &["land", "--continue", "--allow-member-change", DOCS],
        )
        .expect(Exit::Succeeds)
        .may_strand(&[Repo::Proj])
        .claim(Claim::OutputContains(
            "local/remotes/lib: removed (the target's entry, named by --allow-member-change)",
        ))
        .claim(Claim::OutputContains(
            "local/remotes/docs: removed (the target's entry, named by \
             --allow-member-change)",
        ))
        .claim(Claim::Contains {
            at: a(),
            repo: Repo::App,
            rev: "main",
            mark: "w-app",
        }),
        Step::git(&a(), Repo::Proj, &["show", "main:rwv.toml"])
            .claim(Claim::OutputLacks(LIB))
            .claim(Claim::OutputLacks(DOCS)),
    ]);
    scenario("continue rejudges the resolved manifest", steps)
}

/// The other choice a `--continue` refused on its resolution prints: `rwv
/// abort` puts the target back, and a fresh `rwv land` judges the entries
/// again.
pub fn continue_refused_on_its_resolution_aborts() -> Scenario {
    let mut steps = stop_resolved_to_the_features_manifest();
    steps.extend([
        Step::rwv(&ww("w"), &["abort"])
            .expect(Exit::Succeeds)
            .may_rewind(&[Repo::App]),
        Step::rwv(&ww("w"), &["land", "--history", "linear"])
            .expect(Exit::refuses(Repo::Lib, State::TargetMemberChanges))
            .claim(Claim::LocalStateUnchanged(a())),
    ]);
    scenario("continue refused on its resolution aborts", steps)
}

/// `raw-merge-member-change`: a raw merge of `stable` into a feature onto
/// `main` changes `docs`'s declared branch. Landing it refuses until `docs`
/// is named; once landed, `status` in `main`'s workspace reports the change,
/// the pin on the new branch (C25) before the restore (C11), run after run,
/// until the push branch carries it.
pub fn raw_merge_member_change() -> Scenario {
    scenario(
        "raw merge member change",
        vec![
            Step::rwv(
                &a(),
                &[
                    "branch",
                    "create",
                    "stable",
                    "--declared",
                    "--set-new-branch",
                    "local/remotes/docs=docs-stable",
                ],
            )
            .expect(Exit::Succeeds),
            Step::rwv(&a(), &["switch", "main"]).expect(Exit::Succeeds),
            create("w"),
            Step::git(&ww("w"), Repo::Proj, &["merge", "--no-edit", "stable"]),
            Step::commit(&ww("w"), Repo::App, "w-app"),
            Step::rwv(&ww("w"), &["land"])
                .expect(Exit::refuses(Repo::Docs, State::TargetMemberChanges))
                .claim(Claim::OutputContains("branch: main → docs-stable"))
                .claim(Claim::OutputContains(
                    "rwv branch set-mode proj--w feature --target stable",
                ))
                .claim(Claim::LocalStateUnchanged(a())),
            Step::rwv(&ww("w"), &["land", "--allow-member-change", DOCS])
                .expect(Exit::Succeeds)
                .claim(Claim::Contains {
                    at: a(),
                    repo: Repo::App,
                    rev: "main",
                    mark: "w-app",
                })
                .claim(Claim::OnBranch {
                    at: a(),
                    repo: Repo::Docs,
                    branch: "docs-stable",
                }),
            Step::rwv(&a(), &["status"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains("member-changed"))
                .claim(Claim::OutputInOrder(&[
                    "rwv member pin local/remotes/docs",
                    "rwv member set-branch local/remotes/docs main",
                ])),
            Step::rwv(&a(), &["status"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains("member-changed")),
            publishes_records(Step::rwv(&a(), &["push"]))
                .pushes(Remote::origin(Repo::Proj), "main")
                .pushes(Remote::origin(Repo::App), "main")
                .pushes(Remote::origin(Repo::Docs), "docs-stable")
                .expect(Exit::Succeeds),
            Step::rwv(&a(), &["status"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputLacks("member-changed")),
        ],
    )
}

/// In a feature, `update --remote --repo` naming a `dependency` and a
/// `reference` member advances each on its declared branch, which the primary
/// has checked out, so detached at the upstream's tip; the feature's lock
/// records both and `--commit` commits it. Neither gets the feature branch.
pub fn update_remote_repins_dependency_and_reference_in_a_feature() -> Scenario {
    Scenario {
        name: "update --remote --repo re-pins a dependency and a reference in a feature",
        seed: Seed::default()
            .role(Repo::Lib, Role::Dependency)
            .role(Repo::Docs, Role::Reference),
        steps: vec![
            create("w"),
            Step::forge(Forge::Commit {
                repo: Repo::Lib,
                branch: "main",
                mark: "lib-up",
            }),
            Step::forge(Forge::Commit {
                repo: Repo::Docs,
                branch: "main",
                mark: "docs-up",
            }),
            Step::rwv(
                &ww("w"),
                &[
                    "update", "--remote", "--repo", LIB, "--repo", DOCS, "--commit",
                ],
            )
            .expect(Exit::Succeeds)
            .claim(Claim::Contains {
                at: ww("w"),
                repo: Repo::Lib,
                rev: "HEAD",
                mark: "lib-up",
            })
            .claim(Claim::Contains {
                at: ww("w"),
                repo: Repo::Docs,
                rev: "HEAD",
                mark: "docs-up",
            })
            .claim(Claim::PinContains {
                site: LockSite::Checkout(ww("w"), Repo::Proj, "proj--w".to_owned()),
                member: Repo::Lib,
                mark: "lib-up",
            })
            .claim(Claim::PinContains {
                site: LockSite::Checkout(ww("w"), Repo::Proj, "proj--w".to_owned()),
                member: Repo::Docs,
                mark: "docs-up",
            }),
            Step::git(
                &ww("w"),
                Repo::Proj,
                &["diff", "--quiet", "HEAD", "--", "rwv.lock"],
            )
            .expect(Exit::Succeeds),
            Step::git(&ww("w"), Repo::Lib, &["branch", "--list", "proj--w"])
                .claim(Claim::OutputLacks("proj--w")),
            Step::git(&ww("w"), Repo::Docs, &["branch", "--list", "proj--w"])
                .claim(Claim::OutputLacks("proj--w")),
        ],
    }
}

/// `roles-in-feature`: a feature's `dependency` and `reference` members get
/// no branch of its name; `update --remote --repo` re-pins the dependency in
/// the feature and the landing, `main` having moved meanwhile, carries that
/// pin three-way; patching it needs `member set-role <m> fork`, and landing
/// that role change needs `--allow-member-change <m>`. A pin moved backward
/// is a change too.
pub fn roles_in_feature() -> Scenario {
    Scenario {
        name: "roles in feature",
        seed: Seed::default()
            .role(Repo::Lib, Role::Dependency)
            .role(Repo::Docs, Role::Reference),
        steps: vec![
            create("w"),
            Step::git(&ww("w"), Repo::Lib, &["branch", "--list", "proj--w"])
                .claim(Claim::OutputLacks("proj--w")),
            Step::git(&ww("w"), Repo::Docs, &["branch", "--list", "proj--w"])
                .claim(Claim::OutputLacks("proj--w")),
            Step::forge(Forge::Commit {
                repo: Repo::Lib,
                branch: "main",
                mark: "lib-up",
            }),
            Step::rwv(&ww("w"), &["update", "--remote", "--repo", LIB, "--commit"])
                .expect(Exit::Succeeds)
                .claim(Claim::PinContains {
                    site: LockSite::Checkout(ww("w"), Repo::Proj, "proj--w".to_owned()),
                    member: Repo::Lib,
                    mark: "lib-up",
                }),
            Step::commit(&ww("w"), Repo::App, "w-app"),
            Step::commit(&a(), Repo::App, "main-app-1"),
            Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::rwv(&ww("w"), &["land", "--history", "linear", "--retire"])
                .expect(Exit::Succeeds)
                .claim(Claim::PinContains {
                    site: LockSite::Checkout(a(), Repo::Proj, "main".to_owned()),
                    member: Repo::Lib,
                    mark: "lib-up",
                })
                .claim(Claim::PinContains {
                    site: LockSite::Checkout(a(), Repo::Proj, "main".to_owned()),
                    member: Repo::App,
                    mark: "main-app-1",
                }),
            create("back"),
            Step::rwv(&ww("back"), &["member", "pin", LIB, "HEAD~1", "--commit"])
                .expect(Exit::Succeeds)
                .may_rewind(&[Repo::Lib]),
            Step::commit(&a(), Repo::App, "main-app-2"),
            Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::rwv(&ww("back"), &["land", "--history", "linear"])
                .expect(Exit::refuses(Repo::Lib, State::TargetMemberChanges))
                .claim(Claim::OutputContains("pin: "))
                .claim(Claim::OutputContains("rwv member pin local/remotes/lib "))
                .claim(Claim::LocalStateUnchanged(a())),
            Step::rwv(
                &ww("back"),
                &[
                    "land",
                    "--history",
                    "linear",
                    "--retire",
                    "--allow-member-change",
                    LIB,
                ],
            )
            .expect(Exit::Succeeds)
            .may_rewind(&[Repo::Lib])
            .claim(Claim::OutputContains("re-pinned")),
            Step::git(&a(), Repo::Proj, &["log", "-1", "--format=%B", "main"])
                .claim(Claim::OutputContains("Rwv-Repin: \"local/remotes/lib\"")),
            create("patch"),
            Step::rwv(&ww("patch"), &["member", "set-role", LIB, "fork"]).expect(Exit::Succeeds),
            Step::rwv(&ww("patch"), &["update"])
                .expect(Exit::Succeeds)
                .claim(Claim::OnBranch {
                    at: ww("patch"),
                    repo: Repo::Lib,
                    branch: "proj--patch",
                }),
            Step::commit(&ww("patch"), Repo::Lib, "lib-patch"),
            Step::rwv(&ww("patch"), &["land", "--history", "linear"])
                .expect(Exit::refuses(Repo::Lib, State::TargetMemberChanges))
                .claim(Claim::OutputContains("role: dependency → fork"))
                .claim(Claim::OutputContains(
                    "rwv member set-role local/remotes/lib dependency",
                ))
                .claim(Claim::LocalStateUnchanged(a())),
            Step::rwv(
                &ww("patch"),
                &[
                    "land",
                    "--history",
                    "linear",
                    "--retire",
                    "--allow-member-change",
                    LIB,
                ],
            )
            .expect(Exit::Succeeds),
            Step::git(&a(), Repo::Lib, &["show", "main:lib-patch.txt"])
                .claim(Claim::OutputContains("lib-patch")),
            Step::git(&a(), Repo::Proj, &["show", "main:rwv.toml"])
                .claim(Claim::OutputContains("role = \"fork\"")),
        ],
    }
}

/// A `dependency` member's pin a feature changed lands only where its
/// receiving branch reaches it, and only where the target, having changed it
/// too, pins a commit one of the two contains.
pub fn carried_pin_judged_against_the_receiving_branch() -> Scenario {
    Scenario {
        name: "carried pin judged against the receiving branch",
        seed: Seed::default().role(Repo::Lib, Role::Dependency),
        steps: vec![
            create("w"),
            Step::forge(Forge::Commit {
                repo: Repo::Lib,
                branch: "side",
                mark: "lib-side",
            }),
            Step::rwv(&ww("w"), &["fetch"]).expect(Exit::Succeeds),
            Step::rwv(&ww("w"), &["member", "pin", LIB, "origin/side", "--commit"])
                .expect(Exit::Succeeds),
            Step::commit(&ww("w"), Repo::App, "w-app"),
            Step::rwv(&ww("w"), &["land"])
                .expect(Exit::refuses(Repo::Lib, State::PinNotOnDeclaredBranch))
                .claim(Claim::LocalStateUnchanged(a()))
                .claim(Claim::LocalStateUnchanged(ww("w"))),
            Step::forge(Forge::Commit {
                repo: Repo::Lib,
                branch: "main",
                mark: "lib-up",
            }),
            Step::rwv(&a(), &["fetch"]).expect(Exit::Succeeds),
            Step::rwv(&a(), &["member", "pin", LIB, "origin/main", "--commit"])
                .expect(Exit::Succeeds),
            Step::rwv(&ww("w"), &["land"])
                .expect(Exit::refuses(Repo::Lib, State::PinUnmerged))
                .claim(Claim::LocalStateUnchanged(a()))
                .claim(Claim::LocalStateUnchanged(ww("w"))),
        ],
    }
}

/// A receiving branch that exists nowhere is created by the landing, at the
/// target's pin, and the member's work lands on it; a member the target lists
/// without a pin is composed and pinned at its landed tip.
pub fn landing_creates_a_receiving_branch_that_exists_nowhere() -> Scenario {
    scenario(
        "landing creates a receiving branch that exists nowhere",
        vec![
            create("w"),
            Step::hand_edit(
                &ww("w"),
                Repo::Proj,
                "rwv.toml",
                &declared_branch_text(Repo::Lib, "main"),
                &declared_branch_text(Repo::Lib, "lib-next"),
            ),
            Step::git(
                &ww("w"),
                Repo::Proj,
                &["commit", "-qam", "lib onto lib-next"],
            ),
            Step::commit(&ww("w"), Repo::Lib, "w-lib"),
            Step::drop_lock_entry(&a(), Repo::Proj, Repo::Docs),
            Step::git(&a(), Repo::Proj, &["commit", "-qam", "docs unpinned"]),
            Step::commit(&a(), Repo::Docs, "main-docs"),
            Step::commit(&ww("w"), Repo::Docs, "w-docs"),
            Step::rwv(
                &ww("w"),
                &["land", "--history", "linear", "--allow-member-change", LIB],
            )
            .expect(Exit::Succeeds)
            .claim(Claim::OutputContains("created 'lib-next'"))
            .claim(Claim::Contains {
                at: a(),
                repo: Repo::Lib,
                rev: "lib-next",
                mark: "w-lib",
            })
            .claim(Claim::OnBranch {
                at: a(),
                repo: Repo::Lib,
                branch: "lib-next",
            })
            .claim(Claim::PinContains {
                site: LockSite::Checkout(a(), Repo::Proj, "main".to_owned()),
                member: Repo::Docs,
                mark: "main-docs",
            }),
            Step::git(&a(), Repo::Docs, &["show", "main:w-docs.txt"])
                .claim(Claim::OutputContains("w-docs")),
        ],
    )
}

/// A landing interrupted once it has created a receiving branch that existed
/// nowhere, before it moves it, leaves its op record; `rwv abort` deletes the
/// branch it created, and the target stays where it was.
pub fn abort_deletes_a_created_receiving_branch() -> Scenario {
    scenario(
        "abort deletes a created receiving branch",
        vec![
            create("w"),
            Step::hand_edit(
                &ww("w"),
                Repo::Proj,
                "rwv.toml",
                &declared_branch_text(Repo::Lib, "main"),
                &declared_branch_text(Repo::Lib, "lib-next"),
            ),
            Step::git(
                &ww("w"),
                Repo::Proj,
                &["commit", "-qam", "lib onto lib-next"],
            ),
            Step::commit(&ww("w"), Repo::Lib, "w-lib"),
            Step::rwv_paused(
                &ww("w"),
                &["land", "--allow-member-change", LIB],
                "advance-target",
                "landing",
            ),
            Step::kill("landing"),
            Step::rwv(&ww("w"), &["abort"])
                .expect(Exit::Succeeds)
                .may_rewind(&[Repo::Lib])
                .claim(Claim::OnBranch {
                    at: a(),
                    repo: Repo::Lib,
                    branch: "main",
                }),
            Step::git(&a(), Repo::Lib, &["branch", "--list", "lib-next"])
                .claim(Claim::OutputLacks("lib-next")),
        ],
    )
}

/// A landing interrupted once it has created a receiving branch that existed
/// nowhere, then continued under `--json`: the resumed envelope reports the
/// branch the stopped run created.
pub fn continue_reports_a_created_receiving_branch() -> Scenario {
    scenario(
        "continue reports a created receiving branch",
        vec![
            create("w"),
            Step::hand_edit(
                &ww("w"),
                Repo::Proj,
                "rwv.toml",
                &declared_branch_text(Repo::Lib, "main"),
                &declared_branch_text(Repo::Lib, "lib-next"),
            ),
            Step::git(
                &ww("w"),
                Repo::Proj,
                &["commit", "-qam", "lib onto lib-next"],
            ),
            Step::commit(&ww("w"), Repo::Lib, "w-lib"),
            Step::rwv_paused(
                &ww("w"),
                &["land", "--allow-member-change", LIB],
                "advance-target",
                "landing",
            ),
            Step::kill("landing"),
            Step::rwv(&ww("w"), &["land", "--continue", "--json"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputInOrder(&[
                    "\"reports\": [",
                    "\"kind\": \"created\"",
                    "\"path\": \"local/remotes/lib\"",
                    "\"branch\": \"lib-next\"",
                ]))
                .claim(Claim::Contains {
                    at: a(),
                    repo: Repo::Lib,
                    rev: "lib-next",
                    mark: "w-lib",
                }),
        ],
    )
}

fn main_lock() -> LockSite {
    LockSite::Checkout(a(), Repo::Proj, "main".to_owned())
}

/// A record whose target exists nowhere refuses as the target gone, with the
/// source's own refusals in the same report, at `land` and at `sync`; the
/// sole candidate is bound into the rebind, and the landing then goes there.
pub fn land_into_a_target_that_exists_nowhere() -> Scenario {
    scenario(
        "land into a target that exists nowhere",
        vec![
            Step::git(&a(), Repo::Proj, &["branch", "stable"]),
            Step::rwv(&a(), &["branch", "set-mode", "stable", "declared"]).expect(Exit::Succeeds),
            create("w"),
            Step::commit(&ww("w"), Repo::App, "w-app"),
            Step::rwv(
                &ww("w"),
                &[
                    "branch", "set-mode", "proj--w", "feature", "--target", "stable",
                ],
            )
            .expect(Exit::Succeeds),
            Step::git(&a(), Repo::Proj, &["branch", "-D", "stable"]),
            Step::hand_edit(&ww("w"), Repo::Lib, "README", "lib", "lib edited"),
            Step::rwv(&ww("w"), &["land"])
                .expect(Exit::refuses(Repo::Proj, State::TargetGone))
                .claim(Claim::Judged(Repo::Lib, State::EditsDirty))
                .claim(Claim::OutputContains(
                    "rwv branch set-mode proj--w feature --target main",
                ))
                .claim(Claim::LocalStateUnchanged(ww("w"))),
            Step::rwv(&ww("w"), &["sync"])
                .expect(Exit::refuses(Repo::Proj, State::TargetGone))
                .claim(Claim::OutputContains(
                    "rwv branch set-mode proj--w feature --target main",
                ))
                .claim(Claim::LocalStateUnchanged(ww("w"))),
            Step::rwv(
                &ww("w"),
                &[
                    "branch", "set-mode", "proj--w", "feature", "--target", "main",
                ],
            )
            .expect(Exit::Succeeds),
            Step::hand_edit(&ww("w"), Repo::Lib, "README", "lib edited", "lib"),
            Step::rwv(&ww("w"), &["land", "--retire"])
                .expect(Exit::Succeeds)
                .claim(Claim::Contains {
                    at: a(),
                    repo: Repo::App,
                    rev: "main",
                    mark: "w-app",
                }),
        ],
    )
}

/// The target-gone rebind lists every branch the feature could land into and
/// binds none of them where there are several, so a sibling sharing as much
/// history as the trunk is never chosen for the operator; a branch landing
/// into the feature is no candidate, since the rebind would close a cycle.
/// The operator's choice from the list then lands.
pub fn land_target_gone_lists_only_targets_the_rebind_takes() -> Scenario {
    scenario(
        "land target gone lists only targets the rebind takes",
        vec![
            Step::git(&a(), Repo::Proj, &["branch", "stable"]),
            Step::rwv(&a(), &["branch", "set-mode", "stable", "declared"]).expect(Exit::Succeeds),
            Step::git(&a(), Repo::Proj, &["branch", "fa"]),
            Step::rwv(
                &a(),
                &["branch", "set-mode", "fa", "feature", "--target", "main"],
            )
            .expect(Exit::Succeeds),
            create("w"),
            Step::commit(&ww("w"), Repo::App, "w-app"),
            Step::rwv(
                &ww("w"),
                &[
                    "branch", "set-mode", "proj--w", "feature", "--target", "stable",
                ],
            )
            .expect(Exit::Succeeds),
            Step::rwv(&ww("w"), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::rwv(&ww("w"), &["workweave", "create", "--project", "proj", "x"])
                .expect(Exit::Succeeds),
            Step::git(&a(), Repo::Proj, &["branch", "-D", "stable"]),
            Step::rwv(&ww("w"), &["land"])
                .expect(Exit::refuses(Repo::Proj, State::TargetGone))
                .claim(Claim::OutputContains(
                    "--target candidates, nearest first: fa, main",
                ))
                .claim(Claim::OutputContains(
                    "rwv branch set-mode proj--w feature --target <T>",
                ))
                .claim(Claim::OutputLacks("--target fa"))
                .claim(Claim::OutputLacks("--target proj--x"))
                .claim(Claim::LocalStateUnchanged(ww("w"))),
            Step::rwv(
                &ww("w"),
                &[
                    "branch", "set-mode", "proj--w", "feature", "--target", "main",
                ],
            )
            .expect(Exit::Succeeds),
            Step::rwv(&a(), &["workweave", "delete", "--project", "proj", "x"])
                .expect(Exit::Succeeds),
            Step::rwv(&ww("w"), &["land", "--retire"])
                .expect(Exit::Succeeds)
                .claim(Claim::Contains {
                    at: a(),
                    repo: Repo::App,
                    rev: "main",
                    mark: "w-app",
                }),
        ],
    )
}

/// Where the target is checked out, a landing judges edits only in the repos
/// it moves: an edit in a member it composes refuses, one in a member it
/// leaves where it is stays as it is.
pub fn land_judges_target_edits_only_where_it_moves() -> Scenario {
    scenario(
        "land judges target edits only where it moves",
        vec![
            create("w"),
            Step::commit(&ww("w"), Repo::App, "w-app"),
            Step::hand_edit(&a(), Repo::App, "README", "app", "app edited"),
            Step::rwv(&ww("w"), &["land"])
                .expect(Exit::refuses(Repo::App, State::EditsDirty))
                .claim(Claim::LocalStateUnchanged(a())),
            Step::hand_edit(&a(), Repo::App, "README", "app edited", "app"),
            Step::hand_edit(&a(), Repo::Lib, "README", "lib", "lib edited"),
            Step::rwv(&ww("w"), &["land", "--retire"])
                .expect(Exit::Succeeds)
                .claim(Claim::Contains {
                    at: a(),
                    repo: Repo::App,
                    rev: "main",
                    mark: "w-app",
                }),
            Step::git(&a(), Repo::Lib, &["diff", "--name-only"]).claim(Claim::OutputLine("README")),
        ],
    )
}

/// `main` pins `lib` at a commit its `main` does not reach, held only by the
/// primary's detached checkout: `lib`'s `main` was moved back under the pin.
fn main_pins_lib_off_its_branch() -> Vec<Step> {
    vec![
        create("w"),
        Step::commit(&a(), Repo::Lib, "a-lib"),
        Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::git(&a(), Repo::Lib, &["switch", "--detach"]),
        Step::git(&a(), Repo::Lib, &["branch", "-f", "main", "HEAD~1"])
            .may_rewind(&[Repo::Lib])
            .may_rewrite(&[Repo::Lib]),
    ]
}

/// A member the landing does not move keeps the target's pin, whether or not
/// its declared branch reaches that pin: `pin` not on declared branch judges
/// moved members only.
pub fn land_judges_the_pin_on_its_branch_only_for_moved_members() -> Scenario {
    let mut steps = main_pins_lib_off_its_branch();
    steps.extend([
        Step::commit(&ww("w"), Repo::App, "w-app"),
        Step::rwv(&ww("w"), &["land", "--history", "linear", "--retire"])
            .expect(Exit::Succeeds)
            .claim(Claim::PinContains {
                site: main_lock(),
                member: Repo::Lib,
                mark: "a-lib",
            })
            .claim(Claim::PinContains {
                site: main_lock(),
                member: Repo::App,
                mark: "w-app",
            }),
    ]);
    scenario(
        "land judges the pin on its branch only for moved members",
        steps,
    )
}

/// A moved member whose receiving branch does not reach the target's pin
/// refuses the landing; re-pinning it on its declared branch where the target
/// is checked out, as printed, lets the landing go.
pub fn land_refuses_a_moved_member_whose_branch_lacks_the_pin() -> Scenario {
    let mut steps = main_pins_lib_off_its_branch();
    steps.extend([
        Step::commit(&ww("w"), Repo::Lib, "w-lib"),
        Step::rwv(&ww("w"), &["land"])
            .expect(Exit::refuses(Repo::Lib, State::PinNotOnDeclaredBranch))
            .claim(Claim::OutputContains(
                "rwv member pin local/remotes/lib <rev> --commit",
            ))
            .claim(Claim::LocalStateUnchanged(a())),
        Step::rwv(
            &a(),
            &["member", "pin", "local/remotes/lib", "main", "--commit"],
        )
        .expect(Exit::Succeeds)
        .may_rewind(&[Repo::Lib]),
        Step::rwv(&ww("w"), &["land", "--history", "linear", "--retire"])
            .expect(Exit::Succeeds)
            .claim(Claim::PinContains {
                site: main_lock(),
                member: Repo::Lib,
                mark: "w-lib",
            }),
    ]);
    scenario(
        "land refuses a moved member whose branch lacks the pin",
        steps,
    )
}

/// The receiving branch of a moved member holds commits beyond the target's
/// pin: the landing lists them as a fact and pins them with the work. A member
/// it does not move keeps the target's pin though its branch moved on, so a
/// landing that composes nothing (`--history ff-only`, all it lands fast-forwarding) lands the rest.
pub fn land_reports_what_it_pins_beyond_the_target() -> Scenario {
    scenario(
        "land reports what it pins beyond the target",
        vec![
            create("w"),
            Step::commit(&ww("w"), Repo::App, "w-app"),
            Step::forge(Forge::Commit {
                repo: Repo::Lib,
                branch: "main",
                mark: "forge-lib",
            }),
            Step::rwv(&a(), &["fetch"]).expect(Exit::Succeeds),
            Step::rwv(&ww("w"), &["land", "--history", "ff-only"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputLacks("local/remotes/lib: ff-advanced"))
                .claim(Claim::PinContains {
                    site: main_lock(),
                    member: Repo::App,
                    mark: "w-app",
                }),
            Step::commit(&ww("w"), Repo::Lib, "w-lib"),
            Step::rwv(&ww("w"), &["land", "--history", "linear", "--retire"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains(
                    "local/remotes/lib: the branch it lands on holds 1 commit(s) beyond 'main''s \
                     pin",
                ))
                .claim(Claim::OutputContains("forge-lib"))
                .claim(Claim::PinContains {
                    site: main_lock(),
                    member: Repo::Lib,
                    mark: "forge-lib",
                }),
        ],
    )
}

/// Two forward bumps of a `dependency`, one in the feature and a later one
/// on the target, land unnamed: the merge keeps the target's pin, which
/// contains the feature's, so nothing moves backward.
pub fn forward_bumps_on_both_sides_land_unnamed() -> Scenario {
    Scenario {
        name: "forward bumps on both sides land unnamed",
        seed: Seed::default().role(Repo::Lib, Role::Dependency),
        steps: vec![
            create("w"),
            Step::forge(Forge::Commit {
                repo: Repo::Lib,
                branch: "main",
                mark: "lib-b4",
            }),
            Step::rwv(&ww("w"), &["fetch"]).expect(Exit::Succeeds),
            Step::rwv(&ww("w"), &["member", "pin", LIB, "origin/main", "--commit"])
                .expect(Exit::Succeeds),
            Step::forge(Forge::Commit {
                repo: Repo::Lib,
                branch: "main",
                mark: "lib-b5",
            }),
            Step::rwv(&a(), &["fetch"]).expect(Exit::Succeeds),
            Step::rwv(&a(), &["member", "pin", LIB, "origin/main", "--commit"])
                .expect(Exit::Succeeds),
            Step::commit(&ww("w"), Repo::App, "w-app"),
            Step::rwv(&ww("w"), &["land", "--history", "linear"])
                .expect(Exit::Succeeds)
                .claim(Claim::PinContains {
                    site: LockSite::Checkout(a(), Repo::Proj, "main".to_owned()),
                    member: Repo::Lib,
                    mark: "lib-b5",
                }),
        ],
    }
}

/// A backward pin offers no rebind onto a live branch whose entries match
/// but whose pin the landing would move backward too; the pin it does print,
/// run as printed, restores the target's pin and the landing passes unnamed.
pub fn a_backward_pin_offers_only_a_rebind_that_lands_it_unchanged() -> Scenario {
    Scenario {
        name: "a backward pin offers only a rebind that lands it unchanged",
        seed: Seed::default().role(Repo::Lib, Role::Dependency),
        steps: vec![
            Step::forge(Forge::Commit {
                repo: Repo::Lib,
                branch: "main",
                mark: "lib-up",
            }),
            Step::rwv(&a(), &["fetch"]).expect(Exit::Succeeds),
            Step::rwv(&a(), &["member", "pin", LIB, "origin/main", "--commit"])
                .expect(Exit::Succeeds),
            Step::rwv(&a(), &["branch", "create", "stable", "--declared"]).expect(Exit::Succeeds),
            Step::rwv(&a(), &["switch", "main"]).expect(Exit::Succeeds),
            Step::forge(Forge::Commit {
                repo: Repo::Lib,
                branch: "main",
                mark: "lib-up-2",
            }),
            Step::rwv(&a(), &["fetch"]).expect(Exit::Succeeds),
            Step::rwv(&a(), &["member", "pin", LIB, "origin/main", "--commit"])
                .expect(Exit::Succeeds),
            create("back"),
            Step::rwv(&ww("back"), &["member", "pin", LIB, "HEAD~2", "--commit"])
                .expect(Exit::Succeeds)
                .may_rewind(&[Repo::Lib]),
            Step::commit(&ww("back"), Repo::App, "back-app"),
            Step::rwv(&ww("back"), &["land"])
                .expect(Exit::refuses(Repo::Lib, State::TargetMemberChanges))
                .claim(Claim::OutputLacks("--target stable"))
                .claim(Claim::OutputContains("rwv member pin local/remotes/lib "))
                .claim(Claim::LocalStateUnchanged(a())),
            Step::rwv_printed(&ww("back"), "rwv member pin local/remotes/lib ")
                .expect(Exit::Succeeds),
            Step::rwv(&ww("back"), &["land"])
                .expect(Exit::Succeeds)
                .claim(Claim::PinContains {
                    site: LockSite::Checkout(a(), Repo::Proj, "main".to_owned()),
                    member: Repo::Lib,
                    mark: "lib-up-2",
                }),
        ],
    }
}

/// A feature's pin of a `dependency` that contains the target's newer pin
/// but sits off the receiving branch is refused, though the target changed
/// the pin too: the landing would take the feature's pin, which `main` of the
/// member does not reach. The pin it prints, run as printed, clears it.
pub fn a_containing_pin_off_the_receiving_branch_is_refused() -> Scenario {
    Scenario {
        name: "a containing pin off the receiving branch is refused",
        seed: Seed::default().role(Repo::Lib, Role::Dependency),
        steps: vec![
            create("w"),
            Step::forge(Forge::Commit {
                repo: Repo::Lib,
                branch: "main",
                mark: "lib-b5",
            }),
            Step::forge(Forge::Commit {
                repo: Repo::Lib,
                branch: "side",
                mark: "lib-side",
            }),
            Step::rwv(&a(), &["fetch"]).expect(Exit::Succeeds),
            Step::rwv(&a(), &["member", "pin", LIB, "origin/main", "--commit"])
                .expect(Exit::Succeeds),
            Step::rwv(&ww("w"), &["fetch"]).expect(Exit::Succeeds),
            Step::rwv(&ww("w"), &["member", "pin", LIB, "origin/side", "--commit"])
                .expect(Exit::Succeeds),
            Step::commit(&ww("w"), Repo::App, "w-app"),
            Step::rwv(&ww("w"), &["land"])
                .expect(Exit::refuses(Repo::Lib, State::PinNotOnDeclaredBranch))
                .claim(Claim::OutputLacks("cherry-pick"))
                .claim(Claim::OutputLacks("rwv member set-branch"))
                .claim(Claim::LocalStateUnchanged(a()))
                .claim(Claim::LocalStateUnchanged(ww("w"))),
            Step::rwv_printed(&ww("w"), "rwv member pin local/remotes/lib ")
                .expect(Exit::Succeeds)
                .may_rewind(&[Repo::Lib]),
            Step::rwv(&ww("w"), &["land", "--history", "linear"])
                .expect(Exit::Succeeds)
                .claim(Claim::PinContains {
                    site: LockSite::Checkout(a(), Repo::Proj, "main".to_owned()),
                    member: Repo::Lib,
                    mark: "lib-b5",
                }),
        ],
    }
}

/// A `dependency` the feature declares on another branch, which the target
/// re-pinned meanwhile, lands by name at the feature's pin: the two sides
/// declare it differently, so the target's pin is not unmerged, and the
/// commits it added that the kept branch lacks are listed with the copy.
pub fn a_dependency_declared_differently_lands_and_lists_what_it_leaves() -> Scenario {
    Scenario {
        name: "a dependency declared differently lands and lists what it leaves",
        seed: Seed::default().role(Repo::Lib, Role::Dependency),
        steps: vec![
            create("w"),
            Step::forge(Forge::Commit {
                repo: Repo::Lib,
                branch: "stable-lib",
                mark: "lib-stable",
            }),
            Step::forge(Forge::Commit {
                repo: Repo::Lib,
                branch: "main",
                mark: "lib-b5",
            }),
            Step::rwv(&a(), &["fetch"]).expect(Exit::Succeeds),
            Step::rwv(&a(), &["member", "pin", LIB, "origin/main", "--commit"])
                .expect(Exit::Succeeds),
            Step::rwv(&ww("w"), &["fetch"]).expect(Exit::Succeeds),
            Step::rwv(&ww("w"), &["member", "set-branch", LIB, "stable-lib"])
                .expect(Exit::Succeeds),
            Step::commit(&ww("w"), Repo::App, "w-app"),
            Step::rwv(
                &ww("w"),
                &["land", "--history", "linear", "--allow-member-change", LIB],
            )
            .expect(Exit::Succeeds)
            .may_rewind(&[Repo::Lib])
            .claim(Claim::OutputInOrder(&[
                "branch main -> stable-lib",
                "lib-b5",
                "copy these commits onto `stable-lib`",
            ]))
            .claim(Claim::PinContains {
                site: LockSite::Checkout(a(), Repo::Proj, "main".to_owned()),
                member: Repo::Lib,
                mark: "lib-stable",
            }),
        ],
    }
}

/// Where the merge base's `rwv.toml` does not read under the schema, a
/// removal, and a top-level history the feature unset, are still changes
/// the landing names: the safe side.
pub fn a_removal_over_an_unreadable_merge_base_is_named() -> Scenario {
    scenario(
        "a removal over an unreadable merge base is named",
        vec![
            Step::rwv(&a(), &["branch", "set-history", "merge"]).expect(Exit::Succeeds),
            create("w"),
            Step::hand_edit(
                &a(),
                Repo::Proj,
                "rwv.toml",
                "[integrations.vscode-workspace]\n",
                "unreadable = true\n\n[integrations.vscode-workspace]\n",
            ),
            Step::git(&a(), Repo::Proj, &["commit", "-qam", "unreadable"]),
            Step::git(&ww("w"), Repo::Proj, &["merge", "-q", "--no-edit", "main"]),
            Step::hand_edit(&a(), Repo::Proj, "rwv.toml", "unreadable = true\n\n", ""),
            Step::git(&a(), Repo::Proj, &["commit", "-qam", "readable again"]),
            Step::hand_edit(
                &ww("w"),
                Repo::Proj,
                "rwv.toml",
                "unreadable = true\n\n",
                "",
            ),
            Step::git(
                &ww("w"),
                Repo::Proj,
                &["commit", "-qam", "readable again here"],
            ),
            Step::rwv(&ww("w"), &["member", "remove", LIB]).expect(Exit::Succeeds),
            Step::rwv(&ww("w"), &["branch", "set-history", "--unset"]).expect(Exit::Succeeds),
            Step::rwv(&ww("w"), &["land"])
                .expect(Exit::refuses(Repo::Lib, State::TargetMemberChanges))
                .claim(Claim::Judged(Repo::Proj, State::TargetMemberChanges))
                .claim(Claim::OutputContains("removed"))
                .claim(Claim::LocalStateUnchanged(a())),
        ],
    )
}

/// The refusal lists only the fields the landing changes: one the target
/// changed alone is the target's, kept by the merge, and not listed.
pub fn member_changes_are_judged_per_field() -> Scenario {
    scenario(
        "member changes are judged per field",
        vec![
            create("w"),
            Step::rwv(&a(), &["member", "set-recurse-submodules", LIB, "false"])
                .expect(Exit::Succeeds),
            Step::rwv(&ww("w"), &["member", "set-branch", LIB, "dev"]).expect(Exit::Succeeds),
            Step::rwv(&ww("w"), &["land"])
                .expect(Exit::refuses(Repo::Lib, State::TargetMemberChanges))
                .claim(Claim::OutputContains("branch: main → dev"))
                .claim(Claim::OutputLacks("recurse-submodules:"))
                .claim(Claim::LocalStateUnchanged(a())),
        ],
    )
}

/// A consent naming no change joins the landing's one report: the dirt
/// judged beside it is reported on the same run.
pub fn an_inapplicable_consent_joins_the_report() -> Scenario {
    scenario(
        "an inapplicable consent joins the report",
        vec![
            create("w"),
            Step::commit(&ww("w"), Repo::App, "w-app"),
            Step::hand_edit(&ww("w"), Repo::App, "w-app.txt", "w-app", "edited"),
            Step::rwv(&ww("w"), &["land", "--allow-member-change", DOCS])
                .expect(Exit::refuses(Repo::App, State::EditsDirty))
                .claim(Claim::OutputContains(
                    "`--allow-member-change local/remotes/docs` names no change",
                ))
                .claim(Claim::LocalStateUnchanged(a())),
        ],
    )
}

/// The member-change steps every field change of `landing-names-every-field`
/// starts from: a seat that changes `lib`'s role, `app`'s url and history,
/// `docs`'s `recurse-submodules` and the top-level history, and adds a
/// `[workweave]` table.
fn every_field_changed() -> Vec<Step> {
    vec![
        create("w"),
        Step::rwv(&ww("w"), &["member", "set-role", LIB, "fork"]).expect(Exit::Succeeds),
        Step::rwv(
            &ww("w"),
            &[
                "branch",
                "set-history",
                "merge",
                "--repo",
                "local/remotes/app",
            ],
        )
        .expect(Exit::Succeeds),
        Step::rwv(
            &ww("w"),
            &[
                "member",
                "set-url",
                "local/remotes/app",
                "url:mirror-keeping:app",
            ],
        )
        .expect(Exit::Succeeds),
        Step::rwv(
            &ww("w"),
            &["member", "set-recurse-submodules", DOCS, "false"],
        )
        .expect(Exit::Succeeds),
        Step::rwv(&ww("w"), &["branch", "set-history", "merge"]).expect(Exit::Succeeds),
        Step::hand_edit(
            &ww("w"),
            Repo::Proj,
            "rwv.toml",
            "[integrations.vscode-workspace]\nenabled = false\n",
            "[integrations.vscode-workspace]\nenabled = false\n\n[workweave]\ncopy = \
             [\"notes.txt\"]\n",
        ),
        Step::rwv(&ww("w"), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::commit(&ww("w"), Repo::App, "w-app"),
    ]
}

/// `landing-names-every-field`: a seat changing `role`, `url`, `history`,
/// `recurse-submodules` and the top-level `history` is refused, listing each
/// field with both values, until every repo is named, the top-level setting
/// as `projects/proj`; the one consent printed, run as printed, lands them,
/// and the `[workweave]` table lands unnamed.
pub fn landing_names_every_field() -> Scenario {
    let mut steps = every_field_changed();
    steps.extend([
        Step::rwv(&ww("w"), &["land"])
            .expect(Exit::refuses(Repo::Lib, State::TargetMemberChanges))
            .claim(Claim::OutputContains("role: owned → fork"))
            .claim(Claim::OutputContains("history: unset → merge"))
            .claim(Claim::OutputContains("url: "))
            .claim(Claim::OutputContains("recurse-submodules: true → false"))
            .claim(Claim::OutputContains("'main''s top-level history"))
            .claim(Claim::LocalStateUnchanged(a())),
        Step::rwv(
            &ww("w"),
            &[
                "land",
                "--allow-member-change",
                LIB,
                "--allow-member-change",
                "local/remotes/app",
                "--allow-member-change",
                DOCS,
            ],
        )
        .expect(Exit::refuses(Repo::Proj, State::TargetMemberChanges))
        .claim(Claim::OutputContains(
            "rwv land --allow-member-change local/remotes/app --allow-member-change \
             local/remotes/docs --allow-member-change local/remotes/lib --allow-member-change \
             projects/proj",
        ))
        .claim(Claim::LocalStateUnchanged(a())),
        Step::rwv_printed(&ww("w"), "rwv land --allow-member-change ")
            .expect(Exit::Succeeds)
            .claim(Claim::Contains {
                at: a(),
                repo: Repo::App,
                rev: "main",
                mark: "w-app",
            }),
        Step::git(&a(), Repo::Proj, &["show", "main:rwv.toml"])
            .claim(Claim::OutputContains("role = \"fork\""))
            .claim(Claim::OutputContains("recurse-submodules = false"))
            .claim(Claim::OutputContains("copy = [\"notes.txt\"]")),
    ]);
    scenario("landing names every field", steps)
}

/// Each setter a member-change refusal prints, run as printed, restores the
/// target's value, until the landing passes with nothing named and the
/// `[workweave]` table lands as content.
pub fn every_printed_setter_restores_the_target_value() -> Scenario {
    let mut steps = every_field_changed();
    for (repo, prefix) in [
        (Repo::Lib, "rwv member set-role local/remotes/lib "),
        (
            Repo::App,
            "rwv branch set-history --unset --repo local/remotes/app",
        ),
        (Repo::App, "rwv member set-url local/remotes/app "),
        (
            Repo::Docs,
            "rwv member set-recurse-submodules local/remotes/docs ",
        ),
        (Repo::Proj, "rwv branch set-history --unset"),
    ] {
        steps.push(
            Step::rwv(&ww("w"), &["land"])
                .expect(Exit::refuses(repo, State::TargetMemberChanges))
                .claim(Claim::LocalStateUnchanged(a())),
        );
        steps.push(Step::rwv_printed(&ww("w"), prefix).expect(Exit::Succeeds));
    }
    steps.extend([
        Step::rwv(&ww("w"), &["land"])
            .expect(Exit::Succeeds)
            .claim(Claim::Contains {
                at: a(),
                repo: Repo::App,
                rev: "main",
                mark: "w-app",
            }),
        Step::git(&a(), Repo::Proj, &["show", "main:rwv.toml"])
            .claim(Claim::OutputLacks("role = \"fork\""))
            .claim(Claim::OutputContains("copy = [\"notes.txt\"]")),
    ]);
    scenario("every printed setter restores the target value", steps)
}

/// A member holding no new work whose new declared branch exists nowhere
/// follows it, and the landing creates it, at the target's pin.
pub fn a_followed_receiving_branch_that_exists_nowhere_is_created() -> Scenario {
    scenario(
        "a followed receiving branch that exists nowhere is created",
        vec![
            create("w"),
            Step::hand_edit(
                &ww("w"),
                Repo::Proj,
                "rwv.toml",
                &declared_branch_text(Repo::Lib, "main"),
                &declared_branch_text(Repo::Lib, "lib-next"),
            ),
            Step::git(
                &ww("w"),
                Repo::Proj,
                &["commit", "-qam", "lib onto lib-next"],
            ),
            Step::commit(&ww("w"), Repo::App, "w-app"),
            Step::rwv(&ww("w"), &["land", "--allow-member-change", LIB])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains("created 'lib-next'"))
                .claim(Claim::OnBranch {
                    at: a(),
                    repo: Repo::Lib,
                    branch: "lib-next",
                })
                .claim(Claim::AtPin {
                    at: a(),
                    repo: Repo::Lib,
                }),
            Step::git(&a(), Repo::Lib, &["rev-parse", "--verify", "lib-next"]),
        ],
    )
}

/// `member-change-race-listing`, its other half: the copy the listing
/// prints, run as printed in the target's workspace and recorded, ports the
/// fix onto `dev`, and `status` stops listing it.
pub fn member_change_race_listing_until_ported() -> Scenario {
    scenario(
        "member change race listing until ported",
        vec![
            create("fix"),
            create("feat"),
            Step::commit(&ww("fix"), Repo::Lib, "lib-fix"),
            Step::rwv(&ww("fix"), &["land", "--retire"]).expect(Exit::Succeeds),
            Step::rwv(&ww("feat"), &["member", "set-branch", LIB, "dev"]).expect(Exit::Succeeds),
            Step::commit(&ww("feat"), Repo::App, "feat-app"),
            Step::rwv(
                &ww("feat"),
                &["land", "--history", "linear", "--allow-member-change", LIB],
            )
            .expect(Exit::Succeeds)
            .may_rewind(&[Repo::Lib]),
            Step::rwv(&a(), &["status"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains("lib-fix"))
                .claim(Claim::OutputContains("copy these commits onto `dev`")),
            Step::rwv_printed(&a(), "git -C ")
                .expect(Exit::Succeeds)
                .claim(Claim::OnBranch {
                    at: a(),
                    repo: Repo::Lib,
                    branch: "dev",
                }),
            Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::rwv(&a(), &["status"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputLacks("lib-fix")),
        ],
    )
}

/// `raw-merge-member-change` as its line in the scenario index runs it: a
/// raw merge of `stable` into `main` in `main`'s own workspace changes
/// `docs`'s declared branch; `status` reports it, the pin on the new branch
/// (C25) before the restore (C11), until the push branch carries it.
pub fn raw_merge_member_change_into_main() -> Scenario {
    scenario(
        "raw merge member change into main",
        vec![
            Step::rwv(
                &a(),
                &[
                    "branch",
                    "create",
                    "stable",
                    "--declared",
                    "--set-new-branch",
                    "local/remotes/docs=docs-stable",
                ],
            )
            .expect(Exit::Succeeds),
            Step::rwv(&a(), &["switch", "main"]).expect(Exit::Succeeds),
            Step::git(&a(), Repo::Proj, &["merge", "--no-edit", "stable"]),
            Step::rwv(&a(), &["status"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains("member-changed"))
                .claim(Claim::OutputInOrder(&[
                    "rwv member pin local/remotes/docs",
                    "rwv member set-branch local/remotes/docs main",
                ])),
            Step::rwv(&a(), &["status"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains("member-changed")),
        ],
    )
}

/// A `dependency`'s pin the feature changed is judged against its receiving
/// branch, and a receiving branch diverged from its upstream refuses rather
/// than passing unjudged.
pub fn a_carried_pin_against_a_diverged_receiving_branch_is_refused() -> Scenario {
    Scenario {
        name: "a carried pin against a diverged receiving branch is refused",
        seed: Seed::default().role(Repo::Lib, Role::Dependency),
        steps: vec![
            create("w"),
            Step::forge(Forge::Commit {
                repo: Repo::Lib,
                branch: "main",
                mark: "lib-up",
            }),
            Step::rwv(&ww("w"), &["fetch"]).expect(Exit::Succeeds),
            Step::rwv(&ww("w"), &["member", "pin", LIB, "origin/main", "--commit"])
                .expect(Exit::Succeeds),
            Step::commit(&a(), Repo::Lib, "lib-local"),
            Step::commit(&ww("w"), Repo::App, "w-app"),
            Step::rwv(&ww("w"), &["land"])
                .expect(Exit::refuses(Repo::Lib, State::PushBranchDiverged))
                .claim(Claim::LocalStateUnchanged(a()))
                .claim(Claim::LocalStateUnchanged(ww("w"))),
        ],
    }
}

/// A `dependency` checkout moved off its pin in a feature's workspace is
/// refused by alignment before its pin is judged, so the op-start relock
/// never records a carried pin the guard did not see.
pub fn a_dependency_moved_off_its_pin_is_refused_before_the_relock() -> Scenario {
    Scenario {
        name: "a dependency moved off its pin is refused before the relock",
        seed: Seed::default().role(Repo::Lib, Role::Dependency),
        steps: vec![
            create("w"),
            Step::forge(Forge::Commit {
                repo: Repo::Lib,
                branch: "side",
                mark: "lib-side",
            }),
            Step::forge(Forge::Commit {
                repo: Repo::Lib,
                branch: "main",
                mark: "lib-b5",
            }),
            Step::rwv(&a(), &["fetch"]).expect(Exit::Succeeds),
            Step::rwv(&a(), &["member", "pin", LIB, "origin/main", "--commit"])
                .expect(Exit::Succeeds),
            Step::rwv(&ww("w"), &["fetch"]).expect(Exit::Succeeds),
            Step::git(
                &ww("w"),
                Repo::Lib,
                &["checkout", "-q", "--detach", "origin/side"],
            ),
            Step::commit(&ww("w"), Repo::App, "w-app"),
            Step::rwv(&ww("w"), &["land"])
                .expect(Exit::refuses(Repo::Lib, State::AlignmentDetachedElsewhere))
                .claim(Claim::LocalStateUnchanged(a()))
                .claim(Claim::LocalStateUnchanged(ww("w"))),
        ],
    }
}

/// An `owned` member the landing does not move, whose pin both sides changed
/// to commits neither containing the other, is refused before anything
/// moves; the pin printed, at the receiving branch's tip holding both, run
/// as printed, lands it.
pub fn an_unmoved_member_with_diverged_pins_is_refused_before_anything_moves() -> Scenario {
    scenario(
        "an unmoved member with diverged pins is refused before anything moves",
        vec![
            create("w"),
            Step::commit(&ww("w"), Repo::App, "w-app"),
            Step::rwv(&ww("w"), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::git(&ww("w"), Repo::App, &["push", "-q", "origin", "proj--w"])
                .pushes(Remote::origin(Repo::App), "proj--w"),
            Step::forge(Forge::Commit {
                repo: Repo::App,
                branch: "main",
                mark: "main-app",
            }),
            Step::rwv(&a(), &["fetch"]).expect(Exit::Succeeds),
            Step::rwv(&a(), &["member", "pin", APP, "origin/main", "--commit"])
                .expect(Exit::Succeeds),
            Step::forge(Forge::Merge {
                repo: Repo::App,
                branch: "proj--w",
                how: MergeHow::Merge,
            }),
            Step::rwv(&ww("w"), &["fetch"]).expect(Exit::Succeeds),
            Step::commit(&ww("w"), Repo::Docs, "w-docs"),
            Step::rwv(&ww("w"), &["land"])
                .expect(Exit::refuses(Repo::App, State::PinUnmerged))
                .claim(Claim::LocalStateUnchanged(a()))
                .claim(Claim::LocalStateUnchanged(ww("w"))),
            Step::rwv_printed(&ww("w"), "rwv member pin local/remotes/app ").expect(Exit::Succeeds),
            Step::rwv(&ww("w"), &["land", "--history", "linear"])
                .expect(Exit::Succeeds)
                .claim(Claim::PinContains {
                    site: LockSite::Checkout(a(), Repo::Proj, "main".to_owned()),
                    member: Repo::App,
                    mark: "w-app",
                })
                .claim(Claim::PinContains {
                    site: LockSite::Checkout(a(), Repo::Proj, "main".to_owned()),
                    member: Repo::App,
                    mark: "main-app",
                }),
        ],
    )
}

/// An `owned` member the landing does not move, re-pinned backward by name
/// in the feature, is a change the landing names.
pub fn an_unmoved_member_pinned_backward_is_a_member_change() -> Scenario {
    scenario(
        "an unmoved member pinned backward is a member change",
        vec![
            create("w"),
            Step::commit(&a(), Repo::App, "main-app"),
            Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::git(&ww("w"), Repo::Proj, &["merge", "-q", "--no-edit", "main"]),
            Step::rwv(&ww("w"), &["member", "pin", APP, "HEAD", "--commit"])
                .expect(Exit::Succeeds)
                .may_rewind(&[Repo::App]),
            Step::commit(&ww("w"), Repo::Docs, "w-docs"),
            Step::rwv(&ww("w"), &["land"])
                .expect(Exit::refuses(Repo::App, State::TargetMemberChanges))
                .claim(Claim::OutputContains("pin: "))
                .claim(Claim::LocalStateUnchanged(a())),
            Step::rwv(&ww("w"), &["land", "--allow-member-change", APP])
                .expect(Exit::Succeeds)
                .may_rewind(&[Repo::App])
                .claim(Claim::OutputContains("re-pinned")),
        ],
    )
}

/// An `owned` member the landing could not plan is refused before it is
/// planned: detached in the feature's workspace, or pinned on neither side
/// while its receiving branch exists nowhere.
pub fn an_unplannable_member_is_refused_before_it_is_planned() -> Scenario {
    scenario(
        "an unplannable member is refused before it is planned",
        vec![
            create("w"),
            create("v"),
            Step::git(&ww("w"), Repo::Lib, &["checkout", "-q", "--detach"]),
            Step::commit(&ww("w"), Repo::Lib, "w-lib"),
            Step::rwv(&ww("w"), &["land"])
                .expect(Exit::refuses(Repo::Lib, State::AlignmentDetachedUnpushed))
                .claim(Claim::LocalStateUnchanged(a())),
            Step::hand_edit(
                &ww("v"),
                Repo::Proj,
                "rwv.toml",
                &declared_branch_text(Repo::Lib, "main"),
                &declared_branch_text(Repo::Lib, "lib-next"),
            ),
            Step::drop_lock_entry(&ww("v"), Repo::Proj, Repo::Lib),
            Step::git(
                &ww("v"),
                Repo::Proj,
                &["commit", "-qam", "lib onto lib-next"],
            ),
            Step::drop_lock_entry(&a(), Repo::Proj, Repo::Lib),
            Step::git(&a(), Repo::Proj, &["commit", "-qam", "unpinned"]),
            Step::rwv(&ww("v"), &["land", "--allow-member-change", LIB])
                .expect(Exit::refuses(Repo::Lib, State::PinNone))
                .claim(Claim::LocalStateUnchanged(a())),
        ],
    )
}

/// A member whose branch already holds the feature's tip is not moved, but
/// the feature advanced its pin past the target's: aligning the target's
/// workspace moves that checkout, so its edits refuse the landing, and the
/// commits it pins beyond the target's pin are reported, under `--json`
/// among the landing's reports beside the target left ahead of its push
/// branch.
pub fn land_reports_an_unmoved_member_it_pins_beyond_the_target() -> Scenario {
    scenario(
        "land reports an unmoved member it pins beyond the target",
        vec![
            create("w"),
            Step::forge(Forge::Commit {
                repo: Repo::Lib,
                branch: "main",
                mark: "forge-lib",
            }),
            Step::rwv(&ww("w"), &["sync"]).expect(Exit::Succeeds),
            Step::commit(&ww("w"), Repo::App, "w-app"),
            Step::hand_edit(&a(), Repo::Lib, "README", "lib", "lib edited"),
            Step::rwv(&ww("w"), &["land"])
                .expect(Exit::refuses(Repo::Lib, State::EditsDirty))
                .claim(Claim::LocalStateUnchanged(a())),
            Step::hand_edit(&a(), Repo::Lib, "README", "lib edited", "lib"),
            Step::rwv(&ww("w"), &["land", "--retire", "--json"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputInOrder(&[
                    "\"reports\": [",
                    "\"kind\": \"beyond-pin\"",
                    "\"path\": \"local/remotes/lib\"",
                    "forge-lib",
                ]))
                .claim(Claim::OutputContains("\"kind\": \"push-branch-ahead\""))
                .claim(Claim::PinContains {
                    site: main_lock(),
                    member: Repo::Lib,
                    mark: "forge-lib",
                }),
        ],
    )
}

/// A landing resumed after it composed judges the target's edits in the
/// repos it moves before it relocks, as a fresh landing does.
pub fn land_continue_judges_target_edits_where_it_moves() -> Scenario {
    scenario(
        "land continue judges target edits where it moves",
        vec![
            create("w"),
            Step::commit(&ww("w"), Repo::App, "w-app"),
            Step::commit(&ww("w"), Repo::Lib, "w-lib"),
            Step::rwv_paused(&ww("w"), &["land"], "advance-target", "landing"),
            Step::kill("landing"),
            Step::hand_edit(&a(), Repo::Lib, "README", "lib", "lib edited"),
            Step::rwv(&ww("w"), &["land", "--continue"])
                .expect(Exit::refuses(Repo::Lib, State::EditsDirty))
                .claim(Claim::LocalStateUnchanged(a()))
                .claim(Claim::LocalStateUnchanged(ww("w"))),
            Step::hand_edit(&a(), Repo::Lib, "README", "lib edited", "lib"),
            Step::rwv(&ww("w"), &["land", "--continue"])
                .expect(Exit::Succeeds)
                .claim(Claim::Contains {
                    at: a(),
                    repo: Repo::App,
                    rev: "main",
                    mark: "w-app",
                }),
        ],
    )
}

/// A landing leaves the target ahead of its push branch and says so, naming
/// the push that publishes it; running it publishes the landing.
pub fn land_reports_the_target_ahead_of_its_push_branch() -> Scenario {
    scenario(
        "land reports the target ahead of its push branch",
        vec![
            create("w"),
            Step::commit(&ww("w"), Repo::App, "w-app"),
            Step::rwv(&ww("w"), &["land", "--retire"])
                .expect(Exit::Succeeds)
                .claim(Claim::Judged(Repo::Proj, State::PushBranchAhead))
                .claim(Claim::OutputContains("rwv push --branch main")),
            publishes_records(Step::rwv(&a(), &["push", "--branch", "main"]))
                .pushes(Remote::origin(Repo::App), "main")
                .pushes(Remote::origin(Repo::Proj), "main")
                .expect(Exit::Succeeds)
                .claim(Claim::RemoteContains {
                    remote: Remote::origin(Repo::App),
                    branch: "main",
                    mark: "w-app",
                }),
        ],
    )
}

/// A project whose `main` takes in from `origin` and publishes to another
/// remote: a feature this machine published there names `main` as published
/// there, and the landing resolves it to this machine's own `main`.
pub fn land_resolves_a_target_that_publishes_where_its_record_is() -> Scenario {
    let publishing = Remote::fork(Repo::Proj);
    Scenario {
        name: "land resolves a target that publishes where its record is",
        seed: Seed::default().project_fork(),
        steps: vec![
            Step::add_remote(&a(), Repo::Proj, "pub", publishing),
            Step::git(&a(), Repo::Proj, &["config", "remote.pushDefault", "pub"]),
            Step::rwv(&a(), &["push", "--branch", "main"])
                .pushes(publishing, "main")
                .pushes(publishing, RECORDS_BRANCH)
                .expect(Exit::Succeeds),
            create("w"),
            Step::commit(&ww("w"), Repo::App, "w-app"),
            Step::rwv(&ww("w"), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::rwv(&ww("w"), &["branch", "rename", "feat"]).expect(Exit::Succeeds),
            Step::rwv(&ww("w"), &["push"])
                .pushes(publishing, "feat")
                .pushes(publishing, RECORDS_BRANCH)
                .pushes(Remote::origin(Repo::App), "feat")
                .expect(Exit::Succeeds),
            Step::rwv(&ww("w"), &["land"])
                .expect(Exit::Succeeds)
                .claim(Claim::Contains {
                    at: a(),
                    repo: Repo::App,
                    rev: "main",
                    mark: "w-app",
                }),
        ],
    }
}

/// `T` and its upstream diverged: the landing refuses before anything moves,
/// naming the merge where `T` is checked out, never the take of theirs, which
/// the feature landing into `T` refuses.
pub fn land_refuses_a_target_diverged_from_its_upstream() -> Scenario {
    scenario(
        "land refuses a target diverged from its upstream",
        vec![
            create("w"),
            Step::commit(&ww("w"), Repo::App, "w-app"),
            Step::forge(Forge::Commit {
                repo: Repo::Proj,
                branch: "main",
                mark: "forge-proj",
            }),
            Step::rwv(&a(), &["fetch"]).expect(Exit::Succeeds),
            Step::commit(&a(), Repo::Proj, "a-proj"),
            Step::rwv(&ww("w"), &["land"])
                .expect(Exit::refuses(Repo::Proj, State::PushBranchDiverged))
                .claim(Claim::OutputContains("rwv pull --merge-upstream"))
                .claim(Claim::OutputLacks("--reset-to-upstream"))
                .claim(Claim::LocalStateUnchanged(a())),
            Step::rwv(&a(), &["pull", "--merge-upstream"]).expect(Exit::Succeeds),
            Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::rwv(&ww("w"), &["land", "--history", "linear"])
                .expect(Exit::Succeeds)
                .may_strand(&[Repo::Proj])
                .claim(Claim::Contains {
                    at: a(),
                    repo: Repo::Proj,
                    rev: "main",
                    mark: "forge-proj",
                })
                .claim(Claim::Contains {
                    at: a(),
                    repo: Repo::Proj,
                    rev: "main",
                    mark: "a-proj",
                })
                .claim(Claim::Contains {
                    at: a(),
                    repo: Repo::App,
                    rev: "main",
                    mark: "w-app",
                }),
        ],
    )
}

/// A moved member's receiving branch and its upstream diverged: the landing
/// refuses before anything moves, naming the merge where it is checked out,
/// never the take of theirs, which the feature landing there refuses.
pub fn land_refuses_a_receiving_branch_diverged_from_its_upstream() -> Scenario {
    scenario(
        "land refuses a receiving branch diverged from its upstream",
        vec![
            create("w"),
            Step::commit(&ww("w"), Repo::Lib, "w-lib"),
            Step::forge(Forge::Commit {
                repo: Repo::Lib,
                branch: "main",
                mark: "forge-lib",
            }),
            Step::rwv(&a(), &["fetch"]).expect(Exit::Succeeds),
            Step::commit(&a(), Repo::Lib, "a-lib"),
            Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::rwv(&ww("w"), &["land"])
                .expect(Exit::refuses(Repo::Lib, State::PushBranchDiverged))
                .claim(Claim::OutputContains(
                    "rwv update --remote --merge-upstream --repo local/remotes/lib",
                ))
                .claim(Claim::OutputLacks("--reset-to-upstream"))
                .claim(Claim::LocalStateUnchanged(a())),
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
            .expect(Exit::Succeeds),
            Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::rwv(&ww("w"), &["land", "--history", "linear"])
                .expect(Exit::Succeeds)
                .may_strand(&[Repo::Lib, Repo::Proj])
                .claim(Claim::Contains {
                    at: a(),
                    repo: Repo::Lib,
                    rev: "main",
                    mark: "forge-lib",
                })
                .claim(Claim::Contains {
                    at: a(),
                    repo: Repo::Lib,
                    rev: "main",
                    mark: "a-lib",
                })
                .claim(Claim::OutputContains("local/remotes/lib: rebased")),
            Step::git(&a(), Repo::Lib, &["log", "--format=%s", "main"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains("w-lib")),
        ],
    )
}

/// A moved member's receiving branch is checked out in a third workspace, a
/// workweave of the same project: the landing fast-forwards it in place
/// there, and the target's own checkout, detached, follows the new pin.
/// Checked out in a raw worktree, which belongs to no workspace, the landing
/// refuses it before anything moves.
pub fn land_into_a_receiving_branch_checked_out_in_a_third_workspace() -> Scenario {
    scenario(
        "land into a receiving branch checked out in a third workspace",
        vec![
            create("w"),
            create("w3"),
            Step::commit(&ww("w"), Repo::Lib, "w-lib"),
            Step::git(&a(), Repo::Lib, &["switch", "--detach"]),
            Step::git(
                &a(),
                Repo::Lib,
                &["worktree", "add", "-q", "../lib-raw", "main"],
            ),
            Step::rwv(&ww("w"), &["land"])
                .expect(Exit::refuses(Repo::Lib, State::CheckoutInRawWorktree))
                .claim(Claim::LocalStateUnchanged(a()))
                .claim(Claim::LocalStateUnchanged(ww("w"))),
            Step::git(&a(), Repo::Lib, &["worktree", "remove", "../lib-raw"]),
            Step::git(&ww("w3"), Repo::Lib, &["switch", "main"]),
            Step::rwv(&ww("w"), &["land", "--retire"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains("in place in"))
                .claim(Claim::Contains {
                    at: a(),
                    repo: Repo::Lib,
                    rev: "main",
                    mark: "w-lib",
                })
                .claim(Claim::Contains {
                    at: ww("w3"),
                    repo: Repo::Lib,
                    rev: "HEAD",
                    mark: "w-lib",
                })
                .claim(Claim::Contains {
                    at: a(),
                    repo: Repo::Lib,
                    rev: "HEAD",
                    mark: "w-lib",
                }),
        ],
    )
}

/// A landing fast-forwarded `lib` in place in workweave `w3` and stopped in
/// its retire, having released `w3`; `w3` is then deleted. Abort no longer
/// needs to lease `w3`: it says the workspace is gone and puts `lib`'s
/// `main` back as a ref.
pub fn abort_at_retire_after_the_in_place_workspace_went() -> Scenario {
    scenario(
        "abort at retire after the in-place workspace went",
        vec![
            create("w"),
            create("w3"),
            Step::commit(&ww("w"), Repo::Lib, "w-lib"),
            Step::git(&a(), Repo::Lib, &["switch", "--detach"]),
            Step::git(&ww("w3"), Repo::Lib, &["switch", "main"]),
            Step::rwv_paused(&ww("w"), &["land", "--retire"], "retire", "landing"),
            Step::kill("landing"),
            Step::git(&ww("w3"), Repo::Lib, &["switch", "--detach"]),
            Step::rwv(&a(), &["workweave", "delete", "w3"]).expect(Exit::Succeeds),
            Step::rwv(&ww("w"), &["abort"])
                .expect(Exit::Succeeds)
                .may_rewind(&[Repo::Lib, Repo::Proj])
                .may_restore(&[Repo::Lib, Repo::Proj])
                .claim(Claim::OutputContains("gone, so nothing there is put back")),
            Step::git(&a(), Repo::Lib, &["log", "--format=%s", "main"])
                .claim(Claim::OutputLacks("w-lib")),
            Step::rwv(&ww("w"), &["status"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputLacks("in progress")),
        ],
    )
}

/// As [`abort_at_retire_after_the_in_place_workspace_went`], and then the
/// primary's `lib` is switched onto `main`, the branch the landing moved in
/// place. Abort cannot put `main` back as a ref under that checkout: it
/// refuses before anything moves, printing the detach that frees it; run
/// verbatim, the plain abort then puts `main` back.
pub fn abort_after_the_in_place_workspace_went_and_its_branch_was_checked_out(
    end: HeldInPlaceEnd,
) -> Scenario {
    let mut steps = vec![
        create("w"),
        create("w3"),
        Step::commit(&ww("w"), Repo::Lib, "w-lib"),
        Step::git(&a(), Repo::Lib, &["switch", "--detach"]),
        Step::git(&ww("w3"), Repo::Lib, &["switch", "main"]),
        Step::rwv_paused(&ww("w"), &["land", "--retire"], "retire", "landing"),
        Step::kill("landing"),
        Step::git(&ww("w3"), Repo::Lib, &["switch", "--detach"]),
        Step::rwv(&a(), &["workweave", "delete", "w3"]).expect(Exit::Succeeds),
        Step::git(&a(), Repo::Lib, &["switch", "-q", "main"]),
        Step::rwv(&ww("w"), &["abort"])
            .expect(Exit::refuses_kind(RefusalKind::ForeignTip))
            .claim(Claim::OutputContains("is checked out at"))
            .claim(Claim::OutputContains("nothing of either side is restored"))
            .claim(Claim::OutputLacks("abort stopped there"))
            .claim(Claim::LocalStateUnchanged(a()))
            .claim(Claim::LocalStateUnchanged(ww("w"))),
    ];
    match end {
        HeldInPlaceEnd::Detach => steps.extend([
            Step::rwv_printed_holding(&ww("w"), "git -C ", "switch --detach")
                .expect(Exit::Succeeds),
            Step::rwv(&ww("w"), &["abort"])
                .expect(Exit::Succeeds)
                .may_rewind(&[Repo::Lib, Repo::Proj])
                .may_restore(&[Repo::Lib, Repo::Proj]),
            Step::git(&a(), Repo::Lib, &["log", "--format=%s", "main"])
                .claim(Claim::OutputLacks("w-lib")),
        ]),
        HeldInPlaceEnd::Continue => steps.extend([
            Step::rwv_printed(&ww("w"), "rwv land --continue")
                .expect(Exit::Succeeds)
                .claim(Claim::WorkweaveGone(ww("w"))),
            Step::git(&a(), Repo::Lib, &["log", "--format=%s", "main"])
                .claim(Claim::OutputContains("w-lib")),
        ]),
        HeldInPlaceEnd::LeaveTarget => steps.extend([
            Step::rwv_printed_exactly(&ww("w"), "rwv abort --leave-target")
                .expect(Exit::Succeeds)
                .may_strand(&[Repo::Proj])
                .may_rewind(&[Repo::Lib, Repo::Proj])
                .may_restore(&[Repo::Lib, Repo::Proj]),
            Step::git(&a(), Repo::Lib, &["log", "--format=%s", "main"])
                .claim(Claim::OutputContains("w-lib")),
        ]),
    }
    steps.push(
        Step::rwv(&a(), &["status"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputLacks("in progress")),
    );
    scenario(
        "abort after the in-place workspace went and its branch was checked out",
        steps,
    )
}

/// Which printed choice ends
/// [`abort_after_the_in_place_workspace_went_and_its_branch_was_checked_out`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeldInPlaceEnd {
    Detach,
    Continue,
    LeaveTarget,
}

impl HeldInPlaceEnd {
    pub const ALL: [HeldInPlaceEnd; 3] = [
        HeldInPlaceEnd::Detach,
        HeldInPlaceEnd::Continue,
        HeldInPlaceEnd::LeaveTarget,
    ];
}

/// From the primary, a target checked out in a workweave refuses: the primary
/// would switch onto it. Freeing the workweave's checkout as printed, the
/// target is checked out nowhere: it lands as refs, and the primary switches
/// to it and aligns its members to its new pins.
pub fn land_from_the_primary_refuses_a_target_in_a_workweave() -> Scenario {
    scenario(
        "land from the primary refuses a target in a workweave",
        vec![
            Step::rwv(&a(), &["branch", "create", "feat"]).expect(Exit::Succeeds),
            Step::commit(&a(), Repo::App, "feat-app"),
            Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::rwv(
                &a(),
                &[
                    "workweave",
                    "create",
                    "--project",
                    "proj",
                    "t",
                    "--branch",
                    "main",
                ],
            )
            .expect(Exit::Succeeds),
            Step::rwv(&a(), &["land"])
                .expect(Exit::refuses(Repo::Proj, State::CheckoutInAnotherWorkspace))
                .claim(Claim::OutputContains(
                    "rwv workweave delete t --project proj",
                ))
                .claim(Claim::LocalStateUnchanged(a())),
            Step::rwv(&a(), &["workweave", "delete", "t", "--project", "proj"])
                .expect(Exit::Succeeds),
            lands_on_main(Step::rwv(&a(), &["land"]), Repo::App, "feat-app")
                .expect(Exit::Succeeds)
                .claim(Claim::OnBranch {
                    at: a(),
                    repo: Repo::Proj,
                    branch: "main",
                })
                .claim(Claim::AtPin {
                    at: a(),
                    repo: Repo::App,
                }),
        ],
    )
}

/// A moved member's checkout where the target is checked out is not on its
/// receiving branch: the landing refuses; aligning there as printed lets it
/// land.
pub fn land_refuses_a_target_member_off_its_receiving_branch() -> Scenario {
    scenario(
        "land refuses a target member off its receiving branch",
        vec![
            create("w"),
            Step::commit(&ww("w"), Repo::Lib, "w-lib"),
            Step::git(&a(), Repo::Lib, &["switch", "--detach"]),
            Step::rwv(&ww("w"), &["land"])
                .expect(Exit::refuses(Repo::Lib, State::AlignmentDetachedElsewhere))
                .claim(Claim::OutputContains("rwv update"))
                .claim(Claim::LocalStateUnchanged(a())),
            Step::rwv(&a(), &["update"]).expect(Exit::Succeeds),
            Step::rwv(&ww("w"), &["land", "--retire"])
                .expect(Exit::Succeeds)
                .claim(Claim::Contains {
                    at: a(),
                    repo: Repo::Lib,
                    rev: "main",
                    mark: "w-lib",
                }),
        ],
    )
}

/// The target's checkout of a moved member diverged from the target's pin:
/// its receiving branch does not reach the pin, which is what the landing
/// refuses; re-pinning it there as printed lets it land.
pub fn land_refuses_a_target_checkout_diverged_from_its_pin() -> Scenario {
    scenario(
        "land refuses a target checkout diverged from its pin",
        vec![
            create("w"),
            Step::commit(&ww("w"), Repo::Lib, "w-lib"),
            Step::commit(&a(), Repo::Lib, "a-lib"),
            Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::git(&a(), Repo::Lib, &["reset", "-q", "--hard", "HEAD~1"])
                .may_rewind(&[Repo::Lib])
                .may_rewrite(&[Repo::Lib])
                .may_strand(&[Repo::Lib])
                .may_leave_unresolved(&[Repo::Lib]),
            Step::commit(&a(), Repo::Lib, "a-lib-again"),
            Step::rwv(&ww("w"), &["land"])
                .expect(Exit::refuses(Repo::Lib, State::PinNotOnDeclaredBranch))
                .claim(Claim::OutputLacks("pin diverged"))
                .claim(Claim::OutputContains(
                    "rwv member pin local/remotes/lib <rev> --commit",
                ))
                .claim(Claim::LocalStateUnchanged(a())),
            Step::rwv(
                &a(),
                &["member", "pin", "local/remotes/lib", "main", "--commit"],
            )
            .expect(Exit::Succeeds)
            .may_rewind(&[Repo::Lib])
            .may_strand(&[Repo::Lib]),
            Step::rwv(&ww("w"), &["land", "--history", "linear", "--retire"])
                .expect(Exit::Succeeds),
            Step::git(&a(), Repo::Lib, &["cat-file", "-e", "main:w-lib.txt"]),
        ],
    )
}

/// `stable`, declared from `main` with its own `stable-app` and `stable-lib`
/// lines, and the primary back on `main`.
fn stable_lines() -> Vec<Step> {
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

/// The primary on `stable`, and a workweave `p` holding a feature onto it.
fn feature_onto_stable() -> Vec<Step> {
    vec![
        Step::rwv(&a(), &["switch", "stable"]).expect(Exit::Succeeds),
        create("p"),
    ]
}

/// One project commit on `main`, `port-me`: `app` two commits forward, `lib`
/// one, the `dependency` `docs` re-pinned one forward, `lib`'s entry and the
/// top-level `history` changed, and a project file added.
fn port_me_on_main() -> Vec<Step> {
    let lib_main = declared_branch_text(Repo::Lib, "main");
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

/// `cherry-pick-port`: a project commit on `main` ported onto a feature off
/// `stable`: each owned member's range copied in order and recording its
/// source, the dependency's pin carried by name, one project commit
/// mirroring the original with the relock and its source, the entry and
/// `history` changes reported `not-ported` with their setters while the
/// feature's stay; a rerun changes nothing, and the feature lands.
pub fn cherry_pick_port() -> Scenario {
    let mut steps = stable_lines();
    steps.extend(port_me_on_main());
    steps.extend(feature_onto_stable());
    steps.extend([
        Step::rwv(&ww("p"), &["cherry-pick", "main"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputContains("local/remotes/app: copied"))
            .claim(Claim::OutputContains("local/remotes/lib: copied"))
            .claim(Claim::OutputContains(
                "local/remotes/lib: not-ported recurse-submodules: true -> false",
            ))
            .claim(Claim::OutputContains(
                "rwv member set-recurse-submodules local/remotes/lib false",
            ))
            .claim(Claim::OutputContains(
                "projects/proj: not-ported history: unset -> linear",
            ))
            .claim(Claim::OutputContains("rwv branch set-history linear"))
            .claim(Claim::PinContains {
                site: LockSite::Checkout(ww("p"), Repo::Proj, "proj--p".to_owned()),
                member: Repo::Docs,
                mark: "docs-bump",
            })
            .claim(Claim::AtPin {
                at: ww("p"),
                repo: Repo::App,
            })
            .claim(Claim::AtPin {
                at: ww("p"),
                repo: Repo::Lib,
            }),
        Step::git(&ww("p"), Repo::App, &["log", "-2", "--format=%s"])
            .claim(Claim::OutputInOrder(&["fix-app-2", "fix-app-1"])),
        Step::git(&ww("p"), Repo::App, &["log", "-1", "--format=%b"])
            .claim(Claim::OutputContains("cherry picked from commit")),
        Step::git(&ww("p"), Repo::Lib, &["log", "-1", "--format=%s"])
            .claim(Claim::OutputLine("fix-lib")),
        Step::git(&ww("p"), Repo::Proj, &["log", "-1", "--format=%B"])
            .claim(Claim::OutputContains("port-me"))
            .claim(Claim::OutputContains("Rwv-Picked-From: "))
            .claim(Claim::OutputContains("Rwv-Repin: \"local/remotes/docs\"")),
        Step::git(&ww("p"), Repo::Proj, &["show", "HEAD:port-me.txt"])
            .claim(Claim::OutputLine("port-me")),
        Step::git(&ww("p"), Repo::Proj, &["show", "HEAD:rwv.toml"])
            .claim(Claim::OutputLacks("recurse-submodules"))
            .claim(Claim::OutputLacks("history")),
        Step::rwv(&ww("p"), &["cherry-pick", "main"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputContains("already holds what"))
            .claim(Claim::OutputContains("not-ported"))
            .claim(Claim::LocalStateUnchanged(ww("p"))),
        Step::rwv(&ww("p"), &["land"]).expect(Exit::Succeeds),
        Step::git(&a(), Repo::App, &["log", "--format=%s", "stable-app"])
            .claim(Claim::OutputContains("fix-app-2")),
        Step::git(&a(), Repo::Lib, &["log", "--format=%s", "stable-lib"])
            .claim(Claim::OutputContains("fix-lib")),
        Step::git(&a(), Repo::Proj, &["show", "stable:rwv.toml"])
            .claim(Claim::OutputLacks("recurse-submodules")),
    ]);
    Scenario {
        name: "cherry-pick port",
        seed: Seed::default().role(Repo::Docs, Role::Dependency),
        steps,
    }
}

/// `cherry-pick-port` under a `merge` setting: a landing into `main` made by
/// merge commits writes one project merge carrying the relock, which pins
/// each member's landing merge. That merge ports whole with `-m 1`: each
/// landed member's merge is copied against its first parent, and the port
/// lands.
pub fn cherry_pick_port_landing_merge() -> Scenario {
    let mut steps =
        vec![Step::rwv(&a(), &["branch", "set-history", "merge"]).expect(Exit::Succeeds)];
    steps.extend(stable_lines());
    steps.extend([
        create("w"),
        Step::commit(&ww("w"), Repo::App, "w-app"),
        Step::commit(&ww("w"), Repo::Lib, "w-lib"),
        Step::rwv(&ww("w"), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::commit(&a(), Repo::App, "main-moved"),
        Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::rwv(&ww("w"), &["land"]).expect(Exit::Succeeds),
        Step::git(&a(), Repo::Proj, &["rev-parse", "--verify", "main^2"]).expect(Exit::Succeeds),
        Step::git(&a(), Repo::App, &["rev-parse", "--verify", "main^2"]).expect(Exit::Succeeds),
        Step::git(&a(), Repo::Proj, &["show", "main:rwv.lock"])
            .expect(Exit::Succeeds)
            .claim(Claim::PinContains {
                site: LockSite::Branch(Store::Clone(Machine::A), Repo::Proj, "main".to_owned()),
                member: Repo::App,
                mark: "w-app",
            })
            .claim(Claim::PinContains {
                site: LockSite::Branch(Store::Clone(Machine::A), Repo::Proj, "main".to_owned()),
                member: Repo::App,
                mark: "main-moved",
            }),
    ]);
    steps.extend(feature_onto_stable());
    steps.extend([
        Step::rwv(&ww("p"), &["cherry-pick", "main"])
            .expect(Exit::refuses(Repo::Proj, State::PortMainline))
            .claim(Claim::OutputContains("rwv cherry-pick main -m <n>"))
            .claim(Claim::LocalStateUnchanged(ww("p"))),
        Step::rwv(&ww("p"), &["cherry-pick", "main", "-m", "1"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputContains("local/remotes/app: copied"))
            .claim(Claim::OutputContains("local/remotes/lib: copied")),
        Step::rwv(&ww("p"), &["land"]).expect(Exit::Succeeds),
        Step::git(&a(), Repo::App, &["show", "stable-app:w-app.txt"]).expect(Exit::Succeeds),
        Step::git(&a(), Repo::App, &["show", "stable-app:main-moved.txt"]).expect(Exit::Code(128)),
        Step::git(&a(), Repo::Lib, &["log", "--format=%s", "stable-lib"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputContains("w-lib")),
    ]);
    scenario("cherry-pick port of a landing merge", steps)
}

/// A feature off `stable` holding its own edit of `lib`'s README, and a
/// commit on `main` moving `app` cleanly and `lib` by a conflicting edit of
/// the same line.
fn conflicting_port() -> Vec<Step> {
    let mut steps = conflicting_lines();
    steps.push(
        Step::rwv(&ww("p"), &["cherry-pick", "main"])
            .expect(Exit::refuses_kind(RefusalKind::OpParked))
            .claim(Claim::OutputContains("local/remotes/app: copied"))
            .claim(Claim::OutputContains("stopped on a conflict (README)"))
            .claim(Claim::OutputContains("rwv cherry-pick --continue"))
            .claim(Claim::OutputContains("rwv abort"))
            .claim(Claim::OutputLacks("git cherry-pick --continue")),
    );
    steps
}

/// The lines of [`conflicting_port`], before the pick.
fn conflicting_lines() -> Vec<Step> {
    let mut steps = stable_lines();
    steps.extend([
        Step::commit(&a(), Repo::App, "main-app"),
        Step::hand_edit(&a(), Repo::Lib, "README", "lib\n", "lib on main\n"),
        Step::git(&a(), Repo::Lib, &["commit", "-qam", "readme-main"]),
        Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
    ]);
    steps.extend(feature_onto_stable());
    steps.extend([
        Step::hand_edit(&ww("p"), Repo::Lib, "README", "lib\n", "lib in p\n"),
        Step::git(&ww("p"), Repo::Lib, &["commit", "-qam", "readme-p"]),
        Step::rwv(&ww("p"), &["lock", "--commit"]).expect(Exit::Succeeds),
    ]);
    steps
}

/// `cherry-pick-conflict`: one member's copy conflicts; the conflict stop
/// prints the backend's hint up to staging, and `--continue` completes the
/// copy, the project commit and the relock.
pub fn cherry_pick_conflict() -> Scenario {
    let mut steps = conflicting_port();
    steps.extend([
        Step::git(&ww("p"), Repo::Lib, &["checkout", "--theirs", "README"]),
        Step::git(&ww("p"), Repo::Lib, &["add", "README"]),
        Step::rwv(&ww("p"), &["cherry-pick", "--continue"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputContains("ported"))
            .claim(Claim::AtPin {
                at: ww("p"),
                repo: Repo::Lib,
            })
            .claim(Claim::AtPin {
                at: ww("p"),
                repo: Repo::App,
            }),
        Step::git(&ww("p"), Repo::Lib, &["log", "-1", "--format=%s"])
            .claim(Claim::OutputLine("readme-main")),
        Step::git(&ww("p"), Repo::App, &["log", "-1", "--format=%s"])
            .claim(Claim::OutputLine("main-app")),
        Step::rwv(&ww("p"), &["cherry-pick", "--continue"])
            .expect(Exit::refuses_kind(RefusalKind::NoOpRecorded))
            .claim(Claim::LocalStateUnchanged(ww("p"))),
    ]);
    scenario("cherry-pick conflict continued", steps)
}

/// `cherry-pick-conflict`, abandoned: `rwv abort` puts every repo back,
/// the copy that went through included.
pub fn cherry_pick_conflict_aborts() -> Scenario {
    let mut steps = conflicting_port();
    steps.extend([
        Step::rwv(&ww("p"), &["abort"])
            .expect(Exit::Succeeds)
            .may_strand(&[Repo::App])
            .claim(Claim::AtPin {
                at: ww("p"),
                repo: Repo::App,
            })
            .claim(Claim::AtPin {
                at: ww("p"),
                repo: Repo::Lib,
            }),
        Step::git(&ww("p"), Repo::App, &["log", "-1", "--format=%s"])
            .claim(Claim::OutputLacks("main-app")),
        Step::git(&ww("p"), Repo::Lib, &["log", "-1", "--format=%s"])
            .claim(Claim::OutputLine("readme-p")),
        Step::git(&ww("p"), Repo::Lib, &["status", "--porcelain"])
            .claim(Claim::OutputLacks("README")),
        Step::git(&ww("p"), Repo::Proj, &["status", "--porcelain"])
            .claim(Claim::OutputLacks("rwv.")),
        Step::rwv(&ww("p"), &["cherry-pick", "--continue"])
            .expect(Exit::refuses_kind(RefusalKind::NoOpRecorded))
            .claim(Claim::LocalStateUnchanged(ww("p"))),
    ]);
    scenario("cherry-pick conflict aborted", steps)
}

/// `cherry-pick-conflict`, abandoned while the operator is stopped in a
/// merge of their own in `docs`, a member the port never touches: `rwv
/// abort` puts back what the port moved and leaves that merge as it is,
/// neither canceling it nor resetting the branch under it.
pub fn cherry_pick_abort_leaves_an_untouched_members_merge() -> Scenario {
    let mut steps = conflicting_port();
    steps.extend([
        Step::git(&ww("p"), Repo::Docs, &["branch", "own-a"]),
        Step::git(&ww("p"), Repo::Docs, &["branch", "own-b"]),
        Step::hand_commit(&ww("p"), Repo::Docs, "own-a", &[("own.txt", "a\n")]),
        Step::hand_commit(&ww("p"), Repo::Docs, "own-b", &[("own.txt", "b\n")]),
        Step::git(&ww("p"), Repo::Docs, &["switch", "-q", "own-a"]),
        Step::git(&ww("p"), Repo::Docs, &["merge", "--no-edit", "own-b"]).expect(Exit::Code(1)),
        Step::rwv(&ww("p"), &["abort"])
            .expect(Exit::Succeeds)
            .may_strand(&[Repo::App])
            .claim(Claim::OutputContains("left stopped in a merge"))
            .claim(Claim::OnBranch {
                at: ww("p"),
                repo: Repo::Docs,
                branch: "own-a",
            }),
        Step::git(
            &ww("p"),
            Repo::Docs,
            &["rev-parse", "-q", "--verify", "MERGE_HEAD"],
        ),
        Step::git(
            &ww("p"),
            Repo::Docs,
            &["diff", "--name-only", "--diff-filter=U"],
        )
        .claim(Claim::OutputLine("own.txt")),
    ]);
    scenario(
        "cherry-pick abort leaves an untouched member's merge",
        steps,
    )
}

/// The operator commits `own-x` on the branch checked out in `repo` at `at`,
/// then starts a merge of their own there that stops on a conflict.
fn own_merge_on_a_commit(at: &Place, repo: Repo) -> Vec<Step> {
    vec![
        Step::git(at, repo, &["branch", "own-side"]),
        Step::commit(at, repo, "own-x"),
        Step::hand_commit(at, repo, "own-side", &[("own-x.txt", "own side\n")]),
        Step::git(at, repo, &["merge", "--no-edit", "own-side"]).expect(Exit::Code(1)),
    ]
}

/// The claims of an abort refused before it begins: the checkouts at
/// `unchanged` are as they were, and the output says nothing is restored
/// (a landing's wording names both sides) and that no abort stopped part
/// way, which only an abort that began prints.
fn abort_refused_whole(step: Step, landing: bool, unchanged: &[Place]) -> Step {
    let said = if landing {
        "nothing of either side is restored"
    } else {
        "nothing is restored"
    };
    unchanged.iter().fold(
        step.claim(Claim::OutputContains(said))
            .claim(Claim::OutputLacks("abort stopped there")),
        |step, at| step.claim(Claim::LocalStateUnchanged(at.clone())),
    )
}

/// `rwv abort` over the merge [`own_merge_on_a_commit`] started in `repo`,
/// whose operand is `operand`: refused, the merge and its commit left as
/// they are; the printed abandon keeps the merge's edits and the commit as
/// backups, then puts the repo back, and the op is gone.
fn abort_refuses_then_abandons_the_operators_merge(
    at: &Place,
    repo: Repo,
    operand: &'static str,
    may_strand: &[Repo],
) -> Vec<Step> {
    vec![
        abort_refused_whole(
            Step::rwv(at, &["abort"])
                .expect(Exit::refuses_kind(RefusalKind::ForeignTip))
                .claim(Claim::OutputContains("stopped in a merge begun at"))
                .claim(Claim::OutputContains(operand)),
            false,
            std::slice::from_ref(at),
        ),
        Step::git(at, repo, &["rev-parse", "-q", "--verify", "MERGE_HEAD"]),
        Step::git(at, repo, &["log", "-1", "--format=%s"]).claim(Claim::OutputLine("own-x")),
        Step::rwv_printed(at, "rwv abort --abandon-foreign-tip")
            .expect(Exit::Succeeds)
            .may_strand(may_strand)
            .may_restore(&[repo])
            .claim(Claim::OutputContains(
                "its uncommitted edits are kept as backup",
            ))
            .claim(Claim::OutputContains("abandoned foreign tip")),
        Step::git(at, repo, &["rev-parse", "-q", "--verify", "MERGE_HEAD"]).expect(Exit::Code(1)),
        Step::git(
            at,
            repo,
            &["log", "--all", "--format=%s", "--", "own-x.txt"],
        )
        .claim(Claim::OutputLine("own-x")),
        Step::rwv(at, &["abort"]).expect(Exit::refuses_kind(RefusalKind::NoOpRecorded)),
    ]
}

/// `pull --merge-upstream` stopped on its conflict in `app`; the operator
/// gives up that merge and, at the savepoint, starts one of their own. The
/// pull integrates `app`, and canceling returns to the savepoint, but the
/// merge takes in a commit the pull never journaled: abort refuses it, and
/// the printed abandon keeps its staged content as a backup before
/// canceling it.
pub fn pull_abort_refuses_a_merge_the_operator_began_at_the_savepoint() -> Scenario {
    let mut steps = both_sides_move_app_conflictingly();
    steps.push(Step::git(&b(), Repo::App, &["merge", "--abort"]));
    steps.extend(own_merge_at_the_savepoint(&b(), Repo::App));
    steps.extend([
        abort_refused_whole(
            Step::rwv(&b(), &["abort"])
                .expect(Exit::refuses_kind(RefusalKind::ForeignTip))
                .claim(Claim::OutputContains(
                    "local/remotes/app: stopped in a merge begun at the savepoint",
                )),
            false,
            &[b()],
        ),
        Step::git(
            &b(),
            Repo::App,
            &["rev-parse", "-q", "--verify", "MERGE_HEAD"],
        ),
        Step::git(&b(), Repo::App, &["diff", "--cached", "--name-only"])
            .claim(Claim::OutputLine("own-side.txt")),
        Step::rwv_printed(&b(), "rwv abort --abandon-foreign-tip")
            .expect(Exit::Succeeds)
            .claim(Claim::OutputContains(
                "its uncommitted edits are kept as backup",
            )),
        Step::git(
            &b(),
            Repo::App,
            &["rev-parse", "-q", "--verify", "MERGE_HEAD"],
        )
        .expect(Exit::Code(1)),
        Step::git(
            &b(),
            Repo::App,
            &["log", "--all", "--format=kept %s", "--", "own-side.txt"],
        )
        .claim(Claim::OutputContains("kept")),
    ]);
    scenario(
        "pull abort refuses a merge the operator began at the savepoint",
        steps,
    )
}

/// A landing stopped in `app`'s rebase; the operator gives that rebase up
/// and starts an interactive one of their own, stopped after its first
/// pick, and commits there. Canceling it returns to the savepoint, but it replays
/// onto a commit the landing never journaled: abort refuses, and the printed
/// abandon keeps the commit made inside it before canceling.
pub fn abort_refuses_an_interactive_rebase_the_operator_began() -> Scenario {
    let (mut steps, _) = landing_stopped_mid_rebase(MidRebase::Member);
    steps.extend([
        Step::git(&ww("w"), Repo::App, &["rebase", "--abort"]),
        Step::git(
            &ww("w"),
            Repo::App,
            &["rebase", "-q", "--exec", "exit 1", "HEAD~1"],
        )
        .expect(Exit::Code(1)),
        Step::commit(&ww("w"), Repo::App, "inside-edit"),
        abort_refused_whole(
            Step::rwv(&ww("w"), &["abort"])
                .expect(Exit::refuses_kind(RefusalKind::ForeignTip))
                .claim(Claim::OutputContains(
                    "local/remotes/app: stopped in a rebase",
                )),
            true,
            &[ww("w")],
        ),
        Step::rwv_printed(&ww("w"), "rwv abort --abandon-foreign-tip")
            .expect(Exit::Succeeds)
            .may_rewind(&[Repo::App])
            .may_restore(&[Repo::App, Repo::Proj])
            .claim(Claim::OutputContains("kept as backup")),
        Step::git(&ww("w"), Repo::App, &["log", "--all", "--format=%s"])
            .claim(Claim::OutputContains("inside-edit")),
    ]);
    scenario(
        "abort refuses an interactive rebase the operator began",
        steps,
    )
}

/// `pull --merge-upstream` stopped on its conflict in `app`; the operator
/// gives up that merge, commits, and starts a merge of their own there. The
/// pull integrates `app`, but abort cancels only its own merge: the
/// operator's is refused, and their commit stays on its branch.
pub fn pull_abort_refuses_a_merge_begun_on_the_operators_commit() -> Scenario {
    let mut steps = both_sides_move_app_conflictingly();
    steps.push(Step::git(&b(), Repo::App, &["merge", "--abort"]));
    steps.extend(own_merge_on_a_commit(&b(), Repo::App));
    steps.extend(abort_refuses_then_abandons_the_operators_merge(
        &b(),
        Repo::App,
        "local/remotes/app: stopped in a merge",
        &[],
    ));
    scenario(
        "pull abort refuses a merge begun on the operator's commit",
        steps,
    )
}

/// `cherry-pick` copied onto `app`, then stopped on its conflict in `lib`;
/// the operator commits on the copy in `app` and starts a merge of their
/// own there: abort refuses it, keeping their commit.
pub fn cherry_pick_abort_refuses_a_merge_begun_on_the_operators_commit() -> Scenario {
    let mut steps = conflicting_port();
    steps.extend(own_merge_on_a_commit(&ww("p"), Repo::App));
    steps.extend(abort_refuses_then_abandons_the_operators_merge(
        &ww("p"),
        Repo::App,
        "local/remotes/app: stopped in a merge",
        &[Repo::App],
    ));
    scenario(
        "cherry-pick abort refuses a merge begun on the operator's commit",
        steps,
    )
}

/// `pull --merge-upstream` stopped on its conflict in `app`; in `docs`,
/// which the pull never merges, the operator commits and starts a merge of
/// their own: abort leaves it and the commit, and restores the rest.
pub fn pull_abort_refuses_a_merge_in_a_member_it_never_merges() -> Scenario {
    let mut steps = both_sides_move_app_conflictingly();
    steps.extend(own_merge_on_a_commit(&b(), Repo::Docs));
    steps.extend(abort_refuses_then_abandons_the_operators_merge(
        &b(),
        Repo::Docs,
        "local/remotes/docs: stopped in a merge",
        &[],
    ));
    steps.push(
        Step::git(
            &b(),
            Repo::App,
            &["rev-parse", "-q", "--verify", "MERGE_HEAD"],
        )
        .expect(Exit::Code(1)),
    );
    scenario(
        "pull abort refuses a merge in a member it never merges",
        steps,
    )
}

/// The operator starts a merge of their own in `repo` at `at`, on the tip
/// the op savepointed, and leaves it uncommitted.
fn own_merge_at_the_savepoint(at: &Place, repo: Repo) -> Vec<Step> {
    vec![
        Step::git(at, repo, &["branch", "own-side"]),
        Step::hand_commit(at, repo, "own-side", &[("own-side.txt", "own side\n")]),
        Step::git(at, repo, &["merge", "--no-ff", "--no-commit", "own-side"]),
    ]
}

/// `rwv abort` leaves the merge [`own_merge_at_the_savepoint`] started in
/// `repo`, a repo the op never merges, as it is, and ends the op.
fn abort_leaves_the_operators_merge(at: &Place, repo: Repo) -> Vec<Step> {
    vec![
        Step::rwv(at, &["abort"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputContains("left stopped in a merge")),
        Step::git(at, repo, &["rev-parse", "-q", "--verify", "MERGE_HEAD"]),
        Step::git(at, repo, &["diff", "--cached", "--name-only"])
            .claim(Claim::OutputLine("own-side.txt")),
        Step::git(
            at,
            Repo::App,
            &["rev-parse", "-q", "--verify", "MERGE_HEAD"],
        )
        .expect(Exit::Code(1)),
    ]
}

/// `pull --merge-upstream` stopped on its conflict in `app`; in `docs`,
/// which the pull never merges, the operator has a merge of their own in
/// progress, begun where the pull found it: abort leaves that merge.
pub fn pull_abort_leaves_a_merge_in_a_member_it_never_merges() -> Scenario {
    let mut steps = both_sides_move_app_conflictingly();
    steps.extend(own_merge_at_the_savepoint(&b(), Repo::Docs));
    steps.extend(abort_leaves_the_operators_merge(&b(), Repo::Docs));
    scenario(
        "pull abort leaves a merge in a member it never merges",
        steps,
    )
}

/// The steps that stop `update --remote --merge-upstream` on a conflict in
/// `app` on machine A.
fn update_remote_stopped_on_a_conflict() -> Vec<Step> {
    vec![
        Step::new_machine(Machine::B),
        Step::hand_edit(&b(), Repo::App, "README", "app", "beta"),
        Step::git(&b(), Repo::App, &["commit", "-qam", "beta"]),
        Step::git(&b(), Repo::App, &["push", "origin", "main"])
            .pushes(Remote::origin(Repo::App), "main"),
        Step::hand_edit(&a(), Repo::App, "README", "app", "alpha"),
        Step::git(&a(), Repo::App, &["commit", "-qam", "alpha"]),
        Step::rwv(&a(), &["update", "--remote", "--merge-upstream"])
            .expect(Exit::refuses_kind(RefusalKind::OpParked))
            .claim(Claim::OutputContains("rwv update --remote --continue")),
    ]
}

/// `update --remote --reset-to-upstream lib` killed right after it reset
/// `lib`'s `main` to its upstream, `lib` checked out on `main` or detached:
/// the move was journaled before it was made, so abort puts `main` back on
/// the commit it held rather than leaving the upstream as someone else's.
pub fn update_remote_reset_killed_after_the_move(detached: bool) -> Scenario {
    let mut steps = vec![
        Step::new_machine(Machine::B),
        Step::commit(&b(), Repo::Lib, "beta-lib"),
        Step::git(&b(), Repo::Lib, &["push", "origin", "main"])
            .pushes(Remote::origin(Repo::Lib), "main"),
        Step::commit(&a(), Repo::Lib, "alpha-lib"),
    ];
    if detached {
        steps.push(Step::git(&a(), Repo::Lib, &["switch", "-q", "--detach"]));
    }
    steps.extend([
        Step::rwv_paused(
            &a(),
            &["update", "--remote", "--reset-to-upstream", LIB],
            "reset-to-upstream",
            "update",
        )
        .may_rewrite(&[Repo::Lib]),
        Step::kill("update"),
        Step::git(&a(), Repo::Lib, &["log", "--format=%s", "main"])
            .claim(Claim::OutputLacks("alpha-lib")),
        Step::rwv(&a(), &["abort"])
            .expect(Exit::Succeeds)
            .may_restore(&[Repo::Lib]),
        Step::git(&a(), Repo::Lib, &["log", "--format=%s", "main"])
            .claim(Claim::OutputContains("alpha-lib"))
            .claim(Claim::OutputLacks("beta-lib")),
    ]);
    scenario("update --remote reset killed after the move", steps)
}

/// `update --remote --merge-upstream --reset-to-upstream lib` resets
/// `lib`'s `main` to its upstream as a ref, `lib` being detached on machine
/// A, and stops on its conflict in `app`; someone then commits onto `lib`'s `main`. Abort
/// refuses to put that branch back and keeps the op; the printed
/// `--abandon-foreign-tip` keeps the new tip as a backup and restores it.
pub fn update_remote_abort_over_a_ref_moved_since() -> Scenario {
    scenario(
        "update --remote abort over a ref moved since",
        vec![
            Step::new_machine(Machine::B),
            Step::commit(&b(), Repo::Lib, "beta-lib"),
            Step::git(&b(), Repo::Lib, &["push", "origin", "main"])
                .pushes(Remote::origin(Repo::Lib), "main"),
            Step::hand_edit(&b(), Repo::App, "README", "app", "beta"),
            Step::git(&b(), Repo::App, &["commit", "-qam", "beta"]),
            Step::git(&b(), Repo::App, &["push", "origin", "main"])
                .pushes(Remote::origin(Repo::App), "main"),
            Step::hand_edit(&a(), Repo::App, "README", "app", "alpha"),
            Step::git(&a(), Repo::App, &["commit", "-qam", "alpha"]),
            Step::commit(&a(), Repo::Lib, "alpha-lib"),
            Step::git(&a(), Repo::Lib, &["switch", "-q", "--detach"]),
            Step::rwv(
                &a(),
                &[
                    "update",
                    "--remote",
                    "--merge-upstream",
                    "--reset-to-upstream",
                    LIB,
                ],
            )
            .expect(Exit::refuses_kind(RefusalKind::OpParked))
            .may_rewrite(&[Repo::Lib]),
            Step::git(&a(), Repo::Lib, &["log", "--format=%s", "main"])
                .claim(Claim::OutputContains("beta-lib"))
                .claim(Claim::OutputLacks("alpha-lib")),
            Step::hand_commit(&a(), Repo::Lib, "main", &[("raced.txt", "raced\n")]),
            abort_refused_whole(
                Step::rwv(&a(), &["abort"])
                    .expect(Exit::refuses_kind(RefusalKind::ForeignTip))
                    .claim(Claim::OutputContains("moved since the op wrote it")),
                false,
                &[a()],
            ),
            Step::rwv_printed(&a(), "rwv abort --abandon-foreign-tip")
                .expect(Exit::Succeeds)
                .may_restore(&[Repo::Lib]),
            Step::git(&a(), Repo::Lib, &["log", "--format=%s", "main"])
                .claim(Claim::OutputLacks("beta-lib"))
                .claim(Claim::OutputContains("alpha-lib")),
            Step::git(&a(), Repo::Lib, &["log", "--all", "--format=kept %s"])
                .claim(Claim::OutputContains("kept hand commit")),
        ],
    )
}

/// `update --remote --merge-upstream` stopped on its conflict in `app`; in
/// `docs`, which it never merges, the operator has a merge of their own in
/// progress, begun where the update found it: abort leaves that merge.
pub fn update_remote_abort_leaves_a_merge_in_a_member_it_never_merges() -> Scenario {
    let mut steps = update_remote_stopped_on_a_conflict();
    steps.extend(own_merge_at_the_savepoint(&a(), Repo::Docs));
    steps.extend(abort_leaves_the_operators_merge(&a(), Repo::Docs));
    scenario(
        "update --remote abort leaves a merge in a member it never merges",
        steps,
    )
}

/// `update --remote --merge-upstream` stopped on its conflict in `app`; in
/// `docs`, which it never merges, the operator commits and starts a merge
/// of their own: abort leaves it and the commit.
pub fn update_remote_abort_refuses_a_merge_in_a_member_it_never_merges() -> Scenario {
    let mut steps = update_remote_stopped_on_a_conflict();
    steps.extend(own_merge_on_a_commit(&a(), Repo::Docs));
    steps.extend(abort_refuses_then_abandons_the_operators_merge(
        &a(),
        Repo::Docs,
        "local/remotes/docs: stopped in a merge",
        &[],
    ));
    steps.push(
        Step::git(
            &a(),
            Repo::App,
            &["rev-parse", "-q", "--verify", "MERGE_HEAD"],
        )
        .expect(Exit::Code(1)),
    );
    scenario(
        "update --remote abort refuses a merge in a member it never merges",
        steps,
    )
}

/// Where a landing that moved its target's workspace is stopped: once its
/// last fast-forward is made, still holding the target, or in its retire,
/// once it released the target, which `rwv abort` takes back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LandedStop {
    Landed,
    Retire,
}

impl LandedStop {
    pub const ALL: [LandedStop; 2] = [LandedStop::Landed, LandedStop::Retire];

    pub fn point(self) -> &'static str {
        match self {
            LandedStop::Landed => "landed",
            LandedStop::Retire => "retire",
        }
    }
}

/// A landing run in `w` with `args`, stopped in its retire with the retire
/// recorded as stopped: killed before it released the target, then continued
/// into a retire that refuses on an edit in `docs`, which it never moves.
fn retire_refused_on_dirt(args: &[&str]) -> Vec<Step> {
    vec![
        Step::rwv_paused(&ww("w"), args, "retiring", "landing"),
        Step::kill("landing"),
        Step::hand_edit(&ww("w"), Repo::Docs, "README", "docs", "w dirt"),
        Step::rwv(&ww("w"), &["land", "--continue"])
            .expect(Exit::refuses(Repo::Docs, State::EditsDirty)),
    ]
}

/// A landing fast-forwarded `app` in the target's workspace, then was
/// killed; someone edits `app` there. `rwv abort` run in the target keeps
/// the edits as a backup, then resets; run from the source, it refuses to
/// discard edits in a workspace it was not run in before anything of either
/// side moves, printing the choices, and once they are set aside there the
/// rerun restores everything. Stopped in its retire, the landing is aborted
/// from the target only once its retire refused, so its record shows it
/// stopped.
pub fn abort_keeps_edits_made_in_the_target_after_the_stop(
    stop: LandedStop,
    from: AbortFrom,
) -> Scenario {
    let recorded = stop == LandedStop::Retire && from == AbortFrom::Target;
    let mut steps = vec![create("w"), Step::commit(&ww("w"), Repo::App, "w-app")];
    if recorded {
        steps.extend(retire_refused_on_dirt(&["land", "--retire"]));
    } else {
        steps.extend([
            Step::rwv_paused(&ww("w"), &["land", "--retire"], stop.point(), "landing"),
            Step::kill("landing"),
        ]);
    }
    steps.extend([
        Step::git(&a(), Repo::App, &["log", "-1", "--format=%s"]).claim(Claim::OutputLine("w-app")),
        Step::hand_edit(&a(), Repo::App, "README", "app", "edited in the target"),
    ]);
    match from {
        AbortFrom::Target => steps.extend([
            Step::rwv(&a(), &["abort"])
                .expect(Exit::Succeeds)
                .may_strand(&[Repo::Proj])
                .may_rewind(&[Repo::App])
                .may_restore(&[Repo::App, Repo::Proj])
                .claim(Claim::OutputContains(
                    "local/remotes/app: its uncommitted edits are kept as backup",
                )),
            Step::git(
                &a(),
                Repo::App,
                &["log", "--all", "--format=kept %s", "-Sedited in the target"],
            )
            .claim(Claim::OutputContains("kept ")),
        ]),
        AbortFrom::Source => {
            let refused = Step::rwv(&ww("w"), &["abort"])
                .expect(Exit::refuses_kind(RefusalKind::ForeignTip))
                .claim(Claim::OutputLacks("abort stopped there"))
                .claim(Claim::OutputContains(
                    "[target] local/remotes/app: holds uncommitted edits",
                ))
                .claim(Claim::OutputContains("nothing of either side is restored"))
                .claim(Claim::LocalStateUnchanged(a()))
                .claim(Claim::LocalStateUnchanged(ww("w")));
            match stop {
                LandedStop::Landed => steps.extend([
                    refused,
                    Step::rwv_printed(&ww("w"), "rwv -C ")
                        .expect(Exit::Succeeds)
                        .may_strand(&[Repo::Proj])
                        .may_rewind(&[Repo::App])
                        .may_restore(&[Repo::App, Repo::Proj])
                        .claim(Claim::OutputContains(
                            "local/remotes/app: its uncommitted edits are kept as backup",
                        )),
                ]),
                LandedStop::Retire => steps.extend([
                    refused.claim(Claim::OutputLacks("rwv -C ")),
                    Step::git(&a(), Repo::App, &["diff", "--name-only"])
                        .claim(Claim::OutputLine("README")),
                    Step::rwv_printed(&ww("w"), "git -C ").expect(Exit::Succeeds),
                    Step::rwv(&ww("w"), &["abort"])
                        .expect(Exit::Succeeds)
                        .may_strand(&[Repo::Proj])
                        .may_rewind(&[Repo::App])
                        .may_restore(&[Repo::App, Repo::Proj]),
                    Step::git(&a(), Repo::App, &["stash", "list"])
                        .claim(Claim::OutputContains("stash@")),
                ]),
            }
        }
    }
    steps.push(
        Step::git(&a(), Repo::App, &["log", "-1", "--format=%s"])
            .claim(Claim::OutputLacks("w-app")),
    );
    if recorded {
        steps.push(Step::git(
            &ww("w"),
            Repo::Docs,
            &["checkout", "--", "README"],
        ));
    }
    steps.push(
        Step::rwv(&ww("w"), &["land", "--retire"])
            .expect(Exit::Succeeds)
            .claim(Claim::WorkweaveGone(ww("w"))),
    );
    scenario("abort keeps edits made in the target after the stop", steps)
}

/// A landing that removed `app`'s README fast-forwarded the target's
/// workspace and was killed in its retire, releasing it; someone puts an
/// untracked README back there. Restoring the savepoint would overwrite it,
/// and abort from the source is refused before anything moves; abort in the
/// target is refused too, so the printed stash is the way out, and it sets
/// the untracked file aside: the rerun restores everything.
pub fn abort_sets_aside_an_untracked_file_in_the_way_in_the_target() -> Scenario {
    let steps = vec![
        create("w"),
        Step::git(&ww("w"), Repo::App, &["rm", "-q", "README"]),
        Step::git(&ww("w"), Repo::App, &["commit", "-qm", "w drops README"]),
        Step::rwv_paused(&ww("w"), &["land", "--retire"], "retire", "landing"),
        Step::kill("landing"),
        Step::git(&a(), Repo::App, &["log", "-1", "--format=%s"])
            .claim(Claim::OutputLine("w drops README")),
        Step::git(
            &a(),
            Repo::App,
            &["restore", "--source=HEAD~1", "--worktree", "--", "README"],
        ),
        Step::hand_edit(&a(), Repo::App, "README", "app", "untracked in the target"),
        Step::rwv(&ww("w"), &["abort"])
            .expect(Exit::refuses_kind(RefusalKind::ForeignTip))
            .claim(Claim::OutputContains(
                "[target] local/remotes/app: holds uncommitted edits",
            ))
            .claim(Claim::OutputContains("nothing of either side is restored"))
            .claim(Claim::OutputLacks("rwv -C "))
            .claim(Claim::LocalStateUnchanged(a()))
            .claim(Claim::LocalStateUnchanged(ww("w"))),
        Step::rwv_printed(&ww("w"), "git -C ").expect(Exit::Succeeds),
        Step::rwv(&ww("w"), &["abort"])
            .expect(Exit::Succeeds)
            .may_strand(&[Repo::Proj])
            .may_rewind(&[Repo::App])
            .may_restore(&[Repo::App, Repo::Proj]),
        Step::git(
            &a(),
            Repo::App,
            &[
                "log",
                "--all",
                "--format=kept %s",
                "-Suntracked in the target",
            ],
        )
        .claim(Claim::OutputContains("kept ")),
        Step::git(&a(), Repo::App, &["log", "-1", "--format=%s"])
            .claim(Claim::OutputLacks("w drops README")),
        Step::rwv(&ww("w"), &["land", "--retire"])
            .expect(Exit::Succeeds)
            .claim(Claim::WorkweaveGone(ww("w"))),
    ];
    scenario(
        "abort sets aside an untracked file in the way in the target",
        steps,
    )
}

/// A landing fast-forwarded `app` in the target's workspace, then was
/// killed; someone starts a merge there on the landing's tip. Abort never
/// cancels an operation in the target: it refuses before anything of either
/// side moves, keeping the op. Where the landing still leases the target,
/// the printed abort run there gives the merge up, its edits kept as a
/// backup; where it was killed in its retire, abort there is refused, so
/// no give-up is printed, and once that merge is given up by hand, the
/// printed rerun restores everything.
pub fn abort_refuses_a_merge_in_a_target_repo_the_landing_moved(stop: LandedStop) -> Scenario {
    let mut steps = vec![
        create("w"),
        Step::commit(&ww("w"), Repo::App, "w-app"),
        Step::rwv_paused(&ww("w"), &["land", "--retire"], stop.point(), "landing"),
        Step::kill("landing"),
    ];
    steps.extend(own_merge_at_the_savepoint(&a(), Repo::App));
    let refused = Step::rwv(&ww("w"), &["abort"])
        .expect(Exit::refuses_kind(RefusalKind::ForeignTip))
        .claim(Claim::OutputLacks("abort stopped there"))
        .claim(Claim::OutputContains(
            "[target] local/remotes/app: stopped in a merge begun at",
        ))
        .claim(Claim::OutputContains("nothing of either side is restored"))
        .claim(Claim::OutputLacks("rwv abort --abandon-foreign-tip"))
        .claim(Claim::LocalStateUnchanged(a()))
        .claim(Claim::LocalStateUnchanged(ww("w")));
    steps.push(match stop {
        LandedStop::Landed => refused,
        LandedStop::Retire => refused.claim(Claim::OutputLacks("rwv -C ")),
    });
    match stop {
        LandedStop::Landed => steps.extend([
            Step::rwv_printed_holding(&ww("w"), "rwv -C ", "--abandon-foreign-tip")
                .expect(Exit::Succeeds)
                .may_strand(&[Repo::Proj])
                .may_rewind(&[Repo::App])
                .may_restore(&[Repo::App, Repo::Proj])
                .claim(Claim::OutputContains(
                    "local/remotes/app: its uncommitted edits are kept as backup",
                ))
                .claim(Claim::OutputContains(
                    "local/remotes/app: restored (from mid-op state)",
                )),
            Step::git(
                &a(),
                Repo::App,
                &["rev-parse", "-q", "--verify", "MERGE_HEAD"],
            )
            .expect(Exit::Code(1)),
        ]),
        LandedStop::Retire => steps.extend([
            Step::git(&a(), Repo::App, &["merge", "--abort"]),
            Step::rwv_printed(&ww("w"), "rwv abort")
                .expect(Exit::Succeeds)
                .may_strand(&[Repo::Proj])
                .may_rewind(&[Repo::App])
                .may_restore(&[Repo::App, Repo::Proj])
                .claim(Claim::OutputContains(
                    "[target] local/remotes/app: restored (from recorded converged tip)",
                )),
        ]),
    }
    steps.extend([Step::rwv(&ww("w"), &["land", "--retire"])
        .expect(Exit::Succeeds)
        .claim(Claim::WorkweaveGone(ww("w")))]);
    scenario(
        "abort refuses a merge in a target repo the landing moved",
        steps,
    )
}

/// `cherry-pick` copied onto `app`, then stopped on its conflict in `lib`;
/// the operator detaches `app` and deletes the branch the copy went onto.
/// Abort cannot switch `app` back, so it refuses before anything moves,
/// `lib`'s stopped copy included, and prints the command that puts the
/// branch back at the savepoint; `--continue` is still open, and refuses the
/// detached checkout on its own terms; the printed fix lets the rerun
/// restore.
pub fn cherry_pick_abort_names_how_to_put_back_a_deleted_branch() -> Scenario {
    let mut steps = conflicting_port();
    steps.extend([
        Step::git(&ww("p"), Repo::App, &["switch", "-q", "--detach"]),
        Step::git(&ww("p"), Repo::App, &["branch", "-D", "proj--p"]),
        Step::rwv(&ww("p"), &["abort"])
            .expect(Exit::refuses_kind(RefusalKind::ForeignTip))
            .claim(Claim::OutputLacks("abort stopped there"))
            .claim(Claim::OutputContains(
                "local/remotes/app: is off 'proj--p', where its savepoint was taken",
            ))
            .claim(Claim::LocalStateUnchanged(ww("p"))),
        Step::rwv(&ww("p"), &["cherry-pick", "--continue"])
            .expect(Exit::refuses_kind(RefusalKind::HeadNotOnBranch))
            .claim(Claim::LocalStateUnchanged(ww("p"))),
        abort_refused_whole(
            Step::rwv(&ww("p"), &["abort"]).expect(Exit::refuses_kind(RefusalKind::ForeignTip)),
            false,
            &[ww("p")],
        ),
        Step::rwv_printed(&ww("p"), "git -C ").expect(Exit::Succeeds),
        Step::rwv(&ww("p"), &["abort"])
            .expect(Exit::Succeeds)
            .may_strand(&[Repo::App])
            .claim(Claim::OnBranch {
                at: ww("p"),
                repo: Repo::App,
                branch: "proj--p",
            })
            .claim(Claim::AtPin {
                at: ww("p"),
                repo: Repo::App,
            }),
    ]);
    scenario(
        "cherry-pick abort names how to put back a deleted branch",
        steps,
    )
}

/// `cherry-pick` copied onto `app`, then stopped on its conflict in `lib`;
/// the operator edits the copied file in `app`: abort resets `app` to its
/// savepoint, keeping the edit as a backup first.
pub fn cherry_pick_abort_keeps_edits_made_after_the_stop() -> Scenario {
    let mut steps = conflicting_port();
    steps.extend([
        Step::hand_edit(
            &ww("p"),
            Repo::App,
            "main-app.txt",
            "main-app",
            "edited after the stop",
        ),
        Step::rwv(&ww("p"), &["abort"])
            .expect(Exit::Succeeds)
            .may_strand(&[Repo::App])
            .claim(Claim::OutputContains(
                "local/remotes/app: its uncommitted edits are kept as backup",
            ))
            .claim(Claim::AtPin {
                at: ww("p"),
                repo: Repo::App,
            }),
        Step::git(
            &ww("p"),
            Repo::App,
            &[
                "log",
                "--all",
                "--format=kept %s",
                "-Sedited after the stop",
            ],
        )
        .claim(Claim::OutputContains("kept ")),
    ]);
    scenario("cherry-pick abort keeps edits made after the stop", steps)
}

/// `cherry-pick-refusals`: outside a feature, a merge commit without `-m`
/// and an `-m` on a commit that is no merge, a backward pin, and a member
/// range holding a merge are each refused before anything moves, naming the
/// way out; the merge's printed path, the hand cherry-pick then the rerun
/// with `--except`, completes, recording that member's tip.
pub fn cherry_pick_refusals() -> Scenario {
    let mut steps = stable_lines();
    steps.extend([
        Step::rwv(&a(), &["cherry-pick", "main"])
            .expect(Exit::refuses(Repo::Proj, State::TargetNoTarget))
            .claim(Claim::OutputContains("rwv workweave create <ww>"))
            .claim(Claim::OutputContains("rwv branch create <W>"))
            .claim(Claim::LocalStateUnchanged(a())),
        Step::git(&a(), Repo::App, &["branch", "side"]),
        Step::hand_commit(&a(), Repo::App, "side", &[("side.txt", "side\n")]),
        Step::git(
            &a(),
            Repo::App,
            &["merge", "--no-ff", "-m", "app-merge", "side"],
        ),
        Step::commit(&a(), Repo::Lib, "lib-fix"),
        Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::rwv(&a(), &["member", "pin", LIB, "HEAD~1", "--commit"])
            .expect(Exit::Succeeds)
            .may_rewind(&[Repo::Lib]),
        Step::git(&a(), Repo::Proj, &["branch", "proj-side"]),
        Step::hand_commit(&a(), Repo::Proj, "proj-side", &[("side.txt", "side\n")]),
        Step::git(
            &a(),
            Repo::Proj,
            &["merge", "--no-ff", "-m", "proj-merge", "proj-side"],
        ),
    ]);
    steps.extend(feature_onto_stable());
    steps.extend([
        Step::rwv(&ww("p"), &["cherry-pick", "main"])
            .expect(Exit::refuses(Repo::Proj, State::PortMainline))
            .claim(Claim::OutputContains("rwv cherry-pick main -m <n>"))
            .claim(Claim::LocalStateUnchanged(ww("p"))),
        Step::rwv(&ww("p"), &["cherry-pick", "main", "-m", "3"])
            .expect(Exit::refuses(Repo::Proj, State::PortMainline))
            .claim(Claim::LocalStateUnchanged(ww("p"))),
        Step::rwv(&ww("p"), &["cherry-pick", "main~1", "-m", "1"])
            .expect(Exit::refuses(Repo::Proj, State::PortMainline))
            .claim(Claim::OutputContains("rwv cherry-pick main~1\n"))
            .claim(Claim::LocalStateUnchanged(ww("p"))),
        Step::rwv(&ww("p"), &["cherry-pick", "main~1"])
            .expect(Exit::refuses(Repo::Lib, State::PortBackward))
            .claim(Claim::OutputContains("lib-fix"))
            .claim(Claim::OutputContains("revert"))
            .claim(Claim::OutputContains(
                "rerun with --except local/remotes/lib",
            ))
            .claim(Claim::LocalStateUnchanged(ww("p"))),
        Step::rwv(&ww("p"), &["cherry-pick", "main~2", "--except", DOCS])
            .expect(Exit::refuses_kind(RefusalKind::InapplicableFlag))
            .claim(Claim::LocalStateUnchanged(ww("p"))),
        Step::rwv(&ww("p"), &["cherry-pick", "main~2"])
            .expect(Exit::refuses(Repo::App, State::PortMergeInRange))
            .claim(Claim::OutputContains("app-merge (merge)"))
            .claim(Claim::OutputContains("cherry-pick -m 1"))
            .claim(Claim::OutputContains(
                "rerun with --except local/remotes/app",
            ))
            .claim(Claim::LocalStateUnchanged(ww("p"))),
        Step::rwv_printed(&ww("p"), "git -C ").expect(Exit::Succeeds),
        Step::git(&ww("p"), Repo::App, &["show", "HEAD:side.txt"]).claim(Claim::OutputLine("side")),
        Step::rwv(&ww("p"), &["cherry-pick", "main~2", "--except", APP])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputContains("local/remotes/lib: copied"))
            .claim(Claim::AtPin {
                at: ww("p"),
                repo: Repo::App,
            })
            .claim(Claim::AtPin {
                at: ww("p"),
                repo: Repo::Lib,
            }),
        Step::git(&ww("p"), Repo::Lib, &["log", "-1", "--format=%s"])
            .claim(Claim::OutputLine("lib-fix")),
        Step::rwv(&ww("p"), &["cherry-pick", "main~1"])
            .expect(Exit::refuses(Repo::Lib, State::PortBackward))
            .claim(Claim::LocalStateUnchanged(ww("p"))),
        Step::git(&ww("p"), Repo::Lib, &["revert", "--no-edit", "HEAD"]),
        Step::rwv(&ww("p"), &["cherry-pick", "main~1", "--except", LIB])
            .expect(Exit::Succeeds)
            .claim(Claim::AtPin {
                at: ww("p"),
                repo: Repo::Lib,
            }),
        Step::git(&ww("p"), Repo::Proj, &["log", "-1", "--format=%B"])
            .claim(Claim::OutputContains("Rwv-Picked-From: ")),
    ]);
    scenario("cherry-pick refusals", steps)
}

/// `A` presents `proj2`, which declares the shared `lib` on `dev`, behind
/// `proj`'s pin. A landing into `proj`'s `main` there lands what it moves, is
/// not refused for `lib`'s pin, and leaves `lib` where `proj2` has it: it did
/// not finish aligning, and says so with the state that kept `lib` and that
/// state's choice. Running the choice aligns `lib` for `proj`.
pub fn land_leaves_a_shared_member_where_the_presented_project_has_it() -> Scenario {
    Scenario {
        name: "land leaves a shared member where the presented project has it",
        seed: Seed::default().second_project(SecondProject {
            branch: "dev",
            also: &[],
        }),
        steps: vec![
            Step::clone_project(Machine::A, Repo::Proj2).expect(Exit::Succeeds),
            Step::rwv(&a(), &["activate", "proj2", "--update"]).expect(Exit::Succeeds),
            create("w"),
            Step::commit(&ww("w"), Repo::App, "w-app"),
            Step::rwv(&ww("w"), &["land", "--retire"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputLacks("pin behind"))
                .claim(Claim::OutputContains("did not finish aligning:"))
                .claim(Claim::OutputLacks("finishes it"))
                .claim(Claim::Judged(Repo::Lib, State::AlignmentOnBranch))
                .claim(Claim::OutputContains("rwv activate proj --update"))
                .claim(Claim::Contains {
                    at: a(),
                    repo: Repo::App,
                    rev: "main",
                    mark: "w-app",
                })
                .claim(Claim::OnBranch {
                    at: a(),
                    repo: Repo::Lib,
                    branch: "dev",
                })
                .claim(Claim::Presents {
                    at: a(),
                    project: Repo::Proj2,
                }),
            Step::rwv(&a(), &["activate", "proj", "--update"])
                .expect(Exit::Succeeds)
                .claim(Claim::OnBranch {
                    at: a(),
                    repo: Repo::Lib,
                    branch: "main",
                })
                .claim(Claim::Presents {
                    at: a(),
                    project: Repo::Proj,
                }),
        ],
    }
}

/// A fresh landing whose target's workspace presents another project lands
/// under its own: `proj`'s `main` and the members it moves, the shared `lib`
/// among them, which both projects have on `main`; the workspace still
/// presents `proj2`.
pub fn land_into_a_workspace_presenting_another_project() -> Scenario {
    Scenario {
        name: "land into a workspace presenting another project",
        seed: Seed::default().second_project(SecondProject {
            branch: "main",
            also: &[],
        }),
        steps: vec![
            Step::clone_project(Machine::A, Repo::Proj2).expect(Exit::Succeeds),
            Step::rwv(&a(), &["activate", "proj2"]).expect(Exit::Succeeds),
            create("w"),
            Step::commit(&ww("w"), Repo::App, "w-app"),
            Step::commit(&ww("w"), Repo::Lib, "w-lib"),
            Step::rwv(&ww("w"), &["land", "--retire"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputLacks("did not finish aligning"))
                .claim(Claim::PinContains {
                    site: main_lock(),
                    member: Repo::App,
                    mark: "w-app",
                })
                .claim(Claim::PinContains {
                    site: main_lock(),
                    member: Repo::Lib,
                    mark: "w-lib",
                })
                .claim(Claim::Contains {
                    at: a(),
                    repo: Repo::Lib,
                    rev: "HEAD",
                    mark: "w-lib",
                })
                .claim(Claim::Presents {
                    at: a(),
                    project: Repo::Proj2,
                }),
        ],
    }
}

/// `main` lists `docs`, which another machine added and this machine took in
/// with git alone, so it has no clone of it, and `docs`' remote does not
/// answer. Composing the workspace onto `main` cannot make `docs`' clone: the
/// member's line names the failure, the op parks, and `--json` records it as
/// `materialize-failed`. Once the remote answers, `land --continue` clones it
/// and the landing finishes.
pub fn land_names_a_member_composing_cannot_materialize() -> Scenario {
    scenario(
        "land names a member composing cannot materialize",
        vec![
            Step::rwv(&a(), &["member", "remove", DOCS, "--delete"]).expect(Exit::Succeeds),
            Step::git(&a(), Repo::Proj, &["push", "origin", "main"])
                .pushes(Remote::origin(Repo::Proj), "main"),
            create("w"),
            Step::new_machine(Machine::B),
            Step::rwv(&b(), &["member", "add", "url:origin:docs"]).expect(Exit::Succeeds),
            Step::git(&b(), Repo::Proj, &["push", "origin", "main"])
                .pushes(Remote::origin(Repo::Proj), "main"),
            Step::rwv(&a(), &["fetch"]).expect(Exit::Succeeds),
            Step::git(&a(), Repo::Proj, &["merge", "--ff-only", "origin/main"])
                .may_leave_unresolved(&[Repo::Docs]),
            Step::commit(&ww("w"), Repo::App, "w-app"),
            Step::unreachable(Remote::origin(Repo::Docs)),
            Step::rwv(&ww("w"), &["land", "--history", "linear"])
                .expect(Exit::refuses_kind(RefusalKind::OpParked))
                .may_leave_unresolved(&[Repo::Docs])
                .claim(Claim::OutputContains(
                    "  local/remotes/docs: materialize failed:",
                )),
            Step::rwv(&ww("w"), &["abort"]).expect(Exit::Succeeds),
            Step::rwv(&ww("w"), &["land", "--history", "linear", "--json"])
                .expect(Exit::refuses_kind(RefusalKind::OpParked))
                .may_leave_unresolved(&[Repo::Docs])
                .claim(Claim::OutputContains("\"kind\": \"materialize-failed\"")),
            Step::reachable(Remote::origin(Repo::Docs)),
            Step::rwv(&ww("w"), &["land", "--continue"])
                .expect(Exit::Succeeds)
                .claim(Claim::Contains {
                    at: a(),
                    repo: Repo::App,
                    rev: "main",
                    mark: "w-app",
                })
                .claim(Claim::OnBranch {
                    at: a(),
                    repo: Repo::Docs,
                    branch: "main",
                }),
        ],
    )
}

/// `main`'s upstream lists `docs`, which another machine added; this machine
/// only fetched it, so neither its checked-out lock nor its disk has `docs`.
/// The landing takes the upstream in, and composing the workspace onto it
/// makes the canonical clone `rwv update` in a workweave would have made, so
/// the landing finishes.
pub fn land_clones_a_member_its_target_took_in_by_fetch_alone() -> Scenario {
    scenario(
        "land clones a member its target took in by fetch alone",
        vec![
            Step::rwv(&a(), &["member", "remove", DOCS, "--delete"]).expect(Exit::Succeeds),
            Step::git(&a(), Repo::Proj, &["push", "origin", "main"])
                .pushes(Remote::origin(Repo::Proj), "main"),
            create("w"),
            Step::new_machine(Machine::B),
            Step::rwv(&b(), &["member", "add", "url:origin:docs"]).expect(Exit::Succeeds),
            Step::git(&b(), Repo::Proj, &["push", "origin", "main"])
                .pushes(Remote::origin(Repo::Proj), "main"),
            Step::rwv(&a(), &["fetch"]).expect(Exit::Succeeds),
            Step::commit(&ww("w"), Repo::App, "w-app"),
            Step::rwv(&ww("w"), &["land", "--history", "linear"])
                .expect(Exit::Succeeds)
                .claim(Claim::Contains {
                    at: a(),
                    repo: Repo::App,
                    rev: "main",
                    mark: "w-app",
                })
                .claim(Claim::OnBranch {
                    at: a(),
                    repo: Repo::Docs,
                    branch: "main",
                })
                .claim(Claim::AtPin {
                    at: a(),
                    repo: Repo::Docs,
                }),
        ],
    )
}

/// A landing moved `lib-next` as a ref and was killed after the target's
/// workspace aligned onto it; someone switches that checkout back to `main`
/// and deletes `lib-next`. Abort cannot put back a branch that is gone, so it
/// refuses, and the choice it prints clears that state when run.
pub fn abort_over_a_receiving_branch_deleted_after_the_landing_moved_it(
    stop: LandedStop,
) -> Scenario {
    let mut steps = receiving_branch_deleted_after_the_landing_moved_it(stop);
    steps.extend([
        Step::rwv_printed_holding(&ww("w"), "rwv abort ", "--leave-target")
            .expect(Exit::Succeeds)
            .may_strand(&[Repo::Proj])
            .may_rewind(&[Repo::Lib])
            .may_restore(&[Repo::Proj, Repo::Lib])
            .claim(Claim::OutputContains("is left as it stands, gone")),
        Step::git(
            &a(),
            Repo::Lib,
            &["for-each-ref", "--format=%(refname)", "refs/rwv/pre-op/"],
        )
        .claim(Claim::OutputLacks("pre-op")),
        Step::git(&a(), Repo::Lib, &["log", "--all", "--format=%s"])
            .claim(Claim::OutputContains("dev-own")),
        Step::rwv(&ww("w"), &["land", "--continue"])
            .expect(Exit::refuses_kind(RefusalKind::NoOpRecorded)),
    ]);
    scenario(
        "abort over a receiving branch deleted after the landing moved it",
        steps,
    )
}

/// The same state; the printed line that puts `lib-next` back where the
/// landing left it, run verbatim, lets the plain abort restore everything,
/// `lib-next` back at its own tip, and the landing then lands again.
pub fn abort_puts_back_a_receiving_branch_deleted_after_the_landing_moved_it(
    stop: LandedStop,
) -> Scenario {
    let mut steps = receiving_branch_deleted_after_the_landing_moved_it(stop);
    steps.extend([
        Step::rwv_printed_holding(&ww("w"), "git -C ", " branch lib-next ").expect(Exit::Succeeds),
        Step::rwv(&ww("w"), &["abort"])
            .expect(Exit::Succeeds)
            .may_strand(&[Repo::Proj])
            .may_rewind(&[Repo::Lib])
            .may_restore(&[Repo::Proj, Repo::Lib]),
        Step::git(&a(), Repo::Lib, &["log", "-1", "--format=%s", "lib-next"])
            .claim(Claim::OutputLine("dev-own")),
        Step::rwv(
            &ww("w"),
            &[
                "land",
                "--history",
                "linear",
                "--retire",
                "--allow-member-change",
                LIB,
            ],
        )
        .expect(Exit::Succeeds)
        .claim(Claim::WorkweaveGone(ww("w"))),
    ]);
    scenario(
        "abort puts back a receiving branch deleted after the landing moved it",
        steps,
    )
}

/// A landing that moved `lib-next` as a ref, killed at `stop`, its target's
/// `lib` switched back to `main` and `lib-next` deleted; the plain abort
/// refuses, printing what puts the branch back.
fn receiving_branch_deleted_after_the_landing_moved_it(stop: LandedStop) -> Vec<Step> {
    vec![
        create("w"),
        Step::git(&a(), Repo::Lib, &["switch", "-q", "-c", "lib-next"]),
        Step::commit(&a(), Repo::Lib, "dev-own"),
        Step::git(&a(), Repo::Lib, &["switch", "-q", "main"]),
        Step::hand_edit(
            &ww("w"),
            Repo::Proj,
            "rwv.toml",
            &declared_branch_text(Repo::Lib, "main"),
            &declared_branch_text(Repo::Lib, "lib-next"),
        ),
        Step::git(
            &ww("w"),
            Repo::Proj,
            &["commit", "-qam", "lib onto lib-next"],
        ),
        Step::commit(&ww("w"), Repo::Lib, "w-lib"),
        Step::rwv_paused(
            &ww("w"),
            &[
                "land",
                "--history",
                "linear",
                "--retire",
                "--allow-member-change",
                LIB,
            ],
            stop.point(),
            "landing",
        ),
        Step::kill("landing"),
        Step::git(&a(), Repo::Lib, &["switch", "-q", "main"]),
        Step::git(&a(), Repo::Lib, &["branch", "-D", "lib-next"]).may_strand(&[Repo::Lib]),
        abort_refused_whole(
            Step::rwv(&ww("w"), &["abort"]).expect(Exit::refuses_kind(RefusalKind::ForeignTip)),
            true,
            &[a(), ww("w")],
        ),
    ]
}

/// What keeps abort from switching the target's `lib` back onto `main`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwitchBackBlocked {
    /// `main` was deleted.
    Gone,
    /// `main` is checked out in another worktree.
    Held,
}

/// The target's `lib` aligned onto `lib-next` and the landing was killed;
/// then `main`, the branch abort switches `lib` back to, is deleted or held
/// elsewhere. Abort refuses, printing the command that clears it; run as
/// printed, the next abort switches `lib` back.
pub fn abort_switch_back_blocked(blocked: SwitchBackBlocked) -> Scenario {
    let mut steps = vec![
        create("w"),
        Step::git(&a(), Repo::Lib, &["switch", "-q", "-c", "lib-next"]),
        Step::commit(&a(), Repo::Lib, "dev-own"),
        Step::git(&a(), Repo::Lib, &["switch", "-q", "main"]),
        Step::hand_edit(
            &ww("w"),
            Repo::Proj,
            "rwv.toml",
            &declared_branch_text(Repo::Lib, "main"),
            &declared_branch_text(Repo::Lib, "lib-next"),
        ),
        Step::git(
            &ww("w"),
            Repo::Proj,
            &["commit", "-qam", "lib onto lib-next"],
        ),
        Step::commit(&ww("w"), Repo::Lib, "w-lib"),
        Step::rwv_paused(
            &ww("w"),
            &[
                "land",
                "--history",
                "linear",
                "--retire",
                "--allow-member-change",
                LIB,
            ],
            "landed",
            "landing",
        ),
        Step::kill("landing"),
    ];
    let (block, why, choice) = match blocked {
        SwitchBackBlocked::Gone => (
            Step::git(&a(), Repo::Lib, &["branch", "-q", "-D", "main"]).may_strand(&[Repo::Lib]),
            "cannot be switched back: it is gone",
            "branch main ",
        ),
        SwitchBackBlocked::Held => (
            Step::git(
                &a(),
                Repo::Lib,
                &["worktree", "add", "-q", "../lib-held", "main"],
            ),
            "cannot be switched back: it is checked out at",
            "switch --detach",
        ),
    };
    steps.extend([
        block,
        abort_refused_whole(
            Step::rwv(&ww("w"), &["abort"])
                .expect(Exit::refuses_kind(RefusalKind::ForeignTip))
                .claim(Claim::OutputContains(why)),
            true,
            &[a(), ww("w")],
        ),
        Step::rwv_printed_holding(&ww("w"), "git -C ", choice).expect(Exit::Succeeds),
        Step::rwv(&ww("w"), &["abort"])
            .expect(Exit::Succeeds)
            .may_rewind(&[Repo::Lib])
            .may_restore(&[Repo::Proj, Repo::Lib])
            .claim(Claim::OnBranch {
                at: a(),
                repo: Repo::Lib,
                branch: "main",
            }),
    ]);
    scenario("abort switch-back blocked", steps)
}

/// The target's `lib` aligned onto `lib-next`, which the landing moved as a
/// ref, and the landing was killed; someone edits a file `lib-next` holds
/// and `main` lacks. Abort, run from the source, does not discard an edit
/// in the target's workspace to switch the checkout back to `main`: it
/// refuses, leaving the checkout where it is.
fn abort_refused_over_colliding_edits(stop: LandedStop, recorded: bool) -> Vec<Step> {
    let landing = [
        "land",
        "--history",
        "linear",
        "--retire",
        "--allow-member-change",
        LIB,
    ];
    let stopped = if recorded && stop == LandedStop::Retire {
        retire_refused_on_dirt(&landing)
    } else {
        vec![
            Step::rwv_paused(&ww("w"), &landing, stop.point(), "landing"),
            Step::kill("landing"),
        ]
    };
    let mut steps = vec![
        create("w"),
        Step::git(&a(), Repo::Lib, &["switch", "-q", "-c", "lib-next"]),
        Step::commit(&a(), Repo::Lib, "dev-own"),
        Step::git(&a(), Repo::Lib, &["switch", "-q", "main"]),
        Step::hand_edit(
            &ww("w"),
            Repo::Proj,
            "rwv.toml",
            &declared_branch_text(Repo::Lib, "main"),
            &declared_branch_text(Repo::Lib, "lib-next"),
        ),
        Step::git(
            &ww("w"),
            Repo::Proj,
            &["commit", "-qam", "lib onto lib-next"],
        ),
        Step::commit(&ww("w"), Repo::Lib, "w-lib"),
    ];
    steps.extend(stopped);
    steps.extend([
        Step::hand_edit(
            &a(),
            Repo::Lib,
            "w-lib.txt",
            "w-lib",
            "edited in the target",
        ),
        abort_refused_whole(
            Step::rwv(&ww("w"), &["abort"])
                .expect(Exit::refuses_kind(RefusalKind::ForeignTip))
                .claim(Claim::OutputContains(
                    "[target] local/remotes/lib: holds uncommitted edits",
                ))
                .claim(Claim::OnBranch {
                    at: a(),
                    repo: Repo::Lib,
                    branch: "lib-next",
                }),
            true,
            &[a(), ww("w")],
        ),
    ]);
    steps
}

/// The edits keep abort from switching the checkout back
/// ([`abort_refused_over_colliding_edits`]); the printed command sets them
/// aside, and the rerun restores everything.
pub fn abort_refuses_a_switch_back_over_colliding_edits(stop: LandedStop) -> Scenario {
    let mut steps = abort_refused_over_colliding_edits(stop, false);
    steps.extend([
        Step::rwv_printed(&ww("w"), "git -C ").expect(Exit::Succeeds),
        Step::rwv(&ww("w"), &["abort"])
            .expect(Exit::Succeeds)
            .may_strand(&[Repo::Proj, Repo::Lib])
            .may_rewind(&[Repo::Lib])
            .may_restore(&[Repo::Proj, Repo::Lib])
            .claim(Claim::OnBranch {
                at: a(),
                repo: Repo::Lib,
                branch: "main",
            })
            .claim(Claim::AtPin {
                at: a(),
                repo: Repo::Lib,
            }),
        Step::git(&a(), Repo::Lib, &["stash", "list"]).claim(Claim::OutputContains("lib-next")),
        Step::rwv(
            &ww("w"),
            &[
                "land",
                "--history",
                "linear",
                "--retire",
                "--allow-member-change",
                LIB,
            ],
        )
        .expect(Exit::Succeeds),
    ]);
    scenario("abort refuses a switch-back over colliding edits", steps)
}

/// The edits keep abort from switching the checkout back
/// ([`abort_refused_over_colliding_edits`]); run in the target, abort
/// refuses with its own choices there, and the printed
/// `--abandon-foreign-tip` keeps them as a backup, then restores. Stopped in
/// its retire, the retire refused, so abort in the target takes it up.
pub fn abort_abandons_colliding_edits_keeping_them(stop: LandedStop) -> Scenario {
    let mut steps = abort_refused_over_colliding_edits(stop, true);
    steps.extend([
        abort_refused_whole(
            Step::rwv(&a(), &["abort"])
                .expect(Exit::refuses_kind(RefusalKind::ForeignTip))
                .claim(Claim::OutputContains(
                    "local/remotes/lib: is off where its savepoint",
                )),
            true,
            &[a(), ww("w")],
        ),
        Step::rwv_printed(&a(), "rwv abort --abandon-foreign-tip")
            .expect(Exit::Succeeds)
            .may_strand(&[Repo::Proj, Repo::Lib])
            .may_rewind(&[Repo::Lib])
            .may_restore(&[Repo::Proj, Repo::Lib])
            .claim(Claim::OutputContains(
                "local/remotes/lib: its uncommitted edits are kept as backup",
            ))
            .claim(Claim::OnBranch {
                at: a(),
                repo: Repo::Lib,
                branch: "main",
            }),
        Step::git(
            &a(),
            Repo::Lib,
            &["log", "--all", "--format=kept %s", "-Sedited in the target"],
        )
        .claim(Claim::OutputContains("kept ")),
    ]);
    scenario("abort abandons colliding edits keeping them", steps)
}

/// A landing killed right after it fast-forwarded `app` in the target's
/// workspace: the move was journaled before it was made, so abort, run from
/// the source, restores it rather than leaving it as someone else's. Naming
/// `lib`, which the landing never reached, abandons nothing of it, and the
/// abort leaves no savepoint behind in the target.
pub fn abort_restores_a_target_fast_forward_killed_after_the_move() -> Scenario {
    scenario(
        "abort restores a target fast-forward killed after the move",
        vec![
            create("w"),
            Step::commit(&ww("w"), Repo::App, "w-app"),
            Step::rwv_paused(&ww("w"), &["land"], "target-fast-forwarded", "landing"),
            Step::kill("landing"),
            Step::git(&a(), Repo::App, &["log", "-1", "--format=%s"])
                .claim(Claim::OutputLine("w-app")),
            Step::rwv(&ww("w"), &["abort", "--abandon-foreign-tip", LIB])
                .expect(Exit::Succeeds)
                .may_strand(&[Repo::Proj])
                .may_rewind(&[Repo::App])
                .may_restore(&[Repo::App, Repo::Proj])
                .claim(Claim::OutputContains(
                    "[target] local/remotes/app: restored",
                ))
                .claim(Claim::OutputContains(
                    "[target] local/remotes/lib: --abandon-foreign-tip names it, but this \
                     landing never moved it",
                )),
            Step::git(&a(), Repo::App, &["log", "-1", "--format=%s"])
                .claim(Claim::OutputLacks("w-app")),
            Step::git(&a(), Repo::Lib, &["for-each-ref", "refs/rwv/pre-op/"])
                .claim(Claim::OutputLacks("pre-op")),
            Step::git(&a(), Repo::App, &["for-each-ref", "refs/rwv/pre-op/"])
                .claim(Claim::OutputLacks("pre-op")),
            Step::rwv(&ww("w"), &["land"]).expect(Exit::Succeeds),
        ],
    )
}

/// A landing killed right after it fast-forwarded `app` in the target's
/// workspace is continued; between the continued landing's read of the
/// target and its fast-forward of `app`, already done, someone commits onto
/// `app`. The landing stops, `app` stays on its journal, so abort refuses to
/// leave the landing's commits under the new one as someone else's work, and
/// the printed `--abandon-foreign-tip` keeps that commit as a backup.
pub fn a_continued_landing_racing_a_commit_on_a_repo_it_moved() -> Scenario {
    scenario(
        "a continued landing racing a commit on a repo it moved",
        vec![
            create("w"),
            Step::commit(&ww("w"), Repo::App, "w-app"),
            Step::rwv_paused(&ww("w"), &["land"], "target-fast-forwarded", "landing"),
            Step::kill("landing"),
            Step::rwv_paused(
                &ww("w"),
                &["land", "--continue"],
                "advance-target",
                "continued",
            ),
            Step::commit(&a(), Repo::App, "again"),
            Step::release("continued")
                .expect(Exit::refuses(Repo::App, State::TargetMovedMidOperation)),
            abort_refused_whole(
                Step::rwv(&ww("w"), &["abort"])
                    .expect(Exit::refuses_kind(RefusalKind::ForeignTip))
                    .claim(Claim::OutputContains("[target] local/remotes/app")),
                true,
                &[a(), ww("w")],
            ),
            Step::rwv_printed(&ww("w"), "rwv abort --abandon-foreign-tip")
                .expect(Exit::Succeeds)
                .may_rewind(&[Repo::App])
                .may_restore(&[Repo::App])
                .claim(Claim::OutputContains("abandoned foreign tip")),
            Step::git(&a(), Repo::App, &["log", "--format=%s", "main"])
                .claim(Claim::OutputLacks("w-app"))
                .claim(Claim::OutputLacks("again")),
            Step::git(&a(), Repo::App, &["log", "--all", "--format=kept %s"])
                .claim(Claim::OutputContains("kept again")),
            Step::rwv(&ww("w"), &["land"]).expect(Exit::Succeeds),
        ],
    )
}

/// How a landing that changes `lib`'s declared branch to `lib-next` treats `lib-next`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceivingCase {
    /// `lib-next` exists with work of its own and the feature has no `lib` work:
    /// the feature's `lib` follows `lib-next`, which does not move.
    Followed,
    /// `lib-next` exists with work of its own and the feature has `lib` work:
    /// `lib-next` moves as a ref onto the composed work.
    MovedAsRef,
    /// `lib-next` exists nowhere: the landing creates it at `main`'s pin and
    /// moves it onto the composed work.
    Created,
    /// `lib-next` exists nowhere and the feature has no `lib` work: the
    /// landing creates it at `main`'s pin, where the target's checkout of
    /// `lib` already is, and aligning switches that checkout onto it.
    CreatedWithoutWork,
}

/// The workspace `rwv abort` is run in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbortFrom {
    Source,
    Target,
}

/// A landing that changes `lib`'s declared branch to `lib-next` stops after the
/// target's workspace aligned onto `lib-next`, before it releases the target,
/// and `rwv abort` runs from either workspace: the target's `lib` is switched back onto `main` at its pin,
/// `lib-next` is put back where it was (or deleted, where the landing created
/// it) and never reset to `main`'s pin, and no commit is left on no ref.
pub fn abort_after_the_target_aligned(case: ReceivingCase, from: AbortFrom) -> Scenario {
    target_aligned_then_abort(case, from, false)
}

/// `abort_after_the_target_aligned` from the source, where a first abort is
/// killed once it has restored everything and before it ends the landing:
/// the rerun finds nothing in its way and ends it.
pub fn abort_after_the_target_aligned_rerun_after_a_kill(case: ReceivingCase) -> Scenario {
    target_aligned_then_abort(case, AbortFrom::Source, true)
}

fn target_aligned_then_abort(case: ReceivingCase, from: AbortFrom, killed: bool) -> Scenario {
    let mut steps = vec![create("w")];
    let created = matches!(
        case,
        ReceivingCase::Created | ReceivingCase::CreatedWithoutWork
    );
    let with_work = matches!(case, ReceivingCase::MovedAsRef | ReceivingCase::Created);
    if !created {
        steps.extend([
            Step::git(&a(), Repo::Lib, &["switch", "-q", "-c", "lib-next"]),
            Step::commit(&a(), Repo::Lib, "dev-own"),
            Step::git(&a(), Repo::Lib, &["switch", "-q", "main"]),
        ]);
    }
    steps.extend([
        Step::hand_edit(
            &ww("w"),
            Repo::Proj,
            "rwv.toml",
            &declared_branch_text(Repo::Lib, "main"),
            &declared_branch_text(Repo::Lib, "lib-next"),
        ),
        Step::git(
            &ww("w"),
            Repo::Proj,
            &["commit", "-qam", "lib onto lib-next"],
        ),
    ]);
    if with_work {
        steps.push(Step::commit(&ww("w"), Repo::Lib, "w-lib"));
    }
    steps.extend([
        Step::rwv_paused(
            &ww("w"),
            &[
                "land",
                "--history",
                "linear",
                "--retire",
                "--allow-member-change",
                LIB,
            ],
            "landed",
            "landing",
        ),
        Step::kill("landing"),
        Step::git(&a(), Repo::Lib, &["branch", "--show-current"])
            .claim(Claim::OutputLine("lib-next")),
    ]);
    let aborts_in = match from {
        AbortFrom::Source => ww("w"),
        AbortFrom::Target => a(),
    };
    let restored: &[Repo] = match case {
        ReceivingCase::Followed => &[Repo::Proj],
        ReceivingCase::MovedAsRef | ReceivingCase::Created | ReceivingCase::CreatedWithoutWork => {
            &[Repo::Proj, Repo::Lib]
        }
    };
    if killed {
        steps.extend([
            Step::rwv_paused(&aborts_in, &["abort"], "abort-executed", "aborting")
                .may_rewind(&[Repo::Lib])
                .may_restore(restored),
            Step::kill("aborting"),
        ]);
    }
    steps.push(
        Step::rwv(&aborts_in, &["abort"])
            .expect(Exit::Succeeds)
            .may_rewind(&[Repo::Lib])
            .may_restore(restored)
            .claim(Claim::OnBranch {
                at: a(),
                repo: Repo::Lib,
                branch: "main",
            })
            .claim(Claim::AtPin {
                at: a(),
                repo: Repo::Lib,
            })
            .claim(Claim::OutputLacks("foreign")),
    );
    match case {
        ReceivingCase::Followed | ReceivingCase::MovedAsRef => steps.extend([
            Step::git(&a(), Repo::Lib, &["log", "--format=%s", "lib-next"])
                .claim(Claim::OutputLine("dev-own"))
                .claim(Claim::OutputLacks("w-lib")),
            Step::git(&a(), Repo::Lib, &["log", "--format=%s", "-1", "lib-next"])
                .claim(Claim::OutputLine("dev-own")),
        ]),
        ReceivingCase::Created | ReceivingCase::CreatedWithoutWork => steps.push(
            Step::git(&a(), Repo::Lib, &["branch", "--list", "lib-next"])
                .claim(Claim::OutputLacks("lib-next")),
        ),
    }
    if with_work {
        steps.push(
            Step::git(&ww("w"), Repo::Lib, &["log", "--format=%s", "-1"])
                .claim(Claim::OutputLine("w-lib")),
        );
    }
    steps.push(
        Step::rwv(
            &ww("w"),
            &[
                "land",
                "--history",
                "linear",
                "--retire",
                "--allow-member-change",
                LIB,
            ],
        )
        .expect(Exit::Succeeds)
        .claim(Claim::WorkweaveGone(ww("w")))
        .claim(Claim::OnBranch {
            at: a(),
            repo: Repo::Lib,
            branch: "lib-next",
        }),
    );
    if !created {
        steps.push(
            Step::git(&a(), Repo::Lib, &["log", "--format=%s", "lib-next"])
                .claim(Claim::OutputLine("dev-own")),
        );
    }
    scenario(
        "abort after the target aligned onto a new receiving branch",
        steps,
    )
}

/// A landing killed in its retire has moved the target and released it.
/// `rwv abort`, typed in the target's workspace, finds the landing by the
/// record that names that workspace, but nothing shows its retire stopped,
/// so it refuses and prints the abort to run in the landing's own
/// workspace; that one takes the claim and the lease back and restores both
/// sides, and the landing run again lands and retires.
pub fn abort_after_the_landing_released_its_target() -> Scenario {
    let landed = |at: Place| Claim::Contains {
        at,
        repo: Repo::App,
        rev: "main",
        mark: "w-app",
    };
    scenario(
        "abort after the landing released its target",
        vec![
            create("w"),
            Step::commit(&ww("w"), Repo::App, "w-app"),
            Step::rwv_paused(&ww("w"), &["land", "--retire"], "retire", "landing"),
            Step::kill("landing"),
            Step::rwv(&a(), &["abort"])
                .expect(Exit::refuses_kind(RefusalKind::AbortInItsWorkspace))
                .claim(Claim::LocalStateUnchanged(a()))
                .claim(Claim::LocalStateUnchanged(ww("w"))),
            Step::rwv_printed(&a(), "rwv -C")
                .expect(Exit::Succeeds)
                .may_strand(&[Repo::Proj])
                .may_rewind(&[Repo::App, Repo::Proj])
                .may_restore(&[Repo::App, Repo::Proj])
                .claim(Claim::OutputContains("[target] ")),
            Step::git(&a(), Repo::App, &["log", "--format=%s", "main"])
                .claim(Claim::OutputLacks("w-app")),
            Step::rwv(&ww("w"), &["status"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputLacks("in progress")),
            Step::rwv(&ww("w"), &["land", "--retire"])
                .expect(Exit::Succeeds)
                .claim(Claim::WorkweaveGone(ww("w")))
                .claim(landed(a())),
        ],
    )
}

/// The member a second landing moves, over a first that moved `app`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltOn {
    SameMember,
    OtherMember,
}

/// A landing stopped in its retire; the operator commits in its workweave,
/// and a second landing then builds on the target. Both sides are foreign:
/// abort refuses before anything of either moves, and prints the one
/// command that ends the operation, both flags naming what each gives up.
/// Run as printed, it ends the landing: the source back at its savepoint,
/// the commit kept as a backup, the target left as the second landing left
/// it, and a later landing lands.
pub fn abort_with_both_sides_foreign() -> Scenario {
    scenario(
        "abort with both sides foreign",
        vec![
            create("w1"),
            create("w2"),
            create("w3"),
            Step::commit(&ww("w1"), Repo::App, "w1-app"),
            Step::commit(&ww("w2"), Repo::Lib, "w2-lib"),
            Step::rwv_paused(
                &ww("w1"),
                &["land", "--history", "linear", "--retire"],
                "retire",
                "landing",
            ),
            Step::kill("landing"),
            Step::commit(&ww("w1"), Repo::App, "w1-after-the-stop"),
            Step::rwv(&ww("w2"), &["land", "--history", "linear", "--retire"])
                .expect(Exit::Succeeds)
                .claim(Claim::WorkweaveGone(ww("w2"))),
            Step::rwv(&ww("w1"), &["abort"])
                .expect(Exit::refuses_kind(RefusalKind::ForeignTip))
                .claim(Claim::OutputLacks("abort stopped there"))
                .claim(Claim::OutputContains(
                    "rwv abort --leave-target --abandon-foreign-tip local/remotes/app",
                ))
                .claim(Claim::OutputLacks("rwv land --continue"))
                .claim(Claim::LocalStateUnchanged(a()))
                .claim(Claim::LocalStateUnchanged(ww("w1"))),
            Step::rwv(&ww("w1"), &["abort", "--leave-target"])
                .expect(Exit::refuses_kind(RefusalKind::ForeignTip))
                .claim(Claim::OutputLacks("abort stopped there"))
                .claim(Claim::OutputContains(
                    "rwv abort --leave-target --abandon-foreign-tip local/remotes/app",
                ))
                .claim(Claim::LocalStateUnchanged(a()))
                .claim(Claim::LocalStateUnchanged(ww("w1"))),
            Step::rwv_printed_holding(&ww("w1"), "rwv abort ", "--abandon-foreign-tip")
                .expect(Exit::Succeeds)
                .may_strand(&[Repo::App, Repo::Proj])
                .may_rewind(&[Repo::App, Repo::Proj])
                .may_restore(&[Repo::App, Repo::Proj])
                .claim(Claim::OutputContains("abandoned")),
            Step::rwv(&ww("w1"), &["status"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputLacks("in progress")),
            Step::git(&a(), Repo::Lib, &["log", "--format=%s", "main"])
                .claim(Claim::OutputContains("w2-lib")),
            Step::commit(&ww("w3"), Repo::App, "w3-app"),
            Step::rwv(&ww("w3"), &["land", "--history", "linear", "--retire"])
                .expect(Exit::Succeeds)
                .claim(Claim::WorkweaveGone(ww("w3"))),
        ],
    )
}

/// How the operator ends a landing whose target checkout was switched off
/// its branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwitchedOffEnd {
    LeaveTarget,
    Abandon,
}

/// A landing stopped in its retire; in the target, `app` is switched onto a
/// new branch and committed on. The landing's branches are all where it
/// left them, but the checkout it would reset is not: abort refuses before
/// anything of either side moves, and each printed choice, run as printed,
/// ends the operation.
pub fn abort_over_a_target_checkout_switched_off_its_branch(end: SwitchedOffEnd) -> Scenario {
    let mut steps = vec![
        create("w"),
        Step::commit(&ww("w"), Repo::App, "w-app"),
        Step::rwv_paused(
            &ww("w"),
            &["land", "--history", "linear", "--retire"],
            "retire",
            "landing",
        ),
        Step::kill("landing"),
        Step::git(&a(), Repo::App, &["switch", "-q", "-c", "hotfix"]),
        Step::commit(&a(), Repo::App, "hotfix-app"),
        Step::rwv(&ww("w"), &["abort"])
            .expect(Exit::refuses_kind(RefusalKind::ForeignTip))
            .claim(Claim::OutputLacks("abort stopped there"))
            .claim(Claim::OutputContains("[target] local/remotes/app"))
            .claim(Claim::OutputContains("rwv abort --leave-target"))
            .claim(Claim::LocalStateUnchanged(a()))
            .claim(Claim::LocalStateUnchanged(ww("w"))),
    ];
    match end {
        SwitchedOffEnd::LeaveTarget => steps.extend([
            Step::rwv_printed_holding(&ww("w"), "rwv abort ", "--leave-target")
                .expect(Exit::Succeeds)
                .may_strand(&[Repo::App, Repo::Proj])
                .may_rewind(&[Repo::App, Repo::Proj])
                .may_restore(&[Repo::App, Repo::Proj]),
            Step::git(&a(), Repo::App, &["log", "--format=%s", "main"])
                .claim(Claim::OutputContains("w-app")),
        ]),
        SwitchedOffEnd::Abandon => steps.extend([
            Step::rwv_printed_holding(&ww("w"), "rwv abort ", "--abandon-foreign-tip")
                .expect(Exit::Succeeds)
                .may_strand(&[Repo::App, Repo::Proj])
                .may_rewind(&[Repo::App, Repo::Proj])
                .may_restore(&[Repo::App, Repo::Proj])
                .claim(Claim::OnBranch {
                    at: a(),
                    repo: Repo::App,
                    branch: "main",
                }),
            Step::git(&a(), Repo::App, &["log", "--format=%s", "main"])
                .claim(Claim::OutputLacks("w-app")),
            Step::git(&a(), Repo::App, &["log", "--format=%s", "hotfix"])
                .claim(Claim::OutputContains("hotfix-app")),
        ]),
    }
    steps.push(
        Step::rwv(&ww("w"), &["status"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputLacks("in progress")),
    );
    scenario(
        "abort over a target checkout switched off its branch",
        steps,
    )
}

/// Where `main`, the target two landings land into, is checked out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetAt {
    /// In the primary: every repo fast-forwards in its checkout.
    Primary,
    /// Nowhere: every repo moves as a ref.
    Nowhere,
    /// The primary detached on the project, its members on `main`: the
    /// project moves as a ref, each member in place in the primary.
    InPlace,
}

/// The printed choice that ends the first landing once its abort refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndedBy {
    LeaveTarget,
    ContinueTheRetire,
}

/// A landing stopped in its retire released its target, and a second
/// landing then built on it. `rwv abort` of the first finds the target at
/// tips it did not write: it refuses before anything of either side moves,
/// since a target is restored whole or not at all, and a third landing into
/// the target still lands. It offers no `--abandon-foreign-tip`, which would
/// give up the project commit pinning what the second landing moved. Each
/// choice it prints then ends the first.
pub fn abort_after_another_landing_built_on_the_target(
    on: BuiltOn,
    at: TargetAt,
    end: EndedBy,
) -> Scenario {
    let second = match on {
        BuiltOn::SameMember => Repo::App,
        BuiltOn::OtherMember => Repo::Lib,
    };
    let mut steps = vec![create("w1"), create("w2"), create("w3"), create("w4")];
    match at {
        TargetAt::Primary => {}
        TargetAt::Nowhere => steps.extend(main_checked_out_nowhere()),
        TargetAt::InPlace => steps.push(Step::git(&a(), Repo::Proj, &["switch", "-q", "--detach"])),
    }
    let in_main = |marks: &[&'static str]| {
        marks.iter().fold(
            Step::git(&a(), Repo::App, &["log", "--format=%s", "main"]),
            |step, mark| step.claim(Claim::OutputContains(mark)),
        )
    };
    steps.extend([
        Step::commit(&ww("w1"), Repo::App, "w1-app"),
        Step::commit(&ww("w2"), second, "w2-change"),
        Step::commit(&ww("w3"), Repo::App, "w3-app"),
        Step::rwv_paused(
            &ww("w1"),
            &["land", "--history", "linear", "--retire"],
            "retire",
            "landing",
        ),
        Step::kill("landing"),
        Step::rwv(&ww("w2"), &["land", "--history", "linear", "--retire"])
            .expect(Exit::Succeeds)
            .claim(Claim::WorkweaveGone(ww("w2"))),
        Step::rwv(&ww("w1"), &["abort"])
            .expect(Exit::refuses_kind(RefusalKind::ForeignTip))
            .claim(Claim::OutputLacks("abort stopped there"))
            .claim(Claim::OutputContains("where this landing left it"))
            .claim(Claim::OutputContains("rwv abort --leave-target"))
            .claim(Claim::OutputLacks("rwv abort --abandon-foreign-tip"))
            .claim(Claim::LocalStateUnchanged(a()))
            .claim(Claim::LocalStateUnchanged(ww("w1"))),
        Step::rwv(&ww("w3"), &["land", "--history", "linear", "--retire"])
            .expect(Exit::Succeeds)
            .claim(Claim::WorkweaveGone(ww("w3"))),
        in_main(&["w3-app"]),
        Step::rwv(&ww("w1"), &["abort"])
            .expect(Exit::refuses_kind(RefusalKind::ForeignTip))
            .claim(Claim::OutputLacks("abort stopped there"))
            .claim(Claim::OutputContains("w3-app"))
            .claim(Claim::LocalStateUnchanged(a()))
            .claim(Claim::LocalStateUnchanged(ww("w1"))),
    ]);
    match end {
        EndedBy::LeaveTarget => steps.extend([
            Step::rwv_printed_holding(&ww("w1"), "rwv abort ", "--leave-target")
                .expect(Exit::Succeeds)
                .may_strand(&[Repo::Proj])
                .may_rewind(&[Repo::App, Repo::Proj])
                .may_restore(&[Repo::App, Repo::Proj]),
            in_main(&["w1-app", "w3-app"]),
            Step::git(&ww("w1"), Repo::App, &["log", "-1", "--format=%s"])
                .claim(Claim::OutputLine("w1-app")),
        ]),
        EndedBy::ContinueTheRetire => steps.extend([
            Step::rwv_printed(&ww("w1"), "rwv land --continue")
                .expect(Exit::Succeeds)
                .claim(Claim::WorkweaveGone(ww("w1"))),
            in_main(&["w1-app", "w3-app"]),
        ]),
    }
    steps.extend([
        Step::commit(&ww("w4"), Repo::App, "w4-app"),
        Step::rwv(&ww("w4"), &["land", "--history", "linear", "--retire"])
            .expect(Exit::Succeeds)
            .claim(Claim::WorkweaveGone(ww("w4"))),
        in_main(&["w4-app"]),
    ]);
    if end != EndedBy::ContinueTheRetire {
        steps.push(
            Step::rwv(&ww("w1"), &["status"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputLacks("in progress")),
        );
    }
    scenario("abort after another landing built on the target", steps)
}

/// A landing stopped in its retire, and the target's `rwv.toml` mid-edit so
/// that it does not parse: abort cannot plan the target's restore, so it
/// restores nothing of either side and names what it could not read; once
/// the file parses, the abort it printed ends the landing.
pub fn abort_refuses_while_the_target_cannot_be_read() -> Scenario {
    let broken = format!("{}\n[[[", declared_branch_text(Repo::Lib, "main"));
    scenario(
        "abort refuses while the target cannot be read",
        vec![
            create("w1"),
            Step::commit(&ww("w1"), Repo::App, "w1-app"),
            Step::rwv_paused(
                &ww("w1"),
                &["land", "--history", "linear", "--retire"],
                "retire",
                "landing",
            ),
            Step::kill("landing"),
            Step::hand_edit(
                &a(),
                Repo::Proj,
                "rwv.toml",
                &declared_branch_text(Repo::Lib, "main"),
                &broken,
            ),
            Step::rwv(&ww("w1"), &["abort"])
                .expect(Exit::refuses_kind(RefusalKind::ForeignTip))
                .claim(Claim::OutputLacks("abort stopped there"))
                .claim(Claim::OutputContains("could not be read"))
                .claim(Claim::OutputContains("rwv abort --leave-target"))
                .claim(Claim::LocalStateUnchanged(a()))
                .claim(Claim::LocalStateUnchanged(ww("w1"))),
            Step::hand_edit(
                &a(),
                Repo::Proj,
                "rwv.toml",
                &broken,
                &declared_branch_text(Repo::Lib, "main"),
            ),
            Step::rwv_printed_exactly(&ww("w1"), "rwv abort")
                .expect(Exit::Succeeds)
                .may_strand(&[Repo::App, Repo::Proj])
                .may_rewind(&[Repo::App, Repo::Proj])
                .may_restore(&[Repo::App, Repo::Proj]),
            Step::git(&a(), Repo::App, &["log", "--format=%s", "main"])
                .claim(Claim::OutputLacks("w1-app")),
            Step::rwv(&ww("w1"), &["status"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputLacks("in progress")),
        ],
    )
}

/// A landing stopped in its retire, an edit in the target's `app`, and
/// `rwv abort --leave-target` that begins and then finds the source's `app`
/// moved under its write: it stops there and gives up nothing of the
/// target, so the command it prints, run as printed, still leaves the
/// target and ends the landing.
pub fn leave_target_gives_up_the_target_only_after_the_source_is_restored() -> Scenario {
    scenario(
        "leave-target gives up the target only after the source is restored",
        vec![
            create("w1"),
            Step::commit(&ww("w1"), Repo::App, "w1-app"),
            Step::rwv_paused(
                &ww("w1"),
                &["land", "--history", "linear", "--retire"],
                "retire",
                "landing",
            ),
            Step::kill("landing"),
            Step::hand_edit(&a(), Repo::App, "README", "app", "edited in the target"),
            Step::rwv_paused(
                &ww("w1"),
                &["abort", "--leave-target"],
                "abort-begun",
                "aborting",
            ),
            Step::commit(&ww("w1"), Repo::App, "w1-late"),
            Step::release("aborting")
                .expect(Exit::refuses_kind(RefusalKind::ForeignTip))
                .claim(Claim::LocalStateUnchanged(a())),
            Step::rwv_printed_holding(&ww("w1"), "rwv abort --leave-target", "")
                .expect(Exit::Succeeds)
                .may_strand(&[Repo::App, Repo::Proj])
                .may_rewind(&[Repo::App, Repo::Proj])
                .may_restore(&[Repo::App, Repo::Proj]),
            Step::git(&a(), Repo::App, &["log", "--format=%s", "main"])
                .claim(Claim::OutputContains("w1-app")),
            Step::rwv(&ww("w1"), &["status"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputLacks("in progress")),
        ],
    )
}

/// A landing stopped in its retire, and an abort that begins and then finds
/// the target's project branch moved under its first write: it stops there,
/// no member is reset beneath the lock that pins it, and the printed
/// `--leave-target` ends the landing.
pub fn abort_stops_at_the_first_write_that_fails() -> Scenario {
    scenario(
        "abort stops at the first write that does not do what its plan named",
        vec![
            create("w1"),
            Step::commit(&ww("w1"), Repo::App, "w1-app"),
            Step::rwv_paused(
                &ww("w1"),
                &["land", "--history", "linear", "--retire"],
                "retire",
                "landing",
            ),
            Step::kill("landing"),
            Step::rwv_paused(&ww("w1"), &["abort"], "abort-begun", "aborting"),
            Step::hand_commit(&a(), Repo::Proj, "main", &[("late.txt", "late\n")]),
            Step::release("aborting")
                .expect(Exit::refuses_kind(RefusalKind::ForeignTip))
                .claim(Claim::LocalStateUnchanged(ww("w1"))),
            Step::git(&a(), Repo::App, &["log", "--format=%s", "main"])
                .claim(Claim::OutputContains("w1-app")),
            Step::rwv(&ww("w1"), &["abort"])
                .expect(Exit::refuses_kind(RefusalKind::ForeignTip))
                .claim(Claim::OutputLacks("abort stopped there"))
                .claim(Claim::OutputContains("rwv abort --leave-target"))
                .claim(Claim::LocalStateUnchanged(a()))
                .claim(Claim::LocalStateUnchanged(ww("w1"))),
            Step::rwv_printed_exactly(&ww("w1"), "rwv abort --leave-target")
                .expect(Exit::Succeeds)
                .may_rewind(&[Repo::App, Repo::Proj])
                .may_restore(&[Repo::App, Repo::Proj]),
            Step::git(&a(), Repo::App, &["log", "--format=%s", "main"])
                .claim(Claim::OutputContains("w1-app")),
        ],
    )
}

/// A landing stopped in its retire, and an abort that has judged its plan
/// and taken the target's claim when a commit lands on the target's `app`:
/// the plan is made again under the claim, so the abort is refused with
/// nothing of either side restored and no abort begun, and the printed
/// `--leave-target` ends the landing.
pub fn abort_judges_again_once_it_holds_the_target() -> Scenario {
    scenario(
        "abort judges again once it holds the target",
        vec![
            create("w1"),
            Step::commit(&ww("w1"), Repo::App, "w1-app"),
            Step::rwv_paused(
                &ww("w1"),
                &["land", "--history", "linear", "--retire"],
                "retire",
                "landing",
            ),
            Step::kill("landing"),
            Step::rwv_paused(&ww("w1"), &["abort"], "abort-retaken", "aborting"),
            Step::commit(&a(), Repo::App, "late-app"),
            abort_refused_whole(
                Step::release("aborting").expect(Exit::refuses_kind(RefusalKind::ForeignTip)),
                true,
                &[a(), ww("w1")],
            ),
            Step::git(&a(), Repo::App, &["log", "--format=%s", "main"])
                .claim(Claim::OutputContains("late-app")),
            Step::rwv_printed_exactly(&ww("w1"), "rwv abort --leave-target")
                .expect(Exit::Succeeds)
                .may_strand(&[Repo::App, Repo::Proj])
                .may_rewind(&[Repo::App, Repo::Proj])
                .may_restore(&[Repo::App, Repo::Proj]),
            Step::git(&a(), Repo::App, &["log", "--format=%s", "main"])
                .claim(Claim::OutputContains("late-app")),
        ],
    )
}

/// `docs`, which `main` lacks, enters it with a landing on `docs-next`,
/// which the landing creates and aligns `main`'s checkout of `docs` onto;
/// the landing stops in its retire and nothing else happens. Abort puts
/// `main` back whole: the checkout back on the branch it was on, and
/// `docs-next` gone.
pub fn abort_of_a_landing_a_member_entered_with() -> Scenario {
    scenario(
        "abort of a landing a member entered with",
        vec![
            Step::rwv(&a(), &["member", "remove", DOCS]).expect(Exit::Succeeds),
            create("w1"),
            Step::rwv(&ww("w1"), &["member", "add", "url:origin:docs"]).expect(Exit::Succeeds),
            Step::hand_edit(
                &ww("w1"),
                Repo::Proj,
                "rwv.toml",
                &declared_branch_text(Repo::Docs, "main"),
                &declared_branch_text(Repo::Docs, "docs-next"),
            ),
            Step::git(
                &ww("w1"),
                Repo::Proj,
                &["commit", "-qam", "docs on docs-next"],
            ),
            Step::rwv(&ww("w1"), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::commit(&ww("w1"), Repo::App, "w1-app"),
            Step::rwv_paused(
                &ww("w1"),
                &["land", "--history", "linear", "--retire"],
                "retire",
                "landing",
            ),
            Step::kill("landing"),
            Step::rwv(&ww("w1"), &["abort"])
                .expect(Exit::Succeeds)
                .may_strand(&[Repo::Proj])
                .may_rewind(&[Repo::App, Repo::Docs, Repo::Proj])
                .may_restore(&[Repo::App, Repo::Docs, Repo::Proj]),
            Step::git(&a(), Repo::Docs, &["branch", "--show-current"])
                .claim(Claim::OutputLine("main")),
            Step::git(&a(), Repo::Docs, &["branch", "--list", "docs-next"])
                .claim(Claim::OutputLacks("docs-next")),
        ],
    )
}

/// `docs`, which `main` lacks, enters the workweave `t` with a landing from
/// `w1`, a workweave of `t`, on `docs-next`, which the landing creates; `t`
/// has no checkout of `docs`, so aligning `t` makes one. The landing stops
/// in its retire. Abort puts `t` back whole: the checkout the landing made
/// is gone with `docs-next`, and a landing into `t` lands again. Where that
/// checkout holds an edit, abort refuses before anything moves, and the
/// printed stash, run verbatim, lets the rerun remove it.
pub fn abort_of_a_landing_whose_alignment_made_a_checkout(edited: bool) -> Scenario {
    let made = ".workweaves/proj--t/local/remotes/docs";
    let mut steps = vec![
        Step::rwv(&a(), &["member", "remove", DOCS]).expect(Exit::Succeeds),
        create("t"),
        Step::rwv(
            &ww("t"),
            &["workweave", "create", "--project", "proj", "w1"],
        )
        .expect(Exit::Succeeds),
        Step::rwv(&ww("w1"), &["member", "add", "url:origin:docs"]).expect(Exit::Succeeds),
        Step::hand_edit(
            &ww("w1"),
            Repo::Proj,
            "rwv.toml",
            &declared_branch_text(Repo::Docs, "main"),
            &declared_branch_text(Repo::Docs, "docs-next"),
        ),
        Step::git(
            &ww("w1"),
            Repo::Proj,
            &["commit", "-qam", "docs on docs-next"],
        ),
        Step::rwv(&ww("w1"), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::commit(&ww("w1"), Repo::App, "w1-app"),
        Step::rwv_paused(
            &ww("w1"),
            &["land", "--history", "linear", "--retire"],
            "retire",
            "landing",
        ),
        Step::kill("landing"),
        Step::git(&ww("t"), Repo::Docs, &["branch", "--show-current"])
            .claim(Claim::OutputLine("proj--t"))
            .claim(Claim::DirStands(Machine::A, made)),
    ];
    if edited {
        steps.extend([
            Step::hand_edit(&ww("t"), Repo::Docs, "README", "docs", "edited in t"),
            Step::rwv(&ww("w1"), &["abort"])
                .expect(Exit::refuses_kind(RefusalKind::ForeignTip))
                .claim(Claim::OutputContains(
                    "the checkout this landing made holds",
                ))
                .claim(Claim::OutputContains("nothing of either side is restored"))
                .claim(Claim::OutputLacks("abort stopped there"))
                .claim(Claim::LocalStateUnchanged(ww("t")))
                .claim(Claim::LocalStateUnchanged(ww("w1"))),
            Step::rwv_printed(&ww("w1"), "git -C ").expect(Exit::Succeeds),
        ]);
    }
    steps.extend([
        Step::rwv(&ww("w1"), &["abort"])
            .expect(Exit::Succeeds)
            .may_strand(&[Repo::Proj])
            .may_rewind(&[Repo::App, Repo::Docs, Repo::Proj])
            .may_restore(&[Repo::App, Repo::Docs, Repo::Proj])
            .claim(Claim::DirGone(Machine::A, made)),
        Step::git(&ww("w1"), Repo::Docs, &["branch", "--list", "docs-next"])
            .claim(Claim::OutputLacks("docs-next")),
        Step::rwv(&ww("t"), &["status"]).expect(Exit::Succeeds),
        Step::rwv(&ww("w1"), &["land", "--history", "linear"]).expect(Exit::Succeeds),
    ]);
    if edited {
        steps.push(
            Step::git(&ww("w1"), Repo::Docs, &["stash", "list"])
                .claim(Claim::OutputContains("stash@")),
        );
    }
    scenario(
        "abort of a landing whose alignment made a checkout in its target",
        steps,
    )
}

/// What happens to `docs-next`, the branch a landing created for `docs` as
/// `docs` entered `main`, once the landing stopped in its retire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnteredThen {
    /// A second landing moves it.
    BuiltOn,
    /// Someone commits onto it in the target's checkout of it.
    HandCommit,
    /// A raw worktree outside both workspaces checks it out.
    CheckedOut,
}

/// The printed choice run once the first landing's abort refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnteredEnd {
    LeaveTarget,
    ContinueTheRetire,
    /// Freeing the checkout and rerunning abort: the choice that puts the
    /// whole target back. Abort prints no `--abandon-foreign-tip` for a
    /// branch it created that moved on, which it would delete.
    Restore,
}

/// `docs`, which `main` lacks, enters it with a landing on `docs-next`, a
/// branch no store has, so the landing creates it; the landing stops in its
/// retire. `then` happens to `docs-next`, and the landing's abort refuses
/// whole before anything moves; the printed `end` clears it and a landing
/// into `main` lands, once a hand commit the target keeps is locked as the
/// landing's refusal prints.
pub fn abort_after_an_entered_branch_moved(then: EnteredThen, end: EnteredEnd) -> Scenario {
    let mut steps = vec![
        Step::rwv(&a(), &["member", "remove", DOCS]).expect(Exit::Succeeds),
        create("w1"),
        create("w4"),
    ];
    if then == EnteredThen::CheckedOut {
        steps.extend(main_checked_out_nowhere());
    }
    steps.extend([
        Step::rwv(&ww("w1"), &["member", "add", "url:origin:docs"]).expect(Exit::Succeeds),
        Step::hand_edit(
            &ww("w1"),
            Repo::Proj,
            "rwv.toml",
            &declared_branch_text(Repo::Docs, "main"),
            &declared_branch_text(Repo::Docs, "docs-next"),
        ),
        Step::git(
            &ww("w1"),
            Repo::Proj,
            &["commit", "-qam", "docs on docs-next"],
        ),
        Step::rwv(&ww("w1"), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::commit(&ww("w1"), Repo::App, "w1-app"),
        Step::rwv_paused(
            &ww("w1"),
            &["land", "--history", "linear", "--retire"],
            "retire",
            "landing",
        ),
        Step::kill("landing"),
    ]);
    match then {
        EnteredThen::BuiltOn => steps.extend([
            create("w2"),
            Step::commit(&ww("w2"), Repo::Docs, "w2-docs"),
            Step::rwv(&ww("w2"), &["land", "--history", "linear", "--retire"])
                .expect(Exit::Succeeds)
                .claim(Claim::WorkweaveGone(ww("w2"))),
        ]),
        EnteredThen::HandCommit => steps.push(Step::commit(&a(), Repo::Docs, "docs-hand")),
        EnteredThen::CheckedOut => steps.push(Step::git(
            &a(),
            Repo::Docs,
            &["worktree", "add", "-q", "../docs-held", "docs-next"],
        )),
    }
    steps.push(
        Step::rwv(&ww("w1"), &["abort"])
            .expect(Exit::refuses_kind(RefusalKind::ForeignTip))
            .claim(Claim::OutputLacks("abort stopped there"))
            .claim(Claim::OutputContains("rwv abort --leave-target"))
            .claim(Claim::OutputLacks("rwv abort --abandon-foreign-tip"))
            .claim(Claim::LocalStateUnchanged(a()))
            .claim(Claim::LocalStateUnchanged(ww("w1"))),
    );
    match end {
        EnteredEnd::LeaveTarget => steps.push(
            Step::rwv_printed_holding(&ww("w1"), "rwv abort ", "--leave-target")
                .expect(Exit::Succeeds)
                .may_strand(&[Repo::Proj])
                .may_rewind(&[Repo::App, Repo::Proj])
                .may_restore(&[Repo::App, Repo::Proj]),
        ),
        EnteredEnd::ContinueTheRetire => steps.push(
            Step::rwv_printed(&ww("w1"), "rwv land --continue")
                .expect(Exit::Succeeds)
                .claim(Claim::WorkweaveGone(ww("w1"))),
        ),
        EnteredEnd::Restore => {
            assert_eq!(
                then,
                EnteredThen::CheckedOut,
                "only a held branch prints a choice that restores the target"
            );
            steps.push(
                Step::rwv_printed_holding(&ww("w1"), "git -C ", "switch --detach")
                    .expect(Exit::Succeeds),
            );
            let restores = Step::rwv(&ww("w1"), &["abort"]);
            steps.push(
                restores
                    .expect(Exit::Succeeds)
                    .may_strand(&[Repo::Proj])
                    .may_rewind(&[Repo::App, Repo::Docs, Repo::Proj])
                    .may_restore(&[Repo::App, Repo::Docs, Repo::Proj]),
            );
            steps.push(
                Step::git(&a(), Repo::Docs, &["branch", "--list", "docs-next"])
                    .claim(Claim::OutputLacks("docs-next")),
            );
        }
    }
    if then == EnteredThen::HandCommit && end != EnteredEnd::Restore {
        steps.push(Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds));
    }
    steps.extend([
        Step::commit(&ww("w4"), Repo::App, "w4-app"),
        Step::rwv(&ww("w4"), &["land", "--history", "linear", "--retire"])
            .expect(Exit::Succeeds)
            .claim(Claim::WorkweaveGone(ww("w4"))),
    ]);
    if end != EnteredEnd::ContinueTheRetire {
        steps.push(
            Step::rwv(&ww("w1"), &["status"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputLacks("in progress")),
        );
    }
    scenario("abort after an entered branch moved", steps)
}

/// A landing stopped in its retire released its target, and a second
/// landing into it was then killed holding it. The first's `rwv abort` must
/// take the target back before it restores anything, so it refuses on the
/// second's claim, naming its exits; the printed abort ends the second, and
/// the first's abort then restores.
pub fn abort_at_retire_waits_for_the_target_back() -> Scenario {
    scenario(
        "abort at retire waits for the target back",
        vec![
            create("w1"),
            create("w2"),
            Step::commit(&ww("w1"), Repo::App, "w1-app"),
            Step::commit(&ww("w2"), Repo::Lib, "w2-lib"),
            Step::rwv_paused(
                &ww("w1"),
                &["land", "--history", "linear", "--retire"],
                "retire",
                "landing-1",
            ),
            Step::kill("landing-1"),
            Step::rwv_paused(
                &ww("w2"),
                &["land", "--history", "linear", "--retire"],
                "composed",
                "landing-2",
            ),
            Step::kill("landing-2"),
            Step::rwv(&ww("w1"), &["abort"])
                .expect(Exit::refuses_kind(RefusalKind::StateClaimHeld))
                .claim(Claim::LocalStateUnchanged(ww("w1")))
                .claim(Claim::LocalStateUnchanged(a())),
            Step::rwv_printed(&ww("w2"), "rwv abort")
                .expect(Exit::Succeeds)
                .may_rewind(&[Repo::App, Repo::Lib, Repo::Proj])
                .may_restore(&[Repo::Lib, Repo::Proj]),
            Step::rwv(&ww("w1"), &["abort"])
                .expect(Exit::Succeeds)
                .may_strand(&[Repo::Proj])
                .may_rewind(&[Repo::App, Repo::Proj])
                .may_restore(&[Repo::App, Repo::Proj]),
            Step::git(&a(), Repo::App, &["log", "--format=%s", "main"])
                .claim(Claim::OutputLacks("w1-app")),
        ],
    )
}

/// `lost-race-abort` met by a second landing: a landing as refs stopped on a
/// lost race after moving `app`'s `main` still holds its claim on `main`, so
/// a second landing into `main` refuses on the claim, naming the stopped
/// landing's exits, rather than building on what the first moved. The
/// printed abort ends the first; the second then lands.
pub fn a_landing_meets_a_landing_stopped_as_refs() -> Scenario {
    let mut steps = vec![create("w1"), create("w3")];
    steps.extend(main_checked_out_nowhere());
    steps.extend(meets_a_stopped_landing(StoppedLanding::AsRefs));
    scenario("a landing meets a landing stopped as refs", steps)
}

/// The same with `main` checked out in the primary: the stopped landing's
/// claim, not its lease, is what the second landing meets.
pub fn a_landing_meets_a_stopped_landing_in_a_checkout() -> Scenario {
    let mut steps = vec![create("w1"), create("w3")];
    steps.extend(meets_a_stopped_landing(StoppedLanding::InCheckout));
    scenario("a landing meets a stopped landing in a checkout", steps)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum StoppedLanding {
    AsRefs,
    InCheckout,
}

fn meets_a_stopped_landing(how: StoppedLanding) -> Vec<Step> {
    let raced = match how {
        StoppedLanding::AsRefs => {
            Step::hand_commit(&a(), Repo::Lib, "main", &[("raced.txt", "raced\n")])
        }
        StoppedLanding::InCheckout => Step::commit(&a(), Repo::Lib, "hand commit"),
    };
    let mut steps = vec![
        Step::commit(&ww("w1"), Repo::App, "w1-app"),
        Step::commit(&ww("w1"), Repo::Lib, "w1-lib"),
        Step::rwv(&ww("w1"), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::rwv_paused(
            &ww("w1"),
            &["land", "--history", "linear"],
            "advance-target",
            "landing",
        ),
        raced,
        Step::release("landing").expect(Exit::refuses(Repo::Lib, State::TargetMovedMidOperation)),
        Step::commit(&ww("w3"), Repo::App, "w3-app"),
        Step::commit(&ww("w3"), Repo::Lib, "w3-lib"),
        Step::rwv(&ww("w3"), &["land", "--history", "linear", "--retire"])
            .expect(Exit::refuses_kind(RefusalKind::StateClaimHeld))
            .claim(Claim::OutputContains("rwv abort"))
            .claim(Claim::LocalStateUnchanged(ww("w3")))
            .claim(Claim::LocalStateUnchanged(a())),
        Step::rwv_printed(&ww("w1"), "rwv abort")
            .expect(Exit::Succeeds)
            .may_rewind(&[Repo::App, Repo::Proj])
            .may_restore(&[Repo::App, Repo::Proj]),
    ];
    match how {
        StoppedLanding::InCheckout => {
            steps.push(Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds))
        }
        StoppedLanding::AsRefs => steps.extend([
            Step::rwv(&ww("w3"), &["land", "--history", "linear", "--retire"])
                .expect(Exit::refuses(Repo::Lib, State::PinUnpinned)),
            Step::rwv_printed(&ww("w3"), "rwv workweave create").expect(Exit::Succeeds),
            Step::rwv(&ww("main"), &["lock", "--commit"]).expect(Exit::Succeeds),
        ]),
    }
    steps.push(
        Step::rwv(&ww("w3"), &["land", "--history", "linear", "--retire"])
            .expect(Exit::Succeeds)
            .claim(Claim::WorkweaveGone(ww("w3"))),
    );
    if how == StoppedLanding::AsRefs {
        steps.push(
            Step::git(&ww("main"), Repo::App, &["log", "--format=%s", "HEAD"])
                .claim(Claim::OutputContains("w3-app"))
                .claim(Claim::OutputLacks("w1-app")),
        );
    }
    steps
}

/// A landing killed once it claimed its target, before it recorded its
/// plan: `rwv abort` ends it and releases the claim, so the next landing
/// into the target lands at once.
pub fn a_landing_killed_before_its_plan_releases_its_claim_on_abort() -> Scenario {
    scenario(
        "a landing killed before its plan releases its claim on abort",
        vec![
            create("w1"),
            create("w2"),
            Step::commit(&ww("w1"), Repo::App, "w1-app"),
            Step::commit(&ww("w2"), Repo::App, "w2-app"),
            Step::rwv_paused(
                &ww("w1"),
                &["land", "--history", "linear", "--retire"],
                "claimed",
                "landing",
            ),
            Step::kill("landing"),
            Step::rwv(&ww("w1"), &["abort"]).expect(Exit::Succeeds),
            Step::rwv(&ww("w2"), &["land", "--history", "linear", "--retire"])
                .expect(Exit::Succeeds)
                .claim(Claim::WorkweaveGone(ww("w2"))),
            Step::rwv(&ww("w1"), &["land", "--history", "linear", "--retire"])
                .expect(Exit::Succeeds)
                .claim(Claim::WorkweaveGone(ww("w1"))),
        ],
    )
}

/// A landing stopped once it landed, and its `rwv abort` refused on a commit
/// made in the source after the stop. The abort puts back only what it took:
/// a landing stopped in its retire had released its target, so another
/// landing into it lands; one stopped before its retire still held it, so
/// another landing refuses on its claim.
pub fn a_refused_abort_puts_back_what_it_took(stop: LandedStop) -> Scenario {
    let second = Step::rwv(&ww("w2"), &["land", "--history", "linear", "--retire"]);
    let second = match stop {
        LandedStop::Retire => second
            .expect(Exit::Succeeds)
            .claim(Claim::WorkweaveGone(ww("w2"))),
        LandedStop::Landed => second
            .expect(Exit::refuses_kind(RefusalKind::StateClaimHeld))
            .claim(Claim::LocalStateUnchanged(ww("w2"))),
    };
    scenario(
        "a refused abort puts back what it took",
        vec![
            create("w1"),
            create("w2"),
            Step::commit(&ww("w1"), Repo::App, "w1-app"),
            Step::commit(&ww("w2"), Repo::Lib, "w2-lib"),
            Step::rwv_paused(
                &ww("w1"),
                &["land", "--history", "linear", "--retire"],
                stop.point(),
                "landing",
            ),
            Step::kill("landing"),
            Step::commit(&ww("w1"), Repo::App, "after-the-stop"),
            abort_refused_whole(
                Step::rwv(&ww("w1"), &["abort"])
                    .expect(Exit::refuses_kind(RefusalKind::ForeignTip)),
                true,
                &[a(), ww("w1")],
            ),
            second,
        ],
    )
}

/// A landing killed once it recorded its retire, before it released its
/// target: its `rwv land --continue` releases the target before it retires,
/// so while the retire refuses a dirty workweave another landing lands.
pub fn a_continued_retire_releases_the_target_first() -> Scenario {
    scenario(
        "a continued retire releases the target first",
        vec![
            create("w1"),
            create("w2"),
            Step::commit(&ww("w1"), Repo::App, "w1-app"),
            Step::commit(&ww("w2"), Repo::Lib, "w2-lib"),
            Step::rwv_paused(
                &ww("w1"),
                &["land", "--history", "linear", "--retire"],
                "retiring",
                "landing",
            ),
            Step::kill("landing"),
            Step::hand_edit(
                &ww("w1"),
                Repo::App,
                "README",
                "app",
                "edited after the stop",
            ),
            Step::rwv(&ww("w1"), &["land", "--continue"])
                .expect(Exit::refuses(Repo::App, State::EditsDirty)),
            Step::rwv(&ww("w2"), &["land", "--history", "linear", "--retire"])
                .expect(Exit::Succeeds)
                .claim(Claim::WorkweaveGone(ww("w2"))),
        ],
    )
}

/// A landing whose retire refused on a dirty workweave has released its
/// target and recorded that its retire stopped: `rwv abort` typed in the
/// target's workspace names that landing before it moves anything, and
/// restores. With two such landings it refuses, printing the abort of each
/// from its own workspace.
pub fn abort_typed_in_the_target_names_the_landing(two: bool) -> Scenario {
    let mut steps = vec![
        create("w1"),
        create("w2"),
        Step::commit(&ww("w1"), Repo::App, "w1-app"),
        Step::commit(&ww("w2"), Repo::Lib, "w2-lib"),
        Step::rwv_paused(
            &ww("w1"),
            &["land", "--history", "linear", "--retire"],
            "retiring",
            "w1-landing",
        ),
        Step::kill("w1-landing"),
        Step::hand_edit(&ww("w1"), Repo::Docs, "README", "docs", "w1 dirt"),
        Step::rwv(&ww("w1"), &["land", "--continue"])
            .expect(Exit::refuses(Repo::Docs, State::EditsDirty))
            .claim(Claim::OutputContains(
                "rolls it back whole, or refuses before anything moves",
            )),
    ];
    if two {
        steps.extend([
            Step::rwv_paused(
                &ww("w2"),
                &["land", "--history", "linear", "--retire"],
                "retiring",
                "w2-landing",
            ),
            Step::kill("w2-landing"),
            Step::hand_edit(&ww("w2"), Repo::Docs, "README", "docs", "w2 dirt"),
            Step::rwv(&ww("w2"), &["land", "--continue"])
                .expect(Exit::refuses(Repo::Docs, State::EditsDirty)),
            Step::rwv(&a(), &["abort"])
                .expect(Exit::refuses_kind(RefusalKind::AbortInItsWorkspace))
                .claim(Claim::OutputContains("proj--w1 abort"))
                .claim(Claim::OutputContains("proj--w2 abort"))
                .claim(Claim::LocalStateUnchanged(a())),
            Step::rwv_printed_holding(&a(), "rwv -C", "proj--w2")
                .expect(Exit::Succeeds)
                .may_strand(&[Repo::Proj])
                .may_rewind(&[Repo::App, Repo::Lib, Repo::Proj])
                .may_restore(&[Repo::App, Repo::Lib, Repo::Proj]),
        ]);
    }
    steps.extend([
        Step::rwv(&a(), &["abort"])
            .expect(Exit::Succeeds)
            .may_strand(&[Repo::Proj])
            .may_rewind(&[Repo::App, Repo::Proj])
            .may_restore(&[Repo::App, Repo::Proj])
            .claim(Claim::OutputContains("aborting the landing from"))
            .claim(Claim::OutputContains("proj--w1")),
        Step::git(&a(), Repo::App, &["log", "--format=%s", "main"])
            .claim(Claim::OutputLacks("w1-app")),
        Step::rwv(&ww("w1"), &["status"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputLacks("in progress")),
    ]);
    scenario("abort typed in the target names the landing", steps)
}

/// A landing paused once its record is written, before it takes its claim:
/// its record already shows the operation, so a claim it takes next is never
/// judged dead; a second landing meanwhile lands, and the first then lands
/// on what it left.
pub fn a_landing_records_its_op_before_it_claims() -> Scenario {
    scenario(
        "a landing records its op before it claims",
        vec![
            create("w1"),
            create("w2"),
            Step::commit(&ww("w1"), Repo::App, "w1-app"),
            Step::commit(&ww("w2"), Repo::Lib, "w2-lib"),
            Step::rwv_paused(
                &ww("w1"),
                &["land", "--history", "linear", "--retire"],
                "recorded",
                "w1-landing",
            ),
            Step::rwv(&ww("w1"), &["status"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains("in progress")),
            Step::rwv(&ww("w2"), &["land", "--history", "linear", "--retire"])
                .expect(Exit::Succeeds)
                .claim(Claim::WorkweaveGone(ww("w2"))),
            Step::release("w1-landing")
                .expect(Exit::Succeeds)
                .claim(Claim::WorkweaveGone(ww("w1"))),
            Step::git(&a(), Repo::Lib, &["log", "--format=%s", "main"])
                .claim(Claim::OutputContains("w2-lib")),
            Step::git(&a(), Repo::App, &["log", "--format=%s", "main"])
                .claim(Claim::OutputContains("w1-app")),
        ],
    )
}

/// An rwv verb that moves, deletes, renames, re-points or publishes `main`
/// or a branch a landing into it lands into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimedMove {
    Switch,
    SwitchDetached,
    Rename,
    SetMode,
    SetBranch,
    Delete,
    Push,
}

impl ClaimedMove {
    pub const ALL: [ClaimedMove; 7] = [
        ClaimedMove::Switch,
        ClaimedMove::SwitchDetached,
        ClaimedMove::Rename,
        ClaimedMove::SetMode,
        ClaimedMove::SetBranch,
        ClaimedMove::Delete,
        ClaimedMove::Push,
    ];

    fn args(self) -> &'static [&'static str] {
        match self {
            ClaimedMove::Switch => &["switch", "rel"],
            ClaimedMove::SwitchDetached => &["switch", "--detach", "HEAD"],
            ClaimedMove::Rename => &["branch", "rename", "trunk"],
            ClaimedMove::SetMode => &["branch", "set-mode", "main", "declared"],
            ClaimedMove::SetBranch => &["member", "set-branch", "local/remotes/lib", "main"],
            ClaimedMove::Delete => &["branch", "delete", "main", "--target", "proj--w2"],
            ClaimedMove::Push => &["push"],
        }
    }
}

/// A landing into `main` stopped holding its claim: an rwv verb that would
/// move `main`, or a branch the landing moves, refuses before it touches
/// anything, naming the landing and the abort that ends it; that abort runs.
pub fn a_verb_refuses_a_branch_a_stopped_landing_claims(verb: ClaimedMove) -> Scenario {
    scenario(
        "a verb refuses a branch a stopped landing claims",
        vec![
            create("w"),
            create("w2"),
            Step::git(&a(), Repo::Proj, &["branch", "rel", "main"]),
            Step::rwv(&a(), &["branch", "set-mode", "rel", "declared"]).expect(Exit::Succeeds),
            Step::commit(&ww("w"), Repo::App, "w-app"),
            Step::rwv_paused(&ww("w"), &["land"], "landed", "landing"),
            Step::kill("landing"),
            Step::rwv(&a(), verb.args())
                .expect(Exit::refuses_kind(RefusalKind::StateClaimHeld))
                .claim(Claim::OutputContains("landing into 'main' holds it"))
                .claim(Claim::LocalStateUnchanged(a()))
                .claim(Claim::LocalStateUnchanged(ww("w2"))),
            Step::rwv_printed(&ww("w"), "rwv abort")
                .expect(Exit::Succeeds)
                .may_strand(&[Repo::Proj])
                .may_rewind(&[Repo::App])
                .may_restore(&[Repo::App, Repo::Proj]),
            Step::git(&a(), Repo::App, &["log", "--format=%s", "main"])
                .claim(Claim::OutputLacks("w-app")),
        ],
    )
}

/// `main` is checked out nowhere, and a landing into it, which moves `app`'s
/// `main` as a ref, is stopped holding its claim. Switching the primary to
/// `rel`, a declared branch whose members sit on `main`, would check `app`'s
/// claimed `main` out: it refuses before anything moves, naming the landing,
/// and once that landing is aborted the switch goes ahead.
pub fn a_switch_refuses_to_align_a_member_onto_a_claimed_branch() -> Scenario {
    let mut steps = vec![create("w")];
    steps.extend(main_checked_out_nowhere());
    steps.extend([
        Step::git(&a(), Repo::Proj, &["branch", "rel", "main"]),
        Step::rwv(&a(), &["branch", "set-mode", "rel", "declared"]).expect(Exit::Succeeds),
        Step::commit(&ww("w"), Repo::App, "w-app"),
        Step::rwv_paused(&ww("w"), &["land"], "composed", "landing"),
        Step::kill("landing"),
        Step::rwv(&ww("w"), &["status"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputContains(
                "rolls it back whole, or refuses before anything moves",
            )),
        Step::rwv(&ww("w"), &["land"])
            .expect(Exit::refuses_kind(RefusalKind::OpInProgress))
            .claim(Claim::OutputContains(
                "rolls it back whole, or refuses before anything moves",
            )),
        Step::rwv(&ww("w"), &["doctor"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputContains(
                "rolls it back whole, or refuses before anything moves",
            )),
        Step::rwv(&a(), &["switch", "rel"])
            .expect(Exit::refuses_kind(RefusalKind::StateClaimHeld))
            .claim(Claim::OutputContains("landing into 'main' holds it"))
            .claim(Claim::OutputContains("proj--w"))
            .claim(Claim::LocalStateUnchanged(a())),
        Step::rwv_printed(&ww("w"), "rwv abort")
            .expect(Exit::Succeeds)
            .may_strand(&[Repo::Proj])
            .may_rewind(&[Repo::App, Repo::Proj])
            .may_restore(&[Repo::App, Repo::Proj]),
        Step::rwv(&a(), &["switch", "rel"])
            .expect(Exit::Succeeds)
            .claim(Claim::OnBranch {
                at: a(),
                repo: Repo::App,
                branch: "main",
            }),
    ]);
    scenario(
        "a switch refuses to align a member onto a claimed branch",
        steps,
    )
}

/// How a landing killed while it waited on its target's claim is ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KilledWaiter {
    Continued,
    Aborted,
}

/// A landing waits on the claim another landing holds, saying whom it waits
/// for and what ends the wait, and is killed while it waits. It leaves a
/// record `rwv status` shows with two choices, and each runs as printed:
/// `rwv land --continue` waits again and lands once the claim is free;
/// `rwv abort` leaves nothing, and the landing run again lands.
pub fn a_landing_killed_while_it_waits(how: KilledWaiter) -> Scenario {
    let mut steps = vec![
        create("w1"),
        create("w2"),
        Step::commit(&ww("w1"), Repo::App, "w1-app"),
        Step::commit(&ww("w2"), Repo::Lib, "w2-lib"),
        Step::rwv_paused(
            &ww("w1"),
            &["land", "--history", "linear", "--retire"],
            "claimed",
            "first",
        ),
        Step::rwv_paused(
            &ww("w2"),
            &["land", "--history", "linear", "--retire"],
            "landing-claim-held",
            "waiter",
        ),
        Step::kill("waiter"),
        Step::rwv(&ww("w2"), &["status"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputContains("rwv land --continue"))
            .claim(Claim::OutputContains("rwv abort")),
    ];
    match how {
        KilledWaiter::Continued => steps.extend([
            Step::rwv(&ww("w2"), &["land", "--continue"])
                .expect(Exit::refuses_kind(RefusalKind::StateClaimHeld))
                .claim(Claim::OutputContains("waiting on 'main'"))
                .claim(Claim::OutputContains("rwv abort")),
            Step::rwv(&ww("w2"), &["status"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputLacks("in progress")),
            Step::release("first").expect(Exit::Succeeds),
            Step::rwv(&ww("w2"), &["land", "--history", "linear", "--retire"])
                .expect(Exit::Succeeds)
                .claim(Claim::WorkweaveGone(ww("w2"))),
        ]),
        KilledWaiter::Aborted => steps.extend([
            Step::rwv(&ww("w2"), &["abort"]).expect(Exit::Succeeds),
            Step::rwv(&ww("w2"), &["status"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputLacks("in progress")),
            Step::release("first").expect(Exit::Succeeds),
            Step::rwv(&ww("w2"), &["land", "--history", "linear", "--retire"])
                .expect(Exit::Succeeds)
                .claim(Claim::WorkweaveGone(ww("w2"))),
        ]),
    }
    scenario("a landing killed while it waits", steps)
}

/// A landing killed while it waits, continued once the claim is free: it
/// lands under its own op.
pub fn a_landing_killed_while_it_waits_continues_once_free() -> Scenario {
    scenario(
        "a landing killed while it waits continues once free",
        vec![
            create("w1"),
            create("w2"),
            Step::commit(&ww("w1"), Repo::App, "w1-app"),
            Step::commit(&ww("w2"), Repo::Lib, "w2-lib"),
            Step::rwv_paused(
                &ww("w1"),
                &["land", "--history", "linear", "--retire"],
                "claimed",
                "first",
            ),
            Step::rwv_paused(
                &ww("w2"),
                &["land", "--history", "linear", "--retire"],
                "landing-claim-held",
                "waiter",
            ),
            Step::kill("waiter"),
            Step::release("first").expect(Exit::Succeeds),
            Step::rwv(&ww("w2"), &["land", "--continue"])
                .expect(Exit::Succeeds)
                .claim(Claim::WorkweaveGone(ww("w2")))
                .claim(Claim::Contains {
                    at: a(),
                    repo: Repo::Lib,
                    rev: "main",
                    mark: "w2-lib",
                }),
        ],
    )
}

/// A landing with no retire killed once it released its target, after
/// another landing built on it: its `rwv land --continue` takes the claim
/// back, and when it is refused it leaves the target released, so a third
/// landing into it lands.
pub fn a_refused_continue_of_a_released_landing_leaves_the_target_free() -> Scenario {
    scenario(
        "a refused continue of a released landing leaves the target free",
        vec![
            create("w1"),
            create("w2"),
            create("w3"),
            Step::commit(&ww("w1"), Repo::App, "w1-app"),
            Step::commit(&ww("w2"), Repo::Lib, "w2-lib"),
            Step::commit(&ww("w3"), Repo::Lib, "w3-lib"),
            Step::rwv_paused(
                &ww("w1"),
                &["land", "--history", "linear"],
                "ending",
                "landing",
            ),
            Step::kill("landing"),
            Step::rwv(&ww("w2"), &["land", "--history", "linear", "--retire"])
                .expect(Exit::Succeeds)
                .claim(Claim::WorkweaveGone(ww("w2"))),
            Step::rwv(&ww("w1"), &["land", "--continue"])
                .expect(Exit::refuses(Repo::Proj, State::TargetMovedMidOperation)),
            Step::rwv(&ww("w3"), &["land", "--history", "linear", "--retire"])
                .expect(Exit::Succeeds)
                .claim(Claim::WorkweaveGone(ww("w3"))),
        ],
    )
}

/// A landing with no retire releases the target's workspace and its claim
/// before it cleans up, lease first: a second landing that runs while the
/// first is held between the two lands, rather than meeting the first one's
/// lease.
pub fn a_landing_ending_has_released_its_target() -> Scenario {
    scenario(
        "a landing ending has released its target",
        vec![
            create("w1"),
            create("w2"),
            Step::commit(&ww("w1"), Repo::App, "w1-app"),
            Step::commit(&ww("w2"), Repo::Lib, "w2-lib"),
            Step::rwv_paused(
                &ww("w1"),
                &["land", "--history", "linear"],
                "ending",
                "landing",
            ),
            Step::rwv(&ww("w2"), &["land", "--history", "linear", "--retire"])
                .expect(Exit::Succeeds)
                .claim(Claim::WorkweaveGone(ww("w2"))),
            Step::release("landing").expect(Exit::Succeeds),
        ],
    )
}

/// The primary leaves every branch of `main`'s: its project repo and members
/// detached, so `main` and each member's `main` are checked out nowhere.
fn main_checked_out_nowhere() -> Vec<Step> {
    [Repo::Proj, Repo::App, Repo::Lib]
        .into_iter()
        .map(|repo| Step::git(&a(), repo, &["switch", "-q", "--detach"]))
        .collect()
}

fn lands_on_main(step: Step, repo: Repo, mark: &'static str) -> Step {
    step.claim(Claim::Contains {
        at: a(),
        repo,
        rev: "main",
        mark,
    })
    .claim(Claim::PinContains {
        site: LockSite::Branch(Store::Clone(Machine::A), Repo::Proj, "main".to_owned()),
        member: repo,
        mark,
    })
}

/// `landing-as-refs`: `main` checked out nowhere. A seat lands into it as
/// refs: `main` and each member's `main` move by compare-and-swap, no
/// checkout moves, and the primary stays detached where it was.
pub fn landing_as_refs() -> Scenario {
    let mut steps = vec![create("w")];
    steps.extend(main_checked_out_nowhere());
    steps.extend([
        Step::commit(&ww("w"), Repo::App, "w-app"),
        Step::commit(&ww("w"), Repo::Lib, "w-lib"),
        lands_on_main(
            lands_on_main(
                Step::rwv(&ww("w"), &["land", "--retire"]),
                Repo::App,
                "w-app",
            ),
            Repo::Lib,
            "w-lib",
        )
        .expect(Exit::Succeeds)
        .claim(Claim::OutputContains("as refs"))
        .claim(Claim::WorkweaveGone(ww("w"))),
        Step::git(&a(), Repo::Proj, &["branch", "--show-current"])
            .claim(Claim::OutputLacks("main")),
    ]);
    scenario("landing as refs", steps)
}

/// `landing-as-refs-unpinned`: `main` checked out nowhere, its `lib` branch
/// holding a raw commit beyond `main`'s pin. A seat landing `lib` work, which
/// would compose onto that commit and pin it into `main` unrecorded, refuses
/// `pin` unpinned for `lib` in `main`, naming the lock in a workspace on
/// `main` and the `workweave create` that opens one; the seat's own unlocked
/// `app` commit is not refused, its relock takes it. Following the printed
/// choices, the landing then lands.
pub fn landing_as_refs_unpinned() -> Scenario {
    let mut steps = vec![create("w")];
    steps.extend(main_checked_out_nowhere());
    steps.extend([
        Step::hand_commit(&a(), Repo::Lib, "main", &[("raw.txt", "raw\n")]),
        Step::commit(&ww("w"), Repo::App, "w-app"),
        Step::commit(&ww("w"), Repo::Lib, "w-lib"),
        Step::rwv(&ww("w"), &["land", "--history", "linear"])
            .expect(Exit::refuses(Repo::Lib, State::PinUnpinned))
            .claim(Claim::OutputContains(
                "rwv workweave create main --branch main",
            ))
            .claim(Claim::OutputContains("rwv lock --commit"))
            .claim(Claim::OutputLacks("app:"))
            .claim(Claim::LocalStateUnchanged(ww("w"))),
        Step::rwv_printed(&ww("w"), "rwv workweave create").expect(Exit::Succeeds),
        Step::rwv(&ww("main"), &["lock", "--commit"]).expect(Exit::Succeeds),
        lands_on_main(
            Step::rwv(&ww("w"), &["land", "--history", "linear", "--retire"]),
            Repo::App,
            "w-app",
        )
        .expect(Exit::Succeeds)
        .claim(Claim::WorkweaveGone(ww("w")))
        .claim(Claim::Contains {
            at: ww("main"),
            repo: Repo::App,
            rev: "HEAD",
            mark: "w-app",
        }),
    ]);
    scenario("landing as refs unpinned", steps)
}

/// `lost-race-abort`: a landing as refs has moved `app`'s `main` when the
/// compare-and-swap of `lib`'s loses to a commit made there meanwhile. It
/// stops with `target` moved mid-operation and the lost-race recovery;
/// `--continue` refuses; `rwv abort` puts `app`'s `main` back from its
/// savepoint and leaves `lib`'s where the race put it; the landing run again
/// lands on the new tips.
pub fn lost_race_abort() -> Scenario {
    let mut steps = vec![create("w")];
    steps.extend(main_checked_out_nowhere());
    steps.extend([
        Step::commit(&ww("w"), Repo::App, "w-app"),
        Step::commit(&ww("w"), Repo::Lib, "w-lib"),
        Step::rwv(&ww("w"), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::rwv_paused(
            &ww("w"),
            &["land", "--history", "linear"],
            "advance-target",
            "landing",
        ),
        Step::hand_commit(&a(), Repo::Lib, "main", &[("raced.txt", "raced\n")]),
        Step::release("landing")
            .expect(Exit::refuses(Repo::Lib, State::TargetMovedMidOperation))
            .claim(Claim::OutputContains("rwv abort"))
            .claim(Claim::Contains {
                at: a(),
                repo: Repo::App,
                rev: "main",
                mark: "w-app",
            }),
        Step::rwv(&ww("w"), &["land", "--continue"])
            .expect(Exit::refuses(Repo::Proj, State::TargetMovedMidOperation)),
        Step::rwv(&ww("w"), &["abort"])
            .expect(Exit::Succeeds)
            .may_rewind(&[Repo::App])
            .may_restore(&[Repo::App])
            .claim(Claim::OutputContains("restored")),
        Step::git(&a(), Repo::App, &["log", "--format=%s", "main"])
            .claim(Claim::OutputLacks("w-app")),
        Step::git(&a(), Repo::Lib, &["log", "--format=%s", "-1", "main"])
            .claim(Claim::OutputLine("hand commit")),
        Step::rwv(&ww("w"), &["land", "--history", "linear", "--retire"])
            .expect(Exit::refuses(Repo::Lib, State::PinUnpinned))
            .claim(Claim::OutputContains("hand commit"))
            .claim(Claim::LocalStateUnchanged(ww("w"))),
        Step::rwv_printed(&ww("w"), "rwv workweave create").expect(Exit::Succeeds),
        Step::rwv(&ww("main"), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::rwv(&ww("w"), &["land", "--history", "linear", "--retire"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputContains("rebased"))
            .claim(Claim::WorkweaveGone(ww("w")))
            .claim(Claim::Contains {
                at: ww("main"),
                repo: Repo::App,
                rev: "HEAD",
                mark: "w-app",
            }),
        Step::git(&a(), Repo::Lib, &["log", "--format=%s", "main"])
            .claim(Claim::OutputContains("w-lib"))
            .claim(Claim::OutputContains("hand commit")),
    ]);
    scenario("lost race abort", steps)
}

/// A landing as refs loses the compare-and-swap of `app`'s `main`, the first
/// receiving branch it moves, to a commit made there meanwhile. It stops at
/// that loss: `lib`'s `main`, which it would have moved next, is left
/// unmoved. `rwv abort` ends it; the commit raced onto `app`'s `main` is
/// one no lock records, so the landing run again refuses `pin` unpinned for
/// it and lands once the printed choices record it.
pub fn lost_race_on_the_first_ref() -> Scenario {
    let mut steps = vec![create("w")];
    steps.extend(main_checked_out_nowhere());
    steps.extend([
        Step::commit(&ww("w"), Repo::App, "w-app"),
        Step::commit(&ww("w"), Repo::Lib, "w-lib"),
        Step::rwv(&ww("w"), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::rwv_paused(
            &ww("w"),
            &["land", "--history", "linear"],
            "advance-target",
            "landing",
        ),
        Step::hand_commit(&a(), Repo::App, "main", &[("raced.txt", "raced\n")]),
        Step::release("landing")
            .expect(Exit::refuses(Repo::App, State::TargetMovedMidOperation))
            .claim(Claim::OutputContains("rwv abort")),
        Step::git(&a(), Repo::Lib, &["log", "--format=%s", "main"])
            .claim(Claim::OutputLacks("w-lib")),
        Step::rwv_printed(&ww("w"), "rwv abort").expect(Exit::Succeeds),
        Step::git(&a(), Repo::App, &["log", "--format=%s", "-1", "main"])
            .claim(Claim::OutputLine("hand commit")),
        Step::rwv(&ww("w"), &["land", "--history", "linear", "--retire"])
            .expect(Exit::refuses(Repo::App, State::PinUnpinned)),
        Step::rwv_printed(&ww("w"), "rwv workweave create").expect(Exit::Succeeds),
        Step::rwv(&ww("main"), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::rwv(&ww("w"), &["land", "--history", "linear", "--retire"])
            .expect(Exit::Succeeds)
            .claim(Claim::WorkweaveGone(ww("w"))),
        Step::git(&ww("main"), Repo::Lib, &["log", "--format=%s", "HEAD"])
            .claim(Claim::OutputContains("w-lib")),
    ]);
    scenario("lost race on the first ref", steps)
}

/// A landing from the primary into a target checked out nowhere switched the
/// primary's project repo to the target, then was killed. `rwv abort` puts the
/// primary back on the feature it landed from and every ref the landing
/// moved back at its savepoint; the landing run again lands.
pub fn abort_after_the_primary_switched_to_the_target() -> Scenario {
    scenario(
        "abort after the primary switched to the target",
        vec![
            Step::rwv(&a(), &["branch", "create", "feat"]).expect(Exit::Succeeds),
            Step::commit(&a(), Repo::App, "feat-app"),
            Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::rwv_paused(&a(), &["land"], "landed", "landing"),
            Step::git(&a(), Repo::Proj, &["branch", "--show-current"])
                .claim(Claim::OutputLine("main")),
            Step::kill("landing"),
            Step::rwv(&a(), &["abort"])
                .expect(Exit::Succeeds)
                .may_rewind(&[Repo::App, Repo::Proj])
                .may_restore(&[Repo::App, Repo::Proj])
                .claim(Claim::OnBranch {
                    at: a(),
                    repo: Repo::Proj,
                    branch: "feat",
                }),
            Step::git(&a(), Repo::App, &["log", "--format=%s", "main"])
                .claim(Claim::OutputLacks("feat-app")),
            lands_on_main(Step::rwv(&a(), &["land"]), Repo::App, "feat-app")
                .expect(Exit::Succeeds)
                .claim(Claim::OnBranch {
                    at: a(),
                    repo: Repo::Proj,
                    branch: "main",
                }),
        ],
    )
}

/// Where `dc05` stops: a landing as refs killed once it composed, before or
/// after its first move.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dc05 {
    /// Killed before any ref moved, then aborted.
    BeforeTheMoveAborted,
    /// Killed as it moves `app`'s `main`, then aborted.
    AtTheMoveAborted,
    /// Killed as it moves `app`'s `main`, then continued.
    AtTheMoveContinued,
    /// Killed once every ref moved, the project's `main` included, before it
    /// released the target, then aborted.
    LandedAborted,
}

/// `dc05-compose-in-source`: a landing as refs killed after it composed in
/// the source. `rwv abort` restores the source and every ref the op moved;
/// `--continue` completes the landing; either way no tip is left that the op
/// record does not name, and the landing then lands.
pub fn dc05_compose_in_source(stop: Dc05) -> Scenario {
    let mut steps = vec![create("w")];
    steps.extend(main_checked_out_nowhere());
    let point = match stop {
        Dc05::BeforeTheMoveAborted => "composed",
        Dc05::AtTheMoveAborted | Dc05::AtTheMoveContinued => "advance-target",
        Dc05::LandedAborted => "landed",
    };
    steps.extend([
        Step::commit(&ww("w"), Repo::App, "w-app"),
        Step::commit(&ww("w"), Repo::Lib, "w-lib"),
        Step::rwv_paused(&ww("w"), &["land"], point, "landing"),
        Step::kill("landing"),
    ]);
    let land = || {
        lands_on_main(
            lands_on_main(
                Step::rwv(&ww("w"), &["land", "--retire"]),
                Repo::App,
                "w-app",
            ),
            Repo::Lib,
            "w-lib",
        )
        .expect(Exit::Succeeds)
        .claim(Claim::WorkweaveGone(ww("w")))
    };
    match stop {
        Dc05::BeforeTheMoveAborted | Dc05::AtTheMoveAborted | Dc05::LandedAborted => {
            steps.extend([
                Step::rwv(&ww("w"), &["abort"])
                    .expect(Exit::Succeeds)
                    .may_rewind(&[Repo::App, Repo::Lib, Repo::Proj])
                    .may_restore(&[Repo::App, Repo::Lib, Repo::Proj]),
                Step::git(&a(), Repo::App, &["log", "--format=%s", "main"])
                    .claim(Claim::OutputLacks("w-app")),
                Step::git(&a(), Repo::Proj, &["log", "--format=%s", "-1", "main"])
                    .claim(Claim::OutputLacks("auto-relock")),
                Step::git(&ww("w"), Repo::App, &["log", "--format=%s", "-1"])
                    .claim(Claim::OutputLine("w-app")),
                Step::rwv(&ww("w"), &["status"])
                    .expect(Exit::Succeeds)
                    .claim(Claim::OutputLacks("in progress")),
                land(),
            ])
        }
        Dc05::AtTheMoveContinued => steps.extend([lands_on_main(
            lands_on_main(
                Step::rwv(&ww("w"), &["land", "--continue"]),
                Repo::App,
                "w-app",
            ),
            Repo::Lib,
            "w-lib",
        )
        .expect(Exit::Succeeds)]),
    }
    scenario("dc05: compose in source", steps)
}

/// Where a landing that moved its first tip is killed before `--continue`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MovedStop {
    /// Killed once it fast-forwarded `app` in the primary's checkout.
    CheckoutMoved,
    /// Killed as refs once every ref moved, the project's `main` included.
    RefsMoved,
}

/// A landing killed after its first move, then a commit in the source's
/// `app`: `--continue` lands what the landing composed, as an uninterrupted
/// landing would, and moves no ref it already moved; the next landing takes
/// the new commit.
pub fn continue_after_the_first_move(stop: MovedStop) -> Scenario {
    let mut steps = vec![create("w")];
    let point = match stop {
        MovedStop::CheckoutMoved => "target-fast-forwarded",
        MovedStop::RefsMoved => {
            steps.extend(main_checked_out_nowhere());
            "landed"
        }
    };
    steps.extend([
        Step::commit(&ww("w"), Repo::App, "w-app"),
        Step::commit(&ww("w"), Repo::Lib, "w-lib"),
        Step::rwv_paused(&ww("w"), &["land"], point, "landing"),
        Step::kill("landing"),
        Step::commit(&ww("w"), Repo::App, "after-stop"),
        Step::commit(&ww("w"), Repo::Lib, "after-stop-lib"),
        lands_on_main(
            lands_on_main(
                Step::rwv(&ww("w"), &["land", "--continue"]),
                Repo::App,
                "w-app",
            ),
            Repo::Lib,
            "w-lib",
        )
        .expect(Exit::Succeeds),
        Step::git(&a(), Repo::App, &["log", "--format=%s", "main"])
            .claim(Claim::OutputLacks("after-stop")),
        Step::git(&a(), Repo::Lib, &["log", "--format=%s", "main"])
            .claim(Claim::OutputLacks("after-stop-lib")),
        lands_on_main(
            Step::rwv(&ww("w"), &["land", "--retire"]),
            Repo::App,
            "after-stop",
        )
        .expect(Exit::Succeeds)
        .claim(Claim::WorkweaveGone(ww("w"))),
    ]);
    scenario("continue after the first move", steps)
}

/// What changed in the target while a landing sat killed once it composed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MovedWhileStopped {
    /// The target's checkout of `app` switched to another branch.
    SwitchedOffItsBranch,
    /// `lib`'s `main`, moved as a ref, deleted.
    RefGone,
    /// `lib`'s `main`, moved as a ref, moved on by another commit.
    RefMovedOn,
}

/// A landing killed once it composed, its target then changed: `--continue`
/// refuses `target` moved mid-operation, naming what moved, and the printed
/// `rwv abort` ends it.
pub fn continue_refuses_a_target_moved_while_stopped(moved: MovedWhileStopped) -> Scenario {
    let mut steps = vec![create("w")];
    if moved != MovedWhileStopped::SwitchedOffItsBranch {
        steps.extend(main_checked_out_nowhere());
    }
    let (change, says) = match moved {
        MovedWhileStopped::SwitchedOffItsBranch => (
            Step::git(&a(), Repo::App, &["switch", "-q", "-c", "other"]),
            "on 'other', not on 'main'",
        ),
        MovedWhileStopped::RefGone => (
            Step::git(&a(), Repo::Lib, &["branch", "-q", "-D", "main"]).may_strand(&[Repo::Lib]),
            "'main' is gone",
        ),
        MovedWhileStopped::RefMovedOn => (
            Step::hand_commit(&a(), Repo::Lib, "main", &[("moved.txt", "moved\n")]),
            "where the landing read it",
        ),
    };
    steps.extend([
        Step::commit(&ww("w"), Repo::App, "w-app"),
        Step::commit(&ww("w"), Repo::Lib, "w-lib"),
        Step::rwv_paused(&ww("w"), &["land"], "composed", "landing"),
        Step::kill("landing"),
        change,
        Step::rwv(&ww("w"), &["land", "--continue"])
            .expect(Exit::refuses(Repo::Proj, State::TargetMovedMidOperation))
            .claim(Claim::OutputContains(says)),
        Step::rwv_printed_exactly(&ww("w"), "rwv abort")
            .expect(Exit::Succeeds)
            .may_rewind(&[Repo::App, Repo::Lib, Repo::Proj])
            .may_restore(&[Repo::App, Repo::Lib, Repo::Proj]),
    ]);
    scenario("continue refuses a target moved while stopped", steps)
}

/// How a landing that fast-forwards the primary's checkout in place ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InPlaceEnd {
    /// Stopped once landed, then aborted from the source.
    AbortedFromTheSource,
    /// Stopped once landed, then aborted from the primary, whose checkout
    /// the landing moved.
    AbortedFromThePrimary,
    /// The primary's checkout holds an edit: the landing refuses first.
    Dirty,
    /// `main` moved under the primary's checkout from another worktree: the
    /// landing refreshes the drift, then lands.
    Drifted,
    /// Killed once it leased the primary, before it recorded its plan, then
    /// aborted: nothing of it is left in the primary.
    KilledBeforeThePlan,
    /// The primary's checkout is rewound after the landing leased it: the
    /// landing refuses before it files anything, and the commit stays dropped.
    Rewound,
    /// Killed once it released its leases, then continued: the primary is
    /// leased again until the landing ends.
    ContinuedAfterRelease,
}

/// The primary detached on the project, its members still on `main`: a
/// seat's landing moves `main` as a ref for the project and fast-forwards
/// the primary's checkouts of `main` in place. The landing leases the
/// primary while it runs, and `rwv abort` from either side puts the
/// primary's checkouts back; an edit there refuses the landing before
/// anything moves.
pub fn in_place_fast_forward(end: InPlaceEnd) -> Scenario {
    let mut steps = vec![
        create("w"),
        Step::git(&a(), Repo::Proj, &["switch", "-q", "--detach"]),
        Step::commit(&ww("w"), Repo::App, "w-app"),
    ];
    let land = || {
        lands_on_main(
            Step::rwv(&ww("w"), &["land", "--history", "linear", "--retire"]),
            Repo::App,
            "w-app",
        )
        .expect(Exit::Succeeds)
        .claim(Claim::OutputContains("in place in"))
        .claim(Claim::Contains {
            at: a(),
            repo: Repo::App,
            rev: "HEAD",
            mark: "w-app",
        })
        .claim(Claim::WorkweaveGone(ww("w")))
    };
    let left_clean = || {
        [
            Step::rwv(&a(), &["status"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputLacks("in progress")),
            Step::git(&a(), Repo::App, &["for-each-ref", "refs/rwv/pre-op/"])
                .claim(Claim::OutputLacks("-in-place")),
        ]
    };
    let from = match end {
        InPlaceEnd::Dirty => {
            steps.extend([
                Step::hand_edit(&a(), Repo::App, "README", "app", "edited"),
                Step::rwv(&ww("w"), &["land", "--history", "linear"])
                    .expect(Exit::refuses(Repo::App, State::EditsDirty))
                    .claim(Claim::LocalStateUnchanged(a()))
                    .claim(Claim::LocalStateUnchanged(ww("w"))),
                Step::hand_edit(&a(), Repo::App, "README", "edited", "app"),
                land(),
            ]);
            steps.extend(left_clean());
            return scenario("in-place fast-forward: dirty", steps);
        }
        InPlaceEnd::Drifted => {
            steps.extend([
                Step::hand_commit(&ww("w"), Repo::App, "main", &[("moved.txt", "moved\n")]),
                Step::rwv(&ww("w"), &["land", "--history", "linear", "--retire"])
                    .expect(Exit::Succeeds)
                    .claim(Claim::OutputContains("in place in"))
                    .claim(Claim::OutputContains("refreshed drift"))
                    .claim(Claim::OutputContains("moved.txt"))
                    .claim(Claim::OutputLacks("commit your edits"))
                    .claim(Claim::WorkweaveGone(ww("w"))),
                Step::git(&a(), Repo::App, &["log", "--format=%s", "HEAD"])
                    .claim(Claim::OutputContains("w-app")),
                Step::git(&a(), Repo::App, &["status", "--porcelain"])
                    .claim(Claim::OutputLacks("moved.txt")),
            ]);
            return scenario("in-place fast-forward: drifted", steps);
        }
        InPlaceEnd::KilledBeforeThePlan => {
            steps.extend([
                Step::rwv_paused(
                    &ww("w"),
                    &["land", "--history", "linear"],
                    "leased-in-place",
                    "landing",
                ),
                Step::kill("landing"),
                Step::rwv(&ww("w"), &["abort"])
                    .expect(Exit::Succeeds)
                    .may_restore(&[Repo::App, Repo::Proj]),
                Step::rwv(&a(), &["status"])
                    .expect(Exit::Succeeds)
                    .claim(Claim::OutputLacks("in progress")),
                Step::git(&a(), Repo::App, &["for-each-ref", "refs/rwv/pre-op/"])
                    .claim(Claim::OutputLacks("-in-place")),
                land(),
            ]);
            steps.extend(left_clean());
            return scenario("in-place fast-forward: killed before the plan", steps);
        }
        InPlaceEnd::Rewound => {
            steps.extend([
                Step::commit(&a(), Repo::App, "a-app"),
                Step::rwv_paused(
                    &ww("w"),
                    &["land", "--history", "linear"],
                    "leased-in-place",
                    "landing",
                ),
                Step::git(&a(), Repo::App, &["reset", "-q", "--hard", "HEAD~1"])
                    .may_restore(&[Repo::App])
                    .may_strand(&[Repo::App]),
                Step::release("landing")
                    .expect(Exit::refuses(Repo::Proj, State::TargetMovedMidOperation))
                    .claim(Claim::OutputContains("where the landing read it")),
                Step::rwv(&a(), &["status"])
                    .expect(Exit::Succeeds)
                    .claim(Claim::OutputLacks("in progress")),
                Step::git(&a(), Repo::App, &["log", "--format=%s", "main"])
                    .claim(Claim::OutputLacks("a-app")),
                land(),
            ]);
            steps.extend(left_clean());
            return scenario("in-place fast-forward: rewound after the lease", steps);
        }
        InPlaceEnd::ContinuedAfterRelease => {
            steps.extend([
                Step::rwv_paused(
                    &ww("w"),
                    &["land", "--history", "linear"],
                    "ending",
                    "landing",
                ),
                Step::kill("landing"),
                Step::rwv(&a(), &["status"])
                    .expect(Exit::Succeeds)
                    .claim(Claim::OutputLacks("in progress")),
                Step::rwv_paused(&ww("w"), &["land", "--continue"], "advance-target", "again"),
                Step::rwv(&a(), &["status"])
                    .expect(Exit::Succeeds)
                    .claim(Claim::OutputContains("in progress")),
                Step::rwv(&a(), &["lock", "--commit"])
                    .expect(Exit::refuses_kind(RefusalKind::OpInProgress)),
                Step::release("again").expect(Exit::Succeeds),
                Step::rwv(&a(), &["status"])
                    .expect(Exit::Succeeds)
                    .claim(Claim::OutputLacks("in progress")),
            ]);
            return scenario("in-place fast-forward: continued after release", steps);
        }
        InPlaceEnd::AbortedFromTheSource => ww("w"),
        InPlaceEnd::AbortedFromThePrimary => a(),
    };
    steps.extend([
        Step::rwv_paused(
            &ww("w"),
            &["land", "--history", "linear"],
            "landed",
            "landing",
        )
        .claim(Claim::Contains {
            at: a(),
            repo: Repo::App,
            rev: "HEAD",
            mark: "w-app",
        }),
        Step::kill("landing"),
        Step::rwv(&a(), &["status"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputContains("in progress")),
        Step::rwv(&from, &["abort"])
            .expect(Exit::Succeeds)
            .may_rewind(&[Repo::App, Repo::Proj])
            .may_restore(&[Repo::App, Repo::Proj]),
        Step::git(&a(), Repo::App, &["log", "--format=%s", "main"])
            .claim(Claim::OutputLacks("w-app")),
        Step::git(&a(), Repo::App, &["log", "--format=%s", "HEAD"])
            .claim(Claim::OutputLacks("w-app")),
        Step::git(&ww("w"), Repo::App, &["log", "--format=%s", "-1"])
            .claim(Claim::OutputLine("w-app")),
        Step::rwv(&a(), &["status"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputLacks("in progress")),
        land(),
    ]);
    steps.extend(left_clean());
    scenario("in-place fast-forward: aborted", steps)
}

/// A landing stopped once `T`'s workspace aligned, then a commit in that
/// workspace's `docs`, which the landing never moved: `rwv abort` restores
/// what the landing moved and leaves `docs` and its commit as they stand.
pub fn abort_leaves_a_target_member_the_alignment_never_moved() -> Scenario {
    scenario(
        "abort leaves a target member the alignment never moved",
        vec![
            create("w"),
            Step::commit(&ww("w"), Repo::App, "w-app"),
            Step::rwv_paused(
                &ww("w"),
                &["land", "--history", "linear"],
                "landed",
                "landing",
            ),
            Step::kill("landing"),
            Step::commit(&a(), Repo::Docs, "docs-own"),
            Step::rwv(&ww("w"), &["abort"])
                .expect(Exit::Succeeds)
                .may_rewind(&[Repo::App, Repo::Proj])
                .may_restore(&[Repo::App, Repo::Proj]),
            Step::git(&a(), Repo::Docs, &["log", "--format=%s", "-1", "main"])
                .claim(Claim::OutputLine("docs-own")),
            Step::git(&a(), Repo::App, &["log", "--format=%s", "main"])
                .claim(Claim::OutputLacks("w-app")),
        ],
    )
}

/// What happens to a receiving branch the landing moves as a ref while the
/// landing is held at its compare-and-swap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefRace {
    /// Someone commits onto it.
    Advanced,
    /// Someone moves it back by name.
    Rewound,
    /// A checkout takes it.
    CheckedOut,
}

/// A landing that moves `lib-next` as a ref loses its compare-and-swap to
/// `race`: it stops with `target` moved mid-operation, `--continue` refuses,
/// `rwv abort` leaves `lib-next` where the race put it, and the landing run
/// again judges the new tips.
pub fn a_receiving_branch_moved_as_a_ref_loses_its_race(race: RefRace) -> Scenario {
    let mut steps = vec![
        create("w"),
        Step::git(&a(), Repo::Lib, &["switch", "-q", "-c", "lib-next"]),
        Step::commit(&a(), Repo::Lib, "dev-own"),
        Step::commit(&a(), Repo::Lib, "dev-two"),
        Step::git(&a(), Repo::Lib, &["switch", "-q", "main"]),
        Step::hand_edit(
            &ww("w"),
            Repo::Proj,
            "rwv.toml",
            &declared_branch_text(Repo::Lib, "main"),
            &declared_branch_text(Repo::Lib, "lib-next"),
        ),
        Step::git(
            &ww("w"),
            Repo::Proj,
            &["commit", "-qam", "lib onto lib-next"],
        ),
        Step::commit(&ww("w"), Repo::Lib, "w-lib"),
        Step::rwv_paused(
            &ww("w"),
            &["land", "--history", "linear", "--allow-member-change", LIB],
            "advance-target",
            "landing",
        ),
    ];
    steps.push(match race {
        RefRace::Advanced => {
            Step::hand_commit(&a(), Repo::Lib, "lib-next", &[("raced.txt", "raced\n")])
        }
        RefRace::Rewound => Step::git(&a(), Repo::Lib, &["branch", "-f", "lib-next", "lib-next~1"])
            .may_restore(&[Repo::Lib])
            .may_strand(&[Repo::Lib]),
        RefRace::CheckedOut => Step::git(&a(), Repo::Lib, &["switch", "-q", "lib-next"]),
    });
    let held = match race {
        RefRace::CheckedOut => "checked out at",
        RefRace::Advanced | RefRace::Rewound => "does not fast-forward",
    };
    steps.extend([
        Step::release("landing")
            .expect(Exit::refuses(Repo::Lib, State::TargetMovedMidOperation))
            .claim(Claim::OutputContains(held))
            .claim(Claim::OutputContains("rwv abort")),
        Step::git(&a(), Repo::Lib, &["log", "--format=%s", "lib-next"])
            .claim(Claim::OutputLacks("w-lib")),
        Step::rwv(&ww("w"), &["land", "--continue"])
            .expect(Exit::refuses(Repo::Proj, State::TargetMovedMidOperation)),
        Step::rwv(&ww("w"), &["abort"])
            .expect(Exit::Succeeds)
            .may_rewind(&[Repo::Lib]),
        Step::git(&a(), Repo::Lib, &["log", "--format=%s", "lib-next"])
            .claim(Claim::OutputLacks("w-lib")),
        Step::git(&ww("w"), Repo::Lib, &["log", "--format=%s", "-1"])
            .claim(Claim::OutputLine("w-lib")),
    ]);
    if race == RefRace::CheckedOut {
        steps.push(Step::git(&a(), Repo::Lib, &["switch", "-q", "main"]));
    }
    steps.push(
        Step::rwv(
            &ww("w"),
            &["land", "--history", "linear", "--allow-member-change", LIB],
        )
        .expect(Exit::Succeeds)
        .claim(Claim::OnBranch {
            at: a(),
            repo: Repo::Lib,
            branch: "lib-next",
        }),
    );
    steps.push(
        Step::git(&a(), Repo::Lib, &["log", "--format=%s", "lib-next"])
            .claim(Claim::OutputContains("w-lib"))
            .claim(Claim::OutputContains("dev-own")),
    );
    scenario("a receiving branch moved as a ref loses its race", steps)
}

/// Where the target's `main` is held while a landing that composed waits to
/// move it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetHeld {
    /// Checked out nowhere: the landing moves it as a ref.
    AsRef,
    /// Checked out in the target's workspace: the landing fast-forwards it
    /// there.
    InCheckout,
}

/// A commit lands on the target's `main` in `app` after the landing composed
/// and before it moves anything: the landing's own read of the savepoint is
/// what stops it, naming the tip it read, `--continue` refuses, `rwv abort`
/// leaves the commit where it is, and the landing run again lands on it, once
/// a branch left beyond its pin has its commit recorded.
pub fn a_target_moved_before_the_landing_moves_it(held: TargetHeld) -> Scenario {
    let mut steps = vec![create("w")];
    if held == TargetHeld::AsRef {
        steps.extend(main_checked_out_nowhere());
    }
    steps.extend([
        Step::commit(&ww("w"), Repo::App, "w-app"),
        Step::rwv_paused(
            &ww("w"),
            &["land", "--history", "linear"],
            "composed",
            "landing",
        ),
        match held {
            TargetHeld::AsRef => {
                Step::hand_commit(&a(), Repo::App, "main", &[("raced.txt", "raced\n")])
            }
            TargetHeld::InCheckout => Step::commit(&a(), Repo::App, "raced"),
        },
        Step::release("landing")
            .expect(Exit::refuses(Repo::Proj, State::TargetMovedMidOperation))
            .claim(Claim::OutputContains("where the landing read it"))
            .claim(Claim::OutputContains("rwv abort")),
        Step::git(&a(), Repo::App, &["log", "--format=%s", "main"])
            .claim(Claim::OutputLacks("w-app")),
        Step::rwv(&ww("w"), &["land", "--continue"])
            .expect(Exit::refuses(Repo::Proj, State::TargetMovedMidOperation))
            .claim(Claim::OutputContains("where the landing read it")),
        Step::rwv(&ww("w"), &["abort"])
            .expect(Exit::Succeeds)
            .may_rewind(&[Repo::App])
            .claim(Claim::OutputContains(
                "where something other than this landing moved it",
            )),
    ]);
    match held {
        TargetHeld::InCheckout => {
            steps.push(Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds));
        }
        TargetHeld::AsRef => steps.extend([
            Step::rwv(&ww("w"), &["land", "--history", "linear", "--retire"])
                .expect(Exit::refuses(Repo::App, State::PinUnpinned))
                .claim(Claim::OutputContains("hand commit"))
                .claim(Claim::LocalStateUnchanged(ww("w"))),
            Step::rwv_printed(&ww("w"), "rwv workweave create").expect(Exit::Succeeds),
            Step::rwv(&ww("main"), &["lock", "--commit"]).expect(Exit::Succeeds),
        ]),
    }
    steps.extend([
        Step::rwv(&ww("w"), &["land", "--history", "linear", "--retire"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputContains("rebased"))
            .claim(Claim::WorkweaveGone(ww("w"))),
        Step::git(&a(), Repo::App, &["log", "--format=%s", "main"])
            .claim(Claim::OutputContains("w-app"))
            .claim(Claim::OutputContains(match held {
                TargetHeld::AsRef => "hand commit",
                TargetHeld::InCheckout => "raced",
            })),
    ]);
    scenario("a target moved before the landing moves it", steps)
}

/// What happens to `app`'s `main`, which a landing as refs moved, after the
/// landing was killed and before `rwv abort`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MovedRefThen {
    /// Someone commits onto it.
    Committed,
    /// A checkout takes it.
    CheckedOut,
}

/// A landing as refs killed once it moved every ref, before it released the
/// target; then `then` happens to the `main` it moved in `app`. Abort
/// refuses to put that branch back and keeps the op, printing the choice
/// that clears it: for a commit, `--abandon-foreign-tip`, which keeps the
/// new tip as a backup and restores the branch; for a checkout, freeing it
/// and rerunning abort. The landing then lands again.
pub fn abort_over_a_ref_the_landing_moved(then: MovedRefThen) -> Scenario {
    let mut steps = vec![create("w")];
    steps.extend(main_checked_out_nowhere());
    steps.extend([
        Step::commit(&ww("w"), Repo::App, "w-app"),
        Step::rwv_paused(
            &ww("w"),
            &["land", "--history", "linear"],
            "landed",
            "landing",
        ),
        Step::kill("landing"),
    ]);
    let (step, why) = match then {
        MovedRefThen::Committed => (
            Step::hand_commit(&a(), Repo::App, "main", &[("raced.txt", "raced\n")]),
            "where this landing left it",
        ),
        MovedRefThen::CheckedOut => (
            Step::git(&a(), Repo::App, &["switch", "-q", "main"]),
            "free the checkout",
        ),
    };
    steps.extend([
        step,
        Step::rwv(&ww("w"), &["abort"])
            .expect(Exit::refuses_kind(RefusalKind::ForeignTip))
            .claim(Claim::OutputLacks("abort stopped there"))
            .claim(Claim::OutputContains(why))
            .claim(Claim::LocalStateUnchanged(a()))
            .claim(Claim::LocalStateUnchanged(ww("w")))
            .claim(Claim::Contains {
                at: a(),
                repo: Repo::App,
                rev: "main",
                mark: "w-app",
            }),
    ]);
    match then {
        MovedRefThen::Committed => steps.extend([
            Step::rwv_printed(&ww("w"), "rwv abort --abandon-foreign-tip")
                .expect(Exit::Succeeds)
                .may_rewind(&[Repo::App])
                .may_restore(&[Repo::App, Repo::Proj])
                .claim(Claim::OutputContains("abandoned foreign tip")),
            Step::git(&a(), Repo::App, &["log", "--all", "--format=kept %s"])
                .claim(Claim::OutputContains("kept hand commit")),
        ]),
        MovedRefThen::CheckedOut => steps.extend([
            Step::rwv_printed(&ww("w"), "git -C ").expect(Exit::Succeeds),
            Step::git(&a(), Repo::App, &["switch", "-q", "--detach"]),
            Step::rwv(&ww("w"), &["abort"])
                .expect(Exit::Succeeds)
                .may_rewind(&[Repo::App])
                .may_restore(&[Repo::App, Repo::Proj])
                .claim(Claim::OutputContains("'main' restored")),
        ]),
    }
    steps.extend([
        Step::git(&a(), Repo::App, &["log", "--format=%s", "main"])
            .claim(Claim::OutputLacks("w-app"))
            .claim(Claim::OutputLacks("hand commit")),
        lands_on_main(
            Step::rwv(&ww("w"), &["land", "--history", "linear", "--retire"]),
            Repo::App,
            "w-app",
        )
        .expect(Exit::Succeeds)
        .claim(Claim::WorkweaveGone(ww("w"))),
    ]);
    scenario("abort over a ref the landing moved", steps)
}

/// A file `<rev>` adds standing untracked in the project repo refuses the
/// port before anything moves. A port killed once it has recorded that the
/// project copy starts: `--continue` makes the project copy, so the project
/// commit holds `<rev>`'s project files.
pub fn cherry_pick_project_copy_killed() -> Scenario {
    let mut steps = stable_lines();
    steps.extend(port_me_on_main());
    steps.extend(feature_onto_stable());
    steps.extend([
        Step::git(
            &ww("p"),
            Repo::Proj,
            &["checkout", "main", "--", "port-me.txt"],
        ),
        Step::git(
            &ww("p"),
            Repo::Proj,
            &["rm", "-q", "--cached", "port-me.txt"],
        ),
        Step::rwv(&ww("p"), &["cherry-pick", "main"])
            .expect(Exit::refuses_kind(RefusalKind::UntrackedCollision))
            .claim(Claim::OutputContains("port-me.txt"))
            .claim(Claim::LocalStateUnchanged(ww("p"))),
        Step::git(
            &ww("p"),
            Repo::Proj,
            &[
                "stash",
                "push",
                "-q",
                "--include-untracked",
                "--",
                "port-me.txt",
            ],
        ),
        Step::rwv_paused(
            &ww("p"),
            &["cherry-pick", "main"],
            "cherry-pick-project-copy",
            "pick",
        ),
        Step::kill("pick"),
        Step::rwv(&ww("p"), &["cherry-pick", "--continue"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputContains("ported")),
        Step::git(&ww("p"), Repo::Proj, &["show", "HEAD:port-me.txt"])
            .claim(Claim::OutputLine("port-me")),
        Step::git(&ww("p"), Repo::Proj, &["log", "-1", "--format=%B"])
            .claim(Claim::OutputContains("Rwv-Picked-From: ")),
    ]);
    Scenario {
        name: "cherry-pick project copy killed",
        seed: Seed::default().role(Repo::Docs, Role::Dependency),
        steps,
    }
}

/// A port killed inside its op's start, after the project repo's savepoint
/// and before any member's: `--continue` savepoints each member before it
/// copies onto it, so `rwv abort` after a later stop puts the copy back.
pub fn cherry_pick_op_start_killed() -> Scenario {
    let mut steps = conflicting_lines();
    steps.extend([
        Step::rwv_paused(
            &ww("p"),
            &["cherry-pick", "main"],
            "op-savepoint-taken",
            "pick",
        ),
        Step::kill("pick"),
        Step::rwv(&ww("p"), &["cherry-pick", "--continue"])
            .expect(Exit::refuses_kind(RefusalKind::OpParked))
            .claim(Claim::OutputContains("local/remotes/app: copied"))
            .claim(Claim::OutputContains("stopped on a conflict (README)")),
        Step::rwv(&ww("p"), &["abort"])
            .expect(Exit::Succeeds)
            .may_strand(&[Repo::App])
            .claim(Claim::AtPin {
                at: ww("p"),
                repo: Repo::App,
            }),
        Step::git(&ww("p"), Repo::App, &["log", "-1", "--format=%s"])
            .claim(Claim::OutputLacks("main-app")),
    ]);
    scenario("cherry-pick op start killed", steps)
}

/// A feature off `stable` holding its own edit of `app`'s README, and a
/// commit on `main` moving `app` by a conflicting edit of that line and
/// `lib` by `fix-lib`; the pick stops in `app`, `lib` not started.
fn app_conflicting_port() -> Vec<Step> {
    let mut steps = stable_lines();
    steps.extend([
        Step::hand_edit(&a(), Repo::App, "README", "app\n", "app on main\n"),
        Step::git(&a(), Repo::App, &["commit", "-qam", "readme-main"]),
        Step::commit(&a(), Repo::Lib, "fix-lib"),
        Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
    ]);
    steps.extend(feature_onto_stable());
    steps.extend([
        Step::hand_edit(&ww("p"), Repo::App, "README", "app\n", "app in p\n"),
        Step::git(&ww("p"), Repo::App, &["commit", "-qam", "readme-p"]),
        Step::rwv(&ww("p"), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::rwv(&ww("p"), &["cherry-pick", "main"])
            .expect(Exit::refuses_kind(RefusalKind::OpParked))
            .claim(Claim::OutputContains("stopped on a conflict (README)"))
            .claim(Claim::OutputLacks("local/remotes/lib: copied")),
    ]);
    steps
}

/// `--continue` rechecks the members it is to copy onto: one switched to
/// another branch during the stop refuses until it is back, and a commit
/// made by hand in a member not yet started is a foreign tip, never taken
/// for a copy; once it is moved off, the port completes, each commit copied.
pub fn cherry_pick_continue_rechecks_members() -> Scenario {
    let mut steps = app_conflicting_port();
    steps.extend([
        Step::git(&ww("p"), Repo::Lib, &["switch", "-q", "-c", "scratch"]),
        Step::git(&ww("p"), Repo::App, &["checkout", "--theirs", "README"]),
        Step::git(&ww("p"), Repo::App, &["add", "README"]),
        Step::rwv(&ww("p"), &["cherry-pick", "--continue"])
            .expect(Exit::refuses_kind(RefusalKind::HeadNotOnBranch))
            .claim(Claim::OutputContains("scratch"))
            .claim(Claim::LocalStateUnchanged(ww("p"))),
        Step::git(&ww("p"), Repo::Lib, &["switch", "-q", "-"]),
        Step::commit(&ww("p"), Repo::Lib, "hand-x"),
        Step::rwv(&ww("p"), &["cherry-pick", "--continue"])
            .expect(Exit::refuses_kind(RefusalKind::ForeignTip))
            .claim(Claim::OutputContains("local/remotes/lib"))
            .claim(Claim::LocalStateUnchanged(ww("p"))),
        Step::git(&ww("p"), Repo::Lib, &["branch", "keep-x"]),
        Step::git(&ww("p"), Repo::Lib, &["reset", "-q", "--hard", "HEAD~1"]),
        Step::rwv(&ww("p"), &["cherry-pick", "--continue", "--json"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputContains("\"complete\": true"))
            .claim(Claim::OutputContains("\"path\": \"local/remotes/lib\""))
            .claim(Claim::AtPin {
                at: ww("p"),
                repo: Repo::App,
            })
            .claim(Claim::AtPin {
                at: ww("p"),
                repo: Repo::Lib,
            }),
        Step::git(&ww("p"), Repo::Lib, &["log", "-1", "--format=%s"])
            .claim(Claim::OutputLine("fix-lib")),
        Step::git(&ww("p"), Repo::App, &["log", "-1", "--format=%s"])
            .claim(Claim::OutputLine("readme-main")),
    ]);
    scenario("cherry-pick continue rechecks members", steps)
}

/// A commit on `main` editing a project file the feature also edited,
/// renaming another, and changing `lib`'s entry next to the line the
/// feature's `rwv.toml` declares `stable-lib` on: the pick stops on the
/// project file alone, `rwv.toml` left as this branch's.
fn project_conflicting_port() -> Vec<Step> {
    let lib_main = declared_branch_text(Repo::Lib, "main");
    let mut steps = vec![
        Step::commit(&a(), Repo::Proj, "notes"),
        Step::commit(&a(), Repo::Proj, "guide"),
    ];
    steps.extend(stable_lines());
    steps.extend([
        Step::hand_edit(&a(), Repo::Proj, "notes.txt", "notes\n", "notes on main\n"),
        Step::git(&a(), Repo::Proj, &["mv", "guide.txt", "guide.md"]),
        Step::hand_edit(
            &a(),
            Repo::Proj,
            "rwv.toml",
            &lib_main,
            &format!("{lib_main}\nrecurse-submodules = false"),
        ),
        Step::git(&a(), Repo::Proj, &["commit", "-qam", "port-notes"]),
    ]);
    steps.extend(feature_onto_stable());
    steps.extend([
        Step::hand_edit(&ww("p"), Repo::Proj, "notes.txt", "notes\n", "notes in p\n"),
        Step::git(&ww("p"), Repo::Proj, &["commit", "-qam", "notes-p"]),
        Step::rwv(&ww("p"), &["cherry-pick", "main"])
            .expect(Exit::refuses_kind(RefusalKind::OpParked))
            .claim(Claim::OutputContains("stopped on a conflict (notes.txt)"))
            .claim(Claim::OutputContains("rwv abort")),
        Step::git(&ww("p"), Repo::Proj, &["show", ":rwv.toml"])
            .claim(Claim::OutputLacks("recurse-submodules")),
    ]);
    steps
}

/// `cherry-pick-conflict` in the project repo: `--continue` commits the
/// resolution with the renamed file moved, not copied, and this branch's
/// entries.
pub fn cherry_pick_project_conflict() -> Scenario {
    let mut steps = project_conflicting_port();
    steps.extend([
        Step::git(&ww("p"), Repo::Proj, &["checkout", "--theirs", "notes.txt"]),
        Step::git(&ww("p"), Repo::Proj, &["add", "notes.txt"]),
        Step::rwv(&ww("p"), &["cherry-pick", "--continue"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputContains("ported")),
        Step::git(&ww("p"), Repo::Proj, &["show", "HEAD:notes.txt"])
            .claim(Claim::OutputLine("notes on main")),
        Step::git(&ww("p"), Repo::Proj, &["ls-tree", "--name-only", "HEAD"])
            .claim(Claim::OutputLine("guide.md"))
            .claim(Claim::OutputLacks("guide.txt")),
        Step::git(&ww("p"), Repo::Proj, &["show", "HEAD:rwv.toml"])
            .claim(Claim::OutputLacks("recurse-submodules")),
        Step::git(&ww("p"), Repo::Proj, &["status", "--porcelain"])
            .claim(Claim::OutputLacks("guide"))
            .claim(Claim::OutputLacks("notes"))
            .claim(Claim::OutputLacks("rwv.")),
    ]);
    scenario("cherry-pick project conflict continued", steps)
}

/// `cherry-pick-conflict` in the project repo, abandoned: `rwv abort` runs
/// and puts the project repo back.
pub fn cherry_pick_project_conflict_aborts() -> Scenario {
    let mut steps = project_conflicting_port();
    steps.extend([
        Step::rwv(&ww("p"), &["abort"]).expect(Exit::Succeeds),
        Step::git(&ww("p"), Repo::Proj, &["log", "-1", "--format=%s"])
            .claim(Claim::OutputLine("notes-p")),
        Step::git(&ww("p"), Repo::Proj, &["status", "--porcelain"])
            .claim(Claim::OutputLacks("guide"))
            .claim(Claim::OutputLacks("notes"))
            .claim(Claim::OutputLacks("rwv.")),
        Step::rwv(&ww("p"), &["cherry-pick", "--continue"])
            .expect(Exit::refuses_kind(RefusalKind::NoOpRecorded))
            .claim(Claim::LocalStateUnchanged(ww("p"))),
    ]);
    scenario("cherry-pick project conflict aborted", steps)
}

/// A port killed after each step moved a repo and before it recorded the
/// move: `--continue` finds the copy, then the project commit, the run made,
/// takes each as made, and reports the port.
pub fn cherry_pick_killed_after_each_move() -> Scenario {
    let mut steps = stable_lines();
    steps.extend(port_me_on_main());
    steps.extend(feature_onto_stable());
    steps.extend([
        Step::rwv_paused(
            &ww("p"),
            &["cherry-pick", "main"],
            "cherry-pick-member-copied",
            "pick",
        ),
        Step::kill("pick"),
        Step::rwv_paused(
            &ww("p"),
            &["cherry-pick", "--continue"],
            "cherry-pick-project-committed",
            "resume",
        ),
        Step::kill("resume"),
        Step::rwv(&ww("p"), &["cherry-pick", "--continue"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputContains("ported"))
            .claim(Claim::AtPin {
                at: ww("p"),
                repo: Repo::App,
            }),
        Step::git(&ww("p"), Repo::App, &["log", "-3", "--format=%s"])
            .claim(Claim::OutputInOrder(&["fix-app-2", "fix-app-1"])),
        Step::git(&ww("p"), Repo::App, &["log", "-1", "--format=%s", "HEAD~2"])
            .claim(Claim::OutputLacks("fix-app")),
        Step::git(
            &ww("p"),
            Repo::Proj,
            &["log", "-1", "--format=%s", "HEAD~1"],
        )
        .claim(Claim::OutputLacks("port-me")),
        Step::git(&ww("p"), Repo::Proj, &["show", "HEAD:port-me.txt"])
            .claim(Claim::OutputLine("port-me")),
    ]);
    Scenario {
        name: "cherry-pick killed after each move",
        seed: Seed::default().role(Repo::Docs, Role::Dependency),
        steps,
    }
}

/// A seat `w` with work on `app`, and `main` moved on `app` since it was cut,
/// each locked: a landing needs more than a fast-forward.
fn seat_behind_a_moved_main() -> Vec<Step> {
    vec![
        create("w"),
        Step::commit(&ww("w"), Repo::App, "w-app"),
        Step::rwv(&ww("w"), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::commit(&a(), Repo::App, "main-app"),
        Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
    ]
}

/// `history-unset-fast-forward`: a target recording no setting takes a
/// landing that only fast-forwards, reading no setting.
pub fn history_unset_fast_forward() -> Scenario {
    scenario(
        "history: unset, a landing that fast-forwards",
        vec![
            create("w"),
            Step::commit(&ww("w"), Repo::App, "w-app"),
            Step::rwv(&ww("w"), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::rwv(&ww("w"), &["land"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputLacks("history unset"))
                .claim(Claim::Contains {
                    at: a(),
                    repo: Repo::App,
                    rev: "main",
                    mark: "w-app",
                }),
        ],
    )
}

/// `history-unset-behind`: a seat that landed, then fell behind as another
/// seat landed, retires into a target recording no setting: its branches are
/// only behind the target, so nothing needs a setting.
pub fn history_unset_retire_behind() -> Scenario {
    scenario(
        "history: unset, a landed seat retires behind the target",
        vec![
            create("w"),
            Step::commit(&ww("w"), Repo::App, "w-app"),
            Step::rwv(&ww("w"), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::rwv(&ww("w"), &["land"]).expect(Exit::Succeeds),
            create("v"),
            Step::commit(&ww("v"), Repo::App, "v-app"),
            Step::rwv(&ww("v"), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::rwv(&ww("v"), &["land"]).expect(Exit::Succeeds),
            Step::rwv(&ww("w"), &["land", "--retire"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputLacks("history unset"))
                .claim(Claim::WorkweaveGone(ww("w"))),
        ],
    )
}

/// `history-unset-moved`: a target recording no setting that moved refuses
/// the landing, naming `set-history` in a workspace on the target and
/// `--history`; `--history` lands once and records nothing.
pub fn history_unset_moved() -> Scenario {
    let mut steps = seat_behind_a_moved_main();
    steps.extend([
        Step::rwv(&ww("w"), &["land"])
            .expect(Exit::refuses(Repo::App, State::HistoryUnset))
            .claim(Claim::Judged(Repo::Proj, State::HistoryUnset))
            .claim(Claim::OutputContains("rwv branch set-history <h> --repo"))
            .claim(Claim::OutputContains("rerun with --history <h>"))
            .claim(Claim::LocalStateUnchanged(ww("w")))
            .claim(Claim::LocalStateUnchanged(a())),
        Step::rwv(&ww("w"), &["land", "--history", "linear"])
            .expect(Exit::Succeeds)
            .may_strand(&[Repo::App, Repo::Proj])
            .claim(Claim::Contains {
                at: a(),
                repo: Repo::App,
                rev: "main",
                mark: "main-app",
            }),
        Step::git(&a(), Repo::Proj, &["show", "main:rwv.toml"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputLacks("history")),
    ]);
    scenario("history: unset, a target that moved", steps)
}

/// `history-contradicted`: a `linear` target refuses `land --history merge`,
/// naming `set-history` and the rerun without `--history`.
pub fn history_contradicted() -> Scenario {
    let mut steps =
        vec![Step::rwv(&a(), &["branch", "set-history", "linear"]).expect(Exit::Succeeds)];
    steps.extend(seat_behind_a_moved_main());
    steps.push(
        Step::rwv(&ww("w"), &["land", "--history", "merge"])
            .expect(Exit::refuses(Repo::App, State::HistoryContradicted))
            .claim(Claim::OutputContains("rwv branch set-history <h> --repo"))
            .claim(Claim::OutputContains("rerun without --history"))
            .claim(Claim::LocalStateUnchanged(ww("w")))
            .claim(Claim::LocalStateUnchanged(a())),
    );
    scenario("history: a one-run value against a recorded one", steps)
}

/// A one-run `--history` against a recorded value is refused whether or not
/// the run needs a setting: a landing that fast-forwards, and a `--from` sync
/// that merges whatever the setting.
pub fn history_contradicted_where_no_setting_is_read() -> Scenario {
    scenario(
        "history: a contradicting one-run value is refused where nothing reads the setting",
        vec![
            Step::rwv(&a(), &["branch", "set-history", "merge"]).expect(Exit::Succeeds),
            create("w"),
            Step::commit(&ww("w"), Repo::App, "w-app"),
            Step::rwv(&ww("w"), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::rwv(&ww("w"), &["land", "--history", "linear"])
                .expect(Exit::refuses(Repo::App, State::HistoryContradicted))
                .claim(Claim::LocalStateUnchanged(ww("w")))
                .claim(Claim::LocalStateUnchanged(a())),
            Step::rwv(&ww("w"), &["sync", "--from", "main", "--history", "linear"])
                .expect(Exit::refuses_kind(RefusalKind::InapplicableFlag))
                .claim(Claim::LocalStateUnchanged(ww("w"))),
        ],
    )
}

/// `history-equal-one-run`: a one-run `--history` equal to the value the
/// target records is no contradiction: the landing runs by it.
pub fn history_equal_one_run() -> Scenario {
    let mut steps =
        vec![Step::rwv(&a(), &["branch", "set-history", "linear"]).expect(Exit::Succeeds)];
    steps.extend(seat_behind_a_moved_main());
    steps.push(
        Step::rwv(&ww("w"), &["land", "--history", "linear"])
            .expect(Exit::Succeeds)
            .may_strand(&[Repo::App, Repo::Proj])
            .claim(Claim::OutputLacks("contradicted"))
            .claim(Claim::Contains {
                at: a(),
                repo: Repo::App,
                rev: "main",
                mark: "main-app",
            }),
    );
    scenario("history: a one-run value equal to the recorded one", steps)
}

/// `history-linear-landing`: a `linear` target that moved; `land --retire`
/// rebases the source after `rebased` backups, fast-forwards `main`, lands
/// the work by ancestry, and retire deletes the seat's branches.
pub fn history_linear_landing() -> Scenario {
    let mut steps =
        vec![Step::rwv(&a(), &["branch", "set-history", "linear"]).expect(Exit::Succeeds)];
    steps.extend(seat_behind_a_moved_main());
    steps.extend([
        Step::rwv(&ww("w"), &["land", "--retire"])
            .expect(Exit::Succeeds)
            .may_strand(&[Repo::App, Repo::Proj])
            .claim(Claim::OutputContains(
                "local/remotes/app: rebased; the tip it left",
            ))
            .claim(Claim::WorkweaveGone(ww("w")))
            .claim(Claim::OutputLacks("left:"))
            .claim(Claim::Contains {
                at: a(),
                repo: Repo::App,
                rev: "main",
                mark: "main-app",
            })
            .claim(Claim::NoBackup {
                repo: Repo::App,
                mark: "w-app",
            }),
        Step::git(&a(), Repo::App, &["log", "--format=%s", "-1", "main"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputContains("w-app")),
        Step::git(&a(), Repo::App, &["rev-parse", "--verify", "-q", "proj--w"])
            .expect(Exit::Code(1)),
    ]);
    scenario("history: a linear landing", steps)
}

/// `history-ff-only`: an `ff-only` target that moved refuses the landing,
/// naming `rwv sync` (C36); `sync` rebases; `land` then fast-forwards.
pub fn history_ff_only() -> Scenario {
    let mut steps =
        vec![Step::rwv(&a(), &["branch", "set-history", "ff-only"]).expect(Exit::Succeeds)];
    steps.extend(seat_behind_a_moved_main());
    steps.extend([
        Step::rwv(&ww("w"), &["land"])
            .expect(Exit::refuses(Repo::App, State::HistoryNotFastForward))
            .claim(Claim::OutputContains("take the target's new work in first"))
            .claim(Claim::OutputContains("rwv sync"))
            .claim(Claim::LocalStateUnchanged(ww("w")))
            .claim(Claim::LocalStateUnchanged(a())),
        Step::rwv(&ww("w"), &["sync"])
            .expect(Exit::Succeeds)
            .may_strand(&[Repo::App, Repo::Proj])
            .claim(Claim::OutputContains("local/remotes/app: rebased onto")),
        Step::rwv(&ww("w"), &["land"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputLacks("rebased"))
            .claim(Claim::OutputLacks("merged"))
            .claim(Claim::Contains {
                at: a(),
                repo: Repo::App,
                rev: "main^1",
                mark: "main-app",
            }),
        Step::git(&a(), Repo::App, &["log", "--format=%s", "-1", "main"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputContains("w-app")),
    ]);
    scenario("history: ff-only", steps)
}

/// `history-merge`: a `merge` target that moved; the landing's merge commit
/// has `main` as first parent and the seat's work as second, and the seat's
/// branch is not rewritten.
pub fn history_merge() -> Scenario {
    let mut steps =
        vec![Step::rwv(&a(), &["branch", "set-history", "merge"]).expect(Exit::Succeeds)];
    steps.extend(seat_behind_a_moved_main());
    steps.extend([Step::rwv(&ww("w"), &["land"])
        .expect(Exit::Succeeds)
        .claim(Claim::OutputLacks("rebased"))
        .claim(Claim::Contains {
            at: a(),
            repo: Repo::App,
            rev: "main^1",
            mark: "main-app",
        })
        .claim(Claim::Contains {
            at: a(),
            repo: Repo::App,
            rev: "main^2",
            mark: "w-app",
        })]);
    scenario("history: merge", steps)
}

/// `history-range-with-merge`: a `linear` target, a seat whose `lib` range
/// holds a raw merge; the landing integrates `lib` by a merge commit, first
/// parent `main`, reports it `merged`, and flattens nothing.
pub fn history_range_with_merge() -> Scenario {
    scenario(
        "history: a range holding a merge lands by merge",
        vec![
            Step::rwv(&a(), &["branch", "set-history", "linear"]).expect(Exit::Succeeds),
            create("w"),
            Step::git(&ww("w"), Repo::Lib, &["switch", "-c", "side"]),
            Step::commit(&ww("w"), Repo::Lib, "side-lib"),
            Step::git(&ww("w"), Repo::Lib, &["switch", "proj--w"]),
            Step::commit(&ww("w"), Repo::Lib, "w-lib"),
            Step::git(
                &ww("w"),
                Repo::Lib,
                &["merge", "--no-ff", "-m", "raw-merge", "side"],
            ),
            Step::rwv(&ww("w"), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::commit(&a(), Repo::Lib, "main-lib"),
            Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::rwv(&ww("w"), &["land"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains(
                    "local/remotes/lib: merged: the commits a rebase would replay hold the merge",
                ))
                .claim(Claim::OutputLacks("local/remotes/lib: rebased"))
                .claim(Claim::Contains {
                    at: a(),
                    repo: Repo::Lib,
                    rev: "main^1",
                    mark: "main-lib",
                })
                .claim(Claim::Contains {
                    at: a(),
                    repo: Repo::Lib,
                    rev: "main^2^2",
                    mark: "side-lib",
                })
                .claim(Claim::Contains {
                    at: a(),
                    repo: Repo::Lib,
                    rev: "main^2^1",
                    mark: "w-lib",
                }),
        ],
    )
}

/// `history-ff-only-range-with-merge`: an `ff-only` target that moved, a seat
/// whose `lib` range holds a raw merge. The landing refuses, naming `sync`;
/// `sync` takes `main` in by a merge, the seat's branch first; the landing
/// then integrates `lib` by a merge commit whose first parent is `main`'s old
/// tip, so `main`'s first-parent chain never runs through the seat.
pub fn history_ff_only_range_with_merge() -> Scenario {
    scenario(
        "history: ff-only lands a range holding a merge by merge",
        vec![
            Step::rwv(&a(), &["branch", "set-history", "ff-only"]).expect(Exit::Succeeds),
            create("w"),
            Step::git(&ww("w"), Repo::Lib, &["switch", "-c", "side"]),
            Step::commit(&ww("w"), Repo::Lib, "side-lib"),
            Step::git(&ww("w"), Repo::Lib, &["switch", "proj--w"]),
            Step::commit(&ww("w"), Repo::Lib, "w-lib"),
            Step::git(
                &ww("w"),
                Repo::Lib,
                &["merge", "--no-ff", "-m", "raw-merge", "side"],
            ),
            Step::rwv(&ww("w"), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::commit(&a(), Repo::Lib, "main-lib"),
            Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::rwv(&ww("w"), &["land"])
                .expect(Exit::refuses(Repo::Lib, State::HistoryNotFastForward))
                .claim(Claim::LocalStateUnchanged(ww("w")))
                .claim(Claim::LocalStateUnchanged(a())),
            Step::rwv(&ww("w"), &["sync"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains("local/remotes/lib: merged"))
                .claim(Claim::Contains {
                    at: ww("w"),
                    repo: Repo::Lib,
                    rev: "proj--w^1",
                    mark: "side-lib",
                })
                .claim(Claim::Contains {
                    at: ww("w"),
                    repo: Repo::Lib,
                    rev: "proj--w^2",
                    mark: "main-lib",
                }),
            Step::rwv(&ww("w"), &["land"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains("local/remotes/lib: merged"))
                .claim(Claim::Contains {
                    at: a(),
                    repo: Repo::Lib,
                    rev: "main^1",
                    mark: "main-lib",
                })
                .claim(Claim::Contains {
                    at: a(),
                    repo: Repo::Lib,
                    rev: "main^2",
                    mark: "side-lib",
                }),
            Step::git(
                &a(),
                Repo::Lib,
                &["log", "--first-parent", "--format=%s", "main"],
            )
            .expect(Exit::Succeeds)
            .claim(Claim::OutputLacks("raw-merge")),
        ],
    )
}

/// `history-sync-from-declared`: a declared workspace on a `linear` `main`
/// takes `stable` in with `sync --from stable`: a merge, `main` never
/// rebased.
pub fn history_sync_from_declared() -> Scenario {
    scenario(
        "history: sync --from in a declared workspace merges",
        vec![
            Step::rwv(&a(), &["branch", "set-history", "linear"]).expect(Exit::Succeeds),
            Step::rwv(&a(), &["branch", "create", "stable", "--declared"]).expect(Exit::Succeeds),
            Step::commit(&a(), Repo::Proj, "stable-notes"),
            Step::rwv(&a(), &["switch", "main"]).expect(Exit::Succeeds),
            Step::commit(&a(), Repo::Proj, "main-notes"),
            Step::rwv(&a(), &["sync", "--from", "stable"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputLacks("rebased"))
                .claim(Claim::Contains {
                    at: a(),
                    repo: Repo::Proj,
                    rev: "main^1",
                    mark: "main-notes",
                })
                .claim(Claim::Contains {
                    at: a(),
                    repo: Repo::Proj,
                    rev: "main",
                    mark: "stable-notes",
                }),
        ],
    )
}

/// `history-member-override`: top-level `linear`, `lib` overridden to
/// `merge`; one landing rebases `app` and merges `lib`.
pub fn history_member_override_lands() -> Scenario {
    scenario(
        "history: a member's override governs its landing",
        vec![
            Step::rwv(&a(), &["branch", "set-history", "linear"]).expect(Exit::Succeeds),
            Step::rwv(&a(), &["branch", "set-history", "merge", "--repo", LIB])
                .expect(Exit::Succeeds),
            create("w"),
            Step::commit(&ww("w"), Repo::App, "w-app"),
            Step::commit(&ww("w"), Repo::Lib, "w-lib"),
            Step::rwv(&ww("w"), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::commit(&a(), Repo::App, "main-app"),
            Step::commit(&a(), Repo::Lib, "main-lib"),
            Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::rwv(&ww("w"), &["land"])
                .expect(Exit::Succeeds)
                .may_strand(&[Repo::App, Repo::Proj])
                .claim(Claim::OutputContains(
                    "local/remotes/app: rebased; the tip it left",
                ))
                .claim(Claim::OutputLacks("local/remotes/lib: rebased"))
                .claim(Claim::Contains {
                    at: a(),
                    repo: Repo::Lib,
                    rev: "main^1",
                    mark: "main-lib",
                })
                .claim(Claim::Contains {
                    at: a(),
                    repo: Repo::Lib,
                    rev: "main^2",
                    mark: "w-lib",
                }),
            Step::git(&a(), Repo::App, &["log", "--format=%s", "-1", "main"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains("w-app")),
        ],
    )
}

/// `rebased-backup-lifetime`: a `linear` landing of an unpublished seat keeps
/// its `rebased` backup while the op is stopped, `abort` restores the seat's
/// branch, and the landing run to completion drops the backup. A `linear`
/// `sync` of a published feature keeps its backup until the republish
/// replaces the push branch, then drops it.
pub fn rebased_backup_lifetime() -> Scenario {
    let mut steps =
        vec![Step::rwv(&a(), &["branch", "set-history", "linear"]).expect(Exit::Succeeds)];
    steps.extend(seat_behind_a_moved_main());
    steps.extend([
        Step::rwv_paused(&ww("w"), &["land"], "advance-target", "landing"),
        Step::kill("landing"),
        Step::git(&a(), Repo::App, &["status", "--short"])
            .expect(Exit::Succeeds)
            .claim(Claim::BackedUp {
                repo: Repo::App,
                mark: "w-app",
                marked: Some("rebased"),
            }),
        Step::rwv(&ww("w"), &["abort"])
            .expect(Exit::Succeeds)
            .may_rewind(&[Repo::App])
            .claim(Claim::Contains {
                at: ww("w"),
                repo: Repo::App,
                rev: "proj--w",
                mark: "w-app",
            }),
        Step::git(
            &ww("w"),
            Repo::App,
            &["log", "--format=%s", "-1", "proj--w"],
        )
        .expect(Exit::Succeeds)
        .claim(Claim::OutputContains("w-app")),
        Step::rwv(&ww("w"), &["land"])
            .expect(Exit::Succeeds)
            .may_strand(&[Repo::App, Repo::Proj])
            .claim(Claim::NoBackup {
                repo: Repo::App,
                mark: "w-app",
            }),
        Step::rwv(
            &a(),
            &[
                "workweave",
                "create",
                "--project",
                "proj",
                "p",
                "--new-branch",
                "feat-p",
            ],
        )
        .expect(Exit::Succeeds),
        Step::commit(&ww("p"), Repo::App, "p-app"),
        Step::rwv(&ww("p"), &["lock", "--commit"]).expect(Exit::Succeeds),
        publishes_records(Step::rwv(&ww("p"), &["push"]))
            .pushes(Remote::origin(Repo::App), "feat-p")
            .pushes(Remote::origin(Repo::Proj), "feat-p")
            .expect(Exit::Succeeds),
        Step::commit(&a(), Repo::App, "main-app-2"),
        Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::rwv(&ww("p"), &["sync"])
            .expect(Exit::Succeeds)
            .claim(Claim::BackedUp {
                repo: Repo::App,
                mark: "p-app",
                marked: Some("rebased"),
            }),
        Step::rwv(&ww("p"), &["push"])
            .expect(Exit::refuses(Repo::App, State::PushBranchDiverged))
            .claim(Claim::OutputContains(
                "rwv push --overwrite local/remotes/app",
            )),
        Step::rwv(
            &ww("p"),
            &[
                "push",
                "--overwrite",
                "local/remotes/app",
                "--overwrite",
                "projects/proj",
            ],
        )
        .overwrites(Remote::origin(Repo::App), "feat-p")
        .overwrites(Remote::origin(Repo::Proj), "feat-p")
        .expect(Exit::Succeeds)
        .may_strand(&[Repo::App, Repo::Proj])
        .claim(Claim::OutputContains("the republish replaced it"))
        .claim(Claim::NoBackup {
            repo: Repo::App,
            mark: "p-app",
        }),
    ]);
    scenario("rebased backup lifetime", steps)
}

/// A `rebased` backup that outlives its rebase offers no republish: a
/// `linear` `sync` of a published feature stops on a conflict and is aborted,
/// which puts the branch back at the tip the backup holds and drops it; the
/// operator then amends by hand, and `push` offers only the choices for a
/// divergence rwv did not make.
pub fn rebased_backup_after_abort_offers_no_republish() -> Scenario {
    scenario(
        "a rebased backup after an abort offers no republish",
        vec![
            Step::rwv(&a(), &["branch", "set-history", "linear"]).expect(Exit::Succeeds),
            publishes_records(Step::rwv(&a(), &["push"]))
                .pushes(Remote::origin(Repo::Proj), "main")
                .expect(Exit::Succeeds),
            Step::rwv(
                &a(),
                &[
                    "workweave",
                    "create",
                    "--project",
                    "proj",
                    "p",
                    "--new-branch",
                    "feat-p",
                ],
            )
            .expect(Exit::Succeeds),
            Step::hand_edit(&ww("p"), Repo::App, "README", "app\n", "app in p\n"),
            Step::git(&ww("p"), Repo::App, &["commit", "-qam", "readme-p"]),
            Step::rwv(&ww("p"), &["lock", "--commit"]).expect(Exit::Succeeds),
            publishes_records(Step::rwv(&ww("p"), &["push"]))
                .pushes(Remote::origin(Repo::App), "feat-p")
                .pushes(Remote::origin(Repo::Proj), "feat-p")
                .expect(Exit::Succeeds),
            Step::hand_edit(&a(), Repo::App, "README", "app\n", "app on main\n"),
            Step::git(&a(), Repo::App, &["commit", "-qam", "readme-main"]),
            Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::rwv(&ww("p"), &["sync"]).expect(Exit::refuses_kind(RefusalKind::OpParked)),
            Step::rwv(&ww("p"), &["abort"]).expect(Exit::Succeeds),
            Step::rwv(&ww("p"), &["backup", "list"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputLacks("rebased-")),
            Step::git(
                &ww("p"),
                Repo::App,
                &["commit", "--amend", "-qm", "readme-p-amended"],
            ),
            Step::rwv(
                &ww("p"),
                &["member", "pin", "local/remotes/app", "HEAD", "--commit"],
            )
            .expect(Exit::Succeeds),
            Step::rwv(&ww("p"), &["push"])
                .expect(Exit::refuses(Repo::App, State::PushBranchDiverged))
                .claim(Claim::OutputLacks("--overwrite")),
        ],
    )
}

/// A `rebased` backup of one branch offers no republish for another at the
/// same tip: `feat-q` is published at the tip `feat-p` was published at;
/// a `linear` `sync` rebases `feat-p`, keeping its backup; `feat-q`, amended
/// by hand, is offered only the choices for a divergence rwv did not make.
pub fn rebased_backup_of_another_branch_offers_no_republish() -> Scenario {
    let ww_create = |name: &'static str, branch: &'static str| {
        Step::rwv(
            &a(),
            &[
                "workweave",
                "create",
                "--project",
                "proj",
                name,
                "--new-branch",
                branch,
            ],
        )
        .expect(Exit::Succeeds)
    };
    scenario(
        "a rebased backup of another branch offers no republish",
        vec![
            Step::rwv(&a(), &["branch", "set-history", "linear"]).expect(Exit::Succeeds),
            publishes_records(Step::rwv(&a(), &["push"]))
                .pushes(Remote::origin(Repo::Proj), "main")
                .expect(Exit::Succeeds),
            ww_create("p", "feat-p"),
            Step::commit(&ww("p"), Repo::App, "p-app"),
            Step::rwv(&ww("p"), &["lock", "--commit"]).expect(Exit::Succeeds),
            publishes_records(Step::rwv(&ww("p"), &["push"]))
                .pushes(Remote::origin(Repo::App), "feat-p")
                .pushes(Remote::origin(Repo::Proj), "feat-p")
                .expect(Exit::Succeeds),
            ww_create("q", "feat-q"),
            Step::git(&ww("q"), Repo::App, &["reset", "-q", "--hard", "feat-p"]),
            Step::rwv(&ww("q"), &["lock", "--commit"]).expect(Exit::Succeeds),
            publishes_records(Step::rwv(&ww("q"), &["push"]))
                .pushes(Remote::origin(Repo::App), "feat-q")
                .pushes(Remote::origin(Repo::Proj), "feat-q")
                .expect(Exit::Succeeds),
            Step::commit(&a(), Repo::App, "main-app"),
            Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::rwv(&ww("p"), &["sync"])
                .expect(Exit::Succeeds)
                .claim(Claim::BackedUp {
                    repo: Repo::App,
                    mark: "p-app",
                    marked: Some("rebased"),
                }),
            Step::git(
                &ww("q"),
                Repo::App,
                &["commit", "--amend", "-qm", "q-amended"],
            ),
            Step::rwv(&ww("q"), &["member", "pin", APP, "HEAD", "--commit"]).expect(Exit::Succeeds),
            Step::rwv(&ww("q"), &["push"])
                .expect(Exit::refuses(Repo::App, State::PushBranchDiverged))
                .claim(Claim::OutputLacks("--overwrite")),
            Step::rwv(&ww("p"), &["push"])
                .expect(Exit::refuses(Repo::App, State::PushBranchDiverged))
                .claim(Claim::OutputContains(
                    "rwv push --overwrite local/remotes/app",
                )),
        ],
    )
}

/// `history-published-rebase`: a `linear` `sync` of a published feature, then
/// `push`: the rebasing machine is offered the republish recovery, leased to
/// the tip its `rebased` backup holds; another machine holding unpushed work
/// on that branch sees only the ordinary choices for a diverged branch.
pub fn history_published_rebase() -> Scenario {
    let b_seat = Place::ww(Machine::B, "pb");
    scenario(
        "history: republishing a rebased published feature",
        vec![
            Step::rwv(&a(), &["branch", "set-history", "linear"]).expect(Exit::Succeeds),
            publishes_records(Step::rwv(&a(), &["push"]))
                .pushes(Remote::origin(Repo::Proj), "main")
                .expect(Exit::Succeeds),
            Step::rwv(
                &a(),
                &[
                    "workweave",
                    "create",
                    "--project",
                    "proj",
                    "p",
                    "--new-branch",
                    "feat-p",
                ],
            )
            .expect(Exit::Succeeds),
            Step::commit(&ww("p"), Repo::App, "p-app"),
            Step::rwv(&ww("p"), &["lock", "--commit"]).expect(Exit::Succeeds),
            publishes_records(Step::rwv(&ww("p"), &["push"]))
                .pushes(Remote::origin(Repo::App), "feat-p")
                .pushes(Remote::origin(Repo::Proj), "feat-p")
                .expect(Exit::Succeeds),
            Step::new_machine(Machine::B),
            Step::rwv(
                &b(),
                &[
                    "workweave",
                    "create",
                    "--project",
                    "proj",
                    "pb",
                    "--branch",
                    "feat-p",
                ],
            )
            .expect(Exit::Succeeds),
            Step::commit(&b_seat, Repo::App, "b-app"),
            Step::commit(&a(), Repo::App, "main-app"),
            Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::rwv(&ww("p"), &["sync"]).expect(Exit::Succeeds),
            Step::rwv(&ww("p"), &["sync"])
                .expect(Exit::refuses(Repo::App, State::PushBranchDiverged))
                .claim(Claim::OutputContains(
                    "rwv push --overwrite local/remotes/app",
                )),
            Step::rwv(&ww("p"), &["push"])
                .expect(Exit::refuses(Repo::App, State::PushBranchDiverged))
                .claim(Claim::OutputContains(
                    "rwv push --overwrite local/remotes/app",
                )),
            Step::rwv(
                &ww("p"),
                &[
                    "push",
                    "--overwrite",
                    "local/remotes/app",
                    "--overwrite",
                    "projects/proj",
                ],
            )
            .overwrites(Remote::origin(Repo::App), "feat-p")
            .overwrites(Remote::origin(Repo::Proj), "feat-p")
            .expect(Exit::Succeeds)
            .may_strand(&[Repo::App, Repo::Proj]),
            Step::rwv(&b_seat, &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::rwv(&b_seat, &["fetch"]).expect(Exit::Succeeds),
            Step::rwv(&b_seat, &["push"])
                .expect(Exit::refuses(Repo::App, State::PushBranchDiverged))
                .claim(Claim::OutputLacks("--overwrite"))
                .claim(Claim::OutputContains("--merge-upstream")),
        ],
    )
}

/// A colleague pushes the published feature between its rebase and the
/// republish: once fetched, the divergence is no longer rwv's own, so the
/// republish recovery is not printed, and the `rebased` backup of the tip the
/// push branch held before stays.
pub fn history_republish_after_a_colleague_pushed() -> Scenario {
    let b_seat = Place::ww(Machine::B, "pb");
    scenario(
        "history: no republish recovery once a colleague pushed in between",
        vec![
            Step::rwv(&a(), &["branch", "set-history", "linear"]).expect(Exit::Succeeds),
            publishes_records(Step::rwv(&a(), &["push"]))
                .pushes(Remote::origin(Repo::Proj), "main")
                .expect(Exit::Succeeds),
            Step::rwv(
                &a(),
                &[
                    "workweave",
                    "create",
                    "--project",
                    "proj",
                    "p",
                    "--new-branch",
                    "feat-p",
                ],
            )
            .expect(Exit::Succeeds),
            Step::commit(&ww("p"), Repo::App, "p-app"),
            Step::rwv(&ww("p"), &["lock", "--commit"]).expect(Exit::Succeeds),
            publishes_records(Step::rwv(&ww("p"), &["push"]))
                .pushes(Remote::origin(Repo::App), "feat-p")
                .pushes(Remote::origin(Repo::Proj), "feat-p")
                .expect(Exit::Succeeds),
            Step::new_machine(Machine::B),
            Step::rwv(
                &b(),
                &[
                    "workweave",
                    "create",
                    "--project",
                    "proj",
                    "pb",
                    "--branch",
                    "feat-p",
                ],
            )
            .expect(Exit::Succeeds),
            Step::commit(&b_seat, Repo::App, "b-app"),
            Step::rwv(&b_seat, &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::commit(&a(), Repo::App, "main-app"),
            Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::rwv(&ww("p"), &["sync"]).expect(Exit::Succeeds),
            publishes_records(Step::rwv(&b_seat, &["push"]))
                .pushes(Remote::origin(Repo::App), "feat-p")
                .pushes(Remote::origin(Repo::Proj), "feat-p")
                .expect(Exit::Succeeds),
            Step::rwv(&ww("p"), &["fetch"]).expect(Exit::Succeeds),
            Step::rwv(&ww("p"), &["push"])
                .expect(Exit::refuses(Repo::App, State::PushBranchDiverged))
                .claim(Claim::OutputLacks("--overwrite"))
                .claim(Claim::BackedUp {
                    repo: Repo::App,
                    mark: "p-app",
                    marked: Some("rebased"),
                }),
        ],
    )
}

fn published_feature_p_with_moved_main() -> Vec<Step> {
    vec![
        Step::rwv(&a(), &["branch", "set-history", "linear"]).expect(Exit::Succeeds),
        publishes_records(Step::rwv(&a(), &["push"]))
            .pushes(Remote::origin(Repo::Proj), "main")
            .expect(Exit::Succeeds),
        Step::rwv(
            &a(),
            &[
                "workweave",
                "create",
                "--project",
                "proj",
                "p",
                "--new-branch",
                "feat-p",
            ],
        )
        .expect(Exit::Succeeds),
        Step::commit(&ww("p"), Repo::App, "p-app"),
        Step::rwv(&ww("p"), &["lock", "--commit"]).expect(Exit::Succeeds),
        publishes_records(Step::rwv(&ww("p"), &["push"]))
            .pushes(Remote::origin(Repo::App), "feat-p")
            .pushes(Remote::origin(Repo::Proj), "feat-p")
            .expect(Exit::Succeeds),
        Step::commit(&a(), Repo::App, "main-app"),
        Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
    ]
}

/// A published feature landed under `linear` and retired: its branch is
/// deleted, so the `rebased` backup of the rebase no republish can use goes
/// with it.
pub fn history_retired_published_seat_drops_its_rebased_backups() -> Scenario {
    let mut steps = published_feature_p_with_moved_main();
    steps.push(
        Step::rwv(&ww("p"), &["land", "--retire"])
            .expect(Exit::Code(3))
            .may_strand(&[Repo::App, Repo::Proj])
            .may_rewrite(&[Repo::App, Repo::Proj])
            .claim(Claim::NoBackup {
                repo: Repo::App,
                mark: "p-app",
            }),
    );
    scenario(
        "history: a retired published seat keeps no rebased backup",
        steps,
    )
}

/// A republish leased to a newer tip than the `rebased` backup holds (a
/// colleague pushed in between) drops that backup once it lands.
pub fn history_overwrite_to_a_newer_tip_drops_the_old_rebased_backup() -> Scenario {
    let b_seat = Place::ww(Machine::B, "pb");
    let mut steps = published_feature_p_with_moved_main();
    steps.splice(
        6..6,
        [
            Step::new_machine(Machine::B),
            Step::rwv(
                &b(),
                &[
                    "workweave",
                    "create",
                    "--project",
                    "proj",
                    "pb",
                    "--branch",
                    "feat-p",
                ],
            )
            .expect(Exit::Succeeds),
            Step::commit(&b_seat, Repo::App, "b-app"),
            Step::rwv(&b_seat, &["lock", "--commit"]).expect(Exit::Succeeds),
        ],
    );
    steps.extend([
        Step::rwv(&ww("p"), &["sync"]).expect(Exit::Succeeds),
        publishes_records(Step::rwv(&b_seat, &["push"]))
            .pushes(Remote::origin(Repo::App), "feat-p")
            .pushes(Remote::origin(Repo::Proj), "feat-p")
            .expect(Exit::Succeeds),
        Step::rwv(&ww("p"), &["fetch"]).expect(Exit::Succeeds),
        Step::rwv(
            &ww("p"),
            &[
                "push",
                "--overwrite",
                "local/remotes/app",
                "--overwrite",
                "projects/proj",
            ],
        )
        .overwrites(Remote::origin(Repo::App), "feat-p")
        .overwrites(Remote::origin(Repo::Proj), "feat-p")
        .expect(Exit::Succeeds)
        .may_strand(&[Repo::App, Repo::Proj])
        .may_rewind(&[Repo::App]),
        Step::rwv(&a(), &["backup", "list"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputLacks("rebased")),
    ]);
    scenario(
        "history: an overwrite to a newer tip drops the old rebased backup",
        steps,
    )
}

/// `app` declares `DERIVED` regenerated; the seat and the target `main` each
/// changed it, and `OTHER` too where `other` is set. Under a `merge` target
/// the verb composes a merge, whose result must be the one a rebase reaches:
/// `DERIVED` takes the receiving side's content, and a path nobody declared
/// that both sides changed still stops.
fn derived_both_sides(verb: &'static str, other: bool) -> Scenario {
    let main_files: &[(&str, &str)] = if other {
        &[("DERIVED", "main-side\n"), ("OTHER", "main-other\n")]
    } else {
        &[("DERIVED", "main-side\n")]
    };
    let seat_files: &[(&str, &str)] = if other {
        &[("DERIVED", "seat-side\n"), ("OTHER", "seat-other\n")]
    } else {
        &[("DERIVED", "seat-side\n")]
    };
    let mut steps = vec![
        Step::rwv(&a(), &["branch", "set-history", "merge"]).expect(Exit::Succeeds),
        Step::hand_commit(
            &a(),
            Repo::App,
            "main",
            &[
                (".gitattributes", "DERIVED merge=rwv-ours\n"),
                ("DERIVED", "base\n"),
                ("OTHER", "base\n"),
            ],
        ),
        Step::git(&a(), Repo::App, &["reset", "--hard"]),
        Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
        create("w"),
        Step::hand_commit(&ww("w"), Repo::App, "proj--w", seat_files),
        Step::git(&ww("w"), Repo::App, &["reset", "--hard"]),
        Step::rwv(&ww("w"), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::hand_commit(&a(), Repo::App, "main", main_files),
        Step::git(&a(), Repo::App, &["reset", "--hard"]),
        Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
    ];
    let run = Step::rwv(&ww("w"), &[verb]);
    if other {
        steps.push(run.expect(Exit::refuses_kind(RefusalKind::OpParked)));
    } else {
        let read = if verb == "land" {
            Step::git(&a(), Repo::App, &["show", "main:DERIVED"])
        } else {
            Step::git(&ww("w"), Repo::App, &["show", "proj--w:DERIVED"])
        };
        steps.push(
            run.expect(Exit::Succeeds)
                .may_strand(&[Repo::App, Repo::Proj]),
        );
        steps.push(
            read.expect(Exit::Succeeds)
                .claim(Claim::OutputContains("main-side")),
        );
    }
    scenario(
        if verb == "land" {
            "history: a landing merge resolves a derived path to the receiving side"
        } else {
            "history: a sync merge resolves a derived path to the receiving side"
        },
        steps,
    )
}

pub fn history_land_merge_resolves_derived_to_the_receiving_side() -> Scenario {
    derived_both_sides("land", false)
}

pub fn history_land_merge_still_stops_on_an_undeclared_path() -> Scenario {
    derived_both_sides("land", true)
}

pub fn history_sync_merge_resolves_derived_to_the_receiving_side() -> Scenario {
    derived_both_sides("sync", false)
}

pub fn history_sync_merge_still_stops_on_an_undeclared_path() -> Scenario {
    derived_both_sides("sync", true)
}

/// `sync --history` against a recorded value is refused for the project repo
/// and each member alike, though the target's move is only taken in by
/// fast-forward and no setting is read.
pub fn history_sync_contradicted_where_it_fast_forwards() -> Scenario {
    scenario(
        "history: sync refuses a contradicting one-run value where it fast-forwards",
        vec![
            Step::rwv(&a(), &["branch", "set-history", "merge"]).expect(Exit::Succeeds),
            create("w"),
            Step::commit(&a(), Repo::App, "main-app"),
            Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::rwv(&ww("w"), &["sync", "--history", "linear"])
                .expect(Exit::refuses(Repo::App, State::HistoryContradicted))
                .claim(Claim::Judged(Repo::Proj, State::HistoryContradicted))
                .claim(Claim::LocalStateUnchanged(ww("w"))),
        ],
    )
}

/// A `sync` that integrated a range holding a merge by a merge commit and
/// stopped on a conflict reports it again when `--continue` finishes.
pub fn sync_continue_replays_what_the_stopped_run_reported() -> Scenario {
    scenario(
        "sync --continue replays the merged report of the stopped run",
        vec![
            Step::rwv(&a(), &["branch", "set-history", "linear"]).expect(Exit::Succeeds),
            create("w"),
            Step::git(&ww("w"), Repo::App, &["switch", "-c", "side"]),
            Step::commit(&ww("w"), Repo::App, "side-app"),
            Step::git(&ww("w"), Repo::App, &["switch", "proj--w"]),
            Step::hand_edit(&ww("w"), Repo::App, "README", "app\n", "app in w\n"),
            Step::git(&ww("w"), Repo::App, &["commit", "-qam", "readme-w"]),
            Step::git(
                &ww("w"),
                Repo::App,
                &["merge", "--no-ff", "-m", "bump", "side"],
            ),
            Step::rwv(&ww("w"), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::hand_edit(&a(), Repo::App, "README", "app\n", "app on main\n"),
            Step::git(&a(), Repo::App, &["commit", "-qam", "readme-main"]),
            Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::rwv(&ww("w"), &["sync"]).expect(Exit::refuses_kind(RefusalKind::OpParked)),
            Step::git(&ww("w"), Repo::App, &["checkout", "--ours", "README"]),
            Step::git(&ww("w"), Repo::App, &["add", "README"]),
            Step::rwv(&ww("w"), &["sync", "--continue"])
                .expect(Exit::Succeeds)
                .may_strand(&[Repo::App, Repo::Proj])
                .claim(Claim::OutputContains(
                    "local/remotes/app: merged: the commits a rebase would replay hold the merge",
                )),
        ],
    )
}

/// A target recording `history` that moved in `app` and `lib`, and a seat whose `app`
/// edit conflicts with `main`'s: `sync` stops on `app`'s merge before `lib`,
/// planned a fast-forward, is reached. The operator resolves `app` and
/// commits in `lib` during the stop.
fn sync_stopped_then_lib_committed(history: &'static str) -> Vec<Step> {
    vec![
        Step::rwv(&a(), &["branch", "set-history", history]).expect(Exit::Succeeds),
        create("w"),
        Step::hand_edit(&ww("w"), Repo::App, "README", "app\n", "app in w\n"),
        Step::git(&ww("w"), Repo::App, &["commit", "-qam", "readme-w"]),
        Step::rwv(&ww("w"), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::hand_edit(&a(), Repo::App, "README", "app\n", "app on main\n"),
        Step::git(&a(), Repo::App, &["commit", "-qam", "readme-main"]),
        Step::commit(&a(), Repo::Lib, "main-lib"),
        Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::rwv(&ww("w"), &["sync"]).expect(Exit::refuses_kind(RefusalKind::OpParked)),
        Step::git(&ww("w"), Repo::App, &["checkout", "--ours", "README"]),
        Step::git(&ww("w"), Repo::App, &["add", "README"]),
        Step::commit(&ww("w"), Repo::Lib, "w-lib-stop"),
    ]
}

/// `abort-refuses-a-tip-amended-during-a-stop`'s stop, continued instead:
/// the re-map refuses naming the tip it read, and each choice it prints, run
/// as printed, ends the state.
pub fn sync_continue_refuses_a_tip_amended_during_a_stop() -> Scenario {
    let steps = vec![
        Step::rwv(&a(), &["branch", "set-history", "linear"]).expect(Exit::Succeeds),
        create("w"),
        Step::commit(&ww("w"), Repo::App, "w-app"),
        Step::hand_edit(&ww("w"), Repo::Lib, "README", "lib\n", "lib in w\n"),
        Step::git(&ww("w"), Repo::Lib, &["commit", "-qam", "readme-w"]),
        Step::rwv(&ww("w"), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::commit(&a(), Repo::App, "main-app"),
        Step::hand_edit(&a(), Repo::Lib, "README", "lib\n", "lib on main\n"),
        Step::git(&a(), Repo::Lib, &["commit", "-qam", "readme-main"]),
        Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::rwv(&ww("w"), &["sync"])
            .expect(Exit::refuses_kind(RefusalKind::OpParked))
            .claim(Claim::OutputContains("app: rebased")),
        Step::hand_edit(&ww("w"), Repo::App, "README", "app", "app amended"),
        Step::git(
            &ww("w"),
            Repo::App,
            &["commit", "-qa", "--amend", "--no-edit"],
        ),
        Step::git(&ww("w"), Repo::Lib, &["checkout", "--ours", "README"]),
        Step::git(&ww("w"), Repo::Lib, &["add", "README"]),
        Step::rwv(&ww("w"), &["sync", "--continue"])
            .expect(Exit::refuses_kind(RefusalKind::ReplayUnrecorded))
            .may_rewind(&[Repo::App, Repo::Lib])
            .claim(Claim::OutputLacks("rwv abort\n")),
        Step::rwv_printed_exactly(
            &ww("w"),
            "rwv abort --abandon-foreign-tip local/remotes/app",
        )
        .expect(Exit::Succeeds)
        .may_rewind(&[Repo::App, Repo::Lib])
        .may_restore(&[Repo::App, Repo::Proj]),
    ];
    scenario("sync continue refuses a tip amended during a stop", steps)
}

/// A sync rebases `app` and stops on a conflict in `lib`; while stopped, the
/// operator amends `app`'s rebased tip, which keeps its author date. Abort
/// refuses that tip as foreign and leaves the amended commit where it is.
pub fn abort_refuses_a_tip_amended_during_a_stop() -> Scenario {
    let steps = vec![
        Step::rwv(&a(), &["branch", "set-history", "linear"]).expect(Exit::Succeeds),
        create("w"),
        Step::commit(&ww("w"), Repo::App, "w-app"),
        Step::hand_edit(&ww("w"), Repo::Lib, "README", "lib\n", "lib in w\n"),
        Step::git(&ww("w"), Repo::Lib, &["commit", "-qam", "readme-w"]),
        Step::rwv(&ww("w"), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::commit(&a(), Repo::App, "main-app"),
        Step::hand_edit(&a(), Repo::Lib, "README", "lib\n", "lib on main\n"),
        Step::git(&a(), Repo::Lib, &["commit", "-qam", "readme-main"]),
        Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::rwv(&ww("w"), &["sync"])
            .expect(Exit::refuses_kind(RefusalKind::OpParked))
            .claim(Claim::OutputContains("app: rebased")),
        Step::hand_edit(&ww("w"), Repo::App, "README", "app", "app amended"),
        Step::git(
            &ww("w"),
            Repo::App,
            &["commit", "-qa", "--amend", "--no-edit"],
        ),
        abort_refused_whole(
            Step::rwv(&ww("w"), &["abort"]).expect(Exit::refuses_kind(RefusalKind::ForeignTip)),
            false,
            &[ww("w")],
        ),
        Step::git(
            &ww("w"),
            Repo::App,
            &["grep", "-q", "app amended", "HEAD", "--", "README"],
        )
        .expect(Exit::Succeeds),
    ];
    scenario(
        "abort refuses a tip the operator amended during a stop",
        steps,
    )
}

/// `sync --continue` under `linear` judges the dependents of a rebase the
/// stop made it plan before it rebases: during the stop `lib` gains a
/// commit, so `lib`, planned a fast-forward, now needs a rebase, and a
/// dependent `w2` is cut from `w`'s branch, lacking that commit. The
/// continue refuses on `w2` and leaves `lib`'s commit as the operator made
/// it.
pub fn sync_continue_judges_dependents_before_a_rejudged_rebase() -> Scenario {
    let mut steps = sync_stopped_then_lib_committed("linear");
    steps.extend([
        create("w2"),
        dependent_of(&ww("w2"), "w2", "proj--w"),
        Step::rwv(&ww("w"), &["sync", "--continue"])
            .expect(Exit::refuses(Repo::Proj, State::TargetDependentLacksTip))
            .claim(Claim::OutputContains("in local/remotes/lib"))
            .claim(Claim::Contains {
                at: ww("w"),
                repo: Repo::Lib,
                rev: "proj--w",
                mark: "w-lib-stop",
            })
            .claim(Claim::NoBackup {
                repo: Repo::Lib,
                mark: "w-lib-stop",
            }),
    ]);
    scenario(
        "sync --continue judges dependents before a rebase it re-judged",
        steps,
    )
}

/// `sync --continue` re-judges a repo the stop changed: `lib`, planned a
/// fast-forward, now holds the operator's commit, and under `merge` it takes
/// `main` in by a merge commit, the seat's branch first, never by a rebase.
pub fn sync_continue_merges_a_repo_the_stop_diverged() -> Scenario {
    let mut steps = sync_stopped_then_lib_committed("merge");
    steps.push(
        Step::rwv(&ww("w"), &["sync", "--continue"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputLacks("rebased"))
            .claim(Claim::Contains {
                at: ww("w"),
                repo: Repo::Lib,
                rev: "proj--w^1",
                mark: "w-lib-stop",
            })
            .claim(Claim::Contains {
                at: ww("w"),
                repo: Repo::Lib,
                rev: "proj--w^2",
                mark: "main-lib",
            })
            .claim(Claim::NoBackup {
                repo: Repo::Lib,
                mark: "w-lib-stop",
            }),
    );
    scenario("sync --continue merges a repo the stop diverged", steps)
}

/// `sync --continue` under `linear` never flattens a merge the stop left:
/// `lib`, planned a fast-forward, now holds the operator's merge, so it takes
/// `main` in by a merge commit, reported `merged`.
pub fn sync_continue_merges_a_range_the_stop_gave_a_merge() -> Scenario {
    let mut steps = sync_stopped_then_lib_committed("linear");
    steps.extend([
        Step::git(&ww("w"), Repo::Lib, &["switch", "-c", "side", "HEAD~1"]),
        Step::commit(&ww("w"), Repo::Lib, "side-lib"),
        Step::git(&ww("w"), Repo::Lib, &["switch", "proj--w"]),
        Step::git(
            &ww("w"),
            Repo::Lib,
            &["merge", "--no-ff", "-m", "stop-merge", "side"],
        ),
        Step::rwv(&ww("w"), &["sync", "--continue"])
            .expect(Exit::Succeeds)
            .may_strand(&[Repo::App, Repo::Proj])
            .may_rewind(&[Repo::App])
            .claim(Claim::OutputContains(
                "local/remotes/lib: merged: the commits a rebase",
            ))
            .claim(Claim::OutputLacks("local/remotes/lib: rebased"))
            .claim(Claim::Contains {
                at: ww("w"),
                repo: Repo::Lib,
                rev: "proj--w^1^2",
                mark: "side-lib",
            })
            .claim(Claim::Contains {
                at: ww("w"),
                repo: Repo::Lib,
                rev: "proj--w^2",
                mark: "main-lib",
            }),
    ]);
    scenario(
        "sync --continue merges a range the stop gave a merge",
        steps,
    )
}

/// `land --continue` re-judges what the stop left: a `linear` landing stops
/// rebasing `app`; the operator gives up the rebase and merges `main` in by
/// hand, the seat's branch first. The resumed landing composes a merge commit
/// whose first parent is `main`'s tip, reported `merged`, so `main`'s
/// first-parent chain never runs through the seat.
pub fn land_continue_composes_a_merge_the_stop_left() -> Scenario {
    scenario(
        "land --continue composes a merge the stop left",
        vec![
            Step::rwv(&a(), &["branch", "set-history", "linear"]).expect(Exit::Succeeds),
            create("w"),
            Step::hand_edit(&ww("w"), Repo::App, "README", "app\n", "app in w\n"),
            Step::git(&ww("w"), Repo::App, &["commit", "-qam", "readme-w"]),
            Step::rwv(&ww("w"), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::hand_edit(&a(), Repo::App, "README", "app\n", "app on main\n"),
            Step::git(&a(), Repo::App, &["commit", "-qam", "readme-main"]),
            Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::rwv(&ww("w"), &["land"])
                .expect(Exit::refuses_kind(RefusalKind::OpParked))
                .may_strand(&[Repo::Proj])
                .may_rewind(&[Repo::App]),
            Step::git(&ww("w"), Repo::App, &["rebase", "--abort"]),
            Step::git(&ww("w"), Repo::App, &["merge", "main"]).expect(Exit::Code(1)),
            Step::git(&ww("w"), Repo::App, &["checkout", "--ours", "README"]),
            Step::git(&ww("w"), Repo::App, &["commit", "-qam", "hand-merge"]),
            Step::rwv(&ww("w"), &["land", "--continue"])
                .expect(Exit::Succeeds)
                .may_strand(&[Repo::App, Repo::Proj])
                .claim(Claim::OutputContains("local/remotes/app: merged")),
            Step::git(&a(), Repo::App, &["log", "-1", "--format=%s", "main^1"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputLine("readme-main")),
            Step::git(
                &a(),
                Repo::App,
                &["log", "--first-parent", "--format=%s", "main"],
            )
            .expect(Exit::Succeeds)
            .claim(Claim::OutputLacks("hand-merge"))
            .claim(Claim::OutputLacks("readme-w")),
        ],
    )
}

/// A `merge` landing stopped on `app`'s composing merge, its project merge
/// staged and uncommitted.
fn landing_merge_stopped() -> Vec<Step> {
    vec![
        Step::rwv(&a(), &["branch", "set-history", "merge"]).expect(Exit::Succeeds),
        create("w"),
        Step::hand_edit(&ww("w"), Repo::App, "README", "app\n", "app in w\n"),
        Step::git(&ww("w"), Repo::App, &["commit", "-qam", "readme-w"]),
        Step::rwv(&ww("w"), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::hand_edit(&a(), Repo::App, "README", "app\n", "app on main\n"),
        Step::git(&a(), Repo::App, &["commit", "-qam", "readme-main"]),
        Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::rwv(&ww("w"), &["land"]).expect(Exit::refuses_kind(RefusalKind::OpParked)),
        Step::git(
            &ww("w"),
            Repo::App,
            &["rev-parse", "-q", "--verify", "MERGE_HEAD"],
        )
        .expect(Exit::Succeeds),
    ]
}

/// [`landing_merge_stopped`], resolved and continued: the project gets one
/// commit, the merge whose first parent is `main`'s tip, and its lock pins
/// `app`'s landing merge.
pub fn landing_merge_continued_writes_one_project_merge() -> Scenario {
    let mut steps = landing_merge_stopped();
    steps.extend([
        Step::git(&ww("w"), Repo::App, &["checkout", "--ours", "README"]),
        Step::git(&ww("w"), Repo::App, &["add", "README"]),
        Step::rwv_printed(&ww("w"), "rwv land --continue").expect(Exit::Succeeds),
        Step::git(&a(), Repo::App, &["log", "-1", "--format=%s", "main^1"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputLine("readme-main")),
        Step::git(&a(), Repo::Proj, &["log", "-1", "--format=%s", "main^2"]).expect(Exit::Succeeds),
        Step::git(&a(), Repo::Proj, &["log", "-1", "--format=%B", "main"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputContains("Merge ")),
    ]);
    scenario("a landing merge continued writes one project merge", steps)
}

/// A landing stopped in its own merge in `app`, the merge having also
/// brought in `main-clean.txt` cleanly; the operator edits that file and
/// does not stage it, so the VCS's own cancel of the merge would refuse.
/// `rwv abort` sees that before anything moves and refuses whole; the
/// printed `--abandon-foreign-tip` keeps the edit as a backup and ends the
/// landing.
pub fn abort_of_a_landing_over_a_cancel_the_vcs_refuses() -> Scenario {
    let mut steps = vec![
        Step::rwv(&a(), &["branch", "set-history", "merge"]).expect(Exit::Succeeds),
        create("w"),
        Step::hand_edit(&ww("w"), Repo::App, "README", "app\n", "app in w\n"),
        Step::git(&ww("w"), Repo::App, &["commit", "-qam", "readme-w"]),
        Step::rwv(&ww("w"), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::hand_edit(&a(), Repo::App, "README", "app\n", "app on main\n"),
        Step::git(&a(), Repo::App, &["commit", "-qam", "readme-main"]),
        Step::commit(&a(), Repo::App, "main-clean"),
        Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::rwv(&ww("w"), &["land"]).expect(Exit::refuses_kind(RefusalKind::OpParked)),
    ];
    steps.extend([
        Step::hand_edit(
            &ww("w"),
            Repo::App,
            "main-clean.txt",
            "main-clean",
            "edited mid-merge",
        ),
        Step::rwv(&ww("w"), &["abort"])
            .expect(Exit::refuses_kind(RefusalKind::ForeignTip))
            .claim(Claim::OutputContains("could not be canceled"))
            .claim(Claim::OutputLacks("abort stopped there"))
            .claim(Claim::LocalStateUnchanged(a()))
            .claim(Claim::LocalStateUnchanged(ww("w"))),
        Step::rwv_printed(&ww("w"), "rwv abort --abandon-foreign-tip")
            .expect(Exit::Succeeds)
            .claim(Claim::OutputContains(
                "its uncommitted edits are kept as backup",
            ))
            .claim(Claim::LocalStateUnchanged(a())),
        Step::git(
            &ww("w"),
            Repo::App,
            &["rev-parse", "-q", "--verify", "MERGE_HEAD"],
        )
        .expect(Exit::Code(1)),
        Step::git(&ww("w"), Repo::App, &["log", "-1", "--format=%s"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputLine("readme-w")),
        Step::rwv(&ww("w"), &["status"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputLacks("in progress")),
    ]);
    scenario("abort of a landing over a cancel the VCS refuses", steps)
}

/// Which of a landing's replayed project commits stops on a conflict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplayedCommit {
    First,
    Second,
}

/// A linear landing whose project branch carries two commits, `stop` of
/// them conflicting with a commit `main` took since: the replay stops there,
/// and `rwv abort` puts both workspaces back as they were.
pub fn abort_of_a_landing_stopped_in_a_replayed_project_commit(stop: ReplayedCommit) -> Scenario {
    let (first, second) = match stop {
        ReplayedCommit::First => ("clash.txt", "calm.txt"),
        ReplayedCommit::Second => ("calm.txt", "clash.txt"),
    };
    scenario(
        "abort of a landing stopped in a replayed project commit",
        vec![
            create("w"),
            Step::commit(&ww("w"), Repo::App, "w-app"),
            Step::rwv(&ww("w"), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::hand_commit(&ww("w"), Repo::Proj, "proj--w", &[(first, "from w\n")]),
            Step::hand_commit(&ww("w"), Repo::Proj, "proj--w", &[(second, "from w\n")]),
            Step::git(&ww("w"), Repo::Proj, &["reset", "-q", "--hard"]),
            Step::hand_commit(&a(), Repo::Proj, "main", &[("clash.txt", "from main\n")]),
            Step::git(&a(), Repo::Proj, &["reset", "-q", "--hard"]),
            Step::rwv(&ww("w"), &["land", "--history", "linear"])
                .expect(Exit::refuses_kind(RefusalKind::OpParked)),
            Step::rwv(&ww("w"), &["abort"])
                .expect(Exit::Succeeds)
                .may_rewind(&[Repo::App, Repo::Proj])
                .may_restore(&[Repo::App, Repo::Proj])
                .claim(Claim::LocalStateUnchanged(a())),
            Step::git(&ww("w"), Repo::Proj, &["status", "--porcelain"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputLacks("clash")),
            Step::rwv(&ww("w"), &["status"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputLacks("in progress")),
        ],
    )
}

/// [`landing_merge_stopped`], then `rwv abort`: every merge in flight is
/// canceled and both workspaces stand as before.
pub fn abort_after_a_landing_merge_stops() -> Scenario {
    let mut steps = landing_merge_stopped();
    steps.extend([
        Step::rwv(&ww("w"), &["abort"])
            .expect(Exit::Succeeds)
            .claim(Claim::LocalStateUnchanged(a())),
        Step::git(
            &ww("w"),
            Repo::App,
            &["rev-parse", "-q", "--verify", "MERGE_HEAD"],
        )
        .expect(Exit::Code(1)),
        Step::git(
            &ww("w"),
            Repo::Proj,
            &["rev-parse", "-q", "--verify", "MERGE_HEAD"],
        )
        .expect(Exit::Code(1)),
        Step::git(&ww("w"), Repo::App, &["status", "--porcelain"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputLacks("README")),
        Step::git(&ww("w"), Repo::App, &["log", "-1", "--format=%s"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputLine("readme-w")),
    ]);
    scenario("abort after a landing's merge stops", steps)
}

/// The stop of [`sync_stopped_then_lib_committed`] before anything is
/// resolved, and `rwv abort`: `app`'s merge is canceled.
pub fn abort_after_a_sync_merge_stops() -> Scenario {
    let mut steps: Vec<Step> = sync_stopped_then_lib_committed("merge")
        .into_iter()
        .take(10)
        .collect();
    steps.extend([
        Step::rwv(&ww("w"), &["abort"]).expect(Exit::Succeeds),
        Step::git(
            &ww("w"),
            Repo::App,
            &["rev-parse", "-q", "--verify", "MERGE_HEAD"],
        )
        .expect(Exit::Code(1)),
        Step::git(&ww("w"), Repo::App, &["log", "-1", "--format=%s"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputLine("readme-w")),
    ]);
    scenario("abort after a sync's merge stops", steps)
}

/// `dc04-conflict-setter`: both sides change `lib`'s declared branch and its
/// `history`; a `sync` that merges `main` in stops on the `rwv.toml`
/// conflict, printing each field's setter with each side's value. The
/// setters then settle the entry and stage the file, so `--continue`
/// completes with no hand edit: a known defect, since no setter yet reads an
/// `rwv.toml` a merge left conflicted.
pub fn dc04_conflict_setter() -> Scenario {
    scenario(
        "dc04 a conflicted entry is settled by its setters",
        vec![
            create("w"),
            Step::rwv(&ww("w"), &["member", "set-branch", LIB, "lib-w", "--new"])
                .expect(Exit::Succeeds),
            Step::rwv(&ww("w"), &["branch", "set-history", "merge", "--repo", LIB])
                .expect(Exit::Succeeds),
            Step::rwv(&a(), &["member", "set-branch", LIB, "lib-main", "--new"])
                .expect(Exit::Succeeds),
            Step::rwv(&a(), &["branch", "set-history", "linear", "--repo", LIB])
                .expect(Exit::Succeeds),
            Step::rwv(&ww("w"), &["sync", "--history", "merge"])
                .expect(Exit::refuses_kind(RefusalKind::OpParked))
                .claim(Claim::OutputContains(
                    "rwv member set-branch local/remotes/lib lib-w",
                ))
                .claim(Claim::OutputContains(
                    "rwv member set-branch local/remotes/lib lib-main",
                ))
                .claim(Claim::OutputContains(
                    "rwv branch set-history merge --repo local/remotes/lib",
                ))
                .claim(Claim::OutputContains(
                    "rwv branch set-history linear --repo local/remotes/lib",
                )),
            Step::rwv(&ww("w"), &["member", "set-branch", LIB, "lib-w"])
                .expect(Exit::Defect(Defect::SettersCannotReadAConflictedManifest)),
            Step::rwv(&ww("w"), &["branch", "set-history", "merge", "--repo", LIB])
                .expect(Exit::Defect(Defect::SettersCannotReadAConflictedManifest)),
            Step::rwv(&ww("w"), &["sync", "--continue"])
                .expect(Exit::Defect(Defect::SettersCannotReadAConflictedManifest)),
        ],
    )
}

/// Another machine moved `lib`'s pin on `main`, and this machine's `lib`
/// holds an untracked file at a path that move writes. The landing takes the
/// upstream in and lands; aligning the target's `lib` stops on the file with
/// no judged state, so the landing names the `rwv update` that finishes it,
/// which does once the file is moved aside.
pub fn land_names_the_update_that_finishes_an_alignment_it_could_not_judge() -> Scenario {
    scenario(
        "land names the update that finishes an alignment it could not judge",
        vec![
            create("w"),
            Step::new_machine(Machine::B),
            Step::commit(&b(), Repo::Lib, "b-lib"),
            Step::rwv(&b(), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::git(&b(), Repo::Lib, &["push", "origin", "main"])
                .pushes(Remote::origin(Repo::Lib), "main"),
            Step::git(&b(), Repo::Proj, &["push", "origin", "main"])
                .pushes(Remote::origin(Repo::Proj), "main"),
            Step::rwv(&a(), &["fetch"]).expect(Exit::Succeeds),
            Step::git(
                &a(),
                Repo::Lib,
                &["checkout", "origin/main", "--", "b-lib.txt"],
            ),
            Step::hand_edit(&a(), Repo::Lib, "b-lib.txt", "b-lib", "a-lib"),
            Step::git(&a(), Repo::Lib, &["reset", "-q"]),
            Step::commit(&ww("w"), Repo::App, "w-app"),
            Step::rwv(&ww("w"), &["land", "--history", "linear"])
                .expect(Exit::Succeeds)
                .claim(Claim::Contains {
                    at: a(),
                    repo: Repo::App,
                    rev: "main",
                    mark: "w-app",
                })
                .claim(Claim::OutputContains(
                    "did not finish aligning; `rwv update --project proj` there finishes it:",
                ))
                .claim(Claim::Lacks {
                    at: a(),
                    repo: Repo::Lib,
                    rev: "HEAD",
                    mark: "b-lib",
                }),
            Step::git(&a(), Repo::Lib, &["clean", "-q", "-f", "--", "b-lib.txt"]),
            Step::rwv(&a(), &["update", "--project", "proj"])
                .expect(Exit::Succeeds)
                .claim(Claim::Contains {
                    at: a(),
                    repo: Repo::Lib,
                    rev: "HEAD",
                    mark: "b-lib",
                })
                .claim(Claim::AtPin {
                    at: a(),
                    repo: Repo::Lib,
                }),
        ],
    )
}

/// A member range holding a merge: the printed merge, run in the member's
/// checkout, ports it by hand, and the printed rerun leaving the member out
/// ports the rest.
pub fn cherry_pick_runs_the_printed_merge() -> Scenario {
    let mut steps = stable_lines();
    steps.extend([
        Step::git(&a(), Repo::App, &["branch", "side"]),
        Step::hand_commit(&a(), Repo::App, "side", &[("side.txt", "side\n")]),
        Step::git(
            &a(),
            Repo::App,
            &["merge", "--no-ff", "-m", "app-merge", "side"],
        ),
        Step::commit(&a(), Repo::Lib, "lib-fix"),
        Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
    ]);
    steps.extend(feature_onto_stable());
    steps.extend([
        Step::rwv(&ww("p"), &["cherry-pick", "main"])
            .expect(Exit::refuses(Repo::App, State::PortMergeInRange))
            .claim(Claim::LocalStateUnchanged(ww("p"))),
        Step::rwv_printed_holding(&ww("p"), "git -C ", " merge ").expect(Exit::Succeeds),
        Step::git(&ww("p"), Repo::App, &["show", "HEAD:side.txt"]).claim(Claim::OutputLine("side")),
        Step::rwv(&ww("p"), &["cherry-pick", "main", "--except", APP])
            .expect(Exit::Succeeds)
            .claim(Claim::AtPin {
                at: ww("p"),
                repo: Repo::App,
            })
            .claim(Claim::AtPin {
                at: ww("p"),
                repo: Repo::Lib,
            }),
        Step::git(&ww("p"), Repo::Lib, &["log", "-1", "--format=%s"])
            .claim(Claim::OutputLine("lib-fix")),
    ]);
    scenario("cherry-pick runs the printed merge", steps)
}

/// A commit moving a `dependency`'s pin backward: the printed `member pin`
/// re-pins it by name here, and the printed rerun leaving it out ports the
/// rest.
pub fn cherry_pick_runs_the_printed_pin_of_a_dependency_moved_backward() -> Scenario {
    let mut steps = stable_lines();
    steps.extend([
        Step::commit(&a(), Repo::Docs, "docs-1"),
        Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::commit(&a(), Repo::App, "app-fix"),
        Step::rwv(&a(), &["lock"]).expect(Exit::Succeeds),
        Step::rwv(&a(), &["member", "pin", DOCS, "HEAD~1", "--commit"])
            .expect(Exit::Succeeds)
            .may_rewind(&[Repo::Docs]),
    ]);
    steps.extend(feature_onto_stable());
    steps.extend([
        Step::rwv(&ww("p"), &["cherry-pick", "main"])
            .expect(Exit::refuses(Repo::Docs, State::PortBackward))
            .claim(Claim::OutputContains("rwv member pin local/remotes/docs "))
            .claim(Claim::LocalStateUnchanged(ww("p"))),
        Step::rwv_printed(&ww("p"), "rwv member pin ").expect(Exit::Succeeds),
        Step::rwv(&ww("p"), &["cherry-pick", "main", "--except", DOCS])
            .expect(Exit::Succeeds)
            .claim(Claim::AtPin {
                at: ww("p"),
                repo: Repo::Docs,
            }),
    ]);
    Scenario {
        name: "cherry-pick runs the printed pin of a dependency moved backward",
        seed: Seed::default().role(Repo::Docs, Role::Dependency),
        steps,
    }
}

/// A commit moving the pin of a member the feature does not list: the
/// printed `member add` lists it, leaving its checkout behind the pin it
/// records; the printed `rwv update` aligns it, and the pick then ports.
pub fn cherry_pick_runs_the_printed_add_of_a_member_not_listed() -> Scenario {
    let mut steps = stable_lines();
    steps.extend(feature_onto_stable());
    steps.extend([
        Step::rwv(&ww("p"), &["member", "remove", DOCS]).expect(Exit::Succeeds),
        Step::rwv(&a(), &["switch", "main"]).expect(Exit::Succeeds),
        Step::commit(&a(), Repo::Docs, "docs-up"),
        Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::rwv(&ww("p"), &["cherry-pick", "main"])
            .expect(Exit::refuses(Repo::Docs, State::PortNotListed))
            .claim(Claim::LocalStateUnchanged(ww("p"))),
        Step::rwv_printed(&ww("p"), "rwv member add ").expect(Exit::Succeeds),
        Step::rwv(&ww("p"), &["cherry-pick", "main"])
            .expect(Exit::refuses(Repo::Docs, State::PinBehind))
            .claim(Claim::LocalStateUnchanged(ww("p"))),
        Step::rwv_printed(&ww("p"), "rwv update").expect(Exit::Succeeds),
        Step::rwv(&ww("p"), &["cherry-pick", "main"])
            .expect(Exit::Succeeds)
            .claim(Claim::PinContains {
                site: LockSite::Checkout(ww("p"), Repo::Proj, "proj--p".to_owned()),
                member: Repo::Docs,
                mark: "docs-up",
            }),
    ]);
    scenario(
        "cherry-pick runs the printed add of a member not listed",
        steps,
    )
}

/// [`cherry_pick_runs_the_printed_add_of_a_member_not_listed`] for a
/// `dependency`: the printed add restores the member with its role, so the
/// pick then carries its pin by name.
pub fn cherry_pick_runs_the_printed_add_of_a_dependency_not_listed() -> Scenario {
    let mut steps = stable_lines();
    steps.extend(feature_onto_stable());
    steps.extend([
        Step::rwv(&ww("p"), &["member", "remove", DOCS]).expect(Exit::Succeeds),
        Step::rwv(&a(), &["switch", "main"]).expect(Exit::Succeeds),
        Step::commit(&a(), Repo::Docs, "docs-up"),
        Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::rwv(&ww("p"), &["cherry-pick", "main"])
            .expect(Exit::refuses(Repo::Docs, State::PortNotListed))
            .claim(Claim::OutputContains("--role dependency")),
        Step::rwv_printed(&ww("p"), "rwv member add ").expect(Exit::Succeeds),
        Step::git(&ww("p"), Repo::Proj, &["diff", "HEAD", "--", "rwv.toml"]).expect(Exit::Succeeds),
        Step::rwv(&ww("p"), &["cherry-pick", "main"])
            .expect(Exit::Succeeds)
            .claim(Claim::PinContains {
                site: LockSite::Checkout(ww("p"), Repo::Proj, "proj--p".to_owned()),
                member: Repo::Docs,
                mark: "docs-up",
            }),
    ]);
    Scenario {
        name: "cherry-pick runs the printed add of a dependency not listed",
        seed: Seed::default().role(Repo::Docs, Role::Dependency),
        steps,
    }
}

/// `--except` records a member's checked-out tip as `lock` would, so a
/// member checked out off its branch is refused for its alignment before
/// anything is recorded.
pub fn cherry_pick_except_judges_the_member_for_alignment() -> Scenario {
    let mut steps = stable_lines();
    steps.extend([
        Step::commit(&a(), Repo::App, "main-app"),
        Step::commit(&a(), Repo::Lib, "main-lib"),
        Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
    ]);
    steps.extend(feature_onto_stable());
    steps.extend([
        Step::git(&ww("p"), Repo::App, &["switch", "-qc", "try"]),
        Step::rwv(&ww("p"), &["cherry-pick", "main", "--except", APP])
            .expect(Exit::refuses(Repo::App, State::AlignmentOnBranch))
            .claim(Claim::LocalStateUnchanged(ww("p"))),
    ]);
    scenario("cherry-pick except judges the member for alignment", steps)
}

/// A member to copy onto holds a commit no lock reaches: the printed relock
/// records it, and the pick then ports.
pub fn cherry_pick_runs_the_printed_relock_of_an_unpinned_member() -> Scenario {
    let mut steps = stable_lines();
    steps.extend([
        Step::commit(&a(), Repo::App, "main-app"),
        Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
    ]);
    steps.extend(feature_onto_stable());
    steps.extend([
        Step::commit(&ww("p"), Repo::App, "p-app"),
        Step::rwv(&ww("p"), &["cherry-pick", "main"])
            .expect(Exit::refuses(Repo::App, State::PinUnpinned))
            .claim(Claim::LocalStateUnchanged(ww("p"))),
        Step::rwv_printed(&ww("p"), "rwv lock").expect(Exit::Succeeds),
        Step::rwv(&ww("p"), &["cherry-pick", "main"])
            .expect(Exit::Succeeds)
            .claim(Claim::PinContains {
                site: LockSite::Checkout(ww("p"), Repo::Proj, "proj--p".to_owned()),
                member: Repo::App,
                mark: "p-app",
            }),
        Step::git(&ww("p"), Repo::App, &["log", "-1", "--format=%s"])
            .claim(Claim::OutputLine("main-app")),
    ]);
    scenario(
        "cherry-pick runs the printed relock of an unpinned member",
        steps,
    )
}

/// A commit pinning a member to copy onto at a commit this clone lacks is
/// refused before anything moves. Every choice it prints names a value the
/// operator chooses, so none runs as printed.
pub fn cherry_pick_refuses_a_pin_this_clone_lacks() -> Scenario {
    let mut steps = stable_lines();
    steps.extend(feature_onto_stable());
    steps.extend([
        Step::new_machine(Machine::B),
        Step::commit(&b(), Repo::App, "b-app"),
        Step::rwv(&b(), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::git(&b(), Repo::Proj, &["push", "origin", "main"])
            .pushes(Remote::origin(Repo::Proj), "main")
            .may_leave_unresolved(&[Repo::App]),
        Step::git(&ww("p"), Repo::Proj, &["fetch", "-q", "origin"])
            .may_leave_unresolved(&[Repo::App]),
        Step::rwv(&ww("p"), &["cherry-pick", "origin/main"])
            .expect(Exit::refuses(Repo::App, State::PinUnreachable))
            .may_leave_unresolved(&[Repo::App])
            .claim(Claim::LocalStateUnchanged(ww("p"))),
    ]);
    scenario("cherry-pick refuses a pin this clone lacks", steps)
}
/// `history-rebase-pin-remap`: a seat whose project branch holds two relock
/// commits, each pinning `app` at a commit of its own, takes in a moved
/// `linear` `main` and later lands into it once more moved: after each
/// rebase every replayed project commit pins what the `app` rebase made of
/// the commit it pinned, and the landing's lineage rule reads those pins.
pub fn history_rebase_pin_remap() -> Scenario {
    let pins_both = |at: Place, tip: &'static str, before: &'static str| {
        [
            Claim::PinsReplayed {
                at: at.clone(),
                rev: tip,
                member: Repo::App,
                subject: "w-app-2",
                reached_from: "HEAD",
            },
            Claim::PinsReplayed {
                at,
                rev: before,
                member: Repo::App,
                subject: "w-app-1",
                reached_from: "HEAD",
            },
        ]
    };
    let [sync_tip, sync_before] = pins_both(ww("w"), "HEAD", "HEAD~1");
    let [land_tip, land_before] = pins_both(a(), "main", "main~1");
    scenario(
        "history: a rebased project branch pins what the member rebases made",
        vec![
            Step::rwv(&a(), &["branch", "set-history", "linear"]).expect(Exit::Succeeds),
            create("w"),
            Step::commit(&ww("w"), Repo::App, "w-app-1"),
            Step::rwv(&ww("w"), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::commit(&ww("w"), Repo::App, "w-app-2"),
            Step::rwv(&ww("w"), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::commit(&a(), Repo::App, "main-app-1"),
            Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::rwv(&ww("w"), &["sync"])
                .expect(Exit::Succeeds)
                .may_strand(&[Repo::App, Repo::Proj])
                .claim(Claim::OutputContains("local/remotes/app: rebased onto"))
                .claim(sync_tip)
                .claim(sync_before),
            Step::commit(&a(), Repo::App, "main-app-2"),
            Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::rwv(&ww("w"), &["land", "--retire"])
                .expect(Exit::Succeeds)
                .may_strand(&[Repo::App, Repo::Proj])
                .claim(Claim::OutputContains("local/remotes/app: rebased"))
                .claim(Claim::OutputLacks("off lineage"))
                .claim(Claim::WorkweaveGone(ww("w")))
                .claim(land_tip)
                .claim(land_before),
        ],
    )
}

/// `history-rebase-pin-remap`'s sync, killed after the re-map recorded
/// what it made of the replay and before the branch moved to it: the
/// printed `rwv sync --continue` finishes the sync, and the project branch
/// still pins what the `app` rebase made.
pub fn a_sync_killed_between_recording_a_remap_and_moving_the_branch_continues() -> Scenario {
    scenario(
        "history: a sync killed between recording a re-map and moving the branch continues",
        vec![
            Step::rwv(&a(), &["branch", "set-history", "linear"]).expect(Exit::Succeeds),
            create("w"),
            Step::commit(&ww("w"), Repo::App, "w-app-1"),
            Step::rwv(&ww("w"), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::commit(&ww("w"), Repo::App, "w-app-2"),
            Step::rwv(&ww("w"), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::commit(&a(), Repo::App, "main-app-1"),
            Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::rwv_paused(&ww("w"), &["sync"], "remap-recorded", "remapping")
                .may_strand(&[Repo::App, Repo::Proj])
                .may_rewind(&[Repo::App]),
            Step::kill("remapping"),
            Step::rwv(&ww("w"), &["sync", "--continue"])
                .expect(Exit::Succeeds)
                .may_strand(&[Repo::App, Repo::Proj])
                .claim(Claim::PinsReplayed {
                    at: ww("w"),
                    rev: "HEAD",
                    member: Repo::App,
                    subject: "w-app-2",
                    reached_from: "HEAD",
                }),
        ],
    )
}

/// `stack-carried`'s project carry, killed after the re-map recorded what it
/// made of the replay and before the checkout moved to it: the printed
/// `rwv sync --continue` composes it again.
pub fn a_sync_killed_between_recording_a_carry_remap_and_moving_to_it_continues() -> Scenario {
    let mut steps = stack_on_a_seat();
    steps.extend(main_moves_app("main-app"));
    steps.extend([
        Step::rwv_paused(&ww("w1"), &["sync"], "carry-recorded", "carrying"),
        Step::kill("carrying"),
        Step::rwv(&ww("w1"), &["sync", "--continue"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputContains("carried 'w2' along")),
        Step::git(&ww("w1"), Repo::Proj, &["symbolic-ref", "-q", "HEAD"]).expect(Exit::Succeeds),
        Step::git(
            &ww("w1"),
            Repo::App,
            &["merge-base", "--is-ancestor", "proj--w1", "w2"],
        )
        .expect(Exit::Succeeds),
    ]);
    scenario(
        "stack: a sync killed between recording a carry's re-map and moving to it continues",
        steps,
    )
}

/// Where `vcs-continue-mid-rebase`'s landing stops.
#[derive(Debug, Clone, Copy)]
pub enum MidRebase {
    /// On `app`'s rebase, at the first of its three commits.
    Member,
    /// On the project repo's rebase.
    Project,
}

/// What the operator runs once the VCS's own continue has finished the
/// rebase the landing stopped in.
#[derive(Debug, Clone, Copy)]
pub enum AfterHandContinue {
    Continue,
    Abort,
}

/// A `linear` landing stopped on a conflict at `stop`; the repo it stopped
/// in.
fn landing_stopped_mid_rebase(stop: MidRebase) -> (Vec<Step>, Repo) {
    let conflicted = match stop {
        MidRebase::Member => Repo::App,
        MidRebase::Project => Repo::Proj,
    };
    let mut steps = vec![
        Step::rwv(&a(), &["branch", "set-history", "linear"]).expect(Exit::Succeeds),
        Step::commit(&a(), Repo::Proj, "shared"),
        create("w"),
    ];
    match stop {
        MidRebase::Member => steps.extend([
            Step::hand_edit(&ww("w"), Repo::App, "README", "app", "beta"),
            Step::git(&ww("w"), Repo::App, &["commit", "-qam", "w-app-1"]),
            Step::rwv(&ww("w"), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::commit(&ww("w"), Repo::App, "w-app-2"),
            Step::rwv(&ww("w"), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::commit(&ww("w"), Repo::App, "w-app-3"),
            Step::rwv(&ww("w"), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::hand_edit(&a(), Repo::App, "README", "app", "alpha"),
            Step::git(&a(), Repo::App, &["commit", "-qam", "main-app"]),
        ]),
        MidRebase::Project => steps.extend([
            Step::commit(&ww("w"), Repo::App, "w-app-1"),
            Step::rwv(&ww("w"), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::hand_edit(&ww("w"), Repo::Proj, "shared.txt", "shared", "beta"),
            Step::git(&ww("w"), Repo::Proj, &["commit", "-qam", "w-proj"]),
            Step::hand_edit(&a(), Repo::Proj, "shared.txt", "shared", "alpha"),
            Step::git(&a(), Repo::Proj, &["commit", "-qam", "main-proj"]),
            Step::commit(&a(), Repo::App, "main-app"),
        ]),
    }
    steps.extend([
        Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::rwv(&ww("w"), &["land"])
            .expect(Exit::refuses_kind(RefusalKind::OpParked))
            .may_strand(&[Repo::App, Repo::Proj])
            .may_rewind(&[Repo::App])
            .claim(Claim::OutputContains("rwv land --continue"))
            .claim(Claim::OutputContains("rwv abort")),
    ]);
    (steps, conflicted)
}

/// [`landing_stopped_mid_rebase`], resolved by the operator and finished
/// with the VCS's own continue.
fn landing_finished_mid_rebase_by_hand(stop: MidRebase) -> (Vec<Step>, Repo) {
    let (mut steps, conflicted) = landing_stopped_mid_rebase(stop);
    let file = match stop {
        MidRebase::Member => "README",
        MidRebase::Project => "shared.txt",
    };
    steps.extend([
        Step::git(&ww("w"), conflicted, &["checkout", "--theirs", file]),
        Step::git(&ww("w"), conflicted, &["add", file]),
        Step::git(
            &ww("w"),
            conflicted,
            &["-c", "core.editor=true", "rebase", "--continue"],
        )
        .may_strand(&[conflicted])
        .may_rewind(&[Repo::App]),
    ]);
    (steps, conflicted)
}

/// `vcs-continue-mid-rebase`: a `linear` landing stops on a conflict; the
/// operator resolves it and runs the VCS's own continue to the end. Then
/// `rwv land --continue` converges with every replayed project commit
/// pinning the rebased `app`, or `rwv abort` puts the seat back, reading the
/// tip the hand continue wrote as the operation's own, losing nothing.
pub fn vcs_continue_mid_rebase(stop: MidRebase, then: AfterHandContinue) -> Scenario {
    let (mut steps, conflicted) = landing_finished_mid_rebase_by_hand(stop);
    match (stop, then) {
        (MidRebase::Member, AfterHandContinue::Continue) => steps.push(
            Step::rwv(&ww("w"), &["land", "--continue"])
                .expect(Exit::Succeeds)
                .may_strand(&[Repo::App, Repo::Proj])
                .claim(Claim::PinsReplayed {
                    at: a(),
                    rev: "main",
                    member: Repo::App,
                    subject: "w-app-3",
                    reached_from: "HEAD",
                })
                .claim(Claim::PinsReplayed {
                    at: a(),
                    rev: "main~1",
                    member: Repo::App,
                    subject: "w-app-2",
                    reached_from: "HEAD",
                })
                .claim(Claim::PinsReplayed {
                    at: a(),
                    rev: "main~2",
                    member: Repo::App,
                    subject: "w-app-1",
                    reached_from: "HEAD",
                }),
        ),
        (MidRebase::Project, AfterHandContinue::Continue) => steps.push(
            Step::rwv(&ww("w"), &["land", "--continue"])
                .expect(Exit::Succeeds)
                .may_strand(&[Repo::App, Repo::Proj])
                .claim(Claim::PinsReplayed {
                    at: a(),
                    rev: "main",
                    member: Repo::App,
                    subject: "w-app-1",
                    reached_from: "HEAD",
                })
                .claim(Claim::PinsReplayed {
                    at: a(),
                    rev: "main~1",
                    member: Repo::App,
                    subject: "w-app-1",
                    reached_from: "HEAD",
                }),
        ),
        (_, AfterHandContinue::Abort) => steps.extend([
            Step::rwv(&ww("w"), &["abort"])
                .expect(Exit::Succeeds)
                .may_strand(&[Repo::App, Repo::Proj])
                .may_rewind(&[Repo::App])
                .claim(Claim::OutputLacks("foreign")),
            Step::git(&ww("w"), conflicted, &["diff", "--quiet", "HEAD"]).expect(Exit::Succeeds),
            Step::git(
                &ww("w"),
                conflicted,
                &["log", "--format=%s", "-1", "proj--w"],
            )
            .expect(Exit::Succeeds)
            .claim(Claim::OutputContains(match stop {
                MidRebase::Member => "w-app-3",
                MidRebase::Project => "w-proj",
            }))
            .claim(Claim::OnBranch {
                at: ww("w"),
                repo: conflicted,
                branch: "proj--w",
            }),
            Step::git(&a(), Repo::App, &["log", "--format=%s", "-1", "main"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains("main-app")),
        ]),
    }
    scenario(
        "a landing stopped mid-rebase, finished by the VCS's own continue",
        steps,
    )
}

/// A landing stopped in `app`'s rebase, finished by the VCS's own continue,
/// and then the operator commits on top: abort cannot read that commit as a
/// replay of the operation's, so it refuses rather than resetting it onto no
/// ref, and the printed `--abandon-foreign-tip` keeps it as a backup.
pub fn vcs_continue_mid_rebase_then_an_own_commit() -> Scenario {
    let (mut steps, conflicted) = landing_finished_mid_rebase_by_hand(MidRebase::Member);
    steps.extend([
        Step::commit(&ww("w"), conflicted, "operator-own"),
        abort_refused_whole(
            Step::rwv(&ww("w"), &["abort"])
                .expect(Exit::refuses_kind(RefusalKind::ForeignTip))
                .claim(Claim::Contains {
                    at: ww("w"),
                    repo: conflicted,
                    rev: "HEAD",
                    mark: "operator-own",
                }),
            true,
            &[ww("w")],
        ),
        Step::rwv_printed(&ww("w"), "rwv abort --abandon-foreign-tip")
            .expect(Exit::Succeeds)
            .may_rewind(&[Repo::App])
            .may_restore(&[Repo::App, Repo::Proj])
            .claim(Claim::OutputContains("abandoned foreign tip")),
        Step::git(&ww("w"), conflicted, &["log", "--all", "--format=kept %s"])
            .claim(Claim::OutputContains("kept operator-own")),
        Step::git(
            &ww("w"),
            conflicted,
            &["log", "--format=%s", "-1", "proj--w"],
        )
        .claim(Claim::OutputLine("w-app-3")),
    ]);
    scenario(
        "a landing finished mid-rebase by hand, then an own commit, aborted",
        steps,
    )
}

/// A seat `w1` on a `linear` `main`, holding `w1-app`, and `w2`, a feature
/// onto `w1` holding `w2-app` and checked out nowhere: made and filled in
/// `w1`'s seat, which then switches back.
fn stack_on_a_seat() -> Vec<Step> {
    stack_on_a_seat_under("linear")
}

fn stack_on_a_seat_under(history: &'static str) -> Vec<Step> {
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
fn dependent_of(at: &Place, name: &'static str, onto: &'static str) -> Step {
    Step::rwv(at, &["branch", "create", name, "--target", onto]).expect(Exit::Succeeds)
}

fn main_moves_app(mark: &'static str) -> Vec<Step> {
    vec![
        Step::commit(&a(), Repo::App, mark),
        Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
    ]
}

/// `stack-carried`: `w1`'s `sync` under a moved `linear` `main` rebases
/// `w1` and carries `w2`, checked out nowhere, along: its branches hold
/// `main`'s new work and its own, a `rebased` backup keeps its old tips
/// until the sync completes, its project branch pins the carried `app`, and
/// `w2` then lands into `w1` by fast-forward.
pub fn stack_carried() -> Scenario {
    let mut steps = stack_on_a_seat();
    steps.extend(main_moves_app("main-app"));
    steps.extend([
        Step::rwv(&ww("w1"), &["sync"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputContains("carried 'w2' along"))
            .claim(Claim::Contains {
                at: ww("w1"),
                repo: Repo::App,
                rev: "w2",
                mark: "main-app",
            })
            .claim(Claim::PinsReplayed {
                at: ww("w1"),
                rev: "w2",
                member: Repo::App,
                subject: "w2-app",
                reached_from: "w2",
            })
            .claim(Claim::PinsReplayed {
                at: ww("w1"),
                rev: "proj--w1",
                member: Repo::App,
                subject: "w1-app",
                reached_from: "w2",
            }),
        Step::git(
            &ww("w1"),
            Repo::Proj,
            &["merge-base", "--is-ancestor", "proj--w1", "w2"],
        )
        .expect(Exit::Succeeds),
        Step::rwv(
            &a(),
            &[
                "workweave",
                "create",
                "--project",
                "proj",
                "w2s",
                "--branch",
                "w2",
            ],
        )
        .expect(Exit::Succeeds),
        Step::rwv(&ww("w2s"), &["land"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputLacks("rebased"))
            .claim(Claim::PinsReplayed {
                at: ww("w1"),
                rev: "HEAD",
                member: Repo::App,
                subject: "w2-app",
                reached_from: "HEAD",
            }),
    ]);
    scenario("stack: a dependent checked out nowhere is carried", steps)
}

/// `stack-two-dependents`: `w2` and `w3` both land into `w1`. While each is
/// open in a workweave, `w1`'s `sync` refuses with one state for each, its
/// own choices beside it; with neither checked out, the sync carries each
/// by its own replay, with its pins re-mapped.
pub fn stack_two_dependents() -> Scenario {
    let mut steps = stack_on_a_seat();
    steps.extend([
        dependent_of(&ww("w1"), "w3", "proj--w1"),
        Step::commit(&ww("w1"), Repo::App, "w3-app"),
        Step::rwv(&ww("w1"), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::rwv(&ww("w1"), &["switch", "proj--w1"]).expect(Exit::Succeeds),
    ]);
    steps.extend(main_moves_app("main-app"));
    let carried = |branch: &'static str, subject: &'static str| {
        [
            Claim::Contains {
                at: ww("w1"),
                repo: Repo::App,
                rev: branch,
                mark: "main-app",
            },
            Claim::PinsReplayed {
                at: ww("w1"),
                rev: branch,
                member: Repo::App,
                subject,
                reached_from: branch,
            },
        ]
    };
    let [w2_on_main, w2_pins] = carried("w2", "w2-app");
    let [w3_on_main, w3_pins] = carried("w3", "w3-app");
    for (ww_name, branch) in [("w2s", "w2"), ("w3s", "w3")] {
        steps.push(
            Step::rwv(
                &a(),
                &[
                    "workweave",
                    "create",
                    "--project",
                    "proj",
                    ww_name,
                    "--branch",
                    branch,
                ],
            )
            .expect(Exit::Succeeds),
        );
    }
    steps.push(
        Step::rwv(&ww("w1"), &["sync"])
            .expect(Exit::refuses(Repo::Proj, State::TargetDependentCheckedOut))
            .claim(Claim::OutputContains("(w2 lands into proj--w1)"))
            .claim(Claim::OutputContains("(w3 lands into proj--w1)"))
            .claim(Claim::OutputContains(
                "rwv workweave delete w2s --project proj",
            ))
            .claim(Claim::OutputContains(
                "rwv workweave delete w3s --project proj",
            ))
            .claim(Claim::LocalStateUnchanged(ww("w1"))),
    );
    for ww_name in ["w2s", "w3s"] {
        steps.push(
            Step::rwv(&a(), &["workweave", "delete", ww_name, "--project", "proj"])
                .expect(Exit::Code(3))
                .claim(Claim::WorkweaveGone(ww(ww_name))),
        );
    }
    steps.push(
        Step::rwv(&ww("w1"), &["sync"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputContains("carried 'w2' along"))
            .claim(Claim::OutputContains("carried 'w3' along"))
            .claim(w2_on_main)
            .claim(w2_pins)
            .claim(w3_on_main)
            .claim(w3_pins)
            .claim(Claim::Lacks {
                at: ww("w1"),
                repo: Repo::App,
                rev: "w3",
                mark: "w2-app",
            }),
    );
    scenario("stack: two dependents of one branch, each carried", steps)
}

/// `stack-transitive`: `w3` onto `w2` onto `w1`. With `w3` checked out in a
/// workweave of its own, `w1`'s `sync` refuses for `w3` and moves nothing,
/// `w2` included, though `w2` alone could be carried; with that workweave
/// gone, the sync carries both, `w2` before `w3`.
pub fn stack_transitive() -> Scenario {
    let mut steps = stack_on_a_seat();
    steps.extend([
        Step::rwv(&ww("w1"), &["switch", "w2"]).expect(Exit::Succeeds),
        dependent_of(&ww("w1"), "w3", "w2"),
        Step::commit(&ww("w1"), Repo::App, "w3-app"),
        Step::rwv(&ww("w1"), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::rwv(&ww("w1"), &["switch", "proj--w1"]).expect(Exit::Succeeds),
        Step::rwv(
            &a(),
            &[
                "workweave",
                "create",
                "--project",
                "proj",
                "w3s",
                "--branch",
                "w3",
            ],
        )
        .expect(Exit::Succeeds),
    ]);
    steps.extend(main_moves_app("main-app"));
    steps.extend([
        Step::rwv(&ww("w1"), &["sync"])
            .expect(Exit::refuses(Repo::Proj, State::TargetDependentCheckedOut))
            .claim(Claim::JudgedOnly(
                Repo::Proj,
                State::TargetDependentCheckedOut,
            ))
            .claim(Claim::OutputContains("w3 lands into w2"))
            .claim(Claim::LocalStateUnchanged(ww("w1")))
            .claim(Claim::LocalStateUnchanged(ww("w3s")))
            .claim(Claim::Lacks {
                at: ww("w1"),
                repo: Repo::App,
                rev: "w2",
                mark: "main-app",
            }),
        Step::rwv(&a(), &["workweave", "delete", "w3s", "--project", "proj"])
            .expect(Exit::Code(3))
            .claim(Claim::WorkweaveGone(ww("w3s")))
            .claim(Claim::OutputContains(
                "left: local/remotes/app w3: unlanded",
            )),
        Step::rwv(&ww("w1"), &["sync"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputInOrder(&[
                "carried 'w2' along",
                "carried 'w3' along",
            ]))
            .claim(Claim::Contains {
                at: ww("w1"),
                repo: Repo::App,
                rev: "w3",
                mark: "main-app",
            })
            .claim(Claim::PinsReplayed {
                at: ww("w1"),
                rev: "w3",
                member: Repo::App,
                subject: "w3-app",
                reached_from: "w3",
            }),
        Step::git(
            &ww("w1"),
            Repo::App,
            &["merge-base", "--is-ancestor", "w2", "w3"],
        )
        .expect(Exit::Succeeds),
    ]);
    scenario("stack: carried whole or not at all, parents first", steps)
}

/// `stack-merge-setting`: the same stack under a `merge` target, `w2` open
/// in a workweave of its own, which would block a rebase; `w1`'s landing
/// rewrites nothing, so `w2` never blocks and is not moved.
pub fn stack_merge_setting() -> Scenario {
    let mut steps = stack_on_a_seat_under("merge");
    steps.push(
        Step::rwv(
            &a(),
            &[
                "workweave",
                "create",
                "--project",
                "proj",
                "w2s",
                "--branch",
                "w2",
            ],
        )
        .expect(Exit::Succeeds),
    );
    steps.extend(main_moves_app("main-app"));
    steps.extend([Step::rwv(&ww("w1"), &["land"])
        .expect(Exit::Succeeds)
        .claim(Claim::OutputLacks("dependent"))
        .claim(Claim::OutputLacks("carried"))
        .claim(Claim::Lacks {
            at: ww("w1"),
            repo: Repo::App,
            rev: "w2",
            mark: "main-app",
        })]);
    scenario("stack: a merge setting never blocks on dependents", steps)
}

/// How `stack-checked-out`'s refusal is cleared.
#[derive(Debug, Clone, Copy)]
pub enum CheckedOutChoice {
    /// C12: `w2` rebound onto `main`.
    Rebind,
    /// C35, then its landed form: `w2` landed into `w1`, then retired.
    LandThenRetire,
}

/// `stack-checked-out`: `w2` in a workweave of its own; `w1`'s `sync`
/// moves nothing and prints `w2`'s state with its choices, no sequence;
/// each choice, run as printed, then lets the rerun complete.
pub fn stack_checked_out(choice: CheckedOutChoice) -> Scenario {
    let mut steps = vec![
        Step::rwv(&a(), &["branch", "set-history", "linear"]).expect(Exit::Succeeds),
        create("w1"),
        Step::commit(&ww("w1"), Repo::App, "w1-app"),
        Step::rwv(&ww("w1"), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::rwv(
            &ww("w1"),
            &["workweave", "create", "w2", "--new-branch", "w2"],
        )
        .expect(Exit::Succeeds),
        Step::commit(&ww("w2"), Repo::App, "w2-app"),
        Step::rwv(&ww("w2"), &["lock", "--commit"]).expect(Exit::Succeeds),
    ];
    steps.extend(main_moves_app("main-app"));
    steps.push(
        Step::rwv(&ww("w1"), &["sync"])
            .expect(Exit::refuses(Repo::Proj, State::TargetDependentCheckedOut))
            .claim(Claim::JudgedOnly(
                Repo::Proj,
                State::TargetDependentCheckedOut,
            ))
            .claim(Claim::OutputContains("w2 lands into proj--w1"))
            .claim(Claim::OutputContains("rwv land"))
            .claim(Claim::OutputContains(
                "rwv branch set-mode w2 feature --target main",
            ))
            .claim(Claim::OutputContains("rwv workweave delete w2"))
            .claim(Claim::OutputLacks("then run"))
            .claim(Claim::LocalStateUnchanged(ww("w1")))
            .claim(Claim::LocalStateUnchanged(ww("w2"))),
    );
    match choice {
        CheckedOutChoice::Rebind => steps
            .push(Step::rwv_printed(&ww("w1"), "rwv branch set-mode w2 ").expect(Exit::Succeeds)),
        CheckedOutChoice::LandThenRetire => steps.extend([
            Step::rwv_printed_exactly(&ww("w2"), "rwv land").expect(Exit::Succeeds),
            Step::rwv(&ww("w1"), &["sync"])
                .expect(Exit::refuses(Repo::Proj, State::TargetDependentCheckedOut)),
            Step::rwv_printed_exactly(&ww("w2"), "rwv land --retire")
                .expect(Exit::Succeeds)
                .claim(Claim::WorkweaveGone(ww("w2"))),
        ]),
    }
    steps.push(
        Step::rwv(&ww("w1"), &["sync"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputLacks("carried"))
            .claim(Claim::Contains {
                at: ww("w1"),
                repo: Repo::App,
                rev: "HEAD",
                mark: "main-app",
            }),
    );
    scenario("stack: a dependent checked out elsewhere", steps)
}

/// `stack-not-containing`: `w2`, in a workweave of its own, was cut from an
/// older commit of `w1`; `w1`'s `sync` prints `w2` as lacking `w1`'s tip,
/// with C35, C12 and C36; C36, run in `w2`'s workspace, takes `w1` in, and
/// the rerun shows the state that remains, checked out elsewhere.
pub fn stack_not_containing() -> Scenario {
    let mut steps = vec![
        Step::rwv(&a(), &["branch", "set-history", "linear"]).expect(Exit::Succeeds),
        create("w1"),
        Step::commit(&ww("w1"), Repo::App, "w1-app-1"),
        Step::rwv(&ww("w1"), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::rwv(
            &ww("w1"),
            &["workweave", "create", "w2", "--new-branch", "w2"],
        )
        .expect(Exit::Succeeds),
        Step::commit(&ww("w2"), Repo::App, "w2-app"),
        Step::rwv(&ww("w2"), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::commit(&ww("w1"), Repo::App, "w1-app-2"),
        Step::rwv(&ww("w1"), &["lock", "--commit"]).expect(Exit::Succeeds),
    ];
    steps.extend(main_moves_app("main-app"));
    steps.extend([
        Step::rwv(&ww("w1"), &["sync"])
            .expect(Exit::refuses(Repo::Proj, State::TargetDependentLacksTip))
            .claim(Claim::JudgedOnly(
                Repo::Proj,
                State::TargetDependentLacksTip,
            ))
            .claim(Claim::OutputContains("rwv land"))
            .claim(Claim::OutputContains(
                "rwv branch set-mode w2 feature --target main",
            ))
            .claim(Claim::OutputContains("rwv sync"))
            .claim(Claim::LocalStateUnchanged(ww("w1")))
            .claim(Claim::LocalStateUnchanged(ww("w2"))),
        Step::rwv_printed_exactly(&ww("w2"), "rwv sync").expect(Exit::Succeeds),
        Step::rwv(&ww("w1"), &["sync"])
            .expect(Exit::refuses(Repo::Proj, State::TargetDependentCheckedOut))
            .claim(Claim::JudgedOnly(
                Repo::Proj,
                State::TargetDependentCheckedOut,
            )),
    ]);
    scenario("stack: a dependent cut from an older commit", steps)
}

/// `stack-carried` where `w2`'s replay conflicts with `main`'s new work: the
/// sync stops in `w1`'s checkout of `app` with `w2` not moved; the operator
/// resolves it and continues, and `w2` is carried, or aborts, and `w1` and
/// `w2` are as they were.
pub fn stack_carry_conflict(then: AfterHandContinue) -> Scenario {
    let mut steps = vec![
        Step::rwv(&a(), &["branch", "set-history", "linear"]).expect(Exit::Succeeds),
        create("w1"),
        Step::commit(&ww("w1"), Repo::App, "w1-app"),
        Step::rwv(&ww("w1"), &["lock", "--commit"]).expect(Exit::Succeeds),
        dependent_of(&ww("w1"), "w2", "proj--w1"),
        Step::hand_edit(&ww("w1"), Repo::App, "README", "app", "beta"),
        Step::git(&ww("w1"), Repo::App, &["commit", "-qam", "w2-app"]),
        Step::rwv(&ww("w1"), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::rwv(&ww("w1"), &["switch", "proj--w1"]).expect(Exit::Succeeds),
        Step::hand_edit(&a(), Repo::App, "README", "app", "alpha"),
        Step::git(&a(), Repo::App, &["commit", "-qam", "main-app"]),
        Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::rwv(&ww("w1"), &["sync"])
            .expect(Exit::refuses_kind(RefusalKind::OpParked))
            .claim(Claim::OutputContains(
                "carrying 'w2' along in local/remotes/app",
            ))
            .claim(Claim::OutputContains("rwv sync --continue"))
            .claim(Claim::OutputContains("rwv abort")),
        Step::git(&ww("w1"), Repo::App, &["log", "--format=%s", "-1", "w2"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputLine("w2-app")),
    ];
    match then {
        AfterHandContinue::Continue => steps.extend([
            Step::git(&ww("w1"), Repo::App, &["checkout", "--theirs", "README"]),
            Step::git(&ww("w1"), Repo::App, &["add", "README"]),
            Step::rwv(&ww("w1"), &["sync", "--continue"])
                .expect(Exit::Succeeds)
                .resolved_by_hand(&[Repo::App])
                .may_rewind(&[Repo::App])
                .claim(Claim::OutputContains("carried 'w2' along"))
                .claim(Claim::OnBranch {
                    at: ww("w1"),
                    repo: Repo::App,
                    branch: "proj--w1",
                })
                .claim(Claim::PinsReplayed {
                    at: ww("w1"),
                    rev: "w2",
                    member: Repo::App,
                    subject: "w2-app",
                    reached_from: "w2",
                }),
            Step::git(
                &ww("w1"),
                Repo::App,
                &["merge-base", "--is-ancestor", "proj--w1", "w2"],
            )
            .expect(Exit::Succeeds),
        ]),
        AfterHandContinue::Abort => steps.extend([
            Step::rwv(&ww("w1"), &["abort"])
                .expect(Exit::Succeeds)
                .may_rewind(&[Repo::App])
                .claim(Claim::OutputLacks("foreign"))
                .claim(Claim::OnBranch {
                    at: ww("w1"),
                    repo: Repo::App,
                    branch: "proj--w1",
                }),
            Step::git(
                &ww("w1"),
                Repo::App,
                &["log", "--format=%s", "-1", "proj--w1"],
            )
            .expect(Exit::Succeeds)
            .claim(Claim::OutputLine("w1-app")),
            Step::git(&ww("w1"), Repo::App, &["diff", "--quiet", "HEAD"]).expect(Exit::Succeeds),
            Step::git(&ww("w1"), Repo::App, &["log", "--format=%s", "-1", "w2"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputLine("w2-app")),
        ]),
    }
    scenario(
        "stack: a carry that conflicts stops before any dependent moves",
        steps,
    )
}

/// `stack-carry-conflict` with the conflict in the project repo: `w2` and
/// `main` both edit a project file. The sync stops carrying `w2` in
/// the project checkout, which the replay leaves detached; once the operator
/// resolves it, the printed `rwv sync --continue` carries `w2` and puts the
/// checkout back on `w1`'s branch.
pub fn a_carry_stopped_in_the_project_continues() -> Scenario {
    let mut steps = vec![
        Step::rwv(&a(), &["branch", "set-history", "linear"]).expect(Exit::Succeeds),
        Step::commit(&a(), Repo::Proj, "notes"),
        create("w1"),
        Step::commit(&ww("w1"), Repo::App, "w1-app"),
        Step::rwv(&ww("w1"), &["lock", "--commit"]).expect(Exit::Succeeds),
        dependent_of(&ww("w1"), "w2", "proj--w1"),
        Step::hand_edit(
            &ww("w1"),
            Repo::Proj,
            "notes.txt",
            "notes\n",
            "notes on w2\n",
        ),
        Step::git(&ww("w1"), Repo::Proj, &["commit", "-qam", "w2-proj"]),
        Step::rwv(&ww("w1"), &["switch", "proj--w1"]).expect(Exit::Succeeds),
        Step::hand_edit(&a(), Repo::Proj, "notes.txt", "notes\n", "notes on main\n"),
        Step::git(&a(), Repo::Proj, &["commit", "-qam", "main-proj"]),
    ];
    steps.extend(main_moves_app("main-app"));
    steps.extend([
        Step::rwv(&ww("w1"), &["sync"])
            .expect(Exit::refuses_kind(RefusalKind::OpParked))
            .claim(Claim::OutputContains("carrying 'w2' along"))
            .claim(Claim::OutputContains("rwv sync --continue")),
        Step::git(
            &ww("w1"),
            Repo::Proj,
            &["checkout", "--theirs", "notes.txt"],
        ),
        Step::git(&ww("w1"), Repo::Proj, &["add", "notes.txt"]),
        Step::rwv(&ww("w1"), &["sync", "--continue"])
            .expect(Exit::Succeeds)
            .resolved_by_hand(&[Repo::Proj])
            .claim(Claim::OutputContains("carried 'w2' along"))
            .claim(Claim::OnBranch {
                at: ww("w1"),
                repo: Repo::Proj,
                branch: "proj--w1",
            }),
        Step::git(
            &ww("w1"),
            Repo::Proj,
            &["merge-base", "--is-ancestor", "proj--w1", "w2"],
        )
        .expect(Exit::Succeeds),
        Step::git(
            &ww("w1"),
            Repo::App,
            &["merge-base", "--is-ancestor", "proj--w1", "w2"],
        )
        .expect(Exit::Succeeds),
    ]);
    scenario(
        "stack: a carry stopped in the project repo continues",
        steps,
    )
}

/// A landing of `w1` whose carry of `w2` stops in the project repo, `w2`'s
/// project commit also changing `app`'s entry: `rwv land --continue` judges
/// the landing's own entries, not the `rwv.toml` the stopped carry left in
/// the checkout, says so to a consent named for them, and completes with `w2`
/// carried.
pub fn a_landing_whose_carry_stopped_in_the_project_continues() -> Scenario {
    let declared = declared_branch_text(Repo::App, "main");
    let edited = format!("{declared}\nrecurse-submodules = false");
    let mut steps = vec![
        Step::rwv(&a(), &["branch", "set-history", "linear"]).expect(Exit::Succeeds),
        Step::commit(&a(), Repo::Proj, "notes"),
        create("w1"),
        Step::commit(&ww("w1"), Repo::App, "w1-app"),
        Step::rwv(&ww("w1"), &["lock", "--commit"]).expect(Exit::Succeeds),
        dependent_of(&ww("w1"), "w2", "proj--w1"),
        Step::hand_edit(
            &ww("w1"),
            Repo::Proj,
            "notes.txt",
            "notes\n",
            "notes on w2\n",
        ),
        Step::hand_edit(&ww("w1"), Repo::Proj, "rwv.toml", &declared, &edited),
        Step::git(&ww("w1"), Repo::Proj, &["commit", "-qam", "w2-proj"]),
        Step::rwv(&ww("w1"), &["switch", "proj--w1"]).expect(Exit::Succeeds),
        Step::hand_edit(&a(), Repo::Proj, "notes.txt", "notes\n", "notes on main\n"),
        Step::git(&a(), Repo::Proj, &["commit", "-qam", "main-proj"]),
    ];
    steps.extend(main_moves_app("main-app"));
    steps.extend([
        Step::rwv(&ww("w1"), &["land"])
            .expect(Exit::refuses_kind(RefusalKind::OpParked))
            .claim(Claim::OutputContains("carrying 'w2' along"))
            .claim(Claim::OutputContains("rwv land --continue")),
        Step::git(
            &ww("w1"),
            Repo::Proj,
            &["checkout", "--theirs", "notes.txt"],
        ),
        Step::git(&ww("w1"), Repo::Proj, &["add", "notes.txt"]),
        Step::rwv(
            &ww("w1"),
            &[
                "land",
                "--continue",
                "--allow-member-change",
                "local/remotes/app",
            ],
        )
        .expect(Exit::refuses_kind(RefusalKind::InapplicableFlag))
        .claim(Claim::OutputContains(
            "judged again once the carry is continued",
        )),
        Step::rwv(&ww("w1"), &["land", "--continue"])
            .expect(Exit::Succeeds)
            .resolved_by_hand(&[Repo::Proj])
            .claim(Claim::OutputContains("carried 'w2' along"))
            .claim(Claim::OutputLacks("member change")),
        Step::git(
            &ww("w1"),
            Repo::Proj,
            &["merge-base", "--is-ancestor", "proj--w1", "w2"],
        )
        .expect(Exit::Succeeds),
        Step::git(
            &a(),
            Repo::Proj,
            &["grep", "-q", "recurse-submodules", "main", "--", "rwv.toml"],
        )
        .expect(Exit::Code(1)),
    ]);
    scenario(
        "stack: a landing whose carry stopped in the project repo continues",
        steps,
    )
}

/// `w3` onto `w2` onto `w1`, `w1` holding an `app` commit its lock does not
/// pin, so its sync writes a relock commit; `w3`'s replay conflicts after
/// `w2`'s project branch composed on that commit. `rwv sync --continue`
/// keeps the relock commit and every carried project branch holds `w1`'s
/// project tip.
pub fn a_sync_continued_after_its_relock_keeps_it() -> Scenario {
    let mut steps = vec![
        Step::rwv(&a(), &["branch", "set-history", "linear"]).expect(Exit::Succeeds),
        create("w1"),
        Step::commit(&ww("w1"), Repo::App, "w1-app"),
        Step::rwv(&ww("w1"), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::commit(&ww("w1"), Repo::App, "w1-unpinned"),
        dependent_of(&ww("w1"), "w2", "proj--w1"),
        Step::git(
            &ww("w1"),
            Repo::App,
            &["merge", "--ff-only", "-q", "proj--w1"],
        ),
        Step::commit(&ww("w1"), Repo::App, "w2-app"),
        Step::rwv(&ww("w1"), &["lock", "--commit"]).expect(Exit::Succeeds),
        dependent_of(&ww("w1"), "w3", "w2"),
        Step::hand_edit(&ww("w1"), Repo::App, "README", "app", "beta"),
        Step::git(&ww("w1"), Repo::App, &["commit", "-qam", "w3-app"]),
        Step::rwv(&ww("w1"), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::rwv(&ww("w1"), &["switch", "proj--w1"]).expect(Exit::Succeeds),
        Step::hand_edit(&a(), Repo::App, "README", "app", "alpha"),
        Step::git(&a(), Repo::App, &["commit", "-qam", "main-app"]),
        Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::rwv(&ww("w1"), &["sync"])
            .expect(Exit::refuses_kind(RefusalKind::OpParked))
            .claim(Claim::OutputContains("carrying 'w3' along")),
        Step::git(&ww("w1"), Repo::App, &["checkout", "--theirs", "README"]),
        Step::git(&ww("w1"), Repo::App, &["add", "README"]),
        Step::rwv(&ww("w1"), &["sync", "--continue"])
            .expect(Exit::Succeeds)
            .resolved_by_hand(&[Repo::App])
            .may_rewind(&[Repo::App])
            .claim(Claim::OutputContains("carried 'w3' along")),
        Step::git(
            &ww("w1"),
            Repo::Proj,
            &["log", "-1", "--format=%s", "proj--w1"],
        )
        .expect(Exit::Succeeds)
        .claim(Claim::OutputContains("relock after sync")),
    ];
    for (repo, parent, dependent) in [
        (Repo::Proj, "proj--w1", "w2"),
        (Repo::Proj, "w2", "w3"),
        (Repo::App, "proj--w1", "w2"),
        (Repo::App, "w2", "w3"),
    ] {
        steps.push(
            Step::git(
                &ww("w1"),
                repo,
                &["merge-base", "--is-ancestor", parent, dependent],
            )
            .expect(Exit::Succeeds),
        );
    }
    scenario("stack: a sync continued after its relock keeps it", steps)
}

/// `a-sync-continued-after-its-relock-keeps-it`, with a commit made in
/// `w1`'s project repo during the stop: the project branch `w2` was composed
/// on before the stop no longer sits on `w1`'s project tip, so the continued
/// carry composes it again and `w2` holds `w1`'s project tip.
pub fn a_sync_continued_after_its_project_tip_moved_recomposes_the_project_carry() -> Scenario {
    let steps = vec![
        Step::rwv(&a(), &["branch", "set-history", "linear"]).expect(Exit::Succeeds),
        create("w1"),
        Step::commit(&ww("w1"), Repo::App, "w1-app"),
        Step::rwv(&ww("w1"), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::commit(&ww("w1"), Repo::App, "w1-unpinned"),
        dependent_of(&ww("w1"), "w2", "proj--w1"),
        Step::git(
            &ww("w1"),
            Repo::App,
            &["merge", "--ff-only", "-q", "proj--w1"],
        ),
        Step::commit(&ww("w1"), Repo::App, "w2-app"),
        Step::rwv(&ww("w1"), &["lock", "--commit"]).expect(Exit::Succeeds),
        dependent_of(&ww("w1"), "w3", "w2"),
        Step::hand_edit(&ww("w1"), Repo::App, "README", "app", "beta"),
        Step::git(&ww("w1"), Repo::App, &["commit", "-qam", "w3-app"]),
        Step::rwv(&ww("w1"), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::rwv(&ww("w1"), &["switch", "proj--w1"]).expect(Exit::Succeeds),
        Step::hand_edit(&a(), Repo::App, "README", "app", "alpha"),
        Step::git(&a(), Repo::App, &["commit", "-qam", "main-app"]),
        Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::rwv(&ww("w1"), &["sync"])
            .expect(Exit::refuses_kind(RefusalKind::OpParked))
            .claim(Claim::OutputContains("carrying 'w3' along")),
        Step::commit(&ww("w1"), Repo::Proj, "w1-later"),
        Step::git(&ww("w1"), Repo::App, &["checkout", "--theirs", "README"]),
        Step::git(&ww("w1"), Repo::App, &["add", "README"]),
        Step::rwv(&ww("w1"), &["sync", "--continue"])
            .expect(Exit::Succeeds)
            .resolved_by_hand(&[Repo::App])
            .may_rewind(&[Repo::App]),
        Step::git(
            &ww("w1"),
            Repo::Proj,
            &["merge-base", "--is-ancestor", "proj--w1", "w2"],
        )
        .expect(Exit::Succeeds),
    ];
    scenario(
        "stack: a sync continued after its project tip moved recomposes the project carry",
        steps,
    )
}

/// A sync or a landing whose relock commit was made and killed before its
/// tip was journaled, then continued into a stop on the carry of `w2` in
/// `app`: the printed `rwv abort` puts back what the verb moved, the relock
/// commit included.
pub fn a_verb_killed_after_its_relock_commit_then_stopped(
    verb: &'static str,
    point: &'static str,
) -> Scenario {
    let steps = vec![
        Step::rwv(&a(), &["branch", "set-history", "linear"]).expect(Exit::Succeeds),
        create("w1"),
        Step::commit(&ww("w1"), Repo::App, "w1-app"),
        Step::rwv(&ww("w1"), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::commit(&ww("w1"), Repo::App, "w1-unpinned"),
        dependent_of(&ww("w1"), "w2", "proj--w1"),
        Step::git(
            &ww("w1"),
            Repo::App,
            &["merge", "--ff-only", "-q", "proj--w1"],
        ),
        Step::hand_edit(&ww("w1"), Repo::App, "README", "app", "beta"),
        Step::git(&ww("w1"), Repo::App, &["commit", "-qam", "w2-app"]),
        Step::rwv(&ww("w1"), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::rwv(&ww("w1"), &["switch", "proj--w1"]).expect(Exit::Succeeds),
        Step::hand_edit(&a(), Repo::App, "README", "app", "alpha"),
        Step::git(&a(), Repo::App, &["commit", "-qam", "main-app"]),
        Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::rwv_paused(&ww("w1"), &[verb], point, "relocking"),
        Step::kill("relocking"),
        Step::rwv(&ww("w1"), &[verb, "--continue"])
            .expect(Exit::refuses_kind(RefusalKind::OpParked))
            .claim(Claim::OutputContains(
                "carrying 'w2' along in local/remotes/app",
            )),
        Step::rwv(&ww("w1"), &["abort"])
            .expect(Exit::Succeeds)
            .may_rewind(&[Repo::App])
            .may_restore(&[Repo::App, Repo::Proj])
            .claim(Claim::OutputLacks("foreign")),
        Step::git(&ww("w1"), Repo::Proj, &["log", "--format=%s", "proj--w1"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputLacks("relock after")),
    ];
    scenario(
        "stack: a verb killed after its relock commit, then stopped",
        steps,
    )
}

/// `stack-carried`, killed after the carry of `w2` in `app` journaled what
/// it composed and before the checkout went back on its branch: the
/// checkout is on no branch, and the printed `rwv sync --continue` puts it
/// back and carries on.
pub fn a_sync_killed_after_journaling_a_composed_carry_continues() -> Scenario {
    let mut steps = stack_on_a_seat();
    steps.extend(main_moves_app("main-app"));
    steps.extend([
        Step::rwv_paused(&ww("w1"), &["sync"], "carry-journaled", "carrying"),
        Step::kill("carrying"),
        Step::rwv(&ww("w1"), &["sync", "--continue"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputContains("carried 'w2' along")),
        Step::git(&ww("w1"), Repo::App, &["symbolic-ref", "-q", "HEAD"]).expect(Exit::Succeeds),
        Step::git(&ww("w1"), Repo::Proj, &["symbolic-ref", "-q", "HEAD"]).expect(Exit::Succeeds),
    ]);
    scenario(
        "stack: a sync killed after journaling a composed carry continues",
        steps,
    )
}

/// What wrote `w1`'s project tip before its carry of `w2` stopped.
#[derive(Debug, Clone, Copy)]
pub enum BeforeTheCarry {
    /// A sync's relock commit, pinning an `app` commit the lock did not.
    SyncRelocked,
    /// A landing's project merge, under a `merge` project with a `linear`
    /// `app`, by a git without `merge-tree --write-tree`.
    #[cfg(unix)]
    LandingMerged,
    /// Nothing more: the carry's replay was then finished by hand.
    FinishedByHand,
}

/// `w1`'s sync or landing stops carrying `w2`, whose `app` commit conflicts
/// with `main`'s, after rwv wrote `w1`'s project tip or once the operator
/// finished the carry's replay by hand; `rwv abort` reads every tip as the
/// operation's own and puts `w1` and `w2` back as they were.
pub fn abort_after_a_carry_stop(before: BeforeTheCarry) -> Scenario {
    let landing = !matches!(
        before,
        BeforeTheCarry::SyncRelocked | BeforeTheCarry::FinishedByHand
    );
    let history = if landing { "merge" } else { "linear" };
    let unpinned = matches!(before, BeforeTheCarry::SyncRelocked);
    let mut steps =
        vec![Step::rwv(&a(), &["branch", "set-history", history]).expect(Exit::Succeeds)];
    if history == "merge" {
        steps.push(
            Step::rwv(
                &a(),
                &[
                    "branch",
                    "set-history",
                    "linear",
                    "--repo",
                    "local/remotes/app",
                ],
            )
            .expect(Exit::Succeeds),
        );
    }
    steps.extend([
        create("w1"),
        Step::commit(&ww("w1"), Repo::App, "w1-app"),
        Step::rwv(&ww("w1"), &["lock", "--commit"]).expect(Exit::Succeeds),
    ]);
    if unpinned {
        steps.push(Step::commit(&ww("w1"), Repo::App, "w1-unpinned"));
    }
    steps.push(dependent_of(&ww("w1"), "w2", "proj--w1"));
    if unpinned {
        steps.push(Step::git(
            &ww("w1"),
            Repo::App,
            &["merge", "--ff-only", "-q", "proj--w1"],
        ));
    }
    let verb = if landing { "land" } else { "sync" };
    steps.extend([
        Step::hand_edit(&ww("w1"), Repo::App, "README", "app", "beta"),
        Step::git(&ww("w1"), Repo::App, &["commit", "-qam", "w2-app"]),
        Step::rwv(&ww("w1"), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::rwv(&ww("w1"), &["switch", "proj--w1"]).expect(Exit::Succeeds),
        Step::hand_edit(&a(), Repo::App, "README", "app", "alpha"),
        Step::git(&a(), Repo::App, &["commit", "-qam", "main-app"]),
        Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::rwv(&ww("w1"), &[verb])
            .expect(Exit::refuses_kind(RefusalKind::OpParked))
            .claim(Claim::OutputContains(
                "carrying 'w2' along in local/remotes/app",
            )),
    ]);
    if matches!(before, BeforeTheCarry::FinishedByHand) {
        steps.extend([
            Step::git(&ww("w1"), Repo::App, &["checkout", "--theirs", "README"]),
            Step::git(&ww("w1"), Repo::App, &["add", "README"]),
            Step::git(
                &ww("w1"),
                Repo::App,
                &["-c", "core.editor=true", "rebase", "--continue"],
            ),
        ]);
    }
    let w1_app = if unpinned { "w1-unpinned" } else { "w1-app" };
    let abort = match before {
        BeforeTheCarry::FinishedByHand => {
            Step::rwv(&ww("w1"), &["abort"]).discards_a_hand_finished_replay(&[Repo::App])
        }
        _ => Step::rwv(&ww("w1"), &["abort"]),
    };
    steps.extend([
        abort
            .expect(Exit::Succeeds)
            .may_rewind(&[Repo::App])
            .may_restore(&[Repo::App, Repo::Proj])
            .claim(Claim::OutputLacks("foreign"))
            .claim(Claim::OnBranch {
                at: ww("w1"),
                repo: Repo::App,
                branch: "proj--w1",
            })
            .claim(Claim::OnBranch {
                at: ww("w1"),
                repo: Repo::Proj,
                branch: "proj--w1",
            }),
        Step::git(
            &ww("w1"),
            Repo::App,
            &["log", "--format=%s", "-1", "proj--w1"],
        )
        .expect(Exit::Succeeds)
        .claim(Claim::OutputLine(w1_app)),
        Step::git(&ww("w1"), Repo::App, &["log", "--format=%s", "-1", "w2"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputLine("w2-app")),
        Step::git(&ww("w1"), Repo::App, &["log", "--format=%s", "proj--w1"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputLacks("main-app")),
        Step::git(&ww("w1"), Repo::Proj, &["log", "--format=%s", "proj--w1"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputLacks("relock after"))
            .claim(Claim::OutputLacks("Merge")),
    ]);
    Scenario {
        name: "stack: an abort after a carry stopped",
        seed: match before {
            #[cfg(unix)]
            BeforeTheCarry::LandingMerged => Seed::default().old_git(),
            BeforeTheCarry::SyncRelocked | BeforeTheCarry::FinishedByHand => Seed::default(),
        },
        steps,
    }
}

/// A `merge` project with `linear` `app` and `lib`: `w1` commits in both,
/// `w2` onto `w1` only in `app`, and `main` moves both. `w1`'s sync merges
/// its project branch and rebases both members; `w2`'s project branch,
/// carried, holds `w1`'s new project tip and pins the rebased `lib` it
/// inherits, not the commit the rebase replaced.
pub fn a_dependent_carried_onto_a_merged_project_pins_what_it_inherits() -> Scenario {
    let linear = |repo: &'static str| {
        Step::rwv(&a(), &["branch", "set-history", "linear", "--repo", repo]).expect(Exit::Succeeds)
    };
    let mut steps = vec![
        Step::rwv(&a(), &["branch", "set-history", "merge"]).expect(Exit::Succeeds),
        linear("local/remotes/app"),
        linear("local/remotes/lib"),
        create("w1"),
        Step::commit(&ww("w1"), Repo::App, "w1-app"),
        Step::commit(&ww("w1"), Repo::Lib, "w1-lib"),
        Step::rwv(&ww("w1"), &["lock", "--commit"]).expect(Exit::Succeeds),
        dependent_of(&ww("w1"), "w2", "proj--w1"),
        Step::commit(&ww("w1"), Repo::App, "w2-app"),
        Step::rwv(&ww("w1"), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::rwv(&ww("w1"), &["switch", "proj--w1"]).expect(Exit::Succeeds),
        Step::commit(&a(), Repo::App, "main-app"),
        Step::commit(&a(), Repo::Lib, "main-lib"),
        Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::rwv(&ww("w1"), &["sync"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputContains("carried 'w2' along"))
            .claim(Claim::PinsReplayed {
                at: ww("w1"),
                rev: "w2",
                member: Repo::Lib,
                subject: "w1-lib",
                reached_from: "w2",
            })
            .claim(Claim::PinsReplayed {
                at: ww("w1"),
                rev: "w2",
                member: Repo::App,
                subject: "w2-app",
                reached_from: "w2",
            }),
        Step::git(
            &ww("w1"),
            Repo::Proj,
            &["merge-base", "--is-ancestor", "proj--w1", "w2"],
        )
        .expect(Exit::Succeeds),
    ];
    steps.push(Step::rwv(&ww("w1"), &["status"]).expect(Exit::Succeeds));
    scenario(
        "stack: a dependent carried onto a merged project pins what it inherits",
        steps,
    )
}

/// How a refused abort of a carried stack someone built on is cleared.
#[derive(Debug, Clone, Copy)]
pub enum BuiltOnChoice {
    /// Everything put back, the commit built on `w2` kept as a backup.
    Abandon,
    /// The landing finished, `w2` keeping the commit.
    Continue,
}

/// `stack-carried-by-a-landing` killed after its carry moved `w2`, then a
/// commit on `w2`: `rwv abort` puts nothing back and prints the two
/// choices, and the one run as printed clears it.
pub fn abort_a_carried_stack_built_on(choice: BuiltOnChoice) -> Scenario {
    let mut steps = stack_on_a_seat();
    steps.extend(main_moves_app("main-app"));
    steps.extend([
        Step::rwv_paused(&ww("w1"), &["land"], "composed", "landing"),
        Step::kill("landing"),
        Step::hand_commit(&ww("w1"), Repo::App, "w2", &[("more.txt", "more\n")]),
        Step::rwv(&ww("w1"), &["abort"])
            .expect(Exit::refuses_kind(RefusalKind::ForeignTip))
            .claim(Claim::OutputLacks("abort stopped there"))
            .claim(Claim::OutputContains(
                "rwv abort --abandon-foreign-tip local/remotes/app",
            ))
            .claim(Claim::OutputContains("rwv land --continue"))
            .claim(Claim::OutputContains("'w2' keeps its commits"))
            .claim(Claim::LocalStateUnchanged(ww("w1"))),
    ]);
    match choice {
        BuiltOnChoice::Abandon => steps.extend([
            Step::rwv_printed_exactly(
                &ww("w1"),
                "rwv abort --abandon-foreign-tip local/remotes/app",
            )
            .expect(Exit::Succeeds)
            .may_rewind(&[Repo::App])
            .may_restore(&[Repo::App, Repo::Proj])
            .claim(Claim::OutputContains("abandoned"))
            .claim(Claim::OnBranch {
                at: ww("w1"),
                repo: Repo::App,
                branch: "proj--w1",
            }),
            Step::git(&ww("w1"), Repo::App, &["log", "--format=%s", "-1", "w2"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputLine("w2-app")),
            Step::git(&ww("w1"), Repo::App, &["log", "--format=%s", "proj--w1"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputLacks("main-app")),
            Step::rwv(&ww("w1"), &["backup", "list"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains("abandoned")),
            Step::git(&ww("w1"), Repo::App, &["log", "--all", "--format=%s"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains("hand commit")),
        ]),
        BuiltOnChoice::Continue => steps.extend([
            Step::rwv_printed_exactly(&ww("w1"), "rwv land --continue")
                .expect(Exit::Succeeds)
                .claim(Claim::Contains {
                    at: ww("w1"),
                    repo: Repo::App,
                    rev: "w2",
                    mark: "main-app",
                }),
            Step::git(&ww("w1"), Repo::App, &["log", "--format=%s", "-1", "w2"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputLine("hand commit")),
            Step::git(
                &ww("w1"),
                Repo::App,
                &["merge-base", "--is-ancestor", "proj--w1", "w2"],
            )
            .expect(Exit::Succeeds),
            Step::rwv(&ww("w1"), &["sync"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputLacks("lacks")),
        ]),
    }
    scenario("stack: an abort meets a carried dependent built on", steps)
}

/// What stands in the way of an abort putting a carried dependent back.
#[derive(Debug, Clone, Copy)]
pub enum CarriedBlocked {
    /// `w2` checked out in a worktree outside the op's workspaces.
    CheckedOut,
    /// `w2` checked out in the target's checkout of `app`, which the
    /// landing never moved, so abort leaves it on `w2`.
    CheckedOutInTheTarget,
    /// `w2` deleted.
    Gone,
}

/// `stack-carried-by-a-landing` killed after its carry moved `w2`, then
/// `w2` checked out elsewhere or gone: `rwv abort` puts nothing back and
/// names what to do first; once done, `rwv abort` puts everything back.
pub fn abort_a_carried_stack_blocked(blocked: CarriedBlocked) -> Scenario {
    let mut steps = stack_on_a_seat();
    steps.extend(main_moves_app("main-app"));
    steps.extend([
        Step::rwv_paused(&ww("w1"), &["land"], "composed", "landing"),
        Step::kill("landing"),
    ]);
    let (block, needs, clear) = match blocked {
        CarriedBlocked::CheckedOut => (
            Step::git(
                &ww("w1"),
                Repo::App,
                &["worktree", "add", "-q", "../../../../w2-held", "w2"],
            ),
            "is checked out at",
            Step::rwv_printed_holding(&ww("w1"), "git -C ", "switch --detach")
                .expect(Exit::Succeeds),
        ),
        CarriedBlocked::CheckedOutInTheTarget => (
            Step::git(&a(), Repo::App, &["switch", "-q", "w2"]),
            "is checked out at",
            Step::rwv_printed_holding(&ww("w1"), "git -C ", "switch --detach")
                .expect(Exit::Succeeds),
        ),
        CarriedBlocked::Gone => (
            Step::git(&ww("w1"), Repo::App, &["branch", "-q", "-D", "w2"])
                .may_leave_unresolved(&[Repo::App]),
            "put 'w2' back where this operation carried it",
            Step::rwv_printed_holding(&ww("w1"), "git -C ", "branch w2 ").expect(Exit::Succeeds),
        ),
    };
    steps.extend([
        block,
        Step::rwv(&ww("w1"), &["abort"])
            .expect(Exit::refuses_kind(RefusalKind::ForeignTip))
            .claim(Claim::OutputLacks("abort stopped there"))
            .claim(Claim::OutputContains(needs))
            .claim(Claim::OutputContains("once each fix above is made, rerun"))
            .claim(Claim::OutputLacks("--abandon-foreign-tip"))
            .claim(Claim::LocalStateUnchanged(ww("w1"))),
        clear,
        Step::rwv(&ww("w1"), &["abort"])
            .expect(Exit::Succeeds)
            .may_rewind(&[Repo::App])
            .may_restore(&[Repo::App, Repo::Proj])
            .claim(Claim::OutputLacks("foreign")),
        Step::git(&ww("w1"), Repo::App, &["log", "--format=%s", "-1", "w2"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputLine("w2-app")),
        Step::git(&ww("w1"), Repo::App, &["log", "--format=%s", "w2"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputLacks("main-app")),
    ]);
    scenario(
        "stack: an abort meets a carried dependent it cannot move",
        steps,
    )
}

/// `stack-carried-by-a-landing` killed after its carry moved `w2`, then an
/// abort killed once it has put `w2` back: the rerun finds nothing in the
/// way and ends the operation.
pub fn abort_a_carried_stack_killed_after_putting_it_back() -> Scenario {
    let mut steps = stack_on_a_seat();
    steps.extend(main_moves_app("main-app"));
    steps.extend([
        Step::rwv_paused(&ww("w1"), &["land"], "composed", "landing"),
        Step::kill("landing"),
        Step::rwv_paused(&ww("w1"), &["abort"], "abort-carried", "aborting")
            .may_rewind(&[Repo::App])
            .may_restore(&[Repo::App, Repo::Proj]),
        Step::kill("aborting"),
        Step::rwv(&ww("w1"), &["abort"])
            .expect(Exit::Succeeds)
            .may_rewind(&[Repo::App])
            .may_restore(&[Repo::App, Repo::Proj])
            .claim(Claim::OutputLacks("foreign"))
            .claim(Claim::OutputLacks("--abandon-foreign-tip")),
        Step::git(&ww("w1"), Repo::App, &["log", "--format=%s", "-1", "w2"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputLine("w2-app")),
        Step::git(&ww("w1"), Repo::App, &["log", "--format=%s", "w2"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputLacks("main-app")),
        Step::rwv(&ww("w1"), &["status"]).expect(Exit::Succeeds),
    ]);
    scenario(
        "stack: an abort killed after it put a carried stack back is rerun",
        steps,
    )
}

/// `stack-carry-conflict` stopped on `w2` in `app`, and the operator leaves
/// the replay with `git rebase --quit`, its conflicted edits in the
/// worktree: `rwv abort` refuses before anything moves, since no checkout
/// switches over an unresolved index, and naming `app` on
/// `--abandon-foreign-tip` gives the edits up, kept as a backup.
pub fn abort_over_a_carry_left_by_a_quit() -> Scenario {
    let steps = vec![
        Step::rwv(&a(), &["branch", "set-history", "linear"]).expect(Exit::Succeeds),
        create("w1"),
        Step::commit(&ww("w1"), Repo::App, "w1-app"),
        Step::rwv(&ww("w1"), &["lock", "--commit"]).expect(Exit::Succeeds),
        dependent_of(&ww("w1"), "w2", "proj--w1"),
        Step::hand_edit(&ww("w1"), Repo::App, "README", "app", "beta"),
        Step::git(&ww("w1"), Repo::App, &["commit", "-qam", "w2-app"]),
        Step::rwv(&ww("w1"), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::rwv(&ww("w1"), &["switch", "proj--w1"]).expect(Exit::Succeeds),
        Step::hand_edit(&a(), Repo::App, "README", "app", "alpha"),
        Step::git(&a(), Repo::App, &["commit", "-qam", "main-app"]),
        Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::rwv(&ww("w1"), &["sync"]).expect(Exit::refuses_kind(RefusalKind::OpParked)),
        Step::git(&ww("w1"), Repo::App, &["rebase", "--quit"]),
        Step::rwv(&ww("w1"), &["abort"])
            .expect(Exit::refuses_kind(RefusalKind::ForeignTip))
            .claim(Claim::OutputContains("nothing is restored"))
            .claim(Claim::OutputLacks("abort stopped there"))
            .claim(Claim::LocalStateUnchanged(ww("w1"))),
        Step::rwv_printed_exactly(
            &ww("w1"),
            "rwv abort --abandon-foreign-tip local/remotes/app",
        )
        .expect(Exit::Succeeds)
        .may_rewind(&[Repo::App]),
        Step::git(&ww("w1"), Repo::App, &["log", "--format=%s", "-1", "w2"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputLine("w2-app")),
    ];
    scenario("stack: an abort over a carry left by a quit", steps)
}

/// How a dependent that moved as its carry was moving it is taken up.
#[derive(Debug, Clone, Copy)]
pub enum MovedAsCarried {
    /// `rwv sync --continue` carries it again with its new commit.
    CarryAgain,
    /// `rwv abort`, then `rwv sync`.
    AbortThenSync,
    /// The same race in a landing: `rwv land --continue` carries it again.
    CarryAgainLanding,
}

/// `stack-carried`, with a commit made on `w2` after its carry composed and
/// before it moved: the sync moves no dependent and refuses with `target`
/// dependent moved mid-operation, and each printed choice, run as printed,
/// carries `w2` with that commit.
pub fn a_dependent_moved_as_its_carry_moves(then: MovedAsCarried) -> Scenario {
    let verb = match then {
        MovedAsCarried::CarryAgainLanding => "land",
        _ => "sync",
    };
    let mut steps = stack_on_a_seat();
    steps.extend(main_moves_app("main-app"));
    steps.extend([
        Step::rwv_paused(&ww("w1"), &[verb], "carry-move", "carrying"),
        Step::hand_commit(&ww("w1"), Repo::App, "w2", &[("more.txt", "more\n")]),
        Step::release("carrying")
            .expect(Exit::refuses(
                Repo::Proj,
                State::TargetDependentMovedMidOperation,
            ))
            .claim(Claim::OutputContains(if verb == "land" {
                "rwv land --continue"
            } else {
                "rwv sync --continue"
            }))
            .claim(Claim::OutputContains("rwv abort"))
            .claim(Claim::Lacks {
                at: ww("w1"),
                repo: Repo::App,
                rev: "w2",
                mark: "main-app",
            }),
    ]);
    match then {
        MovedAsCarried::CarryAgain => steps.push(
            Step::rwv_printed_exactly(&ww("w1"), "rwv sync --continue").expect(Exit::Succeeds),
        ),
        MovedAsCarried::CarryAgainLanding => steps.extend([
            Step::rwv_printed_exactly(&ww("w1"), "rwv land --continue").expect(Exit::Succeeds),
            Step::git(
                &ww("w1"),
                Repo::App,
                &["merge-base", "--is-ancestor", "proj--w1", "main"],
            )
            .expect(Exit::Succeeds),
        ]),
        MovedAsCarried::AbortThenSync => steps.extend([
            Step::rwv_printed_exactly(&ww("w1"), "rwv abort")
                .expect(Exit::Succeeds)
                .may_rewind(&[Repo::App])
                .may_restore(&[Repo::App, Repo::Proj]),
            Step::rwv(&ww("w1"), &["sync"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains("carried 'w2' along")),
        ]),
    }
    steps.extend([
        Step::git(&ww("w1"), Repo::App, &["log", "--format=%s", "-1", "w2"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputLine("hand commit")),
        Step::git(
            &ww("w1"),
            Repo::App,
            &["merge-base", "--is-ancestor", "proj--w1", "w2"],
        )
        .expect(Exit::Succeeds),
    ]);
    scenario("stack: a dependent moved as its carry moves it", steps)
}

/// `stack-carry-conflict` stopped on `w2` in `app`, and the operator leaves
/// the replay with `git rebase --quit`: the checkout is detached on `main`'s
/// work with no record of what became of `w2`'s commit, so `--continue`
/// refuses `replay-unrecorded` rather than moving `w2` there, and an abort
/// that gives up the quit's conflicted edits, kept as a backup, puts
/// everything back.
pub fn a_carry_left_by_a_quit_is_unrecorded() -> Scenario {
    let steps = vec![
        Step::rwv(&a(), &["branch", "set-history", "linear"]).expect(Exit::Succeeds),
        create("w1"),
        Step::commit(&ww("w1"), Repo::App, "w1-app"),
        Step::rwv(&ww("w1"), &["lock", "--commit"]).expect(Exit::Succeeds),
        dependent_of(&ww("w1"), "w2", "proj--w1"),
        Step::hand_edit(&ww("w1"), Repo::App, "README", "app", "beta"),
        Step::git(&ww("w1"), Repo::App, &["commit", "-qam", "w2-app"]),
        Step::rwv(&ww("w1"), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::rwv(&ww("w1"), &["switch", "proj--w1"]).expect(Exit::Succeeds),
        Step::hand_edit(&a(), Repo::App, "README", "app", "alpha"),
        Step::git(&a(), Repo::App, &["commit", "-qam", "main-app"]),
        Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::rwv(&ww("w1"), &["sync"]).expect(Exit::refuses_kind(RefusalKind::OpParked)),
        Step::git(&ww("w1"), Repo::App, &["rebase", "--quit"]),
        Step::rwv(&ww("w1"), &["sync", "--continue"])
            .expect(Exit::refuses_kind(RefusalKind::ReplayUnrecorded))
            .claim(Claim::LocalStateUnchanged(ww("w1"))),
        Step::rwv(
            &ww("w1"),
            &["abort", "--abandon-foreign-tip", "local/remotes/app"],
        )
        .expect(Exit::Succeeds)
        .may_rewind(&[Repo::App]),
        Step::git(&ww("w1"), Repo::App, &["log", "--format=%s", "-1", "w2"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputLine("w2-app")),
    ];
    scenario("stack: a carry left by a quit is unrecorded", steps)
}

/// `w2` and `w3` both land into `w1`; `w1`'s sync carries `w2` whole and
/// stops on `w3`'s `app`. Meanwhile `w2` gains a commit in `app` that
/// conflicts with `main`, so the resumed carry composes `w2`'s `app` again
/// and stops there. Once that stop is resolved and continued, `w2`'s
/// project branch pins the `app` commit `w2` moves to, not the composition
/// the first stop left behind.
pub fn a_project_carry_follows_its_member_recomposed_across_a_stop() -> Scenario {
    let steps = vec![
        Step::rwv(&a(), &["branch", "set-history", "linear"]).expect(Exit::Succeeds),
        create("w1"),
        Step::commit(&ww("w1"), Repo::App, "w1-app"),
        Step::rwv(&ww("w1"), &["lock", "--commit"]).expect(Exit::Succeeds),
        dependent_of(&ww("w1"), "w2", "proj--w1"),
        Step::commit(&ww("w1"), Repo::App, "w2-app"),
        Step::rwv(&ww("w1"), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::rwv(&ww("w1"), &["switch", "proj--w1"]).expect(Exit::Succeeds),
        dependent_of(&ww("w1"), "w3", "proj--w1"),
        Step::hand_edit(&ww("w1"), Repo::App, "README", "app", "gamma"),
        Step::git(&ww("w1"), Repo::App, &["commit", "-qam", "w3-app"]),
        Step::rwv(&ww("w1"), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::rwv(&ww("w1"), &["switch", "proj--w1"]).expect(Exit::Succeeds),
        Step::hand_edit(&a(), Repo::App, "README", "app", "alpha"),
        Step::git(&a(), Repo::App, &["commit", "-qam", "main-app"]),
        Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::rwv(&ww("w1"), &["sync"])
            .expect(Exit::refuses_kind(RefusalKind::OpParked))
            .claim(Claim::OutputContains("carrying 'w3' along")),
        Step::hand_commit(&ww("w1"), Repo::App, "w2", &[("README", "delta\n")]),
        Step::git(&ww("w1"), Repo::App, &["checkout", "--theirs", "README"]),
        Step::git(&ww("w1"), Repo::App, &["add", "README"]),
        Step::rwv(&ww("w1"), &["sync", "--continue"])
            .expect(Exit::refuses_kind(RefusalKind::OpParked))
            .resolved_by_hand(&[Repo::App])
            .may_rewind(&[Repo::App])
            .claim(Claim::OutputContains("carrying 'w2' along")),
        Step::git(&ww("w1"), Repo::App, &["checkout", "--theirs", "README"]),
        Step::git(&ww("w1"), Repo::App, &["add", "README"]),
        Step::rwv(&ww("w1"), &["sync", "--continue"])
            .expect(Exit::Succeeds)
            .resolved_by_hand(&[Repo::App])
            .may_rewind(&[Repo::App])
            .claim(Claim::PinsReplayed {
                at: ww("w1"),
                rev: "w2",
                member: Repo::App,
                subject: "w2-app",
                reached_from: "w2",
            }),
    ];
    scenario(
        "stack: a project carry follows its member recomposed across a stop",
        steps,
    )
}

/// `w2` and `w3` both land into `w1` and sit at one commit in `app` and in
/// the project repo, `w3` set there by hand. `w1`'s sync, held after its first replay until a
/// second has passed, carries both to one commit in each: the second
/// dependent takes the first one's replay rather than replaying the same
/// commits again over the first one's record.
pub fn siblings_at_one_commit_are_carried_to_one_commit() -> Scenario {
    let mut steps = stack_on_a_seat();
    steps.extend([
        dependent_of(&ww("w1"), "w3", "proj--w1"),
        Step::rwv(&ww("w1"), &["switch", "proj--w1"]).expect(Exit::Succeeds),
        Step::git(&ww("w1"), Repo::App, &["update-ref", "refs/heads/w3", "w2"]),
        Step::git(
            &ww("w1"),
            Repo::Proj,
            &["update-ref", "refs/heads/w3", "w2"],
        ),
    ]);
    steps.extend(main_moves_app("main-app"));
    let same = |repo: Repo, one: &'static str, other: &'static str| {
        Step::git(
            &ww("w1"),
            repo,
            &["merge-base", "--is-ancestor", one, other],
        )
        .expect(Exit::Succeeds)
    };
    steps.extend([
        Step::rwv_paused(&ww("w1"), &["sync"], "carry-composed", "carrying"),
        Step::git(&ww("w1"), Repo::App, &["-c", "alias.nap=!sleep 1.2", "nap"]),
        Step::release("carrying")
            .expect(Exit::Succeeds)
            .claim(Claim::OutputContains("carried 'w3' along")),
        same(Repo::App, "w2", "w3"),
        same(Repo::App, "w3", "w2"),
        same(Repo::Proj, "w2", "w3"),
        same(Repo::Proj, "w3", "w2"),
        Step::git(
            &ww("w1"),
            Repo::App,
            &["merge-base", "--is-ancestor", "proj--w1", "w3"],
        )
        .expect(Exit::Succeeds),
    ]);
    scenario(
        "stack: siblings at one commit are carried to one commit",
        steps,
    )
}

/// `a-landing-killed-after-its-carry-continues`, with `w1`'s top-level
/// `history` changed and committed before the resume: the carry being
/// done, `rwv land --continue` judges `w1`'s entries against `main` as
/// before and refuses the change unnamed, and lands it once named.
pub fn a_landing_resumed_after_its_carry_judges_an_entry_changed_since() -> Scenario {
    let mut steps = stack_on_a_seat();
    steps.extend(main_moves_app("main-app"));
    steps.extend([
        Step::rwv_paused(&ww("w1"), &["land"], "composed", "landing"),
        Step::kill("landing"),
        Step::hand_edit(&ww("w1"), Repo::Proj, "rwv.toml", "\"linear\"", "\"merge\""),
        Step::git(&ww("w1"), Repo::Proj, &["commit", "-qam", "history merge"]),
        Step::rwv(&ww("w1"), &["land", "--continue"])
            .expect(Exit::refuses(Repo::Proj, State::TargetMemberChanges)),
        Step::rwv(
            &ww("w1"),
            &[
                "land",
                "--continue",
                "--allow-member-change",
                "projects/proj",
            ],
        )
        .expect(Exit::Succeeds),
    ]);
    scenario(
        "stack: a landing resumed after its carry judges an entry changed since",
        steps,
    )
}

/// How the carry a landing into its dependent waits on ends.
#[derive(Debug, Clone, Copy)]
pub enum StoppedCarryEnds {
    /// The printed `rwv abort`.
    Abort,
    /// The conflict resolved by hand, then the printed `rwv sync --continue`.
    Continue,
}

/// `stack-carry-conflict` stopped on `w2`, and a landing into `w2` from a
/// third workweave meanwhile: the stopped carry holds `w2` as a landing
/// would, so the landing refuses naming the carry's op, and once the
/// printed choice ends it the landing lands. Where the carry continues, the
/// third workweave lands into `w2` for one run, its record onto `main`: a
/// record onto `w2` would make it a dependent the carry cannot move while
/// it is checked out.
pub fn a_landing_into_a_dependent_waits_on_its_stopped_carry(ends: StoppedCarryEnds) -> Scenario {
    let mut steps = vec![
        Step::rwv(&a(), &["branch", "set-history", "linear"]).expect(Exit::Succeeds),
        create("w1"),
        Step::commit(&ww("w1"), Repo::App, "w1-app"),
        Step::rwv(&ww("w1"), &["lock", "--commit"]).expect(Exit::Succeeds),
        dependent_of(&ww("w1"), "w2", "proj--w1"),
        Step::hand_edit(&ww("w1"), Repo::App, "README", "app", "beta"),
        Step::git(&ww("w1"), Repo::App, &["commit", "-qam", "w2-app"]),
        Step::rwv(&ww("w1"), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::rwv(&ww("w1"), &["switch", "proj--w1"]).expect(Exit::Succeeds),
        Step::hand_edit(&a(), Repo::App, "README", "app", "alpha"),
        Step::git(&a(), Repo::App, &["commit", "-qam", "main-app"]),
        Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::rwv(&ww("w1"), &["sync"])
            .expect(Exit::refuses_kind(RefusalKind::OpParked))
            .claim(Claim::OutputContains(
                "carrying 'w2' along in local/remotes/app",
            )),
    ];
    if let StoppedCarryEnds::Continue = ends {
        steps.extend([
            Step::git(&ww("w1"), Repo::App, &["checkout", "--theirs", "README"]),
            Step::git(&ww("w1"), Repo::App, &["add", "README"]),
        ]);
    }
    let land: &[&str] = match ends {
        StoppedCarryEnds::Abort => &["land"],
        StoppedCarryEnds::Continue => &["land", "--target", "w2"],
    };
    steps.push(create("w3"));
    if let StoppedCarryEnds::Abort = ends {
        steps.push(dependent_of(&ww("w3"), "w3", "w2"));
    }
    steps.extend([
        Step::commit(&ww("w3"), Repo::Lib, "w3-lib"),
        Step::rwv(&ww("w3"), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::rwv(&ww("w3"), land)
            .expect(Exit::refuses_kind(RefusalKind::StateClaimHeld))
            .claim(Claim::OutputContains("rwv sync --continue"))
            .claim(Claim::OutputContains("rwv abort"))
            .claim(Claim::LocalStateUnchanged(ww("w3"))),
    ]);
    steps.push(match ends {
        StoppedCarryEnds::Abort => Step::rwv_printed_exactly(&ww("w1"), "rwv abort")
            .expect(Exit::Succeeds)
            .may_rewind(&[Repo::App])
            .claim(Claim::OutputLacks("foreign")),
        StoppedCarryEnds::Continue => Step::rwv_printed_exactly(&ww("w1"), "rwv sync --continue")
            .expect(Exit::Succeeds)
            .resolved_by_hand(&[Repo::App])
            .may_rewind(&[Repo::App])
            .claim(Claim::OutputContains("carried 'w2' along")),
    });
    steps.push(
        Step::rwv(&ww("w3"), land)
            .expect(Exit::Succeeds)
            .claim(Claim::Contains {
                at: ww("w3"),
                repo: Repo::Lib,
                rev: "w2",
                mark: "w3-lib",
            }),
    );
    if let StoppedCarryEnds::Continue = ends {
        steps.push(
            Step::git(
                &ww("w1"),
                Repo::App,
                &["merge-base", "--is-ancestor", "main", "w2"],
            )
            .expect(Exit::Succeeds),
        );
    }
    scenario(
        "stack: a landing into a dependent waits on its stopped carry",
        steps,
    )
}

/// `stack-carried` as a landing of `w1` into `main` killed after the target
/// moved, a dependent `w2` carried, and a hand commit on `main` after: abort
/// refuses the foreign tip and prints `--leave-target`, which leaves `main`
/// as it stands and puts `w1` and `w2` back where they were.
pub fn abort_leaving_the_target_of_a_landing_that_carried_a_dependent() -> Scenario {
    let mut steps = stack_on_a_seat();
    steps.extend(main_moves_app("main-app"));
    steps.extend([
        Step::rwv_paused(&ww("w1"), &["land"], "landed", "landing"),
        Step::kill("landing"),
        Step::git(&a(), Repo::App, &["log", "--format=%s", "main"])
            .claim(Claim::OutputContains("w1-app")),
        Step::hand_commit(&a(), Repo::Proj, "main", &[("late.txt", "late\n")]),
        Step::rwv(&ww("w1"), &["abort"])
            .expect(Exit::refuses_kind(RefusalKind::ForeignTip))
            .claim(Claim::OutputLacks("abort stopped there"))
            .claim(Claim::OutputContains("rwv abort --leave-target"))
            .claim(Claim::LocalStateUnchanged(a()))
            .claim(Claim::LocalStateUnchanged(ww("w1"))),
        Step::rwv_printed_exactly(&ww("w1"), "rwv abort --leave-target")
            .expect(Exit::Succeeds)
            .may_rewind(&[Repo::App, Repo::Proj])
            .may_restore(&[Repo::App, Repo::Proj]),
        Step::git(&a(), Repo::App, &["log", "--format=%s", "main"])
            .claim(Claim::OutputContains("w1-app")),
        Step::git(&a(), Repo::Proj, &["log", "--format=%s", "main"])
            .claim(Claim::OutputContains("hand commit")),
        Step::git(&ww("w1"), Repo::App, &["log", "--format=%s", "w2"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputLacks("main-app"))
            .claim(Claim::OutputContains("w2-app")),
        Step::git(&ww("w1"), Repo::App, &["log", "--format=%s", "proj--w1"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputLacks("main-app")),
        Step::rwv(&ww("w1"), &["status"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputLacks("in progress")),
    ]);
    scenario(
        "stack: abort --leave-target of a landing that carried a dependent",
        steps,
    )
}

/// `stack-carried`, `w2` tracking a file `w1`'s checkout of `app` holds
/// ignored: the carry composes in that checkout, which would write over the
/// file, so the sync refuses before any dependent moves, naming it, the file
/// as it was, and once the file is moved aside `--continue` carries `w2`.
pub fn a_carry_refuses_to_write_over_an_ignored_file() -> Scenario {
    let mut steps = stack_on_a_seat();
    steps.extend([
        Step::hand_commit(
            &ww("w1"),
            Repo::App,
            "w2",
            &[("scratch.cfg", "theirs\n")],
        ),
        Step::git(
            &ww("w1"),
            Repo::App,
            &[
                "-c",
                "alias.mk=!echo scratch.cfg >> \"$(git rev-parse --git-path info/exclude)\"; printf mine > scratch.cfg",
                "mk",
            ],
        ),
    ]);
    steps.extend(main_moves_app("main-app"));
    steps.extend([
        Step::rwv(&ww("w1"), &["sync"])
            .expect(Exit::refuses_kind(RefusalKind::UntrackedCollision))
            .claim(Claim::OutputContains("scratch.cfg"))
            .claim(Claim::OutputContains("rwv sync --continue")),
        Step::git(
            &ww("w1"),
            Repo::App,
            &["-c", "alias.slurp=!cat scratch.cfg", "slurp"],
        )
        .claim(Claim::OutputContains("mine")),
        Step::git(
            &ww("w1"),
            Repo::App,
            &["-c", "alias.aside=!mv scratch.cfg aside.cfg", "aside"],
        ),
        Step::rwv(&ww("w1"), &["sync", "--continue"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputContains("carried 'w2' along")),
        Step::git(
            &ww("w1"),
            Repo::App,
            &["-c", "alias.slurp=!cat aside.cfg", "slurp"],
        )
        .claim(Claim::OutputContains("mine")),
    ]);
    scenario(
        "stack: a carry refuses to write over an ignored file",
        steps,
    )
}

/// `stack-carried` under a `pre-rebase` hook that refuses the replay of
/// `w2`'s commits: the carry's replay fails with no conflict, the sync
/// refuses naming `rwv sync --continue` and `rwv abort`, the checkout is back
/// on `w1`'s branch with no rebase in progress, and once the hook is gone
/// `--continue` carries `w2`.
pub fn a_carry_replay_that_fails_without_a_conflict_leaves_the_checkout_on_its_branch() -> Scenario
{
    let mut steps = stack_on_a_seat();
    steps.extend(main_moves_app("main-app"));
    steps.extend([
        Step::git(
            &ww("w1"),
            Repo::App,
            &[
                "-c",
                "alias.plant=!d=\"$(git rev-parse --git-path hooks)\"; mkdir -p \"$d\"; printf '#!/bin/sh\\n[ \"$(git log -1 --format=%%s)\" = w2-app ] && exit 1\\nexit 0\\n' > \"$d/pre-rebase\"; chmod +x \"$d/pre-rebase\"",
                "plant",
            ],
        ),
        Step::rwv(&ww("w1"), &["sync"])
            .expect(Exit::refuses_kind(RefusalKind::OpParked))
            .claim(Claim::OutputContains("rwv sync --continue"))
            .claim(Claim::OutputContains("rwv abort")),
        Step::git(&ww("w1"), Repo::App, &["symbolic-ref", "--short", "HEAD"])
            .claim(Claim::OutputLine("proj--w1")),
        Step::git(
            &ww("w1"),
            Repo::App,
            &["-c", "alias.nohook=!rm -f \"$(git rev-parse --git-path hooks)/pre-rebase\"", "nohook"],
        ),
        Step::rwv(&ww("w1"), &["sync", "--continue"])
            .expect(Exit::Succeeds)
            .may_rewind(&[Repo::App])
            .claim(Claim::OutputContains("carried 'w2' along")),
    ]);
    scenario(
        "stack: a carry replay that fails without a conflict leaves the checkout on its branch",
        steps,
    )
}

/// `stack-carried`, killed after the carry moved `w2` and before the sync
/// reported it: the printed `rwv sync --continue` finds the moves made and
/// still says what was carried and where the old tip is kept.
pub fn a_sync_resumed_after_its_carry_moved_reports_the_carry() -> Scenario {
    let mut steps = stack_on_a_seat();
    steps.extend(main_moves_app("main-app"));
    steps.extend([
        Step::rwv_paused(&ww("w1"), &["sync"], "carry-moved", "carrying"),
        Step::kill("carrying"),
        Step::rwv(&ww("w1"), &["sync", "--continue"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputContains("carried 'w2' along"))
            .claim(Claim::OutputContains(
                "the tip it left kept in the rebased backup w2/rebased-",
            )),
    ]);
    scenario(
        "stack: a sync resumed after its carry moved reports the carry",
        steps,
    )
}

/// `stack-carried`, with a signer that fails under `commit.gpgSign`: `w1`'s
/// rebase stops with no conflict in the checkout, and the sync says it
/// failed rather than that it stopped on a conflict; the printed `rwv abort`
/// puts everything back.
pub fn a_rebase_that_stops_without_a_conflict_is_not_reported_as_one() -> Scenario {
    let mut steps = stack_on_a_seat();
    steps.extend(main_moves_app("main-app"));
    steps.extend([
        Step::git(&ww("w1"), Repo::App, &["config", "commit.gpgsign", "true"]),
        Step::git(&ww("w1"), Repo::App, &["config", "gpg.program", "false"]),
        Step::rwv(&ww("w1"), &["sync"])
            .expect(Exit::refuses_kind(RefusalKind::OpParked))
            .claim(Claim::OutputContains("failed at"))
            .claim(Claim::OutputLacks("conflict"))
            .claim(Claim::OutputContains("rwv abort")),
        Step::rwv(&ww("w1"), &["abort"])
            .expect(Exit::Succeeds)
            .may_rewind(&[Repo::App])
            .may_restore(&[Repo::App, Repo::Proj]),
    ]);
    scenario(
        "stack: a rebase that stops without a conflict is not reported as one",
        steps,
    )
}

/// `stack-carried`, with `w2` opened in a workweave of its own after its
/// carry composed and before it moved: the sync moves no dependent and
/// refuses with the dependent checked out elsewhere, printing no landing
/// the carry's claim would refuse and deletion of that workweave would also
/// be refused by; the printed `rwv abort` ends the operation, and the rerun
/// offers the landing and the delete, which clears it.
pub fn a_dependent_opened_elsewhere_as_its_carry_moves_it_is_not_moved() -> Scenario {
    let mut steps = stack_on_a_seat();
    steps.extend(main_moves_app("main-app"));
    steps.extend([
        Step::rwv_paused(&ww("w1"), &["sync"], "carry-move", "carrying"),
        Step::rwv(
            &a(),
            &[
                "workweave",
                "create",
                "--project",
                "proj",
                "w2s",
                "--branch",
                "w2",
            ],
        )
        .expect(Exit::Succeeds),
        Step::release("carrying")
            .expect(Exit::refuses(Repo::Proj, State::TargetDependentCheckedOut))
            .claim(Claim::OutputContains("w2 lands into proj--w1"))
            .claim(Claim::OutputContains("rwv abort"))
            .claim(Claim::OutputLacks("rwv land"))
            .claim(Claim::OutputLacks("rwv workweave delete"))
            .claim(Claim::Lacks {
                at: ww("w1"),
                repo: Repo::App,
                rev: "w2",
                mark: "main-app",
            }),
        Step::rwv_printed_exactly(&ww("w1"), "rwv abort")
            .expect(Exit::Succeeds)
            .may_rewind(&[Repo::App]),
        Step::rwv(&ww("w1"), &["sync"])
            .expect(Exit::refuses(Repo::Proj, State::TargetDependentCheckedOut))
            .claim(Claim::OutputContains("rwv land")),
        Step::rwv_printed(&a(), "rwv workweave delete w2s").expect(Exit::Code(3)),
        Step::rwv(&ww("w1"), &["sync"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputContains("carried 'w2' along")),
    ]);
    scenario(
        "stack: a dependent opened elsewhere as its carry moves it is not moved",
        steps,
    )
}

/// How the refusal of a continued carry, met a dependent checked out
/// elsewhere, is cleared.
#[derive(Debug, Clone, Copy)]
pub enum ContinuedCarryMeets {
    /// C16: the dependent's workweave deleted, then the carry continued.
    WorkweaveDelete,
    /// C12: the dependent rebound onto `main`, then the carry continued.
    Rebind,
    /// `rwv abort`.
    Abort,
}

/// `stack-carry-conflict` stopped on `w2`, resolved by hand, and `w3`
/// recorded onto `w2` and checked out in a workweave of its own: the carry
/// holds the landing claim on `w2`, so the refusal of `rwv sync --continue`
/// prints no landing, which the claim would refuse; each choice it does
/// print, and `rwv abort`, runs as printed to the end of the operation.
pub fn a_stopped_carry_continued_meets_a_dependent_checked_out(
    ends: ContinuedCarryMeets,
) -> Scenario {
    let mut steps = vec![
        Step::rwv(&a(), &["branch", "set-history", "linear"]).expect(Exit::Succeeds),
        create("w1"),
        Step::commit(&ww("w1"), Repo::App, "w1-app"),
        Step::rwv(&ww("w1"), &["lock", "--commit"]).expect(Exit::Succeeds),
        dependent_of(&ww("w1"), "w2", "proj--w1"),
        Step::hand_edit(&ww("w1"), Repo::App, "README", "app", "beta"),
        Step::git(&ww("w1"), Repo::App, &["commit", "-qam", "w2-app"]),
        Step::rwv(&ww("w1"), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::rwv(&ww("w1"), &["switch", "proj--w1"]).expect(Exit::Succeeds),
        Step::hand_edit(&a(), Repo::App, "README", "app", "alpha"),
        Step::git(&a(), Repo::App, &["commit", "-qam", "main-app"]),
        Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::rwv(&ww("w1"), &["sync"])
            .expect(Exit::refuses_kind(RefusalKind::OpParked))
            .claim(Claim::OutputContains(
                "carrying 'w2' along in local/remotes/app",
            )),
        Step::git(&ww("w1"), Repo::App, &["checkout", "--theirs", "README"]),
        Step::git(&ww("w1"), Repo::App, &["add", "README"]),
        create("w3"),
        dependent_of(&ww("w3"), "w3", "w2"),
        Step::commit(&ww("w3"), Repo::Lib, "w3-lib"),
        Step::rwv(&ww("w3"), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::rwv(&ww("w1"), &["sync", "--continue"])
            .expect(Exit::refuses(Repo::Proj, State::TargetDependentCheckedOut))
            .claim(Claim::OutputContains("w3 lands into w2"))
            .claim(Claim::OutputLacks("rwv land"))
            .claim(Claim::OutputContains("rwv workweave delete w3"))
            .claim(Claim::OutputContains(
                "rwv branch set-mode w3 feature --target main",
            )),
    ];
    match ends {
        ContinuedCarryMeets::WorkweaveDelete => steps.extend([
            Step::rwv_printed(&a(), "rwv workweave delete w3").expect(Exit::Code(3)),
            Step::rwv(&ww("w1"), &["sync", "--continue"])
                .expect(Exit::Succeeds)
                .resolved_by_hand(&[Repo::App])
                .may_rewind(&[Repo::App])
                .claim(Claim::OutputContains("carried 'w2' along")),
        ]),
        ContinuedCarryMeets::Rebind => steps.extend([
            Step::rwv_printed(&ww("w1"), "rwv branch set-mode w3 ").expect(Exit::Succeeds),
            Step::rwv(&ww("w1"), &["sync", "--continue"])
                .expect(Exit::Succeeds)
                .resolved_by_hand(&[Repo::App])
                .may_rewind(&[Repo::App])
                .claim(Claim::OutputContains("carried 'w2' along")),
        ]),
        ContinuedCarryMeets::Abort => steps.push(
            Step::rwv(&ww("w1"), &["abort"])
                .expect(Exit::Succeeds)
                .may_rewind(&[Repo::App])
                .claim(Claim::OutputLacks("foreign")),
        ),
    }
    scenario(
        "stack: a continued carry meets a dependent checked out elsewhere",
        steps,
    )
}

/// `stack-carried` under an operator's `rebase.updateRefs`, with `w2`
/// holding no `app` commit of its own, so it sits at `w1`'s tip there: the
/// sync's rebase of `w1` leaves `w2` where it is, and the carry moves it.
pub fn stack_at_the_tip_under_update_refs() -> Scenario {
    let mut steps = vec![
        Step::rwv(&a(), &["branch", "set-history", "linear"]).expect(Exit::Succeeds),
        create("w1"),
        Step::commit(&ww("w1"), Repo::App, "w1-app"),
        Step::rwv(&ww("w1"), &["lock", "--commit"]).expect(Exit::Succeeds),
        dependent_of(&ww("w1"), "w2", "proj--w1"),
        Step::rwv(&ww("w1"), &["switch", "proj--w1"]).expect(Exit::Succeeds),
        Step::git(
            &ww("w1"),
            Repo::App,
            &["config", "rebase.updateRefs", "true"],
        ),
    ];
    steps.extend(main_moves_app("main-app"));
    steps.push(
        Step::rwv(&ww("w1"), &["sync"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputContains("carried 'w2' along"))
            .claim(Claim::Contains {
                at: ww("w1"),
                repo: Repo::App,
                rev: "w2",
                mark: "main-app",
            }),
    );
    scenario(
        "stack: under updateRefs, a dependent at the target's tip is carried",
        steps,
    )
}

/// `w3` onto `w2` onto `w1` under an operator's `rebase.updateRefs`, `w3`'s
/// replay conflicting with `main`: the sync stops on `w3` with `w2`
/// composed and not moved, and `rwv abort` leaves `w2` as it was, nothing
/// stranded.
pub fn stack_carry_stopped_under_update_refs() -> Scenario {
    let steps = vec![
        Step::rwv(&a(), &["branch", "set-history", "linear"]).expect(Exit::Succeeds),
        create("w1"),
        Step::commit(&ww("w1"), Repo::App, "w1-app"),
        Step::rwv(&ww("w1"), &["lock", "--commit"]).expect(Exit::Succeeds),
        dependent_of(&ww("w1"), "w2", "proj--w1"),
        Step::commit(&ww("w1"), Repo::App, "w2-app"),
        Step::rwv(&ww("w1"), &["lock", "--commit"]).expect(Exit::Succeeds),
        dependent_of(&ww("w1"), "w3", "w2"),
        Step::hand_edit(&ww("w1"), Repo::App, "README", "app", "beta"),
        Step::git(&ww("w1"), Repo::App, &["commit", "-qam", "w3-app"]),
        Step::rwv(&ww("w1"), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::rwv(&ww("w1"), &["switch", "proj--w1"]).expect(Exit::Succeeds),
        Step::git(
            &ww("w1"),
            Repo::App,
            &["config", "rebase.updateRefs", "true"],
        ),
        Step::hand_edit(&a(), Repo::App, "README", "app", "alpha"),
        Step::git(&a(), Repo::App, &["commit", "-qam", "main-app"]),
        Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::rwv(&ww("w1"), &["sync"])
            .expect(Exit::refuses_kind(RefusalKind::OpParked))
            .claim(Claim::OutputContains("carrying 'w3' along")),
        Step::git(&ww("w1"), Repo::App, &["log", "--format=%s", "w2"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputLacks("main-app")),
        Step::rwv(&ww("w1"), &["abort"])
            .expect(Exit::Succeeds)
            .may_rewind(&[Repo::App])
            .claim(Claim::OutputLacks("foreign")),
        Step::git(&ww("w1"), Repo::App, &["log", "--format=%s", "w2"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputLacks("main-app")),
        Step::git(&ww("w1"), Repo::App, &["log", "--format=%s", "-1", "w2"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputLine("w2-app")),
    ];
    scenario(
        "stack: under updateRefs, a stopped carry moves no dependent and aborts clean",
        steps,
    )
}

/// `git-below-2.38`: git answers as a release without `merge-tree
/// --write-tree`. `main` merges its project branch and rebases `app`;
/// `w1`'s landing, with `w2` landing into `w1`, merges the project in the
/// source's checkout and carries `w2` along per dependent, `app` replayed
/// and `w2`'s project branch re-pinned in place, and nothing refuses for the
/// git release.
#[cfg(unix)]
pub fn git_below_2_38() -> Scenario {
    let mut steps = vec![
        Step::rwv(&a(), &["branch", "set-history", "merge"]).expect(Exit::Succeeds),
        Step::rwv(
            &a(),
            &[
                "branch",
                "set-history",
                "linear",
                "--repo",
                "local/remotes/app",
            ],
        )
        .expect(Exit::Succeeds),
        create("w1"),
        Step::commit(&ww("w1"), Repo::App, "w1-app"),
        Step::rwv(&ww("w1"), &["lock", "--commit"]).expect(Exit::Succeeds),
        dependent_of(&ww("w1"), "w2", "proj--w1"),
        Step::commit(&ww("w1"), Repo::App, "w2-app"),
        Step::rwv(&ww("w1"), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::rwv(&ww("w1"), &["switch", "proj--w1"]).expect(Exit::Succeeds),
    ];
    steps.extend(main_moves_app("main-app"));
    steps.extend([
        Step::rwv(&ww("w1"), &["land"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputContains("carried 'w2' along"))
            .claim(Claim::Contains {
                at: ww("w1"),
                repo: Repo::App,
                rev: "w2",
                mark: "main-app",
            })
            .claim(Claim::PinsReplayed {
                at: ww("w1"),
                rev: "w2",
                member: Repo::App,
                subject: "w2-app",
                reached_from: "w2",
            }),
        Step::git(
            &ww("w1"),
            Repo::Proj,
            &["rev-parse", "--verify", "-q", "HEAD^2"],
        )
        .expect(Exit::Succeeds),
    ]);
    Scenario {
        name: "git below 2.38: a merge in the checkout and a carried stack",
        seed: Seed::default().old_git(),
        steps,
    }
}

/// `stack-carried`, by a landing: `w1`'s `linear` landing rebases `w1`
/// onto the moved `main` in its seat and carries `w2` along before `main`
/// fast-forwards.
/// `stack-carried-by-a-landing` killed as advance-target begins, after the carry
/// moved `w2` and before `main` moved: `rwv land --continue` reads `w2` as
/// carried, not as lacking `w1`'s tip, and the landing completes.
pub fn a_landing_killed_after_its_carry_continues() -> Scenario {
    let mut steps = stack_on_a_seat();
    steps.extend(main_moves_app("main-app"));
    steps.extend([
        Step::rwv_paused(&ww("w1"), &["land"], "composed", "landing"),
        Step::kill("landing"),
        Step::rwv(&ww("w1"), &["land", "--continue"])
            .expect(Exit::Succeeds)
            .claim(Claim::Contains {
                at: ww("w1"),
                repo: Repo::App,
                rev: "w2",
                mark: "main-app",
            })
            .claim(Claim::OutputLacks("lacks")),
        Step::git(
            &ww("w1"),
            Repo::App,
            &["merge-base", "--is-ancestor", "proj--w1", "main"],
        )
        .expect(Exit::Succeeds),
    ]);
    scenario(
        "stack: a landing killed after its carry moved continues",
        steps,
    )
}

/// `a-landing-killed-after-its-carry-continues`, with a commit made on
/// `w1` before the resume: the carry does not compose `w2` again, so
/// `rwv land --continue` completes with `w2` where the carry moved it, and
/// `w2` takes the commit at its own next sync.
pub fn a_landing_resumed_after_its_target_moved_past_a_carry() -> Scenario {
    let mut steps = stack_on_a_seat();
    steps.extend(main_moves_app("main-app"));
    steps.extend([
        Step::rwv_paused(&ww("w1"), &["land"], "composed", "landing"),
        Step::kill("landing"),
        Step::commit(&ww("w1"), Repo::App, "w1-later"),
        Step::rwv(&ww("w1"), &["land", "--continue"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputLacks("lacks"))
            .claim(Claim::OutputLacks("moved mid-operation"))
            .claim(Claim::Contains {
                at: ww("w1"),
                repo: Repo::App,
                rev: "w2",
                mark: "main-app",
            })
            .claim(Claim::Lacks {
                at: ww("w1"),
                repo: Repo::App,
                rev: "w2",
                mark: "w1-later",
            }),
        Step::rwv(
            &a(),
            &[
                "workweave",
                "create",
                "--project",
                "proj",
                "w2s",
                "--branch",
                "w2",
            ],
        )
        .expect(Exit::Succeeds),
        Step::rwv(&ww("w2s"), &["sync"])
            .expect(Exit::Succeeds)
            .claim(Claim::Contains {
                at: ww("w2s"),
                repo: Repo::App,
                rev: "HEAD",
                mark: "w1-later",
            }),
    ]);
    scenario(
        "stack: a landing resumed after its target moved past a carry",
        steps,
    )
}

pub fn stack_carried_by_a_landing() -> Scenario {
    let mut steps = stack_on_a_seat();
    steps.extend(main_moves_app("main-app"));
    steps.push(
        Step::rwv(&ww("w1"), &["land"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputContains("carried 'w2' along"))
            .claim(Claim::Contains {
                at: ww("w1"),
                repo: Repo::App,
                rev: "w2",
                mark: "main-app",
            })
            .claim(Claim::PinsReplayed {
                at: ww("w1"),
                rev: "w2",
                member: Repo::App,
                subject: "w2-app",
                reached_from: "w2",
            })
            .claim(Claim::PinsReplayed {
                at: a(),
                rev: "main",
                member: Repo::App,
                subject: "w1-app",
                reached_from: "main",
            }),
    );
    steps.push(
        Step::git(
            &ww("w1"),
            Repo::Proj,
            &["merge-base", "--is-ancestor", "main", "w2"],
        )
        .expect(Exit::Succeeds),
    );
    scenario("stack: a landing carries a dependent along", steps)
}

/// `stack-cycle`: a record naming its own branch as its target is refused
/// at write; a cycle formed by merging two machines' records (`x -> y`
/// written on A, `y -> x` on B) reads as `target` cycle, refusing a
/// deletion and a sync of either branch with that state alone, and C12
/// settles it.
pub fn stack_cycle() -> Scenario {
    let set_mode = |at: Place, branch: &'static str, target: &'static str| {
        Step::rwv(
            &at,
            &["branch", "set-mode", branch, "feature", "--target", target],
        )
        .expect(Exit::Succeeds)
    };
    let push = |at: Place, branch: &'static str| {
        publishes_records(Step::rwv(&at, &["push", "--branch", branch]))
            .pushes(Remote::origin(Repo::Proj), branch)
            .expect(Exit::Succeeds)
    };
    scenario(
        "stack: a target cycle, at write and by merge",
        vec![
            create("w1"),
            Step::rwv(
                &ww("w1"),
                &[
                    "branch", "set-mode", "proj--w1", "feature", "--target", "proj--w1",
                ],
            )
            .expect(Exit::refuses(Repo::Proj, State::TargetCycle)),
            Step::git(&a(), Repo::Proj, &["branch", "x"]),
            Step::git(&a(), Repo::Proj, &["branch", "y"]),
            set_mode(a(), "y", "main"),
            set_mode(a(), "x", "main"),
            push(a(), "y"),
            push(a(), "x"),
            Step::new_machine(Machine::B),
            set_mode(a(), "x", "y"),
            push(a(), "x"),
            Step::git(&b(), Repo::Proj, &["branch", "y", "origin/y"]),
            set_mode(b(), "y", "x"),
            Step::rwv(&b(), &["fetch"]).expect(Exit::Succeeds),
            Step::rwv(&b(), &["push", "--branch", "y"])
                .expect(Exit::refuses(Repo::Proj, State::TargetCycle))
                .claim(Claim::JudgedOnly(Repo::Proj, State::TargetCycle)),
            Step::rwv(&b(), &["branch", "delete", "x"])
                .expect(Exit::refuses(Repo::Proj, State::TargetCycle))
                .claim(Claim::JudgedOnly(Repo::Proj, State::TargetCycle))
                .claim(Claim::OutputContains("rwv branch set-mode")),
            set_mode(b(), "y", "main"),
            Step::rwv(&b(), &["branch", "delete", "y"])
                .expect(Exit::refuses(Repo::Proj, State::TargetDependents))
                .claim(Claim::JudgedOnly(Repo::Proj, State::TargetDependents)),
        ],
    )
}

/// `stack-remote-dependent`: B publishes `w2`, landing into A's published
/// `w1`. A's `w1` `sync` under a moved `linear` `main` reads `w2` as on
/// another machine: nothing moves, and C35 (on its machine) and C12 are
/// printed; A's `branch delete w1` refuses too. Once A rebinds `w2` here by
/// the printed C12, the sync carries nothing and passes.
pub fn stack_remote_dependent() -> Scenario {
    let mut steps = vec![
        Step::rwv(&a(), &["branch", "set-history", "linear"]).expect(Exit::Succeeds),
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
        Step::commit(&ww("w1"), Repo::App, "w1-app"),
        Step::rwv(&ww("w1"), &["lock", "--commit"]).expect(Exit::Succeeds),
        publishes_records(Step::rwv(&ww("w1"), &["push"]))
            .pushes(Remote::origin(Repo::App), "w1")
            .pushes(Remote::origin(Repo::Proj), "w1")
            .expect(Exit::Succeeds),
        Step::new_machine(Machine::B),
        Step::rwv(
            &b(),
            &[
                "workweave",
                "create",
                "--project",
                "proj",
                "w2",
                "--new-branch",
                "w2",
                "--target",
                "w1",
            ],
        )
        .expect(Exit::Succeeds),
        Step::commit(&b_ww("w2"), Repo::App, "w2-app"),
        Step::rwv(&b_ww("w2"), &["lock", "--commit"]).expect(Exit::Succeeds),
        publishes_records(Step::rwv(&b_ww("w2"), &["push"]))
            .pushes(Remote::origin(Repo::App), "w2")
            .pushes(Remote::origin(Repo::Proj), "w2")
            .expect(Exit::Succeeds),
        Step::rwv(&a(), &["fetch"]).expect(Exit::Succeeds),
    ];
    steps.extend(main_moves_app("main-app"));
    steps.extend([
        Step::rwv(&ww("w1"), &["sync"])
            .expect(Exit::refuses(
                Repo::Proj,
                State::TargetDependentOnAnotherMachine,
            ))
            .claim(Claim::JudgedOnly(
                Repo::Proj,
                State::TargetDependentOnAnotherMachine,
            ))
            .claim(Claim::OutputContains("w2 lands into w1"))
            .claim(Claim::OutputContains("rwv land"))
            .claim(Claim::OutputContains(
                "rwv branch set-mode w2 feature --target main",
            ))
            .claim(Claim::LocalStateUnchanged(ww("w1"))),
        Step::rwv(&a(), &["branch", "delete", "w1"])
            .expect(Exit::refuses(Repo::Proj, State::TargetDependents))
            .claim(Claim::LocalStateUnchanged(a())),
        Step::rwv_printed(&ww("w1"), "rwv branch set-mode w2 ").expect(Exit::Succeeds),
        Step::rwv(&ww("w1"), &["sync"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputLacks("carried")),
    ]);
    scenario("stack: a dependent published from another machine", steps)
}

/// A member's branch moved under its seat, the seat refreshed by hand with a
/// git command that writes no reflog entry, then a staged return of every
/// path the move changed: three returns, each after three hand refreshes.
/// The index, files and reflog are then those of a seat never refreshed, so
/// nothing git records tells the staged return from drift. The landing
/// refreshes it and names each file; what it replaces is the moved-from tip's
/// content, which history holds.
pub fn drift_staged_return_after_a_hand_refresh() -> Scenario {
    struct Seat {
        name: &'static str,
        added: Option<&'static str>,
        changed: Option<&'static str>,
    }
    let refreshes: [&[&str]; 3] = [
        &["checkout", "HEAD", "--", "."],
        &["restore", "--staged", "--worktree", "--source=HEAD", "."],
        &["read-tree", "-m", "-u", "HEAD"],
    ];
    let seat = |name, added, changed| Seat {
        name,
        added,
        changed,
    };
    let seats: [[Seat; 3]; 3] = [
        [
            seat("q3-add-1", Some("q3-add-1-new.txt"), None),
            seat("q3-add-2", Some("q3-add-2-new.txt"), None),
            seat("q3-add-3", Some("q3-add-3-new.txt"), None),
        ],
        [
            seat("q3-change-1", None, Some("q3-change-1.txt")),
            seat("q3-change-2", None, Some("q3-change-2.txt")),
            seat("q3-change-3", None, Some("q3-change-3.txt")),
        ],
        [
            seat("q3-two-1", Some("q3-two-1-new.txt"), Some("q3-two-1.txt")),
            seat("q3-two-2", Some("q3-two-2-new.txt"), Some("q3-two-2.txt")),
            seat("q3-two-3", Some("q3-two-3-new.txt"), Some("q3-two-3.txt")),
        ],
    ];
    let mut steps = Vec::new();
    for variant in &seats {
        for (refresh, seat) in refreshes.iter().zip(variant) {
            let at = ww(seat.name);
            let branch = format!("proj--{}", seat.name);
            steps.push(create(seat.name));
            if seat.changed.is_some() {
                steps.push(Step::commit(&at, Repo::Lib, seat.name));
            }
            let moved: Vec<(&str, &str)> = seat
                .changed
                .map(|file| (file, "moved\n"))
                .into_iter()
                .chain(seat.added.map(|file| (file, "new\n")))
                .collect();
            steps.push(Step::hand_commit(&a(), Repo::Lib, &branch, &moved));
            steps.push(Step::git(&at, Repo::Lib, refresh));
            match (seat.added, seat.changed) {
                (Some(_), Some(_)) => {
                    steps.push(Step::git(
                        &at,
                        Repo::Lib,
                        &["revert", "--no-commit", "HEAD"],
                    ));
                    steps.push(Step::git(&at, Repo::Lib, &["revert", "--quit"]));
                }
                (Some(added), None) => {
                    steps.push(Step::git(&at, Repo::Lib, &["rm", "-q", added]));
                }
                (None, Some(changed)) => {
                    steps.push(Step::git(
                        &at,
                        Repo::Lib,
                        &["checkout", "HEAD~1", "--", changed],
                    ));
                }
                (None, None) => unreachable!("every seat's move changes a path"),
            }
            steps.push(
                Step::git(&at, Repo::Lib, &["diff", "--cached", "--quiet", "HEAD"])
                    .expect(Exit::Code(1)),
            );
            steps.push(
                Step::git(&at, Repo::Lib, &["diff", "--cached", "--quiet", "HEAD~1"])
                    .expect(Exit::Succeeds),
            );
            steps.push(
                Step::git(&at, Repo::Lib, &["diff", "--quiet", "HEAD~1"]).expect(Exit::Succeeds),
            );
            let mut land = Step::rwv(&at, &["land", "--history", "linear"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains("refreshed drift"));
            for file in seat.added.into_iter().chain(seat.changed) {
                land = land.claim(Claim::OutputContains(file));
            }
            steps.push(land);
        }
    }
    scenario("drift staged return after a hand refresh", steps)
}

/// A staged return after a hand refresh, one of whose files then gets content
/// no commit holds: that file is an edit and is never replaced, and the
/// landing refuses it.
pub fn drift_staged_return_with_an_edit_is_kept() -> Scenario {
    let at = ww("q3-edit");
    scenario(
        "drift staged return with an edit is kept",
        vec![
            create("q3-edit"),
            Step::commit(&at, Repo::Lib, "q3-edit"),
            Step::hand_commit(
                &a(),
                Repo::Lib,
                "proj--q3-edit",
                &[("q3-edit.txt", "moved\n")],
            ),
            Step::git(&at, Repo::Lib, &["checkout", "HEAD", "--", "."]),
            Step::git(&at, Repo::Lib, &["checkout", "HEAD~1", "--", "q3-edit.txt"]),
            Step::hand_edit(&at, Repo::Lib, "q3-edit.txt", "q3-edit", "mine"),
            Step::git(&at, Repo::Lib, &["add", "q3-edit.txt"]),
            Step::rwv(&at, &["land", "--history", "linear"])
                .expect(Exit::refuses(Repo::Lib, State::EditsDirty))
                .claim(Claim::OutputLacks("refreshed drift")),
            Step::git(
                &at,
                Repo::Lib,
                &["grep", "-q", "--cached", "mine", "--", "q3-edit.txt"],
            ),
            Step::git(&at, Repo::Lib, &["grep", "-q", "mine", "--", "q3-edit.txt"]),
        ],
    )
}

/// What machine B has of `w2` beyond what it landed into `w1`.
#[derive(Debug, Clone, Copy)]
pub enum RemoteDependentLanded {
    /// Nothing: every commit of `w2` it published has landed.
    Wholly,
    /// One more commit, published and not landed.
    ButOneCommit,
}

/// `stack-remote-dependent`, with B landing its `w2` into `w1` and pushing
/// `w1`, A then pulling `w1`. A's `sync` under a moved `linear` `main`
/// reads `w2` as landed, its published copy held by `w1`'s tip: it carries
/// nothing, says so, and passes. Where B published one more commit of `w2`
/// it did not land, the sync refuses with C35 printed; B runs that landing
/// as printed and pushes, and A's next sync passes.
pub fn stack_remote_dependent_landed(published: RemoteDependentLanded) -> Scenario {
    let push_w1 = || {
        publishes_records(Step::rwv(&b(), &["push", "--branch", "w1"]))
            .pushes(Remote::origin(Repo::App), "w1")
            .pushes(Remote::origin(Repo::Proj), "w1")
            .expect(Exit::Succeeds)
    };
    let mut steps = vec![
        Step::rwv(&a(), &["branch", "set-history", "linear"]).expect(Exit::Succeeds),
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
        Step::commit(&ww("w1"), Repo::App, "w1-app"),
        Step::rwv(&ww("w1"), &["lock", "--commit"]).expect(Exit::Succeeds),
        publishes_records(Step::rwv(&ww("w1"), &["push"]))
            .pushes(Remote::origin(Repo::App), "w1")
            .pushes(Remote::origin(Repo::Proj), "w1")
            .expect(Exit::Succeeds),
        Step::new_machine(Machine::B),
        Step::rwv(
            &b(),
            &[
                "workweave",
                "create",
                "--project",
                "proj",
                "w2",
                "--new-branch",
                "w2",
                "--target",
                "w1",
            ],
        )
        .expect(Exit::Succeeds),
        Step::commit(&b_ww("w2"), Repo::App, "w2-app"),
        Step::rwv(&b_ww("w2"), &["lock", "--commit"]).expect(Exit::Succeeds),
        publishes_records(Step::rwv(&b_ww("w2"), &["push"]))
            .pushes(Remote::origin(Repo::App), "w2")
            .pushes(Remote::origin(Repo::Proj), "w2")
            .expect(Exit::Succeeds),
        Step::rwv(&b_ww("w2"), &["land"]).expect(Exit::Succeeds),
        push_w1(),
    ];
    if let RemoteDependentLanded::ButOneCommit = published {
        steps.extend([
            Step::commit(&b_ww("w2"), Repo::App, "w2-more"),
            Step::rwv(&b_ww("w2"), &["lock", "--commit"]).expect(Exit::Succeeds),
            publishes_records(Step::rwv(&b_ww("w2"), &["push"]))
                .pushes(Remote::origin(Repo::App), "w2")
                .pushes(Remote::origin(Repo::Proj), "w2")
                .expect(Exit::Succeeds),
        ]);
    }
    steps.extend([
        Step::rwv(&a(), &["fetch"]).expect(Exit::Succeeds),
        Step::rwv(&ww("w1"), &["pull"]).expect(Exit::Succeeds),
    ]);
    steps.extend(main_moves_app("main-app"));
    let passes = || {
        Step::rwv(&ww("w1"), &["sync"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputContains(
                "'w2' has landed into 'w1': nothing to carry; its branch still exists on origin",
            ))
            .claim(Claim::OutputLacks("carried"))
            .claim(Claim::Contains {
                at: ww("w1"),
                repo: Repo::App,
                rev: "HEAD",
                mark: "main-app",
            })
    };
    match published {
        RemoteDependentLanded::Wholly => steps.push(passes()),
        RemoteDependentLanded::ButOneCommit => steps.extend([
            Step::rwv(&ww("w1"), &["sync"])
                .expect(Exit::refuses(
                    Repo::Proj,
                    State::TargetDependentOnAnotherMachine,
                ))
                .claim(Claim::JudgedOnly(
                    Repo::Proj,
                    State::TargetDependentOnAnotherMachine,
                ))
                .claim(Claim::OutputContains("w2 lands into w1"))
                .claim(Claim::LocalStateUnchanged(ww("w1"))),
            Step::rwv_printed(&b_ww("w2"), "rwv land").expect(Exit::Succeeds),
            push_w1(),
            Step::rwv(&a(), &["fetch"]).expect(Exit::Succeeds),
            Step::rwv(&ww("w1"), &["pull"]).expect(Exit::Succeeds),
            passes(),
        ]),
    }
    scenario(
        "stack: a dependent published from another machine and landed there",
        steps,
    )
}

enum MovedOnA {
    App,
    Lib,
}

/// `A` holds a locked commit in a member and has not fetched; `B` has since
/// published the project branch with a manifest without `docs` and, where
/// `b_moves_lib` is set, a locked commit in `lib`.
fn project_ahead_and_behind(a_moves: MovedOnA, b_moves_lib: bool, drops_docs: bool) -> Vec<Step> {
    let mut steps = vec![Step::new_machine(Machine::B)];
    steps.push(match a_moves {
        MovedOnA::App => Step::commit(&a(), Repo::App, "a-app"),
        MovedOnA::Lib => Step::commit(&a(), Repo::Lib, "a-lib"),
    });
    steps.push(Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds));
    let mut push =
        publishes_records(Step::rwv(&b(), &["push"])).pushes(Remote::origin(Repo::Proj), "main");
    if b_moves_lib {
        steps.push(Step::commit(&b(), Repo::Lib, "b-lib"));
        push = push.pushes(Remote::origin(Repo::Lib), "main");
    }
    if drops_docs {
        steps.push(Step::rwv(&b(), &["member", "remove", DOCS]).expect(Exit::Succeeds));
    }
    steps.push(Step::rwv(&b(), &["lock", "--commit"]).expect(Exit::Succeeds));
    steps.push(push.expect(Exit::Succeeds));
    steps
}

pub fn push_refuses_a_project_branch_ahead_and_behind_before_publishing_a_member() -> Scenario {
    let mut steps = project_ahead_and_behind(MovedOnA::App, false, true);
    steps.extend([Step::rwv(&a(), &["push"])
        .expect(Exit::refuses(Repo::Proj, State::PushBranchDiverged))
        .claim(Claim::OutputContains("ahead: 1"))
        .claim(Claim::OutputContains("behind: 1"))
        .claim(Claim::OutputContains("rwv pull --merge-upstream"))
        .claim(Claim::LocalStateUnchanged(a()))]);
    scenario(
        "push refuses a project branch ahead and behind before publishing a member",
        steps,
    )
}

pub fn push_dry_run_refuses_a_project_branch_ahead_and_behind() -> Scenario {
    let mut steps = project_ahead_and_behind(MovedOnA::App, false, true);
    steps.extend([
        Step::rwv(&a(), &["push", "--dry-run"])
            .expect(Exit::refuses(Repo::Proj, State::PushBranchDiverged))
            .claim(Claim::OutputContains("rwv pull --merge-upstream"))
            .claim(Claim::LocalStateUnchanged(a())),
        Step::rwv(&a(), &["fetch"]).expect(Exit::Succeeds),
        Step::rwv(&a(), &["push", "--dry-run"])
            .expect(Exit::refuses(Repo::Proj, State::PushBranchDiverged))
            .claim(Claim::LocalStateUnchanged(a())),
    ]);
    scenario(
        "push --dry-run refuses a project branch ahead and behind",
        steps,
    )
}

pub fn pull_merge_upstream_brings_a_project_branch_ahead_and_behind_to_pushable() -> Scenario {
    let mut steps = project_ahead_and_behind(MovedOnA::App, true, true);
    steps.extend(settle_by_the_printed_merge_then_push(true));
    scenario(
        "pull --merge-upstream brings a project branch ahead and behind to pushable",
        steps,
    )
}

fn settle_by_the_printed_merge_then_push(b_moved_lib: bool) -> Vec<Step> {
    let local = LockSite::Checkout(a(), Repo::Proj, "main".to_owned());
    let mut merge = Step::rwv_printed(&a(), "rwv pull --merge-upstream")
        .expect(Exit::Succeeds)
        .claim(Claim::PinContains {
            site: local.clone(),
            member: Repo::App,
            mark: "a-app",
        })
        .claim(Claim::DirGone(Machine::A, "weave/local/remotes/docs"));
    let mut push = publishes_records(Step::rwv(&a(), &["push"]))
        .pushes(Remote::origin(Repo::Proj), "main")
        .pushes(Remote::origin(Repo::App), "main")
        .expect(Exit::Succeeds)
        .claim(Claim::RemoteContains {
            remote: Remote::origin(Repo::App),
            branch: "main",
            mark: "a-app",
        })
        .claim(Claim::PinContains {
            site: LockSite::remote_main(Repo::Proj),
            member: Repo::App,
            mark: "a-app",
        });
    let mut refused_push = Step::rwv(&a(), &["push"])
        .expect(Exit::refuses(Repo::Proj, State::PushBranchDiverged))
        .claim(Claim::OutputContains("ahead: 1"));
    if b_moved_lib {
        refused_push = refused_push.claim(Claim::OutputContains("behind: 2"));
        merge = merge
            .claim(Claim::PinContains {
                site: local,
                member: Repo::Lib,
                mark: "b-lib",
            })
            .claim(Claim::Contains {
                at: a(),
                repo: Repo::Lib,
                rev: "HEAD",
                mark: "b-lib",
            });
        push = push.claim(Claim::PinContains {
            site: LockSite::remote_main(Repo::Proj),
            member: Repo::Lib,
            mark: "b-lib",
        });
    }
    vec![refused_push, merge, push]
}

pub fn pull_merge_upstream_joins_both_sides_moving_the_same_member() -> Scenario {
    let mut steps = project_ahead_and_behind(MovedOnA::Lib, true, false);
    steps.extend([
        Step::rwv(&a(), &["push"]).expect(Exit::refuses(Repo::Proj, State::PushBranchDiverged)),
        Step::rwv_printed(&a(), "rwv pull --merge-upstream")
            .expect(Exit::Succeeds)
            .claim(Claim::Contains {
                at: a(),
                repo: Repo::Lib,
                rev: "HEAD",
                mark: "a-lib",
            })
            .claim(Claim::Contains {
                at: a(),
                repo: Repo::Lib,
                rev: "HEAD",
                mark: "b-lib",
            })
            .claim(Claim::PinContains {
                site: LockSite::Checkout(a(), Repo::Proj, "main".to_owned()),
                member: Repo::Lib,
                mark: "a-lib",
            })
            .claim(Claim::PinContains {
                site: LockSite::Checkout(a(), Repo::Proj, "main".to_owned()),
                member: Repo::Lib,
                mark: "b-lib",
            }),
        publishes_records(Step::rwv(&a(), &["push"]))
            .pushes(Remote::origin(Repo::Proj), "main")
            .pushes(Remote::origin(Repo::Lib), "main")
            .expect(Exit::Succeeds)
            .claim(Claim::RemoteContains {
                remote: Remote::origin(Repo::Lib),
                branch: "main",
                mark: "a-lib",
            })
            .claim(Claim::PinContains {
                site: LockSite::remote_main(Repo::Proj),
                member: Repo::Lib,
                mark: "a-lib",
            })
            .claim(Claim::PinContains {
                site: LockSite::remote_main(Repo::Proj),
                member: Repo::Lib,
                mark: "b-lib",
            }),
    ]);
    scenario(
        "pull --merge-upstream joins both sides moving the same member",
        steps,
    )
}

pub fn pull_merge_upstream_takes_in_a_dropped_member_beside_local_lock_commits() -> Scenario {
    let mut steps = project_ahead_and_behind(MovedOnA::App, false, true);
    steps.extend(settle_by_the_printed_merge_then_push(false));
    steps.push(
        Step::git(&a(), Repo::Proj, &["show", "origin/main:rwv.toml"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputLacks(DOCS)),
    );
    scenario(
        "pull --merge-upstream takes in a dropped member beside local lock commits",
        steps,
    )
}

pub fn push_dry_run_on_an_unreachable_remote_is_unconfirmed_not_would_push() -> Scenario {
    let mut steps = project_ahead_and_behind(MovedOnA::App, true, false);
    steps.extend([
        Step::unreachable(Remote::origin(Repo::Proj)),
        Step::rwv(&a(), &["push", "--dry-run"])
            .expect(Exit::refuses(Repo::Proj, State::PushBranchUnconfirmed))
            .claim(Claim::OutputLacks("would push"))
            .claim(Claim::LocalStateUnchanged(a())),
        Step::rwv(&a(), &["push"])
            .expect(Exit::refuses(Repo::Proj, State::PushBranchUnconfirmed))
            .claim(Claim::LocalStateUnchanged(a())),
        Step::reachable(Remote::origin(Repo::Proj)),
        Step::rwv(&a(), &["push", "--dry-run"])
            .expect(Exit::refuses(Repo::Proj, State::PushBranchDiverged)),
    ]);
    scenario(
        "push --dry-run on an unreachable remote is unconfirmed, not would push",
        steps,
    )
}
