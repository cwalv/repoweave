# Journey tasks

> **Implementation, not interface.** Nothing in this document is a stable
> surface: the structures described here may change without notice, and
> operating on them directly — with shell tools, file edits, or git commands
> against rwv-managed state — is not supported. Operations on or between
> workweaves go through rwv verbs. If you need an operation no verb provides,
> that is a UX gap: file it rather than working around it at the file level.

The tasks an operator does with the branching surface, written as outcomes. Each
task is one end-to-end journey test in the scenario binary
(`tests/scenario/scenario_harness_test/journeys.rs`), and the same text is the prompt
set of a usability probe: an agent that knows nothing of rwv is given the **Task**
paragraph and the start state, and its end state is judged by the **Check**. So a
task names no verb, flag, file format or ref name that the reader would have to know
rwv for. A Check may name anything; it is the judge's, not the agent's.

## The shared world

Every task starts from this world unless it says otherwise.

- A team keeps one project, `web-app`, in a hosted repository, and three code
  repositories it covers: `protocol`, `server` and `web`. All four sit on the
  hosting service under `chatly`, each with one commit on its default branch
  `main`. The project's `main` records all three code repositories as followed
  on their own default branch.
- **Machine A** is the operator's. The project is set up in `~/work`, at the state the
  hosting service holds, nothing edited.
- **Machine B** is a second computer of the operator's, with nothing on it.
- **A colleague** works on a third computer. What the colleague publishes is on the
  hosting service; the operator reaches it only through the project's own tooling.
- Nothing is in progress anywhere: no unfinished operation, no spare workspace.

A **clean end** is the check every task includes: the project reports no drift, no
member is off the commit the project records for it, and no unfinished operation is
left, in every workspace the task touched.

## Building a start state outside the tests

Each journey's start state is its first `start` path steps
(`Journey::start_scenario` in `tests/common/scenario_steps.rs`). One command
builds it in a sandbox left on disk, with `HOME` inside the sandbox and every
remote a local bare repository, and prints where everything is:

```sh
RWV_JOURNEY=j03 cargo test --release --features test-pause --test scenario -- \
  build_a_journey_start_state --ignored --nocapture
```

It prints `SANDBOX`, `HOME`, `MACHINE_A_WEAVE` (the operator's `~/work`),
`MACHINE_B_WEAVE` and `GOLDEN_RWV_COMMANDS`, the number of rwv commands the documented
path runs after the start state. A probe runs with `HOME` set to the printed one, the
`rwv` under test on `PATH`, and the weave as its working directory.

The build that makes the start state is the test build, `--features test-pause`, at
`target/release/rwv`: J18 and J19 hold a landing at a pause point and kill it, which a
plain build cannot do. The binary the agent works with is the plain build
(`cargo build --release` without the feature, copied elsewhere before the test build
overwrites it). The sandbox's names differ from the task text's: `server` is `app`,
`protocol` is `lib`, the third code repository is `docs` (`web` in a task that has one,
`docs` in one that adds one), the project is `proj`, and the hosting service is the
bare repositories under `SANDBOX/remotes/`.

When the agent is done, the end check runs against the same sandbox:

```sh
RWV_JOURNEY=j03 SANDBOX=<root> cargo test --release --features test-pause --test scenario -- \
  check_a_journey_end_state --ignored --nocapture
```

It is the task's **Check**: refs, pins, records, published state, no operation in
progress, doctor clean. A claim about a commit finds it by its subject line, which must be the prescribed
message or begin with it followed by a space or a colon (a body or trailers on later
lines are ignored); so a task that
wants a commit the agent makes identified says its message (J01, J08), and one that
wants content checked says the file and its text (J14 to J16). A test reads each journey's `holds` list on
its built start state, so a start state that lacks what its task text presupposes
fails in the suite, and runs every journey's end check mark-free after its own path.

## Tasks

### J01 Ship a change that spans two repositories

**Start:** the shared world, machine A.
**Task:** The welcome screen needs a new field. Add a `welcome.txt` containing
`welcome: string` to `protocol` and a `welcome.txt` containing `serve the welcome` to
`server`, as one change (the commits' messages are `welcome-lib` and `welcome-app`), without touching `~/work` until the change is ready. When it
is ready, have it in `main` in `~/work` and published for the team.
**Check:** the hosting service's `main` of `protocol`, `server` and the project each
holds the change; the project's lock on the hosting service pins each of the two at
the commit its `main` holds and `web` where it was; `~/work` is clean; no spare
workspace and no branch of the change remains on machine A; a clean end.

### J02 Pick up a colleague's landing and carry on

**Start:** the shared world; on machine A a change to `server` is in progress in a
spare workspace and committed there. Meanwhile the colleague has
published a change to `protocol` into `main`, and in the same landing added a
fourth repository, `docs`, to the project.
**Task:** Get the colleague's changes into your own copy of the project, then finish
your in-progress change so that `main` holds yours on top of theirs, and publish. Leave
no spare workspace behind.
**Check:** hosting `main` of `protocol` holds the colleague's commit and `server`'s the
operator's; the hosting project's description lists `docs` and its
lock pins every repository at its `main` tip; `~/work` has a `docs` checkout at its
pin; the spare workspace is gone; a clean end.

### J03 A landing stops on a conflict; finish it

**Start:** the shared world; two spare workspaces on machine A, each with one commit
to `server` that writes a different line to the same line of `README`. The first
has been brought into `main`.
**Task:** Bring the second one into `main` as well, keeping the line of the second.
Leave no spare workspace behind.
**Check:** `main`'s `README` in `server` holds the second workspace's line; the
history of `server` holds both commits; the second workspace is gone; no unfinished
operation anywhere; a clean end.

### J04 A landing is abandoned after it stops

**Start:** as J03.
**Task:** Try to bring the second one into `main`; when that cannot be done without
choosing between the two lines, give up on it and leave everything as it was before
you started, with the second workspace still there and still holding its commit.
**Check:** `main` in every repository is where the first landing left it; the second
workspace holds its commit and is clean; no unfinished operation; nothing the
operator wrote during the attempt is lost (a backup holds it); a clean end.

### J05 A landing loses a race; recover

**Start:** the shared world; on machine A a change to `server` is in a spare
workspace and committed. The colleague has published a different change to `server`'s
`main` since machine A last looked.
**Task:** Put your change into `main` and publish it.
**Check:** hosting `main` of `server` holds the colleague's commit and then the
operator's; the project's lock on the hosting service pins `server` at that tip;
`~/work` is clean; a clean end.

### J06 Rename a feature that is already published

**Start:** the shared world; on machine A a change to `server` has been made on a new
branch of the project called `try-1`, committed, and published for review.
**Task:** The branch should have been called `welcome-field`. Give it that name
everywhere it exists, and publish it so the old name is not left on the hosting
service.
**Check:** the hosting service has the branch `welcome-field` in the project and in
`server`, holding the commit, and no `try-1` in either; machine A has no `try-1`
branch in any repository; the project's record of how `welcome-field` is used is the
one `try-1` had; a clean end.

### J07 Add a repository, point it at another branch, pin a dependency back

**Start:** the shared world; a fourth repository, `docs`, exists on the hosting
service with a `main` and a branch `v2` of two commits, and is not part of the project;
`server`'s `main` has two commits, and the project records the newer.
**Task:** Make `docs` part of the project, following its `v2` branch but at the commit
before `v2`'s tip. Make the project use the older of `server`'s two commits instead of
its newest. Do both as one change to the project, put it in `main`, and publish.
**Check:** the hosting project's `main` lists `docs` with branch `v2`; its lock pins
`docs` at `v2`'s older commit and `server` at its older commit; no spare workspace
remains; a clean end.

### J08 A second feature stacked on the first

**Start:** the shared world; on machine A a feature `a` is under way in a spare
workspace, with a commit in `server`.
**Task:** Start a second feature `b`, which needs `a`'s commit, so that it begins from
the first feature rather than from `main`. Make a commit in `protocol` for `b`, with the message `b-lib`. Bring
`a` into `main`, then `b`, and publish. Leave no spare workspace and no branch of `a`
or `b` behind.
**Check:** hosting `main` of `server` holds `a`'s commit, hosting `main` of `protocol`
holds `b`'s commit; the project's history shows `a`'s change before `b`'s; both
workspaces are gone; no branch of `a` or `b` remains on machine A; a clean end.

### J09 Throw away a feature whose work never made it into `main`

**Start:** the shared world; on machine A a spare workspace holds an experiment: a commit
to `server` that was never brought into `main`.
**Task:** The experiment failed. Remove the spare workspace and the branch it made,
while being able to get the commit back afterwards.
**Check:** no spare workspace; no branch for the experiment in any repository; the
commit is recoverable from the kept backups; `main` is unchanged everywhere; a clean
end.

### J10 Set up the project on a second computer

**Start:** the shared world, after J01 has been done on machine A and published.
**Task:** On machine B, set the project up so that it is in the same state as the
published one, and show that it is the same as machine A's.
**Check:** machine B's lock file is byte-identical to machine A's and to the hosting
service's; every member of machine B sits at the commit the lock records; `main` is
the project's active branch; a clean end.

### J11 Turn a repository that is not yet a project into one

**Start:** the shared world, except that the hosting service also holds `docs` as an
ordinary repository with a `README` and no project description, and machine B has the
team's project set up.
**Task:** On machine B, take `docs` as a second project, named `docs`, with `server`
and `protocol` as its code repositories, and publish what you set up so machine A can
take it from there.
**Check:** the hosting `docs` has a description listing `server` and `protocol` and a
lock pinning both; machine B ends clean.

### J12 A hand-edited description converges

**Start:** the shared world; on machine A the project's description file has been
edited by hand to name a different branch for `server` (`v1`, which exists), and a
second hand edit removes `web`.
**Task:** Make `~/work` agree with what the file now says, without losing anything
that was in the removed `web` checkout that is not already on the hosting service.
**Check:** `~/work` has `server` on `v1` and no `web` checkout; no leftover of `web`
that was not published unless it was named as discarded; the project's description
and lock agree with the checkouts; a clean end.

### J13 Cut a release line

**Start:** the shared world; `server` and `protocol` each have a release commit on
`main` (in `server`, `welcome.txt` reads `helo, world`), and the project records them.
**Task:** Start a line of releases at that state for `server` and `protocol` so fixes
to the released version can go in without taking `main`'s newer work; the third
repository keeps following `main`. The team calls the line `stable`, and its branches
`v1` for `server` and `protov1` for `protocol`. Publish the line.
**Check:** the hosting service has a project branch `stable` whose description names
`v1` for `server` and `protov1` for `protocol`, each at the release commit, and the
third repository on `main`; the record of `stable` is published as a line that follows those branches; a
clean end.

### J14 Land a hotfix on a release line

**Start:** as J13 after the line exists and is published, and `main` has moved on by
one commit in `server`.
**Task:** The released `welcome.txt` in `server` reads `helo, world`; it should read
`hello, world`. Fix it for the released version only, put the fix in the line and
publish it, and leave `main` alone. Leave no spare workspace behind.
**Check:** hosting `v1` of `server` has `welcome.txt` reading `hello`, hosting `main`
of `server` still reads `helo`; hosting `stable` pins `server` at the fixed commit; no spare workspace remains;
the project's `main` is where it was; a clean end.

### J15 Port a fix from one line to the other

**Start:** as J14 after the hotfix is published.
**Task:** The same fix is needed on `main`: its `welcome.txt` reads `helo, world`. Take
the fix over from the release line without dragging along anything else of the line,
and publish. Leave no spare workspace behind.
**Check:** hosting `main` of `server` has `welcome.txt` reading `hello` and keeps its
own newer commit; the project's `main` lock pins `server` there; no spare workspace
remains; a clean end.

### J16 Retire a release line

**Start:** as J14; the line has shipped its last fix.
**Task:** Remove the line `stable` from this machine and from the team's hosting
service, keeping the ability to get its last state back on this machine.
**Check:** no `stable` branch in the project on machine A or the hosting service; the
line's tip is in the kept backups; `main` and its members are unchanged; the
branches `v1` and `protov1` of the members remain; a clean end.

### J17 A landing stops because a retired workspace holds unpublished work

**Start:** the shared world; `server` has a nested repository at `sub`, a clone of
`protocol`. A spare workspace on machine A has a commit in `server` that records a
new commit inside `sub`, which is on no other computer.
**Task:** Bring the change into `main` and remove the spare workspace. When it will
not be removed without losing the nested commit, publish the nested commit and finish
the removal.
**Check:** `main` in `server` holds the change; the nested repository's hosting
service has the nested commit; the spare workspace is gone; a clean end.

### J18 Recover after the machine dies mid-landing

**Start:** the shared world; a spare workspace on machine A with commits to `server`
and `protocol`. The process bringing it into `main` is killed part-way,
then the machine is back.
**Task:** Find out what state `~/work` and the workspace are in and finish bringing
the change into `main`.
**Check:** `main` of `server` and `protocol` hold the change once each; the project's
lock pins both; no unfinished operation anywhere; the workspace is gone; a clean end.

### J19 Kill mid-landing, then give up

**Start:** as J18.
**Task:** Find out what state things are in and put them back as they were before the
landing began.
**Check:** `main` of every repository is where it was before; the workspace still
holds its commits; no unfinished operation; a clean end.

### J20 A repository with a nested repository: take in a colleague's change to it

**Start:** the shared world, except that `server` holds a nested repository at
`sub`, a clone of `protocol` that `server` records at an older commit of its `main`. On machine A a spare workspace has a commit in `web`.
The colleague has published a change to `server` that moves the nested repository to its
second commit.
**Task:** Take in the colleague's change, keeping your own work, and bring yours into
`main`; the nested repository must end up where the colleague put it. Publish.
**Check:** `server`'s nested repository is at the second commit in `~/work` and in
the hosting service's record; hosting `main` of `web` holds the operator's commit;
the spare workspace is gone; a clean end.

### J21 Turn on an editor workspace file for the project

**Start:** the shared world; the project does not generate an editor workspace file
(its description has the editor-workspace setting off).
**Task:** Make the project generate the editor workspace file for everyone who takes
the project, and publish that.
**Check:** the hosting project's description has the editor-workspace setting on; the
file the setting authors is committed with it; a clean end.

### J22 Land a feature started in place from the primary

**Start:** the shared world, machine A, nothing in progress.
**Task:** Work on a new feature right in `~/work`, without a second workspace: commit
one change in `server` and one in `protocol` (the commits' messages are `pay-app` and
`pay-lib`), bring it into `main`, leave no trace of
the feature's branches on this machine, and publish.
**Check:** hosting `main` of `server` and `protocol` hold the changes and the project's
lock pins `server` at its commit; no branch of the feature remains in the project or
any member on machine A; a clean end.
