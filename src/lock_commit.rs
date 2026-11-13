//! `rwv lock --commit` over a `rwv.toml` that differs from the committed one:
//! each changed entry's setter step runs first, then the rest of the file is
//! committed as it stands, with the files its regeneration rewrites.

use crate::diagnosis::Report;
use crate::entry_report::{between, entries_of, MemberChange};
use crate::lock::{CommitMessage, Relocking, WholeFile};
use crate::manifest::{History, LockEntry, LockFile, Manifest, RepoPath, Role};
use crate::member_branch::{BranchEntry, Creation, EntryStep, Loaded};
use crate::vcs::{project_vcs, vcs_for, RawRevisionId};
use crate::workspace::WorkspaceContext;
use std::collections::BTreeSet;

const VERB: &str = "lock";

/// What the working `rwv.toml` changes against the committed one.
pub(crate) struct WholeChanges {
    changed: Vec<MemberChange>,
    added: Vec<RepoPath>,
    removed: Vec<RepoPath>,
    top_level_history: Option<(Option<History>, Option<History>)>,
    other_tables: bool,
}

fn other_tables(manifest: &Manifest) -> toml::Value {
    let mut value = toml::Value::Table(manifest.declared().unwrap_or_default());
    if let Some(table) = value.as_table_mut() {
        table.remove("repositories");
        table.remove("history");
    }
    value
}

/// The changes the whole-file commit carries, or `None` where the working
/// `rwv.toml` is the committed one.
pub(crate) fn plan(
    ctx: &WorkspaceContext,
    _relocking: &Relocking<'_>,
) -> anyhow::Result<Option<WholeChanges>> {
    let (project, _) = crate::lock::scope(ctx)?;
    let dir = crate::workspace::project_dir(ctx.active_path(), &project);
    let vcs = project_vcs();
    if !vcs.is_repo(&dir) {
        return Ok(None);
    }
    let Some(head) = crate::manifest_commit::committed_manifest(vcs.as_ref(), &dir)? else {
        return Ok(None);
    };
    let working = Manifest::from_path(&dir.join(Manifest::FILE_NAME))?;
    let (before, after) = (entries_of(&head), entries_of(&working));
    let changed = between(&before, &after);
    let added: Vec<RepoPath> = working
        .iter_repo_paths()
        .filter(|path| !head.contains_repo(path))
        .cloned()
        .collect();
    let removed: Vec<RepoPath> = head
        .iter_repo_paths()
        .filter(|path| !working.contains_repo(path))
        .cloned()
        .collect();
    let other_tables = other_tables(&head) != other_tables(&working);
    let top_level_history =
        (head.history != working.history).then_some((head.history, working.history));
    if changed.is_empty()
        && added.is_empty()
        && removed.is_empty()
        && top_level_history.is_none()
        && !other_tables
    {
        return Ok(None);
    }
    Ok(Some(WholeChanges {
        changed,
        added,
        removed,
        top_level_history,
        other_tables,
    }))
}

struct Moving {
    member: RepoPath,
    step: EntryStep,
    after: crate::manifest::RepoEntry,
}

fn relocking_for<'a>(relocking: &Relocking<'a>, whole: WholeFile<'a>) -> Relocking<'a> {
    Relocking {
        verb: relocking.verb,
        acting: relocking.acting,
        dirty: relocking.dirty,
        first_pins: relocking.first_pins,
        unaligned: relocking.unaligned,
        whole,
        op: relocking.op,
        by_name: relocking.by_name,
    }
}

pub(crate) fn run(
    ctx: &WorkspaceContext,
    relocking: &Relocking<'_>,
    changes: WholeChanges,
    commit: Option<CommitMessage>,
) -> anyhow::Result<()> {
    crate::op_state::check_no_op_in_progress(&[ctx.active_path()])?;
    let loaded = Loaded::open(ctx, VERB)?;
    let working = loaded.manifest.clone();
    let committed =
        crate::manifest_commit::committed_manifest(project_vcs().as_ref(), &loaded.project_dir)?
            .unwrap_or_else(|| working.clone());
    let authored =
        crate::manifest_commit::authored_across(ctx, &loaded.project, &[&committed, &working]);

    let mut refusals = Report::default();
    let mut moving: Vec<Moving> = Vec::new();
    let mut lock = loaded.old.clone().unwrap_or(LockFile {
        repositories: Default::default(),
    });
    for change in changes.changed.iter().filter(|c| c.changed("branch")) {
        let request = BranchEntry {
            member: change.path.clone(),
            branch: change.now.branch.local_counterpart().as_str().to_owned(),
            rev: None,
            creation: Creation::Existing,
        };
        let Some(step) =
            crate::member_branch::judge_entry(ctx, &loaded, VERB, &request, &mut refusals)?
        else {
            continue;
        };
        if let Some(pin) = &step.pin {
            lock.insert_entry(
                change.path.clone(),
                LockEntry {
                    vcs_type: change.now.vcs_type,
                    url: change.now.url.clone(),
                    version: RawRevisionId::new(pin.id().as_str()),
                },
            );
        }
        moving.push(Moving {
            member: change.path.clone(),
            step,
            after: change.now.clone(),
        });
    }
    for entry in &moving {
        let located = loaded.checkout(ctx, &entry.member);
        let owned_in_feature =
            loaded.in_feature() && matches!(entry.after.role, Role::Owned | Role::Fork);
        if located.aligns && !owned_in_feature && entry.step.pin.is_some() {
            let vcs = vcs_for(entry.after.vcs_type);
            crate::member_branch::judge_move(
                &loaded,
                vcs.as_ref(),
                &entry.member,
                &located.dir,
                &entry.after,
                &entry.step,
                &lock,
                &mut refusals,
            )?;
        }
    }
    let mut relinks = Vec::new();
    for change in changes.changed.iter().filter(|c| c.changed("url")) {
        let located = loaded.checkout(ctx, &change.path);
        let vcs = vcs_for(change.now.vcs_type);
        let relink = crate::member_url::judge_relink(
            vcs.as_ref(),
            &change.path,
            &located.dir,
            &change.was.url,
            &change.now.url,
            VERB,
            &mut refusals,
        )?;
        relinks.push((
            change.path.clone(),
            located.dir.clone(),
            change.now.vcs_type,
            relink,
        ));
    }
    for change in changes
        .changed
        .iter()
        .filter(|c| c.changed("role") && c.now.role == Role::Reference)
    {
        let located = loaded.checkout(ctx, &change.path);
        let vcs = vcs_for(change.now.vcs_type);
        if !vcs.is_repo(&located.dir) {
            continue;
        }
        let dirty = vcs.tracked_dirty_file_names(&located.dir)?;
        if let Some(judging) = (!dirty.is_empty())
            .then(|| {
                crate::diagnosis::dirty_beside_drift(
                    vcs.as_ref(),
                    &change.path,
                    &located.dir,
                    dirty,
                )
            })
            .flatten()
        {
            refusals.add(judging.checkout(&located.dir).judge(vcs.as_ref()));
        }
    }
    if let Some(path) = changes
        .added
        .iter()
        .find(|path| !loaded.checkout(ctx, path).dir.exists())
    {
        refusals.into_result()?;
        return Err(crate::untokened!(
            Owed,
            "{path}: the manifest lists a member whose directory is absent; run `rwv update` \
             to materialize it, then re-run `rwv lock --commit`"
        ));
    }
    let skipping: BTreeSet<RepoPath> = moving.iter().map(|m| m.member.clone()).collect();
    if let Err(e) = crate::lock::relock(
        ctx,
        &relocking_for(
            relocking,
            WholeFile {
                skipping: Some(&skipping),
                judge_only: true,
            },
        ),
        Some(CommitMessage::Generated),
    ) {
        crate::member_url::merge_refusal(&mut refusals, e)?;
    }
    refusals.into_result()?;

    let vcs_of = |t| vcs_for(t);
    for path in &changes.added {
        let entry = working
            .get_entry(path)
            .expect("an added member is in the manifest");
        let vcs = vcs_of(entry.vcs_type);
        let located = loaded.checkout(ctx, path);
        if let Some(pin) = crate::add_remove::first_pin(vcs.as_ref(), &located.dir, entry)? {
            lock.insert_entry(
                path.clone(),
                LockEntry {
                    vcs_type: entry.vcs_type,
                    url: entry.url.clone(),
                    version: RawRevisionId::new(pin.id().as_str()),
                },
            );
        }
    }
    let mut scratch = working.clone();
    for entry in &moving {
        let located = loaded.checkout(ctx, &entry.member);
        let vcs = vcs_of(entry.after.vcs_type);
        entry
            .step
            .apply(vcs.as_ref(), &located.dir, &mut scratch, &mut lock)?;
    }
    if !moving.is_empty() || !changes.added.is_empty() {
        crate::lock::write_lock_in(&lock, &loaded.project_dir)?;
    }
    for entry in &moving {
        let located = loaded.checkout(ctx, &entry.member);
        let owned_in_feature =
            loaded.in_feature() && matches!(entry.after.role, Role::Owned | Role::Fork);
        if located.aligns && !owned_in_feature && entry.step.pin.is_some() {
            let vcs = vcs_of(entry.after.vcs_type);
            crate::member_branch::align_member(
                &loaded,
                vcs.as_ref(),
                &entry.member,
                &located.dir,
                &entry.after,
                &lock,
                VERB,
            )?;
        }
    }
    for (member, dir, vcs_type, relink) in &relinks {
        let vcs = vcs_of(*vcs_type);
        relink.apply(vcs.as_ref(), dir)?;
        if let Some(lines) = relink.describe(vcs.as_ref(), member) {
            eprintln!("rwv {VERB}: {lines}");
        }
    }
    for change in changes.changed.iter().filter(|c| c.changed("role")) {
        let located = loaded.checkout(ctx, &change.path);
        let vcs = vcs_of(change.now.vcs_type);
        if vcs.is_repo(&located.dir) && !located.dir.is_symlink() {
            crate::settle::settle(vcs.as_ref(), &located.dir, change.now.role)?;
        }
    }
    crate::add_remove::settle_manifest(
        ctx,
        &loaded.project,
        None,
        &[],
        crate::activate::ActivateOptions {
            no_materialize: true,
        },
    )?;

    let pending = crate::lock::relock(
        ctx,
        &relocking_for(
            relocking,
            WholeFile {
                skipping: None,
                judge_only: false,
            },
        ),
        commit,
    )?;
    report(ctx, &loaded, &changes, &moving)?;
    if let Some(pending) = pending {
        let named = moving
            .iter()
            .filter(|entry| entry.step.pin.is_some())
            .map(|entry| entry.member.clone());
        crate::lock::commit_project_lock(&pending.by_name("lock --commit", named), &authored)?;
    }
    Ok(())
}

fn report(
    ctx: &WorkspaceContext,
    loaded: &Loaded,
    changes: &WholeChanges,
    moving: &[Moving],
) -> anyhow::Result<()> {
    if let Some((was, now)) = changes.top_level_history {
        eprintln!(
            "{}, committed as the file has it",
            crate::set_history::change_line(
                VERB,
                &crate::set_history::HistoryKey::TopLevel(loaded.project.clone()),
                was,
                now
            )
        );
    }
    for change in &changes.changed {
        for field in &change.fields {
            eprintln!(
                "rwv {VERB}: {}: {} {} -> {}, committed as the file has it",
                change.path, field.name, field.was, field.now
            );
        }
    }
    for entry in moving {
        if let Some(pin) = &entry.step.pin {
            eprintln!(
                "rwv {VERB}: {}: pinned at {} on {}",
                entry.member,
                pin.id().short(),
                entry.after.branch
            );
            if let Some(was) = &entry.step.was_pin {
                crate::lock::report_backward_repin(
                    vcs_for(entry.after.vcs_type).as_ref(),
                    &loaded.checkout(ctx, &entry.member).dir,
                    VERB,
                    &entry.member,
                    was,
                    pin,
                )?;
            }
        }
    }
    for path in &changes.added {
        eprintln!("rwv {VERB}: {path}: a member added by hand, committed with its first pin");
    }
    for path in &changes.removed {
        eprintln!("rwv {VERB}: {path}: a member removed by hand; its checkout is kept");
    }
    if changes.other_tables {
        eprintln!(
            "rwv {VERB}: the other tables of rwv.toml changed, committed as they stand with the \
             files regenerated from them"
        );
    }
    Ok(())
}
