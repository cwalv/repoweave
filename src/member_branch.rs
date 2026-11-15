//! `rwv member set-branch`, and the entry-setting step the creating verbs run
//! for `--set-branch` and `--set-new-branch`: declare a branch for a member,
//! pin the member on it, and switch the member onto it.

use crate::alignment::{Accounting, Aligner, Applied, Found, Member, Mode, Pin, Plan, Target};
use crate::diagnosis::{ChoiceId, Report, State};
use crate::manifest::{LockFile, Manifest, Project, ProjectName, RepoEntry, RepoPath, Role};
use crate::refusal::RefusalKind;
use crate::vcs::{
    project_vcs, vcs_for, RawRefName, Relation, ResolvedRevisionId, TrackingRef, Vcs,
};
use crate::workspace::{project_dir, WorkspaceContext};
use anyhow::Context;
use std::path::{Path, PathBuf};

pub(crate) const VERB: &str = "member set-branch";

/// Whether the branch an entry names exists already, or is created for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Creation {
    Existing,
    New,
}

/// One declaration of a member's branch: `<m>=<b>` as a creating verb spells
/// it, with `--pin <m>=<rev>` giving its start, or the arguments of `member
/// set-branch`.
#[derive(Debug, Clone)]
pub struct BranchEntry {
    pub member: RepoPath,
    pub branch: String,
    pub rev: Option<String>,
    pub creation: Creation,
}

fn split_flag<'a>(flag: &str, spec: &'a str) -> anyhow::Result<(&'a str, &'a str)> {
    match spec.split_once('=') {
        Some((member, rest)) if !member.is_empty() && !rest.is_empty() => Ok((member, rest)),
        _ => crate::refuse!(
            RefusalKind::InapplicableFlag,
            "'{spec}' is not `{flag} <member>=<value>`"
        ),
    }
}

impl BranchEntry {
    /// Read `<m>=<b>`: the member path holds no `=`, and the whole remainder
    /// is the branch name.
    pub fn parse(spec: &str, creation: Creation) -> anyhow::Result<Self> {
        let flag = match creation {
            Creation::Existing => "--set-branch",
            Creation::New => "--set-new-branch",
        };
        let (member, branch) = split_flag(flag, spec)?;
        Ok(Self {
            member: RepoPath::new(member)?,
            branch: branch.to_owned(),
            rev: None,
            creation,
        })
    }

    /// Give each `--pin <m>=<rev>` to the entry naming the same member.
    pub fn pinned(mut entries: Vec<Self>, pins: &[String]) -> anyhow::Result<Vec<Self>> {
        let mut seen: Vec<RepoPath> = Vec::new();
        for spec in pins {
            let (member, rev) = split_flag("--pin", spec)?;
            let member = RepoPath::new(member)?;
            if seen.contains(&member) {
                crate::refuse!(
                    RefusalKind::InapplicableFlag,
                    "--pin names {member} more than once"
                );
            }
            let Some(entry) = entries.iter_mut().find(|entry| entry.member == member) else {
                crate::refuse!(
                    RefusalKind::InapplicableFlag,
                    "--pin {member}={rev} needs a --set-branch or --set-new-branch naming {member}"
                );
            };
            entry.rev = Some(rev.to_owned());
            seen.push(member);
        }
        Ok(entries)
    }
}

/// The workspace a setter acts in, read once.
pub(crate) struct Loaded {
    pub(crate) project: ProjectName,
    pub(crate) project_dir: PathBuf,
    pub(crate) workweave_dir: Option<PathBuf>,
    pub(crate) manifest: Manifest,
    pub(crate) old: Option<LockFile>,
    pub(crate) aligner: Aligner,
}

impl Loaded {
    pub(crate) fn open(ctx: &WorkspaceContext, verb: &str) -> anyhow::Result<Self> {
        crate::branch::require_recorded(ctx)?;
        let (project, workweave_dir) = crate::lock::scope(ctx)?;
        let project_dir = project_dir(ctx.active_path(), &project);
        crate::diagnosis::settle_project_drift(&project, &project_dir)?;
        let Project { manifest, .. } =
            Project::from_dir_skip_lock(&project_dir, project.clone())
                .with_context(|| format!("failed to load project '{project}'"))?;
        let old = crate::lock::read_old_lock(&LockFile::path_in(&project_dir), verb);
        let aligner = Aligner::open(
            ctx.active_path(),
            &project,
            Accounting::scan(ctx.primary_path())?,
        )?;
        crate::lock::refuse_detached(&aligner, &project)?;
        Ok(Self {
            project,
            project_dir,
            workweave_dir,
            manifest,
            old,
            aligner,
        })
    }

    pub(crate) fn checkout(&self, ctx: &WorkspaceContext, member: &RepoPath) -> MemberDir {
        let checkout = crate::workspace::member_checkout_dir(
            member,
            ctx.primary_path(),
            self.workweave_dir.as_deref(),
        );
        MemberDir {
            aligns: self.workweave_dir.is_none() || checkout.is_workweave_slot(),
            dir: checkout.path().to_path_buf(),
        }
    }

    pub(crate) fn in_feature(&self) -> bool {
        matches!(self.aligner.mode, Mode::Feature { .. })
    }
}

pub(crate) struct MemberDir {
    pub(crate) dir: PathBuf,
    pub(crate) aligns: bool,
}

/// What the entry-setting step decided, judged and not yet done.
#[derive(Debug)]
pub(crate) struct EntryStep {
    pub(crate) member: RepoPath,
    pub(crate) branch: TrackingRef,
    creates: Option<(RawRefName, ResolvedRevisionId)>,
    pub(crate) pin: Option<ResolvedRevisionId>,
    pub(crate) was_pin: Option<ResolvedRevisionId>,
    pub(crate) ahead_of_rev: bool,
}

impl EntryStep {
    /// Create the branch where the step creates one, then set the entry in
    /// `manifest` and the pin in `lock`. This is the step a creating verb runs
    /// for each `--set-branch` and `--set-new-branch`, after judging every one
    /// of them, with no relock between.
    pub(crate) fn apply(
        &self,
        vcs: &dyn Vcs,
        dir: &Path,
        manifest: &mut Manifest,
        lock: &mut LockFile,
    ) -> anyhow::Result<()> {
        if let Some((name, at)) = &self.creates {
            if vcs.resolve_local_branch_tip(dir, name)?.is_none() {
                vcs.create_branch(dir, name, at, None)?;
            }
        }
        let mut entry = manifest
            .get_entry(&self.member)
            .cloned()
            .expect("a judged member is in the manifest");
        entry.branch = self.branch.clone();
        manifest.insert_repo(self.member.clone(), entry.clone());
        if let Some(pin) = &self.pin {
            lock.insert_entry(
                self.member.clone(),
                crate::manifest::LockEntry {
                    vcs_type: entry.vcs_type,
                    url: entry.url.clone(),
                    version: crate::vcs::RawRevisionId::new(pin.id().as_str()),
                },
            );
        }
        Ok(())
    }
}

fn branch_tip(
    vcs: &dyn Vcs,
    dir: &Path,
    entry: &RepoEntry,
    branch: &RawRefName,
) -> anyhow::Result<Option<ResolvedRevisionId>> {
    Ok(match crate::alignment::find_e(vcs, dir, entry, branch)? {
        Found::Local { tip }
        | Found::LocalGivenUpstream { tip, .. }
        | Found::OnRemote { tip, .. } => Some(tip),
        Found::Foreign { .. } => vcs.resolve_local_branch_tip(dir, branch)?,
        Found::Nowhere => None,
    })
}

fn reaches_branch(
    vcs: &dyn Vcs,
    dir: &Path,
    pin: &ResolvedRevisionId,
    branch: &RawRefName,
) -> anyhow::Result<bool> {
    let mut tips: Vec<ResolvedRevisionId> = Vec::new();
    tips.extend(vcs.resolve_local_branch_tip(dir, branch)?);
    tips.extend(vcs.remote_copies(dir, branch)?.into_iter().map(|c| c.tip));
    Ok(tips
        .iter()
        .any(|tip| tip == pin || vcs.is_ancestor(dir, pin, tip).unwrap_or(false)))
}

/// Judge one entry-setting step against the workspace, adding every refusal
/// to `refusals` and changing nothing. `None` where it refused.
pub(crate) fn judge_entry(
    ctx: &WorkspaceContext,
    loaded: &Loaded,
    verb: &'static str,
    request: &BranchEntry,
    refusals: &mut Report,
) -> anyhow::Result<Option<EntryStep>> {
    let member = &request.member;
    let Some(entry) = loaded.manifest.get_entry(member) else {
        crate::refuse!(
            RefusalKind::RepoNotInManifest,
            "path '{}' not found in manifest",
            member.as_str()
        );
    };
    let vcs = vcs_for(entry.vcs_type);
    let vcs = vcs.as_ref();
    let located = loaded.checkout(ctx, member);
    let dir = located.dir.as_path();
    if !dir.exists() {
        return Err(crate::untokened!(
            Owed,
            "{member}: clone missing on disk; run `rwv update` to re-materialize it, then re-run \
         `rwv {verb}`"
        ));
    }
    let owned_in_feature = loaded.in_feature() && matches!(entry.role, Role::Owned | Role::Fork);
    if owned_in_feature && request.rev.is_some() {
        crate::refuse!(
            RefusalKind::InapplicableFlag,
            "rwv {verb}: --at does not apply to an owned or fork member of a feature: its \
             pin stays on the feature's branch, and only its entry changes"
        );
    }

    let tracking = match TrackingRef::parse(RawRefName::new(request.branch.clone())) {
        Ok(tracking) => tracking,
        Err(crate::naming::RefNameError::ShaShaped(name)) => {
            return Ok(refused_name(
                refusals,
                vcs,
                member,
                verb,
                State::NameARevision,
                &name,
                &["it reads as a commit id, so there is no branch to track"],
                ChoiceId::C11,
            ));
        }
        Err(crate::naming::RefNameError::TagShaped(name)) => {
            return Ok(refused_name(
                refusals,
                vcs,
                member,
                verb,
                State::NameARevision,
                &name,
                &["it reads as a release tag, so there is no branch to track"],
                ChoiceId::C11,
            ));
        }
        Err(other) => {
            return Err(anyhow::Error::new(other)
                .context(format!("'{}' is not a usable branch name", request.branch)));
        }
    };
    if matches!(
        crate::records::BranchName::parse(
            project_vcs().as_ref(),
            tracking.local_counterpart().as_str()
        ),
        Err(crate::records::NameError::Reserved)
    ) {
        return Ok(refused_name(
            refusals,
            vcs,
            member,
            verb,
            State::NameReserved,
            tracking.local_counterpart().as_str(),
            &[],
            ChoiceId::C11,
        ));
    }
    if let Ok(name) = crate::records::BranchName::parse(
        project_vcs().as_ref(),
        tracking.local_counterpart().as_str(),
    ) {
        let mut features = crate::names::other_projects_with_feature(
            ctx.primary_path(),
            &loaded.project,
            &name,
            member,
        )?;
        let pvcs = project_vcs();
        if let Ok(store) = crate::records::Store::open(pvcs.as_ref(), &loaded.project_dir) {
            if let Ok(view) = store.view() {
                if matches!(
                    view.lookup(&name),
                    crate::records::Lookup::Value(crate::records::Record::Feature { .. })
                ) {
                    features.push(crate::workspace::project_rel_path(&loaded.project).to_string());
                }
            }
        }
        if !features.is_empty() {
            return Ok(refused_name(
                refusals,
                vcs,
                member,
                verb,
                State::NameFeature,
                tracking.local_counterpart().as_str(),
                &features.iter().map(String::as_str).collect::<Vec<_>>(),
                ChoiceId::C11,
            ));
        }
    }
    let raw = RawRefName::new(tracking.local_counterpart().as_str());
    let tip = branch_tip(vcs, dir, entry, &raw)?;
    let already_declared = entry.branch == tracking;
    match (request.creation, &tip) {
        (Creation::Existing, None) => {
            refusals.add(
                crate::alignment::declared_nowhere(member, dir, &raw)
                    .by_verb(verb)
                    .judge(vcs),
            );
            return Ok(None);
        }
        (Creation::New, Some(_)) if !already_declared => {
            refusals.add(
                State::NameExists
                    .of(member)
                    .member()
                    .by_verb(verb)
                    .subject(tracking.local_counterpart().as_str())
                    .bind("<m>", member)
                    .bind("<b>", tracking.local_counterpart().as_str())
                    .checkout(dir)
                    .judge(vcs),
            );
            return Ok(None);
        }
        _ => {}
    }

    let was_pin = match crate::alignment::pin_of(vcs, dir, loaded.old.as_ref(), member)? {
        Pin::At(at) => Some(at),
        Pin::None | Pin::Unreachable => None,
    };

    if owned_in_feature {
        let creates = match (request.creation, &tip) {
            (Creation::New, None) => {
                let Some(at) = start_of_new_branch(loaded, vcs, dir, member, refusals)? else {
                    return Ok(None);
                };
                Some((raw, at))
            }
            _ => None,
        };
        return Ok(Some(EntryStep {
            member: member.clone(),
            branch: tracking,
            creates,
            pin: None,
            was_pin,
            ahead_of_rev: false,
        }));
    }

    let mut ahead_of_rev = false;
    let (pin, creates) = match request.creation {
        Creation::Existing => {
            let tip = tip.expect("an existing branch has a tip");
            match &request.rev {
                None => (tip, None),
                Some(rev) => {
                    let Ok(at) = vcs.resolve_revision(dir, rev) else {
                        refusals.add(
                            State::NameNoSuchBranch
                                .of(member)
                                .member()
                                .subject(rev)
                                .checkout(dir)
                                .judge(vcs),
                        );
                        return Ok(None);
                    };
                    if !reaches_branch(vcs, dir, &at, &raw)? {
                        refusals.add(
                            State::PinNotOnDeclaredBranch
                                .of(member)
                                .member()
                                .subject(tracking.local_counterpart().as_str())
                                .only(&[ChoiceId::C25])
                                .bind("<m>", member)
                                .checkout(dir)
                                .judge(vcs),
                        );
                        return Ok(None);
                    }
                    ahead_of_rev = at != tip
                        && vcs
                            .relation(dir, &tip, &at)
                            .is_ok_and(|rel| rel == Relation::Ahead);
                    (at, None)
                }
            }
        }
        Creation::New => {
            let at = match (&request.rev, &tip, &was_pin) {
                (_, Some(tip), _) if already_declared && request.rev.is_none() => tip.clone(),
                (Some(rev), _, _) => match vcs.resolve_revision(dir, rev) {
                    Ok(at) => at,
                    Err(_) => {
                        refusals.add(
                            State::NameNoSuchBranch
                                .of(member)
                                .member()
                                .subject(rev)
                                .checkout(dir)
                                .judge(vcs),
                        );
                        return Ok(None);
                    }
                },
                (None, _, Some(pin)) => pin.clone(),
                (None, _, None) => {
                    refusals.add(
                        State::PinNone
                            .of(member)
                            .member()
                            .only(&[ChoiceId::C25])
                            .bind("<m>", member)
                            .checkout(dir)
                            .judge(vcs),
                    );
                    return Ok(None);
                }
            };
            let creates = tip.is_none().then(|| (raw, at.clone()));
            (at, creates)
        }
    };
    Ok(Some(EntryStep {
        member: member.clone(),
        branch: tracking,
        creates,
        pin: Some(pin),
        was_pin,
        ahead_of_rev,
    }))
}

#[allow(clippy::too_many_arguments)]
fn refused_name(
    refusals: &mut Report,
    vcs: &dyn Vcs,
    member: &RepoPath,
    verb: &'static str,
    state: State,
    name: &str,
    listing: &[&str],
    only: ChoiceId,
) -> Option<EntryStep> {
    refusals.add(
        state
            .of(member)
            .member()
            .by_verb(verb)
            .subject(name)
            .listing(listing.iter().copied())
            .only(&[only])
            .bind("<m>", member)
            .judge(vcs),
    );
    None
}

/// Where a branch an owned or fork member of a feature declares starts: the
/// member's pin on the branch the feature lands into, else its pin here.
fn start_of_new_branch(
    loaded: &Loaded,
    vcs: &dyn Vcs,
    dir: &Path,
    member: &RepoPath,
    refusals: &mut Report,
) -> anyhow::Result<Option<ResolvedRevisionId>> {
    let version = match target_pin(loaded, member, refusals)? {
        TargetPin::Pinned(version) => Some(version),
        TargetPin::Lacks => loaded
            .old
            .as_ref()
            .and_then(|lock| lock.get_entry(member))
            .map(|held| held.version.clone()),
        TargetPin::Refused => return Ok(None),
    };
    let at = version.and_then(|version| vcs.resolve_revision(dir, version.as_str()).ok());
    if at.is_none() {
        refusals.add(
            State::PinNone
                .of(member)
                .member()
                .only(&[ChoiceId::C25])
                .bind("<m>", member)
                .checkout(dir)
                .judge(vcs),
        );
    }
    Ok(at)
}

enum TargetPin {
    Pinned(crate::vcs::RawRevisionId),
    Lacks,
    Refused,
}

fn target_pin(
    loaded: &Loaded,
    member: &RepoPath,
    refusals: &mut Report,
) -> anyhow::Result<TargetPin> {
    let vcs = project_vcs();
    let vcs = vcs.as_ref();
    let dir = &loaded.project_dir;
    let Mode::Feature { branch } = &loaded.aligner.mode else {
        return Ok(TargetPin::Lacks);
    };
    let name = crate::records::BranchName::parse(vcs, branch).map_err(|e| {
        crate::untokened!(Owed, "`{branch}` is not a usable weave branch name: {e:?}")
    })?;
    let crate::records::Record::Feature { target, .. } =
        crate::branch::recorded_value(vcs, &loaded.project, dir, &name)?
    else {
        return Ok(TargetPin::Lacks);
    };
    let raw = RawRefName::new(target.as_str());
    let home = vcs.project_url_remote(dir)?;
    let tip = match crate::push_branch::newer_of_branch_and_upstream(vcs, dir, &raw, home.as_ref())?
    {
        crate::push_branch::NewerOfBranchAndUpstream::Tip(tip) => tip,
        crate::push_branch::NewerOfBranchAndUpstream::Nowhere => return Ok(TargetPin::Lacks),
        crate::push_branch::NewerOfBranchAndUpstream::Diverged { .. } => {
            refusals.add(
                State::PushBranchDiverged
                    .of(crate::workspace::project_rel_path(&loaded.project))
                    .project_repo()
                    .subject(target.as_str())
                    .checkout(dir)
                    .judge(vcs),
            );
            return Ok(TargetPin::Refused);
        }
    };
    let Ok(text) = vcs.read_file_at_revision(dir, &tip, Path::new(LockFile::FILE_NAME)) else {
        return Ok(TargetPin::Lacks);
    };
    let Ok(lock) = LockFile::from_json_str(&text) else {
        return Ok(TargetPin::Lacks);
    };
    Ok(match lock.get_entry(member) {
        Some(held) => TargetPin::Pinned(held.version.clone()),
        None => TargetPin::Lacks,
    })
}

/// Judge the move of a member onto the branch `step` declares: the lines that
/// say why it cannot leave where it is.
#[allow(clippy::too_many_arguments)]
pub(crate) fn judge_move(
    loaded: &Loaded,
    vcs: &dyn Vcs,
    member: &RepoPath,
    dir: &Path,
    after: &RepoEntry,
    step: &EntryStep,
    lock: &LockFile,
    refusals: &mut Report,
) -> anyhow::Result<()> {
    let candidate = Member {
        path: member,
        entry: after,
        dir,
        vcs,
    };
    let plan = candidate_plan(&loaded.aligner, &candidate, step, lock)?;
    for line in loaded.aligner.blocked(&candidate, &plan, member)? {
        refusals.add(line);
    }
    Ok(())
}

/// Put the member where `lock` and its entry say, reporting what that found.
/// The notes are what the verb adds to its own report.
pub(crate) fn align_member(
    loaded: &Loaded,
    vcs: &dyn Vcs,
    member: &RepoPath,
    dir: &Path,
    after: &RepoEntry,
    lock: &LockFile,
    verb: &str,
) -> anyhow::Result<Vec<String>> {
    let candidate = Member {
        path: member,
        entry: after,
        dir,
        vcs,
    };
    let plan = loaded.aligner.plan(&candidate, Some(lock))?;
    let mut notes = Vec::new();
    let mut report = Report::default();
    match loaded.aligner.apply(&candidate, &plan, member)? {
        Applied::Aligned {
            facts, notes: more, ..
        } => {
            for fact in &facts {
                notes.push(crate::diagnosis::fact_text(fact));
            }
            for line in more {
                report.add(line);
            }
        }
        Applied::Left { judged } => {
            for line in judged {
                report.add(line);
            }
        }
    }
    if !report.is_empty() {
        eprint!("{}", report.render_completed(verb));
    }
    Ok(notes)
}

/// Judge every `--set-branch` and `--set-new-branch` of a creating verb
/// against the workspace as the new branch will leave it, with `manifest` and
/// `lock` as its first commit starts from. Nothing is written. `moves` also
/// judges the move of each member this workspace aligns, and what the commit
/// would sweep up.
pub(crate) fn judge_entries(
    ctx: &WorkspaceContext,
    verb: &'static str,
    (manifest, lock): (Manifest, Option<LockFile>),
    branch: (&crate::records::BranchName, &crate::records::Record),
    entries: &[BranchEntry],
    moves: bool,
    refusals: &mut Report,
) -> anyhow::Result<Vec<EntryStep>> {
    let (project, workweave_dir) = crate::lock::scope(ctx)?;
    let project_dir = project_dir(ctx.active_path(), &project);
    let aligner = Aligner::prospective(
        ctx.active_path(),
        &project,
        Accounting::scan(ctx.primary_path())?,
        Some(branch),
    )?;
    let loaded = Loaded {
        project,
        project_dir,
        workweave_dir,
        manifest,
        old: lock,
        aligner,
    };
    let mut seen: Vec<&RepoPath> = Vec::new();
    for entry in entries {
        if seen.contains(&&entry.member) {
            crate::refuse!(
                RefusalKind::InapplicableFlag,
                "rwv {verb}: {} is named by more than one --set-branch or --set-new-branch",
                entry.member
            );
        }
        seen.push(&entry.member);
    }
    let mut steps = Vec::new();
    let mut pinned = loaded.old.clone().unwrap_or(LockFile {
        repositories: Default::default(),
    });
    for entry in entries {
        let Some(step) = judge_entry(ctx, &loaded, verb, entry, refusals)? else {
            continue;
        };
        let member_entry = loaded
            .manifest
            .get_entry(&entry.member)
            .expect("a judged member is in the manifest");
        let located = loaded.checkout(ctx, &entry.member);
        let owned_in_feature =
            loaded.in_feature() && matches!(member_entry.role, Role::Owned | Role::Fork);
        let mut after = member_entry.clone();
        after.branch = step.branch.clone();
        if let Some(pin) = &step.pin {
            pinned.insert_entry(
                entry.member.clone(),
                crate::manifest::LockEntry {
                    vcs_type: after.vcs_type,
                    url: after.url.clone(),
                    version: crate::vcs::RawRevisionId::new(pin.id().as_str()),
                },
            );
        }
        if moves && located.aligns && !owned_in_feature {
            let vcs = vcs_for(after.vcs_type);
            judge_move(
                &loaded,
                vcs.as_ref(),
                &entry.member,
                &located.dir,
                &after,
                &step,
                &pinned,
                refusals,
            )?;
        }
        steps.push(step);
    }
    if moves && !steps.is_empty() {
        if let Err(e) = crate::manifest_commit::judge(&loaded.project_dir) {
            crate::member_url::merge_refusal(refusals, e)?;
        }
    }
    Ok(steps)
}

/// Run the steps `judge_entries` judged, in the first commit of the branch
/// the workspace is now on: the branches they create, the entries and pins
/// they set, each member they move, and one commit for all of them with no
/// relock between.
pub(crate) fn apply_entries(
    ctx: &WorkspaceContext,
    verb: &'static str,
    steps: &[EntryStep],
    opts: crate::activate::ActivateOptions,
) -> anyhow::Result<()> {
    if steps.is_empty() {
        return Ok(());
    }
    let loaded = Loaded::open(ctx, verb)?;
    let mut manifest = loaded.manifest.clone();
    let mut lock = loaded.old.clone().unwrap_or(LockFile {
        repositories: Default::default(),
    });
    let mut moved: Vec<(&EntryStep, MemberDir, crate::manifest::RepoEntry)> = Vec::new();
    for step in steps {
        let entry = manifest
            .get_entry(&step.member)
            .expect("a judged member is in the manifest")
            .clone();
        let located = loaded.checkout(ctx, &step.member);
        let vcs = vcs_for(entry.vcs_type);
        let aligns = located.aligns
            && !(loaded.in_feature() && matches!(entry.role, Role::Owned | Role::Fork));
        if aligns {
            vcs.backup_unpreserved_head(&located.dir)?;
        }
        step.apply(vcs.as_ref(), &located.dir, &mut manifest, &mut lock)?;
        if aligns {
            let mut after = entry.clone();
            after.branch = step.branch.clone();
            moved.push((step, located, after));
        }
    }
    crate::add_remove::settle_manifest(ctx, &loaded.project, Some(&manifest), &[], opts)?;
    let touches_lock = steps.iter().any(|step| step.pin.is_some());
    if touches_lock {
        crate::lock::write_lock_in(&lock, &loaded.project_dir)?;
    }
    for (step, located, after) in &moved {
        let vcs = vcs_for(after.vcs_type);
        for note in align_member(
            &loaded,
            vcs.as_ref(),
            &step.member,
            &located.dir,
            after,
            &lock,
            verb,
        )? {
            eprintln!("rwv {verb}: {}: {note}", step.member);
        }
    }

    let edits: Vec<(RepoPath, crate::manifest::RepoEntry)> = steps
        .iter()
        .map(|step| {
            let mut held = manifest
                .get_entry(&step.member)
                .cloned()
                .expect("a judged member is in the manifest");
            held.branch = step.branch.clone();
            (step.member.clone(), held)
        })
        .collect();
    let edit_manifest = move |m: &mut Manifest| {
        for (member, held) in &edits {
            if m.get_entry(member).is_some() {
                m.insert_repo(member.clone(), held.clone());
            }
        }
    };
    let pins: Vec<(RepoPath, crate::manifest::LockEntry)> = steps
        .iter()
        .filter_map(|step| {
            let pin = step.pin.as_ref()?;
            let entry = manifest.get_entry(&step.member)?;
            Some((
                step.member.clone(),
                crate::manifest::LockEntry {
                    vcs_type: entry.vcs_type,
                    url: entry.url.clone(),
                    version: crate::vcs::RawRevisionId::new(pin.id().as_str()),
                },
            ))
        })
        .collect();
    let edit_lock = move |l: &mut LockFile| {
        for (member, entry) in &pins {
            l.insert_entry(member.clone(), entry.clone());
        }
    };
    let authored = crate::manifest_commit::authored_across(
        ctx,
        &loaded.project,
        &[&loaded.manifest, &manifest],
    );
    let listing: String = steps
        .iter()
        .map(|step| {
            let was = loaded
                .manifest
                .get_entry(&step.member)
                .map(|e| e.branch.to_string())
                .unwrap_or_default();
            format!("\n  - {}: branch {was} -> {}", step.member, step.branch)
        })
        .collect();
    crate::manifest_commit::commit(&crate::manifest_commit::MemberCommit {
        project: &loaded.project,
        project_dir: &loaded.project_dir,
        edit_manifest: &edit_manifest,
        edit_lock: touches_lock.then_some(&edit_lock as &dyn Fn(&mut LockFile)),
        authored: &authored,
        message: &format!("{verb}: set member branches\n{listing}"),
        naming: crate::repin::Naming::by_name(verb, steps.iter().map(|step| step.member.clone())),
    })?;
    for step in steps {
        eprintln!("rwv {verb}: {}: branch -> {}", step.member, step.branch);
    }
    Ok(())
}

/// The arguments of `rwv member set-branch`.
pub struct SetBranch<'a> {
    pub repo: &'a str,
    pub branch: &'a str,
    pub new: bool,
    pub at: Option<&'a str>,
    pub url: Option<&'a str>,
}

fn candidate_plan(
    aligner: &Aligner,
    member: &Member<'_>,
    step: &EntryStep,
    lock: &LockFile,
) -> anyhow::Result<Plan> {
    let raw = RawRefName::new(step.branch.local_counterpart().as_str());
    let exists = member
        .vcs
        .resolve_local_branch_tip(member.dir, &raw)?
        .is_some()
        || !member.vcs.remote_copies(member.dir, &raw)?.is_empty();
    if exists {
        return aligner.plan(member, Some(lock));
    }
    let at = step
        .creates
        .as_ref()
        .map(|(_, at)| at.clone())
        .expect("a branch that exists nowhere is created by the step");
    Ok(Plan {
        target: Target::Branch {
            name: raw,
            found: Found::Local { tip: at.clone() },
            fast_forward_to: None,
        },
        pin: Pin::At(at),
        unfound: None,
        unaligned: false,
    })
}

/// Execute `rwv member set-branch <repo> <branch> [--new] [--at <rev>]
/// [--url <url>]`.
///
/// Every refusal is reported before anything changes. The entry and the lock
/// then change together, the member is switched onto the branch, and one
/// commit carries the entry and the pin and nothing else the project repo
/// holds uncommitted.
pub fn run_set_branch(
    request: SetBranch<'_>,
    ctx: &WorkspaceContext,
    opts: crate::activate::ActivateOptions,
) -> anyhow::Result<()> {
    let loaded = Loaded::open(ctx, VERB)?;
    let entry_request = BranchEntry {
        member: RepoPath::new(request.repo)?,
        branch: request.branch.to_owned(),
        rev: request.at.map(str::to_owned),
        creation: if request.new {
            Creation::New
        } else {
            Creation::Existing
        },
    };
    let member = entry_request.member.clone();
    let project_vcs = crate::vcs::project_vcs();
    let moved: Vec<_> = crate::branch::checked_out(project_vcs.as_ref(), &loaded.project_dir)?
        .map(|on| (crate::manifest::RepoKey::Project, on))
        .into_iter()
        .chain([(
            crate::manifest::RepoKey::Member(member.clone()),
            request.branch.to_owned(),
        )])
        .collect();
    crate::landing_claim::refuse_moving_claimed(
        project_vcs.as_ref(),
        &loaded.project_dir,
        &loaded.project,
        &moved,
        None,
    )?;
    let new_url: Option<crate::manifest::RepoUrl> = request.url.map(str::parse).transpose()?;
    let mut refusals = Report::default();
    let step = judge_entry(ctx, &loaded, VERB, &entry_request, &mut refusals)?;
    let located = loaded.checkout(ctx, &member);
    let dir = located.dir.clone();
    let entry = loaded
        .manifest
        .get_entry(&member)
        .expect("a judged member is in the manifest")
        .clone();
    let vcs = vcs_for(entry.vcs_type);
    let vcs = vcs.as_ref();

    if let Err(e) = crate::manifest_commit::judge(&loaded.project_dir) {
        crate::member_url::merge_refusal(&mut refusals, e)?;
    }

    let mut relink = crate::member_url::Relinking::Stays;
    if let (Some(step), Some(new_url)) = (&step, &new_url) {
        relink = crate::member_url::judge_relink_of(
            vcs,
            &member,
            &dir,
            &RawRefName::new(step.branch.local_counterpart().as_str()),
            &entry.url,
            new_url,
            VERB,
            &mut refusals,
        )?;
    }

    let mut after = entry.clone();
    if let Some(step) = &step {
        after.branch = step.branch.clone();
    }
    if let Some(new_url) = &new_url {
        after.url = new_url.clone();
    }
    let owned_in_feature = loaded.in_feature() && matches!(entry.role, Role::Owned | Role::Fork);
    let mut new_lock = loaded.old.clone().unwrap_or(LockFile {
        repositories: Default::default(),
    });
    if let Some(step) = &step {
        if let Some(pin) = &step.pin {
            new_lock.insert_entry(
                member.clone(),
                crate::manifest::LockEntry {
                    vcs_type: after.vcs_type,
                    url: after.url.clone(),
                    version: crate::vcs::RawRevisionId::new(pin.id().as_str()),
                },
            );
        }
        if located.aligns && !owned_in_feature {
            judge_move(
                &loaded,
                vcs,
                &member,
                &dir,
                &after,
                step,
                &new_lock,
                &mut refusals,
            )?;
        }
    }
    refusals.into_result()?;
    let step = step.expect("an unrefused judgment has a step");

    let witness = (located.aligns && !owned_in_feature)
        .then(|| vcs.backup_unpreserved_head(&dir))
        .transpose()?
        .flatten();

    let mut manifest = loaded.manifest.clone();
    step.apply(vcs, &dir, &mut manifest, &mut new_lock)?;
    if let Some(new_url) = &new_url {
        let mut held = manifest
            .get_entry(&member)
            .cloned()
            .expect("a judged member is in the manifest");
        held.url = new_url.clone();
        manifest.insert_repo(member.clone(), held);
    }
    crate::add_remove::settle_manifest(ctx, &loaded.project, Some(&manifest), &[], opts)?;
    if step.pin.is_some() || new_url.is_some() {
        if let Some(url) = &new_url {
            if let Some(held) = new_lock.get_entry(&member).cloned() {
                new_lock.insert_entry(
                    member.clone(),
                    crate::manifest::LockEntry {
                        url: url.clone(),
                        ..held
                    },
                );
            }
        }
        crate::lock::write_lock_in(&new_lock, &loaded.project_dir)?;
    }

    let mut notes: Vec<String> = Vec::new();
    if let Some((name, at)) = &step.creates {
        notes.push(format!("created {name} at {}", at.id().short()));
    }
    if let Some(witness) = &witness {
        notes.push(format!(
            "the commits its detached HEAD held are kept as {}",
            witness.backup().name
        ));
    }
    if located.aligns && !owned_in_feature {
        notes.extend(align_member(
            &loaded, vcs, &member, &dir, &after, &new_lock, VERB,
        )?);
    }
    relink.apply(vcs, &dir)?;
    if let Some(lines) = relink.describe(vcs, &member) {
        notes.push(lines.replacen(&format!("{member}: "), "", 1));
    }

    let branch_was = entry.branch.clone();
    let branch_now = step.branch.clone();
    let pin_now = step.pin.clone();
    let url_now = new_url.clone();
    let member_for_manifest = member.clone();
    let edit_manifest = {
        let branch_now = branch_now.clone();
        let url_now = url_now.clone();
        move |m: &mut Manifest| {
            if let Some(mut held) = m.get_entry(&member_for_manifest).cloned() {
                held.branch = branch_now.clone();
                if let Some(url) = &url_now {
                    held.url = url.clone();
                }
                m.insert_repo(member_for_manifest.clone(), held);
            }
        }
    };
    let member_for_lock = member.clone();
    let vcs_type = after.vcs_type;
    let lock_url = after.url.clone();
    let edit_lock = move |l: &mut LockFile| {
        if let Some(pin) = &pin_now {
            l.insert_entry(
                member_for_lock.clone(),
                crate::manifest::LockEntry {
                    vcs_type,
                    url: lock_url.clone(),
                    version: crate::vcs::RawRevisionId::new(pin.id().as_str()),
                },
            );
        } else if let (Some(url), Some(held)) = (&url_now, l.get_entry(&member_for_lock).cloned()) {
            l.insert_entry(
                member_for_lock.clone(),
                crate::manifest::LockEntry {
                    url: url.clone(),
                    ..held
                },
            );
        }
    };
    let settled_authored = crate::manifest_commit::authored_across(
        ctx,
        &loaded.project,
        &[&loaded.manifest, &manifest],
    );
    let touches_lock = step.pin.is_some() || new_url.is_some();
    crate::manifest_commit::commit(&crate::manifest_commit::MemberCommit {
        project: &loaded.project,
        project_dir: &loaded.project_dir,
        edit_manifest: &edit_manifest,
        edit_lock: touches_lock.then_some(&edit_lock as &dyn Fn(&mut LockFile)),
        authored: &settled_authored,
        message: &format!(
            "member set-branch {member} {branch_now}\n\n  - {member}: branch {branch_was} -> {branch_now}"
        ),
        naming: crate::repin::Naming::by_name(VERB, [member.clone()]),
    })?;

    eprintln!("rwv {VERB}: {member}: branch {branch_was} -> {branch_now}");
    for note in notes {
        eprintln!("rwv {VERB}: {member}: {note}");
    }
    if let (Some(pin), Some(was)) = (&step.pin, &step.was_pin) {
        crate::lock::report_backward_repin(vcs, &dir, VERB, &member, was, pin)?;
    }
    let own = loaded.aligner.checked_out().map(|branch| {
        format!(
            "{branch} ({})",
            crate::workspace::project_rel_path(&loaded.project)
        )
    });
    let others: Vec<String> = loaded
        .aligner
        .accounting
        .declarers(&member, branch_now.local_counterpart().as_str())
        .into_iter()
        .filter(|declarer| Some(declarer) != own.as_ref())
        .collect();
    if !others.is_empty() {
        eprintln!(
            "rwv {VERB}: {member}: {}",
            crate::diagnosis::fact_text(&crate::diagnosis::Fact {
                kind: crate::diagnosis::FactKind::Shared,
                about: Some(format!(
                    "{} is also declared by {}; a landing into any of them moves it",
                    branch_now.local_counterpart().as_str(),
                    others.join(", ")
                )),
            })
        );
    }
    if step.ahead_of_rev {
        eprintln!(
            "rwv {VERB}: {member}: {branch_now} is ahead of the pin: a landing merges into its \
             tip, so a member whose pin must stay put needs a branch of its own \
             (`rwv member set-branch {member} <b> --new --at <rev>`)"
        );
    }
    Ok(())
}
