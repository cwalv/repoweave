use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

use crate::invariants::{self, LockSite, Snapshot, Violation};
use crate::sandbox::{Machine, Place, Repo, Sandbox, SecondProject};

/// A verb a scenario needs that rwv does not have. A step that needs one is
/// recorded and the scenario stops there: nothing stands in for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MissingVerb {
    /// Push a workweave's member and project branches somewhere others can
    /// review them, and push again after an amend.
    Publish,
    /// Start a branch across the project and its members, or move a weave
    /// between such branches.
    WeaveBranch,
    /// Give a workweave's branches a chosen name after creation.
    Rename,
    /// Change the branch `rwv.toml` declares for a member.
    Declare,
    /// Materialize a branch someone else pushed, in a workweave of its own.
    Review,
    /// Read or repair what a weave records about where its branch came from
    /// or lands.
    BaseRecord,
}

impl MissingVerb {
    pub fn describe(self) -> &'static str {
        match self {
            MissingVerb::Publish => "publish a workweave's branches for review",
            MissingVerb::WeaveBranch => "create or switch a branch across the whole weave",
            MissingVerb::Rename => "rename a workweave's branches",
            MissingVerb::Declare => "change a member's declared branch",
            MissingVerb::Review => "materialize someone else's pushed branch",
            MissingVerb::BaseRecord => "read or repair a weave branch's origin record",
        }
    }
}

/// What the owner of a remote does to it from outside every machine — a forge
/// merging or deleting a branch, a colleague pushing — performed as plain git
/// in a clone of the forge's own.
#[derive(Debug, Clone)]
pub enum Forge {
    Commit {
        repo: Repo,
        branch: &'static str,
        mark: &'static str,
    },
    Merge {
        repo: Repo,
        branch: &'static str,
        how: MergeHow,
    },
    DeleteBranch {
        repo: Repo,
        branch: &'static str,
    },
}

#[derive(Debug, Clone, Copy)]
pub enum MergeHow {
    Merge,
    Squash,
}

#[derive(Debug, Clone)]
pub enum Action {
    Git {
        at: Place,
        repo: Repo,
        args: Vec<String>,
    },
    /// Write `<mark>.txt` and commit it with message `<mark>`, recording the
    /// commit under that mark for claims to name.
    Commit {
        at: Place,
        repo: Repo,
        mark: &'static str,
    },
    /// Replace the one occurrence of `from` in a tracked file with `to`, as
    /// an operator editing it by hand would.
    HandEdit {
        at: Place,
        repo: Repo,
        file: &'static str,
        from: &'static str,
        to: &'static str,
    },
    Rwv {
        at: Place,
        args: Vec<String>,
    },
    /// An empty weave root on `machine`, filled by `rwv fetch` from `proj`'s
    /// remote.
    NewMachine(Machine),
    /// `rwv fetch` of another project's remote into an existing weave.
    FetchProject(Machine, Repo),
    Forge(Forge),
    Pending(MissingVerb),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    Succeeds,
    Fails,
    Undecided,
}

/// A fact about the state after a step. Marks name commits recorded by
/// [`Action::Commit`] and [`Forge::Commit`].
#[derive(Debug, Clone)]
pub enum Claim {
    Contains {
        at: Place,
        repo: Repo,
        rev: &'static str,
        mark: &'static str,
    },
    RemoteContains {
        repo: Repo,
        branch: &'static str,
        mark: &'static str,
    },
    PinContains {
        site: LockSite,
        member: Repo,
        mark: &'static str,
    },
    WorkweaveGone(Place),
}

/// The outcome a step is expected to have. A step whose outcome the adopted
/// design decides carries [`Expect::undecided`] until that design fills it.
#[derive(Debug, Clone)]
pub struct Expect {
    pub exit: Exit,
    pub claims: Vec<Claim>,
}

impl Expect {
    pub fn undecided() -> Expect {
        Expect {
            exit: Exit::Undecided,
            claims: Vec::new(),
        }
    }

    pub fn succeeds() -> Expect {
        Expect {
            exit: Exit::Succeeds,
            claims: Vec::new(),
        }
    }

    pub fn is_decided(&self) -> bool {
        self.exit != Exit::Undecided || !self.claims.is_empty()
    }
}

#[derive(Debug, Clone)]
pub struct Step {
    pub label: String,
    pub action: Action,
    pub may_rewind: Vec<Repo>,
    pub expect: Expect,
}

impl Step {
    fn new(label: impl Into<String>, action: Action, expect: Expect) -> Step {
        Step {
            label: label.into(),
            action,
            may_rewind: Vec::new(),
            expect,
        }
    }

    pub fn git(at: &Place, repo: Repo, args: &[&str]) -> Step {
        Step::new(
            format!("git -C {} {}", repo.name(), args.join(" ")),
            Action::Git {
                at: at.clone(),
                repo,
                args: args.iter().map(|a| (*a).to_owned()).collect(),
            },
            Expect::succeeds(),
        )
    }

    pub fn commit(at: &Place, repo: Repo, mark: &'static str) -> Step {
        Step::new(
            format!("commit {mark} in {}", repo.name()),
            Action::Commit {
                at: at.clone(),
                repo,
                mark,
            },
            Expect::succeeds(),
        )
    }

    pub fn hand_edit(
        at: &Place,
        repo: Repo,
        file: &'static str,
        from: &'static str,
        to: &'static str,
    ) -> Step {
        Step::new(
            format!("hand-edit {} {file}: {from:?} -> {to:?}", repo.name()),
            Action::HandEdit {
                at: at.clone(),
                repo,
                file,
                from,
                to,
            },
            Expect::succeeds(),
        )
    }

    pub fn rwv(at: &Place, args: &[&str]) -> Step {
        Step::new(
            format!("rwv {}", args.join(" ")),
            Action::Rwv {
                at: at.clone(),
                args: args.iter().map(|a| (*a).to_owned()).collect(),
            },
            Expect::undecided(),
        )
    }

    pub fn new_machine(machine: Machine) -> Step {
        Step::new(
            format!("fresh clone on {machine:?}"),
            Action::NewMachine(machine),
            Expect::succeeds(),
        )
    }

    pub fn fetch_project(machine: Machine, project: Repo) -> Step {
        Step::new(
            format!("rwv fetch {} on {machine:?}", project.name()),
            Action::FetchProject(machine, project),
            Expect::succeeds(),
        )
    }

    pub fn forge(forge: Forge) -> Step {
        Step::new(
            format!("forge: {forge:?}"),
            Action::Forge(forge),
            Expect::succeeds(),
        )
    }

    pub fn pending(label: &str, verb: MissingVerb) -> Step {
        Step::new(label, Action::Pending(verb), Expect::undecided())
    }

    pub fn expect(mut self, exit: Exit) -> Step {
        self.expect.exit = exit;
        self
    }

    pub fn claim(mut self, claim: Claim) -> Step {
        self.expect.claims.push(claim);
        self
    }

    pub fn may_rewind(mut self, members: &[Repo]) -> Step {
        self.may_rewind.extend_from_slice(members);
        self
    }
}

pub struct Scenario {
    pub name: &'static str,
    pub second_project: Option<SecondProject>,
    pub steps: Vec<Step>,
}

#[derive(Debug, Clone)]
pub struct StepRecord {
    pub label: String,
    pub succeeded: bool,
    pub decided: bool,
    pub output: String,
}

#[derive(Debug, Default)]
pub struct Report {
    pub name: &'static str,
    pub ran: Vec<StepRecord>,
    pub pending: Option<(String, MissingVerb)>,
    pub violations: Vec<(String, Violation)>,
    pub unmet: Vec<(String, String)>,
    pub marks: BTreeMap<&'static str, String>,
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "scenario {}", self.name)?;
        for record in &self.ran {
            let verdict = if record.succeeded { "ok" } else { "FAILED" };
            writeln!(f, "  [{verdict}] {}", record.label)?;
            if !record.succeeded {
                for line in record.output.lines() {
                    writeln!(f, "        {line}")?;
                }
            }
        }
        if let Some((label, verb)) = &self.pending {
            writeln!(
                f,
                "  [pending] {label}: needs a verb to {}",
                verb.describe()
            )?;
        }
        for (label, violation) in &self.violations {
            writeln!(f, "  VIOLATION after `{label}`: {violation}")?;
        }
        for (label, why) in &self.unmet {
            writeln!(f, "  UNMET after `{label}`: {why}")?;
        }
        Ok(())
    }
}

impl Report {
    pub fn assert_green(&self) {
        assert!(
            self.violations.is_empty() && self.unmet.is_empty() && self.pending.is_none(),
            "expected a green run:\n{self}"
        );
        assert!(!self.ran.is_empty(), "a green run ran no steps:\n{self}");
    }

    /// [`Report::assert_green`], and every step carried a decided outcome.
    pub fn assert_fully_decided_green(&self) {
        self.assert_green();
        let undecided: Vec<&str> = self
            .ran
            .iter()
            .filter(|r| !r.decided)
            .map(|r| r.label.as_str())
            .collect();
        assert!(
            undecided.is_empty(),
            "undecided steps {undecided:?}:\n{self}"
        );
    }

    pub fn assert_pending_on(&self, verb: MissingVerb) {
        assert!(
            self.violations.is_empty() && self.unmet.is_empty(),
            "the steps before the pending one must hold:\n{self}"
        );
        assert_eq!(
            self.pending.as_ref().map(|(_, v)| *v),
            Some(verb),
            "expected to stop on a missing verb:\n{self}"
        );
        crate::common::report_skip(&format!(
            "scenario {} stops at `{}`: no verb to {}",
            self.name,
            self.pending.as_ref().unwrap().0,
            verb.describe()
        ));
    }
}

struct Run<'a> {
    sandbox: &'a Sandbox,
    marks: BTreeMap<&'static str, String>,
}

pub fn run(scenario: Scenario) -> Report {
    let sandbox = Sandbox::new(scenario.second_project);
    run_in(&sandbox, scenario)
}

pub fn run_in(sandbox: &Sandbox, scenario: Scenario) -> Report {
    let mut report = Report {
        name: scenario.name,
        ..Report::default()
    };
    let mut run = Run {
        sandbox,
        marks: BTreeMap::new(),
    };
    let steps = std::iter::once(Step::new_machine(Machine::A)).chain(scenario.steps);
    let mut before = invariants::snapshot(sandbox);
    for step in steps {
        if let Action::Pending(verb) = step.action {
            report.pending = Some((step.label, verb));
            break;
        }
        let (succeeded, output) = run.perform(&step.action);
        let after = invariants::snapshot(sandbox);
        for violation in invariants::check(sandbox, &before, &after, &step.may_rewind) {
            report.violations.push((step.label.clone(), violation));
        }
        match (step.expect.exit, succeeded) {
            (Exit::Succeeds, false) => report
                .unmet
                .push((step.label.clone(), format!("expected success:\n{output}"))),
            (Exit::Fails, true) => report.unmet.push((
                step.label.clone(),
                "expected a refusal; it succeeded".to_owned(),
            )),
            _ => {}
        }
        for claim in &step.expect.claims {
            if let Err(why) = run.holds(claim, &after) {
                report.unmet.push((step.label.clone(), why));
            }
        }
        report.ran.push(StepRecord {
            label: step.label,
            succeeded,
            decided: step.expect.is_decided(),
            output,
        });
        before = after;
    }
    report.marks = run.marks;
    report
}

fn rendered(out: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

impl Run<'_> {
    fn perform(&mut self, action: &Action) -> (bool, String) {
        let sb = self.sandbox;
        match action {
            Action::Git { at, repo, args } => {
                let args: Vec<&str> = args.iter().map(String::as_str).collect();
                let out = sb.git(&sb.checkout(at, *repo), &args);
                (out.status.success(), rendered(&out))
            }
            Action::Commit { at, repo, mark } => self.commit_in(sb.checkout(at, *repo), mark),
            Action::HandEdit {
                at,
                repo,
                file,
                from,
                to,
            } => {
                let path = sb.checkout(at, *repo).join(file);
                let text = std::fs::read_to_string(&path).unwrap();
                if text.matches(from).count() != 1 {
                    return (
                        false,
                        format!("{from:?} is not in {} exactly once", path.display()),
                    );
                }
                std::fs::write(&path, text.replacen(from, to, 1)).unwrap();
                (true, String::new())
            }
            Action::Rwv { at, args } => {
                let args: Vec<&str> = args.iter().map(String::as_str).collect();
                let out = sb.rwv(&sb.place_root(at), &args);
                (out.status.success(), rendered(&out))
            }
            Action::NewMachine(machine) => {
                let root = sb.prepare_machine(*machine);
                let fetch = sb.fetch_command(Repo::Proj);
                let args: Vec<&str> = fetch.iter().map(String::as_str).collect();
                let out = sb.rwv(&root, &args);
                (out.status.success(), rendered(&out))
            }
            Action::FetchProject(machine, project) => {
                let fetch = sb.fetch_command(*project);
                let args: Vec<&str> = fetch.iter().map(String::as_str).collect();
                let out = sb.rwv(&sb.weave_root(*machine), &args);
                (out.status.success(), rendered(&out))
            }
            Action::Forge(forge) => self.forge(forge),
            Action::Pending(_) => {
                unreachable!("a pending step stops the run before it is performed")
            }
        }
    }

    fn commit_in(&mut self, dir: PathBuf, mark: &'static str) -> (bool, String) {
        let sb = self.sandbox;
        assert!(
            !self.marks.contains_key(mark),
            "mark {mark} is recorded twice in one scenario"
        );
        std::fs::write(dir.join(format!("{mark}.txt")), format!("{mark}\n")).unwrap();
        let add = sb.git(&dir, &["add", &format!("{mark}.txt")]);
        let commit = sb.git(&dir, &["commit", "-m", mark]);
        if !add.status.success() || !commit.status.success() {
            return (false, format!("{}{}", rendered(&add), rendered(&commit)));
        }
        let sha = sb.git_ok(&dir, &["rev-parse", "HEAD"]);
        self.marks.insert(mark, sha);
        (true, rendered(&commit))
    }

    fn forge_clone(&self, repo: Repo) -> PathBuf {
        let sb = self.sandbox;
        let dir = sb.root.join("forge").join(repo.name());
        if dir.is_dir() {
            sb.git_ok(&dir, &["fetch", "--prune", "origin"]);
        } else {
            std::fs::create_dir_all(dir.parent().unwrap()).unwrap();
            sb.git_ok(
                dir.parent().unwrap(),
                &["clone", &sb.remote_url(repo), dir.to_str().unwrap()],
            );
        }
        dir
    }

    fn forge(&mut self, forge: &Forge) -> (bool, String) {
        let sb = self.sandbox;
        match forge {
            Forge::Commit { repo, branch, mark } => {
                let dir = self.forge_clone(*repo);
                let tracking = format!("refs/remotes/origin/{branch}");
                let start = if sb
                    .git(&dir, &["rev-parse", "--verify", &tracking])
                    .status
                    .success()
                {
                    tracking
                } else {
                    "refs/remotes/origin/main".to_owned()
                };
                sb.git_ok(&dir, &["switch", "-C", branch, &start]);
                let result = self.commit_in(dir.clone(), mark);
                let out = sb.git(&dir, &["push", "origin", branch]);
                (result.0 && out.status.success(), result.1 + &rendered(&out))
            }
            Forge::Merge { repo, branch, how } => {
                let dir = self.forge_clone(*repo);
                sb.git_ok(&dir, &["switch", "-C", "main", "origin/main"]);
                let source = format!("origin/{branch}");
                let merged = match how {
                    MergeHow::Merge => sb.git(&dir, &["merge", "--no-ff", "--no-edit", &source]),
                    MergeHow::Squash => {
                        sb.git_ok(&dir, &["merge", "--squash", &source]);
                        sb.git(&dir, &["commit", "-m", &format!("squash {branch}")])
                    }
                };
                let out = sb.git(&dir, &["push", "origin", "main"]);
                (
                    merged.status.success() && out.status.success(),
                    rendered(&merged) + &rendered(&out),
                )
            }
            Forge::DeleteBranch { repo, branch } => {
                let out = sb.git(&sb.remote(*repo), &["branch", "-D", branch]);
                (out.status.success(), rendered(&out))
            }
        }
    }

    fn mark(&self, mark: &str) -> Result<&String, String> {
        self.marks
            .get(mark)
            .ok_or_else(|| format!("no commit is recorded under mark {mark}"))
    }

    fn contains(&self, dir: &std::path::Path, rev: &str, mark: &str) -> Result<(), String> {
        let sha = self.mark(mark)?;
        let ok = self
            .sandbox
            .git(dir, &["merge-base", "--is-ancestor", sha, rev])
            .status
            .success();
        if ok {
            Ok(())
        } else {
            Err(format!(
                "{} at {rev} does not contain {mark} ({sha})",
                dir.display()
            ))
        }
    }

    fn holds(&self, claim: &Claim, after: &Snapshot) -> Result<(), String> {
        let sb = self.sandbox;
        match claim {
            Claim::Contains {
                at,
                repo,
                rev,
                mark,
            } => self.contains(&sb.checkout(at, *repo), rev, mark),
            Claim::RemoteContains { repo, branch, mark } => {
                self.contains(&sb.remote(*repo), &format!("refs/heads/{branch}"), mark)
            }
            Claim::PinContains { site, member, mark } => {
                let pin = after
                    .pins
                    .get(site)
                    .and_then(|pins| pins.get(member))
                    .ok_or_else(|| format!("{site:?} has no pin for {}", member.name()))?;
                let store = invariants::store_for(sb, site, *member)
                    .ok_or_else(|| format!("no repository backs {} at {site:?}", member.name()))?;
                self.contains(&store, pin, mark)
            }
            Claim::WorkweaveGone(place) => {
                if sb.place_root(place).exists() {
                    Err(format!("{place:?} still exists"))
                } else {
                    Ok(())
                }
            }
        }
    }
}
