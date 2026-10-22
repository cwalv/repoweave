//! A path reaches `git`'s argv only as a `GitArg`, and only `GitArg::from_path`
//! makes one. What this pins is code that does not compile, which no run of
//! the binary can show, so the pins are compile-fail probes.
//!
//! Each probe includes `src/git/argv.rs` by `#[path]`, so the snippet stands
//! where `src/git.rs` stands relative to that module. That is what lets a probe
//! see a field widened to `pub(super)`: from outside the crate, `pub(super)`
//! and private refuse alike. The module names nothing outside `std`, which is
//! what allows this.
//!
//! The command-line side is closed by the type: `GitCommand` keeps its
//! `Command` private and takes arguments only as an `Argv`, whose text slot
//! takes `AsRef<str>` and whose path slot takes `&GitArg`. A `&Path`, an
//! `OsStr` or a `PathBuf` fits neither.
//!
//! Not pinned: a path turned into a `&str` by hand (`to_str()`) and given to a
//! text slot — a `&str` carries no record of where it came from, and nothing
//! in this crate can make one that does. And the one door out of the module,
//! `GitCommand::into_command`, is a `Command` that takes any argument; the
//! count test below holds how many production lines in `src/git.rs` open it.

mod common;

use std::path::{Path, PathBuf};

use common::compile_probe::{assert_fails_with, compile};
use common::src_scan::production_lines;

fn argv_rs() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src/git/argv.rs")
}

fn including(module: &Path, body: &str) -> String {
    format!("#[path = {module:?}]\nmod argv;\n{body}")
}

fn refuses(code: &str, what: &str, body: &str) {
    assert_fails_with(code, what, &including(&argv_rs(), body));
}

const FORGES_A_GIT_ARG: &str = r#"
    pub fn forge() -> argv::GitArg {
        argv::GitArg { text: String::from("C:/anywhere") }
    }
"#;

const READS_THE_COMMAND: &str = r#"
    pub fn peek(c: &mut argv::GitCommand) -> &mut std::process::Command {
        &mut c.command
    }
"#;

#[test]
fn the_harness_can_compile_a_legal_use_of_the_module() {
    let (compiled, stderr) = compile(&including(
        &argv_rs(),
        r#"
            pub fn legal(p: &std::path::Path) -> Option<std::io::Result<std::process::Output>> {
                let dest = argv::GitArg::from_path(p, "destination path").ok()?;
                Some(
                    argv::GitCommand::new()
                        .args(argv::Argv::from(["clone", "--quiet"]).text("url").path(&dest))
                        .args(["--progress"])
                        .env("GIT_TERMINAL_PROMPT", "0")
                        .current_dir(p)
                        .output(),
                )
            }
            "#,
    ));
    assert!(compiled, "control snippet must compile; got:\n{stderr}");
}

#[test]
fn a_path_is_not_a_word_of_an_argv() {
    refuses(
        "E0277",
        "a `&Path` is not `AsRef<str>`, so it cannot be collected into an `Argv`",
        r#"
        pub fn words(p: &std::path::Path) -> argv::Argv {
            argv::Argv::from([p])
        }
        "#,
    );
}

#[test]
fn a_path_does_not_fill_a_text_slot() {
    refuses(
        "E0277",
        "`Argv::text` takes `AsRef<str>`",
        r#"
        pub fn text(p: &std::path::Path) -> argv::Argv {
            argv::Argv::from(["clone"]).text(p)
        }
        "#,
    );
}

#[test]
fn a_path_does_not_fill_a_path_slot() {
    refuses(
        "E0308",
        "`Argv::path` takes a `&GitArg`, which only `GitArg::from_path` makes",
        r#"
        pub fn slot(p: &std::path::Path) -> argv::Argv {
            argv::Argv::from(["clone"]).path(p)
        }
        "#,
    );
}

#[test]
fn text_does_not_fill_a_path_slot() {
    refuses(
        "E0308",
        "a string literal is not a `GitArg`",
        r#"
        pub fn slot() -> argv::Argv {
            argv::Argv::from(["clone"]).path("C:/anywhere")
        }
        "#,
    );
}

#[test]
fn a_git_arg_does_not_fill_a_text_slot() {
    refuses(
        "E0277",
        "a `GitArg` goes in the path slot, so a reader can tell the locations of an argv apart",
        r#"
        pub fn slot(g: &argv::GitArg) -> argv::Argv {
            argv::Argv::from(["clone"]).text(g)
        }
        "#,
    );
}

#[test]
fn a_command_takes_no_argument_but_an_argv() {
    refuses(
        "E0277",
        "`GitCommand::args` takes `Into<Argv>`, which a `&Path` is not",
        r#"
        pub fn spawn(p: &std::path::Path) {
            let _ = argv::GitCommand::new().args(p);
        }
        "#,
    );
    refuses(
        "E0599",
        "`GitCommand` has no `arg` that takes `AsRef<OsStr>`",
        r#"
        pub fn spawn(p: &std::path::Path) {
            let _ = argv::GitCommand::new().arg(p);
        }
        "#,
    );
}

#[test]
fn a_git_arg_cannot_be_made_from_text() {
    refuses(
        "E0277",
        "there is no `From<&str>` for `GitArg`",
        r#"
        pub fn from_text() -> argv::GitArg {
            "C:/anywhere".into()
        }
        "#,
    );
}

#[test]
fn a_git_arg_cannot_be_assembled_field_by_field() {
    refuses(
        "E0451",
        "`from_path` is the only way to make a `GitArg`",
        FORGES_A_GIT_ARG,
    );
}

#[test]
fn the_command_inside_a_git_command_cannot_be_reached() {
    refuses(
        "E0616",
        "the `Command` is private to argv.rs; `into_command` is the one door out",
        READS_THE_COMMAND,
    );
}

#[test]
fn the_probes_see_a_field_widened_to_pub_super() {
    let source = std::fs::read_to_string(argv_rs()).unwrap();
    let mut widened = source.clone();
    for (private, visible) in [
        ("    text: String,\n}", "    pub(super) text: String,\n}"),
        (
            "    command: Command,\n}",
            "    pub(super) command: Command,\n}",
        ),
    ] {
        assert_eq!(
            widened.matches(private).count(),
            1,
            "argv.rs no longer spells `{private}` once"
        );
        widened = widened.replacen(private, visible, 1);
    }
    let tmp = common::tempdir().unwrap();
    let copy = tmp.path().join("argv.rs");
    std::fs::write(&copy, widened).unwrap();

    for (what, body) in [
        ("forging a GitArg", FORGES_A_GIT_ARG),
        ("reading the command", READS_THE_COMMAND),
    ] {
        let (compiled, stderr) = compile(&including(&copy, body));
        assert!(
            compiled,
            "the {what} probe must compile against a widened copy, or it cannot \
             tell that widening from private:\n{stderr}"
        );
    }
}

#[test]
fn the_door_out_of_the_command_has_one_production_caller() {
    let callers: Vec<String> = production_lines()
        .into_iter()
        .filter(|l| l.file == "git.rs" && l.text.contains(".into_command()"))
        .map(|l| format!("{} {}", l.site(), l.text.trim()))
        .collect();
    assert_eq!(
        callers.len(),
        1,
        "`into_command` hands back a `Command` that takes any argument, so each \
         production use is a place a path can bypass `GitArg`. The one use is \
         `fetch_all_prune_command`, whose caller spawns through the reporter. \
         Found: {callers:#?}"
    );
}
