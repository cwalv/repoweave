//! Consent tokens.
//!
//! This is the CLI layer's flag module, and it is the *only* place that can
//! construct the tokens it defines. Not by convention — there is no route a
//! reviewer has to watch for. Two compiler-checked seals, one per construction
//! route a token has:
//!
//!  1. The tuple literal. Each struct's field is unnamed and unmarked
//!     (private), and Rust resolves field privacy against the *declaring
//!     module*, not the call site — so `DiscardUnlandedConsent(())` cannot be written
//!     from any other module, in this crate or any other. Pinned per token, by
//!     error code, in `tests/source/branch_model_compile_fail_test.rs` and
//!     `tests/source/workweave_consent_compile_fail_test.rs`.
//!
//!  2. The minting function. `from_flag` is `pub(in crate::cli)`: visible to
//!     this module tree and nowhere else. The only other member of the tree
//!     is `cli::dispatch`, which is where a parsed flag exists to mint from.
//!     `vcs.rs` — which must only ever *receive* a token, never mint one —
//!     cannot call it: `DiscardUnlandedConsent::from_flag(true)` there is E0624,
//!     `associated function is private`. Within dispatch every mint sits in
//!     `run`, which reads argv and takes no arguments, so no caller of the
//!     library picks the flag a token is minted from. No visibility rule can
//!     say that; `tests/source/consent_minted_from_argv_test.rs` holds it.
//!
//! Seal 2 is why dispatch lives in `cli::dispatch` rather than in `main.rs`.
//! A `[[bin]]` target is a *separate crate* from this `[lib]`, so a minting
//! caller out there can only reach a `pub` constructor — and a `pub fn`
//! returning the token is a second construction route reachable from every
//! module of this crate — exactly the reach these seals exist to deny. The
//! narrowest visibility that admits an out-of-crate caller is `pub`; the
//! narrowest that admits `cli::dispatch` is `pub(in crate::cli)`.
//!
//! `granted()` — the unconditional mint, which checks nothing — is
//! `#[cfg(test)]`, and exists only on the tokens some in-crate fixture
//! actually needs one for. It is absent from the build of the library that
//! the binary and the integration tests link against, so in-crate fixtures
//! can still build a token while product code has no unconditional mint to
//! reach for at all.
//!
//! House rule: escape hatches are named for the precondition they waive,
//! never a bare `--force`.

use crate::manifest::{ProjectName, RepoKey, RepoOperand};
use crate::vcs::ResolvedRevisionId;

/// Proof that the operator consented to deleting a branch whose commits have
/// not landed, after a backup. Minted from `--discard-unlanded`.
///
/// `Copy`: see [`DiscardUnlandedConsent`]'s doc comment.
#[derive(Debug, Clone, Copy)]
pub struct DiscardUnlandedConsent(());

impl DiscardUnlandedConsent {
    /// Mint unconditionally. `#[cfg(test)]`: see
    /// [`DiscardUnlandedConsent::granted`]'s doc comment.
    #[cfg(test)]
    pub(crate) fn granted() -> Self {
        Self(())
    }

    /// Mint from the parsed `--discard-unlanded` value: `Some`
    /// iff the operator passed it. `pub(in crate::cli)`: see
    /// [`DiscardUnlandedConsent::from_flag`]'s doc comment.
    pub(in crate::cli) fn from_flag(discard_unlanded: bool) -> Option<Self> {
        discard_unlanded.then_some(Self(()))
    }
}

/// Proof that the operator consented to `rwv push` replacing the push branch
/// of the repos named here, discarding the remote history the pushed tip does
/// not contain. Minted from `--overwrite`, one occurrence per repo.
///
/// Like [`AbandonForeignTipConsent`] it carries data, because the loss is
/// per repo: a flag that named no repo, or all of them, would answer for
/// repos the operator never looked at.
#[derive(Debug, Clone, Default)]
pub struct OverwriteConsent(std::collections::BTreeSet<RepoOperand>);

impl OverwriteConsent {
    /// Mint unconditionally. `#[cfg(test)]`: see
    /// [`DiscardUnlandedConsent::granted`]'s doc comment.
    #[cfg(test)]
    pub(crate) fn granted(repo: RepoOperand) -> Self {
        Self(std::iter::once(repo).collect())
    }

    /// Mint from the parsed `--overwrite` values, one per occurrence of the
    /// flag. `pub(in crate::cli)`: see [`DiscardUnlandedConsent::from_flag`]'s
    /// doc comment.
    pub(in crate::cli) fn from_flag(overwrite: &[RepoOperand]) -> Self {
        Self(overwrite.iter().cloned().collect())
    }

    /// The repos the operator named.
    pub fn named(&self) -> impl Iterator<Item = &RepoOperand> {
        self.0.iter()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The lease that lets a push replace `repo`'s push branch while it
    /// still holds `expected`, when the operator named `repo`.
    pub fn lease(&self, repo: &RepoOperand, expected: ResolvedRevisionId) -> Option<Lease> {
        self.0.contains(repo).then_some(Lease { expected })
    }
}

/// Permission to replace one push branch, provided it still holds the tip it
/// was read at. Minted only by [`OverwriteConsent::lease`].
#[derive(Debug, Clone)]
pub struct Lease {
    expected: ResolvedRevisionId,
}

impl Lease {
    /// The tip the push branch must still hold for the replacement to land.
    pub fn expected(&self) -> &ResolvedRevisionId {
        &self.expected
    }
}

/// Proof that the operator consented to losing uncommitted changes in a
/// workweave being deleted. Minted from `--discard-uncommitted`.
///
/// `Copy`: see [`DiscardUnlandedConsent`]'s doc comment.
#[derive(Debug, Clone, Copy)]
pub struct DiscardUncommittedConsent(());

impl DiscardUncommittedConsent {
    /// Mint from the parsed `--discard-uncommitted` value: `Some` iff the
    /// operator passed it. `pub(in crate::cli)`: see
    /// [`DiscardUnlandedConsent::from_flag`]'s doc comment.
    pub(in crate::cli) fn from_flag(discard_uncommitted: bool) -> Option<Self> {
        discard_uncommitted.then_some(Self(()))
    }
}

/// Proof that the operator consented to giving up a workweave member's
/// pinned commits that are in no clone, relinking its checkout onto the
/// workweave's branch created at the canonical clone's HEAD. Minted from
/// `--abandon-lost-pins`.
///
/// `Copy`: see [`DiscardUnlandedConsent`]'s doc comment.
#[derive(Debug, Clone, Copy)]
pub struct AbandonLostPinsConsent(());

impl AbandonLostPinsConsent {
    /// Mint from the parsed `--abandon-lost-pins` value: `Some` iff the
    /// operator passed it. `pub(in crate::cli)`: see
    /// [`DiscardUnlandedConsent::from_flag`]'s doc comment.
    pub(in crate::cli) fn from_flag(abandon_lost_pins: bool) -> Option<Self> {
        abandon_lost_pins.then_some(Self(()))
    }
}

/// Proof that the operator consented to `rwv doctor --fix` merging edits
/// onto a branch's moved tip, writing the merged files over the working copy
/// once it is kept as a backup. Minted from `--merge-edited-drift`.
///
/// `Copy`: see [`DiscardUnlandedConsent`]'s doc comment.
#[derive(Debug, Clone, Copy)]
pub struct MergeEditedDriftConsent(());

impl MergeEditedDriftConsent {
    /// Mint from the parsed `--merge-edited-drift` value: `Some` iff the
    /// operator passed it. `pub(in crate::cli)`: see
    /// [`DiscardUnlandedConsent::from_flag`]'s doc comment.
    pub(in crate::cli) fn from_flag(merge_edited_drift: bool) -> Option<Self> {
        merge_edited_drift.then_some(Self(()))
    }
}

/// Proof that the operator consented to `rwv doctor --fix` setting edits
/// aside: each file both a branch's move and an edit changed takes the moved
/// tip's content once the working copy and index are kept as a backup, the
/// backup then the only place the edit is held. Minted from
/// `--set-aside-edited-drift`.
///
/// `Copy`: see [`DiscardUnlandedConsent`]'s doc comment.
#[derive(Debug, Clone, Copy)]
pub struct SetAsideEditedDriftConsent(());

impl SetAsideEditedDriftConsent {
    /// Mint from the parsed `--set-aside-edited-drift` value: `Some` iff the
    /// operator passed it. `pub(in crate::cli)`: see
    /// [`DiscardUnlandedConsent::from_flag`]'s doc comment.
    pub(in crate::cli) fn from_flag(set_aside_edited_drift: bool) -> Option<Self> {
        set_aside_edited_drift.then_some(Self(()))
    }
}

/// Which of the two resolutions of edited drift the operator consented to.
#[derive(Debug, Clone, Copy)]
pub enum EditedDriftConsent {
    Merge(MergeEditedDriftConsent),
    SetAside(SetAsideEditedDriftConsent),
}

impl EditedDriftConsent {
    pub fn resolution(self) -> crate::vcs::EditResolution {
        match self {
            Self::Merge(_) => crate::vcs::EditResolution::Merge,
            Self::SetAside(_) => crate::vcs::EditResolution::SetAside,
        }
    }
}

/// Proof that the operator consented to releasing the landing claims and
/// takeover guards `rwv doctor` reports as unreadable, giving up the claim a
/// live landing of another rwv release may hold: every one, or only those on
/// one target. Minted from `--release-unreadable-claims` or
/// `--release-unreadable-claims-on`.
#[derive(Debug, Clone)]
pub struct ReleaseUnreadableClaimsConsent(ClaimScope);

#[derive(Debug, Clone)]
enum ClaimScope {
    Every,
    Target(String),
}

impl ReleaseUnreadableClaimsConsent {
    /// Mint from the parsed flags: `Some` iff the operator passed either.
    /// `pub(in crate::cli)`: see [`DiscardUnlandedConsent::from_flag`]'s doc
    /// comment.
    pub(in crate::cli) fn from_flags(every: bool, on_target: Option<String>) -> Option<Self> {
        match on_target {
            Some(target) => Some(Self(ClaimScope::Target(target))),
            None => every.then_some(Self(ClaimScope::Every)),
        }
    }

    /// Whether `file`, a claim or takeover guard of `project`'s store, is
    /// within what the operator consented to release.
    pub fn admits(&self, file: &std::path::Path, project: Option<&ProjectName>) -> bool {
        match &self.0 {
            ClaimScope::Every => true,
            ClaimScope::Target(target) => project
                .is_some_and(|project| crate::landing_claim::is_on_target(file, project, target)),
        }
    }
}

/// Proof that the operator consented to releasing the fetch, workweave-index
/// and owned-digest claims `rwv doctor` reports as `claim-holder-unreachable`,
/// giving up the claim a holder on a host this one cannot reach may still
/// hold. Minted from `--release-unreachable-claims`.
#[derive(Debug, Clone, Copy)]
pub struct ReleaseUnreachableClaimsConsent(());

impl ReleaseUnreachableClaimsConsent {
    /// Mint from the parsed flag: `Some` iff the operator passed it.
    /// `pub(in crate::cli)`: see [`DiscardUnlandedConsent::from_flag`]'s doc
    /// comment.
    pub(in crate::cli) fn from_flag(release: bool) -> Option<Self> {
        release.then_some(Self(()))
    }
}

/// The claim releases the operator consented to, each minted from its flag.
#[derive(Debug, Clone, Default)]
pub struct ClaimReleases {
    pub unreadable: Option<ReleaseUnreadableClaimsConsent>,
    pub unreachable: Option<ReleaseUnreachableClaimsConsent>,
}

impl ClaimReleases {
    /// `pub(in crate::cli)`: see [`DiscardUnlandedConsent::from_flag`]'s doc
    /// comment.
    pub(in crate::cli) fn new(
        unreadable: Option<ReleaseUnreadableClaimsConsent>,
        unreachable: Option<ReleaseUnreachableClaimsConsent>,
    ) -> Self {
        Self {
            unreadable,
            unreachable,
        }
    }
}

/// Proof that the operator consented to discarding the current content of
/// an rwv-attested generated file and regenerating it from the current
/// inputs. Minted from `--regenerate-drifted`.
///
/// `Copy`: see [`DiscardUnlandedConsent`]'s doc comment.
#[derive(Debug, Clone, Copy)]
pub struct RegenerateDriftedConsent(());

impl RegenerateDriftedConsent {
    /// Mint unconditionally. `#[cfg(test)]`: see
    /// [`DiscardUnlandedConsent::granted`]'s doc comment.
    #[cfg(test)]
    pub(crate) fn granted() -> Self {
        Self(())
    }

    /// Mint from the parsed `--regenerate-drifted` value: `Some` iff the
    /// operator passed it. `pub(in crate::cli)`: see
    /// [`DiscardUnlandedConsent::from_flag`]'s doc comment.
    pub(in crate::cli) fn from_flag(regenerate_drifted: bool) -> Option<Self> {
        regenerate_drifted.then_some(Self(()))
    }
}

/// Proof that the operator consented to recording an rwv-attested
/// generated file's current content as the accepted generation. Minted
/// from `--adopt-drifted`.
///
/// `Copy`: see [`DiscardUnlandedConsent`]'s doc comment.
#[derive(Debug, Clone, Copy)]
pub struct AdoptDriftedConsent(());

impl AdoptDriftedConsent {
    /// Mint unconditionally. `#[cfg(test)]`: see
    /// [`DiscardUnlandedConsent::granted`]'s doc comment.
    #[cfg(test)]
    pub(crate) fn granted() -> Self {
        Self(())
    }

    /// Mint from the parsed `--adopt-drifted` value: `Some` iff the
    /// operator passed it. `pub(in crate::cli)`: see
    /// [`DiscardUnlandedConsent::from_flag`]'s doc comment.
    pub(in crate::cli) fn from_flag(adopt_drifted: bool) -> Option<Self> {
        adopt_drifted.then_some(Self(()))
    }
}

/// Proof that the operator consented to removing weave-root symlinks at names
/// the presented project no longer declares. Minted from
/// `--remove-undeclared-links`.
///
/// Named for what it destroys, per the house rule — links, at undeclared names
/// — rather than for a mode. The destruction is narrow in a way the name has to
/// carry: it unlinks, and the file each link pointed at survives, so the flag
/// must not read as consent to deleting content.
///
/// `Copy`: a zero-sized proof token; see [`DiscardUnlandedConsent`]'s doc comment.
#[derive(Debug, Clone, Copy)]
pub struct RemoveUndeclaredLinksConsent(());

impl RemoveUndeclaredLinksConsent {
    // No `granted()`: nothing in-crate needs to mint one, and this module's
    // rule is that the unconditional mint exists only on tokens some fixture
    // actually needs. The behavior is driven end to end through the flag.

    /// Mint from the parsed `--remove-undeclared-links` value: `Some` iff the
    /// operator passed it. `pub(in crate::cli)`: see
    /// [`DiscardUnlandedConsent::from_flag`]'s doc comment.
    pub(in crate::cli) fn from_flag(remove_undeclared_links: bool) -> Option<Self> {
        remove_undeclared_links.then_some(Self(()))
    }
}

/// Which exit the operator chose out of drift in an rwv-attested generated
/// file. `None` at the call site is the third state — no choice made — and
/// the one that refuses.
///
/// One enum over two independent `Option`s because the exits destroy
/// opposite things: regenerating discards content the operator may have
/// produced deliberately, adopting attests content that may be an accident.
/// Both at once is not a stricter request, it is two contradictory ones,
/// and a type that cannot hold both is what keeps a precedence rule from
/// being invented to break the tie.
#[derive(Debug, Clone, Copy)]
pub enum DriftConsent {
    Regenerate(RegenerateDriftedConsent),
    Adopt(AdoptDriftedConsent),
}

/// Proof that the operator consented to `rwv abort` moving a branch off
/// commits the operation did not create — in the repos named here and no
/// others. Minted from `--abandon-foreign-tip`.
///
/// The one token in this module carrying data rather than being a
/// zero-sized proof, and the module's opening house rule is why. The
/// consequence being waived is per-repo: whether abandoning THIS repo's
/// foreign commits is acceptable is a judgment about those commits, and
/// a blanket spelling would answer it for repos the operator never
/// looked at. So the flag names a repo, there is no all-repos form to
/// mint from, and the empty set — the operator passing nothing — covers
/// nothing rather than everything.
///
/// It also carries `--leave-target`, which ends a landing leaving a target
/// something else moved as it stands.
#[derive(Debug, Clone, Default)]
pub struct AbandonForeignTipConsent {
    abandon: std::collections::BTreeSet<RepoOperand>,
    leave_target: bool,
}

impl AbandonForeignTipConsent {
    // No `granted()`: the tests that exercise this token drive the binary,
    // so they mint through the flag like an operator does.

    /// Mint from the parsed `--abandon-foreign-tip` values — one entry
    /// per occurrence of the flag. `pub(in crate::cli)`: see
    /// [`DiscardUnlandedConsent::from_flag`]'s doc comment.
    ///
    /// Each value is a [`RepoOperand`] as the operator typed it, which already
    /// reads a completed path, a `./` prefix and a Windows separator as the
    /// repo abort's own per-repo output names.
    pub(in crate::cli) fn from_flag(
        abandon_foreign_tip: &[RepoOperand],
        leave_target: bool,
    ) -> Self {
        Self {
            abandon: abandon_foreign_tip.iter().cloned().collect(),
            leave_target,
        }
    }

    pub fn leaves_target(&self) -> bool {
        self.leave_target
    }

    /// Whether the operator named this repo of the op of `project`.
    /// `repo_key` is the same key abort's per-repo maps use, so a `land`
    /// op consults it once per side: consent is given for a repo, and both
    /// workspaces' copies of that repo are covered by it.
    pub fn covers(&self, repo_key: &RepoKey, project: &ProjectName) -> bool {
        self.abandon.contains(&RepoOperand::of(repo_key, project))
    }

    pub fn named(&self) -> impl Iterator<Item = &RepoOperand> {
        self.abandon.iter()
    }

    /// A project other than `project` whose repo the operator named, if any:
    /// consent given for it reaches nothing in this op.
    pub fn names_another_project(&self, project: &ProjectName) -> Option<&ProjectName> {
        self.abandon
            .iter()
            .filter_map(RepoOperand::project)
            .find(|named| *named != project)
    }
}
