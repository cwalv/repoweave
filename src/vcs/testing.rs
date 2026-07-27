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
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

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

type Hook = Box<dyn FnOnce(&FakeVcs)>;

/// A fake [`Vcs`] over a map of `(store, ref name) -> tip`.
///
/// Dropping one with a scripted failure or hook still queued panics: a script
/// nothing consumed means the call it was written for never happened, and the
/// assertions that ran did so against a path the test did not intend.
#[derive(Default)]
pub(crate) struct FakeVcs {
    branches: RefCell<BTreeMap<(PathBuf, String), ResolvedRevisionId>>,
    ancestries: RefCell<BTreeSet<(String, String)>>,
    failures: RefCell<BTreeMap<VcsCall, VecDeque<VcsError>>>,
    hooks: RefCell<BTreeMap<VcsCall, VecDeque<Hook>>>,
    log: RefCell<Vec<VcsCall>>,
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
            .borrow_mut()
            .insert(Self::key(store, name), at);
    }

    pub(crate) fn branch_tip(&self, store: &Path, name: &RawRefName) -> Option<ResolvedRevisionId> {
        self.branches.borrow().get(&Self::key(store, name)).cloned()
    }

    pub(crate) fn declare_ancestor(
        &self,
        ancestor: &ResolvedRevisionId,
        descendant: &ResolvedRevisionId,
    ) {
        self.ancestries
            .borrow_mut()
            .insert((ancestor.as_str().to_owned(), descendant.as_str().to_owned()));
    }

    /// Make the next `call` return `err` instead of consulting the store.
    pub(crate) fn fail_next(&self, call: VcsCall, err: VcsError) {
        self.failures
            .borrow_mut()
            .entry(call)
            .or_default()
            .push_back(err);
    }

    /// Run `hook` immediately before the next `call`, with the store open for
    /// writing.
    pub(crate) fn before_next(&self, call: VcsCall, hook: impl FnOnce(&FakeVcs) + 'static) {
        self.hooks
            .borrow_mut()
            .entry(call)
            .or_default()
            .push_back(Box::new(hook));
    }

    /// Every modelled call this double has served, in order.
    pub(crate) fn calls(&self) -> Vec<VcsCall> {
        self.log.borrow().clone()
    }

    fn key(store: &Path, name: &RawRefName) -> (PathBuf, String) {
        (store.to_path_buf(), name.as_str().to_owned())
    }

    fn enter(&self, call: VcsCall) -> Result<(), VcsError> {
        self.log.borrow_mut().push(call);
        let hook = self
            .hooks
            .borrow_mut()
            .get_mut(&call)
            .and_then(VecDeque::pop_front);
        if let Some(hook) = hook {
            hook(self);
        }
        let failure = self
            .failures
            .borrow_mut()
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
        let failures: usize = self.failures.borrow().values().map(VecDeque::len).sum();
        let hooks: usize = self.hooks.borrow().values().map(VecDeque::len).sum();
        assert_eq!(
            (failures, hooks),
            (0, 0),
            "FakeVcs dropped with {failures} scripted failure(s) and {hooks} hook(s) unconsumed \
             after serving {served:?}",
            served = self.log.borrow(),
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
            .borrow()
            .contains(&(ancestor.as_str().to_owned(), descendant.as_str().to_owned())))
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
        let mut branches = self.branches.borrow_mut();
        if branches.contains_key(&key) {
            return Ok(false);
        }
        branches.insert(key, start_point.clone());
        Ok(true)
    }

    fn destroy_local_ref(&self, store: &Path, name: &RawRefName) -> Result<(), VcsError> {
        self.enter(VcsCall::DestroyLocalRef)?;
        self.branches.borrow_mut().remove(&Self::key(store, name));
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
            .borrow()
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

    fn clone_repo_with_remote_name(
        &self,
        _url: &str,
        _dest: &Path,
        _remote_name: &str,
    ) -> Result<(), VcsError> {
        unsupported("clone_repo_with_remote_name")
    }

    fn clone_with_role(&self, _url: &str, _dest: &Path, _role: Role) -> Result<(), VcsError> {
        unsupported("clone_with_role")
    }

    fn resolve_branch_on_remote(
        &self,
        _repo: &Path,
        _role: Role,
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

    fn tag_at_head(&self, _repo: &Path) -> Result<Option<RefName>, VcsError> {
        unsupported("tag_at_head")
    }

    fn worktree_prune(&self, _repo: &Path) -> Result<(), VcsError> {
        unsupported("worktree_prune")
    }

    fn conflict_resolution_hint(&self, _op: ConflictOp) -> String {
        unsupported("conflict_resolution_hint")
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

    fn set_replay_exclusion(&self, _repo: &Path, _path: &Path) -> Result<(), VcsError> {
        unsupported("set_replay_exclusion")
    }

    fn has_replay_exclusion(&self, _repo: &Path, _path: &Path) -> Result<bool, VcsError> {
        unsupported("has_replay_exclusion")
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

    fn resolve_savepoint(&self, _repo: &Path, _op_id: &str) -> Option<ResolvedRevisionId> {
        unsupported("resolve_savepoint")
    }

    fn drop_savepoint(&self, _repo: &Path, _op_id: &str) {
        unsupported("drop_savepoint")
    }

    fn create_pre_abort_ref(&self, _repo: &Path, _op_id: &str) -> Result<PreAbortRef, VcsError> {
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
        _role: Role,
    ) -> Result<bool, VcsError> {
        unsupported("branch_has_remote_counterpart")
    }

    fn count_commits_ahead_of_remote(
        &self,
        _repo: &Path,
        _branch: &RefName,
        _role: Role,
    ) -> Result<usize, VcsError> {
        unsupported("count_commits_ahead_of_remote")
    }

    fn list_local_branches(&self, _repo: &Path) -> Result<Vec<RefName>, VcsError> {
        unsupported("list_local_branches")
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

    fn remote_url(&self, _repo: &Path, _remote: &str) -> Result<Option<String>, VcsError> {
        unsupported("remote_url")
    }

    fn commit_object_exists(&self, _repo: &Path, _sha: &str) -> Result<bool, VcsError> {
        unsupported("commit_object_exists")
    }

    fn resolve_canonical_store(&self, _workspace: &Path) -> Option<PathBuf> {
        unsupported("resolve_canonical_store")
    }

    fn list_stale_worktree_registrations(&self, _repo: &Path) -> Result<Vec<PathBuf>, VcsError> {
        unsupported("list_stale_worktree_registrations")
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
        _role: Role,
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

    fn push_ref(
        &self,
        _repo: &Path,
        _role: Role,
        _r: &PublishRef,
        _force: bool,
    ) -> Result<(), VcsError> {
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
}
