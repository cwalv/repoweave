//! Pins the readers of the process cwd in `src/` to two sanctioned sites,
//! each reading it for a different subject.
//!
//! `workspace::acquire_origin_dir` reads the cwd as the *invocation
//! origin*: its result feeds resolution, so a second origin-reader would
//! silently un-do `-C` and `-w`, which work by injecting a different
//! origin into the resolver. Such a handler would be correct in a bare
//! invocation and wrong under either addressing flag — the shape a test
//! catches and a reviewer does not. ARCHITECTURE.md repeats the claim.
//!
//! `workweave delete`'s step-out probe reads the cwd as *process state*:
//! whether this process's own open-directory handle sits inside the tree
//! about to be removed (a Windows delete fails on exactly that handle,
//! and the holder is the deleting process itself). No path or resolution
//! flows from the read — its one consumer is the decision to step out —
//! and the resolved origin cannot substitute: `-C` points the origin
//! elsewhere while the handle stays put.
//!
//! A new reader is sanctioned only on the second site's terms: the
//! subject is the process's own state, and nothing derived from the read
//! feeds addressing. A reader whose result feeds resolution takes the
//! resolved origin as a parameter instead.
//!
//! The needle is `std`'s own spelling, so unlike the `*_single_mint_test`
//! family it cannot be derived from a repo symbol. The per-site count
//! guard below is what stands in for that: if the needle stops matching,
//! a sanctioned site stops being found and the test fails rather than
//! passing on an empty scan.
//!
//! Residue: only the `env::current_dir` spelling is matched. A caller
//! that binds `use std::env::current_dir` and calls it bare, or reaches
//! the cwd through `std::env::var("PWD")` or a `Command`'s inherited
//! working directory, is not one of the shapes here.

mod common;

use common::src_scan::{production_lines, SourceLine};

/// Each sanctioned site: file, owning function, exact number of reads.
const SANCTIONED: &[(&str, &str, usize)] = &[
    ("workspace.rs", "acquire_origin_dir", 1),
    ("workweave.rs", "delete_workweave_inner_at", 1),
];

const NEEDLE: &str = "env::current_dir(";

fn lines_reading_the_process_cwd() -> Vec<SourceLine> {
    production_lines()
        .into_iter()
        .filter(|l| l.text.contains(NEEDLE))
        .collect()
}

#[test]
fn every_sanctioned_site_is_still_found() {
    let hits = lines_reading_the_process_cwd();
    for (file, owner_fn, expected) in SANCTIONED {
        let owned = hits.iter().filter(|l| l.file == *file).count();
        assert_eq!(
            owned, *expected,
            "expected exactly {expected} `{NEEDLE}` read(s) in src/{file}, \
             found {owned} — either the needle no longer matches the source \
             (an empty result elsewhere would prove nothing; re-derive it \
             before trusting the other test in this file), or a reader was \
             added inside a sanctioned file, where the stray scan below \
             cannot see it."
        );
        let src = std::fs::read_to_string(common::src_scan::src_dir().join(file))
            .unwrap_or_else(|e| panic!("read src/{file}: {e}"));
        assert!(
            src.contains(&format!("fn {owner_fn}(")),
            "src/{file} no longer defines `{owner_fn}`; the sanctioned \
             reader moved and this pin names the wrong site."
        );
    }
}

#[test]
fn no_module_outside_the_sanctioned_sites_reads_the_process_cwd() {
    let sanctioned_files: Vec<&str> = SANCTIONED.iter().map(|(f, _, _)| *f).collect();
    let strays: Vec<String> = lines_reading_the_process_cwd()
        .iter()
        .filter(|l| !sanctioned_files.contains(&l.file.as_str()))
        .map(|l| format!("{} {}", l.site(), l.text.trim()))
        .collect();

    assert!(
        strays.is_empty(),
        "the process cwd has two sanctioned readers: \
         `workspace::acquire_origin_dir`, whose result feeds resolution, \
         and `workweave delete`'s step-out probe, which reads it as \
         process state and derives no path from it. A new reader whose \
         result feeds addressing un-does `-C` and `-w` — take the \
         resolved origin as a parameter instead. Found: {strays:#?}"
    );
}
