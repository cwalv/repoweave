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
//! Only the methods some uncovered arm needed are modelled. Every other method
//! panics naming itself: a method nobody has needed yet is one whose faithful
//! fake behaviour nobody has thought about, and guessing quietly is how a
//! double starts lying.

use super::*;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Mutex;

/// The trait methods [`FakeVcs`] models, and the key a script or a hook is
/// registered under.
///
/// Named for the *required* method rather than the provided wrapper above it:
/// `create_worktree_on` and `delete_owned_ref` carry the receipts and warrants
/// but delegate the VCS work, so those delegations are what a fake can
/// intercept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum VcsCall {
    IsAncestor,
    ListBranchNamesWithPrefix,
    ResolveLocalBranchTip,
    MaterializeWorktreeOnRef,
    DestroyLocalRef,
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
}

impl FakeVcs {
    pub(crate) fn new() -> Self {
        Self::default()
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
        self.ancestries
            .lock()
            .unwrap()
            .insert((ancestor.as_str().to_owned(), descendant.as_str().to_owned()));
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

    /// Every modelled call this double has served, in order.
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
        "FakeVcs models no behaviour for `{method}`; add it, scoped to the arm under test, \
         rather than to the trait"
    )
}

impl Vcs for FakeVcs {
    fn name(&self) -> &str {
        "fake"
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
        Ok(self
            .ancestries
            .lock()
            .unwrap()
            .contains(&(ancestor.as_str().to_owned(), descendant.as_str().to_owned())))
    }

    /// The canonical store `workspace` resolves to: always `None`.
    ///
    /// A modelled answer, not an absent one. The real method reports `None`
    /// for a path that is not a repo, and this double's refs live in a map
    /// keyed by the path a caller hands it rather than in an object store two
    /// paths could share — so no path it is given has one. That is what keeps
    /// an ownership receipt keyed to the path under test.
    ///
    /// It has no error channel, so nothing here can be failed or intercepted
    /// and it takes no [`VcsCall`]. A test that needs two checkouts to share a
    /// store is asserting about the store itself; that wants real repos, or a
    /// declared mapping added here beside the arm that needs it.
    fn resolve_canonical_store(&self, _workspace: &Path) -> Option<PathBuf> {
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
    ) -> Result<bool, VcsError> {
        self.enter(VcsCall::MaterializeWorktreeOnRef)?;
        let key = Self::key(store, name);
        let mut branches = self.branches.lock().unwrap();
        if branches.contains_key(&key) {
            return Ok(false);
        }
        branches.insert(key, start_point.clone());
        Ok(true)
    }

    fn destroy_local_ref(&self, store: &Path, name: &RawRefName) -> Result<(), VcsError> {
        self.enter(VcsCall::DestroyLocalRef)?;
        self.branches
            .lock()
            .unwrap()
            .remove(&Self::key(store, name));
        Ok(())
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

    fn clone_repo(&self, _url: &str, _dest: &Path) -> Result<(), VcsError> {
        unsupported("clone_repo")
    }

    fn conventional_remote_name(&self) -> &str {
        unsupported("conventional_remote_name")
    }

    fn resolve_branch_on_remote(
        &self,
        _repo: &Path,
        _branch: &RefName,
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

    fn is_tracked(&self, _repo: &Path, _path: &Path) -> Result<bool, VcsError> {
        unsupported("is_tracked")
    }

    fn stage_paths(&self, _repo: &Path, _paths: &[&str]) -> Result<(), VcsError> {
        unsupported("stage_paths")
    }

    fn has_staged_changes(&self, _repo: &Path) -> Result<bool, VcsError> {
        unsupported("has_staged_changes")
    }

    fn staged_paths(&self, _repo: &Path) -> Result<Vec<String>, VcsError> {
        unsupported("staged_paths")
    }

    fn commit(&self, _repo: &Path, _message: &str) -> Result<(), VcsError> {
        unsupported("commit")
    }

    fn add_remote(&self, _repo: &Path, _url: &str) -> Result<(), VcsError> {
        unsupported("add_remote")
    }

    fn tag_at_head(&self, _repo: &Path) -> Result<Option<RefName>, VcsError> {
        unsupported("tag_at_head")
    }

    fn worktree_prune(&self, _repo: &Path) -> Result<(), VcsError> {
        unsupported("worktree_prune")
    }

    fn conflict_resolution_hint(&self, _op: ConflictOp) -> String {
        unsupported("conflict_resolution_hint")
    }

    fn remote_default_branch_repair_hint(&self) -> String {
        unsupported("remote_default_branch_repair_hint")
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

    fn set_replay_exclusion(&self, _repo: &Path, _path: &Path) -> Result<(), VcsError> {
        unsupported("set_replay_exclusion")
    }

    fn replay_exclusion_state(
        &self,
        _repo: &Path,
        _path: &Path,
    ) -> Result<crate::vcs::ReplayExclusionState, VcsError> {
        unsupported("replay_exclusion_state")
    }

    fn has_committed_replay_exclusion(&self, _repo: &Path, _path: &Path) -> Result<bool, VcsError> {
        unsupported("has_committed_replay_exclusion")
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

    fn create_savepoint(&self, _repo: &Path, _op_id: &str) -> Result<ResolvedRevisionId, VcsError> {
        unsupported("create_savepoint")
    }

    fn savepoint_label(&self, _op_id: &str) -> String {
        unsupported("savepoint_label")
    }

    fn savepoint_namespace(&self) -> String {
        unsupported("savepoint_namespace")
    }

    fn resolve_savepoint(&self, _repo: &Path, _op_id: &str) -> Option<ResolvedRevisionId> {
        unsupported("resolve_savepoint")
    }

    fn drop_savepoint(&self, _repo: &Path, _op_id: &str) {
        unsupported("drop_savepoint")
    }

    fn create_pre_abort_ref(
        &self,
        _repo: &Path,
        _op_id: &str,
        _foreign_tip_policy: ForeignTipPolicy,
    ) -> Result<PreAbortRef, VcsError> {
        unsupported("create_pre_abort_ref")
    }

    fn resolve_pre_abort_ref(&self, _repo: &Path, _op_id: &str) -> Option<PreAbortRef> {
        unsupported("resolve_pre_abort_ref")
    }

    fn verified_restore_savepoint(
        &self,
        _repo: &Path,
        _op_id: &str,
        _recorded_intent_tip: Option<&str>,
        _recorded_converged_tip: Option<&str>,
        _foreign_tip_policy: ForeignTipPolicy,
    ) -> Result<VerifiedRestoreOutcome, VcsError> {
        unsupported("verified_restore_savepoint")
    }

    fn mid_op(&self, _repo: &Path) -> Option<ConflictOp> {
        unsupported("mid_op")
    }

    fn cancel_in_flight_op(&self, _repo: &Path) {
        unsupported("cancel_in_flight_op")
    }

    fn branch_has_remote_counterpart(
        &self,
        _repo: &Path,
        _branch: &RefName,
    ) -> Result<bool, VcsError> {
        unsupported("branch_has_remote_counterpart")
    }

    fn count_commits_ahead_of_remote(
        &self,
        _repo: &Path,
        _branch: &RefName,
    ) -> Result<usize, VcsError> {
        unsupported("count_commits_ahead_of_remote")
    }

    fn fetch_objects_from(&self, _dst_repo: &Path, _src_repo: &Path) {
        unsupported("fetch_objects_from")
    }

    fn refresh_index_to_head_if_safe(&self, _repo: &Path) {
        unsupported("refresh_index_to_head_if_safe")
    }

    fn refresh_working_tree_to_head_if_safe(&self, _repo: &Path) {
        unsupported("refresh_working_tree_to_head_if_safe")
    }

    fn remote_url(&self, _repo: &Path) -> Result<Option<String>, VcsError> {
        unsupported("remote_url")
    }

    fn commit_object_exists(&self, _repo: &Path, _sha: &str) -> Result<bool, VcsError> {
        unsupported("commit_object_exists")
    }

    fn list_stale_worktree_registrations(&self, _repo: &Path) -> Result<Vec<PathBuf>, VcsError> {
        unsupported("list_stale_worktree_registrations")
    }

    fn live_worktree_branches(&self, _repo: &Path) -> Result<Vec<RawRefName>, VcsError> {
        unsupported("live_worktree_branches")
    }

    fn list_savepoint_op_ids(&self, _repo: &Path) -> Result<Vec<String>, VcsError> {
        unsupported("list_savepoint_op_ids")
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

    fn log_oneline_range(
        &self,
        _repo: &Path,
        _from: &str,
        _to: &str,
        _cap: usize,
    ) -> (Vec<String>, usize) {
        unsupported("log_oneline_range")
    }

    fn ahead_behind(&self, _repo: &Path, _savepoint: &str, _tip: &str) -> (usize, usize) {
        unsupported("ahead_behind")
    }

    fn unique_commits(
        &self,
        _repo: &Path,
        _parent_tip: &ResolvedRevisionId,
    ) -> Result<Vec<CommitSummary>, VcsError> {
        unsupported("unique_commits")
    }

    fn unique_diff(
        &self,
        _repo: &Path,
        _parent_tip: &ResolvedRevisionId,
    ) -> Result<UniqueDiff, VcsError> {
        unsupported("unique_diff")
    }

    fn observe_head(&self, _repo: &Path) -> Result<HeadObservation, VcsError> {
        unsupported("observe_head")
    }

    fn mid_operation(&self, _repo: &Path) -> Option<String> {
        unsupported("mid_operation")
    }

    fn clone_attached_at(
        &self,
        _url: &str,
        _dest: &Path,
        _name: &LocalRefName,
        _at: &RawRevisionId,
    ) -> Result<ResolvedRevisionId, VcsError> {
        unsupported("clone_attached_at")
    }

    fn set_detached_head(&self, _repo: &Path, _to: &ResolvedRevisionId) -> Result<(), VcsError> {
        unsupported("set_detached_head")
    }

    fn attach_head_to(&self, _repo: &Path, _name: &LocalRefName) -> Result<(), VcsError> {
        unsupported("attach_head_to")
    }

    fn rename_local_ref(
        &self,
        _store: &Path,
        _from: &RawRefName,
        _to: &RawRefName,
    ) -> Result<(), VcsError> {
        unsupported("rename_local_ref")
    }

    fn birth_ref_at_head(&self, _repo: &Path, _name: &RawRefName) -> Result<(), VcsError> {
        unsupported("birth_ref_at_head")
    }

    fn push_ref(&self, _repo: &Path, _r: &PublishRef, _force: bool) -> Result<(), VcsError> {
        unsupported("push_ref")
    }

    fn remote_default_branch(&self, _repo: &Path) -> Result<Option<RemoteDefaultBranch>, VcsError> {
        unsupported("remote_default_branch")
    }

    fn list_local_branch_names(&self, _repo: &Path) -> Result<Vec<RawRefName>, VcsError> {
        unsupported("list_local_branch_names")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rev(hex: char) -> ResolvedRevisionId {
        ResolvedRevisionId::from_canonical(hex.to_string().repeat(40), None)
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
        assert!(!fake
            .materialize_worktree_on_ref(store, Path::new("/dest"), &name, &rev('b'))
            .unwrap());
        assert_eq!(fake.branch_tip(store, &name).unwrap(), rev('a'));
    }

    #[test]
    fn an_unscripted_materialize_authors_the_ref_at_its_start_point() {
        let fake = FakeVcs::new();
        let store = Path::new("/store");
        let name = RawRefName::new("p--w");

        assert!(fake
            .materialize_worktree_on_ref(store, Path::new("/dest"), &name, &rev('b'))
            .unwrap());
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
            VcsCall::DestroyLocalRef,
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
    fn an_unmodelled_method_names_itself() {
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

        assert!(!fake
            .materialize_worktree_on_ref(store, Path::new("/dest"), &name, &rev('b'))
            .unwrap());
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
        require::<FakeVcs>();
    }
}
