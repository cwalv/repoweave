//! The library writes to standard output through `src/stdout.rs` and nowhere
//! else: `println!` and `print!` panic on a closed pipe, and a hand-rolled
//! `io::stdout()` write brings back whatever rule its author chose.
//!
//! Scope: production lines of the library under `src/`, outside `#[cfg(test)]`
//! items and comments. `src/bin/` is not read: the generator and the bundled
//! plugin are separate programs, and a plugin never names the library, so it
//! keeps a writer of its own. A write through a handle obtained some other way
//! than `stdout()` is invisible here.

mod common;

use common::src_scan::production_lines;

fn prints_to_std_stdout(text: &str) -> bool {
    let mut rest = text;
    while let Some(at) = rest.find("print") {
        let before = &rest[..at];
        let after = &rest[at..];
        let named = after.starts_with("println!(") || after.starts_with("print!(");
        let prefix = before.chars().last();
        if named && !prefix.is_some_and(|c| c.is_alphanumeric() || c == '_') {
            return true;
        }
        rest = &rest[at + "print".len()..];
    }
    false
}

#[test]
fn the_library_writes_stdout_only_through_the_shared_writer() {
    let lines = production_lines();

    let through_writer = lines
        .iter()
        .filter(|l| l.text.contains("outln!(") || l.text.contains("out!("))
        .count();
    assert!(
        through_writer > 50,
        "{through_writer} writes through the shared writer found — the needles no longer match \
         the source shape, so emptiness below would prove nothing"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.file == "stdout.rs" && l.text.contains("io::stdout()")),
        "src/stdout.rs no longer reaches stdout — this scan is pointed at the wrong corpus"
    );

    let strays: Vec<String> = lines
        .iter()
        .filter(|l| !l.file.starts_with("bin/") && l.file != "stdout.rs")
        .filter(|l| prints_to_std_stdout(&l.text) || l.text.contains("stdout()"))
        .map(|l| format!("{} {}", l.site(), l.text.trim()))
        .collect();
    assert!(
        strays.is_empty(),
        "these write to stdout without the shared writer; use `out!`/`outln!` or \
         `stdout::write`: {strays:#?}"
    );
}
