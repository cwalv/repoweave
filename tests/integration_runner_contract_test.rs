//! Pins the three contracts between the integration runner and the
//! integrations it drives. Each reads `src/` itself, so the pin's input is the
//! source tree rather than a list restated here.
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

mod common;

use common::src_scan::{production_lines, string_arguments_to, struct_literal_needle};
use repoweave::integration::{Integration, IntegrationContext};
use repoweave::integration_runner::{detection_vocabulary, IntegrationContextBase};
use repoweave::integrations::builtin_integrations;
use std::collections::BTreeSet;

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
