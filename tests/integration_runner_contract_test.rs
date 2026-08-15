//! Pins the contracts between the integration runner and the integrations it
//! drives. The source-scanning ones read `src/` itself, so the pin's input is
//! the source tree rather than a list restated here.
//!
//! 1. **Detection vocabulary lives on the trait.** The filenames the runner
//!    pre-computes a detection list for are exactly the ones production code
//!    detects by. The list used to be a hand-maintained const, and it had
//!    already drifted: it cached `go.sum`, which nothing has ever detected by.
//! 2. **The context is assembled by its owner.** `IntegrationContext` and
//!    `IntegrationContextBase` each have one construction site, so the derived
//!    `output_dir` rule cannot be sidestepped by a field-by-field literal.
//! 3. **An integration's name is minted once.** No production module outside
//!    `src/integrations/` re-spells a name an integration returns from
//!    `name()`; a rename would otherwise leave a config lookup silently
//!    falling back to the default config.
//! 4. **A finding's kind is a value, not a sentence.** The verify state
//!    machine's four outcomes and the member-incompatibility observation reach
//!    `Issue::kind` intact, and no production code reads a finding back out of
//!    `Issue::message`.

mod common;

use common::src_scan::{production_lines, string_arguments_to, struct_literal_needle};
use repoweave::integration::{Integration, IntegrationContext, IssueKind, MemberIncompatibility};
use repoweave::integration_runner::{detection_vocabulary, IntegrationContextBase};
use repoweave::integrations::builtin_integrations;
use repoweave::integrations::merge::{drift_issues, missing_issue};
use std::collections::BTreeSet;
use std::path::Path;

#[test]
fn detection_vocabulary_is_exactly_what_production_code_detects_by() {
    // Binding the method itself means a rename breaks compilation here rather
    // than leaving the needle below pointing at nothing.
    let _detector: fn(&IntegrationContext<'static>, &str) -> Vec<String> =
        IntegrationContext::detect_repos_with_manifest;
    let method = "detect_repos_with_manifest";

    let mut detected: BTreeSet<String> = BTreeSet::new();
    let mut call_sites = 0usize;
    for line in production_lines() {
        for filename in string_arguments_to(&line.text, method) {
            detected.insert(filename);
            call_sites += 1;
        }
    }

    assert!(
        call_sites >= 20,
        "scanner found only {call_sites} `{method}(\"…\")` call sites in src/; \
         it found 26 when this pin was written, so a collapse to near zero \
         means the scan broke, not that the calls went away"
    );

    let builtin = builtin_integrations();
    let integrations: Vec<&dyn Integration> = builtin.iter().map(|b| b.as_ref()).collect();
    let declared = detection_vocabulary(&integrations);

    assert_eq!(
        declared, detected,
        "the detection cache's vocabulary and the filenames production code \
         detects by must be the same set. A filename declared but never \
         detected by is a cache slot with no reader; one detected by but not \
         declared misses the cache and falls back to a live filesystem scan \
         on every call. Declare it in that integration's \
         `detection_manifests()`."
    );
}

#[test]
fn integration_context_types_are_constructed_only_by_their_owners() {
    let owners = [
        (
            struct_literal_needle::<IntegrationContext<'static>>(),
            "integration_runner.rs",
        ),
        (
            struct_literal_needle::<IntegrationContextBase<'static>>(),
            "workspace.rs",
        ),
    ];

    let lines = production_lines();
    for (needle, owner) in owners {
        let sites: Vec<String> = lines
            .iter()
            .filter(|l| l.text.contains(&needle))
            .map(|l| l.site())
            .collect();

        assert_eq!(
            sites.len(),
            1,
            "`{needle}` must be built in exactly one production site — \
             {owner}'s constructor, which derives `output_dir` instead of \
             taking it. Found: {sites:?}"
        );
        assert!(
            sites[0].starts_with(owner),
            "`{needle}`'s one production site must be in {owner}; found {}",
            sites[0]
        );
    }
}

#[test]
fn integration_names_are_not_respelled_outside_their_modules() {
    let builtin = builtin_integrations();
    // The needles are the names the integrations return, not names typed here.
    let names: Vec<String> = builtin.iter().map(|b| b.name().to_string()).collect();
    assert!(
        names.len() >= 8,
        "expected the builtin registry to carry at least 8 integrations; \
         got {}: {names:?}",
        names.len()
    );

    let lines = production_lines();
    for name in &names {
        let literal = format!("\"{name}\"");
        let (inside, outside): (Vec<_>, Vec<_>) = lines
            .iter()
            .filter(|l| l.text.contains(&literal))
            .partition(|l| l.file.starts_with("integrations/"));

        assert!(
            !inside.is_empty(),
            "no production line under src/integrations/ carries the literal \
             {literal}, but `{name}` is what that integration's `name()` \
             returns — the scan is not seeing the mint it should"
        );
        let outside: Vec<String> = outside.iter().map(|l| l.site()).collect();
        assert!(
            outside.is_empty(),
            "{literal} is re-spelled outside src/integrations/ at {outside:?}. \
             Take the name from the integration value — `CargoWorkspace.name()` \
             — so a rename moves the lookup with it instead of silently \
             falling back to the default config."
        );
    }
}

/// The verify state machine's three issue-producing outcomes must be readable
/// off the `Issue` without matching on its prose. Inputs are the real
/// `drift_issues` / `missing_issue` boundary, not a restatement of it.
#[test]
fn verify_states_reach_issue_kind_intact() {
    let path = Path::new("/w/projects/p/pnpm-workspace.yaml");

    // USER-HELD: owned key on disk, no rwv marker.
    let user_held = drift_issues(
        "pnpm",
        path,
        false,
        true,
        Some(&[]),
        &[],
        "cut over",
        "detail",
    );
    assert_eq!(user_held.len(), 1);
    assert_eq!(user_held[0].kind, IssueKind::ManagedFileUserHeld);
    assert!(!user_held[0].safe_to_fix);

    // DRIFT: marker present, content diverges.
    let on_disk = ["a".to_string()];
    let expected = ["b".to_string()];
    let drift = drift_issues(
        "pnpm",
        path,
        true,
        true,
        Some(&on_disk),
        &expected,
        "cut over",
        "detail",
    );
    assert_eq!(drift.len(), 1);
    assert_eq!(drift[0].kind, IssueKind::ManagedFileDrift);

    // CLEAN produces nothing at all.
    let clean = drift_issues(
        "pnpm",
        path,
        true,
        true,
        Some(&on_disk),
        &on_disk,
        "cut over",
        "detail",
    );
    assert!(clean.is_empty());

    assert_eq!(
        missing_issue("pnpm", path).kind,
        IssueKind::ManagedFileMissing
    );
}

/// The member-incompatibility observation must survive into the kind. Every
/// fact is compared against what was handed to the predicate, so a renderer
/// can read them instead of parsing the sentence back apart.
#[test]
fn member_incompatibility_observations_survive_into_the_kind() {
    let path = Path::new("/w/projects/p/go.work");
    let issue = MemberIncompatibility::new(
        "go-work",
        path,
        "go",
        "1.21",
        "1.26",
        "github/org/member-a/go.mod",
    )
    .into_issue();

    let IssueKind::MemberIncompatibility(observed) = &issue.kind else {
        panic!("into_issue must tag the finding with its own kind; got {issue:?}");
    };
    assert_eq!(observed.path(), path);
    assert_eq!(observed.key(), "go");
    assert_eq!(observed.on_disk(), "1.21");
    assert_eq!(observed.required(), "1.26");
    assert_eq!(observed.required_by(), "github/org/member-a/go.mod");
    assert_eq!(issue.kind.tag(), IssueKind::MEMBER_INCOMPATIBILITY);
}

/// The one kind tag that is also published prose has one mint. The needle is
/// the const itself, so it cannot point at a word the const no longer uses.
#[test]
fn published_kind_tag_is_minted_once() {
    let literal = format!("\"{}\"", IssueKind::MEMBER_INCOMPATIBILITY);
    let sites: Vec<String> = production_lines()
        .iter()
        .filter(|l| l.text.contains(&literal))
        .map(|l| l.site())
        .collect();

    assert_eq!(
        sites.len(),
        1,
        "{literal} is operator-facing prose (doctor keys the category by it) \
         *and* a discriminant, so it has one mint: \
         `IssueKind::MEMBER_INCOMPATIBILITY`. Found: {sites:?}"
    );
    assert!(
        sites[0].starts_with("integration.rs"),
        "the mint must be the const in integration.rs; found {}",
        sites[0]
    );
}

/// No production code recovers a finding's identity from its message text.
/// The tag used to be the only discriminant available, which made this the
/// obvious thing to reach for; `Issue::kind` is now the thing to match on.
#[test]
fn no_production_code_dispatches_on_issue_message() {
    let needles = [
        "message.contains(",
        "message.starts_with(",
        "message.ends_with(",
    ];
    let lines = production_lines();
    assert!(
        lines.len() > 10_000,
        "scanner returned only {} production lines from src/; it is not \
         reading the tree",
        lines.len()
    );

    let sites: Vec<String> = lines
        .iter()
        .filter(|l| needles.iter().any(|n| l.text.contains(n)))
        .map(|l| format!("{}: {}", l.site(), l.text.trim()))
        .collect();

    assert!(
        sites.is_empty(),
        "production code must not read a finding back out of `Issue::message` \
         — match on `Issue::kind`. Found: {sites:#?}"
    );
}
