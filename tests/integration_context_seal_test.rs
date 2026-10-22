//! An integration's context is built only from a `WorkspaceSession`, and an
//! issue's message is only ever rendered.
//!
//! `IntegrationContextBase::new` derives the project directory from the
//! session's root, so no caller can bind a context to a directory of its own
//! choosing; `IntegrationContextBase::build_context` is the one way to a
//! context. `IssueMessage` offers `Display` and nothing else, so a finding is
//! told apart by its kind rather than by searching its words. Pinned by
//! diagnostic code, from an external crate, where every visibility narrower
//! than `pub` looks alike.
//!
//! Residue: privacy stops at `integration.rs`, where both literals are
//! writable, and at `workspace.rs` for the session's.

mod common;

use common::compile_probe::{assert_fails_with, compile};

#[test]
fn the_harness_can_build_a_context_and_render_a_message() {
    // Control. Everything below asserts a failure, so a broken rustc
    // invocation would make them all pass for the wrong reason.
    let (compiled, stderr) = compile(
        r#"
        use repoweave::integration::{IntegrationContextBase, Issue};
        use repoweave::manifest::{IntegrationConfig, Manifest, ProjectName};
        use repoweave::workspace::WorkspaceSession;
        use std::collections::HashMap;
        use std::path::{Path, PathBuf};

        pub fn context_dir(
            root: &Path,
            project: &ProjectName,
            config: &IntegrationConfig,
            manifest: &Manifest,
        ) -> PathBuf {
            let session = WorkspaceSession::new(root);
            let cache = HashMap::new();
            let base = IntegrationContextBase::new(&session, project, &cache, None);
            let ctx = base.build_context(config, manifest);
            ctx.output_dir().to_path_buf()
        }

        pub fn says(issue: &Issue, needle: &str) -> bool {
            issue.message.to_string().contains(needle)
        }
        "#,
    );
    assert!(compiled, "control snippet must compile; got:\n{stderr}");
}

#[test]
fn a_context_base_cannot_be_written_as_a_literal() {
    assert_fails_with(
        "E0451",
        "IntegrationContextBase's fields are private to integration.rs",
        r#"
        use repoweave::integration::IntegrationContextBase;
        use repoweave::manifest::ProjectName;
        use repoweave::workspace::WorkspaceSession;
        use std::collections::HashMap;
        use std::path::PathBuf;

        pub fn build<'a>(
            session: &'a WorkspaceSession,
            project: &'a ProjectName,
            detection_cache: &'a HashMap<String, Vec<String>>,
            output_dir: PathBuf,
        ) -> IntegrationContextBase<'a> {
            IntegrationContextBase {
                session,
                output_dir,
                project,
                detection_cache,
                workweave: None,
            }
        }
        "#,
    );
}

#[test]
fn a_context_cannot_be_written_as_a_literal() {
    assert_fails_with(
        "E0451",
        "IntegrationContext's fields are private to integration.rs",
        r#"
        use repoweave::integration::{IntegrationContext, IntegrationContextBase};
        use repoweave::manifest::IntegrationConfig;

        pub fn build<'a>(
            base: &'a IntegrationContextBase<'a>,
            config: &'a IntegrationConfig,
        ) -> IntegrationContext<'a> {
            IntegrationContext {
                base,
                repos: Vec::new(),
                config,
            }
        }
        "#,
    );
}

#[test]
fn a_session_cannot_be_written_as_a_literal() {
    assert_fails_with(
        "E0451",
        "WorkspaceSession's fields are private to workspace.rs",
        r#"
        use repoweave::workspace::{ContainerKind, WorkspaceSession};
        use std::path::PathBuf;

        pub fn build(root: PathBuf) -> WorkspaceSession {
            WorkspaceSession {
                root,
                container_kind: ContainerKind::Primary,
                repos_on_disk: Vec::new(),
                project_paths: Vec::new(),
            }
        }
        "#,
    );
}

#[test]
fn a_message_cannot_be_searched_in_place() {
    assert_fails_with(
        "E0599",
        "IssueMessage has no str methods",
        r#"
        use repoweave::integration::Issue;

        pub fn says(issue: &Issue, needle: &str) -> bool {
            issue.message.contains(needle)
        }
        "#,
    );
}

#[test]
fn a_message_cannot_be_borrowed_as_a_str() {
    assert_fails_with(
        "E0308",
        "IssueMessage does not deref to str",
        r#"
        use repoweave::integration::Issue;

        pub fn text(issue: &Issue) -> &str {
            &issue.message
        }
        "#,
    );
}
