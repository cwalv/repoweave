//! Shared common-contract helper for hybrid file-ownership tests.
//!
//! This module captures the three regression-test shapes shared by every
//! hybrid integration (npm/vscode/cargo/uv — the four hybrid JSON/TOML cases
//! the spec names) plus pnpm/go-work where the helper applies. The shapes
//! are extracted from the npm precedent in
//! `tests/integrations_test/npm_workspaces.rs` and described normatively in
//! `docs/explanation/joints/file-ownership.md`.
//!
//! # The three shapes
//!
//! 1. **activate preserves foreign content.** Seed a file → run activate →
//!    assert the owned keys are set, the marker is present, and a list of
//!    foreign-content substrings still appears verbatim in the file bytes.
//! 2. **activate is idempotent.** Run activate twice in a row over the same
//!    (or a one-repo-changed) ctx → the second run must yield byte-identical
//!    text to the first (when the manifest is unchanged), or differ only in
//!    the documented owned region (when membership changes).
//! 3. **deactivate strips owned + marker; preserves foreign; delete-if-empty.**
//!    Two sub-cases:
//!    - marker + owned only (no foreign content) → file deleted.
//!    - marker + owned + foreign content → file rewritten without the marker
//!      or owned keys; foreign content survives byte-stable.
//! 4. **activate leaves a user-held file alone.** Seed a file carrying the
//!    owned region but NO marker → run activate → the bytes are unchanged.
//!    The mirror of shape 3's marker gate: without the marker the user holds
//!    the pen, so neither verb may write.
//!
//! # API shape (the spec asked us to pick one and document)
//!
//! The helper is a small set of free functions, each taking:
//! - a closure `activate: FnOnce(&Path)` (the caller wraps `integration.activate(&ctx)`),
//! - a closure `deactivate: FnOnce(&Path)` (wraps `integration.deactivate(root)`),
//! - the path to the file under test,
//! - a list of "presence probes": closures `Fn(&str) -> bool` that read the
//!   on-disk text and return true iff a managed key is present. We use closures
//!   rather than a JSON/TOML-typed key list because the helper has to span
//!   four parsers (serde_json, toml, line-oriented YAML, line-oriented go.work),
//!   and the most uniform way to ask "is the owned key here?" is to let each
//!   integration's test write a probe.
//! - a list of foreign-content substrings expected to survive byte-stable.
//!
//! This shape was chosen over passing a typed `&[KeyPath]` because:
//! - The four JSON/TOML integrations have radically different shapes
//!   (npm: top-level keys; vscode: `settings.foo`; cargo: `[workspace].members`
//!   key-decoration; uv: `[tool.uv.workspace].members` key-decoration).
//! - The marker check is per-format (`x-repoweave` vs `rwv.generated` vs
//!   per-key decor vs comment-line), so any typed key-list helper would have
//!   to take per-format marker hints anyway.
//! - Closures keep the per-integration assertion code short (one or two lines)
//!   while making the shared helper exactly the activate/idempotence/deactivate
//!   plumbing.
//!
//! Live in `tests/common/contract.rs` to match the existing test tree shape
//! (`tests/common/mod.rs` already exists with the shared `git()` / `rwv()`
//! helpers; adding `contract` as a sibling module keeps the convention).

#![allow(dead_code)]

use std::fs;
use std::path::Path;

/// A simple probe: given the on-disk text of the managed file, returns
/// `true` iff the named owned key is present. Each integration's test
/// provides its own closure so the helper does not depend on a parser.
///
/// The probe is paired with a human-readable label so failure messages
/// say what's missing without forcing the test to re-derive the name.
pub struct Probe {
    pub label: String,
    pub present: Box<dyn Fn(&str) -> bool>,
}

impl Probe {
    pub fn new<F>(label: impl Into<String>, present: F) -> Self
    where
        F: Fn(&str) -> bool + 'static,
    {
        Self {
            label: label.into(),
            present: Box::new(present),
        }
    }
}

/// Shape 1: activate preserves foreign content.
///
/// Caller seeds the file at `path` before calling. The helper runs
/// `activate`, then asserts:
/// - the file exists,
/// - every probe in `owned_probes` reports the key is present (and the
///   marker probe reports the marker is present),
/// - every substring in `foreign_substrings` still appears in the file.
pub fn assert_activate_preserves_foreign<A>(
    path: &Path,
    activate: A,
    owned_probes: &[Probe],
    marker_probe: &Probe,
    foreign_substrings: &[&str],
) where
    A: FnOnce(),
{
    activate();
    assert!(
        path.exists(),
        "activate should produce a file at {}",
        path.display()
    );
    let text = fs::read_to_string(path).expect("readable file after activate");

    for probe in owned_probes {
        assert!(
            (probe.present)(&text),
            "owned key `{}` should be present after activate; content:\n{text}",
            probe.label
        );
    }
    assert!(
        (marker_probe.present)(&text),
        "ownership marker `{}` should be present after activate; content:\n{text}",
        marker_probe.label
    );

    for needle in foreign_substrings {
        assert!(
            text.contains(needle),
            "foreign content {needle:?} should survive activate byte-stable; \
             content:\n{text}"
        );
    }
}

/// Shape 2: activate is idempotent.
///
/// Calls `activate` twice. The first call may seed the file or operate on
/// an already-seeded one (the caller decides). After the second call:
/// - every owned-key probe reports present,
/// - the marker probe reports present,
/// - every foreign substring survives,
/// - the file content after the second activate is byte-identical to the
///   content after the first activate (i.e. no churn).
///
/// `caller` is invoked between the two activates so the test can simulate a
/// user mutation (e.g. add a `scripts.ci` entry) and assert it survives. If
/// the caller mutates between activates, byte-stability is asserted against
/// the post-mutation file; pass an empty closure if not mutating.
pub fn assert_activate_idempotent<A, M>(
    path: &Path,
    activate: A,
    mutate_between: M,
    owned_probes: &[Probe],
    marker_probe: &Probe,
    foreign_substrings: &[&str],
) where
    A: Fn(),
    M: FnOnce(&Path),
{
    activate();
    assert!(path.exists(), "first activate should produce a file");
    mutate_between(path);
    let before = fs::read_to_string(path).expect("readable file after first activate");
    activate();
    let after = fs::read_to_string(path).expect("readable file after second activate");

    assert_eq!(
        before, after,
        "second activate should be byte-identical to first; diff:\nBEFORE:\n{before}\nAFTER:\n{after}"
    );
    for probe in owned_probes {
        assert!(
            (probe.present)(&after),
            "owned key `{}` should remain present after idempotent activate",
            probe.label
        );
    }
    assert!(
        (marker_probe.present)(&after),
        "marker `{}` should remain present after idempotent activate",
        marker_probe.label
    );
    for needle in foreign_substrings {
        assert!(
            after.contains(needle),
            "foreign content {needle:?} should survive a second activate"
        );
    }
}

/// Shape 3a: deactivate strips owned + marker, preserves foreign content,
/// rewrites the file.
///
/// Caller seeds the file with marker + owned keys + foreign content, then
/// the helper runs `deactivate`. Asserts:
/// - the file STILL EXISTS,
/// - every probe in `owned_probes` reports absent,
/// - the marker probe reports absent,
/// - every foreign substring survives.
pub fn assert_deactivate_strips_keeps<D>(
    path: &Path,
    deactivate: D,
    owned_probes: &[Probe],
    marker_probe: &Probe,
    foreign_substrings: &[&str],
) where
    D: FnOnce(),
{
    deactivate();
    assert!(
        path.exists(),
        "deactivate must NOT delete a file with user-authored foreign content; missing: {}",
        path.display()
    );
    let text = fs::read_to_string(path).expect("readable file after deactivate-keeps");

    for probe in owned_probes {
        assert!(
            !(probe.present)(&text),
            "owned key `{}` should be stripped on deactivate; content:\n{text}",
            probe.label
        );
    }
    assert!(
        !(marker_probe.present)(&text),
        "marker `{}` should be stripped on deactivate; content:\n{text}",
        marker_probe.label
    );

    for needle in foreign_substrings {
        assert!(
            text.contains(needle),
            "foreign content {needle:?} should survive deactivate byte-stable; \
             content:\n{text}"
        );
    }
}

/// Shape 3b: deactivate deletes a fully-rwv-owned file (marker + owned
/// keys, no foreign content).
///
/// Caller seeds the file with only marker + owned keys. After `deactivate`,
/// the file MUST be gone.
pub fn assert_deactivate_deletes_when_only_owned<D>(path: &Path, deactivate: D)
where
    D: FnOnce(),
{
    deactivate();
    assert!(
        !path.exists(),
        "deactivate must DELETE a file with no user-authored content; still present: {}",
        path.display()
    );
}

/// Shape 4: activate leaves a user-held file alone.
///
/// Caller seeds the file at `path` with the owned region but no ownership
/// marker. `activate` is run twice — a refusal that only holds for one run is
/// not a refusal — and the bytes must be identical to what was seeded.
pub fn assert_activate_leaves_user_held_untouched<A>(path: &Path, activate: A)
where
    A: Fn(),
{
    let before = fs::read_to_string(path).expect("caller must seed the file before calling");
    activate();
    activate();
    let after = fs::read_to_string(path).expect("activate must not delete a user-held file");
    assert_eq!(
        before, after,
        "activate must not write a file carrying the owned region without the \
         ownership marker; diff:\nBEFORE:\n{before}\nAFTER:\n{after}"
    );
}

/// Convenience: a probe that checks substring presence in the raw text.
/// Used for line-oriented integrations (pnpm, go-work) and for foreign
/// content survival.
pub fn substr_probe(label: impl Into<String>, needle: impl Into<String>) -> Probe {
    let needle = needle.into();
    Probe::new(label, move |text: &str| text.contains(&needle))
}

/// Convenience: a probe that parses the text as JSON and asks `f` whether
/// the resulting value carries the key. Returns false if the text is not
/// valid JSON (the probe interprets parse-failure as "key not present").
pub fn json_probe<F>(label: impl Into<String>, f: F) -> Probe
where
    F: Fn(&serde_json::Value) -> bool + 'static,
{
    Probe::new(label, move |text: &str| {
        serde_json::from_str::<serde_json::Value>(text)
            .map(|v| f(&v))
            .unwrap_or(false)
    })
}
