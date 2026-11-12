# Refusals

When `rwv` declines to act on purpose, the last lines it prints are route lines,
one per condition it refused on:

```
rwv explain <token>
```

A refusal comes in one of two shapes, and both end that way.

A refusal that judged repos prints one line for each repo and the axis it was
judged on, `rwv <verb>: <repo>: <kind> <state>`, with what the state lists
beneath it: files, paths, what a remote answered. Then `choices:` lists what
each state can run, the commands spelled out, and names the directory to run a
choice in where that is not the one the verb ran in. Then one route line per
distinct kind. The kinds are the first entries below; each lists its states and
every choice a state prints. Under `--json`, the verb's envelope carries the
same repos in its `refused` array, each with its `path`, `axis`, `state`,
`subject`, `listed`, `facts` and `choices`; a verb that refused before printing
anything prints `{"$schema": "<url>", "refused": [...]}` instead.

A refusal that names a condition prints `Error:` and its reason, then one route
line naming the condition's token: a stable kebab-case name for the
*condition* — not for the message, which may be reworded, and not for the site,
which may move. Every such message is written to stand on its own: it names
what stopped and what unblocks. What an entry adds is the part a message cannot
carry — why the rule exists, which exit applies under which circumstances, and
where the rule sits in the machine.

This page has one entry per kind and per token, under that exact name, so a
refusal you met in the terminal can be looked up without translation. A kind or
token that `rwv doctor` also reports has its one entry on
[Doctor findings](./doctor-findings.md) instead.

## Reading an entry

Run the command the refusal printed:

```bash
rwv explain edits
```

`rwv explain` serves the entry below verbatim. A kind or token whose condition
`rwv doctor` also reports keeps its single entry on
[Doctor findings](./doctor-findings.md) — one condition has one entry
wherever that entry lives, and `rwv explain` finds it either way.

## What does and does not carry a token

A route line means **rwv could have acted and declined**: a precondition it will
not cross, or an input it will not accept. Absence is informative. An error
with no route line is a passthrough (a VCS or filesystem failure), an internal
invariant, or an argument the parser rejected before any verb ran — those
print `error:` and exit 2, where a refusal exits 1.

## Kinds

A kind that `rwv doctor` also reports has its one entry, states and choices
included, on [Doctor findings](./doctor-findings.md):

- [`unparseable-project`](./doctor-findings.md#unparseable-project)
- [`pin`](./doctor-findings.md#pin)
- [`history-disagreement`](./doctor-findings.md#history-disagreement)
- [`alignment`](./doctor-findings.md#alignment)
- [`legacy-workweave-index`](./doctor-findings.md#legacy-workweave-index)
- [`missing-lock-merge-driver`](./doctor-findings.md#missing-lock-merge-driver)

The others follow.

### `edits`

**Condition.** A checkout the verb must move, rewrite, record or remove holds uncommitted changes, or is stopped in the middle of a VCS operation.

**Why.** Moving or removing a checkout over uncommitted changes would lose them or tangle them into a half-applied result, and recording its tip would describe something other than what is on disk. An operation in progress holds a partial result that only the tool which started it can finish. A project that shares a checkout with the project the weave root presents runs its install hooks against it on activation, and would build against edits made under the other project.

**States.** A verb that judges a repo in one of these prints `rwv <verb>: <repo>: edits <state>`, with what the state lists beneath it, and refuses unless its row below says it completes.

| State | Means | Refused by |
|---|---|---|
| `dirty` | the checkout has uncommitted changes, listed by file | `rwv sync` and `rwv sync-to`, for a repo they would rebase or fast-forward (tracked changes only); `rwv lock` without `--dirty`, for a member (tracked changes only); `rwv pull`, for project-repo files its fast-forward would overwrite; `rwv member set-role <repo> reference`; `rwv member remove --delete` and replay's prune of a dropped member; `rwv workweave delete` and `rwv sync-to --retire`, for every checkout in the workweave; `rwv activate`, for a member the project shares with the project the weave root presents; `rwv update` and `rwv pull`, for such a member that is not aligned for a project the weave root does not present |
| `mid-operation` | a rebase, merge, cherry-pick or bisect is in progress, named | `rwv sync` and `rwv sync-to`, for a repo they would rebase or fast-forward; `rwv activate`, `rwv update` and `rwv pull`, for a shared member as `dirty` is |
| `rwv.toml or rwv.lock uncommitted` | the project repo's only uncommitted changes are to `rwv.toml` or `rwv.lock`, or either is not yet tracked | `rwv push`; `rwv switch`, and `rwv branch create` for an `rwv.lock` where the branch starts at another commit than the checked-out one. In a detached project repo the choice is `rwv branch create`, which carries both files |
| `lock unresolved` | the project repo's `rwv.lock` is unmerged: a merge, rebase or copy the lock merge driver could not settle left it conflicted with both pins marked, or a driver that could not run left one side in the working file with the conflict recorded only by the VCS. Resolve it to the pins you mean and stage it | every verb resolved for a project, until it is resolved; `rwv doctor` and `rwv abort` run |

**Choices.** Each state's choices print under `choices:` in this order, with a directory named where a choice does not run where the verb did. A state with no row here prints none.

| State | Choice | Spelled | Where | Keeps work because |
|---|---|---|---|---|
| `dirty` | C13: commit your edits, or set them aside | VCS action (git: `git -C <checkout> commit`, `git -C <checkout> stash push`) | the checkout | both keep edits |
| `dirty`, for `rwv workweave delete` | C13: commit your edits, or set them aside | `rwv workweave delete <ww> --discard-uncommitted` | the primary | both keep edits |
| `dirty`, for `rwv lock` | record the committed HEAD, leaving these uncommitted edits out of the lock | `rwv lock --dirty` | here | the edits stay in the checkout, uncommitted |
| `dirty`, for `rwv update --remote` | record the committed HEAD, leaving these uncommitted edits out of the lock | `rwv update --remote --dirty` | here | the edits stay in the checkout, uncommitted |
| `mid-operation` | C32: finish or abandon the operation in progress first | VCS action (git: `git -C <checkout> status`) | the checkout | rwv moves nothing until it ends |
| `rwv.toml or rwv.lock uncommitted` | C5: record it: the lock, and `rwv.toml` as it stands | `rwv lock --commit` | a workspace with the branch checked out | records only |
| `rwv.toml or rwv.lock uncommitted`, for `rwv switch` | C30: return the project repo to a branch, or start one carrying the edits | `rwv branch create <W> --target <T>`; `rwv branch create <W> --declared` | here | moves only what alignment moves |
| `rwv.toml or rwv.lock uncommitted`, for `rwv branch create` | C30: return the project repo to a branch, or start one carrying the edits | `rwv branch create <W> --target <T>`; `rwv branch create <W> --declared` | here | moves only what alignment moves |
| `lock unresolved` | C32: finish or abandon the operation in progress first | VCS action (git: `git -C <checkout> status`) | the checkout | rwv moves nothing until it ends |

**Where this sits.** [Destructive operations](../explanation/destructive-operations.md); [Sync semantics](../explanation/joints/sync-semantics.md).

### `checkout`

**Condition.** Another checkout derives from the clone a verb would delete: a workweave's, or a worktree added with the VCS directly.

**Why.** Deleting a clone's store takes every ref and object with it at once, for every checkout derived from it, so a live one would be left pointing at nothing.

**States.** A verb that judges a repo in one of these prints `rwv <verb>: <repo>: checkout <state>`, with what the state lists beneath it, and refuses unless its row below says it completes.

| State | Means | Refused by |
|---|---|---|
| `in another workspace` | a workweave's checkout derives from the clone, named; or the branch is checked out there | `rwv member remove --delete` in the primary, and replay's prune of a dropped member there; `rwv branch delete`, `rwv switch` and `rwv workweave create --branch`, for a project branch checked out in another workspace |
| `in a raw worktree` | a worktree no workweave records derives from the clone, named by path; or the branch is checked out there | `rwv member remove --delete` in the primary, and replay's prune of a dropped member there; `rwv branch delete`, for a branch checked out in a raw worktree |

**Choices.** Each state's choices print under `choices:` in this order, with a directory named where a choice does not run where the verb did. A state with no row here prints none.

| State | Choice | Spelled | Where | Keeps work because |
|---|---|---|---|---|
| `in another workspace` | C16: free the other checkout | `rwv switch <other>` | the other checkout | removal refuses over edits |
| `in another workspace` | C16: free the other checkout | `rwv workweave delete <ww> --project <project>` | the primary | removal refuses over edits |
| `in another workspace`, for `rwv switch` | C16: free the other checkout | `rwv workweave create <new> --detach <W>` | here | removal refuses over edits |
| `in another workspace`, for `rwv workweave create` | C16: free the other checkout | `rwv workweave create <new> --detach <W>` | here | removal refuses over edits |
| `in a raw worktree` | C16: free the other checkout | VCS action (git: `git -C <checkout> worktree remove <worktree>`) | the checkout | removal refuses over edits |

**Where this sits.** [Clone topology](../explanation/joints/clone-topology.md).

### `push-branch`

**Condition.** A branch and the remote branch it takes in from or publishes to have each moved, or the remote has none to give.

**Why.** Taking in a branch that has commits of its own would have to discard them or tangle the two histories, and only a fast-forward does neither, so a verb that takes in stops where the histories have diverged instead of choosing for you.

**States.** A verb that judges a repo in one of these prints `rwv <verb>: <repo>: push-branch <state>`, with what the state lists beneath it, and refuses unless its row below says it completes.

| State | Means | Refused by |
|---|---|---|
| `diverged` | the branch and its upstream each hold commits the other lacks, counted on the line | `rwv pull`, for the project branch; `rwv update --remote`, for a member; `rwv branch create` and `rwv workweave create`, for a branch that would start at the tip of a branch that has diverged from its upstream |
| `behind` | the branch is an ancestor of its push branch, as last fetched: the remote holds commits the branch lacks | `rwv push` skips it and reports it; `rwv status` reports it |
| `ahead` | its push branch is an ancestor of the branch, as last fetched: the branch holds commits the remote lacks | `rwv status` reports it; `rwv push` publishes it |
| `gone` | the branch takes in from a branch its remote no longer has, as last fetched, and holds commits no remote-tracking branch reaches, listed | `rwv push` skips it unless `--repo` names it; `rwv status` reports it |
| `foreign upstream` | the branch takes in from a branch of another name, which the subject names; a branch publishes to the branch of its own name | `rwv push`; `rwv status` reports it |
| `no push target` | no remote is configured for the branch to push to, and the repo has no url remote to fall back on | `rwv push`; `rwv status` reports it |
| `unconfirmed` | the last pruning fetch of the remote the branch publishes to could not complete, the cause listed; the verb needs the remote's answer and judges nothing from refs as last fetched | `rwv push` |

**Choices.** Each state's choices print under `choices:` in this order, with a directory named where a choice does not run where the verb did. A state with no row here prints none.

| State | Choice | Spelled | Where | Keeps work because |
|---|---|---|---|---|
| `diverged` | C2: merge yours and theirs, theirs first (commits they dropped come back, unless only their rewrite held them) | `rwv pull --merge-upstream` | a workspace with the branch checked out | a merge discards nothing; never across lineages; `rwv abort` undoes it |
| `diverged` | C3: take theirs; yours is kept in a backup | `rwv pull --reset-to-upstream <repo>` | a workspace with the branch checked out | backup first |
| `diverged` | C2: merge yours and theirs, theirs first (commits they dropped come back, unless only their rewrite held them) | `rwv update --remote --merge-upstream --repo <m>` | a workspace with the branch checked out | a merge discards nothing; never across lineages; `rwv abort` undoes it |
| `diverged` | C3: take theirs; yours is kept in a backup | `rwv update --remote --reset-to-upstream <repo>` | a workspace with the branch checked out | backup first |
| `diverged`, for `rwv branch create` | C34: start from this clone's `<T>` | rerun with `--at <T>` | here | starts only |
| `diverged`, for `rwv workweave create` | C34: start from this clone's `<T>` | rerun with `--at <T>` | here | starts only |
| `behind` | C1: take in theirs | `rwv pull` | any workspace | fast-forward only |
| `behind` | C1: take in theirs | `rwv update --remote --repo <m>` | any workspace | fast-forward only |
| `ahead` | C7: publish it (again) | `rwv push` | here | adds only |
| `ahead` | C7: publish it (again) | `rwv push --repo <m>` | here | adds only |
| `gone` | C10: name it, or adopt the remote branch you name | `rwv branch rename <new> --adopt` | here | renames or links only |
| `gone` | C11: declare a branch for it, or keep, restore or port the entry's value | `rwv member set-branch <m> <b>` | a workspace with the branch checked out | re-pins by name, backup kept |
| `gone` | C22: squashed, rebase-merged or on another lineage: reset yours to the target's here | `rwv sync --reset-to-target <m>` | the workspace with that feature | backup kept; local only |
| `gone` | C9: delete what is landed | `rwv branch delete <W> --discard-unlanded` | a workspace where `<W>` is not checked out | deletes only what is landed elsewhere; local only |
| `gone` | C7: publish it (again) | `rwv push --repo <m>` | here | adds only |
| `foreign upstream` | C17: choose where it pushes and pulls | VCS action (git: `git -C <checkout> config branch.<b>.pushRemote <r>`, `git -C <checkout> config remote.pushDefault <r>`, `git -C <checkout> config checkout.defaultRemote <r>`, `git -C <checkout> remote add <r> <url>`, `git -C <checkout> branch --unset-upstream`) | any workspace | config only |
| `no push target` | C17: choose where it pushes and pulls | VCS action (git: `git -C <checkout> config branch.<b>.pushRemote <r>`, `git -C <checkout> config remote.pushDefault <r>`, `git -C <checkout> config checkout.defaultRemote <r>`, `git -C <checkout> remote add <r> <url>`, `git -C <checkout> branch --unset-upstream`) | any workspace | config only |
| `unconfirmed` | C33: look for what may be published but not yet fetched | `rwv fetch` | any workspace | downloads only |

**Where this sits.** [Sync semantics](../explanation/joints/sync-semantics.md).

### `name`

**Condition.** A name the verb was given cannot be used: another workweave already holds it, a repo already has a branch of that name holding commits, it reads as a revision where a branch belongs, it names nothing in a repo the verb read, or it is the name rwv keeps its branch records under.

**Why.** A workweave name maps to one workweave; recording a second would make every later address ambiguous, and a delete would remove whichever was found first. A branch that already holds commits under the name a create would give its checkout is work this create did not make, and starting the new branch there or somewhere else would hide or lose it. A member entry's `branch` declares the branch to track while the lock records where you are, so a commit id or release tag there leaves nothing to track. A name that resolves to nothing has no tip to read, and reading another branch in its place would answer a question nobody asked. The branch that holds the branch records is rwv's own: a weave branch of that name, or below it, would be read as records and written over by them.

**States.** A verb that judges a repo in one of these prints `rwv <verb>: <repo>: name <state>`, with what the state lists beneath it, and refuses unless its row below says it completes.

| State | Means | Refused by |
|---|---|---|
| `taken` | the project already records a workweave by this name at another path, or the directory the workweave would take is another workweave's, or holds a workweave that is not clean, was made from another parent, or holds something other than what this spelling would make; or a member's clone has a remote holding the name its url remote would take, with another url | `rwv workweave create`; for a member's url remote, `rwv fetch`, `rwv pull`, `rwv update`, `rwv update --remote`, `rwv backup prune`, `rwv init` and `rwv member add` |
| `exists` | a repo already has a branch of the name the verb would give its checkout, standing at a revision other than the one the verb would start it at, which the line lists; it holds commits the verb did not put there. For a rename, the new name is a local branch of the project repo or of a member the rename would move, or a branch a remote has, as last fetched; `--adopt` takes up the remote's | `rwv workweave create`, for the branch it mints in the project repo and in each owned or fork member; `rwv branch rename`; `rwv member set-branch --new`, for a branch the member's repo already has; `rwv branch create`, for a name any repo of the workspace has, locally or on a remote as last fetched |
| `a revision` | the branch `rwv member add` would declare reads as a commit id (seven or more hex digits) or a release tag (`v2.0`, `v0.3.4-rc1`) | `rwv member add`, for the remote's default branch or, with `--new`, the branch the new repo is on; `rwv member set-branch` |
| `no-such-branch` | a branch or revision the verb was asked to read names nothing in this repo, spelled as the repo's VCS resolves it; the name is listed with the repo it is missing from, and the repos that have it are still read | `rwv log`, for a range endpoint a member lacks, and for the weave branch `--against` names; `rwv branch set-mode`, for a branch the project repo has neither locally nor on a remote as last fetched; `rwv member set-branch`, for a branch the member's repo has neither locally nor on a remote as last fetched; `rwv lock --commit` and `rwv update --remote`, for a declared branch a hand edit named that exists nowhere; `rwv switch`, for a branch the project repo lacks, and `rwv branch create`, for a `--target` or `--at` that names nothing here |
| `reserved` | the name is the one rwv keeps its branch records under, or is below it (`<records>/<name>`); no weave branch can take it | `rwv branch set-mode`, `rwv branch create`, `rwv switch`, `rwv init`, `rwv clone`, and the verbs that align, lock, land or publish while the project repo is on a branch of that name; `rwv member set-branch` |
| `minted` | the project branch is the name `rwv workweave create` gave a workweave that was not given one, `<project>--<workweave>`, which is never published, and neither is its record | `rwv push` |
| `declared` | a feature would take a name that a live weave branch declares as a member's branch, the member and the declaring branches listed: the member's branch of that name is the declared branch, and a feature's own member branch cannot be it | the verbs that write, open, align or publish a feature |
| `feature` | a feature would take a name that another project's feature of that name already uses for a member both list, the project and the member named: one member branch would serve two features | the verbs that write, open, align or publish a feature; `rwv member set-branch`, for a branch a feature of that name uses |

**Choices.** Each state's choices print under `choices:` in this order, with a directory named where a choice does not run where the verb did. A state with no row here prints none.

| State | Choice | Spelled | Where | Keeps work because |
|---|---|---|---|---|
| `taken`, for `rwv workweave create` | use another name | — | here | nothing changes |
| `taken`, for `rwv workweave create` | C9: delete what is landed | `rwv workweave delete <ww> --project <project>` | the primary | deletes only what is landed elsewhere; local only |
| `taken`, for `rwv fetch` | C17: choose where it pushes and pulls | VCS action (git: `git -C <checkout> config branch.<b>.pushRemote <r>`, `git -C <checkout> config remote.pushDefault <r>`, `git -C <checkout> config checkout.defaultRemote <r>`, `git -C <checkout> remote add <r> <url>`, `git -C <checkout> branch --unset-upstream`) | any workspace | config only |
| `taken`, for `rwv pull` | C17: choose where it pushes and pulls | VCS action (git: `git -C <checkout> config branch.<b>.pushRemote <r>`, `git -C <checkout> config remote.pushDefault <r>`, `git -C <checkout> config checkout.defaultRemote <r>`, `git -C <checkout> remote add <r> <url>`, `git -C <checkout> branch --unset-upstream`) | any workspace | config only |
| `taken`, for `rwv backup prune` | C17: choose where it pushes and pulls | VCS action (git: `git -C <checkout> config branch.<b>.pushRemote <r>`, `git -C <checkout> config remote.pushDefault <r>`, `git -C <checkout> config checkout.defaultRemote <r>`, `git -C <checkout> remote add <r> <url>`, `git -C <checkout> branch --unset-upstream`) | any workspace | config only |
| `taken`, for `rwv member add` | C17: choose where it pushes and pulls | VCS action (git: `git -C <checkout> config branch.<b>.pushRemote <r>`, `git -C <checkout> config remote.pushDefault <r>`, `git -C <checkout> config checkout.defaultRemote <r>`, `git -C <checkout> remote add <r> <url>`, `git -C <checkout> branch --unset-upstream`) | any workspace | config only |
| `taken`, for `rwv update` | C17: choose where it pushes and pulls | VCS action (git: `git -C <checkout> config branch.<b>.pushRemote <r>`, `git -C <checkout> config remote.pushDefault <r>`, `git -C <checkout> config checkout.defaultRemote <r>`, `git -C <checkout> remote add <r> <url>`, `git -C <checkout> branch --unset-upstream`) | any workspace | config only |
| `taken`, for `rwv init` | C17: choose where it pushes and pulls | VCS action (git: `git -C <checkout> config branch.<b>.pushRemote <r>`, `git -C <checkout> config remote.pushDefault <r>`, `git -C <checkout> config checkout.defaultRemote <r>`, `git -C <checkout> remote add <r> <url>`, `git -C <checkout> branch --unset-upstream`) | any workspace | config only |
| `exists`, for `rwv workweave create` | use another name | — | here | nothing changes |
| `exists`, for `rwv branch create` | use another name | — | here | nothing changes |
| `exists`, for `rwv branch create` | open it | `rwv switch <W>` | here | moves only what alignment moves |
| `exists`, for `rwv workweave create` | open it | `rwv workweave create <ww> --branch <W>` | here | moves only what alignment moves |
| `exists`, for `rwv branch rename` | C10: name it, or adopt the remote branch you name | `rwv branch rename <new>` | here | renames or links only |
| `exists`, for `rwv branch rename` | C10: name it, or adopt the remote branch you name | `rwv branch rename <new> --adopt` | here | renames or links only |
| `exists`, for `rwv member set-branch` | C11: declare a branch for it, or keep, restore or port the entry's value | `rwv member set-branch <m> <b>` | a workspace with the branch checked out | re-pins by name, backup kept |
| `a revision` | C11: declare a branch for it, or keep, restore or port the entry's value | `rwv member set-branch <m> <b>` | a workspace with the branch checked out | re-pins by name, backup kept |
| `no-such-branch` | C33: look for what may be published but not yet fetched | `rwv fetch` | any workspace | downloads only |
| `no-such-branch` | C11: declare a branch for it, or keep, restore or port the entry's value | `rwv member set-branch <m> <b> --new` | a workspace with the branch checked out | re-pins by name, backup kept |
| `no-such-branch` | C18: start `<m>`'s `<b>` at a commit you choose | VCS action (git: `git -C <checkout> branch <name> <sha>`); then `rwv update` | any workspace | creates only |
| `no-such-branch`, for `rwv switch` | C24: look at it without a weave branch | VCS action (git: `git -C <checkout> switch --detach <r>/<b>`) | any workspace | the left HEAD holds nothing unpreserved |
| `no-such-branch`, for `rwv switch` | C14: keep this work on a branch, then return to the expected branch | VCS action (git: `git -C <checkout> branch <name> <sha>`); then `rwv update` | any workspace | the branch holds the work before anything moves |
| `reserved` | C10: name it, or adopt the remote branch you name | `rwv branch rename <new>` | here | renames or links only |
| `reserved`, for `rwv member set-branch` | C11: declare a branch for it, or keep, restore or port the entry's value | `rwv member set-branch <m> <b>` | a workspace with the branch checked out | re-pins by name, backup kept |
| `reserved`, for `rwv branch create` | use another name | — | here | nothing changes |
| `reserved`, for `rwv switch` | use another name | — | here | nothing changes |
| `minted` | C10: name it, or adopt the remote branch you name | `rwv branch rename <new>` | here | renames or links only |
| `declared` | C10: name it, or adopt the remote branch you name | `rwv branch rename <new>` | here | renames or links only |
| `declared` | C12: say how this branch is used: feature work onto `<T>`, or declared | `rwv branch set-mode <W> declared` | any workspace | records only |
| `declared`, for `rwv branch create` | use another name | — | here | nothing changes |
| `feature` | C10: name it, or adopt the remote branch you name | `rwv branch rename <new>` | here | renames or links only |
| `feature` | C12: say how this branch is used: feature work onto `<T>`, or declared | `rwv branch set-mode <W> declared` | any workspace | records only |
| `feature`, for `rwv member set-branch` | C11: declare a branch for it, or keep, restore or port the entry's value | `rwv member set-branch <m> <b>` | a workspace with the branch checked out | re-pins by name, backup kept |
| `feature`, for `rwv branch create` | use another name | — | here | nothing changes |

**Where this sits.** [Workweave hierarchy](../explanation/joints/workweave-hierarchy.md); [Lock as derived](../explanation/joints/lock-as-derived.md).

### `publish`

**Condition.** `rwv push` published member repos but withheld the project repo.

**Why.** The project repo carries the lock that pins each member's commit. Publishing it while a member's push failed would advertise a commit collaborators cannot fetch, so the lock is withheld until every member it pins is published.

**States.** A verb that judges a repo in one of these prints `rwv <verb>: <repo>: publish <state>`, with what the state lists beneath it, and refuses unless its row below says it completes.

| State | Means | Refused by |
|---|---|---|
| `withheld` | the project repo was not pushed: a pin the project branch's lock holds is not reached by the remote-tracking branches that must reach it, or the remote rejected a member's push; the branches that do reach each pin, and what the remote answered, are listed | `rwv push` |
| `records rejected` | a fact: the remote the project branch publishes to refused the branch that holds the weave branches' records, and the project branch was published without its record there. What the remote answered is listed. Another machine opening the branch finds it unrecorded and is asked once how to treat it (`target` `unrecorded`) | nothing: `rwv push` reports it and publishes the branch |
| `no pin` | the project branch's lock has no entry for a member the push does not act on, so the project repo was not pushed with a lock that pins nothing there | `rwv push` |

**Choices.** Each state's choices print under `choices:` in this order, with a directory named where a choice does not run where the verb did. A state with no row here prints none.

| State | Choice | Spelled | Where | Keeps work because |
|---|---|---|---|---|
| `withheld` | C7: publish it (again) | `rwv push` | here | adds only |
| `withheld` | C7: publish it (again) | `rwv push --repo <m>` | here | adds only |
| `withheld` | C27: publish this commit under a branch name you choose | VCS action (git: `git -C <checkout> push <r> <sha>:refs/heads/<name>`) | any workspace | creates or fast-forwards |
| `withheld` | C17: choose where it pushes and pulls | VCS action (git: `git -C <checkout> config branch.<b>.pushRemote <r>`, `git -C <checkout> config remote.pushDefault <r>`, `git -C <checkout> config checkout.defaultRemote <r>`, `git -C <checkout> remote add <r> <url>`, `git -C <checkout> branch --unset-upstream`) | any workspace | config only |
| `no pin` | C25: pin it at a commit you choose on its declared branch | `rwv member pin <m> <rev> --commit` | a workspace with the branch checked out | re-pins by name; backup for a detached HEAD |

### `target`

**Condition.** A weave branch's record cannot be used: the branch has none, two writers recorded it differently, the entry does not read, its target chain returns to it, or the branch it lands into is gone; or other branches' records land into it and a delete would strand them.

**Why.** A weave branch's record is the one statement of how it is used: as feature work onto a target, or as declared. Every verb that aligns members, locks, lands or publishes reads it, and rwv picks none for a branch that has none, because a guess that is wrong fails later, about something else. A record two writers set differently keeps both values until one is chosen, an entry rwv cannot read is refused for that branch alone, and a target chain that returns to its branch could never land.

**States.** A verb that judges a repo in one of these prints `rwv <verb>: <repo>: target <state>`, with what the state lists beneath it, and refuses unless its row below says it completes.

| State | Means | Refused by |
|---|---|---|
| `no target` | the branch's record is `declared`, so it names no branch the branch lands into | `rwv branch delete`, which needs the branch whose commits should have landed, named with `--target` |
| `unrecorded` | the branch has no record here: a raw switch or rename made it, its record was never published, or it came from a remote that carries none. The branches that could be its target are listed, nearest first | `rwv lock`, `rwv update`, `rwv pull`, `rwv sync`, `rwv sync-to` and `rwv push`, for the project repo's checked-out branch; `rwv workweave create`, for the branch it forks from; `rwv branch create` with no spelling, for the branch it starts at, and `--target`, for a target that has no record; `rwv switch` and `rwv workweave create --branch`, for a branch with no record; `rwv branch set-mode`, for a `--target` that has no record |
| `conflict` | two writers recorded the branch differently and a merge kept both values, listed; the conflict stays on the machine whose merge met it | every verb that reads the branch's record on this machine |
| `differs` | the record a verb would give the branch differs from the one it holds, both listed: a record is replaced only by `rwv branch set-mode`; nothing on the records branch is conflicted | `rwv branch rename --adopt` onto a name recorded otherwise |
| `unreadable` | the branch's entry on the records branch does not read as a record, with why and the rwv release that wrote it when its commit names one; other branches' entries read as usual | every verb that reads the branch's record |
| `cycle` | the branch's target chain returns to it, listed from the branch | `rwv branch set-mode`, for a `--target` that would close one, and every verb that reads the record of a branch a merge of records put on one |
| `gone` | the branch the record lands into exists nowhere: not as a local branch, nor on a remote as last fetched | `rwv branch list` reports it |
| `dependents` | another branch's record names this branch as the one it lands into, one line for each such branch; deleting this one would leave that record with no target | `rwv branch delete`, `rwv workweave delete` and `rwv sync-to --retire` |
| `record changed` | the checked-out branch's record differs from the one this workspace last saw it hold, and this workspace did not write the change: the old and the new value, listed, and whether the change arrived by fetch or was written in another workspace of this clone | `rwv sync`, before anything is fetched or moved, until the change is accepted or the old value restored; `rwv status`, `rwv update`, `rwv pull`, `rwv switch`, `rwv push` and `rwv sync-to` report it |

**Choices.** Each state's choices print under `choices:` in this order, with a directory named where a choice does not run where the verb did. A state with no row here prints none.

| State | Choice | Spelled | Where | Keeps work because |
|---|---|---|---|---|
| `no target` | C12: say how this branch is used: feature work onto `<T>`, or declared | `rwv branch set-mode <W> feature --target <T>` | any workspace | records only |
| `unrecorded` | C33: look for what may be published but not yet fetched | `rwv fetch` | any workspace | downloads only |
| `unrecorded` | C12: say how this branch is used: feature work onto `<T>`, or declared | `rwv branch set-mode <W> feature --target <T>` | any workspace | records only |
| `unrecorded` | C12: say how this branch is used: feature work onto `<T>`, or declared | `rwv branch set-mode <W> declared` | any workspace | records only |
| `conflict` | C12: say how this branch is used: feature work onto `<T>`, or declared | `rwv branch set-mode <W> feature --target <T>` | any workspace | records only |
| `conflict` | C12: say how this branch is used: feature work onto `<T>`, or declared | `rwv branch set-mode <W> declared` | any workspace | records only |
| `differs` | C12: say how this branch is used: feature work onto `<T>`, or declared | `rwv branch set-mode <W> feature --target <T>` | any workspace | records only |
| `differs` | C12: say how this branch is used: feature work onto `<T>`, or declared | `rwv branch set-mode <W> declared` | any workspace | records only |
| `unreadable` | C12: say how this branch is used: feature work onto `<T>`, or declared | `rwv branch set-mode <W> feature --target <T>` | any workspace | records only |
| `unreadable` | C12: say how this branch is used: feature work onto `<T>`, or declared | `rwv branch set-mode <W> declared` | any workspace | records only |
| `cycle` | C12: say how this branch is used: feature work onto `<T>`, or declared | `rwv branch set-mode <W> feature --target <T>` | any workspace | records only |
| `cycle` | C12: say how this branch is used: feature work onto `<T>`, or declared | `rwv branch set-mode <W> declared` | any workspace | records only |
| `gone` | C12: say how this branch is used: feature work onto `<T>`, or declared | `rwv branch set-mode <W> feature --target <T>` | any workspace | records only |
| `dependents` | C12: say how this branch is used: feature work onto `<T>`, or declared | `rwv branch set-mode <W> feature --target <T>` | any workspace | records only |
| `record changed` | C6: align members for the checked-out branch | `rwv update` | here | moves only what alignment moves |
| `record changed` | C12: say how this branch is used: feature work onto `<T>`, or declared | `rwv branch set-mode <W> feature --target <T>` | any workspace | records only |
| `record changed` | C12: say how this branch is used: feature work onto `<T>`, or declared | `rwv branch set-mode <W> declared` | any workspace | records only |

**Where this sits.** [Workweave hierarchy](../explanation/joints/workweave-hierarchy.md); [Formats](./formats.md).

### `history`

**Condition.** Integrating work needs more than a fast-forward, and the branch it moves into records no `history` for that repo; or a run names a `history` the branch records otherwise; or the branch records `ff-only` and only a fast-forward would do.

**Why.** A branch's `rwv.toml` records how it accepts landings: `linear`, `merge` or `ff-only`, at the top level and per member. No value is the default and no role implies one, because the choice between rewriting a feature onto the target and recording a merge is the target's owners' to make, once, and every later landing follows it. A fast-forward reads no setting.

**States.** A verb that judges a repo in one of these prints `rwv <verb>: <repo>: history <state>`, with what the state lists beneath it, and refuses unless its row below says it completes.

| State | Means | Refused by |
|---|---|---|
| `unset` | integrating this repo needs more than a fast-forward, and the branch it moves into records no `history` for it: neither the member's own nor the top-level one | a verb that takes a target's work in or lands work into it, where a fast-forward does not do it |
| `contradicted` | the run names a `history` for one run, and the branch it moves into records another for this repo | a verb that takes a target's work in or lands work into it, run with a `history` for one run |
| `not fast-forward` | the branch records `ff-only` for this repo, and the landing is not a fast-forward: the target has work the source has not taken in | a landing into a branch that records `ff-only` |

**Choices.** Each state's choices print under `choices:` in this order, with a directory named where a choice does not run where the verb did. A state with no row here prints none.

| State | Choice | Spelled | Where | Keeps work because |
|---|---|---|---|---|
| `unset` | C23: say how `<T>` accepts landings | `rwv branch set-history <h>` | a workspace on `<T>` | records only |
| `contradicted` | C23: say how `<T>` accepts landings | `rwv branch set-history <h>` | a workspace on `<T>` | records only |
| `not fast-forward` | C36: take the target's new work in first | `rwv sync` | the workspace with that feature | by the target's setting; `rwv abort` undoes it |

**Where this sits.** [Formats](./formats.md) states the `history` keys; [Sync semantics](../explanation/joints/sync-semantics.md).

### `protected`

**Condition.** A verb would rewrite, rename or delete a branch that other branches depend on staying where it is: a branch some live weave branch declares for a member, or the project branch whose record is `declared`.

**Why.** A declared branch is what every weave branch that declares it follows, and what a fresh clone checks out; rewriting, renaming or deleting it moves them all at once, and a rewrite can strand commits others have built on. A declared-mode project branch is protected against rewrites and the deletion of an unlanded push branch alone: a rename moves only its record, stored entries and the records that land into it, and a landed push branch deleted loses nothing.

**States.** A verb that judges a repo in one of these prints `rwv <verb>: <repo>: protected <state>`, with what the state lists beneath it, and refuses unless its row below says it completes.

| State | Means | Refused by |
|---|---|---|
| `declared` | a member's branch that some live weave branch declares, listed with the weave branches declaring it; rwv never rewrites, renames or deletes it | `rwv branch rename`, for a feature's member branch of the renamed name; `rwv push --overwrite`, for a member; `rwv branch delete` and `rwv workweave delete` with `--delete-remote` or `--delete-remote-unlanded`, for a member branch of the deleted name; `rwv sync` and `rwv sync-to` with `--strategy rebase`, for a member on the branch that has commits to replay |
| `declared mode` | a project branch whose record is `declared`; rwv never rewrites it, and never deletes its unlanded push branch. Renaming it, and deleting its push branch once landed, run | `rwv push --overwrite`; `rwv branch delete --delete-remote-unlanded`, for the project repo |

**Choices.** Each state's choices print under `choices:` in this order, with a directory named where a choice does not run where the verb did. A state with no row here prints none.

| State | Choice | Spelled | Where | Keeps work because |
|---|---|---|---|---|
| `declared` | C2: merge yours and theirs, theirs first (commits they dropped come back, unless only their rewrite held them) | `rwv update --remote --merge-upstream --repo <m>` | a workspace with the branch checked out | a merge discards nothing; never across lineages; `rwv abort` undoes it |
| `declared mode` | C2: merge yours and theirs, theirs first (commits they dropped come back, unless only their rewrite held them) | `rwv pull --merge-upstream` | a workspace with the branch checked out | a merge discards nothing; never across lineages; `rwv abort` undoes it |

**Where this sits.** [Workweave hierarchy](../explanation/joints/workweave-hierarchy.md); [Destructive operations](../explanation/destructive-operations.md).

## Tokens

### `adopt-path-outside-weave`

**Condition.** `rwv member add --adopt <path>` names a location under neither the
primary weave root nor, when run from a workweave, that workweave. A relative
`<path>` resolves against the directory `rwv` runs in, or the `-C`/`-w`
directory, and the location is where the path leads with every symlink in it
followed: `sym/..` climbs from the directory `sym` points to.

**Why.** The member path `--adopt` records is the clone's position under the
weave root containing it. A location outside every weave root has no such
position, so there is nothing to record.

**Exits.** Name the clone by a path inside the weave: relative to where you
run `rwv`, or absolute.

### `ambiguous-explain-token`

**Condition.** `rwv explain` was given a sub-kind that is a sub-kind of more
than one finding kind, and [Doctor findings](./doctor-findings.md) has an entry
for it under each. `safe-to-fix` is one: `index-drift` and
`working-tree-drift` both carry it, with different repairs.

**Why.** The name means one thing under each kind, and nothing in it says
which kind you meant. Serving one of the entries would answer a question you
may not have asked.

**Exits.** Run one of the commands the refusal lists. Each names the kind
before the sub-kind, as `rwv doctor` prints a finding's token.

### `ambiguous-workweave-address`

**Condition.** Two recorded `(project, workweave)` pairs render the same flat
`<project>--<name>` address, so `-w` names more than one checkout.

**Why.** The flat address is meant to be a bijection: one spelling, one
workweave. When it is not, acting on "the" match would pick one of two live
checkouts by accident, and the wrong one is destructive on a delete.

**Exits.** Address the workweave by path with `-C` instead, which is
unambiguous. To remove the collision permanently, retire one of the two and
recreate it under a name that does not collide.

**Where this sits.** The flat rendering and the characters it reserves are in
[Formats](./formats.md); the addressing model is
[Workweave hierarchy](../explanation/joints/workweave-hierarchy.md).

### `backslash-in-repo-path`

**Condition.** A repo path spells `\`.

**Why.** A member repo path is a POSIX-shaped key that becomes a directory
path on every platform. Accepting `\` would make one manifest mean two
different trees depending on the OS reading it.

**Exits.** Spell the path with `/`. This is a validation refusal — nothing was
written, so there is nothing to undo.

**Where this sits.** [Formats](./formats.md) states the path grammar.

### `claude-settings-missing`

**Condition.** `rwv setup claude` was asked to edit a settings file that does
not exist.

**Why.** The verb edits an existing settings document rather than creating
one, because creating it would mean inventing defaults for a tool rwv does not
own.

**Exits.** Run the tool once so it writes its own settings file, then re-run
`rwv setup claude`.

### `conflicting-root-write`

**Condition.** `rwv activate` was asked to switch the weave root to another
project while a tool of the project it presents left a lock file as a real
file at the root, and the project's own copy differs from it with neither
version control nor rwv's record holding that copy as it stands.

**Why.** Either way of taking that file loses one of the two copies, so rwv
takes neither. Left at the root over a switch, nothing would say which project
wrote it, and the next project to surface that name would take it in as its
own. rwv keeps no record of whose a root file is; refusing the switch is what
keeps the file with the project whose tool wrote it.

**Exits.** Keep the root's copy by committing or deleting the project's, or
keep the project's by deleting the file at the weave root. Then run `rwv doctor
--fix` and re-run the switch. Nothing was changed, so there is nothing to undo.

**Where this sits.** [Weave root](../explanation/joints/weave-root.md).

### `cwd-in-inactive-project`

**Condition.** A verb that writes into its project (`add`, `remove`, `role`,
`lock`, `fetch`, `pull`, `update`, `push`, `sync`, `sync-to`, or `doctor
--fix`) was run from inside `projects/<name>/` at a primary weave root, with no
`--project`, while `.rwv-active` selects a different project.

**Why.** Changing into a project's directory does not select it, so the verb
would write into the active project while the directory says the other one was
meant. Writing into a project the operator did not mean is the outcome the
refusal prevents; `rwv status` and `rwv doctor` without `--fix` only read and
do not refuse, and `rwv status` warns.

**Exits.** Pass `--project <name>` naming either project to act on it from
where you are, or `rwv activate <name>` to make the directory's project the
active one. Nothing was written.

**Where this sits.** [Weave root](../explanation/joints/weave-root.md).

### `dangling-primary`

**Condition.** A workweave marker records a `primary:` path that no longer
exists.

**Why.** The marker is how a workweave knows which weave it belongs to. With
the primary gone, every path the marker implies is unresolvable, and rwv
cannot tell a moved primary from a deleted one.

**Exits.** If the primary moved, re-point the marker at its new location. If
it was deleted, this workweave is orphaned — retire the directory. `rwv doctor
--fix` does not guess between those two.

**Where this sits.** [Workweave lifecycle](../explanation/joints/workweave-lifecycle.md).

### `derivation-inputs-moved`

**Condition.** An attested input changed while a generated file was being
written from it.

**Why.** rwv records what a generated file was derived from so a later read
can tell "this is current" from "this looks plausible". If an input moves
mid-write, the record would attest a derivation that never happened as a whole.

**Exits.** Re-run the verb. The write was abandoned rather than half-recorded,
so the previous state is intact.

**Where this sits.** [File ownership](../explanation/joints/file-ownership.md).

### `dropped-repo-has-unique-commits`

**Condition.** A checkout being dropped from the lock, or deleted by `rwv
member remove --delete`, carries commits no other clone reaches: a branch
whose commits no remote-tracking branch holds, or a [backup](../explanation/destructive-operations.md)
holding commits no branch reaches.

**Why.** Dropping a repo removes its checkout. If it holds the only copy of
some commits, removal destroys them, and no `rwv abort` recovers work that was
never anywhere else. A backup is the only copy by construction, so it counts.

**Exits.** Push the commits to their remote, or merge them somewhere that
survives, then re-run `rwv member remove --delete`. `rwv backup list` shows the
backups and what each holds; `rwv backup drop <repo> <name>` drops the ones that are
not wanted. If the commits are genuinely unwanted, remove the checkout by hand
so the loss is a deliberate act. A sync whose replay refuses this prune still
completes, reports the checkout and exits 1; re-running it will not prune the
checkout, so remove it by hand once nothing in it is needed.

**Where this sits.** [Destructive operations](../explanation/destructive-operations.md).

### `empty-selector-pattern`

**Condition.** `--repo re:` or `--repo glob:` was given with nothing after the
prefix.

**Why.** An empty pattern matches everything or nothing depending on the
engine, and a repo subset that silently means "all" is the shape that runs a
destructive verb over the whole weave.

**Exits.** Supply a pattern, or drop `--repo` entirely to mean all repos
explicitly.

### `foreign-clone-at-placement`

**Condition.** `rwv member add <url>` (or shorthand) for a repo the manifest does not
list yet places at a path that already holds a clone whose configured
`origin` URL is not equivalent to the one given, or that has no `origin` at
all.

**Why.** A clone already at the placement stands in for the one `add` would
make only when it is a clone of that URL. Registering any other clone under
the URL given would record a remote the checkout does not track. The
comparison reads the configured URL, not one a `url.<base>.insteadOf` rule
rewrites, and treats a `file://` URL as equal to the path it encodes.

**Exits.** Register the existing clone under the URL it records with `rwv member add
--adopt <path>`, or move it aside and re-run the add. Nothing was written.

### `foreign-clone-in-workweave`

**Condition.** `rwv member add --adopt <path>`, run from a workweave, names a
location inside the workweave that holds a clone of its own rather than a
worktree of the primary's clone at the same member path.

**Why.** `--adopt` registers the canonical clone under the primary weave root,
and the workweave gets a worktree of it. Registering the primary's copy while
`<path>` names a different clone would adopt something other than what was
pointed at.

**Exits.** Move the clone to its member path under the primary weave root and
name it there, or name the primary's clone directly. Nothing was written.

### `foreign-tip`

**Condition.** `rwv abort` found a ref at a tip the recorded operation does not
account for. The verdict is the same whichever workspace of the op abort runs
from.

**Why.** Abort restores each repo to the savepoint the op took. A tip the op
never wrote means something else moved that ref, and restoring would discard a
change the operation is not responsible for.

**Exits.** Inspect the named repos and decide: if the foreign commits are
wanted, move them somewhere safe first; if not, reset the ref yourself and
re-run `rwv abort`, or pass `--abandon-foreign-tip <repo>` to restore that repo
anyway, first keeping the abandoned commits as a backup marked `abandoned`,
which `rwv backup list` shows and `rwv backup drop` drops. Name the project
repo as `projects/<name>`. The refusal moves nothing and writes nothing.

**Where this sits.** [Shared refs and drift](../explanation/joints/shared-refs-drift.md).

### `head-not-on-branch`

**Condition.** HEAD is detached, unborn, or unreadable in a repo the verb must
land on or move. `rwv sync-to` checks both ends: every repo of the target, and
the project repo and every member repo of the source workweave.

**Why.** Landing work means advancing a branch. A detached HEAD names no
branch to advance, and an unborn one has no commit to advance from, so the
verb has nowhere to put the result.

**Exits.** Check out the branch you meant. For a detached repo in the source of
`rwv sync-to`, the refusal prints the command, chosen by how HEAD stands
against the workweave's branch: `git switch <branch>` where the branch carries
HEAD; `git switch <branch>` then `git merge --ff-only <HEAD>` where HEAD is
ahead, since switching alone would leave HEAD's commits on no branch; and where
the two have diverged, `git switch -c <new-branch>`, then a rebase onto the
workweave's branch and a fast-forward of that branch to `<new-branch>`, which
leaves the workweave on its own branch rather than on a shared one. For an
unborn branch, make an initial commit first.

**Where this sits.** [Workweave lifecycle](../explanation/joints/workweave-lifecycle.md).

### `health-floor-too-low`

**Condition.** The weave records a health floor below the version this binary
requires.

**Why.** Repair arms for older states are removed at each release rather than
carried forever. A binary that cannot repair the state it finds must say so
instead of acting on assumptions that no longer hold.

**Exits.** Step through the bridge version the message names: install it, run
`rwv doctor --fix --all` until `rwv doctor --all` reports clean — which records
the floor — then upgrade again.

**Where this sits.** [Upgrade rwv](../how-to/upgrade-rwv.md).

### `inapplicable-flag`

**Condition.** A flag was passed in a mode where it has no meaning.

**Why.** Accepting it silently would let an operator believe a constraint was
applied that nothing read. The refusal is generic: a per-flag
token would name the remedy's argument rather than a state.

**Exits.** Drop the flag, or switch to the mode that reads it — the message
names which. For example, `--force` on a verb that does not declare it refuses:
rwv has no blanket force, and each refusal names the flag that consents to the
one loss it would cause.

### `invalid-ref-name`

**Condition.** A name is not usable as a ref-name component.

**Why.** rwv mints refs from names an operator supplies. A name git will not
accept as a ref becomes a failure deep inside a VCS call, far from the input
that caused it, so it is rejected at the boundary instead.

**Exits.** Choose another name. The rules are git's rather than rwv's, and the
message names which one was broken.

**Where this sits.** [Formats](./formats.md) lists the characters rwv's own
naming rules exclude on top of git's.

### `malformed-provider`

**Condition.** `--provider` is not `registry/owner`.

**Why.** The provider is two fields, and a single token could be either one.
Guessing would silently create a project under the wrong owner.

**Exits.** Spell it `registry/owner`.

### `malformed-repo-path`

**Condition.** A repo path is not `registry/owner/repo`.

**Why.** The three segments are what makes a path resolvable to a clone URL
without a lookup table. A shorter path names no registry, so no URL can be
inferred.

**Exits.** Supply all three segments, or pass a full URL and let `rwv member add`
derive the path.

**Where this sits.** [Formats](./formats.md).

### `malformed-workweave-address`

**Condition.** `-w` is not `<project>--<name>` with both halves non-empty.

**Why.** The flat address is the one spelling that identifies a workweave
without a path. Half an address cannot select one.

**Exits.** Give both halves, or address by path with `-C`.

### `marker-binding-disagreement`

**Condition.** The project this operation is bound to and the one the marker
records are not the same.

**Why.** A workweave is created for one project and holds that project's
repos. Acting under a different binding would write one project's state into
another's checkout.

**Exits.** Run the verb from the workspace whose marker matches, or pass the
project the marker records. Do not edit the marker to agree — that hides the
disagreement rather than resolving it.

### `missing-creation-param`

**Condition.** An `rwv member add --new` creation address's registry declares a
required parameter the supplied map has no entry for.

**Why.** `plan_creation` cannot mint a URL or an upstream from an incomplete
parameter set, and guessing a value would place or create something the
operator did not ask for.

**Exits.** Add `--param <name>=<value>` for each parameter the refusal names
— the same declared surface `rwv explain member`'s creation-parameter table
renders, so the two cannot disagree.

### `missing-vcs-tool`

**Condition.** A verb that reads or writes a repository could not start the
version-control tool its backend drives (`git`): no executable of that name is
on `PATH`, or the system refused to run it. The message carries what the system
said.

**Why.** Every repository operation goes through that tool, so a verb that
started anyway would stop at its first repository read, after it may already
have written something else. The check runs before the verb does anything.
`rwv explain`, `rwv completions`, `rwv setup claude`, `--help`, `--version` and
an external verb (a plugin, which rwv starts without reading a repository)
run without the tool.

**Exits.** Install git at the release the message names, or newer, and put it
on `PATH`.

**Where this sits.** [Install](../introduction.md#install).

### `nested-project-name`

**Condition.** The name would place a project inside another project.

**Why.** Projects are siblings under `projects/`. A nested one would be found
by the containment walk as part of its parent, and every path derived from it
would be ambiguous.

**Exits.** Choose a name that does not traverse into an existing project.

### `no-active-project`

**Condition.** No project was supplied and none is selected here.

**Why.** Project-scoped verbs act on one project's repos. With no selection
rwv would have to pick, and picking wrongly acts on the wrong tree.

**Exits.** `rwv activate <name>` to select one, or pass `--project <name>` for
a single invocation.

**Where this sits.** [Weave root](../explanation/joints/weave-root.md).

### `no-clone-to-adopt`

**Condition.** `rwv member add --adopt <path>` names a path that holds no clone. A
bare repository is not a clone here: it has no checkout to register.

**Why.** `--adopt` registers a clone already on disk and reads its URL from
that clone. With nothing there, there is no URL to record and nothing to
register.

**Exits.** Name the clone by its path (relative to the directory `rwv` runs in), or drop `--adopt`
and give the URL to clone from (`file://` for a local remote, which is the
spelling for a bare repository on disk).

### `no-explain-entry`

**Condition.** `rwv explain` has no page for the name given.

**Why.** `rwv explain` is reflection over core's committed, CI-checked
surfaces. It never consults `$PATH`, so a plugin's name is not an entry here
even when the plugin is installed and dispatchable.

**Exits.** `rwv explain` with no argument lists everything served. For a
plugin, its own `--help` is the documentation.

**Where this sits.** [Plugin boundary](../explanation/joints/plugin-boundary.md).

### `no-matching-registry`

**Condition.** `rwv member add` was given a URL from which no placement can be
derived, such as one with no owner and repo path (`https://example.com`).

**Why.** Every member lands at `<registry>/<owner>/<repo>/`, derived from its
URL: a built-in registry's name, the URL's own host, or `local` for `file://`.
A URL that yields no owner and repo cannot be placed on disk.

**Exits.** Give the repository's own clone URL. An argument that is not a URL
at all is a different refusal, `unresolvable-repo-source`.

### `no-op-recorded`

**Condition.** `--continue` or `abort` was invoked where no operation is
recorded.

**Why.** Both act on op-state. With none present there is nothing to resume or
roll back, and inventing a starting point would replay from a state no
operation ever established.

**Exits.** Omit `--continue` to start a new operation. If you expected one to
be in flight, it completed or was already aborted.

### `no-remote-default-branch`

**Condition.** A repo records no default branch for its remote (`origin/HEAD`
is unset, for git). `rwv member add` reads it to fill `branch`, and `rwv push` reads
it for the project repo, whose canonical branch it is.

**Why.** rwv records what to track and what to publish rather than guessing
`main`. A repo with no recorded default gives nothing verifiable to use, and a
fabricated guess would be wrong silently.

**Exits.** The message names the repair for the VCS in question — for git,
`git fetch origin && git remote set-head origin -a` in the named repo — then
re-run. `rwv member add <url>` names its own, in the clone it keeps: a fresh clone
records any default the remote names, so the remote has either no branch yet
or a default that names none of its branches. For the first, commit in the
clone and push the branch; for the second, check out one of the branches the
message lists. Either way, record the branch with
`git remote set-head origin <branch>`. `rwv member add --adopt` reads the same two
cases from what the clone has fetched; where it has fetched no branch, the
message leads with `git fetch origin`, then names `set-head -a` if the remote
has a branch and the first push if it has none.

### `no-remote-url`

**Condition.** `rwv member add --adopt` names a clone with no remote to read a
URL from, one with several remotes (the URL to record is a choice among them),
or one whose remote records a relative path.

**Why.** The manifest records where a repo comes from. A clone with no remote
has no origin to record, and an invented one would not fetch. A clone with
several remotes would have rwv pick one for you: name the URL instead
(`rwv member add <url>`, for a clone at that URL's placement), or remove the
remotes that are not the member's. git reads a relative path from each
checkout's own directory, so it names no one remote to record.

**Exits.** Add the remote to the clone, or give it an absolute URL
(`git -C <path> remote set-url origin <url>`), and re-run; or move the clone
aside and add the repo by its URL.

### `no-such-workweave`

**Condition.** The address names no recorded workweave.

**Why.** Addressing is by record, not by directory scan: a directory that
looks like a workweave but is not recorded in the project clone's workweave index is not addressable.

**Exits.** `rwv workweave list` shows what is recorded. If the
directory exists but is unrecorded, `rwv doctor --fix` adopts it when it sits
in the recorded container under the expected name. A target for `rwv sync-to`
that is a bare word names a workweave; to name a directory, write `./<dir>`.

**Where this sits.** [Workweave hierarchy](../explanation/joints/workweave-hierarchy.md).

### `no-weave-root`

**Condition.** The containment walk found no weave above the working
directory.

**Why.** Every path rwv resolves is relative to a weave root. Without one
there is no frame to interpret the command in.

**Exits.** `cd` into a weave, or `rwv init` to create one here.

**Where this sits.** [Weave root](../explanation/joints/weave-root.md).

### `non-clone-at-placement`

**Condition.** `rwv member add <url>` finds something at the member's placement that
is not a clone: a directory, empty or not, or a file. From a workweave the
workweave's own placement is checked too, and so is it for `rwv member add --adopt
<path>` when `<path>` lands inside the workweave.

**Why.** A clone at the placement can stand in for the one `add` would make,
because its `origin` can be compared with the URL. Anything else has no remote
to compare, cannot be registered, and leaves no room to clone or place a
checkout.

**Exits.** Move it aside and re-run the command the message names. Nothing
was written.

### `not-fast-forwardable`

**Condition.** The tips are not in the fast-forward relation `--strategy=ff`
requires.

**Why.** `ff` is the strategy that promises to add nothing and rewrite
nothing. Where a fast-forward is impossible, honoring that promise means
stopping rather than quietly doing something else.

**Exits.** Re-run with `--strategy rebase` to replay the divergent commits, or
reconcile the repos by hand first. Where the commits a rebase would replay hold
a merge, `rwv sync` names `rwv sync --from <target>` instead, which takes the
target in by a merge rather than flattening it. Past replay the exit is
different — see `target-diverged-mid-op`.

**Where this sits.** [Sync semantics](../explanation/joints/sync-semantics.md).

### `occupied-bootstrap-dir`

**Condition.** The bootstrap target is neither a workspace nor empty, and no
consent flag was given.

**Why.** Bootstrapping writes weave structure into a directory. Doing that
over unrelated content mixes two trees with no record of which files were
whose.

**Exits.** Choose an empty directory, or pass `--allow-non-empty-dir` to
accept bootstrapping over what is there.

### `occupied-placement`

**Condition.** An `rwv member add <url>`, or an `rwv member add --new` creation address,
places at a path the manifest already maps to a different URL.

**Why.** Placement is a function of the identity a registry mints, not of a
creation parameter like `--param root=<dir>`. Two different roots naming the
same owner and repo would both place at the same path if this proceeded
silently, and the second creation would overwrite the first's meaning in the
manifest without saying so.

**Exits.** To finish an entry already in the manifest, re-run with the URL it
records. Otherwise use a different owner or repo so the two do not collide, or,
if replacing it is what's intended, remove the existing entry and its clone
first with `rwv member remove <path> --delete`. A plain `rwv member remove` keeps the clone,
which the re-run then refuses as `foreign-clone-at-placement`. In a workweave,
whose clones the primary weave holds, run the delete in the workweave and then
in the primary weave.

### `op-acquisition-raced`

**Condition.** The atomic op-state create lost to a peer whose record then
vanished.

**Why.** Two processes tried to start an operation at once. One won and then
finished or aborted, so by the time this process looked, the record it lost to
was gone — a race, not a stuck state.

**Exits.** Retry. This is distinct from `op-in-progress`, where a record is
present and the exits are resume-or-abort rather than retry.

### `op-in-progress`

**Condition.** A sync or sync-to operation already holds op-state covering
this workspace.

**Why.** An operation in flight is rewriting the very repos another verb would
read or move. Op-state persists across a crash — there is no
auto-expiry, because a stale record and a live one are indistinguishable from
outside.

**Exits.** Two, and the message names both: resume with `--continue` from the
owning workspace once the cause is resolved, or `rwv abort` to roll the whole
operation back. Waiting is only an exit if the operation is genuinely still
running.

**Where this sits.** [Sync semantics](../explanation/joints/sync-semantics.md).

### `op-parked`

**Condition.** A phase stopped and op-state is held at that phase.

**Why.** The refusal that stopped the phase changed nothing, and the operation
is still recorded. Both ways out of it remain open, which is what a fresh
refusal cannot tell you.

**Exits.** Fix the cause the message names, then `--continue`; or `rwv abort`
to roll the whole operation back. The route line for a parked operation names
the *gate* that fired where that gate has its own token, so you may be sent to
a more specific entry than this one.

**Where this sits.** [Sync semantics](../explanation/joints/sync-semantics.md).

### `partial-run-aborted`

**Condition.** Some repos failed, and an artifact that would otherwise be
written was withheld.

**Why.** This is not a bare failure tally. A lock or success record written
over a partial run would describe a state that never existed, so the run
declines to write it — the withholding is the decision this token names.

**Exits.** Fix the failures the run listed, then re-run. Nothing partial was
recorded, so the re-run starts from the same place this one did.

### `project-dir-missing`

**Condition.** The named project has no `projects/<name>/` in this workspace.

**Why.** A project is its directory. rwv will not create one implicitly to
satisfy an address, because a typo would then produce an empty project rather
than an error.

**Exits.** Check the spelling against `rwv status`, or `rwv clone` the project
into this workspace if it belongs here.

### `project-dir-occupied`

**Condition.** The project directory name is already taken.

**Why.** Two projects at one directory name would share every derived path.
rwv will not merge into an occupied slot, because the occupant's files are not
rwv's to reinterpret.

**Exits.** For `rwv clone <source>`: if the project already there is the one
you meant, `rwv update --project <name>` brings up its members in place; to
clone the other one too, run `rwv clone <source>` in an empty directory, which
becomes a weave of its own. For `rwv init`, choose another name.

### `provider-cannot-mint-url`

**Condition.** `rwv init --provider <registry>/<owner>` names a registry that
cannot construct a clone URL from an owner and a project name alone.

**Why.** `init --provider` mints a URL and adds it as the project repo's
remote; it carries no creation surface of its own — no `--root`, no
parameters — so a registry whose `clone_url` needs more than that, `local`
being the case that exists, has nothing to mint from.

**Exits.** Create the project without `--provider` (`rwv init <name>`), then
set the remote once the repository exists elsewhere.

### `registry-path-disagreement`

**Condition.** Two paths both claim to be this workweave.

**Why.** The index maps a name to one path. Two claimants mean a delete could
remove either, and the one it removes is not the one the operator addressed.

**Exits.** Inspect both directories and retire the stale one, then re-run.

### `repair-target-changed`

**Condition.** The object `--fix` re-observed before acting is gone or moved.

**Why.** `--fix` observes, decides, then re-observes immediately before
writing. A target that changed in that window is one another process is
touching, and repairing it would act on a state nobody surveyed.

**Exits.** Re-run `rwv doctor --fix`. This is a race rather than a fault:
whether it vanished or merely moved is timing, which is why one token covers
both.

### `repo-not-in-manifest`

**Condition.** `rwv member remove` names a path the manifest does not carry, and
this workspace holds no checkout there that is this project's to finish
removing: one this project's lock still names, or one no other project lists.

**Why.** Removing something absent is either a typo or a stale script. Silently
succeeding would let both go unnoticed. A checkout another project lists is
that project's, not this one's leftover.

**Exits.** Check the spelling against `rwv status`. An entry removed from
`rwv.toml` by hand is finished by `rwv member remove <path>` while its checkout is
still on disk; this refusal means there is no such checkout here.

### `repo-without-commits`

**Condition.** A repo the create would fork has no resolvable HEAD.

**Why.** A workweave forks each repo at a commit. A repo with no commits has
no point to fork from, and creating an empty checkout would produce a
workweave whose repos are not comparable to the source's.

**Exits.** Make an initial commit in the named repo, or remove it from the
manifest if it does not belong.

### `reserved-repo-path`

**Condition.** A repo path is `(project)`.

**Why.** `(project)` is how the project repo is named wherever a repo is a key:
in an operation's recorded tips, in `--json` records, and in the output of
`rwv abort`. A member at that path could not be told from the project repo.

**Exits.** Choose another path for the member. This is a validation refusal —
nothing was written, so there is nothing to undo.

**Where this sits.** [Formats](./formats.md) states the path grammar.

### `resume-contradicts-record`

**Condition.** `--continue` arguments disagree with the recorded operation.

**Why.** The record is what the operation is; the arguments are what the caller
believes it is. Proceeding under the caller's belief would resume a different
operation than the one that stopped.

**Exits.** Re-invoke `--continue` with arguments matching the record — the
message names both sides — or `rwv abort` if the record is the thing that is
wrong.

### `shared-clone-referenced`

**Condition.** `--delete`'s clone is referenced by other projects, and
`--delete-shared-clone` was not given.

**Why.** One clone can serve several projects. Deleting it because one project
dropped the repo would break the others without their asking.

**Exits.** Drop the repo from the manifest without `--delete` to leave the
clone in place, or pass `--delete-shared-clone` to accept removing it for
every project that references it. This refusal is only raised at the primary
weave, the one place `--delete` removes the canonical clone.

### `shared-name-contested`

**Condition.** Two projects claim one weave-root surfacing name.

**Why.** Surfacing links live at the weave root, which is a single namespace.
Two claimants mean whichever activates last wins, and the loser's link
silently points at the other's file.

**Exits.** Rename the surfaced file in one of the projects, or stop surfacing
it there.

**Where this sits.** [Symlinks as structure](../explanation/joints/symlinks-as-structure.md).

### `stale-lock`

**Condition.** `rwv sync-to` found a member whose checkout is not at the commit
the lock it reads pins for it, or for which the lock has no entry.

**Why.** sync-to replays against another workspace's committed lock and relocks
from its own checkouts. A tip that differs from its pin means the lock the replay reads
and the commits the replay carries disagree: a primary source whose lock is
behind its HEAD would hand on commits no pin records, and a checkout behind or
diverged from its pin would be moved or relocked over commits nobody chose to
drop.

**Exits.** `rwv status` and `rwv doctor` judge the same condition per member as
a [`pin`](./doctor-findings.md#pin) state, each with its own choices.
`rwv lock --commit` records a checkout that is ahead of its pin;
`rwv member pin <repo> HEAD --commit` pins one that is behind or has diverged, by
name; `rwv member pin <repo> <rev> --commit` gives a member the lock has no pin
for one. `rwv sync-to --allow-stale-lock` skips the check.

**Where this sits.** [Sync semantics](../explanation/joints/sync-semantics.md);
[Lock as derived](../explanation/joints/lock-as-derived.md).

### `state-claim-held`

**Condition.** A peer process holds the state-file claim past the wait budget.

**Why.** State files are written under a claim so two processes cannot
interleave writes. The budget exists so a hung peer surfaces as a refusal
rather than an indefinite hang.

**Exits.** Wait for the other process and re-run. If no rwv is running, the
claim is stale: delete the claim file the refusal names, then re-run.

### `surfacing-path-occupied`

**Condition.** Something rwv did not create sits where a surfacing link
belongs. A real file at a lock file's path (`Cargo.lock`, `package-lock.json`,
`pnpm-lock.yaml`, `uv.lock`, `go.work.sum`) is not one: it is the tool's own
write that missed its link, and is taken into the project rather than refused.

**Why.** rwv replaces links it authored, never files it did not. An occupant
with no provenance may be an operator's real file, and overwriting it would be
silent data loss. Every wanted path is read before any link moves, so the
refusal names every occupant at once and leaves the root presenting what it
presented before.

**Exits.** Move or delete the occupant yourself, then re-run the verb that
surfaces it.

**Where this sits.** [Symlinks as structure](../explanation/joints/symlinks-as-structure.md).

### `target-dir-occupied`

**Condition.** The directory slot is taken by an occupant rwv cannot address.

**Why.** Distinct from the `name` kind's `taken`: that occupant is a workweave
rwv could name and retire, this one is not addressable, so there is no verb
that can clear it.

**Exits.** Clear the directory yourself, or choose another with `--dir`.

### `target-diverged-mid-op`

**Condition.** At advance time the target carries commits the current tip does
not.

**Why.** Replay already happened against the target as it was. A target that
moved since means the plan this operation computed no longer describes the
landing, and re-choosing a strategy now would replay against a base the earlier
phases did not use.

**Exits.** `rwv abort` and start again against the target's current state. This
is not `not-fast-forwardable`'s remedy — past replay, a different
strategy is not the fix.

**Where this sits.** [Sync semantics](../explanation/joints/sync-semantics.md).

### `target-lock-behind`

**Condition.** A sync-to target's committed lock is behind its own HEAD.

**Why.** sync-to replays against the target's committed lock. A lock behind
HEAD means those commits would be missing from the replayed tip, and the final
fast-forward could not land.

**Exits.** Run `rwv lock --commit` in the target workspace, then re-run. There
is no `--allow-stale-lock` here: skipping the check does not make
the missing commits appear, which is why this is not `stale-lock`.

**Where this sits.** [Lock as derived](../explanation/joints/lock-as-derived.md).

### `unaccepted-generated-content`

**Condition.** Attested generated files hold content rwv never accepted, and no
consent was given.

**Why.** rwv records a digest when it accepts a generation. Content that does
not match was written by something else, and regenerating over it would
discard an edit nobody recorded.

**Exits.** Inspect the difference. Keep the edit by moving it somewhere rwv
does not own, or accept the regeneration with the consent flag the message
names.

**Where this sits.** [File ownership](../explanation/joints/file-ownership.md).

### `uncompilable-selector`

**Condition.** A `--repo` pattern does not compile.

**Why.** An uncompilable pattern selects nothing, and a repo subset that
silently selects nothing looks like a successful no-op run.

**Exits.** Fix the pattern. The engine's own error is quoted in the message;
that text belongs to the pattern library rather than to rwv.

### `unknown-finding-kind`

**Condition.** `--kind` names nothing in the doctor wire vocabulary.

**Why.** Filtering on an unknown kind would report zero findings, which is
indistinguishable from a clean weave — the most dangerous silent success in the
tool.

**Exits.** The refusal lists the valid kinds, derived from the vocabulary
itself: the kinds `violations` carries and the integration kinds `issues`
carries. [Doctor findings](./doctor-findings.md) has an entry for each.

### `unknown-registry`

**Condition.** `--provider` names a registry rwv does not have.

**Why.** The registry is what turns a path into a clone URL. An unknown one
cannot do that, and defaulting would fetch from somewhere the operator did not
name.

**Exits.** Use one of the built-in registries, or give a full URL so no
registry lookup is needed.

### `unknown-role`

**Condition.** A role value names no known role, including the legacy
spellings.

**Why.** Roles decide what sync and push will do to a repo. An unrecognized
role has no defined behavior, and defaulting one would apply a policy the
manifest did not ask for.

**Exits.** Use one of the values in [Roles](./roles.md). Legacy spellings are
recognized well enough to be rejected by name rather than silently accepted.

### `unknown-verb`

**Condition.** The name is neither a core verb nor an `rwv-<verb>` executable
on `$PATH`.

**Why.** rwv dispatches unknown verbs to plugins by convention. A name that
matches neither is a typo or a missing install, and guessing between them
would run the wrong thing.

**Exits.** `rwv --help` lists core verbs. For a plugin, check it is installed
and named `rwv-<verb>`. `rwv each` is a plugin that every release channel
installs beside `rwv`; refused here, its `rwv-each` is not on `PATH` — see
[rwv each](./rwv-each.md#installation). This is distinct from `no-explain-entry`, which
refuses names that *do* dispatch.

**Where this sits.** [Plugin boundary](../explanation/joints/plugin-boundary.md).

### `unreadable-status`

**Condition.** A dirty-state read failed, so the operation failed closed.

**Why.** The gate asks whether a checkout is clean. When that cannot be
answered, treating it as clean would rebase over unknown state — so an
unreadable repo is refused rather than assumed.

**Exits.** Inspect the named repos directly (`git -C <repo> status`) and fix
what makes them unreadable, then re-run. No commit-or-stash advice is offered
here on purpose: rwv cannot enumerate changes it could not read.

### `unrenderable-name`

**Condition.** A name spells a character the flat rendering reserves.

**Why.** Project and workweave names are rendered into a single flat address.
The excluded characters are the ones that would make two distinct pairs render
the same string, so the address would stop identifying one checkout.

**Exits.** Choose another name. [Formats](./formats.md) lists the excluded
characters and the rule each one protects.

### `unresolvable-lock-entry`

**Condition.** `rwv sync-to` found a lock entry that names a revision this
clone cannot resolve.

**Why.** A lock written against history that was never fetched here, or
rewritten or garbage-collected upstream, pins nothing the clone can compare its
checkout with. The replay cannot tell whether the checkout is ahead of, behind
or diverged from it, so it refuses rather than guess.

**Exits.** `rwv fetch --repo <repo>` brings in the history that may hold the
revision. If it is gone, `rwv member pin <repo> HEAD --commit` pins the checkout's
commit by name, or `rwv member pin <repo> <rev> --commit` pins another.
`rwv status` and `rwv doctor` judge the same condition as [`pin`](./doctor-findings.md#pin)
`unreachable`.

**Where this sits.** [Lock as derived](../explanation/joints/lock-as-derived.md).

### `unresolvable-repo-source`

**Condition.** The source parses as neither a URL nor a registry shorthand
(`<registry>/<owner>/<repo>`, its first segment a built-in registry). For
`rwv member add`, `rwv clone` and `rwv init --adopt` this includes a plain path, relative
or absolute, and a two-segment `<owner>/<repo>`, which names no registry; so does a
member's `url` in `rwv.toml` when `rwv clone`, `rwv update` or `rwv sync` clones
it. For a member, `rwv clone` and `rwv update` name this token after that member's error line
and in its `--json` record's `refusal`, and the run as a whole ends with
`partial-run-aborted`. `rwv doctor` reports every such member as
`provenance` / `unresolvable-url` before any clone is attempted.

**Why.** A plain path could name a remote to clone from or, for `rwv member add`, a
clone already on disk to register, and `<owner>/<repo>` could name a repo on any
registry or a relative path. Guessing between them clones from somewhere
unintended or records the wrong URL, so each meaning has its own spelling.

**Exits.** The message names them. To clone from a local remote, spell it as a
URL: `file:///abs/path`, in `rwv.toml` as well. To register the clone already at that path under the
weave root: `rwv member add --adopt <path>`. To bring up the members of a project already
in this weave, drop the source: `rwv update --project <name>`. Otherwise give a
full URL, or the shorthand with its registry (`github/<owner>/<repo>`; the message
lists the registries). A member `url` holding neither is corrected with
`rwv member set-url <repo> <url>`.

### `unspellable-directory-name`

**Condition.** `rwv member add --adopt <path>` names a clone whose position under the
weave root passes through a directory name no member path can spell: one
holding a `\`, which on Unix is an ordinary filename character, or one that is
not valid Unicode. The refusal names the directory and which of the two it is.

**Why.** The member path `--adopt` records is the clone's position under the
weave root, written with `/` between names. A `\` read as a separator would
record a different directory, and a member path is text, so a name that is not
Unicode has no spelling in it. rwv does not rename the directory itself: any
new name would be a guess.

**Exits.** Rename the directory to a name without `\` that is valid Unicode,
then run `rwv member add --adopt` with the clone's new path. Nothing was written, so
there is nothing to undo.

**Where this sits.** [Formats](./formats.md) states the path grammar.

### `unsupported-vcs-tool`

**Condition.** The version-control tool a backend drives (`git`) is older than
the oldest release that backend runs on, or names no release when asked its
version. The message names the release found and the release required.

**Why.** rwv runs commands and options that older releases lack. Started on
one, a verb would fail partway through, with the tool's own error about an
unknown option, after it had already changed some repositories and not others.
The check runs once, before the verb reads or writes anything. Commands newer
than the required release are detected rather than required: rwv uses another
way to do the same work where the installed tool lacks them. `rwv explain`,
`rwv completions`, `rwv setup claude`, `--help`, `--version` and an external
verb (a plugin) run with any release.

**Exits.** Install the required release or newer and put it first on `PATH`;
`git --version` shows which one `PATH` finds.

**Where this sits.** [Install](../introduction.md#install).

### `untracked-collision`

**Condition.** Untracked files stand where the operation must write.

**Why.** Untracked files are normally harmless — they survive rebase and
fast-forward untouched, which is why dirt checks ignore them. This is the
exception: these specific paths are ones the operation would overwrite, and
they are not in any commit to recover from.

**Exits.** Move or delete the named files, then re-run.

### `unusable-creation-param`

**Condition.** An `rwv member add --new` creation parameter's value cannot be used
as given: a name the registry does not declare, a `--params-json` entry that
is not a JSON string, an empty `root`, a `root` directory that does not
exist, a `root` inside the weave it would create a member of, or a name
supplied through more than one spelling (the address's own three-segment
shorthand, `--param`, and `--params-json` all write into the same map, and a
name given twice is a disagreement about intent rather than a precedence
question a default could answer silently).

**Why.** Each of these would silently create something other than what the
operator asked for — a directory materialized from a typo, a parameter no
registry reads, an upstream the weave itself would walk, delete, and report
on as one of its own members, or a value picked by unstated precedence
between two the operator gave.

**Exits.** Correct the named parameter and re-run. A relative `root` is not
refused: it resolves against the directory rwv runs in (the `-C` or `-w`
directory when one is given), and the messages name the absolute path it
resolved to. For a `root` that does not exist, create it first; for one inside the weave, choose a directory outside
it; for a name given twice, supply it in only one spelling.

### `wrong-address-flag`

**Condition.** An addressing flag was given the other flag's argument shape.

**Why.** `-C` takes a path and `-w` takes a flat address. Each is unambiguous
alone; swapped, one would resolve to nothing and the other to the wrong
checkout. One token covers both directions because it is one mistake seen from
two ends.

**Exits.** `-C <path>` for a directory, `-w <project>--<name>` for a recorded
workweave.

### `wrong-checkout-kind`

**Condition.** The verb is defined for the other kind of checkout.

**Why.** Some verbs act on the primary weave and some on a workweave, because
the state they change lives at one and not the other. Running one in the wrong
place would write state that its own reader never looks for.

**Exits.** `cd` to the checkout the verb is defined at — the message names
which and where it is. `push` is defined at both.

**Where this sits.** [Workweave hierarchy](../explanation/joints/workweave-hierarchy.md).
