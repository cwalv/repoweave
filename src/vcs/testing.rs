//! A [`Vcs`] backed by an in-memory ref store, for arms a real repo cannot
//! reach.
//!
//! Two things a script of return values cannot express, and both are why this
//! exists:
//!
//! - **A window.** A caller that classifies refs and then acts on the
//!   classification has an interval between the two calls. Handing the second
//!   call a canned verdict asserts the effect and skips the cause. Here the
//!   verdict is derived from the ref store, and [`FakeVcs::before_next`] runs
//!   a caller-supplied hook inside the interval, so a test states what happened
//!   in the window and the double works out the rest.
//! - **A failure the operating system will not produce on demand.** A ref
//!   deletion that fails, a branch listing that errors: reachable through
//!   [`FakeVcs::fail_next`] and otherwise only through permission games that
//!   depend on who is running the suite.
//!
//! Only the methods some uncovered arm needed are modeled. Every other method
//! panics naming itself: a method nobody has needed yet is one whose faithful
//! fake behavior nobody has thought about, and guessing quietly is how a
//! double starts lying.

use super::*;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Mutex;

/// The trait methods [`FakeVcs`] models, and the key a script or a hook is
/// registered under.
///
/// Named for the *required* method rather than the provided wrapper above it:
/// `create_worktree_on` carries the found branch but delegates the VCS work,
/// so that delegation is what a fake can intercept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum VcsCall {
    IsAncestor,
    ListBranchNamesWithPrefix,
    ResolveLocalBranchTip,
    MaterializeWorktreeOnRef,
    MaterializeUnpopulatedWorktreeOnRef,
    DeleteBranch,
    HoldsCommittedContent,
}

type Hook = Box<dyn FnOnce(&FakeVcs) + Send>;

/// A fake [`Vcs`] over a map of `(store, ref name) -> tip`.
///
/// Dropping one with a scripted failure or hook still queued panics: a script
/// nothing consumed means the call it was written for never happened, and the
/// assertions that ran did so against a path the test did not intend.
///
/// State sits behind [`Mutex`] rather than `RefCell` because [`Vcs`] is
/// `Send + Sync`. `Mutex` is not reentrant, so no guard may be held while a
/// hook runs — see [`FakeVcs::locks_all_free`].
#[derive(Default)]
pub(crate) struct FakeVcs {
    branches: Mutex<BTreeMap<(PathBuf, String), ResolvedRevisionId>>,
    ancestries: Mutex<BTreeSet<(String, String)>>,
    failures: Mutex<BTreeMap<VcsCall, VecDeque<VcsError>>>,
    hooks: Mutex<BTreeMap<VcsCall, VecDeque<Hook>>>,
    log: Mutex<Vec<VcsCall>>,
    answers_population: bool,
}

impl FakeVcs {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// A double that answers [`Vcs::population`]. Of those questions only
    /// the unpopulated create is modeled, in the same ref store a populated
    /// one uses.
    pub(crate) fn answering_population() -> Self {
        let mut fake = Self::default();
        fake.answers_population = true;
        fake
    }

    /// Put a ref in the store at `at`, as a birth would.
    ///
    /// Takes `&self` so a hook can call it: opening a window means writing to
    /// the store from inside a call the subject is already making.
    pub(crate) fn put_branch(&self, store: &Path, name: &RawRefName, at: ResolvedRevisionId) {
        self.branches
            .lock()
            .unwrap()
            .insert(Self::key(store, name), at);
    }

    pub(crate) fn branch_tip(&self, store: &Path, name: &RawRefName) -> Option<ResolvedRevisionId> {
        self.branches
            .lock()
            .unwrap()
            .get(&Self::key(store, name))
            .cloned()
    }

    pub(crate) fn declare_ancestor(
        &self,
        ancestor: &ResolvedRevisionId,
        descendant: &ResolvedRevisionId,
    ) {
        self.ancestries.lock().unwrap().insert((
            ancestor.id().as_str().to_owned(),
            descendant.id().as_str().to_owned(),
        ));
    }

    /// Make the next `call` return `err` instead of consulting the store.
    pub(crate) fn fail_next(&self, call: VcsCall, err: VcsError) {
        self.failures
            .lock()
            .unwrap()
            .entry(call)
            .or_default()
            .push_back(err);
    }

    /// Run `hook` immediately before the next `call`, with the store open for
    /// writing.
    pub(crate) fn before_next(&self, call: VcsCall, hook: impl FnOnce(&FakeVcs) + Send + 'static) {
        self.hooks
            .lock()
            .unwrap()
            .entry(call)
            .or_default()
            .push_back(Box::new(hook));
    }

    /// Every modeled call this double has served, in order.
    pub(crate) fn calls(&self) -> Vec<VcsCall> {
        self.log.lock().unwrap().clone()
    }

    /// True when every lock this double holds is free.
    ///
    /// A hook runs inside a call the subject is already making, and may call
    /// back into any method. `Mutex` is not reentrant, so a guard still held
    /// when the hook fires turns that re-entry into a deadlock — a hang, which
    /// no assertion can report — rather than a failure.
    fn locks_all_free(&self) -> bool {
        self.branches.try_lock().is_ok()
            && self.ancestries.try_lock().is_ok()
            && self.failures.try_lock().is_ok()
            && self.hooks.try_lock().is_ok()
            && self.log.try_lock().is_ok()
    }

    fn key(store: &Path, name: &RawRefName) -> (PathBuf, String) {
        (store.to_path_buf(), name.as_str().to_owned())
    }

    fn author_or_adopt(
        &self,
        store: &Path,
        name: &RawRefName,
        at: &ResolvedRevisionId,
    ) -> RefOrigin {
        let key = Self::key(store, name);
        let mut branches = self.branches.lock().unwrap();
        if branches.contains_key(&key) {
            return RefOrigin::Adopted;
        }
        branches.insert(key, at.clone());
        RefOrigin::Authored
    }

    fn enter(&self, call: VcsCall) -> Result<(), VcsError> {
        self.log.lock().unwrap().push(call);
        let hook = self
            .hooks
            .lock()
            .unwrap()
            .get_mut(&call)
            .and_then(VecDeque::pop_front);
        if let Some(hook) = hook {
            hook(self);
        }
        let failure = self
            .failures
            .lock()
            .unwrap()
            .get_mut(&call)
            .and_then(VecDeque::pop_front);
        match failure {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

impl Drop for FakeVcs {
    fn drop(&mut self) {
        if std::thread::panicking() {
            return;
        }
        fn queued<T>(m: &Mutex<BTreeMap<VcsCall, VecDeque<T>>>) -> usize {
            m.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .values()
                .map(VecDeque::len)
                .sum()
        }
        let failures = queued(&self.failures);
        let hooks = queued(&self.hooks);
        assert_eq!(
            (failures, hooks),
            (0, 0),
            "FakeVcs dropped with {failures} scripted failure(s) and {hooks} hook(s) unconsumed \
             after serving {served:?}",
            served = self
                .log
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
    }
}

fn unsupported(method: &str) -> ! {
    panic!(
        "FakeVcs models no behavior for `{method}`; add it, scoped to the arm under test, \
         rather than to the trait"
    )
}

impl Vcs for FakeVcs {
    fn name(&self) -> &str {
        "fake"
    }

    fn minimum_release(&self) -> String {
        "0".to_owned()
    }

    fn tool_readiness(&self) -> crate::vcs::ToolReadiness {
        crate::vcs::ToolReadiness::Ready
    }

    fn is_ancestor(
        &self,
        _repo: &Path,
        ancestor: &ResolvedRevisionId,
        descendant: &ResolvedRevisionId,
    ) -> Result<bool, VcsError> {
        self.enter(VcsCall::IsAncestor)?;
        if ancestor == descendant {
            return Ok(true);
        }
        Ok(self.ancestries.lock().unwrap().contains(&(
            ancestor.id().as_str().to_owned(),
            descendant.id().as_str().to_owned(),
        )))
    }

    /// The canonical store `workspace` resolves to: always `None`.
    ///
    /// A modeled answer, not an absent one. The real method reports `None`
    /// for a path that is not a repo, and this double's refs live in a map
    /// keyed by the path a caller hands it rather than in an object store two
    /// paths could share — so no path it is given has one. That is what keeps
    /// a branch keyed to the path under test.
    ///
    /// It has no error channel, so nothing here can be failed or intercepted
    /// and it takes no [`VcsCall`]. A test that needs two checkouts to share a
    /// store is asserting about the store itself; that wants real repos, or a
    /// declared mapping added here beside the arm that needs it.
    fn store_of(&self, _workspace: &Path) -> Option<PathBuf> {
        None
    }

    fn main_checkout(&self, _checkout: &Path) -> Option<PathBuf> {
        None
    }

    fn resolve_local_branch_tip(
        &self,
        repo: &Path,
        name: &RawRefName,
    ) -> Result<Option<ResolvedRevisionId>, VcsError> {
        self.enter(VcsCall::ResolveLocalBranchTip)?;
        Ok(self.branch_tip(repo, name))
    }

    fn materialize_worktree_on_ref(
        &self,
        store: &Path,
        _dest: &Path,
        name: &RawRefName,
        start_point: &ResolvedRevisionId,
    ) -> Result<RefOrigin, VcsError> {
        self.enter(VcsCall::MaterializeWorktreeOnRef)?;
        Ok(self.author_or_adopt(store, name, start_point))
    }

    /// `None` unless built by [`FakeVcs::answering_population`]: a backend
    /// that answers none of the population questions.
    fn population(&self) -> Option<&dyn WorkspacePopulation> {
        self.answers_population
            .then_some(self as &dyn WorkspacePopulation)
    }

    /// Deletes only at the proof's tip, as git's delete of a ref at an
    /// expected old value does: a branch that moved since the proof stays.
    fn delete_branch(&self, proof: DestroyProof) -> Result<(), VcsError> {
        self.enter(VcsCall::DeleteBranch)?;
        let key = Self::key(proof.store(), proof.branch());
        let mut branches = self.branches.lock().unwrap();
        let expected = proof.tip().id().as_str();
        match branches.get(&key) {
            Some(tip) if tip.id().as_str() != expected => Err(VcsError::CommandFailed {
                args: vec![
                    "delete".to_owned(),
                    format!("refs/heads/{}", proof.branch()),
                    expected.to_owned(),
                ],
                repo: proof.store().to_path_buf(),
                stderr: format!(
                    "cannot lock ref 'refs/heads/{}': is at {} but expected {expected}",
                    proof.branch(),
                    tip.id().as_str()
                ),
            }),
            _ => {
                branches.remove(&key);
                Ok(())
            }
        }
    }

    fn list_branch_names_with_prefix(
        &self,
        repo: &Path,
        prefix: &str,
    ) -> Result<Vec<RawRefName>, VcsError> {
        self.enter(VcsCall::ListBranchNamesWithPrefix)?;
        Ok(self
            .branches
            .lock()
            .unwrap()
            .keys()
            .filter(|(store, name)| store == repo && name.starts_with(prefix))
            .map(|(_, name)| RawRefName::new(name.clone()))
            .collect())
    }

    fn init_repo(&self, _dest: &Path) -> Result<(), VcsError> {
        unsupported("init_repo")
    }

    fn init_bare_repo(&self, _dest: &Path) -> Result<(), VcsError> {
        unsupported("init_bare_repo")
    }

    fn clone_repo(&self, _url: &CloneUrl, _dest: &Path) -> Result<RemoteHandle, VcsError> {
        unsupported("clone_repo")
    }

    fn resolve_branch_on_remote(
        &self,
        _repo: &Path,
        _remote: &RemoteHandle,
        _branch: &RemoteRef,
    ) -> Result<ResolvedRevisionId, VcsError> {
        unsupported("resolve_branch_on_remote")
    }

    fn head_revision(&self, _repo: &Path) -> Result<ResolvedRevisionId, VcsError> {
        unsupported("head_revision")
    }

    fn resolve_revision(&self, _repo: &Path, _rev: &str) -> Result<ResolvedRevisionId, VcsError> {
        unsupported("resolve_revision")
    }

    fn remove_worktree(&self, _repo: &Path, _worktree_path: &Path) -> Result<(), VcsError> {
        unsupported("remove_worktree")
    }

    fn is_repo(&self, _path: &Path) -> bool {
        unsupported("is_repo")
    }

    fn list_worktrees(&self, _repo: &Path) -> Result<Vec<PathBuf>, VcsError> {
        unsupported("list_worktrees")
    }

    fn has_uncommitted_changes(&self, _repo: &Path) -> Result<bool, VcsError> {
        unsupported("has_uncommitted_changes")
    }

    fn dirty_file_names(&self, _repo: &Path) -> Result<Vec<String>, VcsError> {
        unsupported("dirty_file_names")
    }

    fn tracked_dirty_file_names(&self, _repo: &Path) -> Result<Vec<String>, VcsError> {
        unsupported("tracked_dirty_file_names")
    }

    fn ignored_file_names(&self, _repo: &Path) -> Result<Vec<String>, VcsError> {
        unsupported("ignored_file_names")
    }

    fn ignored_removal_hint(&self, _workspace: &Path) -> String {
        unsupported("ignored_removal_hint")
    }

    fn is_tracked(&self, _repo: &Path, _path: &Path) -> Result<bool, VcsError> {
        unsupported("is_tracked")
    }

    /// The double keeps no file content, so it holds none.
    fn holds_committed_content(&self, _repo: &Path, _path: &Path) -> Result<bool, VcsError> {
        self.enter(VcsCall::HoldsCommittedContent)?;
        Ok(false)
    }

    fn stage_paths(&self, _repo: &Path, _paths: &[&str]) -> Result<(), VcsError> {
        unsupported("stage_paths")
    }

    fn stage_content(&self, _repo: &Path, _path: &Path, _content: &[u8]) -> Result<(), VcsError> {
        unsupported("stage_content")
    }

    fn unstage_paths(&self, _repo: &Path, _paths: &[&str]) -> Result<(), VcsError> {
        unsupported("unstage_paths")
    }

    fn discard_captured_change(
        &self,
        _repo: &Path,
        _path: &Path,
        _captured: &[u8],
    ) -> Result<DiscardOutcome, VcsError> {
        unsupported("discard_captured_change")
    }

    fn has_staged_changes(&self, _repo: &Path) -> Result<bool, VcsError> {
        unsupported("has_staged_changes")
    }

    fn staged_paths(&self, _repo: &Path) -> Result<Vec<String>, VcsError> {
        unsupported("staged_paths")
    }

    fn unstaged_paths(&self, _repo: &Path) -> Result<Vec<String>, VcsError> {
        unsupported("unstaged_paths")
    }

    fn commit(&self, _repo: &Path, _message: &str) -> Result<(), VcsError> {
        unsupported("commit")
    }

    fn commit_only(
        &self,
        _repo: &Path,
        _paths: &[&str],
        _message: &str,
        _annotations: &[Annotation],
    ) -> Result<bool, VcsError> {
        unsupported("commit_only")
    }

    fn has_commit_identity(&self, _dir: &Path) -> Result<bool, VcsError> {
        unsupported("has_commit_identity")
    }
    fn annotation_values(
        &self,
        _repo: &Path,
        _commit: &CommitId,
        _key: &str,
    ) -> Result<Vec<String>, VcsError> {
        unsupported("annotation_values")
    }

    fn tag_at_head(&self, _repo: &Path) -> Result<Option<RawRefName>, VcsError> {
        unsupported("tag_at_head")
    }

    fn conflict_resolution_hint(&self, _op: ConflictOp) -> String {
        unsupported("conflict_resolution_hint")
    }

    fn hint(&self, _action: VcsAction) -> &'static str {
        unsupported("hint")
    }

    fn remote_default_branch_repair_hint(&self, _remote: &RemoteHandle) -> String {
        unsupported("remote_default_branch_repair_hint")
    }

    fn clone_default_branch_repair_hint(
        &self,
        _clone: &Path,
        _remote: &RemoteHandle,
        _copies: super::RemoteBranchCopies,
    ) -> Result<String, VcsError> {
        unsupported("clone_default_branch_repair_hint")
    }

    fn rebase(
        &self,
        _repo: &Path,
        _onto: &ResolvedRevisionId,
        _upstream: &ResolvedRevisionId,
        _derived: DerivedContentPolicy,
    ) -> Result<(), VcsError> {
        unsupported("rebase")
    }

    fn replayed(
        &self,
        _repo: &Path,
        _from: &CommitId,
        _old: &CommitId,
        _onto: &CommitId,
        _new: &CommitId,
    ) -> Result<Option<super::Replayed>, VcsError> {
        unsupported("replayed")
    }

    fn record_replayed(
        &self,
        _repo: &Path,
        _old: &CommitId,
        _onto: &CommitId,
        _replayed: &super::Replayed,
    ) -> Result<(), VcsError> {
        unsupported("record_replayed")
    }

    fn forget_replays(&self, _repo: &Path) -> Result<(), VcsError> {
        Ok(())
    }

    fn copy_replayed(
        &self,
        _repo: &Path,
        _commit: &CommitId,
        _parent: &CommitId,
        _changes: &[super::FileChange],
        _annotations: Option<(&str, &[super::Annotation])>,
    ) -> Result<CommitId, VcsError> {
        unsupported("copy_replayed")
    }

    fn take_rewritten(
        &self,
        _on: &super::AttachedRef,
        _from: &CommitId,
        _to: &CommitId,
    ) -> Result<(), VcsError> {
        unsupported("take_rewritten")
    }

    fn rebase_continue(
        &self,
        _repo: &Path,
        _derived: DerivedContentPolicy,
    ) -> Result<(), VcsError> {
        unsupported("rebase_continue")
    }

    fn derived_content_dropped_by_replay(
        &self,
        _repo: &Path,
        _base: &ResolvedRevisionId,
        _source: &ResolvedRevisionId,
        _landed: &ResolvedRevisionId,
    ) -> Result<Vec<String>, VcsError> {
        unsupported("derived_content_dropped_by_replay")
    }

    fn changed_paths_between(
        &self,
        _repo: &Path,
        _from: &ResolvedRevisionId,
        _to: &ResolvedRevisionId,
    ) -> Result<Vec<String>, VcsError> {
        unsupported("changed_paths_between")
    }

    fn lock_driver_setup(&self, _repo: &Path) -> Result<crate::vcs::LockDriverSetup, VcsError> {
        unsupported("lock_driver_setup")
    }

    fn lock_driver_declaration_file(&self) -> &'static str {
        ".fake-attributes"
    }

    fn declare_lock_driver_in(&self, _text: Option<&str>) -> Option<String> {
        None
    }

    fn define_lock_driver(&self, _repo: &Path) -> Result<(), VcsError> {
        unsupported("define_lock_driver")
    }

    fn merging_commits(&self, _repo: &Path) -> Option<(CommitId, CommitId)> {
        None
    }

    fn advance_if_fast_forward(
        &self,
        _repo: &Path,
        _to: &ResolvedRevisionId,
    ) -> Result<(), VcsError> {
        unsupported("advance_if_fast_forward")
    }

    fn hard_reset(&self, _repo: &Path, _to: &ResolvedRevisionId) -> Result<(), VcsError> {
        unsupported("hard_reset")
    }

    fn count_commits_in_range(
        &self,
        _repo: &Path,
        _from: &ResolvedRevisionId,
        _to: &ResolvedRevisionId,
    ) -> Result<usize, VcsError> {
        unsupported("count_commits_in_range")
    }

    fn create_savepoint(
        &self,
        _repo: &Path,
        _key: &SavepointKey,
    ) -> Result<ResolvedRevisionId, VcsError> {
        unsupported("create_savepoint")
    }

    fn create_ref_savepoint(
        &self,
        _repo: &Path,
        _key: &SavepointKey,
        _branch: &RawRefName,
        _at: &CommitId,
    ) -> Result<(), VcsError> {
        unsupported("create_ref_savepoint")
    }

    fn ref_savepoints(
        &self,
        _repo: &Path,
        _key: &SavepointKey,
    ) -> Result<Vec<RefSavepoint>, VcsError> {
        unsupported("ref_savepoints")
    }

    fn drop_ref_savepoint(
        &self,
        _repo: &Path,
        _key: &SavepointKey,
        _branch: &RawRefName,
    ) -> Result<(), VcsError> {
        unsupported("drop_ref_savepoint")
    }

    fn plan_ref_restore(
        &self,
        _repo: &Path,
        _saved: &RefSavepoint,
        _freed: &[PathBuf],
    ) -> Result<crate::vcs::RefRestorePlan, VcsError> {
        unsupported("plan_ref_restore")
    }

    fn execute_ref_restore(
        &self,
        _repo: &Path,
        _key: &SavepointKey,
        _saved: &RefSavepoint,
        _plan: &crate::vcs::RefRestorePlan,
    ) -> Result<RefRestoreOutcome, VcsError> {
        unsupported("execute_ref_restore")
    }

    fn savepoint_label(&self, _key: &SavepointKey) -> String {
        unsupported("savepoint_label")
    }

    fn savepoint_namespace(&self) -> String {
        unsupported("savepoint_namespace")
    }

    fn resolve_savepoint(&self, _repo: &Path, _key: &SavepointKey) -> Option<ResolvedRevisionId> {
        unsupported("resolve_savepoint")
    }

    fn drop_savepoint(&self, _repo: &Path, _key: &SavepointKey) {
        unsupported("drop_savepoint")
    }

    fn drop_checkout_savepoint(&self, _repo: &Path, _key: &SavepointKey) -> Result<(), VcsError> {
        unsupported("drop_checkout_savepoint")
    }

    fn plan_checkout_restore(
        &self,
        _repo: &Path,
        _key: &SavepointKey,
        _attribution: &crate::vcs::Attribution<'_>,
    ) -> Result<crate::vcs::CheckoutRestorePlan, VcsError> {
        unsupported("plan_checkout_restore")
    }

    fn execute_checkout_restore(
        &self,
        _repo: &Path,
        _key: &SavepointKey,
        _plan: &crate::vcs::CheckoutRestorePlan,
    ) -> Result<VerifiedRestoreOutcome, VcsError> {
        unsupported("execute_checkout_restore")
    }

    fn cancel_returns_to(&self, _repo: &Path) -> Result<Option<ResolvedRevisionId>, VcsError> {
        unsupported("cancel_returns_to")
    }

    fn write_backup(
        &self,
        _repo: &Path,
        _source: &BackupSource,
        _tip: &ResolvedRevisionId,
        _mark: Option<BackupMark>,
    ) -> Result<Backup, VcsError> {
        unsupported("write_backup")
    }

    fn list_backups(
        &self,
        _repo: &Path,
        _remote_tracking: RemoteTracking,
    ) -> Result<Vec<ListedBackup>, VcsError> {
        unsupported("list_backups")
    }

    fn drop_backup(&self, _repo: &Path, _name: &BackupName) -> Result<(), VcsError> {
        unsupported("drop_backup")
    }

    fn holds_only_its_own(
        &self,
        _repo: &Path,
        _tip: &CommitId,
        _branch: Option<&RawRefName>,
        _beside: &CommitId,
    ) -> Result<bool, VcsError> {
        unsupported("holds_only_its_own")
    }

    fn show_kept(&self, _checkout: &Path, _base: &CommitId, _kept: &[&BackupName]) -> String {
        String::new()
    }

    fn reached_by_no_branch(
        &self,
        _repo: &Path,
        _tip: &ResolvedRevisionId,
        _remote_tracking: RemoteTracking,
    ) -> Result<Vec<CommitSummary>, VcsError> {
        unsupported("reached_by_no_branch")
    }

    fn snapshot_edits(&self, _repo: &Path) -> Result<Option<ResolvedRevisionId>, VcsError> {
        unsupported("snapshot_edits")
    }

    fn branch_tips(&self, _repo: &Path) -> Result<Vec<BranchTip>, VcsError> {
        unsupported("branch_tips")
    }

    fn private_dir(&self, _checkout: &Path) -> Result<PathBuf, VcsError> {
        unsupported("private_dir")
    }

    fn checkouts_of_branch(
        &self,
        _repo: &Path,
        _branch: &RawRefName,
    ) -> Result<Vec<PathBuf>, VcsError> {
        unsupported("checkouts_of_branch")
    }

    fn upstream_of(
        &self,
        _repo: &Path,
        _branch: &RawRefName,
    ) -> Result<Option<Upstream>, VcsError> {
        unsupported("upstream_of")
    }

    fn remote_copies(
        &self,
        _repo: &Path,
        _branch: &RawRefName,
    ) -> Result<Vec<RemoteTip>, VcsError> {
        unsupported("remote_copies")
    }

    fn set_upstream(
        &self,
        _repo: &Path,
        _branch: &RawRefName,
        _upstream: &Upstream,
    ) -> Result<(), VcsError> {
        unsupported("set_upstream")
    }

    fn rename_branch(
        &self,
        _repo: &Path,
        _from: &RawRefName,
        _to: &RawRefName,
    ) -> Result<(), VcsError> {
        unsupported("rename_branch")
    }

    fn create_branch(
        &self,
        _repo: &Path,
        _name: &RawRefName,
        _at: &ResolvedRevisionId,
        _upstream: Option<&Upstream>,
    ) -> Result<(), VcsError> {
        unsupported("create_branch")
    }

    fn stray_commits(
        &self,
        _repo: &Path,
        _tip: &ResolvedRevisionId,
        _leaving: Option<&RawRefName>,
        _onto: Option<&ResolvedRevisionId>,
    ) -> Result<Vec<CommitSummary>, VcsError> {
        unsupported("stray_commits")
    }

    fn materialize_worktree_detached(
        &self,
        _store: &Path,
        _dest: &Path,
        _at: &ResolvedRevisionId,
    ) -> Result<(), VcsError> {
        unsupported("materialize_worktree_detached")
    }

    fn records_branch(&self) -> &'static str {
        unsupported("records_branch")
    }

    fn records_tip(
        &self,
        _repo: &Path,
        _site: RecordsSite<'_>,
    ) -> Result<Option<CommitId>, VcsError> {
        unsupported("records_tip")
    }

    fn advance_records(
        &self,
        _repo: &Path,
        _expected: Option<&CommitId>,
        _to: &CommitId,
    ) -> Result<RecordsAdvance, VcsError> {
        unsupported("advance_records")
    }

    fn publish_records(
        &self,
        _repo: &Path,
        _remote: &RemoteHandle,
        _commit: &CommitId,
    ) -> Result<RecordsPublish, VcsError> {
        unsupported("publish_records")
    }

    fn files_at(&self, _repo: &Path, _commit: &CommitId) -> Result<Vec<CommittedFile>, VcsError> {
        unsupported("files_at")
    }

    fn write_commit(
        &self,
        _repo: &Path,
        _parents: &[CommitId],
        _changes: &[FileChange],
        _message: &str,
    ) -> Result<CommitId, VcsError> {
        unsupported("write_commit")
    }

    fn start_merge(
        &self,
        _checkout: &Path,
        _other: &CommitId,
        _derived: DerivedContentPolicy,
    ) -> Result<Vec<String>, VcsError> {
        unsupported("start_merge")
    }

    fn merge_in_flight(&self, _checkout: &Path) -> Result<Option<CommitId>, VcsError> {
        unsupported("merge_in_flight")
    }

    fn rebase_in_flight(&self, _checkout: &Path) -> Result<Option<CommitId>, VcsError> {
        unsupported("rebase_in_flight")
    }

    fn branch_under_rebase(&self, _checkout: &Path) -> Result<Option<RawRefName>, VcsError> {
        unsupported("branch_under_rebase")
    }

    fn end_merge(&self, _checkout: &Path) -> Result<(), VcsError> {
        unsupported("end_merge")
    }

    fn copy_commit(
        &self,
        _checkout: &Path,
        _commit: &CommitId,
        _mainline: Option<usize>,
    ) -> Result<crate::vcs::Copied, VcsError> {
        unsupported("copy_replayed")
    }

    fn continue_copy(&self, _checkout: &Path) -> Result<crate::vcs::Copied, VcsError> {
        unsupported("continue_copy")
    }

    fn copy_uncommitted(
        &self,
        _checkout: &Path,
        _commit: &CommitId,
        _mainline: Option<usize>,
    ) -> Result<Vec<String>, VcsError> {
        unsupported("copy_uncommitted")
    }

    fn copy_in_flight(&self, _checkout: &Path) -> Result<Option<CommitId>, VcsError> {
        unsupported("copy_in_flight")
    }

    fn copied_from(&self, _repo: &Path, _commit: &CommitId) -> Result<Option<CommitId>, VcsError> {
        unsupported("copied_from")
    }

    fn copied_into(
        &self,
        _repo: &Path,
        _commits: &[CommitId],
        _target: &ResolvedRevisionId,
    ) -> Result<std::collections::BTreeSet<CommitId>, VcsError> {
        unsupported("copied_into")
    }

    fn parents(&self, _repo: &Path, _commit: &CommitId) -> Result<Vec<CommitId>, VcsError> {
        unsupported("parents")
    }

    fn message_without(
        &self,
        _repo: &Path,
        _commit: &CommitId,
        _keys: &[&str],
    ) -> Result<String, VcsError> {
        unsupported("message_without")
    }

    fn unmerged_paths(&self, _checkout: &Path) -> Result<Vec<String>, VcsError> {
        unsupported("unmerged_paths")
    }

    fn conflict_sides(
        &self,
        _checkout: &Path,
        _path: &str,
    ) -> Result<Option<crate::vcs::ConflictSides>, VcsError> {
        unsupported("conflict_sides")
    }

    fn record_conflict(
        &self,
        _checkout: &Path,
        _path: &str,
        _sides: &crate::vcs::ConflictSides,
    ) -> Result<(), VcsError> {
        unsupported("record_conflict")
    }

    fn conflict_picks(
        &self,
        _checkout: &Path,
        _path: &str,
        _text: &str,
    ) -> Result<Option<[String; 2]>, VcsError> {
        unsupported("conflict_picks")
    }

    fn staged_file(&self, _checkout: &Path, _path: &str) -> Result<Option<String>, VcsError> {
        unsupported("staged_file")
    }

    fn commit_merge(
        &self,
        _checkout: &Path,
        _first: &CommitId,
        _second: &CommitId,
        _message: &str,
        _annotations: &[Annotation],
    ) -> Result<CommitId, VcsError> {
        unsupported("commit_merge")
    }

    fn merge_as_refs(
        &self,
        _repo: &Path,
        _first: &CommitId,
        _second: &CommitId,
        _message: &str,
        _derived: crate::vcs::DerivedContentPolicy,
        _incoming: &CommitId,
    ) -> Result<crate::vcs::RefMerge, VcsError> {
        unsupported("merge_as_refs")
    }

    fn advance_branch_ref(
        &self,
        _repo: &Path,
        _branch: &crate::vcs::RawRefName,
        _expected: &CommitId,
        _to: &CommitId,
    ) -> Result<(), VcsError> {
        unsupported("advance_branch_ref")
    }

    fn restore_branch_ref(
        &self,
        _repo: &Path,
        _branch: &crate::vcs::RawRefName,
        _moved_to: &CommitId,
        _original: &CommitId,
    ) -> Result<(), VcsError> {
        unsupported("restore_branch_ref")
    }

    fn reset_branch_ref(
        &self,
        _repo: &Path,
        _branch: &crate::vcs::RawRefName,
        _expected: &CommitId,
        _to: &CommitId,
        _kept: &crate::vcs::BackupWitness,
    ) -> Result<(), VcsError> {
        unsupported("reset_branch_ref")
    }

    fn merge_bases(
        &self,
        _repo: &Path,
        _a: &CommitId,
        _b: &CommitId,
    ) -> Result<Vec<CommitId>, VcsError> {
        unsupported("merge_bases")
    }

    fn merge_base_against(
        &self,
        _repo: &Path,
        _commit: &CommitId,
        _merged: &[CommitId],
    ) -> Result<Option<CommitId>, VcsError> {
        unsupported("merge_base_against")
    }

    fn last_message_touching(
        &self,
        _repo: &Path,
        _tip: &CommitId,
        _path: &str,
    ) -> Result<Option<String>, VcsError> {
        unsupported("last_message_touching")
    }

    fn branch_push_targets(
        &self,
        _repo: &Path,
        _url_remote: Option<&RemoteHandle>,
    ) -> Result<Vec<RemoteHandle>, VcsError> {
        unsupported("branch_push_targets")
    }

    fn named_branches(&self, _repo: &Path) -> Result<Vec<NamedBranch>, VcsError> {
        unsupported("named_branches")
    }

    fn cancel_in_flight_op(&self, _repo: &Path) {
        unsupported("cancel_in_flight_op")
    }

    fn held_by_no_remote(&self, _repo: &Path) -> Result<Vec<HeldAlone>, VcsError> {
        unsupported("held_by_no_remote")
    }

    fn holds_only_what_it_fetched(&self, _repo: &Path) -> Result<bool, VcsError> {
        unsupported("holds_only_what_it_fetched")
    }

    fn ending_stopped(&self, _repo: &Path, _stopped: crate::vcs::MidOperation) -> String {
        unsupported("ending_stopped")
    }

    fn branch_left(&self, _checkout: &Path) -> Result<Option<RawRefName>, VcsError> {
        unsupported("branch_left")
    }

    fn nested_stores(&self, _checkout: &Path) -> Result<Vec<PathBuf>, VcsError> {
        unsupported("nested_stores")
    }

    fn stores_kept_for(&self, _store: &Path, _checkout: &Path) -> Result<Vec<PathBuf>, VcsError> {
        unsupported("stores_kept_for")
    }

    fn pushed_range(
        &self,
        _repo: &Path,
        _tip: &ResolvedRevisionId,
        _pushed: Option<&ResolvedRevisionId>,
    ) -> Result<Vec<RangeCommit>, VcsError> {
        unsupported("pushed_range")
    }

    fn fetch_objects_from(&self, _dst_repo: &Path, _src_repo: &Path) {
        unsupported("fetch_objects_from")
    }

    fn fetch_remote(&self, _repo: &Path, _remote: &RemoteHandle) -> Result<(), VcsError> {
        unsupported("fetch_remote")
    }

    fn upstream_tip(&self, _on: &AttachedRef) -> Result<Option<ResolvedRevisionId>, VcsError> {
        unsupported("upstream_tip")
    }

    fn unpushed_commits(
        &self,
        _repo: &Path,
        _branch: &RawRefName,
    ) -> Result<Vec<CommitSummary>, VcsError> {
        unsupported("unpushed_commits")
    }

    fn unpushed_beyond(
        &self,
        _repo: &Path,
        _tip: &ResolvedRevisionId,
        _base: &ResolvedRevisionId,
    ) -> Result<Vec<CommitSummary>, VcsError> {
        unsupported("unpushed_beyond")
    }

    fn upstream(&self, _on: &AttachedRef) -> Result<Option<Upstream>, VcsError> {
        unsupported("upstream")
    }

    fn push_target(
        &self,
        _on: &AttachedRef,
        _url_remote: Option<&RemoteHandle>,
    ) -> Result<Option<RemoteHandle>, VcsError> {
        unsupported("push_target")
    }

    fn push_branch(
        &self,
        _on: &AttachedRef,
        _target: &RemoteHandle,
    ) -> Result<Option<ResolvedRevisionId>, VcsError> {
        unsupported("push_branch")
    }

    fn home_remote(
        &self,
        _repo: &Path,
        _url_remote: &RemoteHandle,
    ) -> Result<RemoteHandle, VcsError> {
        unsupported("home_remote")
    }

    fn url_remote(&self, _repo: &Path, _url: &CloneUrl) -> Result<UrlRemote, VcsError> {
        unsupported("url_remote")
    }

    fn add_url_remote(
        &self,
        _repo: &Path,
        _absent: &AbsentRemote,
    ) -> Result<RemoteHandle, VcsError> {
        unsupported("add_url_remote")
    }

    fn project_url_remote(&self, _repo: &Path) -> Result<Option<RemoteHandle>, VcsError> {
        unsupported("project_url_remote")
    }

    fn record_project_url_remote(
        &self,
        _repo: &Path,
        _remote: &RemoteHandle,
    ) -> Result<(), VcsError> {
        unsupported("record_project_url_remote")
    }

    fn recorded_urls(&self, _repo: &Path) -> Result<Vec<RecordedUrl>, VcsError> {
        unsupported("recorded_urls")
    }

    fn reached_by_remote(
        &self,
        _repo: &Path,
        _commit: &ResolvedRevisionId,
        _remote: &RemoteHandle,
    ) -> Result<bool, VcsError> {
        unsupported("reached_by_remote")
    }

    fn fetch_pruning(
        &self,
        _claim: &crate::fetch_claim::FetchClaim,
        _repo: &Path,
        _only: Option<&[RemoteHandle]>,
        _superseded: crate::vcs::Superseded,
    ) -> Result<PruningFetch, VcsError> {
        unsupported("fetch_pruning")
    }

    fn push_target_of(
        &self,
        _repo: &Path,
        _branch: &RawRefName,
        _url_remote: Option<&RemoteHandle>,
    ) -> Result<Option<RemoteHandle>, VcsError> {
        unsupported("push_target_of")
    }

    fn own_push_remote(
        &self,
        _repo: &Path,
        _branch: &RawRefName,
    ) -> Result<Option<RemoteHandle>, VcsError> {
        unsupported("own_push_remote")
    }

    fn file_versions(
        &self,
        _repo: &Path,
        _tip: &ResolvedRevisionId,
        _path: &Path,
    ) -> Result<Vec<String>, VcsError> {
        unsupported("file_versions")
    }

    fn carry_edits_onto(
        &self,
        _repo: &Path,
        _branch: &str,
    ) -> Result<crate::vcs::Refusable<()>, VcsError> {
        unsupported("carry_edits_onto")
    }

    fn check_out_branch(
        &self,
        _repo: &Path,
        _branch: &str,
    ) -> Result<crate::vcs::Refusable<()>, VcsError> {
        unsupported("check_out_branch")
    }

    fn set_edits_aside(
        &self,
        _repo: &Path,
        _paths: &[String],
        _branch: &str,
    ) -> Result<crate::vcs::Refusable<CommitId>, VcsError> {
        unsupported("set_edits_aside")
    }

    fn set_aside_from(
        &self,
        _repo: &Path,
        _at: &CommitId,
        _branch: &str,
    ) -> Result<Vec<CommitId>, VcsError> {
        Ok(Vec::new())
    }

    fn commit_object_exists(&self, _repo: &Path, _id: &CommitId) -> Result<bool, VcsError> {
        unsupported("commit_object_exists")
    }

    fn refs_unreachable_from(&self, _repo: &Path, _other: &Path) -> Result<Vec<String>, VcsError> {
        unsupported("refs_unreachable_from")
    }

    fn list_stale_worktree_registrations(&self, _repo: &Path) -> Result<Vec<PathBuf>, VcsError> {
        unsupported("list_stale_worktree_registrations")
    }

    fn prune_stale_worktree_registration(
        &self,
        _repo: &Path,
        _worktree: &Path,
    ) -> Result<RegistrationPrune, VcsError> {
        unsupported("prune_stale_worktree_registration")
    }

    fn checkout_recorded_elsewhere(&self, _checkout: &Path) -> Option<PathBuf> {
        unsupported("checkout_recorded_elsewhere")
    }

    fn record_checkout_moved(&self, _checkout: &Path) -> Result<(), VcsError> {
        unsupported("record_checkout_moved")
    }

    fn live_worktree_branches(&self, _repo: &Path) -> Result<Vec<RawRefName>, VcsError> {
        unsupported("live_worktree_branches")
    }

    fn worktree_registration(
        &self,
        _store: &Path,
        _checkout: &Path,
    ) -> Result<Option<PathBuf>, VcsError> {
        unsupported("worktree_registration")
    }

    fn reattach_registered_worktree(
        &self,
        _store: &Path,
        _checkout: &Path,
        _registration: &Path,
    ) -> Result<(), VcsError> {
        unsupported("reattach_registered_worktree")
    }

    fn register_worktree_in_place(
        &self,
        _store: &Path,
        _checkout: &Path,
        _branch: &RawRefName,
        _create_at: Option<&ResolvedRevisionId>,
    ) -> Result<(), VcsError> {
        unsupported("register_worktree_in_place")
    }

    fn list_savepoints(&self, _repo: &Path) -> Result<SavepointListing, VcsError> {
        unsupported("list_savepoints")
    }

    fn read_file_at_revision(
        &self,
        _repo: &Path,
        _revision: &ResolvedRevisionId,
        _file_path: &Path,
    ) -> Result<String, VcsError> {
        unsupported("read_file_at_revision")
    }

    fn rebase_stopped_commit_detail(&self, _repo: &Path) -> String {
        unsupported("rebase_stopped_commit_detail")
    }

    fn commits_in_range(
        &self,
        _repo: &Path,
        _from: &CommitId,
        _to: &CommitId,
        _cap: usize,
    ) -> (Vec<CommitSummary>, usize) {
        unsupported("commits_in_range")
    }

    fn ahead_behind(&self, _repo: &Path, _savepoint: &CommitId, _tip: &CommitId) -> (usize, usize) {
        unsupported("ahead_behind")
    }

    fn commits_beyond(
        &self,
        _repo: &Path,
        _tip: &ResolvedRevisionId,
        _base: &ResolvedRevisionId,
    ) -> Result<Vec<CommitSummary>, VcsError> {
        unsupported("commits_beyond")
    }

    fn commits_excluding(
        &self,
        _repo: &Path,
        _tip: &ResolvedRevisionId,
        _excluding: &[CommitId],
        _superseded: bool,
    ) -> Result<Vec<CommitSummary>, VcsError> {
        unsupported("commits_excluding")
    }

    fn first_parent_chain(&self, _repo: &Path, _tip: &CommitId) -> Result<Vec<CommitId>, VcsError> {
        unsupported("first_parent_chain")
    }

    fn on_first_parents(
        &self,
        _repo: &Path,
        _tip: &CommitId,
        _commit: &CommitId,
    ) -> Result<bool, VcsError> {
        unsupported("on_first_parents")
    }

    fn commits_changing(
        &self,
        _repo: &Path,
        _tip: &CommitId,
        _excluding: &[CommitId],
        _path: &str,
    ) -> Result<Vec<CommitId>, VcsError> {
        unsupported("commits_changing")
    }

    fn ancestry(
        &self,
        _repo: &Path,
        _tip: &CommitId,
    ) -> Result<Vec<(CommitId, Vec<CommitId>)>, VcsError> {
        unsupported("ancestry")
    }

    fn patch_equivalents(
        &self,
        _repo: &Path,
        _unlanded: &CommitId,
        _onto: &CommitId,
    ) -> Option<crate::vcs::PatchEquivalents> {
        None
    }

    fn diff_beyond(
        &self,
        _repo: &Path,
        _base: &ResolvedRevisionId,
    ) -> Result<DiffBeyond, VcsError> {
        unsupported("diff_beyond")
    }

    fn log(&self, _repo: &Path, _range: Option<&str>) -> Result<RangeListing, VcsError> {
        unsupported("log")
    }

    fn observe_head(&self, _repo: &Path) -> Result<HeadObservation, VcsError> {
        unsupported("observe_head")
    }

    fn mid_operation(&self, _repo: &Path) -> Option<MidOperation> {
        unsupported("mid_operation")
    }

    fn clone_attached_at(
        &self,
        _url: &CloneUrl,
        _dest: &Path,
        _name: &LocalRefName,
        _declared: &RemoteRef,
        _pin: Option<&RawRevisionId>,
    ) -> Result<MemberBirth, VcsError> {
        unsupported("clone_attached_at")
    }

    fn set_detached_head(&self, _repo: &Path, _to: &ResolvedRevisionId) -> Result<(), VcsError> {
        unsupported("set_detached_head")
    }

    fn create_branch_and_attach(
        &self,
        _repo: &Path,
        _name: &RawRefName,
        _at: &ResolvedRevisionId,
        _departure: &crate::alignment::Departure,
    ) -> Result<(), VcsError> {
        unsupported("create_branch_and_attach")
    }

    fn attach_head_to(&self, _repo: &Path, _name: &LocalRefName) -> Result<(), VcsError> {
        unsupported("attach_head_to")
    }

    fn push_ref(
        &self,
        _repo: &Path,
        _target: &RemoteHandle,
        _r: &PublishRef,
        _replace: &Replace,
    ) -> Result<(), VcsError> {
        unsupported("push_ref")
    }

    fn remote_default_branch(
        &self,
        _repo: &Path,
        _remote: &RemoteHandle,
    ) -> Result<Option<RemoteDefaultBranch>, VcsError> {
        unsupported("remote_default_branch")
    }

    fn list_local_branch_names(&self, _repo: &Path) -> Result<Vec<RawRefName>, VcsError> {
        unsupported("list_local_branch_names")
    }
}

impl WorkspacePopulation for FakeVcs {
    fn materialize_unpopulated_worktree_on_ref(
        &self,
        store: &Path,
        _dest: &Path,
        name: &RawRefName,
        start_point: &ResolvedRevisionId,
    ) -> Result<RefOrigin, VcsError> {
        self.enter(VcsCall::MaterializeUnpopulatedWorktreeOnRef)?;
        Ok(self.author_or_adopt(store, name, start_point))
    }

    fn materialize_unpopulated_worktree_detached(
        &self,
        _store: &Path,
        _dest: &Path,
        _at: &ResolvedRevisionId,
    ) -> Result<(), VcsError> {
        unsupported("materialize_unpopulated_worktree_detached")
    }

    fn tracked_files(&self, _workspace: &Path) -> Result<Vec<TrackedFile>, VcsError> {
        unsupported("tracked_files")
    }

    fn files_holding_recorded_content(
        &self,
        _workspace: &Path,
        _paths: &[String],
    ) -> Result<Vec<TrackedFile>, VcsError> {
        unsupported("files_holding_recorded_content")
    }

    fn finish_checkout(&self, _workspace: &Path) -> Result<(), VcsError> {
        unsupported("finish_checkout")
    }

    fn check_out_paths(&self, _workspace: &Path, _paths: &[String]) -> Result<(), VcsError> {
        unsupported("check_out_paths")
    }

    fn updates_replace_files(&self) -> bool {
        unsupported("updates_replace_files")
    }

    fn recorded_submodules(&self, _workspace: &Path) -> Result<Vec<RecordedSubmodule>, VcsError> {
        Ok(Vec::new())
    }

    fn module_data_holds(&self, _module_data: &Path, _submodule: &RecordedSubmodule) -> bool {
        unsupported("module_data_holds")
    }

    fn create_unpopulated_submodule(
        &self,
        _workspace: &Path,
        _submodule: &RecordedSubmodule,
        _module_data: &Path,
    ) -> Result<SubmoduleBirth, VcsError> {
        unsupported("create_unpopulated_submodule")
    }

    fn connect_submodule_upstream(
        &self,
        _workspace: &Path,
        _submodule: &RecordedSubmodule,
        _source: &Path,
    ) -> Result<(), VcsError> {
        unsupported("connect_submodule_upstream")
    }

    fn discard_submodule(
        &self,
        _workspace: &Path,
        _submodule: &RecordedSubmodule,
    ) -> Result<(), VcsError> {
        unsupported("discard_submodule")
    }

    fn check_out_submodule_from_upstream(
        &self,
        _workspace: &Path,
        _submodule: &RecordedSubmodule,
    ) -> Result<(), VcsError> {
        unsupported("check_out_submodule_from_upstream")
    }

    fn fetch_into_module_data(
        &self,
        _module_data: &Path,
        _submodule: &RecordedSubmodule,
    ) -> Result<(), VcsError> {
        unsupported("fetch_into_module_data")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rev(hex: char) -> ResolvedRevisionId {
        ResolvedRevisionId::from_canonical(
            crate::vcs::CommitId::parse(&hex.to_string().repeat(40)).unwrap(),
            None,
        )
    }

    #[test]
    fn a_hook_fires_between_the_two_calls_it_was_registered_between() {
        let fake = FakeVcs::new();
        let store = Path::new("/store");
        let name = RawRefName::new("p--w");

        fake.before_next(VcsCall::MaterializeWorktreeOnRef, {
            let name = name.clone();
            move |vcs| vcs.put_branch(Path::new("/store"), &name, rev('a'))
        });

        assert_eq!(fake.resolve_local_branch_tip(store, &name).unwrap(), None);
        assert_eq!(
            fake.materialize_worktree_on_ref(store, Path::new("/dest"), &name, &rev('b'))
                .unwrap(),
            RefOrigin::Adopted
        );
        assert_eq!(fake.branch_tip(store, &name).unwrap(), rev('a'));
    }

    #[test]
    fn an_unscripted_materialize_authors_the_ref_at_its_start_point() {
        let fake = FakeVcs::new();
        let store = Path::new("/store");
        let name = RawRefName::new("p--w");

        assert_eq!(
            fake.materialize_worktree_on_ref(store, Path::new("/dest"), &name, &rev('b'))
                .unwrap(),
            RefOrigin::Authored
        );
        assert_eq!(fake.branch_tip(store, &name).unwrap(), rev('b'));
        assert_eq!(fake.calls(), vec![VcsCall::MaterializeWorktreeOnRef]);
    }

    #[test]
    fn a_scripted_failure_is_returned_once_and_not_again() {
        let fake = FakeVcs::new();
        let store = Path::new("/store");
        let name = RawRefName::new("p--w");
        fake.fail_next(
            VcsCall::ResolveLocalBranchTip,
            VcsError::NotARepo(store.to_path_buf()),
        );

        assert!(fake.resolve_local_branch_tip(store, &name).is_err());
        assert!(fake.resolve_local_branch_tip(store, &name).is_ok());
    }

    #[test]
    #[should_panic(expected = "unconsumed")]
    fn dropping_with_a_scripted_failure_nothing_consumed_is_a_test_bug() {
        let fake = FakeVcs::new();
        fake.fail_next(
            VcsCall::DeleteBranch,
            VcsError::NotARepo(PathBuf::from("/store")),
        );
    }

    #[test]
    #[should_panic(expected = "unconsumed")]
    fn dropping_with_a_hook_nothing_ran_is_a_test_bug() {
        let fake = FakeVcs::new();
        fake.before_next(VcsCall::IsAncestor, |_| {});
    }

    #[test]
    #[should_panic(expected = "`head_revision`")]
    fn an_unmodeled_method_names_itself() {
        let _ = FakeVcs::new().head_revision(Path::new("/store"));
    }

    /// The re-entrant call is what the hazard is about; the assertion before it
    /// is what keeps a regression reportable. Held guards make the call hang,
    /// and a hung suite prints nothing, so the cheap check has to run first.
    #[test]
    fn a_hook_can_re_enter_a_call_that_takes_the_locks_it_was_dispatched_from() {
        let fake = FakeVcs::new();
        let store = Path::new("/store");
        let name = RawRefName::new("p--w");

        fake.before_next(VcsCall::MaterializeWorktreeOnRef, {
            let name = name.clone();
            move |vcs| {
                assert!(
                    vcs.locks_all_free(),
                    "a lock was held while the hook ran; re-entry deadlocks instead of failing"
                );
                vcs.put_branch(Path::new("/store"), &name, rev('a'));
                assert_eq!(
                    vcs.resolve_local_branch_tip(Path::new("/store"), &name)
                        .unwrap(),
                    Some(rev('a'))
                );
            }
        });

        assert_eq!(
            fake.materialize_worktree_on_ref(store, Path::new("/dest"), &name, &rev('b'))
                .unwrap(),
            RefOrigin::Adopted
        );
        assert_eq!(
            fake.calls(),
            vec![
                VcsCall::MaterializeWorktreeOnRef,
                VcsCall::ResolveLocalBranchTip
            ]
        );
    }

    #[test]
    fn a_poisoned_lock_does_not_turn_a_clean_drop_into_a_panic() {
        let fake = FakeVcs::new();

        let poisoner = std::thread::scope(|s| {
            s.spawn(|| {
                let _held = fake.failures.lock().unwrap();
                panic!("poisoning a queue that a clean drop still has to read");
            })
            .join()
        });

        assert!(poisoner.is_err());
    }

    /// No production code consumes the supertrait yet, so removing it from
    /// [`Vcs`] compiles clean and every other test stays green. Until the
    /// verbs thread a handle into `run_in_parallel`, this is the only thing
    /// holding it.
    #[test]
    fn the_seam_and_the_double_both_cross_threads() {
        fn require<T: Send + Sync + ?Sized>() {}
        require::<dyn Vcs>();
        require::<dyn WorkspacePopulation>();
        require::<FakeVcs>();
    }

    fn found(name: &str, at: ResolvedRevisionId) -> FoundBranch {
        FoundBranch::named(PathBuf::from("/store"), name, at)
    }

    #[test]
    fn a_double_answers_population_only_when_built_to() {
        assert!(FakeVcs::new().population().is_none());
        assert!(FakeVcs::answering_population().population().is_some());
    }

    #[test]
    fn an_unpopulated_create_authors_and_adopts_as_a_populated_one_does() {
        for unpopulated in [false, true] {
            let fake = FakeVcs::answering_population();
            let create = |r: &FoundBranch, dest: &str| {
                let dest = Path::new(dest);
                if unpopulated {
                    fake.population()
                        .unwrap()
                        .create_unpopulated_worktree_on(r, dest)
                } else {
                    fake.create_worktree_on(r, dest)
                }
            };

            assert_eq!(
                create(&found("p--w", rev('b')), "/one").unwrap(),
                RefOrigin::Authored
            );
            assert_eq!(
                create(&found("p--w", rev('c')), "/two").unwrap(),
                RefOrigin::Adopted
            );
            assert_eq!(
                fake.branch_tip(Path::new("/store"), &RawRefName::new("p--w")),
                Some(rev('b')),
                "adopting moved the ref"
            );

            let served = if unpopulated {
                VcsCall::MaterializeUnpopulatedWorktreeOnRef
            } else {
                VcsCall::MaterializeWorktreeOnRef
            };
            assert_eq!(fake.calls(), vec![served, served]);
        }
    }

    #[test]
    fn a_failed_unpopulated_create_authors_nothing() {
        let fake = FakeVcs::answering_population();
        fake.fail_next(
            VcsCall::MaterializeUnpopulatedWorktreeOnRef,
            VcsError::NotARepo(PathBuf::from("/store")),
        );

        let created = fake
            .population()
            .unwrap()
            .create_unpopulated_worktree_on(&found("p--w", rev('b')), Path::new("/one"));

        assert!(created.is_err());
        assert_eq!(
            fake.branch_tip(Path::new("/store"), &RawRefName::new("p--w")),
            None
        );
    }

    #[test]
    #[should_panic(expected = "`tracked_files`")]
    fn an_unmodeled_population_question_names_itself() {
        let fake = FakeVcs::answering_population();
        let _ = fake.population().unwrap().tracked_files(Path::new("/one"));
    }
}
