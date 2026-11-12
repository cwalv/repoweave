//! Running `rwv` and judging what it printed.
//!
//! [`refused`] is the one assertion of a refusal: the census in
//! `tests/refusal_census_test.rs` credits a site as read only where a test
//! names the site's state or token as this function's second argument,
//! spelled `State::<Variant>` or `RefusalKind::<Variant>`.

use repoweave::diagnosis::State;
use repoweave::refusal::RefusalKind;
use std::path::Path;
use std::process::Output;

pub fn run(args: &[&str], dir: &Path) -> Output {
    super::rwv().args(args).current_dir(dir).output().unwrap()
}

pub fn describe(out: &Output) -> String {
    format!(
        "status {:?}\nstdout:\n{}\nstderr:\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// Runs `rwv` and requires exit 0.
pub fn succeed(args: &[&str], dir: &Path) -> Output {
    let out = run(args, dir);
    assert!(
        out.status.success(),
        "`rwv {}` failed:\n{}",
        args.join(" "),
        describe(&out)
    );
    out
}

/// What a refusal names: a judged state, or a token no axis carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    Judged(State),
    Token(RefusalKind),
}

impl From<State> for Refusal {
    fn from(state: State) -> Self {
        Refusal::Judged(state)
    }
}

impl From<RefusalKind> for Refusal {
    fn from(kind: RefusalKind) -> Self {
        Refusal::Token(kind)
    }
}

/// The route tokens of the block of `rwv explain <token>` lines that ends
/// `stderr` after a blank line, or `None` where stderr does not end in one.
pub fn route_block(stderr: &str) -> Option<Vec<&str>> {
    let lines: Vec<&str> = stderr.strip_suffix('\n')?.split('\n').collect();
    let routes = lines
        .iter()
        .rev()
        .take_while(|l| l.starts_with("rwv explain "))
        .count();
    let blank = lines.len().checked_sub(routes + 1)?;
    (routes > 0 && lines[blank].is_empty()).then(|| {
        lines[blank + 1..]
            .iter()
            .map(|l| &l["rwv explain ".len()..])
            .collect()
    })
}

/// Whether `line` is a judged line for `state`: `…: <axis> <state>`, with
/// the subject in parentheses after it where the state names one.
pub fn is_judged_line(line: &str, state: State) -> bool {
    let judged = format!(": {} {}", state.axis().token(), state.name());
    line.match_indices(&judged).any(|(at, _)| {
        let rest = &line[at + judged.len()..];
        rest.is_empty() || (rest.starts_with(" (") && rest.ends_with(')'))
    })
}

/// Requires `out` to be the refusal `expected` names, and returns its stderr.
///
/// A judged state: exit 1, a line judging that state, and its axis among the
/// route lines that end the output. A token: exit 1 and exactly one route
/// line, naming it, last.
pub fn refused(out: &Output, expected: impl Into<Refusal>) -> String {
    let expected = expected.into();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert_eq!(
        out.status.code(),
        Some(1),
        "expected the refusal {expected:?}:\n{}",
        describe(out)
    );
    let routes = route_block(&stderr).unwrap_or_default();
    match expected {
        Refusal::Judged(state) => {
            assert!(
                stderr.lines().any(|l| is_judged_line(l, state)),
                "no line judges `{} {}`:\n{stderr}",
                state.axis().token(),
                state.name()
            );
            let axis = state.axis().token();
            assert!(
                routes.contains(&axis.as_str()),
                "the route lines ending stderr do not name `{axis}`:\n{stderr}"
            );
        }
        Refusal::Token(kind) => {
            let token = kind.token();
            let every_route: Vec<&str> = stderr
                .lines()
                .filter_map(|l| l.strip_prefix("rwv explain "))
                .collect();
            assert_eq!(
                (every_route, routes),
                (vec![token.as_str()], vec![token.as_str()]),
                "expected exactly one route line, last, naming `{token}`:\n{stderr}"
            );
        }
    }
    stderr
}
