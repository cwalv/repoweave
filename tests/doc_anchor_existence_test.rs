//! Every anchor-bearing link on the entry pages resolves to a real heading.
//!
//! Nothing checked this before. A renamed heading breaks every link into it
//! silently: the link-check does not exist, mdBook does not validate
//! fragments, and the published page simply scrolls to the top. The failure is
//! invisible in the repository and visible only to a reader who followed the
//! link and landed nowhere.
//!
//! Structural, under docs/internals/testing.md's licence 2 — a prohibition over an enumerable
//! population. There is no behavioural surface here: an anchor is resolved by
//! the reader's browser, not by anything this suite can drive.
//!
//! **Scope: every page under `docs/`, not just the two that carry refusal
//! entries.** The narrower scope this gate was specified with would police a
//! population of TWO links — and one of those two is a same-page anchor. That
//! number is not a property of the domain: `refusals.md` contributes zero
//! because its author deliberately wrote no anchor links while nothing checked
//! them. A gate whose population is an artifact of an earlier choice not to
//! exercise it is close to vacuous, so the scope is the whole published tree,
//! where the same defect is the same defect.
//!
//! Widening was not free and the cost is recorded rather than hidden: it found
//! three broken anchors that had been shipping, all repaired in the commit that
//! widened it. Two were fragments naming a heading that exists under a
//! different id; the third cited a section of `docs/explanation/joints/sync-semantics.md` that does not
//! exist and never did, so the anchor was dropped and the page link kept.
//!
//! What remains invisible: anchors written in `src/` comments, and links in the
//! generated explain bundles under `docs/reference/explain/`, which are build
//! artifacts rather than authored pages.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Every authored page under `docs/`, generated explain bundles excluded.
fn scanned_pages() -> Vec<PathBuf> {
    let mut out = Vec::new();
    collect(&docs_root(), &mut out);
    out.sort();
    out
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("docs/ is readable").flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n == "explain") {
                continue;
            }
            collect(&path, out);
        } else if path.extension().is_some_and(|e| e == "md") {
            out.push(path);
        }
    }
}

fn docs_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("docs")
}

/// mdBook's heading-id rule: lowercase, whitespace becomes `-`, alphanumerics
/// and `-`/`_` survive, everything else is dropped.
///
/// Leading and trailing hyphens are deliberately NOT stripped — a heading like
/// `` `--json` envelope convention `` anchors as `--json-envelope-convention`,
/// and a slugger that trims them reports live links as broken. That mistake
/// turned three findings into seventeen while this gate was being written,
/// which is why [`the_slugger_matches_known_headings`] exists.
fn heading_id(title: &str) -> String {
    title
        .trim()
        .to_lowercase()
        .chars()
        .filter_map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                Some(c)
            } else if c.is_whitespace() {
                Some('-')
            } else {
                None
            }
        })
        .collect()
}

fn heading_ids(markdown: &str) -> BTreeSet<String> {
    markdown
        .lines()
        .filter_map(|line| {
            let hashes = line.len() - line.trim_start_matches('#').len();
            (1..=6).contains(&hashes).then_some(())?;
            let rest = line.get(hashes..)?;
            rest.starts_with(' ').then(|| heading_id(rest))
        })
        .collect()
}

/// Every `[text](target#fragment)` on `markdown`, as `(target, fragment)`.
fn anchor_links(markdown: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = markdown;
    while let Some(open) = rest.find("](") {
        let after = &rest[open + 2..];
        match after.find(')') {
            Some(close) => {
                let target = &after[..close];
                if !target.starts_with("http") {
                    if let Some((page, fragment)) = target.split_once('#') {
                        out.push((page.to_owned(), fragment.to_owned()));
                    }
                }
                rest = &after[close..];
            }
            None => break,
        }
    }
    out
}

/// Anchors on `page` that do not resolve, as human-readable findings.
fn unresolved_anchors(page: &str, body: &str) -> Vec<String> {
    let mut findings = Vec::new();
    for (target, fragment) in anchor_links(body) {
        let target_path = if target.is_empty() {
            docs_root().join(page.trim_start_matches("docs/"))
        } else {
            let dir = docs_root().join(page.trim_start_matches("docs/"));
            dir.parent()
                .expect("a page has a parent directory")
                .join(&target)
        };
        let Ok(target_body) = std::fs::read_to_string(&target_path) else {
            findings.push(format!(
                "{page} -> {target}#{fragment}: target page does not exist"
            ));
            continue;
        };
        if !heading_ids(&target_body).contains(&fragment) {
            findings.push(format!(
                "{page} -> {target}#{fragment}: no heading on that page anchors as `{fragment}`"
            ));
        }
    }
    findings
}

/// The slugger is the whole gate: get it wrong in the permissive direction and
/// nothing is caught, wrong in the strict direction and correct links are
/// reported until someone turns the gate off. Pinned against headings that
/// really exist, including the two shapes that break naive sluggers — leading
/// punctuation, and a heading that is entirely a code span.
#[test]
fn the_slugger_matches_known_headings() {
    assert_eq!(
        heading_id("`--json` envelope convention"),
        "--json-envelope-convention"
    );
    assert_eq!(
        heading_id("Names, and the characters they exclude"),
        "names-and-the-characters-they-exclude"
    );
    assert_eq!(heading_id("`misnamed-dir`"), "misnamed-dir");
    assert_eq!(heading_id("Reading an entry"), "reading-an-entry");
}

#[test]
fn every_published_anchor_resolves() {
    let pages = scanned_pages();
    let mut scanned = 0usize;
    let mut findings = Vec::new();
    for page in &pages {
        let body = std::fs::read_to_string(page).expect("a walked page is readable");
        let rel = page
            .strip_prefix(Path::new(env!("CARGO_MANIFEST_DIR")))
            .unwrap_or(page)
            .to_string_lossy()
            .into_owned();
        scanned += anchor_links(&body).len();
        findings.extend(unresolved_anchors(&rel, &body));
    }

    // Floors on both the page walk and the link walk. Either one silently
    // yielding nothing is the failure mode a clean-tree assertion cannot tell
    // from success, and this gate's population was measured at 63 links over
    // 40-odd pages when it was written.
    assert!(
        pages.len() >= 20,
        "the walk found {} pages under docs/; it has stopped reading them",
        pages.len()
    );
    assert!(
        scanned >= 40,
        "the walk found {scanned} anchor-bearing links; it has stopped reading them"
    );
    assert!(
        findings.is_empty(),
        "these published links land nowhere:\n{}",
        findings.join("\n")
    );
}

/// The gate must report a broken anchor, not merely pass over a clean tree.
///
/// Both halves of the rf3f pair are seeded here against fixture text rather
/// than the real pages, so the demonstration does not depend on breaking a
/// published document: renaming the heading alone breaks the link, renaming
/// the link alone breaks it, and renaming both together is a rename rather
/// than a break.
#[test]
fn a_broken_anchor_is_reported_and_a_matched_rename_is_not() {
    let target = "docs/reference/formats.md";
    let link_to = |fragment: &str| format!("see [names]({}#{fragment})", "formats.md");

    let real = heading_id("Names, and the characters they exclude");
    let renamed = heading_id("Names, and what they exclude");

    // Control: the citation as it stands resolves.
    assert!(
        unresolved_anchors("docs/reference/doctor-findings.md", &link_to(&real)).is_empty(),
        "precondition: the shipped heading really does anchor as `{real}`"
    );

    // Citation renamed alone: the heading did not move, so this lands nowhere.
    assert_eq!(
        unresolved_anchors("docs/reference/doctor-findings.md", &link_to(&renamed)).len(),
        1,
        "a citation pointing at a heading that does not exist must be reported"
    );

    // Heading renamed alone is the same failure seen from the other end: the
    // fragment the page publishes no longer matches what cites it.
    let body_after_heading_rename = "## Names, and what they exclude\n";
    assert!(
        !heading_ids(body_after_heading_rename).contains(&real),
        "renaming the heading must stop it anchoring as `{real}`"
    );

    // Both renamed together: a rename, not a break.
    assert!(
        heading_ids(body_after_heading_rename).contains(&renamed),
        "a matched rename must still resolve"
    );
    let _ = target;
}
