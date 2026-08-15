//! E2E tests for `rwv setup` subcommands (claude).

use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;

mod common;

// ============================================================================
// `rwv setup agents-md` does not parse — the subcommand is gone
// ============================================================================

#[test]
fn setup_agents_md_does_not_parse() {
    Command::cargo_bin("rwv")
        .unwrap()
        .args(["setup", "agents-md"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unrecognized subcommand"));
}

// ============================================================================
// 1. setup claude — errors when settings.json missing
// ============================================================================

#[test]
fn setup_claude_errors_without_settings() {
    let tmp = common::tempdir().unwrap();

    // Point HOME to a dir without .claude/settings.json
    Command::cargo_bin("rwv")
        .unwrap()
        .args(["setup", "claude"])
        .env("HOME", tmp.path())
        .env("USERPROFILE", tmp.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("does not exist"));
}

// ============================================================================
// 2. setup claude — registers hooks in empty settings
// ============================================================================

#[test]
fn setup_claude_registers_hooks() {
    let tmp = common::tempdir().unwrap();
    let claude_dir = tmp.path().join(".claude");
    fs::create_dir_all(&claude_dir).unwrap();
    fs::write(claude_dir.join("settings.json"), "{}").unwrap();

    Command::cargo_bin("rwv")
        .unwrap()
        .args(["setup", "claude"])
        .env("HOME", tmp.path())
        .env("USERPROFILE", tmp.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Registered"));

    let content: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(claude_dir.join("settings.json")).unwrap())
            .unwrap();

    // rwv prime registered for SessionStart + PreCompact
    for event in &["SessionStart", "PreCompact"] {
        let arr = content["hooks"][event].as_array().unwrap();
        assert!(!arr.is_empty(), "{event} hooks should be non-empty");
        let found = arr.iter().any(|g| {
            g["hooks"]
                .as_array()
                .map(|hs| {
                    hs.iter()
                        .any(|h| h["command"].as_str() == Some("rwv prime"))
                })
                .unwrap_or(false)
        });
        assert!(found, "{event} should contain rwv prime hook");
    }

    // WorktreeCreate + WorktreeRemove registered with rwv workweave --claude-hook
    for event in &["WorktreeCreate", "WorktreeRemove"] {
        let arr = content["hooks"][event]
            .as_array()
            .unwrap_or_else(|| panic!("{event} should be registered"));
        let found = arr.iter().any(|g| {
            g["hooks"]
                .as_array()
                .map(|hs| {
                    hs.iter()
                        .any(|h| h["command"].as_str() == Some("rwv workweave --claude-hook"))
                })
                .unwrap_or(false)
        });
        assert!(
            found,
            "{event} should contain 'rwv workweave --claude-hook'"
        );
    }

    // No hook scripts should be installed — rwv workweave --claude-hook is used directly.
    let hooks_dir = claude_dir.join("hooks");
    assert!(
        !hooks_dir.exists(),
        "hooks_dir should not be created (no shell scripts needed)"
    );
}

// ============================================================================
// 3. setup claude — idempotent
// ============================================================================

#[test]
fn setup_claude_idempotent() {
    let tmp = common::tempdir().unwrap();
    let claude_dir = tmp.path().join(".claude");
    fs::create_dir_all(&claude_dir).unwrap();
    fs::write(claude_dir.join("settings.json"), "{}").unwrap();

    Command::cargo_bin("rwv")
        .unwrap()
        .args(["setup", "claude"])
        .env("HOME", tmp.path())
        .env("USERPROFILE", tmp.path())
        .assert()
        .success();

    let first = fs::read_to_string(claude_dir.join("settings.json")).unwrap();

    Command::cargo_bin("rwv")
        .unwrap()
        .args(["setup", "claude"])
        .env("HOME", tmp.path())
        .env("USERPROFILE", tmp.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("up to date"));

    let second = fs::read_to_string(claude_dir.join("settings.json")).unwrap();
    assert_eq!(first, second, "second run should not modify the file");
}

// ============================================================================
// 4. setup claude — no hook scripts installed (uses rwv workweave --claude-hook)
// ============================================================================

#[test]
fn setup_claude_does_not_create_hooks_dir() {
    // rwv setup claude no longer installs shell scripts; it registers
    // `rwv workweave --claude-hook` directly. Verify no scripts are installed.
    let tmp = common::tempdir().unwrap();
    let claude_dir = tmp.path().join(".claude");
    fs::create_dir_all(&claude_dir).unwrap();
    fs::write(claude_dir.join("settings.json"), "{}").unwrap();

    Command::cargo_bin("rwv")
        .unwrap()
        .args(["setup", "claude"])
        .env("HOME", tmp.path())
        .env("USERPROFILE", tmp.path())
        .assert()
        .success();

    let hooks_dir = claude_dir.join("hooks");
    assert!(
        !hooks_dir.exists(),
        "hooks_dir should not be created (scripts not needed with --claude-hook)"
    );
}
