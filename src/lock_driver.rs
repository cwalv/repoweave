//! The lock merge driver: the command the VCS runs to merge `rwv.lock` in a
//! merge rwv does not make, the setup that has it run, and the refusals that
//! hold every verb off a project whose setup or lock it leaves unsettled.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::Path;

use anyhow::Context;

use crate::check::MissingLockMergeDriverKind;
use crate::diagnosis::{ChoiceId, Report, State};
use crate::lock_merge::{merge_pins, Held, PinFinding, PinMerge, Reach};
use crate::manifest::{LockEntry, LockFile, Manifest, RepoPath};
use crate::vcs::{vcs_for, CommitId, LockDriverPart, ResolvedRevisionId, Vcs};

/// Refuse when `dir`'s lock merge driver is not set up as this release sets
/// it up, one line per part that is not. `workspace` is the workspace `dir`
/// belongs to when that is not the one the verb runs in.
pub(crate) fn refuse_unmigrated(
    vcs: &dyn Vcs,
    repo: impl fmt::Display,
    dir: &Path,
    workspace: Option<&Path>,
) -> anyhow::Result<()> {
    if !vcs.is_repo(dir) {
        return Ok(());
    }
    let setup = vcs.lock_driver_setup(dir).with_context(|| {
        format!(
            "failed to read the lock merge driver setup in {}",
            dir.display()
        )
    })?;
    let mut report = Report::default();
    for gap in MissingLockMergeDriverKind::of(setup) {
        let judging = gap.state().of(&repo).project_repo().checkout(dir);
        report.add(
            match workspace {
                Some(workspace) => judging.elsewhere(workspace),
                None => judging,
            }
            .judge(vcs),
        );
    }
    report.into_result()
}

/// Refuse while `dir`'s `rwv.lock` is unmerged: whatever the working file
/// holds, it is not a lock any side agreed on.
pub(crate) fn refuse_unresolved(
    vcs: &dyn Vcs,
    repo: impl fmt::Display,
    dir: &Path,
    workspace: Option<&Path>,
) -> anyhow::Result<()> {
    if !vcs.is_repo(dir) {
        return Ok(());
    }
    let unmerged = vcs.unmerged_paths(dir)?;
    if !unmerged.iter().any(|path| path == LockFile::FILE_NAME) {
        return Ok(());
    }
    let judging = State::EditsLockUnresolved
        .of(repo)
        .project_repo()
        .listing([LockFile::FILE_NAME])
        .bind_list("<paths>", [LockFile::FILE_NAME])
        .checkout(dir);
    let (judging, recorded) = crate::commit_guard::with_recorded_operation(judging, workspace);
    let without: &[ChoiceId] = if recorded {
        &[ChoiceId::C32]
    } else {
        &[ChoiceId::ResumeOperation, ChoiceId::AbortOperation]
    };
    Err(judging.without(without).judge(vcs).refused())
}

/// What setting up the lock merge driver did.
#[derive(Debug, Default)]
pub(crate) struct Migrated {
    /// The checked-out commit's declaration was rewritten, in a commit
    /// holding only that.
    pub(crate) committed: bool,
    /// The working copy's declaration was rewritten.
    pub(crate) declared: bool,
    /// The clone's definition was written.
    pub(crate) defined: bool,
    /// Why the commit was not made, when it was owed and withheld.
    pub(crate) withheld: Option<String>,
}

/// Set the lock merge driver up in the project repo `dir`: the declaration
/// rewritten in the checked-out commit, committing only that, and in the
/// working copy with its other edits in place; the definition replaced.
///
/// The commit holds the rewrite alone, whatever else is staged; it is
/// withheld, the working copy then left alone too, while an operation is in
/// progress.
pub(crate) fn migrate(vcs: &dyn Vcs, dir: &Path) -> anyhow::Result<Migrated> {
    let setup = vcs.lock_driver_setup(dir)?;
    let mut migrated = Migrated::default();
    if setup.declaration != LockDriverPart::Current {
        let file = vcs.lock_driver_declaration_file();
        let head = vcs.head_revision(dir).ok();
        let committed = head
            .as_ref()
            .and_then(|head| vcs.read_file_at_revision(dir, head, Path::new(file)).ok());
        let rewritten = head
            .as_ref()
            .and_then(|_| vcs.declare_lock_driver_in(committed.as_deref()));
        match (&rewritten, vcs.mid_operation(dir)) {
            (Some(_), Some(op)) => {
                migrated.withheld = Some(format!(
                    "a {} is in progress in the project repo; finish or abort it first",
                    op.operation()
                ))
            }
            (Some(text), None) => {
                vcs.stage_content(dir, Path::new(file), text.as_bytes())?;
                migrated.committed = crate::repin::commit(
                    vcs,
                    dir,
                    &[file],
                    "chore: set up the rwv.lock merge driver (rwv doctor --fix)",
                    None,
                    &crate::repin::Naming::Unnamed,
                )
                .with_context(|| format!("failed to commit in {}", dir.display()))?;
            }
            (None, _) => {}
        }
        if migrated.withheld.is_none() {
            migrated.declared = declare_in_working_copy(vcs, dir)?;
        }
    }
    if setup.definition != LockDriverPart::Current {
        vcs.define_lock_driver(dir)?;
        migrated.defined = true;
    }
    Ok(migrated)
}

/// Rewrite the working copy's declaration; `true` when it changed.
pub(crate) fn declare_in_working_copy(vcs: &dyn Vcs, dir: &Path) -> anyhow::Result<bool> {
    let path = dir.join(vcs.lock_driver_declaration_file());
    let working = match std::fs::read_to_string(&path) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            return Err(anyhow::Error::new(e).context(format!("failed to read {}", path.display())))
        }
    };
    let Some(rewritten) = vcs.declare_lock_driver_in(working.as_deref()) else {
        return Ok(false);
    };
    crate::durable_file::replace(&path, rewritten.as_bytes())
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(true)
}

/// How the driver ended: the merged lock written, or the file left
/// conflicted.
#[derive(Debug, PartialEq, Eq)]
pub enum DriverOutcome {
    Merged,
    Conflicted,
}

/// Merge the lock files `base`, `ours` and `theirs` as the VCS hands them to a
/// merge driver, writing the result over `ours`, from the project repo
/// checkout `dir` the VCS runs the driver in.
///
/// Pins merge three-way by [`merge_pins`]. Which branch each side declares a
/// member on is read from each side's `rwv.toml` at its commit, where the VCS
/// names both commits; where it does not, any member the two sides moved
/// differently is left for the operator, because the rule that settles it
/// turns on what each side declares. A member left unsettled leaves the whole
/// file conflicted: both locks, between conflict markers, which no verb reads
/// as a lock.
pub fn run(base: &Path, ours: &Path, theirs: &Path, dir: &Path) -> anyhow::Result<DriverOutcome> {
    let read = |path: &Path| -> anyhow::Result<String> {
        std::fs::read_to_string(path).map_err(|e| {
            crate::untokened!(
                HookInput,
                "rwv lock --merge-driver: cannot read {}: {e:#}; this form is the command a VCS \
                 runs to merge rwv.lock, given the base, ours and theirs files it wrote",
                path.display()
            )
        })
    };
    let (base_text, ours_text, theirs_text) = (read(base)?, read(ours)?, read(theirs)?);
    let vcs = crate::vcs::project_vcs();
    let decided = decide(vcs.as_ref(), dir, [&base_text, &ours_text, &theirs_text]);
    let (written, outcome) = match decided {
        Ok(lock) => {
            let mut json =
                serde_json::to_string_pretty(&lock).context("failed to serialize lock file")?;
            json.push('\n');
            (json, DriverOutcome::Merged)
        }
        Err(unsettled) => {
            for line in &unsettled {
                eprintln!("rwv lock --merge-driver: {line}");
            }
            (
                conflicted(&ours_text, &theirs_text),
                DriverOutcome::Conflicted,
            )
        }
    };
    std::fs::write(ours, written).with_context(|| format!("failed to write {}", ours.display()))?;
    Ok(outcome)
}

fn conflicted(ours: &str, theirs: &str) -> String {
    let mut text = String::from("<<<<<<< ours\n");
    for side in [ours, "=======\n", theirs] {
        text.push_str(side);
        if !text.ends_with('\n') {
            text.push('\n');
        }
    }
    text.push_str(">>>>>>> theirs\n");
    text
}

/// The three locks' pins, or `None` where one does not parse or pins a member
/// by anything but a commit id.
fn pins_of(text: &str) -> Option<(Held, BTreeMap<RepoPath, LockEntry>)> {
    let lock = LockFile::from_json_str(text).ok()?;
    let mut held = Held::default();
    for (path, entry) in lock.iter_entries() {
        held.pins
            .insert(path.clone(), entry.version.as_commit_id()?);
    }
    Some((held, lock.repo_map().clone()))
}

fn decide(vcs: &dyn Vcs, dir: &Path, texts: [&str; 3]) -> Result<LockFile, Vec<String>> {
    let [base, ours, theirs] = texts.map(pins_of);
    let (Some(base), Some(ours), Some(theirs)) = (base, ours, theirs) else {
        return Err(vec![
            "a side's rwv.lock does not parse, or pins a member by name; left conflicted"
                .to_owned(),
        ]);
    };
    let (mut base_held, base_entries) = base;
    let (mut ours_held, ours_entries) = ours;
    let (mut theirs_held, theirs_entries) = theirs;

    let sides = vcs.merging_commits(dir);
    let mut ours_manifest = None;
    if let Some((ours_commit, theirs_commit)) = &sides {
        ours_held.declared = declared_at(vcs, dir, ours_commit);
        theirs_held.declared = declared_at(vcs, dir, theirs_commit);
        if let Ok([only]) = <[CommitId; 1]>::try_from(
            vcs.merge_bases(dir, ours_commit, theirs_commit)
                .unwrap_or_default(),
        ) {
            base_held.declared = declared_at(vcs, dir, &only);
        }
        ours_manifest = manifest_at(vcs, dir, ours_commit);
    }

    let members: BTreeSet<&RepoPath> = base_held
        .pins
        .keys()
        .chain(ours_held.pins.keys())
        .chain(theirs_held.pins.keys())
        .collect();
    let mut merged_declared = BTreeMap::new();
    let mut undecided = BTreeSet::new();
    for member in members {
        let branch = three_way(
            base_held.declared.get(member),
            ours_held.declared.get(member),
            theirs_held.declared.get(member),
        );
        match branch {
            Some(Some(branch)) => {
                merged_declared.insert(member.clone(), branch.clone());
            }
            Some(None) | None => {
                undecided.insert(member.clone());
                merged_declared.insert(member.clone(), String::new());
            }
        }
    }

    let reach = MemberReach {
        root: weave_root_of(dir),
        manifest: ours_manifest.as_ref(),
    };
    let merged = merge_pins(
        &PinMerge {
            base: Some(&base_held),
            first: &ours_held,
            second: &theirs_held,
            merged_declared: &merged_declared,
            member_merges: &BTreeMap::new(),
        },
        &reach,
    );

    let mut unsettled = Vec::new();
    for finding in &merged.findings {
        match finding {
            PinFinding::Unmerged {
                member,
                first,
                second,
            } => unsettled.push(format!(
                "{member}: both sides moved the pin under one branch and neither contains the \
                 other ({} and {}); left conflicted with both pins marked",
                first.short(),
                second.short()
            )),
            PinFinding::DeclaredDiffer {
                member,
                kept_pin,
                other_pin,
                ..
            } if undecided.contains(member) => unsettled.push(format!(
                "{member}: the sides pin {} and {}, and which branch the merge keeps for it is \
                 not known here; left conflicted with both pins marked",
                kept_pin.short(),
                other_pin.short()
            )),
            PinFinding::DeclaredDiffer {
                member,
                kept_pin,
                other_pin,
                ..
            } => eprintln!(
                "rwv lock --merge-driver: {member}: the sides declare it differently; kept the \
                 pin {} of the branch the merge keeps; the other side's {} is not carried",
                kept_pin.short(),
                other_pin.short()
            ),
        }
    }
    if !unsettled.is_empty() {
        return Err(unsettled);
    }

    let mut lock = LockFile {
        repositories: BTreeMap::new(),
    };
    for (member, pin) in &merged.pins {
        let entry = [&ours_entries, &theirs_entries, &base_entries]
            .into_iter()
            .filter_map(|entries| entries.get(member))
            .find(|entry| entry.version.as_commit_id().as_ref() == Some(pin))
            .expect("a merged pin is one a side holds");
        lock.insert_entry(member.clone(), entry.clone());
    }
    Ok(lock)
}

/// The three-way merge of one value: `Some` with the merged value, `None`
/// when both sides changed it differently.
fn three_way<'a, T: PartialEq>(
    base: Option<&'a T>,
    ours: Option<&'a T>,
    theirs: Option<&'a T>,
) -> Option<Option<&'a T>> {
    if ours == theirs || theirs == base {
        Some(ours)
    } else if ours == base {
        Some(theirs)
    } else {
        None
    }
}

fn manifest_at(vcs: &dyn Vcs, dir: &Path, commit: &CommitId) -> Option<Manifest> {
    let text = vcs
        .read_file_at_revision(
            dir,
            &ResolvedRevisionId::from(commit.clone()),
            Path::new(Manifest::FILE_NAME),
        )
        .ok()?;
    Manifest::from_toml_str(&text).ok()
}

fn declared_at(vcs: &dyn Vcs, dir: &Path, commit: &CommitId) -> BTreeMap<RepoPath, String> {
    manifest_at(vcs, dir, commit)
        .map(|manifest| {
            manifest
                .iter_entries()
                .map(|(path, entry)| {
                    (
                        path.clone(),
                        entry.branch.local_counterpart().as_str().to_owned(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

fn weave_root_of(dir: &Path) -> Option<&Path> {
    dir.ancestors()
        .skip(1)
        .find(|candidate| crate::workspace::is_workspace_root(candidate))
}

struct MemberReach<'a> {
    root: Option<&'a Path>,
    manifest: Option<&'a Manifest>,
}

impl Reach for MemberReach<'_> {
    fn reaches(&self, member: &RepoPath, ancestor: &CommitId, of: &CommitId) -> Option<bool> {
        let entry = self.manifest?.get_entry(member)?;
        vcs_for(entry.vcs_type)
            .is_ancestor(
                &self.root?.join(member.as_path()),
                &ResolvedRevisionId::from(ancestor.clone()),
                &ResolvedRevisionId::from(of.clone()),
            )
            .ok()
    }

    fn named(&self, member: &RepoPath, name: &str) -> Option<CommitId> {
        let entry = self.manifest?.get_entry(member)?;
        Some(
            vcs_for(entry.vcs_type)
                .resolve_revision(&self.root?.join(member.as_path()), name)
                .ok()?
                .id()
                .clone(),
        )
    }
}
