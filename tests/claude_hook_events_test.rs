//! `rwv workweave --claude-hook` reads Claude Code's event name from stdin and
//! handles the workweave events. The same four names `rwv setup claude`
//! registers hooks under are the whole vocabulary: an event Claude Code does
//! not define is not an input, and the two a different hook registers are not
//! this command's.

use repoweave::setup::HookEvent;

mod common;

fn hook(payload: &str) -> (bool, String) {
    let out = common::rwv()
        .args(["workweave", "--claude-hook"])
        .write_stdin(payload.to_owned())
        .output()
        .expect("rwv runs");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn event_payload(event: HookEvent) -> String {
    serde_json::json!({ "hook_event_name": event }).to_string()
}

#[test]
fn the_events_a_hook_does_not_handle_are_refused_by_name() {
    for event in HookEvent::PRIME {
        let (ok, stderr) = hook(&event_payload(event));
        assert!(!ok, "{event} is not a workweave event");
        assert!(
            stderr.contains(&format!("`{event}` is not a workweave hook event")),
            "{event}: {stderr}"
        );
    }
}

#[test]
fn each_workweave_event_reaches_its_own_arm() {
    let wanted = [
        (HookEvent::WorktreeCreate, "missing cwd in hook input"),
        (
            HookEvent::WorktreeRemove,
            "missing worktree_path in hook input",
        ),
    ];
    assert_eq!(
        wanted.map(|(event, _)| event),
        HookEvent::WORKWEAVE,
        "an event handled here is an event `rwv setup claude` registers"
    );
    for (event, message) in wanted {
        let (ok, stderr) = hook(&event_payload(event));
        assert!(!ok, "{event} with no payload fields cannot succeed");
        assert!(stderr.contains(message), "{event}: {stderr}");
    }
}

#[test]
fn an_event_claude_code_does_not_define_does_not_parse() {
    for token in ["UnknownEvent", "worktree-create"] {
        let (ok, stderr) = hook(&format!(r#"{{"hook_event_name": "{token}"}}"#));
        assert!(!ok);
        assert!(stderr.contains(token), "{token}: {stderr}");
    }
}

#[test]
fn a_payload_with_no_event_is_refused() {
    let (ok, stderr) = hook("{}");
    assert!(!ok);
    assert!(
        stderr.contains("missing hook_event_name in hook input"),
        "{stderr}"
    );
}
