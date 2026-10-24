use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{bail, Context};
use serde::Deserialize;

use crate::wire::Role;

#[derive(Debug, Clone)]
pub struct Repo {
    pub path: String,
    pub role: Role,
    pub dir: PathBuf,
    pub present: bool,
}

/// The `RWV_*` envelope rwv set on this process when it dispatched `rwv each`.
#[derive(Debug, Default)]
pub struct Envelope {
    pub workspace: Option<OsString>,
    pub workweave: Option<OsString>,
    pub workweave_unregistered: bool,
    pub project: Option<OsString>,
}

impl Envelope {
    pub fn from_process() -> Self {
        Self {
            workspace: std::env::var_os("RWV_WORKSPACE"),
            workweave: std::env::var_os("RWV_WORKWEAVE"),
            workweave_unregistered: std::env::var_os("RWV_WORKWEAVE_UNREGISTERED").is_some(),
            project: std::env::var_os("RWV_PROJECT"),
        }
    }

    /// The `rwv status` invocation, without its selectors, that reports the
    /// checkout this plugin was dispatched from.
    pub fn status_args(&self) -> anyhow::Result<Vec<OsString>> {
        let Some(workspace) = &self.workspace else {
            bail!("no workspace resolved: run this from inside a workspace, or address one with rwv's -C <path>");
        };
        let mut args: Vec<OsString> = vec!["-C".into(), workspace.clone()];
        if let Some(workweave) = &self.workweave {
            args.extend([
                "-w".into(),
                workweave.clone(),
                "status".into(),
                "--json".into(),
            ]);
        } else if self.workweave_unregistered {
            bail!("this workweave is not registered, so it cannot be addressed by name and its repos cannot be told from the primary's; run `rwv doctor --fix` to register it");
        } else if let Some(project) = &self.project {
            args.extend([
                "status".into(),
                "--json".into(),
                "--project".into(),
                project.clone(),
            ]);
        } else {
            bail!("no project resolved for this workspace; activate one with `rwv activate <project>`");
        }
        Ok(args)
    }
}

#[derive(Debug, Deserialize)]
struct StatusEnvelope {
    repos: Vec<StatusRepo>,
}

#[derive(Debug, Deserialize)]
struct StatusRepo {
    path: String,
    role: Role,
    absolute_path: PathBuf,
    relation: Relation,
}

/// How a checkout stands against its lock, as `rwv status --json` spells it.
/// Only `missing` changes what this plugin does, but the set is closed so that
/// a token it does not know is output it refuses to read rather than a repo it
/// reads as present.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Relation {
    Ok,
    Ahead,
    Behind,
    Diverged,
    NoLock,
    Unknown,
    Missing,
    Unreachable,
}

pub fn select(
    rwv: &OsStr,
    envelope: &Envelope,
    roles: &[String],
    selectors: &[String],
) -> anyhow::Result<Vec<Repo>> {
    let mut command = Command::new(rwv);
    command.args(envelope.status_args()?);
    for role in roles {
        command.args(["--role", role]);
    }
    for selector in selectors {
        command.args(["--repo", selector]);
    }
    let output = command
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()
        .with_context(|| format!("failed to run `{}`", rwv.to_string_lossy()))?;
    if !output.status.success() {
        bail!("`rwv status` did not resolve the repos to run in (its error is above)");
    }
    let status: StatusEnvelope = serde_json::from_slice(&output.stdout)
        .context("`rwv status --json` printed output this rwv-each cannot read")?;
    let repos = apply_default_scope(status.repos, roles.is_empty() && selectors.is_empty());
    if repos.is_empty() {
        bail!("{}", nothing_matched(roles, selectors));
    }
    Ok(repos)
}

fn apply_default_scope(repos: Vec<StatusRepo>, default_scope: bool) -> Vec<Repo> {
    repos
        .into_iter()
        .filter(|r| !default_scope || r.role != Role::Reference)
        .map(|r| Repo {
            present: r.relation != Relation::Missing,
            path: r.path,
            role: r.role,
            dir: r.absolute_path,
        })
        .collect()
}

fn nothing_matched(roles: &[String], selectors: &[String]) -> String {
    if roles.is_empty() && selectors.is_empty() {
        return format!(
            "no repo to run in: with no --role or --repo, every repo except role {reference} runs, and this project has none; pass --role {reference} to include reference repos",
            reference = Role::Reference

        );
    }
    let named: Vec<String> = roles
        .iter()
        .map(|r| format!("--role {r}"))
        .chain(selectors.iter().map(|s| format!("--repo {s}")))
        .collect();
    format!("no repo matched {}", named.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status_repo(path: &str, role: Role, relation: Relation) -> StatusRepo {
        StatusRepo {
            path: path.into(),
            role,
            absolute_path: PathBuf::from("/ws").join(path),
            relation,
        }
    }

    fn status_json(role: &str, relation: &str) -> String {
        format!(
            r#"{{"repos": [{{"path": "a", "role": "{role}", "absolute_path": "/ws/a", "relation": "{relation}"}}]}}"#
        )
    }

    #[test]
    fn the_default_scope_drops_reference_repos_and_selectors_do_not() {
        let repos = || {
            vec![
                status_repo("a", Role::Owned, Relation::Ok),
                status_repo("b", Role::Reference, Relation::Ok),
            ]
        };
        let paths = |rs: Vec<Repo>| rs.into_iter().map(|r| r.path).collect::<Vec<_>>();
        assert_eq!(paths(apply_default_scope(repos(), true)), ["a"]);
        assert_eq!(paths(apply_default_scope(repos(), false)), ["a", "b"]);
    }

    #[test]
    fn a_missing_checkout_is_not_present() {
        let repos = apply_default_scope(
            vec![
                status_repo("a", Role::Owned, Relation::Missing),
                status_repo("b", Role::Owned, Relation::Behind),
            ],
            true,
        );
        assert!(!repos[0].present);
        assert!(repos[1].present);
    }

    #[test]
    fn every_role_and_relation_status_reports_is_read() {
        for role in ["owned", "fork", "dependency", "reference"] {
            for relation in [
                "ok",
                "ahead",
                "behind",
                "diverged",
                "no-lock",
                "unknown",
                "missing",
                "unreachable",
            ] {
                let status: StatusEnvelope = serde_json::from_str(&status_json(role, relation))
                    .unwrap_or_else(|e| {
                        panic!("{role} {relation} was refused: {e}");
                    });
                assert_eq!(status.repos.len(), 1);
            }
        }
    }

    #[test]
    fn a_token_the_plugin_does_not_know_is_output_it_refuses_to_read() {
        for (role, relation, unknown) in [
            ("owned", "elsewhere", "elsewhere"),
            ("owned", "no_lock", "no_lock"),
            ("primary", "ok", "primary"),
            ("Owned", "ok", "Owned"),
        ] {
            let err = serde_json::from_str::<StatusEnvelope>(&status_json(role, relation))
                .expect_err("an unknown token is not a status");
            assert!(err.to_string().contains(unknown), "{err}");
        }
    }

    #[test]
    fn an_empty_selection_names_every_selector() {
        let msg = nothing_matched(&["fork".into()], &["glob:x/*".into()]);
        assert_eq!(msg, "no repo matched --role fork --repo glob:x/*");
    }

    #[test]
    fn an_unregistered_workweave_is_refused_rather_than_addressed_as_primary() {
        let envelope = Envelope {
            workspace: Some("/ws".into()),
            workweave: None,
            workweave_unregistered: true,
            project: Some("p".into()),
        };
        let err = envelope.status_args().unwrap_err().to_string();
        assert!(err.contains("rwv doctor --fix"), "{err}");
    }

    #[test]
    fn a_workweave_is_addressed_by_name_and_primary_by_project() {
        let workweave = Envelope {
            workspace: Some("/ws".into()),
            workweave: Some("p--x".into()),
            workweave_unregistered: false,
            project: Some("p".into()),
        };
        assert_eq!(
            workweave.status_args().unwrap(),
            ["-C", "/ws", "-w", "p--x", "status", "--json"].map(OsString::from)
        );
        let primary = Envelope {
            workweave: None,
            ..workweave
        };
        assert_eq!(
            primary.status_args().unwrap(),
            ["-C", "/ws", "status", "--json", "--project", "p"].map(OsString::from)
        );
    }
}
