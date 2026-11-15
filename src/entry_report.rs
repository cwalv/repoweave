//! The member-entry changes a workspace stands in: each field of an entry the
//! checked-out branch holds that differs from what was published for the
//! branch, or from what the workspace last aligned it by, reported with the
//! previous and the new value until the push branch carries it or it is
//! restored.

use crate::alignment::{Aligner, Mode};
use crate::manifest::{Manifest, RepoEntry, RepoPath, Role};
use crate::vcs::{HeadAttachment, RawRefName, Vcs};
use crate::workspace::WorkspaceContext;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FieldChange {
    pub(crate) name: &'static str,
    pub(crate) was: String,
    pub(crate) now: String,
}

#[derive(Debug, Clone)]
pub(crate) struct MemberChange {
    pub(crate) path: RepoPath,
    pub(crate) was: RepoEntry,
    pub(crate) now: RepoEntry,
    pub(crate) fields: Vec<FieldChange>,
}

impl MemberChange {
    pub(crate) fn changed(&self, name: &str) -> bool {
        self.fields.iter().any(|field| field.name == name)
    }
}

/// What the workspace stands in for the checked-out branch.
#[derive(Debug, Default)]
pub(crate) struct Standing {
    pub(crate) changes: Vec<MemberChange>,
    /// Whether the entries the workspace stores for the branch become the
    /// entries now: nothing was stored, or nothing differs.
    pub(crate) store: bool,
}

pub(crate) fn fields_between(was: &RepoEntry, now: &RepoEntry) -> Vec<FieldChange> {
    let mut fields = Vec::new();
    let mut note = |name: &'static str, was: String, now: String| {
        if was != now {
            fields.push(FieldChange { name, was, now });
        }
    };
    note("url", was.url.to_string(), now.url.to_string());
    note("branch", was.branch.to_string(), now.branch.to_string());
    note("role", was.role.to_string(), now.role.to_string());
    note(
        "recurse-submodules",
        bool::from(was.recurse_submodules).to_string(),
        bool::from(now.recurse_submodules).to_string(),
    );
    note(
        "history",
        crate::set_history::spelled(was.history),
        crate::set_history::spelled(now.history),
    );
    fields
}

pub(crate) fn between(
    before: &BTreeMap<String, RepoEntry>,
    after: &BTreeMap<String, RepoEntry>,
) -> Vec<MemberChange> {
    let mut changes = Vec::new();
    for (path, now) in after {
        let Some(was) = before.get(path) else {
            continue;
        };
        let fields = fields_between(was, now);
        if fields.is_empty() {
            continue;
        }
        let Ok(path) = RepoPath::new(path.clone()) else {
            continue;
        };
        changes.push(MemberChange {
            path,
            was: was.clone(),
            now: now.clone(),
            fields,
        });
    }
    changes
}

pub(crate) fn entries_of(manifest: &Manifest) -> BTreeMap<String, RepoEntry> {
    manifest
        .iter_entries()
        .map(|(path, entry)| (path.to_string(), entry.clone()))
        .collect()
}

/// The entries the project branch's push branch carries, as last fetched.
fn published(vcs: &dyn Vcs, project_dir: &Path) -> Option<BTreeMap<String, RepoEntry>> {
    let HeadAttachment::Attached(attached) = vcs.head_attachment(project_dir).ok()? else {
        return None;
    };
    let url_remote = vcs.project_url_remote(project_dir).ok()?;
    let target = vcs.push_target(&attached, url_remote.as_ref()).ok()??;
    let tip = vcs.push_branch(&attached, &target).ok()??;
    let text = vcs
        .read_file_at_revision(project_dir, &tip, Path::new(Manifest::FILE_NAME))
        .ok()?;
    Manifest::from_other_commit(&text)
        .ok()
        .map(|manifest| entries_of(&manifest))
}

/// What the checked-out branch's entries differ in from what the push branch
/// carries, or, for a branch with none, from what the workspace stored.
pub(crate) fn standing(
    vcs: &dyn Vcs,
    project_dir: &Path,
    aligner: &Aligner,
    current: &Manifest,
) -> Standing {
    let now = entries_of(current);
    let baseline = published(vcs, project_dir).or_else(|| {
        aligner
            .checked_out()
            .and_then(|branch| aligner.state.stored.get(branch))
            .map(|stored| stored.baseline.clone())
    });
    match baseline {
        None => Standing {
            changes: Vec::new(),
            store: true,
        },
        Some(baseline) => {
            let changes = between(&baseline, &now);
            Standing {
                store: changes.is_empty(),
                changes,
            }
        }
    }
}

fn member_dir(ctx: &WorkspaceContext, path: &RepoPath) -> std::path::PathBuf {
    let workweave_dir = match &ctx.checkout {
        crate::workspace::Checkout::Workweave { dir, .. } => Some(dir.as_path().to_path_buf()),
        crate::workspace::Checkout::Primary { .. } => None,
    };
    crate::workspace::member_checkout_dir(path, ctx.primary_path(), workweave_dir.as_deref())
        .path()
        .to_path_buf()
}

/// The commits unpushed on the branch a declared-branch change leaves, less
/// those whose change the new branch already carries.
fn unpushed_on_old_branch(
    ctx: &WorkspaceContext,
    change: &MemberChange,
) -> Vec<crate::vcs::CommitSummary> {
    if !change.changed("branch") {
        return Vec::new();
    }
    let vcs = crate::vcs::vcs_for(change.was.vcs_type);
    let dir = member_dir(ctx, &change.path);
    let old = RawRefName::new(change.was.branch.local_counterpart().as_str());
    let unpushed = vcs.unpushed_commits(&dir, &old).unwrap_or_default();
    let new = RawRefName::new(change.now.branch.local_counterpart().as_str());
    let tips = (
        vcs.resolve_local_branch_tip(&dir, &old).ok().flatten(),
        vcs.resolve_local_branch_tip(&dir, &new).ok().flatten(),
    );
    let (Some(old_tip), Some(new_tip)) = tips else {
        return unpushed;
    };
    let Some(found) = vcs.patch_equivalents(&dir, old_tip.id(), new_tip.id()) else {
        return unpushed;
    };
    unpushed
        .into_iter()
        .filter(|commit| !found.matched.iter().any(|(ours, _)| *ours == commit.id))
        .collect()
}

/// The report of `standing`, one block per changed member, for `verb`.
pub(crate) fn render(
    verb: &str,
    ctx: &WorkspaceContext,
    aligner: &Aligner,
    standing: &Standing,
    list_choices: bool,
) -> String {
    use crate::diagnosis::Form;
    let mut out = String::new();
    let in_feature = matches!(aligner.mode, Mode::Feature { .. });
    for change in &standing.changes {
        let fields: Vec<String> = change
            .fields
            .iter()
            .map(|field| format!("{}: {} -> {}", field.name, field.was, field.now))
            .collect();
        out.push_str(&format!(
            "rwv {verb}: {}: member-changed {}\n",
            change.path,
            fields.join(", ")
        ));
        let declared_follows =
            !in_feature || matches!(change.now.role, Role::Dependency | Role::Reference);
        if declared_follows {
            out.push_str("    follows the merged declared branches; not yet published\n");
        } else {
            out.push_str("    a landing decides it; not yet published\n");
        }
        let unpushed = unpushed_on_old_branch(ctx, change);
        if !unpushed.is_empty() {
            out.push_str(&format!(
                "    unpushed on {}:\n",
                change.was.branch.local_counterpart().as_str()
            ));
            for commit in &unpushed {
                out.push_str(&format!("      {} {}\n", commit.id.short(), commit.subject));
            }
        }
        if in_feature {
            continue;
        }
        let vcs = crate::vcs::vcs_for(change.now.vcs_type);
        let mut bindings = BTreeMap::new();
        bindings.insert("<m>", change.path.to_string());
        bindings.insert("<rev>", "<rev>".to_owned());
        bindings.insert(
            "<checkout>",
            member_dir(ctx, &change.path).display().to_string(),
        );
        bindings.insert(
            "<ids>",
            unpushed
                .iter()
                .rev()
                .map(|commit| commit.id.short())
                .collect::<Vec<_>>()
                .join(" "),
        );
        bindings.insert(
            "<b>",
            change.now.branch.local_counterpart().as_str().to_owned(),
        );
        let mut onto_new: Vec<Form> = Vec::new();
        if change.changed("branch") {
            onto_new.push(Form::C25);
        }
        if !unpushed.is_empty() {
            onto_new.push(Form::C15);
        }
        if !list_choices {
            let text = crate::diagnosis::choices_text(vcs.as_ref(), &onto_new, &bindings);
            if let Some(line) = text.lines().find_map(|l| l.strip_prefix("    ")) {
                out.push_str(&format!("next: {line}\n"));
            }
            continue;
        }
        let onto_new_text = crate::diagnosis::choices_text(vcs.as_ref(), &onto_new, &bindings);
        let mut forms: Vec<Form> = Vec::new();
        let mut restores = String::new();
        for field in &change.fields {
            let (form, key) = match field.name {
                "branch" => (Form::C11SetBranch, "<b>"),
                "url" => (Form::C11SetUrl, "<url>"),
                "role" => (Form::C11SetRole, "<role>"),
                "history" if change.was.history.is_none() => (Form::C11UnsetHistory, "<h>"),
                "history" => (Form::C11SetHistory, "<h>"),
                _ => (Form::C11SetRecurseSubmodules, "<bool>"),
            };
            forms.push(form);
            bindings.insert(key, field.was.clone());
            restores.push_str(&format!("{key}={}\n", field.was));
        }
        let _ = restores;
        let restore_text = crate::diagnosis::choices_text(vcs.as_ref(), &forms, &bindings);
        out.push_str(&onto_new_text);
        match (
            onto_new_text.is_empty(),
            restore_text.strip_prefix("choices:\n"),
        ) {
            (false, Some(rest)) => out.push_str(rest),
            _ => out.push_str(&restore_text),
        }
    }
    out
}

/// Print what the checked-out branch's entries stand in, for `verb`, and
/// relink the branch each url change leaves on the old remote. Returns the
/// standing, which says whether the workspace's stored entries may follow.
pub(crate) fn report(
    verb: &'static str,
    ctx: &WorkspaceContext,
    aligner: &Aligner,
    project_dir: &Path,
    manifest: &Manifest,
    relinking: bool,
) -> Standing {
    let vcs = crate::vcs::project_vcs();
    let standing = standing(vcs.as_ref(), project_dir, aligner, manifest);
    eprint!("{}", render(verb, ctx, aligner, &standing, false));
    if relinking {
        relink(verb, ctx, &standing);
    }
    standing
}

fn relink(verb: &'static str, ctx: &WorkspaceContext, standing: &Standing) {
    for change in standing.changes.iter().filter(|c| c.changed("url")) {
        let vcs = crate::vcs::vcs_for(change.now.vcs_type);
        let dir = member_dir(ctx, &change.path);
        if !dir.exists() {
            continue;
        }
        let mut refusals = crate::diagnosis::Report::default();
        let relink = crate::member_url::judge_relink(
            vcs.as_ref(),
            &change.path,
            &dir,
            &change.was.url,
            &change.now.url,
            verb,
            &mut refusals,
        );
        if !refusals.is_empty() {
            eprint!("{}", refusals.render(verb));
            continue;
        }
        if let Ok(relink) = relink {
            if relink.apply(vcs.as_ref(), &dir).is_ok() {
                if let Some(lines) = relink.describe(vcs.as_ref(), &change.path) {
                    eprintln!("rwv {verb}: {lines}");
                }
            }
        }
    }
}
