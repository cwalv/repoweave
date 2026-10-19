//! Every message that prints an absolute path to a person renders it through
//! `crate::path_spelling::operator_path`.
//!
//! **Why this is structural rather than behavioral.** On Unix `operator_path`
//! is `dunce::simplified(path).to_string_lossy()`, and dunce's UNC test is a
//! `const fn` returning false off Windows — so `simplified` is the identity
//! and `operator_path(p)` is byte-for-byte `p.display()` on every platform the
//! suite runs. That was measured, not assumed: a routed site was reverted and
//! the full release suite ran green through doc-tests. No assertion on output
//! can separate the two spellings here, however many are written, so a
//! behavioral test over this seam would pass with or without the mint and
//! read as coverage while providing none. Reading the source is what is left.
//!
//! **Scope, and therefore the blind spots.** This reads non-comment lines of
//! `src/`, `#[cfg(test)]` items skipped by brace depth, and it judges only the
//! constructs [`ROUTED`] names. It is an inventory of what is routed, not a
//! census of what should be. The several hundred other `.display()` sites in
//! `src/` are outside it, deliberately: whether a `&Path` descends from a
//! canonicalized root is a dataflow question, and no rule reads it off the
//! line, so that set stays open and this test does not pretend otherwise. It
//! sees `src/` and nothing else — a path spelled in `docs/`, a template, or a
//! test fixture is invisible to it, including the needles quoted in this file.
//!
//! What it does catch: a routed site losing its mint, a routed message growing
//! a second path in the other spelling, and a new mint landing without an
//! entry here.
//!
//! A construct is bounded at the render macro it opens on — balanced
//! parentheses, string literals skipped — never at a line count. A fixed-size
//! window is how a neighboring correct call comes to vouch for a mutated one.
//!
//! The bounder has a blind spot of its own: [`paren_delta`] reads each line on
//! its own, so a continuation line of a multi-line string literal is read as
//! code rather than as the prose it is. Unbalanced parentheses there would
//! mis-bound the window. No entry below spans one today; if a future one does,
//! it surfaces as a wrong `renders` count naming that entry, the same
//! direction every other failure here fails in — not as a site going quietly
//! unpinned.

use std::collections::BTreeSet;

mod common;

use common::src_scan::{self, SourceLine};

/// One message that names an absolute path to a person.
struct Routed {
    file: &'static str,
    /// A line that must occur exactly once in `file` and precede `needle`.
    /// Empty where `needle` alone is unique in the file.
    scope: &'static str,
    /// A line inside the construct — its message text, never the
    /// `operator_path` call. Keying on the call would turn a reverted site
    /// into a lookup failure, and the red would name the wrong defect.
    needle: &'static str,
    /// Paths this construct renders through the seam.
    renders: usize,
    /// What it prints, so an entry can be judged without opening the file.
    prints: &'static str,
}

/// The routed set. Findings are reported per entry, and every requirement is
/// stated per entry too — there is no global count standing in for one.
const ROUTED: &[Routed] = &[
    Routed {
        file: "integrations/cargo_workspace.rs",
        scope: "",
        needle: r#""[warning] cargo-workspace: cargo {subcommand} wrote {}, a real file where \"#,
        renders: 2,
        prints: "the cargo hook's notice that its lock landed past the link, naming the \
                 root file and the canonical lock it is not yet taken into",
    },
    Routed {
        file: "activate.rs",
        scope: "fn settle_arrived_drift(",
        needle: r#""\n  {}","#,
        renders: 1,
        prints: "materialize's arrived-drift refusal, listing each file it would act on",
    },
    Routed {
        file: "activate.rs",
        scope: "fn quoted_link_target(",
        needle: r#""`{}`","#,
        renders: 1,
        prints: "the target of an undeclared surfacing link, as doctor and materialize report it",
    },
    Routed {
        file: "activate.rs",
        scope: "",
        needle: r#"cd to primary ({}) and rerun.","#,
        renders: 1,
        prints: "activate's refusal in a workweave, naming the primary to cd to",
    },
    Routed {
        file: "activate.rs",
        scope: "fn withhold_hooks_over_unsettled_drift(",
        needle: r#""\n  {}","#,
        renders: 1,
        prints: "the withheld-hooks notice, listing the same files as the refusal above",
    },
    Routed {
        file: "activate.rs",
        scope: "fn reset_ledger_no_generator_rebuilt(",
        needle: r#""[reset] core: {} did not parse"#,
        renders: 1,
        prints: "materialize's notice that it emptied an unreadable owned-generation \
                 record nothing here could rebuild",
    },
    Routed {
        file: "check.rs",
        scope: "",
        needle: r#"`.rwv-active` here (the marker is left alone)","#,
        renders: 1,
        prints: "doctor: a weave root carrying both `.rwv-active` and a registered marker",
    },
    Routed {
        file: "check.rs",
        scope: "",
        needle: r#"`--fix` does not touch the pointer until the marker is repaired","#,
        renders: 1,
        prints: "doctor: the same conflict with the marker unverifiable",
    },
    Routed {
        file: "check.rs",
        scope: "",
        needle: r#"wrong by hand","#,
        renders: 1,
        prints: "doctor: the same conflict with nothing witnessing which file is the stray",
    },
    Routed {
        file: "check.rs",
        scope: "",
        needle: "run `rwv doctor --fix` to re-point parent to primary\",",
        renders: 2,
        prints: "doctor: a marker `parent` pointing at a directory that does not exist",
    },
    Routed {
        file: "check.rs",
        scope: "",
        needle: r#""{}: workweave parent-chain anomaly: {}","#,
        renders: 1,
        prints: "doctor: a workweave parent chain that does not resolve",
    },
    Routed {
        file: "check.rs",
        scope: "",
        needle: r#""{}: directory under workweaves parent has no `.rwv-workweave` marker","#,
        renders: 1,
        prints: "doctor: a directory under the workweaves parent with no marker",
    },
    Routed {
        file: "check.rs",
        scope: "",
        needle: r#"this workweave may have been copied from another machine","#,
        renders: 2,
        prints: "doctor: a marker `primary` naming a different workspace",
    },
    Routed {
        file: "check.rs",
        scope: "",
        needle: r#""{}: belongs to workspace `{}`; not this workspace's to manage","#,
        renders: 2,
        prints: "doctor: a workweave another workspace owns",
    },
    Routed {
        file: "check.rs",
        scope: "",
        needle: r#"to prune","#,
        renders: 1,
        prints: "doctor: a registry entry recorded at a path that no longer holds it",
    },
    Routed {
        file: "check.rs",
        scope: "",
        needle: "run `rwv doctor --fix` to adopt it\",",
        renders: 1,
        prints: "doctor: a workweave on disk that the index does not record",
    },
    Routed {
        file: "check.rs",
        scope: "",
        needle: r#"`.git/info/exclude`, so it needs no `.gitignore` entry","#,
        renders: 2,
        prints: "doctor: a tracked workweave index, and the `git rm --cached` that untracks it",
    },
    Routed {
        file: "check.rs",
        scope: "",
        needle: r#"worktrees inside it","#,
        renders: 1,
        prints: "doctor: a legacy nested workweave, with the retire-and-recreate remedy",
    },
    Routed {
        file: "check.rs",
        scope: "",
        needle: r#"the recorded identity","#,
        renders: 1,
        prints: "doctor: a workweave directory name disagreeing with its records, rename known",
    },
    Routed {
        file: "check.rs",
        scope: "",
        needle: r#"{detail}. Rename it to the `<project>--<name>` you intend, \"#,
        renders: 1,
        prints: "doctor: the same disagreement with no name to rename to",
    },
    Routed {
        file: "cli/dispatch.rs",
        scope: "Some(Commands::Resolve) => {",
        needle: r#""{}","#,
        renders: 1,
        prints: "`rwv resolve` — the whole of its stdout",
    },
    Routed {
        file: "integrations/merge.rs",
        scope: "",
        needle: r#""{name} managed file present but unmarked: {}; \"#,
        renders: 1,
        prints: "doctor: a managed file in USER-HELD state",
    },
    Routed {
        file: "integrations/merge.rs",
        scope: "",
        needle: r#""{name} managed file has drift: {}; {drift_detail} \"#,
        renders: 1,
        prints: "doctor: a managed file in DRIFT state",
    },
    Routed {
        file: "integrations/merge.rs",
        scope: "",
        needle: r#""{name} managed file missing: {}; run rwv doctor --fix to regenerate","#,
        renders: 1,
        prints: "doctor: a managed file in MISSING state",
    },
    Routed {
        file: "integrations/merge.rs",
        scope: "",
        needle: r#""{name} managed file has drift: {}; {detail} \"#,
        renders: 1,
        prints: "doctor: a marked region no current membership justifies",
    },
    Routed {
        file: "integrations/merge.rs",
        scope: "",
        needle: r#""{name} generated file has drift: {} ({detail}); \"#,
        renders: 1,
        prints: "doctor: a fully-owned generated file that is unreadable, unparseable, or not what rwv would author",
    },
    Routed {
        file: "integrations/merge.rs",
        scope: "",
        needle: r#""refusing to write {}: the content generated for it does not parse ({e:#}); \"#,
        renders: 1,
        prints: "an authoring pass refusing to write a managed file that would not parse",
    },
    Routed {
        file: "op_state.rs",
        scope: "",
        needle: "Rerun with `{resume}` from that workspace after resolving, \\",
        renders: 1,
        prints: "the in-flight-op refusal, owner-record arm",
    },
    Routed {
        file: "op_state.rs",
        scope: "",
        needle: r#"workspace ({dir}) is leased to it. Owner workspace: {owner}.\n\"#,
        renders: 2,
        prints: "the same refusal from a leased workspace, owner record resolvable",
    },
    Routed {
        file: "op_state.rs",
        scope: "",
        needle: r#""op {id} in progress (lease at {dir}; owner workspace: {owner}).\n\"#,
        renders: 2,
        prints: "the same refusal with a dangling lease pointer",
    },
    Routed {
        file: "owned_state.rs",
        scope: "",
        needle: r#""another rwv still holds the owned-digest ledger of {dir} \"#,
        renders: 2,
        prints: "the owned-digest ledger claim refusal, naming the directory and the \
                 claim file to delete",
    },
    Routed {
        file: "owned_state.rs",
        scope: "",
        needle: r#""failed to release the claim on the owned-digest ledger of {dir}. \"#,
        renders: 2,
        prints: "the error an edit returns when its ledger claim could not be removed, \
                 naming the directory and the claim file to delete",
    },
    Routed {
        file: "workweave_index.rs",
        scope: "",
        needle: r#""another rwv still holds the workweave index of project \"#,
        renders: 2,
        prints: "the workweave-index claim refusal, naming the primary root and \
                 the claim file to delete",
    },
    Routed {
        file: "workweave_index.rs",
        scope: "",
        needle: r#""failed to release the claim on the workweave index of project \"#,
        renders: 2,
        prints: "the error an edit returns when its index claim could not be removed, \
                 naming the primary root and the claim file to delete",
    },
    Routed {
        file: "push.rs",
        scope: "",
        needle: r#"None => format!("at {}","#,
        renders: 1,
        prints: "`rwv push`'s refusal, naming an unrecorded workweave by its directory",
    },
    Routed {
        file: "status.rs",
        scope: "",
        needle: r#""{verb} in progress (op {id}, mid `{phase}`, started {elapsed} ago) at {owner}","#,
        renders: 1,
        prints: "status's op header",
    },
    Routed {
        file: "sync.rs",
        scope: "",
        needle: r#"problem: {summary}. Fix it there too, separately, before this converges:\n\"#,
        renders: 1,
        prints: "the replay-exclusion note naming the rebase base as also broken",
    },
    Routed {
        file: "sync.rs",
        scope: "",
        needle: "To fix: run `rwv doctor --fix` there:\\n\\",
        renders: 1,
        prints: "the refusal for a clean cwd whose rebase base is not",
    },
    Routed {
        file: "workspace.rs",
        scope: "",
        needle: r#""the workweave at {} carries a `.rwv-workweave` marker for project `{}`, \"#,
        renders: 1,
        prints: "the refusal to name an unregistered workweave",
    },
    Routed {
        file: "workspace.rs",
        scope: "",
        needle: r#""{} already exists — the filesystem lists it as `{listed}` and treats the \"#,
        renders: 1,
        prints: "the occupant sentence where the filesystem spells the name differently",
    },
    Routed {
        file: "workspace.rs",
        scope: "",
        needle: r#""{} already exists","#,
        renders: 1,
        prints: "the occupant sentence otherwise",
    },
    Routed {
        file: "workspace.rs",
        scope: "",
        needle: r#""target: workspace {} · project {} (.rwv-active)","#,
        renders: 1,
        prints: "the target line printed before a project-scoped verb acts",
    },
    Routed {
        file: "workspace.rs",
        scope: "pub fn display(&self) -> String {",
        needle: r#""Weave: {}","#,
        renders: 1,
        prints: "the context display's `Weave:` line, primary arm",
    },
    Routed {
        file: "workspace.rs",
        scope: "",
        needle: r#""Workweave: {}","#,
        renders: 1,
        prints: "the context display's `Workweave:` line",
    },
    Routed {
        file: "workspace.rs",
        scope: "Checkout::Workweave { name, dir, .. } => {",
        needle: r#""Weave: {}","#,
        renders: 1,
        prints: "the context display's `Weave:` line, workweave arm",
    },
    Routed {
        file: "workweave.rs",
        scope: "",
        needle: r#""workweave `{name}` of project `{project}` is registered at {registered}, \"#,
        renders: 2,
        prints: "the delete refusal naming the registered directory and the one reached",
    },
    Routed {
        file: "workweave.rs",
        scope: "",
        needle: r#""workweave {dir} holds {n} member(s) as a symlink, a shape rwv no longer operates on:\n  \"#,
        renders: 1,
        prints: "the refusal on a workweave holding a symlinked member, naming where to run doctor",
    },
    Routed {
        file: "workweave.rs",
        scope: "",
        needle: r#""{which} checkout missing at {}; run `rwv fetch` in that workweave to re-materialize it","#,
        renders: 1,
        prints: "workweave log's note for a workweave or parent checkout that is not on disk",
    },
    Routed {
        file: "workweave.rs",
        scope: "",
        needle: r#""failed to move the full clone at {} aside to {}","#,
        renders: 2,
        prints: "the standalone repair's failure to rename the clone aside, naming both paths",
    },
    Routed {
        file: "workweave.rs",
        scope: "",
        needle: r#""created the worktree for {repo_path}, but the full clone it replaced is \"#,
        renders: 1,
        prints: "the standalone repair's failure to remove the replaced clone, naming where it stands",
    },
    Routed {
        file: "workweave.rs",
        scope: "",
        needle: r#""the full clone of {repo_path} is left at {} (moving it back failed: {restore})","#,
        renders: 1,
        prints: "the standalone repair's failed birth whose move-back also failed, naming where the clone is",
    },
    Routed {
        file: "workweave.rs",
        scope: "",
        needle: r#""the full clone of {repo_path} is left at {}","#,
        renders: 1,
        prints: "the standalone repair's failed birth after something took the slot, naming where the clone is",
    },
    Routed {
        file: "workweave.rs",
        scope: "",
        needle: r#""{} already exists, and the clone is moved there while its worktree is \"#,
        renders: 1,
        prints: "the standalone repair's reason that the aside path is taken",
    },
    Routed {
        file: "workweave.rs",
        scope: "",
        needle: r#""there is no canonical clone at {} to link a worktree into; \"#,
        renders: 1,
        prints: "the standalone repair's reason that the canonical clone is missing",
    },
    Routed {
        file: "workweave.rs",
        scope: "",
        needle: r#""the canonical slot {} is itself a worktree of {}, not a full clone to link \"#,
        renders: 2,
        prints: "the standalone repair's reason that the canonical slot is a worktree, naming its store",
    },
    Routed {
        file: "workweave.rs",
        scope: "",
        needle: r#""these refs hold commits no ref of the canonical clone at {} reaches; land \"#,
        renders: 1,
        prints: "the standalone repair's reason that refs hold commits the canonical clone does not reach",
    },
    Routed {
        file: "workweave.rs",
        scope: "",
        needle: r#""not replacing the full clone at {} with a worktree of the canonical clone:\n  - {}\n\n\"#,
        renders: 1,
        prints: "the standalone repair's refusal, naming the clone it left",
    },
    Routed {
        file: "git.rs",
        scope: "",
        needle: r#""`git -C {at} clean -X -d -n` lists what would go, and \"#,
        renders: 1,
        prints: "the commands that preview and delete a workspace's ignored files, for an operator to run",
    },
    Routed {
        file: "workweave.rs",
        scope: "",
        needle: r#""workweave created at {}; only its install hooks did not complete. Fix \"#,
        renders: 1,
        prints: "create's failure at the install hooks, naming the workweave it kept",
    },
    Routed {
        file: "integrations/cargo_workspace.rs",
        scope: "",
        needle: r#""[warning] cargo-workspace: the rustc-wrapper key was not fitted to this \"#,
        renders: 1,
        prints: "the notice that a symlinked `.cargo` path kept the wrapper key from being \
                 fitted, naming the link",
    },
    Routed {
        file: "add_remove.rs",
        scope: "",
        needle: r#""rwv add: canonical clone at {} has no commits yet; \"#,
        renders: 1,
        prints: "create_worktree_in_workweave's notice that the canonical clone has no \
                 commits yet, naming it",
    },
    Routed {
        file: "add_remove.rs",
        scope: "",
        needle: r#""failed to create worktree at {} from canonical clone {}","#,
        renders: 2,
        prints: "the worktree-creation failure, naming the worktree destination and the \
                 canonical clone it came from",
    },
    Routed {
        file: "add_remove.rs",
        scope: "pub(crate) fn settle_manifest(",
        needle: r#""failed to load manifest at {}","#,
        renders: 1,
        prints: "settle_manifest's own manifest-load failure",
    },
    Routed {
        file: "add_remove.rs",
        scope: "",
        needle: r#""the regeneration failed, and these files could not be put back as they \"#,
        renders: 1,
        prints: "settle_manifest's failed-regeneration notice, listing files the restore \
                 could not put back",
    },
    Routed {
        file: "add_remove.rs",
        scope: "",
        needle: r#""creation parameter '{}' resolves to '{}', which is not valid UTF-8","#,
        renders: 1,
        prints: "add --new's refusal of a relative path parameter whose resolved path is \
                 not UTF-8, naming that path",
    },
    Routed {
        file: "add_remove.rs",
        scope: "pub fn run_add(",
        needle: r#""failed to load manifest at {}","#,
        renders: 1,
        prints: "run_add's own manifest-load failure",
    },
    Routed {
        file: "add_remove.rs",
        scope: "pub fn run_add(",
        needle: r#""Directory already exists at '{}', skipping clone","#,
        renders: 1,
        prints: "run_add's notice that the clone destination is already occupied",
    },
    Routed {
        file: "add_remove.rs",
        scope: "pub fn run_add(",
        needle: r#""failed to determine default branch for {}""#,
        renders: 1,
        prints: "run_add's failure to read the freshly cloned remote's default branch",
    },
    Routed {
        file: "add_remove.rs",
        scope: "pub fn run_add(",
        needle: r#""rwv add: '{}' at {}: {}, then re-run `rwv add {}`","#,
        renders: 1,
        prints: "run_add's refusal when the remote has no default branch to read",
    },
    Routed {
        file: "add_remove.rs",
        scope: "fn refuse_foreign_clone_at_placement(",
        needle: r#""could not determine the {} URL for '{}'","#,
        renders: 1,
        prints: "the foreign-clone-at-placement check's failure to read the existing \
                 clone's remote URL",
    },
    Routed {
        file: "add_remove.rs",
        scope: "",
        needle: r#""rwv add --adopt: no clone at '{}'. --adopt registers a clone already under \"#,
        renders: 1,
        prints: "run_add_adopt's refusal when nothing git-shaped is at the adopt path",
    },
    Routed {
        file: "add_remove.rs",
        scope: "fn make_dir_all(",
        needle: r#"format!("failed to create directory {}""#,
        renders: 1,
        prints: "AddLeftovers::make_dir_all's failure to create a directory it is about to \
                 track",
    },
    Routed {
        file: "add_remove.rs",
        scope: "",
        needle: r#""removed {}, which it had created","#,
        renders: 1,
        prints: "AddLeftovers::refused's take-back line for a directory or repo this add \
                 created",
    },
    Routed {
        file: "add_remove.rs",
        scope: "",
        needle: r#""{} was created by it and could not be removed ({e:#}); delete it by hand","#,
        renders: 1,
        prints: "AddLeftovers::refused's line when taking back a creation fails",
    },
    Routed {
        file: "add_remove.rs",
        scope: "",
        needle: r#""kept the worktree it made at {}: `{rerun}` reuses it, and \"#,
        renders: 1,
        prints: "AddLeftovers::refused's line naming a kept workweave worktree",
    },
    Routed {
        file: "add_remove.rs",
        scope: "",
        needle: r#""kept the clone it made at {}: `{rerun}` reuses it, and \"#,
        renders: 1,
        prints: "AddLeftovers::refused's line naming a kept canonical clone",
    },
    Routed {
        file: "add_remove.rs",
        scope: "fn clone_into(",
        needle: r#"format!("failed to create directory {}""#,
        renders: 1,
        prints: "clone_into's failure to create the clone destination's parent directory",
    },
    Routed {
        file: "add_remove.rs",
        scope: "fn clone_into(",
        needle: r#""failed to clone '{}' into {}","#,
        renders: 1,
        prints: "clone_into's clone failure, naming the URL and the destination",
    },
    Routed {
        file: "add_remove.rs",
        scope: "fn run_add_from_local_path(",
        needle: r#""could not determine the {} URL for '{}'","#,
        renders: 1,
        prints: "run_add_from_local_path's failure to read the adopted clone's remote URL",
    },
    Routed {
        file: "add_remove.rs",
        scope: "",
        needle: r#""'{}' has no {} remote to read a URL from","#,
        renders: 1,
        prints: "the no-remote-URL refusal for a clone with nothing to adopt a URL from",
    },
    Routed {
        file: "add_remove.rs",
        scope: "fn run_add_from_local_path(",
        needle: r#""failed to load manifest at {}","#,
        renders: 1,
        prints: "run_add_from_local_path's own manifest-load failure",
    },
    Routed {
        file: "add_remove.rs",
        scope: "fn run_add_from_local_path(",
        needle: r#""failed to determine default branch for {}","#,
        renders: 1,
        prints: "run_add_from_local_path's failure to read the adopted clone's default \
                 branch",
    },
    Routed {
        file: "add_remove.rs",
        scope: "fn run_add_from_local_path(",
        needle: r#""rwv add: '{}' at {}: {}, then re-run `rwv add --adopt {}`","#,
        renders: 1,
        prints: "run_add_from_local_path's refusal when the adopted clone has no default \
                 branch",
    },
    Routed {
        file: "add_remove.rs",
        scope: "pub fn run_remove(",
        needle: r#""failed to load manifest at {}","#,
        renders: 1,
        prints: "run_remove's own manifest-load failure",
    },
    Routed {
        file: "add_remove.rs",
        scope: "",
        needle: r#"eprintln!("Deleted '{}'","#,
        renders: 1,
        prints: "run_remove's confirmation naming the checkout it deleted",
    },
    Routed {
        file: "add_remove.rs",
        scope: "",
        needle: r#""'{}' is out of this workweave, but the branch the workweave held in \"#,
        renders: 1,
        prints: "run_remove's failure to delete the branch a workweave held, naming the \
                 canonical clone it is in",
    },
    Routed {
        file: "add_remove.rs",
        scope: "",
        needle: r#""warning: kept branch `{owned}` in {}: it holds commits the clone's \"#,
        renders: 1,
        prints: "run_remove's notice that a workweave's branch holding commits stays, naming \
                 the canonical clone it stays in",
    },
    Routed {
        file: "add_remove.rs",
        scope: "",
        needle: r#"format!("failed to load lock at {}""#,
        renders: 1,
        prints: "leftover_checkout_vcs's failure to read rwv.lock",
    },
    Routed {
        file: "add_remove.rs",
        scope: "",
        needle: r#""live worktree registered at {}","#,
        renders: 1,
        prints: "refuse_claimed_store's claim line naming a live worktree against the store",
    },
    Routed {
        file: "add_remove.rs",
        scope: "",
        needle: r#""refusing to delete '{}': it is a repo whose worktree registrations \"#,
        renders: 1,
        prints: "refuse_claimed_store's refusal when worktree registrations cannot be read",
    },
    Routed {
        file: "add_remove.rs",
        scope: "",
        needle: r#""refusing to delete '{}': the ownership receipts for project `{}` could \"#,
        renders: 1,
        prints: "refuse_claimed_store's refusal when a project's ownership receipts cannot \
                 be read",
    },
    Routed {
        file: "add_remove.rs",
        scope: "",
        needle: r#""refusing to delete '{}': the store is still claimed:\n  {}\n\n\"#,
        renders: 1,
        prints: "refuse_claimed_store's refusal naming the store with outstanding claims",
    },
    Routed {
        file: "add_remove.rs",
        scope: "pub fn run_add_new(",
        needle: r#""failed to load manifest at {}","#,
        renders: 1,
        prints: "run_add_new's own manifest-load failure",
    },
    Routed {
        file: "add_remove.rs",
        scope: "",
        needle: r#""the root '{}' does not exist; rwv creates '<root>/<owner>/<repo>' but \"#,
        renders: 1,
        prints: "run_add_new's refusal when the `--param root=` directory does not exist",
    },
    Routed {
        file: "add_remove.rs",
        scope: "",
        needle: r#"format!("failed to resolve root {}""#,
        renders: 1,
        prints: "run_add_new's failure to canonicalize the `--param root=` directory",
    },
    Routed {
        file: "add_remove.rs",
        scope: "",
        needle: r#""failed to resolve weave root {}","#,
        renders: 1,
        prints: "run_add_new's failure to canonicalize the primary weave root for the \
                 inside-the-weave check",
    },
    Routed {
        file: "add_remove.rs",
        scope: "",
        needle: r#""the root '{}' is inside the weave at '{}'; the upstream it would \"#,
        renders: 2,
        prints: "run_add_new's refusal when the creation root sits inside the weave it \
                 would back",
    },
    Routed {
        file: "add_remove.rs",
        scope: "",
        needle: r#""failed to init bare upstream at {}","#,
        renders: 1,
        prints: "run_add_new's failure to `git init --bare` the upstream",
    },
    Routed {
        file: "add_remove.rs",
        scope: "pub fn run_add_new(",
        needle: r#""Directory already exists at '{}', skipping clone","#,
        renders: 1,
        prints: "run_add_new's notice that the member destination is already occupied \
                 (InitBareAt arm)",
    },
    Routed {
        file: "add_remove.rs",
        scope: "pub fn run_add_new(",
        needle: r#""failed to clone '{}' into {}","#,
        renders: 1,
        prints: "run_add_new's clone failure, naming the upstream URL and the member \
                 destination",
    },
    Routed {
        file: "add_remove.rs",
        scope: "",
        needle: r#""Directory already exists at '{}', skipping init","#,
        renders: 1,
        prints: "run_add_new's notice that the member destination is already occupied \
                 (Named arm)",
    },
    Routed {
        file: "add_remove.rs",
        scope: "",
        needle: r#"format!("failed to init repo at {}""#,
        renders: 1,
        prints: "run_add_new's failure to `git init` the member (Named arm)",
    },
    Routed {
        file: "add_remove.rs",
        scope: "",
        needle: r#"format!("failed to add the remote in {}""#,
        renders: 1,
        prints: "run_add_new's failure to add the upstream remote to the freshly inited \
                 member",
    },
    Routed {
        file: "add_remove.rs",
        scope: "",
        needle: r#""rwv add: repo at {} has a detached HEAD right after creation; \"#,
        renders: 2,
        prints: "run_add_new's refusal when the member it found already at the placement \
                 has a detached HEAD",
    },
    Routed {
        file: "add_remove.rs",
        scope: "",
        needle: r#""rwv add: the clone at {} has a detached HEAD right after creation, \"#,
        renders: 2,
        prints: "run_add_new's refusal when the member it cloned takes a detached HEAD from \
                 the upstream, naming the clone and the upstream",
    },
    Routed {
        file: "add_remove.rs",
        scope: "",
        needle: r#""[warning] add: clone {} is already registered by project '{}' with role {}; \"#,
        renders: 1,
        prints: "warn_if_shared_clone's notice naming the shared clone",
    },
];

/// The macros a message is rendered by. A construct is one of these calls.
const RENDER_MACROS: &[&str] = &[
    "format!(",
    "bail!(",
    "refuse!(",
    "anyhow!(",
    "eprintln!(",
    "println!(",
    "write!(",
    "writeln!(",
    "panic!(",
];

/// Net parenthesis depth across `line`, string and char literals skipped.
///
/// An unterminated literal ends the count where it opens: the rest of the line
/// is prose, and its parentheses are text. A continuation line of a multi-line
/// literal is read as code, so prose there with unbalanced parentheses would
/// mis-bound a window — which shows up as a wrong `renders` count rather than
/// as silence, because every entry states its own.
fn paren_delta(line: &str) -> i32 {
    let b = line.as_bytes();
    let mut depth = 0i32;
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'r' if i + 1 < b.len() && (b[i + 1] == b'"' || b[i + 1] == b'#') => {
                let mut j = i + 1;
                let mut hashes = 0;
                while j < b.len() && b[j] == b'#' {
                    hashes += 1;
                    j += 1;
                }
                if j < b.len() && b[j] == b'"' {
                    let closer = format!("\"{}", "#".repeat(hashes));
                    match line[j + 1..].find(&closer) {
                        Some(rel) => i = j + 1 + rel + closer.len(),
                        None => return depth,
                    }
                } else {
                    i += 1;
                }
            }
            b'"' => {
                let mut j = i + 1;
                loop {
                    if j >= b.len() {
                        return depth;
                    }
                    if b[j] == b'\\' {
                        j += 2;
                        continue;
                    }
                    if b[j] == b'"' {
                        break;
                    }
                    j += 1;
                }
                i = j + 1;
            }
            b'\'' => {
                if i + 2 < b.len() && b[i + 2] == b'\'' {
                    i += 3;
                } else if i + 3 < b.len() && b[i + 1] == b'\\' && b[i + 3] == b'\'' {
                    i += 4;
                } else {
                    i += 1;
                }
            }
            b'(' => {
                depth += 1;
                i += 1;
            }
            b')' => {
                depth -= 1;
                i += 1;
            }
            _ => i += 1,
        }
    }
    depth
}

/// The index one past the render-macro call opening at `start`, `col` bytes in.
fn call_end(lines: &[&SourceLine], start: usize, col: usize) -> usize {
    let mut depth = paren_delta(&lines[start].text[col..]);
    if depth <= 0 {
        return start;
    }
    for (offset, line) in lines.iter().enumerate().skip(start + 1) {
        depth += paren_delta(&line.text);
        if depth <= 0 {
            return offset;
        }
    }
    lines.len() - 1
}

/// The innermost render-macro call containing `at`.
///
/// Found by walking back from a line inside the construct, never forward from
/// one before it: backward from a point inside a call lands in that call or in
/// one enclosing it, and the containment test below rejects anything else. A
/// forward scan can walk into the next arm, which is how a neighboring correct
/// call comes to vouch for a mutated one.
fn enclosing_call(lines: &[&SourceLine], at: usize) -> Option<(usize, usize)> {
    for back in (0..=at).rev() {
        for macro_name in RENDER_MACROS {
            let Some(col) = lines[back].text.rfind(macro_name) else {
                continue;
            };
            let end = call_end(lines, back, col + macro_name.len() - 1);
            if back <= at && at <= end {
                return Some((back, end));
            }
        }
    }
    None
}

/// Uses of `operator_path`, never mentions: the definition is excluded by name,
/// and a doc-comment reference (`[\`operator_path\`]`) carries no `(` to match.
fn operator_path_uses(text: &str) -> usize {
    text.lines()
        .filter(|l| !l.contains("fn operator_path("))
        .map(|l| l.matches("operator_path(").count())
        .sum()
}

fn unminted_renders(text: &str) -> usize {
    text.matches(".display()").count()
}

fn production_lines_of(file: &str) -> Vec<SourceLine> {
    src_scan::production_lines()
        .into_iter()
        .filter(|l| l.file == file)
        .collect()
}

#[test]
fn every_routed_operator_render_goes_through_the_seam() {
    let mut failures = Vec::new();
    let mut located = BTreeSet::new();
    let mut table_renders = 0usize;

    for entry in ROUTED {
        let owned = production_lines_of(entry.file);
        let lines: Vec<&SourceLine> = owned.iter().collect();
        if lines.is_empty() {
            failures.push(format!(
                "{}: the scan yielded no production lines; its walk is broken, not the \
                 tree clean",
                entry.file
            ));
            continue;
        }

        let mut from = 0;
        if !entry.scope.is_empty() {
            let hits: Vec<usize> = (0..lines.len())
                .filter(|&i| lines[i].text.contains(entry.scope))
                .collect();
            if hits.len() != 1 {
                failures.push(format!(
                    "{}: scope `{}` occurs {} time(s), needs exactly one to name a region",
                    entry.file,
                    entry.scope,
                    hits.len()
                ));
                continue;
            }
            from = hits[0];
        }

        let hits: Vec<usize> = (from..lines.len())
            .filter(|&i| lines[i].text.contains(entry.needle))
            .collect();
        if hits.is_empty() {
            failures.push(format!(
                "{}: nothing matches `{}` — the message moved or was rewritten, so the \
                 entry no longer names anything. It printed: {}",
                entry.file, entry.needle, entry.prints
            ));
            continue;
        }
        if entry.scope.is_empty() && hits.len() != 1 {
            failures.push(format!(
                "{}: `{}` occurs {} times with no scope to separate them; give the entry \
                 a scope line or the pin cannot say which construct it means",
                entry.file,
                entry.needle,
                hits.len()
            ));
            continue;
        }

        let Some((start, end)) = enclosing_call(&lines, hits[0]) else {
            failures.push(format!(
                "{}:{}: no render macro encloses `{}`",
                entry.file, lines[hits[0]].line, entry.needle
            ));
            continue;
        };

        let site = format!("{}:{}-{}", entry.file, lines[start].line, lines[end].line);
        if !located.insert(site.clone()) {
            failures.push(format!(
                "{site}: two entries resolve to one construct, so one of them is vouched \
                 for by the other's mint rather than by its own"
            ));
            continue;
        }

        let body: String = lines[start..=end]
            .iter()
            .map(|l| l.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        table_renders += entry.renders;

        let uses = operator_path_uses(&body);
        if uses != entry.renders {
            failures.push(format!(
                "{site}: {uses} render(s) go through `operator_path`, {} expected.\n  \
                 Prints: {}\n{body}",
                entry.renders, entry.prints
            ));
        }
        let raw = unminted_renders(&body);
        if raw != 0 {
            failures.push(format!(
                "{site}: {raw} path(s) still rendered by `.display()` in a message that \
                 mints the rest — one message, two spellings.\n  Prints: {}\n{body}",
                entry.prints
            ));
        }
    }

    // The floor. It is derived from the tree, and the defect this pin guards
    // moves the tree side DOWN — un-minting a site lowers the count while the
    // table stays put — so no violation can inflate the number the floor is
    // calibrated against.
    let tree_uses: usize = operator_path_uses(
        &src_scan::production_lines()
            .iter()
            .map(|l| l.text.as_str())
            .collect::<Vec<_>>()
            .join("\n"),
    );
    if tree_uses != table_renders {
        failures.push(format!(
            "`src/` holds {tree_uses} use(s) of `operator_path`, this table accounts for \
             {table_renders}. A mint outside the table is a message no entry pins; a \
             shortfall is a site that lost its mint. Reconcile — do not adjust the \
             number."
        ));
    }

    assert!(
        failures.is_empty(),
        "an absolute path printed to a person is spelled by \
         `crate::path_spelling::operator_path`, never by `Display`. Off Windows the two \
         are the same bytes, so nothing but this test can tell them apart.\n\n{}",
        failures.join("\n\n")
    );
}

/// The scan reaches its corpus, bounds a call at the call, and counts uses
/// rather than mentions.
///
/// A source walk that yields nothing is indistinguishable, when green, from one
/// that found everything — and each assertion here is a way this instrument has
/// been wrong before or could quietly become vacuous.
#[test]
fn the_scan_reaches_its_corpus_and_bounds_what_it_reads() {
    let lines = src_scan::production_lines();
    for file in ROUTED.iter().map(|r| r.file).collect::<BTreeSet<_>>() {
        assert!(
            lines.iter().any(|l| l.file == file),
            "the scan yielded no production lines for {file}; its walk is broken, not \
             the tree clean"
        );
    }

    assert_eq!(paren_delta("format!("), 1);
    assert_eq!(paren_delta("foo(bar())"), 0);
    assert_eq!(
        paren_delta(r#"    "run `rwv doctor --fix` (twice)","#),
        0,
        "parentheses inside a string literal are prose, not depth"
    );
    assert_eq!(
        paren_delta(r#"    "an unterminated literal ( opens here \"#),
        0,
        "a `(` after an unterminated literal is prose too"
    );
    assert_eq!(
        paren_delta(r#"    let c = matches('(');"#),
        0,
        "a parenthesis in a char literal is not depth"
    );

    assert_eq!(
        operator_path_uses("pub fn operator_path(path: &Path) -> String {"),
        0,
        "the definition is not a call site"
    );
    assert_eq!(
        operator_path_uses("//! - [`operator_path`] answers a **person**"),
        0,
        "a doc-comment mention is not a call site"
    );
    assert_eq!(
        operator_path_uses("dir = crate::path_spelling::operator_path(dir),"),
        1
    );

    // The seam's own module still defines what every entry above routes
    // through; a rename there must not leave this test quietly counting zero.
    assert!(
        src_scan::src_dir().join("path_spelling.rs").exists(),
        "the seam's module is gone; this pin's needle counts nothing"
    );
    assert!(
        ROUTED.len() >= 30,
        "the routed set has collapsed to {} entries — a table this pin can satisfy \
         trivially is not a pin",
        ROUTED.len()
    );
}
