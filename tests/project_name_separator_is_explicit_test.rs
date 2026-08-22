//! Pins that `project_name_from_dir` never renders a path to text — the `/`
//! between a nested project's segments is written by this code, not chosen by
//! the host.
//!
//! A project name is an identifier before it is a path: `ProjectName::new`
//! validates it as a git ref name, and that char class rejects a backslash.
//! `PathBuf` joins components with `std::path::MAIN_SEPARATOR`, and
//! `to_string_lossy` on a `Path` hands back whatever separator its source was
//! spelled with. Either route makes the identifier's spelling a property of
//! the machine or the caller, so on Windows a nested project renders as
//! `chatly\web-app` and cannot construct a `ProjectName` at all.
//!
//! The pin is structural because the defect is invisible to a run here.
//! `MAIN_SEPARATOR` is already `/` on this platform, so the two renderings are
//! byte-identical and every behavioural assertion holds equally before and
//! after the fix — measured, not assumed: reverting the second arm leaves the
//! whole suite green, while poisoning that arm's separator reddens two tests,
//! so the arm is reached and the green is equality rather than dead code. The
//! source, unlike the run, is the same on both platforms, so a scan sees what
//! no local execution can.
//!
//! **What this does not buy.** It asserts the code does not take the
//! host-dependent route. It does not assert the output is correct on Windows —
//! only a run there says that, and the advisory workflow is the one instrument
//! that performs it. The two claims are different and neither replaces the
//! other.
//!
//! The function reads one way, not two. It used to answer from a
//! weave-relative path by stripping a leading `projects/` and from an absolute
//! one by searching for the last `projects` component — readings that disagree
//! for a name that itself contains that segment. It now strips the root's
//! projects directory and nothing else, so there is a single body to scan.
//!
//! Residue. The body is located by `body_of`, keyed on the function's name
//! rather than its position, so a declaration inserted or removed around it
//! does not change what is scanned. The needles name three spellings of
//! "render a path as text"; a fourth reached some other way — a helper that
//! renders internally, a `Display` impl, `OsString` concatenation — is not
//! their shape. `src_scan`'s comment filter is line-leading `//` only, so a
//! needle inside a trailing or block comment reads as a live use.

mod common;

use common::src_scan::{body_of, production_lines};

const OWNER: &str = "workspace.rs";
const TARGET_FN: &str = "project_name_from_dir";

/// The spellings that turn a `Path` or `PathBuf` into text using the host's
/// separator. Each must still occur somewhere in the owner file, which the
/// vacuity guard asserts — a rename that emptied the corpus would otherwise
/// leave the main assertion passing over nothing.
const RENDER_NEEDLES: &[&str] = &["to_string_lossy", ".display()", "PathBuf"];

/// The helper that writes the separator explicitly.
const EXPLICIT_JOIN: &str = "slash_separated(";

#[test]
fn the_scan_reaches_the_pinned_body() {
    let lines = production_lines();
    assert!(
        lines.iter().any(|l| l.file == OWNER),
        "no production lines scanned from src/{OWNER} — every assertion below \
         would hold vacuously"
    );

    let body = body_of(&lines, OWNER, TARGET_FN);
    assert!(
        !body.is_empty(),
        "src/{OWNER} no longer defines `{TARGET_FN}` with a non-empty body — \
         the function was renamed, removed, or emptied, and this pin names \
         the wrong target"
    );

    for needle in RENDER_NEEDLES {
        assert!(
            lines
                .iter()
                .any(|l| l.file == OWNER && l.text.contains(needle)),
            "src/{OWNER} no longer spells `{needle}` anywhere — the needle was \
             renamed or removed, so finding none of it inside the body below \
             would prove nothing"
        );
    }
}

#[test]
fn the_name_is_not_rendered_from_a_path() {
    let lines = production_lines();
    let body = body_of(&lines, OWNER, TARGET_FN);

    let strays: Vec<String> = body
        .iter()
        .filter(|l| RENDER_NEEDLES.iter().any(|n| l.text.contains(n)))
        .map(|l| format!("{} {}", l.site(), l.text.trim()))
        .collect();

    assert!(
        strays.is_empty(),
        "`project_name_from_dir` renders a project name, and a name is an \
         identifier rather than a path: `ProjectName::new` validates it as a \
         git ref name, whose char class rejects the backslash a `PathBuf` join \
         writes on Windows. Rendering a `Path` here — by collecting components \
         into a `PathBuf`, by `to_string_lossy`, or by `.display()` — hands the \
         separator to the host or to whatever spelling the caller passed in, \
         and a nested project then cannot be loaded there at all. Join the \
         components explicitly instead.\n\
         \n\
         Found: {strays:#?}"
    );
}

#[test]
fn the_name_is_joined_explicitly() {
    let lines = production_lines();
    let body = body_of(&lines, OWNER, TARGET_FN);

    let calls = body
        .iter()
        .filter(|l| l.text.contains(EXPLICIT_JOIN))
        .count();

    assert!(
        calls >= 1,
        "`project_name_from_dir` must write the separator itself. Found \
         {calls} call(s) to `{EXPLICIT_JOIN}` in the body, so the name is \
         produced some other way. The absence assertion above cannot see that \
         on its own: a body that stopped returning a name at all would \
         satisfy it."
    );
}
