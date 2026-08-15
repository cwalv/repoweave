//! Consent tokens.
//!
//! This is the CLI layer's flag module, and it is the *only* place that can
//! construct `DetachConsent`, `ReattachConsent`, `DiscardUnmergedConsent` and
//! `AdoptDetachedConsent`. Not by convention — there is no route a reviewer
//! has to watch for. Two compiler-checked seals, one per construction route a
//! token has:
//!
//!  1. The tuple literal. Each struct's field is unnamed and unmarked
//!     (private), and Rust resolves field privacy against the *declaring
//!     module*, not the call site — so `DetachConsent(())` cannot be written
//!     from any other module, in this crate or any other. Pinned per token by
//!     `tests/branch_model_compile_fail_test.rs`, by error code.
//!
//!  2. The minting function. `from_flag` is `pub(in crate::cli)`: visible to
//!     this module tree and nowhere else. The only other member of the tree
//!     is `cli::dispatch`, which is where a parsed flag exists to mint from.
//!     `vcs.rs` — which must only ever *receive* a token, never mint one —
//!     cannot call it: `DetachConsent::from_flag(true)` there is E0624,
//!     `associated function is private`.
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
//! never a bare `--force`. `--detach-checkouts` and `--reattach-checkouts`
//! name two categorically different consequences — losing the name your
//! commits hang off, versus moving which name they hang off — so they mint
//! two tokens, not one `ChangeAttachmentConsent`.
/// Proof that the operator consented to leaving a checkout on no
/// branch. Minted from `--detach-checkouts`.
///
/// `Copy`: a zero-sized proof token, not a capability that guards a
/// resource — duplicating "the operator consented" is harmless, and
/// per-repo callers (parallel fetch/update workers) each need their
/// own value from the one token the CLI dispatch minted.
#[derive(Debug, Clone, Copy)]
pub struct DetachConsent(());

impl DetachConsent {
    /// Mint unconditionally, for in-crate test fixtures that need a
    /// token without exercising CLI parsing (e.g. `git.rs`'s `Vcs` impl
    /// tests). `#[cfg(test)]`: absent from the library that the binary
    /// and the integration tests link against, so no product code — in
    /// this module or any other — has an unconditional mint to reach for.
    #[cfg(test)]
    pub(crate) fn granted() -> Self {
        Self(())
    }

    /// Mint from the parsed `--detach-checkouts` value: `Some` iff the
    /// operator passed it. Every verb's dispatch mints through here,
    /// so the flag-to-token mapping lives in exactly one place.
    ///
    /// `pub(in crate::cli)`: a parsed flag exists only in
    /// [`crate::cli::dispatch`], and confining the mint to this module
    /// tree is what turns "only the flag module can construct one" into a
    /// compile error everywhere else.
    pub(in crate::cli) fn from_flag(detach_checkouts: bool) -> Option<Self> {
        detach_checkouts.then_some(Self(()))
    }
}

/// Proof that the operator consented to moving a checkout from one
/// branch to another. Minted from `--reattach-checkouts`.
///
/// `Copy`: see [`DetachConsent`]'s doc comment.
#[derive(Debug, Clone, Copy)]
pub struct ReattachConsent(());

impl ReattachConsent {
    /// Mint unconditionally. `#[cfg(test)]`: see
    /// [`DetachConsent::granted`]'s doc comment.
    #[cfg(test)]
    pub(crate) fn granted() -> Self {
        Self(())
    }

    /// Mint from the parsed `--reattach-checkouts` value: `Some` iff
    /// the operator passed it. `pub(in crate::cli)`: see
    /// [`DetachConsent::from_flag`]'s doc comment.
    pub(in crate::cli) fn from_flag(reattach_checkouts: bool) -> Option<Self> {
        reattach_checkouts.then_some(Self(()))
    }
}

/// Proof that the operator consented to discarding commits that are
/// not merged into the baseline. Minted from
/// `--discard-unmerged-commits`.
///
/// `Copy`: see [`DetachConsent`]'s doc comment.
#[derive(Debug, Clone, Copy)]
pub struct DiscardUnmergedConsent(());

impl DiscardUnmergedConsent {
    /// Mint unconditionally. `#[cfg(test)]`: see
    /// [`DetachConsent::granted`]'s doc comment.
    #[cfg(test)]
    pub(crate) fn granted() -> Self {
        Self(())
    }

    /// Mint from the parsed `--discard-unmerged-commits` value: `Some`
    /// iff the operator passed it. `pub(in crate::cli)`: see
    /// [`DetachConsent::from_flag`]'s doc comment. Integration tests
    /// that need the post-waiver behaviour of `workweave delete` enter
    /// through [`crate::cli::dispatch::workweave_delete`], which is this
    /// mint's only caller.
    pub(in crate::cli) fn from_flag(discard_unmerged_commits: bool) -> Option<Self> {
        discard_unmerged_commits.then_some(Self(()))
    }
}

/// Proof that the operator consented to two things a migration of a
/// detached checkout does: minting a workweave's flat ephemeral ref **at
/// a detached HEAD**, and — when a pre-flat branch holds the name —
/// giving that branch's name up so the flat one can exist in its place.
/// Minted from `--adopt-detached-checkouts`.
///
/// A third token rather than a reuse of [`ReattachConsent`]: reattaching
/// moves a checkout onto a branch that already exists and loses nothing,
/// while this births a branch at the lock SHA and can strand a legacy
/// branch's tip. Different consequence, different flag, different token —
/// the house rule stated at the top of this module.
///
/// `Copy`: see [`DetachConsent`]'s doc comment.
#[derive(Debug, Clone, Copy)]
pub struct AdoptDetachedConsent(());

impl AdoptDetachedConsent {
    // No `granted()`: no in-crate fixture needs one yet, and an unused
    // unconditional mint is dead code the linter would reject anyway.

    /// Mint from the parsed `--adopt-detached-checkouts` value: `Some`
    /// iff the operator passed it. `pub(in crate::cli)`: see
    /// [`DetachConsent::from_flag`]'s doc comment.
    pub(in crate::cli) fn from_flag(adopt_detached_checkouts: bool) -> Option<Self> {
        adopt_detached_checkouts.then_some(Self(()))
    }
}

/// Proof that the operator consented to discarding the current content of
/// an rwv-attested generated file and regenerating it from the current
/// inputs. Minted from `--regenerate-drifted`.
///
/// `Copy`: see [`DetachConsent`]'s doc comment.
#[derive(Debug, Clone, Copy)]
pub struct RegenerateDriftedConsent(());

impl RegenerateDriftedConsent {
    /// Mint unconditionally. `#[cfg(test)]`: see
    /// [`DetachConsent::granted`]'s doc comment.
    #[cfg(test)]
    pub(crate) fn granted() -> Self {
        Self(())
    }

    /// Mint from the parsed `--regenerate-drifted` value: `Some` iff the
    /// operator passed it. `pub(in crate::cli)`: see
    /// [`DetachConsent::from_flag`]'s doc comment.
    pub(in crate::cli) fn from_flag(regenerate_drifted: bool) -> Option<Self> {
        regenerate_drifted.then_some(Self(()))
    }
}

/// Proof that the operator consented to recording an rwv-attested
/// generated file's current content as the accepted generation. Minted
/// from `--adopt-drifted`.
///
/// `Copy`: see [`DetachConsent`]'s doc comment.
#[derive(Debug, Clone, Copy)]
pub struct AdoptDriftedConsent(());

impl AdoptDriftedConsent {
    /// Mint unconditionally. `#[cfg(test)]`: see
    /// [`DetachConsent::granted`]'s doc comment.
    #[cfg(test)]
    pub(crate) fn granted() -> Self {
        Self(())
    }

    /// Mint from the parsed `--adopt-drifted` value: `Some` iff the
    /// operator passed it. `pub(in crate::cli)`: see
    /// [`DetachConsent::from_flag`]'s doc comment.
    pub(in crate::cli) fn from_flag(adopt_drifted: bool) -> Option<Self> {
        adopt_drifted.then_some(Self(()))
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
/// foreign commits is acceptable is a judgement about those commits, and
/// a blanket spelling would answer it for repos the operator never
/// looked at. So the flag names a repo, there is no all-repos form to
/// mint from, and the empty set — the operator passing nothing — covers
/// nothing rather than everything.
#[derive(Debug, Clone, Default)]
pub struct AbandonForeignTipConsent(std::collections::BTreeSet<String>);

impl AbandonForeignTipConsent {
    // No `granted()`: see [`AdoptDetachedConsent`]. The tests that
    // exercise this token drive the binary, so they mint through the
    // flag like an operator does.

    /// Mint from the parsed `--abandon-foreign-tip` values — one entry
    /// per occurrence of the flag. `pub(in crate::cli)`: see
    /// [`DetachConsent::from_flag`]'s doc comment.
    ///
    /// Each value is read as a repo key in the spelling abort's own
    /// per-repo output uses: a manifest repo's workspace-relative path,
    /// or `(project)` for the project repo. Shell completion routinely
    /// appends a separator and prefixes `./` on a path typed from the
    /// workspace root, so both are stripped before matching — an
    /// operator who pasted the path abort printed and let the shell
    /// complete it means the same repo either way.
    pub(in crate::cli) fn from_flag(abandon_foreign_tip: &[String]) -> Self {
        Self(
            abandon_foreign_tip
                .iter()
                .map(|raw| {
                    raw.trim()
                        .trim_start_matches("./")
                        .trim_end_matches('/')
                        .to_string()
                })
                .collect(),
        )
    }

    /// Whether the operator named this repo. `repo_key` is the same key
    /// abort's per-repo maps use, so a `sync-to` op consults it once per
    /// side: consent is given for a repo, and both workspaces' copies of
    /// that repo are covered by it.
    pub fn covers(&self, repo_key: &str) -> bool {
        self.0.contains(repo_key)
    }
}
