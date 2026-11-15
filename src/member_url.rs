//! `rwv member set-url`, and the relink a url change carries: the branch a
//! member sits on, taking in from the old url's remote, takes in from the new
//! url's remote instead.

use crate::diagnosis::{Report, State};
use crate::manifest::{LockEntry, RepoEntry, RepoPath, RepoUrl};
use crate::member_branch::Loaded;
use crate::registry::CloneUrl;
use crate::vcs::{HeadAttachment, RawRefName, RemoteHandle, Upstream, Vcs};
use crate::workspace::WorkspaceContext;
use std::path::Path;

pub(crate) const VERB: &str = "member set-url";

/// A branch to take in from the new url's remote.
#[derive(Debug, Clone)]
pub(crate) struct Relink {
    branch: RawRefName,
    upstream: Upstream,
}

impl Relink {
    pub(crate) fn apply(&self, vcs: &dyn Vcs, dir: &Path) -> anyhow::Result<()> {
        vcs.set_upstream(dir, &self.branch, &self.upstream)?;
        Ok(())
    }

    pub(crate) fn describe(&self, member: &RepoPath) -> String {
        format!(
            "{member}: '{}' takes in from {}/{} now",
            self.branch, self.upstream.remote, self.upstream.branch
        )
    }
}

/// What a url change does to the branch the member sits on.
#[derive(Debug, Clone)]
pub(crate) enum Relinking {
    To(Relink),
    /// The new url's remote holds no copy of the branch as last fetched: none
    /// is added and nothing fetched here, so the relink waits for a fetch.
    Unfetched {
        branch: RawRefName,
        url: RepoUrl,
    },
    Stays,
}

impl Relinking {
    pub(crate) fn apply(&self, vcs: &dyn Vcs, dir: &Path) -> anyhow::Result<()> {
        match self {
            Relinking::To(relink) => relink.apply(vcs, dir),
            Relinking::Unfetched { .. } | Relinking::Stays => Ok(()),
        }
    }

    /// The lines saying what the change did to the branch, each beginning
    /// with `member`; nothing where it leaves the branch as it was for good.
    pub(crate) fn describe(&self, vcs: &dyn Vcs, member: &RepoPath) -> Option<String> {
        match self {
            Relinking::To(relink) => Some(relink.describe(member)),
            Relinking::Unfetched { branch, url } => Some(format!(
                "{member}: '{branch}' relinks to {url} once a fetch finds it there\n{}",
                crate::diagnosis::choices_text(
                    vcs,
                    &[crate::diagnosis::Form::C33],
                    &Default::default()
                )
                .trim_end()
            )),
            Relinking::Stays => None,
        }
    }
}

fn clone_url(url: &RepoUrl) -> anyhow::Result<CloneUrl> {
    Ok(crate::registry::resolve_to_clone_info(url)?.url)
}

/// What relinking the branch the member sits on takes: a relink to do, one
/// waiting for a fetch, or nothing, or a refusal added to `refusals`.
///
/// The branch relinks when it takes in from the old url's remote and has no
/// push remote of its own, and the new url's remote has a branch of its name
/// as last fetched. A branch that has diverged from that one is refused.
pub(crate) fn judge_relink(
    vcs: &dyn Vcs,
    member: &RepoPath,
    dir: &Path,
    old: &RepoUrl,
    new: &RepoUrl,
    verb: &'static str,
    refusals: &mut Report,
) -> anyhow::Result<Relinking> {
    let HeadAttachment::Attached(attached) = vcs.head_attachment(dir)? else {
        return Ok(Relinking::Stays);
    };
    judge_relink_of(
        vcs,
        member,
        dir,
        &RawRefName::new(attached.to_string()),
        old,
        new,
        verb,
        refusals,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn judge_relink_of(
    vcs: &dyn Vcs,
    member: &RepoPath,
    dir: &Path,
    branch: &RawRefName,
    old: &RepoUrl,
    new: &RepoUrl,
    verb: &'static str,
    refusals: &mut Report,
) -> anyhow::Result<Relinking> {
    let Some(upstream) = vcs.upstream_of(dir, branch)? else {
        return Ok(Relinking::Stays);
    };
    let Some(old_remote) = crate::url_remote::present(vcs, dir, old)? else {
        return Ok(Relinking::Stays);
    };
    if upstream.remote != old_remote || vcs.own_push_remote(dir, branch)?.is_some() {
        return Ok(Relinking::Stays);
    }
    let unfetched = Relinking::Unfetched {
        branch: branch.clone(),
        url: new.clone(),
    };
    let new_remote: RemoteHandle =
        match crate::url_remote::find(vcs, member, verb, dir, &clone_url(new)?)? {
            crate::url_remote::Found::Present(remote) => remote,
            crate::url_remote::Found::Missing(_) => return Ok(unfetched),
            crate::url_remote::Found::Taken(judged) => {
                refusals.add(judged);
                return Ok(Relinking::Stays);
            }
        };
    let Some(copy) = vcs
        .remote_copies(dir, branch)?
        .into_iter()
        .find(|copy| copy.remote == new_remote)
    else {
        return Ok(unfetched);
    };
    if let Some(local) = vcs.resolve_local_branch_tip(dir, branch)? {
        if vcs.relation(dir, &local, &copy.tip)? == crate::vcs::Relation::Diverged {
            let (ahead, behind) = vcs.ahead_behind(dir, copy.tip.id(), local.id());
            refusals.add(
                State::PushBranchDiverged
                    .of(member)
                    .member()
                    .subject(format!("{ahead} ahead, {behind} behind"))
                    .listing([format!(
                        "'{branch}' and {new_remote}/{branch} have each moved"
                    )])
                    .checkout(dir)
                    .judge(vcs),
            );
            return Ok(Relinking::Stays);
        }
    }
    Ok(Relinking::To(Relink {
        branch: branch.clone(),
        upstream: Upstream {
            remote: new_remote,
            branch: branch.clone(),
        },
    }))
}

/// Execute `rwv member set-url <repo> <url>`.
///
/// Sets the member's url, relinks the branch it sits on, and commits the
/// entry and the lock's url for it together, leaving every other edit to
/// `rwv.toml` where it stands. Every refusal is reported before anything
/// changes.
pub fn run_set_url(
    repo: &str,
    url: &str,
    ctx: &WorkspaceContext,
    opts: crate::activate::ActivateOptions,
) -> anyhow::Result<()> {
    if let Ok(crate::manifest::RepoOperand::Project(project)) = repo.parse() {
        return run_set_project_url(&project, url, ctx);
    }
    let loaded = Loaded::open(ctx, VERB)?;
    let member = RepoPath::new(repo)?;
    let new: RepoUrl = url.parse()?;
    let mut refusals = Report::default();
    let step = judge_set_url(ctx, &loaded, &member, &new, &mut refusals)?;
    if let Err(e) = crate::manifest_commit::judge(&loaded.project_dir) {
        merge_refusal(&mut refusals, e)?;
    }
    refusals.into_result()?;
    let step = step.expect("an unrefused judgment has a step");
    step.finish(ctx, &loaded, &member, new, opts)
}

/// Record `url`'s remote as the project repo's url remote, adding the remote
/// when the repo has none for it. Nothing in `rwv.toml` changes: the project
/// repo is not a member.
fn run_set_project_url(
    project: &crate::manifest::ProjectName,
    url: &str,
    ctx: &WorkspaceContext,
) -> anyhow::Result<()> {
    let project_dir = crate::workspace::project_dir(ctx.active_path(), project);
    if !project_dir.exists() {
        return Err(crate::untokened!(
            Owed,
            "{}: no project repo on disk at {}",
            crate::manifest::RepoOperand::Project(project.clone()),
            project_dir.display()
        ));
    }
    let clone_url = clone_url(&url.parse()?)?;
    let vcs = crate::vcs::project_vcs();
    let remote = crate::url_remote::ensure(vcs.as_ref(), project, VERB, &project_dir, &clone_url)?;
    vcs.record_project_url_remote(&project_dir, &remote)?;
    eprintln!("rwv {VERB}: projects/{project}: url remote {remote} ({clone_url})");
    Ok(())
}

pub(crate) fn merge_refusal(refusals: &mut Report, error: anyhow::Error) -> anyhow::Result<()> {
    match crate::diagnosis::report_of(&error) {
        Some(report) => {
            for line in report.lines() {
                refusals.add(line.clone());
            }
            Ok(())
        }
        None => Err(error),
    }
}

pub(crate) struct SetUrl {
    was: RepoEntry,
    relink: Relinking,
    same: bool,
}

fn judge_set_url(
    ctx: &WorkspaceContext,
    loaded: &Loaded,
    member: &RepoPath,
    new: &RepoUrl,
    refusals: &mut Report,
) -> anyhow::Result<Option<SetUrl>> {
    let Some(entry) = loaded.manifest.get_entry(member) else {
        crate::refuse!(
            crate::refusal::RefusalKind::RepoNotInManifest,
            "path '{}' not found in manifest",
            member.as_str()
        );
    };
    let vcs = crate::vcs::vcs_for(entry.vcs_type);
    let located = loaded.checkout(ctx, member);
    let dir = located.dir.as_path();
    if !dir.exists() {
        return Err(crate::untokened!(
            Owed,
            "{member}: clone missing on disk; run `rwv update` to re-materialize it, then re-run \
         `rwv {VERB}`"
        ));
    }
    let same = clone_url(&entry.url)?.names_same_remote(&clone_url(new)?);
    let relink = if same {
        Relinking::Stays
    } else {
        if let crate::url_remote::Found::Taken(judged) =
            crate::url_remote::find(vcs.as_ref(), member, VERB, dir, &clone_url(new)?)?
        {
            refusals.add(judged);
        }
        judge_relink(vcs.as_ref(), member, dir, &entry.url, new, VERB, refusals)?
    };
    Ok(Some(SetUrl {
        was: entry.clone(),
        relink,
        same,
    }))
}

impl SetUrl {
    fn finish(
        self,
        ctx: &WorkspaceContext,
        loaded: &Loaded,
        member: &RepoPath,
        new: RepoUrl,
        opts: crate::activate::ActivateOptions,
    ) -> anyhow::Result<()> {
        if self.same {
            eprintln!("rwv {VERB}: {member} already has that url; nothing to change");
            return Ok(());
        }
        let vcs = crate::vcs::vcs_for(self.was.vcs_type);
        let dir = loaded.checkout(ctx, member).dir;
        let mut manifest = loaded.manifest.clone();
        let mut entry = self.was.clone();
        entry.url = new.clone();
        manifest.insert_repo(member.clone(), entry.clone());
        crate::add_remove::settle_manifest(ctx, &loaded.project, Some(&manifest), &[], opts)?;

        let had_pin = loaded
            .old
            .as_ref()
            .is_some_and(|lock| lock.contains_repo(member));
        let relocked = loaded.old.as_ref().map(|lock| {
            let mut lock = lock.clone();
            set_lock_url(&mut lock, member, &new);
            lock
        });
        if let Some(lock) = &relocked {
            crate::lock::write_lock_in(lock, &loaded.project_dir)?;
        }
        self.relink.apply(vcs.as_ref(), &dir)?;

        let authored = crate::manifest_commit::authored_across(
            ctx,
            &loaded.project,
            &[&loaded.manifest, &manifest],
        );
        let url_for_manifest = new.clone();
        let url_for_lock = new.clone();
        let member_for_manifest = member.clone();
        let member_for_lock = member.clone();
        let edit_manifest = move |m: &mut crate::manifest::Manifest| {
            if let Some(mut held) = m.get_entry(&member_for_manifest).cloned() {
                held.url = url_for_manifest.clone();
                m.insert_repo(member_for_manifest.clone(), held);
            }
        };
        let edit_lock = move |l: &mut crate::manifest::LockFile| {
            set_lock_url(l, &member_for_lock, &url_for_lock);
        };
        crate::manifest_commit::commit(&crate::manifest_commit::MemberCommit {
            project: &loaded.project,
            project_dir: &loaded.project_dir,
            edit_manifest: &edit_manifest,
            edit_lock: had_pin.then_some(&edit_lock as &dyn Fn(&mut crate::manifest::LockFile)),
            authored: &authored,
            message: &format!("member set-url {member}\n\n  - {member}: {}", new),
            naming: crate::repin::Naming::Unnamed,
        })?;
        eprintln!("rwv {VERB}: {member}: url {} -> {}", self.was.url, new);
        if let Some(lines) = self.relink.describe(vcs.as_ref(), member) {
            eprintln!("rwv {VERB}: {lines}");
        }
        Ok(())
    }
}

fn set_lock_url(lock: &mut crate::manifest::LockFile, member: &RepoPath, url: &RepoUrl) {
    if let Some(held) = lock.get_entry(member).cloned() {
        lock.insert_entry(
            member.clone(),
            LockEntry {
                url: url.clone(),
                ..held
            },
        );
    }
}
