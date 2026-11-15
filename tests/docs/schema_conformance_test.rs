//! Every committed artifact under `docs/reference/schemas/` must have output
//! validated against it, and the registry that says which ones do is the
//! directory listing rather than a list someone maintains.
//!
//! `--json` is advertised as the agent-integration surface, each envelope
//! embeds a `$schema` URL naming one of these files, and the files are
//! regenerated from the Rust types by `cargo run --example generate-explain`.
//! Regeneration proves the artifact matches the type. It does not prove a
//! serialized envelope satisfies the artifact: a `skip_serializing_if` that
//! drops a required key, a `flatten` that moves one, a rename on one side
//! only, all survive regeneration. This file is where that is asked, of every
//! verb, from one table.
//!
//! Documents here are built by serializing the production envelope types. What
//! a verb writes to stdout is a separate question with a separate answer in
//! `tests/docs/schema_conformance_wire_test.rs`: an envelope this file blesses can
//! still reach nobody if the print path mints its own bytes.
//!
//! The validator, and the draft-07 subset it implements, is
//! `tests/common/json_schema.rs`.
//!
//! # Adding a field to an envelope
//!
//! Six of the seven envelopes are built here with struct literals, so a new
//! field is a compile error naming the file and line. That is the intended
//! cost: the alternative is a corpus that silently never samples the new field,
//! which validates and proves nothing about it. Add the field to the sample,
//! and to the second sample if the field is optional — an `Option` wants both
//! settings, since `skip_serializing_if` makes them different wire shapes.
//!
//! **`doctor` is the exception, and it is the gap in that property.** Its
//! report renderer takes only what the findings pipeline builds, so its
//! documents come from `common::doctor_corpus::wire_document`, which writes the
//! `DoctorJsonOutput` literal itself and mints each record through the
//! production `ViolationOutput::from_violation`. A new field on
//! `ViolationOutput` leaves this file compiling, sampled at whatever that
//! renderer yields from the inputs below, which for an `Option` is plausibly
//! the absent shape and nothing else. Minting the records in production code
//! is what makes them real emissions rather than a test's guess at them, so
//! the exchange is deliberate; it is not free.
//!
//! [`every_corpus_carries_every_envelope_field_its_schema_declares`] narrows
//! that exception to nested fields: a new *envelope* field on any verb,
//! doctor included, cannot reach the regenerated artifact and stay unsampled.
//! A new field on a nested type doctor mints inside the builder still can.
//!
//! Two neighboring shapes do *not* widen the gap, both checked rather than
//! assumed. A new *variant* is caught by
//! [`every_corpus_exercises_every_member_its_schema_declares`], which reads the
//! regenerated artifact. And no envelope reaches a type through
//! `#[serde(flatten)]`: `src/` has none. The `allOf` in
//! `docs/reference/schemas/update.json` and `doctor.json` is schemars wrapping
//! a single `$ref` beside a description, not a flattened struct.
//!
//! # Residue
//!
//!   - `doctor` is registered here for envelope conformance and for
//!     `ViolationOutput` variant width, taken from `tests/common/doctor_corpus.rs`.
//!     `tests/docs/doctor_schema_conformance_test.rs` is the doctor-specific
//!     instrument and holds the seeded-divergence corpus for that envelope.
//!   - Enum-member coverage is a floor, not a proof of variant completeness:
//!     it reads members the artifact declares under a property name, so a
//!     member reachable only through an array's `items` is not counted.

use crate::common;

use common::json_schema;
use repoweave::backup::{
    BackupJsonOutput, BackupOutput, HeldCommitOutput, RepoBackupsOutput, BACKUP_SCHEMA_URL,
};
use repoweave::branch::{
    BranchDeleteJsonOutput, BranchListJsonOutput, ListedBranch, RecordOutput, ValueOutput,
    BRANCH_DELETE_SCHEMA_URL, BRANCH_SCHEMA_URL,
};
use repoweave::check::{CheckViolation, PlacementConvergence, ProvenanceKind, DOCTOR_SCHEMA_URL};
use repoweave::clone::{CloneJsonOutput, CLONE_SCHEMA_URL};
use repoweave::deletion::{Deleted, Outcome, Reason, Survivor};
use repoweave::diagnosis::{
    Axis, ChoiceOutput, FactKind, FactOutput, JudgedOutput, RefusalJsonOutput, REFUSAL_SCHEMA_URL,
};
use repoweave::fetch::{
    BranchMoveKind, BranchMoveOutput, FetchJsonOutput, FetchStatus, FetchedRepoOutput,
    FETCH_SCHEMA_URL,
};
use repoweave::integration::{Issue, IssueKind, IssueSource, Severity};
use repoweave::integration::{MemberIncompatibility, MissingTool, Observed};
use repoweave::integrations::GoWork;
use repoweave::integrations::NpmWorkspaces;
use repoweave::log::{LogCommit, LogEndpoint, LogJsonOutput, LogRepo, LOG_SCHEMA_URL};
use repoweave::op_state::{OpPhase, OpVerb};
use repoweave::path_spelling::WirePath;
use repoweave::placement::PlacementOutput;
use repoweave::pull::{
    EntryChange, MemberChangeOutput, ProjectTakeInOutput, PullJsonOutput, RePinnedOutput, TakeIn,
    PULL_SCHEMA_URL,
};
use repoweave::push::{PushJsonOutput, PushOutcomeOutput, PUSH_SCHEMA_URL};
use repoweave::records::Mode;
use repoweave::share::FilePlacement;
use repoweave::skipped::{SkipReason, SkippedEffect};
use repoweave::status::{
    LockRelation, OpStatus, ParentInfo, RepoStatus, StatusJsonOutput, STATUS_SCHEMA_URL,
};
use repoweave::sync::{
    ContainmentOutput, ContainmentVerdictOutput, LandJsonOutput, LandingReportOutput,
    LeftBehindOutput, LeftCommitOutput, MemberOutput, PinTakenInOutput, ReplayBaseline,
    Step3AdvanceOutput, SyncFailureOutput, SyncJsonOutput, SyncOutcomeOutput, LAND_JSON_SCHEMA_URL,
    SYNC_JSON_SCHEMA_URL,
};
use repoweave::update::{
    MemberOutcomeOutput, MemberOutcomeStatus, UpdateJsonOutput, UPDATE_SCHEMA_URL,
};
use repoweave::update_remote::{
    RepoUpdateRecord, UpdateKind, UpdateRemoteJsonOutput, UPDATE_REMOTE_SCHEMA_URL,
};
use repoweave::vcs::BackupMark;
use repoweave::vcs::{ConflictOp, MidOperation, VcsErrorOutput};
use repoweave::workspace::{AdvisoryKindOutput, AdvisoryOutput, Resolution};
use repoweave::workweave::{
    DiscardedOutput, KeptWork, UnlandedBranch, WorkweaveDeleteJsonOutput, WORKWEAVE_SCHEMA_URL,
};
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashMap};
use std::path::Path;

// The bundled rwv-each plugin's wire types: a binary's items are not linkable
// from here, so its source is compiled in.
#[path = "../../src/bin/rwv-each/schema_url.rs"]
mod each_schema_url;
#[allow(dead_code)]
#[path = "../../src/bin/rwv-each/wire.rs"]
mod each_wire;

// ---------------------------------------------------------------------------
// The registry
// ---------------------------------------------------------------------------

/// One verb's committed artifact and the emissions validated against it.
struct VerbCase {
    verb: &'static str,
    /// The URL the verb embeds. Pinned separately from the artifact it names:
    /// a consumer fetches what the bytes point at, not what a test knows.
    schema_url: &'static str,
    corpus: fn() -> Vec<(&'static str, Value)>,
}

fn cases() -> Vec<VerbCase> {
    vec![
        VerbCase {
            verb: "backup",
            schema_url: BACKUP_SCHEMA_URL,
            corpus: backup_corpus,
        },
        VerbCase {
            verb: "branch",
            schema_url: BRANCH_SCHEMA_URL,
            corpus: branch_corpus,
        },
        VerbCase {
            verb: "branch-delete",
            schema_url: BRANCH_DELETE_SCHEMA_URL,
            corpus: branch_delete_corpus,
        },
        VerbCase {
            verb: "doctor",
            schema_url: DOCTOR_SCHEMA_URL,
            corpus: doctor_corpus,
        },
        VerbCase {
            verb: "clone",
            schema_url: CLONE_SCHEMA_URL,
            corpus: clone_corpus,
        },
        VerbCase {
            verb: "fetch",
            schema_url: FETCH_SCHEMA_URL,
            corpus: fetch_corpus,
        },
        VerbCase {
            verb: "log",
            schema_url: LOG_SCHEMA_URL,
            corpus: log_corpus,
        },
        VerbCase {
            verb: "pull",
            schema_url: PULL_SCHEMA_URL,
            corpus: pull_corpus,
        },
        VerbCase {
            verb: "push",
            schema_url: PUSH_SCHEMA_URL,
            corpus: push_corpus,
        },
        VerbCase {
            verb: "status",
            schema_url: STATUS_SCHEMA_URL,
            corpus: status_corpus,
        },
        VerbCase {
            verb: "sync",
            schema_url: SYNC_JSON_SCHEMA_URL,
            corpus: sync_corpus,
        },
        VerbCase {
            verb: "land",
            schema_url: LAND_JSON_SCHEMA_URL,
            corpus: sync_to_corpus,
        },
        VerbCase {
            verb: "cherry-pick",
            schema_url: repoweave::cherry_pick::CHERRY_PICK_JSON_SCHEMA_URL,
            corpus: cherry_pick_corpus,
        },
        VerbCase {
            verb: "update",
            schema_url: UPDATE_SCHEMA_URL,
            corpus: update_corpus,
        },
        VerbCase {
            verb: "update-remote",
            schema_url: UPDATE_REMOTE_SCHEMA_URL,
            corpus: update_remote_corpus,
        },
        VerbCase {
            verb: "refusal",
            schema_url: REFUSAL_SCHEMA_URL,
            corpus: refusal_corpus,
        },
        VerbCase {
            verb: "each",
            schema_url: each_schema_url::EACH_SCHEMA_URL,
            corpus: each_corpus,
        },
        VerbCase {
            verb: "workweave",
            schema_url: WORKWEAVE_SCHEMA_URL,
            corpus: workweave_corpus,
        },
    ]
}

// ---------------------------------------------------------------------------
// Shared sample values
// ---------------------------------------------------------------------------

const REPO: &str = "github/acme/repo";
const ABS: &str = "/ws/github/acme/repo";

fn wire(path: &str) -> WirePath {
    WirePath::of(Path::new(path))
}

fn sha(byte: char) -> String {
    std::iter::repeat_n(byte, 40).collect()
}

fn resolution() -> Resolution {
    Resolution {
        workspace: repoweave::path_spelling::WirePath::of(std::path::Path::new("/ws")),
        workweave: Some("proj--feat-a".into()),
        workweave_unregistered: false,
        project: Some(repoweave::manifest::ProjectName::new("proj").unwrap()),
    }
}

fn primary_resolution() -> Resolution {
    Resolution {
        workspace: repoweave::path_spelling::WirePath::of(std::path::Path::new("/ws")),
        workweave: None,
        workweave_unregistered: false,
        project: Some(repoweave::manifest::ProjectName::new("proj").unwrap()),
    }
}

/// The third checkout state: in a workweave, with no registry entry naming
/// it. Distinct from both above, and the one a present-or-absent `workweave`
/// cannot carry.
fn unregistered_resolution() -> Resolution {
    Resolution {
        workspace: repoweave::path_spelling::WirePath::of(std::path::Path::new("/ws")),
        workweave: None,
        workweave_unregistered: true,
        project: Some(repoweave::manifest::ProjectName::new("proj").unwrap()),
    }
}

fn advance() -> Step3AdvanceOutput {
    Step3AdvanceOutput {
        from_sha: sha('a'),
        to_sha: sha('b'),
    }
}

/// A checkout in this workspace and one in another, as land places both.
fn placements() -> Vec<PlacementOutput> {
    vec![
        PlacementOutput {
            path: REPO.to_owned(),
            absolute_path: wire(ABS),
            files: FilePlacement {
                reflinked: 4,
                hardlinked: 2,
                written: 1,
            },
        },
        PlacementOutput {
            path: REPO.to_owned(),
            absolute_path: repoweave::path_spelling::WirePath::of(std::path::Path::new(
                "/target/github/acme/repo",
            )),
            files: FilePlacement {
                reflinked: 0,
                hardlinked: 0,
                written: 3,
            },
        },
    ]
}

fn advisories() -> Vec<AdvisoryOutput> {
    vec![AdvisoryOutput {
        kind: AdvisoryKindOutput::DerivedStateStale,
        remedy: "rwv materialize".to_owned(),
        inputs: vec!["projects/proj/rwv.lock".to_owned()],
    }]
}

fn value(envelope: impl serde::Serialize) -> Value {
    serde_json::to_value(envelope).expect("envelope serializes")
}

// ---------------------------------------------------------------------------
// backup
// ---------------------------------------------------------------------------

fn held(byte: char, subject: &str) -> HeldCommitOutput {
    let id = sha(byte);
    HeldCommitOutput {
        sha: repoweave::vcs::CommitId::parse(&id).expect("a full commit id"),
        short: id[..12].to_owned(),
        subject: subject.to_owned(),
    }
}

fn backup(name: &str, byte: char, mark: Option<BackupMark>, redundant: bool) -> BackupOutput {
    BackupOutput {
        name: name.to_owned(),
        tip: repoweave::vcs::CommitId::parse(&sha(byte)).expect("a full commit id"),
        mark,
        redundant,
        held: if redundant {
            Vec::new()
        } else {
            vec![held(byte, "work only this backup holds")]
        },
    }
}

fn backup_corpus() -> Vec<(&'static str, Value)> {
    vec![
        (
            "every-mark-and-none",
            value(BackupJsonOutput {
                schema_url: BACKUP_SCHEMA_URL.to_owned(),
                repos: vec![
                    RepoBackupsOutput {
                        path: REPO.to_owned(),
                        backups: vec![
                            backup("proj--anon/aaaaaaaaaaaa", 'a', None, false),
                            backup(
                                "proj--anon/rebased-bbbbbbbbbbbb",
                                'b',
                                Some(BackupMark::Rebased),
                                false,
                            ),
                            backup("HEAD/cccccccccccc", 'c', None, false),
                            backup(
                                "origin/main/superseded-ffffffffffff",
                                'f',
                                Some(BackupMark::Superseded),
                                false,
                            ),
                            backup("proj--old/dddddddddddd", 'd', None, true),
                            backup("proj--anon/aborted", '1', Some(BackupMark::Aborted), false),
                            backup(
                                "proj--anon/aborted-working-copy",
                                '2',
                                Some(BackupMark::AbortedWorkingCopy),
                                false,
                            ),
                        ],
                    },
                    RepoBackupsOutput {
                        path: "projects/web-app".to_owned(),
                        backups: vec![backup("main/eeeeeeeeeeee", 'e', None, false)],
                    },
                ],
            }),
        ),
        (
            "none",
            value(BackupJsonOutput {
                schema_url: BACKUP_SCHEMA_URL.to_owned(),
                repos: Vec::new(),
            }),
        ),
    ]
}

// ---------------------------------------------------------------------------
// branch
// ---------------------------------------------------------------------------

fn listed(name: &str, record: RecordOutput, published: Option<bool>) -> ListedBranch {
    ListedBranch {
        name: name.to_owned(),
        checked_out: name == "main",
        local: name != "remote-only",
        record,
        published,
        target_gone: false,
    }
}

fn branch_corpus() -> Vec<(&'static str, Value)> {
    vec![
        (
            "every-record-state",
            value(BranchListJsonOutput {
                schema_url: BRANCH_SCHEMA_URL.to_owned(),
                branches: vec![
                    listed("main", RecordOutput::Declared, Some(true)),
                    listed(
                        "feat-x",
                        RecordOutput::Feature {
                            target_branch: "main".to_owned(),
                            as_mode: Mode::Declared,
                        },
                        Some(false),
                    ),
                    listed(
                        "stacked",
                        RecordOutput::Feature {
                            target_branch: "feat-x".to_owned(),
                            as_mode: Mode::Feature,
                        },
                        Some(true),
                    ),
                    listed("raw", RecordOutput::Unrecorded, None),
                    listed(
                        "clash",
                        RecordOutput::Conflict {
                            values: vec![
                                ValueOutput {
                                    mode: Mode::Declared,
                                    target_branch: None,
                                    as_mode: None,
                                },
                                ValueOutput {
                                    mode: Mode::Feature,
                                    target_branch: Some("main".to_owned()),
                                    as_mode: Some(Mode::Declared),
                                },
                            ],
                        },
                        Some(false),
                    ),
                    listed(
                        "odd",
                        RecordOutput::Unreadable {
                            reason: "unknown field `later`".to_owned(),
                        },
                        Some(true),
                    ),
                    listed(
                        "loop",
                        RecordOutput::Cycle {
                            chain: vec!["loop".to_owned(), "loop".to_owned()],
                        },
                        Some(false),
                    ),
                    ListedBranch {
                        target_gone: true,
                        ..listed(
                            "remote-only",
                            RecordOutput::Feature {
                                target_branch: "deleted".to_owned(),
                                as_mode: Mode::Declared,
                            },
                            Some(true),
                        )
                    },
                ],
            }),
        ),
        (
            "none",
            value(BranchListJsonOutput {
                schema_url: BRANCH_SCHEMA_URL.to_owned(),
                branches: Vec::new(),
            }),
        ),
    ]
}

// ---------------------------------------------------------------------------
// doctor
// ---------------------------------------------------------------------------

/// Wire values `tests/common/doctor_corpus.rs` does not reach: it fixes every
/// sample's `severity` at `warning`, samples the only violation carrying an
/// `OpVerb` at `land`, and holds one sample per finding, so a finding's
/// closed-vocabulary field takes a single value there. Nothing else produces
/// these, so they are supplied here rather than left unvalidated.
fn doctor_corpus_supplement() -> (Vec<CheckViolation>, Vec<Issue>) {
    let mut violations: Vec<CheckViolation> = [
        OpVerb::Sync,
        OpVerb::Pull,
        OpVerb::UpdateRemote,
        OpVerb::CherryPick,
    ]
    .into_iter()
    .map(|verb| CheckViolation::StaleOpState {
        workspace_dir: common::doctor_corpus::path("/ws"),
        verb,
        started_at: "2026-01-01T00:00:00Z".to_owned(),
    })
    .collect();
    violations.push(CheckViolation::Provenance {
        project: common::doctor_corpus::project(),
        repo: common::doctor_corpus::repo(),
        sub_kind: ProvenanceKind::PlacementDisagreement {
            expected_path: "github/other/repo".into(),
            converge: PlacementConvergence::FromPrimary,
        },
    });
    let issues = vec![Issue {
        source: IssueSource::integration("go-work"),
        severity: Severity::Error,
        message: "member-incompatibility: go.work sets `go` to `1.21`"
            .to_owned()
            .into(),
        kind: IssueKind::MemberIncompatibility(Box::new(MemberIncompatibility::new(
            &GoWork,
            &common::doctor_corpus::path("/ws/projects/proj/go.work"),
            Observed {
                key: "go",
                on_disk: "1.21",
                required: "1.23",
                required_by: "github/acme/repo/go.mod",
            },
        ))),
        safe_to_fix: false,
    }];
    (violations, issues)
}

fn doctor_corpus() -> Vec<(&'static str, Value)> {
    let mut workweave_dirs = HashMap::new();
    workweave_dirs.insert(
        common::doctor_corpus::workweave(),
        common::doctor_corpus::path("/ws/.workweaves/proj--feat-a"),
    );
    let (extra_violations, extra_issues) = doctor_corpus_supplement();
    let mut violations = common::doctor_corpus::corpus();
    violations.extend(extra_violations);
    let mut issues = common::doctor_corpus::issue_corpus();
    issues.extend(extra_issues);
    let populated = serde_json::to_value(common::doctor_corpus::wire_document(
        violations,
        issues,
        &common::doctor_corpus::path("/ws"),
        &workweave_dirs,
        Some(resolution()),
        Vec::new(),
        advisories(),
    ))
    .expect("doctor payload serializes");
    let empty = serde_json::to_value(common::doctor_corpus::wire_document(
        Vec::new(),
        Vec::new(),
        &common::doctor_corpus::path("/ws"),
        &HashMap::new(),
        None,
        Vec::new(),
        Vec::new(),
    ))
    .expect("doctor payload serializes");
    vec![("every-violation-and-issue", populated), ("clean", empty)]
}

// ---------------------------------------------------------------------------
// fetch
// ---------------------------------------------------------------------------

/// Every [`MemberOutcomeStatus`], and `message` at both settings. The match is
/// exhaustive so a new status stops this file compiling.
fn member_outcomes() -> Vec<MemberOutcomeOutput> {
    [
        MemberOutcomeStatus::Ok,
        MemberOutcomeStatus::Skipped,
        MemberOutcomeStatus::Failed,
    ]
    .into_iter()
    .map(|status| {
        let message = match status {
            MemberOutcomeStatus::Ok => None,
            MemberOutcomeStatus::Skipped => Some("role: reference".to_owned()),
            MemberOutcomeStatus::Failed => Some("remote unreachable".to_owned()),
        };
        MemberOutcomeOutput {
            path: REPO.to_owned(),
            absolute_path: wire(ABS),
            refusal: (status == MemberOutcomeStatus::Failed)
                .then(|| "unresolvable-repo-source".to_owned()),
            moved_from: (status == MemberOutcomeStatus::Ok).then(|| "wip".to_owned()),
            facts: if status == MemberOutcomeStatus::Ok {
                vec![FactOutput {
                    fact: FactKind::Checkout,
                    about: Some("main is checked out in /primary".to_owned()),
                }]
            } else {
                Vec::new()
            },
            status,
            message,
        }
    })
    .collect()
}

/// One record of every shape a skipped side effect takes.
fn every_skip() -> Vec<SkippedEffect> {
    let mut skips = vec![
        SkippedEffect::install_hooks(SkipReason::NoMaterialize),
        SkippedEffect::install_hooks(SkipReason::InsideWorkweave),
        SkippedEffect::tool_missing(&NpmWorkspaces, MissingTool::of::<NpmWorkspaces>()),
        SkippedEffect::activation_while_active(
            &repoweave::manifest::ProjectName::new("other").unwrap(),
            &repoweave::manifest::ProjectName::new("app").unwrap(),
        ),
        SkippedEffect::hooks_while_active(&repoweave::manifest::ProjectName::new("app").unwrap()),
    ];
    skips.extend(SkippedEffect::not_presented(
        &repoweave::manifest::ProjectName::new("other").unwrap(),
    ));
    skips.extend(SkippedEffect::narrowed());
    skips
}

/// The two states a clone lists a member in, and one judged line on every
/// axis, since `left` carries the judged-line shape whole.
fn left_members() -> Vec<JudgedOutput> {
    let mut left = vec![
        judged(Axis::Pin, "none", "rwv update --remote --repo x"),
        judged(Axis::Pin, "unreachable", "git remote add"),
    ];
    left.extend(judged_on_every_axis());
    left
}

fn clone_corpus() -> Vec<(&'static str, Value)> {
    vec![
        (
            "every-status-and-every-pin-state",
            value(CloneJsonOutput {
                schema: CLONE_SCHEMA_URL.to_owned(),
                outcomes: member_outcomes(),
                left: left_members(),
                placements: placements(),
                resolution: Some(resolution()),
                skipped: every_skip(),
            }),
        ),
        (
            "unresolved-workspace",
            value(CloneJsonOutput {
                schema: CLONE_SCHEMA_URL.to_owned(),
                outcomes: Vec::new(),
                left: Vec::new(),
                placements: Vec::new(),
                resolution: None,
                skipped: Vec::new(),
            }),
        ),
    ]
}

fn update_corpus() -> Vec<(&'static str, Value)> {
    vec![
        (
            "every-status",
            value(UpdateJsonOutput {
                schema: UPDATE_SCHEMA_URL.to_owned(),
                outcomes: member_outcomes(),
                judged: left_members(),
                placements: placements(),
                resolution: Some(resolution()),
                skipped: every_skip(),
            }),
        ),
        (
            "unresolved-workspace",
            value(UpdateJsonOutput {
                schema: UPDATE_SCHEMA_URL.to_owned(),
                outcomes: Vec::new(),
                judged: Vec::new(),
                placements: Vec::new(),
                resolution: None,
                skipped: Vec::new(),
            }),
        ),
    ]
}

/// One record of every status a fetched repo takes, and one move of every
/// kind. The matches are exhaustive so a new status or kind stops this file
/// compiling.
fn fetched_repos() -> Vec<FetchedRepoOutput> {
    let moves: Vec<BranchMoveOutput> = [
        BranchMoveKind::Created,
        BranchMoveKind::Advanced,
        BranchMoveKind::Rewritten,
        BranchMoveKind::Pruned,
    ]
    .into_iter()
    .map(|kind| {
        let (from, to) = match kind {
            BranchMoveKind::Created => (None, Some(sha('b'))),
            BranchMoveKind::Advanced | BranchMoveKind::Rewritten => {
                (Some(sha('a')), Some(sha('b')))
            }
            BranchMoveKind::Pruned => (Some(sha('a')), None),
        };
        BranchMoveOutput {
            branch: "origin/main".to_owned(),
            kind,
            from,
            to,
        }
    })
    .collect();
    [
        FetchStatus::Fetched,
        FetchStatus::Unconfirmed,
        FetchStatus::Skipped,
    ]
    .into_iter()
    .map(|status| {
        let (moved, message, refusal) = match status {
            FetchStatus::Fetched => (moves.clone(), None, None),
            FetchStatus::Unconfirmed => (
                Vec::new(),
                Some("remote unreachable".to_owned()),
                Some("state-claim-held".to_owned()),
            ),
            FetchStatus::Skipped => (Vec::new(), Some("not on disk".to_owned()), None),
        };
        FetchedRepoOutput {
            path: REPO.to_owned(),
            absolute_path: wire(ABS),
            status,
            moved,
            message,
            refusal,
            pins_off_remote: match status {
                FetchStatus::Fetched => {
                    let mut lines = vec![judged(Axis::Pin, "off remote", "rwv push --repo x")];
                    lines.extend(judged_on_every_axis());
                    lines
                }
                FetchStatus::Unconfirmed | FetchStatus::Skipped => Vec::new(),
            },
        }
    })
    .collect()
}

fn fetch_corpus() -> Vec<(&'static str, Value)> {
    vec![
        (
            "every-status-and-move",
            value(FetchJsonOutput {
                schema: FETCH_SCHEMA_URL.to_owned(),
                repos: fetched_repos(),
                resolution: Some(resolution()),
            }),
        ),
        (
            "unresolved-workspace",
            value(FetchJsonOutput {
                schema: FETCH_SCHEMA_URL.to_owned(),
                repos: Vec::new(),
                resolution: None,
            }),
        ),
    ]
}

/// One take-in of every result, the changes at both settings of their lists.
fn project_take_ins() -> Vec<ProjectTakeInOutput> {
    [
        TakeIn::Advanced,
        TakeIn::Even,
        TakeIn::Ahead,
        TakeIn::NoUpstream,
        TakeIn::Detached,
        TakeIn::Merged,
        TakeIn::Reset,
    ]
    .into_iter()
    .map(|result| {
        let advanced = result == TakeIn::Advanced;
        ProjectTakeInOutput {
            path: "projects/web".to_owned(),
            branch: (result != TakeIn::Detached).then(|| "main".to_owned()),
            result,
            from: advanced.then(|| sha('a')),
            to: (advanced || result == TakeIn::Merged || result == TakeIn::Reset).then(|| sha('b')),
            backup: (result == TakeIn::Reset).then(|| "main/1".to_owned()),
            re_pinned: if advanced {
                vec![RePinnedOutput {
                    path: REPO.to_owned(),
                    from: sha('a'),
                    to: sha('b'),
                }]
            } else {
                Vec::new()
            },
            member_changes: if advanced {
                [
                    (EntryChange::Added, Vec::new()),
                    (EntryChange::Removed, Vec::new()),
                    (EntryChange::Changed, vec!["branch: main -> dev".to_owned()]),
                ]
                .into_iter()
                .map(|(change, fields)| MemberChangeOutput {
                    path: REPO.to_owned(),
                    change,
                    fields,
                })
                .collect()
            } else {
                Vec::new()
            },
        }
    })
    .collect()
}

fn pull_corpus() -> Vec<(&'static str, Value)> {
    project_take_ins()
        .into_iter()
        .enumerate()
        .map(|(i, project)| {
            let name: &'static str = [
                "advanced",
                "even",
                "ahead",
                "no-upstream",
                "detached",
                "merged",
                "reset",
            ][i];
            let full = i == 0;
            (
                name,
                value(PullJsonOutput {
                    schema: PULL_SCHEMA_URL.to_owned(),
                    fetched: if full { fetched_repos() } else { Vec::new() },
                    project,
                    outcomes: if full { member_outcomes() } else { Vec::new() },
                    judged: if full { left_members() } else { Vec::new() },
                    placements: if full { placements() } else { Vec::new() },
                    resolution: full.then(resolution),
                    skipped: if full { every_skip() } else { Vec::new() },
                }),
            )
        })
        .collect()
}

// ---------------------------------------------------------------------------
// push
// ---------------------------------------------------------------------------

fn push_outcomes() -> Vec<PushOutcomeOutput> {
    vec![
        PushOutcomeOutput::Pushed {
            path: REPO.to_owned(),
            absolute_path: wire(ABS),
        },
        PushOutcomeOutput::Skipped {
            path: REPO.to_owned(),
            absolute_path: wire(ABS),
        },
        PushOutcomeOutput::Failed {
            path: REPO.to_owned(),
            absolute_path: wire(ABS),
            message: "non-fast-forward".to_owned(),
        },
        PushOutcomeOutput::RecordsPublished {
            path: "projects/proj".to_owned(),
            absolute_path: repoweave::path_spelling::WirePath::of(std::path::Path::new(
                "/ws/projects/proj",
            )),
            project: repoweave::manifest::ProjectName::new("proj").unwrap(),
            dropped: vec!["feat-y".to_owned()],
        },
        PushOutcomeOutput::RecordsRejected {
            path: "projects/proj".to_owned(),
            absolute_path: repoweave::path_spelling::WirePath::of(std::path::Path::new(
                "/ws/projects/proj",
            )),
            project: repoweave::manifest::ProjectName::new("proj").unwrap(),
            message: "this remote refuses refs/heads/rwv-records".to_owned(),
        },
        PushOutcomeOutput::ProjectRepoPushed {
            path: "projects/proj".to_owned(),
            absolute_path: repoweave::path_spelling::WirePath::of(std::path::Path::new(
                "/ws/projects/proj",
            )),
            project: repoweave::manifest::ProjectName::new("proj").unwrap(),
        },
        PushOutcomeOutput::ProjectRepoFailed {
            path: "projects/proj".to_owned(),
            absolute_path: repoweave::path_spelling::WirePath::of(std::path::Path::new(
                "/ws/projects/proj",
            )),
            project: repoweave::manifest::ProjectName::new("proj").unwrap(),
            message: "remote rejected".to_owned(),
        },
    ]
}

fn push_corpus() -> Vec<(&'static str, Value)> {
    vec![
        (
            "every-outcome-kind",
            value(PushJsonOutput {
                schema_url: PUSH_SCHEMA_URL.to_owned(),
                outcomes: push_outcomes(),
                refused: judged_on_every_axis(),
                reported: judged_on_every_axis(),
                resolution: Some(unregistered_resolution()),
            }),
        ),
        (
            "unresolved-workspace",
            value(PushJsonOutput {
                schema_url: PUSH_SCHEMA_URL.to_owned(),
                outcomes: Vec::new(),
                refused: Vec::new(),
                reported: Vec::new(),
                resolution: None,
            }),
        ),
    ]
}

// ---------------------------------------------------------------------------
// log
// ---------------------------------------------------------------------------

fn log_commit(byte: char, subject: &str) -> LogCommit {
    let sha = repoweave::vcs::CommitId::parse(&sha(byte)).unwrap();
    LogCommit {
        short: sha.short().to_owned(),
        sha,
        subject: subject.to_owned(),
    }
}

fn log_repo(path: &str) -> LogRepo {
    LogRepo {
        path: serde_json::from_value(json!(path)).unwrap(),
        head: Some(repoweave::vcs::CommitId::parse(&sha('a')).unwrap()),
        pin: None,
        endpoints: Vec::new(),
        commits: Vec::new(),
        diff_base: None,
        diff: None,
        note: None,
        refused: Vec::new(),
    }
}

fn log_corpus() -> Vec<(&'static str, Value)> {
    let pin = repoweave::vcs::CommitId::parse(&sha('b')).unwrap();
    let ranged = LogRepo {
        endpoints: vec![
            LogEndpoint {
                spelled: "main".to_owned(),
                id: repoweave::vcs::CommitId::parse(&sha('b')).unwrap(),
            },
            LogEndpoint {
                spelled: "feat-x".to_owned(),
                id: repoweave::vcs::CommitId::parse(&sha('a')).unwrap(),
            },
        ],
        commits: vec![log_commit('a', "lib: feat work")],
        ..log_repo(REPO)
    };
    let missing_an_endpoint = LogRepo {
        head: None,
        refused: judged_on_every_axis(),
        ..log_repo("github/acme/other")
    };
    let pinned = LogRepo {
        pin: Some(pin.clone()),
        commits: vec![log_commit('a', "lib: feat work"), log_commit('c', "second")],
        ..log_repo(REPO)
    };
    let not_pinned = LogRepo {
        note: Some("not in main".to_owned()),
        ..log_repo("github/acme/added")
    };
    let diffed = LogRepo {
        pin: Some(pin.clone()),
        diff_base: Some(pin),
        diff: Some("diff --git a/f b/f\n".to_owned()),
        ..log_repo(REPO)
    };
    let project = || log_repo("(project)");
    vec![
        (
            "a-range-one-repo-lacks-an-endpoint-of",
            value(LogJsonOutput {
                schema_url: LOG_SCHEMA_URL.to_owned(),
                range: Some("main..feat-x".to_owned()),
                against: None,
                diff: false,
                repos: vec![ranged, missing_an_endpoint, project()],
            }),
        ),
        (
            "against-with-a-member-the-branch-does-not-pin",
            value(LogJsonOutput {
                schema_url: LOG_SCHEMA_URL.to_owned(),
                range: None,
                against: Some("main".to_owned()),
                diff: false,
                repos: vec![pinned, not_pinned, project()],
            }),
        ),
        (
            "against-with-a-diff",
            value(LogJsonOutput {
                schema_url: LOG_SCHEMA_URL.to_owned(),
                range: None,
                against: Some("main".to_owned()),
                diff: true,
                repos: vec![diffed, project()],
            }),
        ),
        (
            "bare",
            value(LogJsonOutput {
                schema_url: LOG_SCHEMA_URL.to_owned(),
                range: None,
                against: None,
                diff: false,
                repos: vec![LogRepo {
                    commits: vec![log_commit('a', "initial")],
                    ..log_repo(REPO)
                }],
            }),
        ),
    ]
}

// ---------------------------------------------------------------------------
// status
// ---------------------------------------------------------------------------

fn every_relation() -> Vec<LockRelation> {
    vec![
        LockRelation::Ok,
        LockRelation::Ahead,
        LockRelation::Behind,
        LockRelation::Diverged,
        LockRelation::NoLock,
        LockRelation::Unknown,
        LockRelation::Missing,
        LockRelation::Unreachable,
    ]
}

fn status_repos() -> Vec<RepoStatus> {
    every_relation()
        .into_iter()
        .enumerate()
        .map(|(i, relation)| {
            // Alternate the optional fields so neither the present nor the
            // absent wire shape goes unsampled.
            let sparse = i % 2 == 1;
            RepoStatus {
                path: REPO.to_owned(),
                branch: (!sparse).then(|| "main".to_owned()),
                tip: (!sparse).then(|| sha('a')),
                lock_sha: (!sparse).then(|| sha('b')),
                lock_pinned_by_name: sparse,
                relation,
                mid_op: MidOperation::all().nth(i),
                role: repoweave::manifest::Role::ALL[i % repoweave::manifest::Role::ALL.len()],
                url: "https://github.com/acme/repo.git".to_owned(),
                project: repoweave::manifest::ProjectName::new("proj").unwrap(),
                absolute_path: wire(ABS),
                parent: (!sparse).then(|| ParentInfo {
                    path: repoweave::path_spelling::WirePath::of(std::path::Path::new("/ws")),
                    tip: Some(repoweave::vcs::CommitId::parse(&sha('c')).unwrap()),
                }),
            }
        })
        .collect()
}

fn op_status(verb: OpVerb, phase: OpPhase) -> OpStatus {
    OpStatus {
        id: "op-1".to_owned(),
        verb,
        phase,
        started_at: "2026-01-01T00:00:00Z".to_owned(),
        owner: wire("/ws"),
        source: wire("/ws/.workweaves/proj--feat-a"),
        target: Some(wire("/ws")),
    }
}

fn status_records() -> Vec<repoweave::branch::BranchRecordStatus> {
    let project = repoweave::manifest::ProjectName::new("app").unwrap();
    let record = |branch: &str, record: RecordOutput| repoweave::branch::BranchRecordStatus {
        project: project.clone(),
        branch: branch.to_owned(),
        record,
        dependents: Vec::new(),
    };
    vec![
        repoweave::branch::BranchRecordStatus {
            dependents: vec!["feat-x".to_owned(), "feat-y".to_owned()],
            ..record("main", RecordOutput::Declared)
        },
        record(
            "feat-x",
            RecordOutput::Feature {
                target_branch: "main".to_owned(),
                as_mode: Mode::Declared,
            },
        ),
        record("raw", RecordOutput::Unrecorded),
        record(
            "clash",
            RecordOutput::Conflict {
                values: vec![
                    ValueOutput {
                        mode: Mode::Declared,
                        target_branch: None,
                        as_mode: None,
                    },
                    ValueOutput {
                        mode: Mode::Feature,
                        target_branch: Some("main".to_owned()),
                        as_mode: Some(Mode::Feature),
                    },
                ],
            },
        ),
        record(
            "odd",
            RecordOutput::Unreadable {
                reason: "unknown field `later`".to_owned(),
            },
        ),
        record(
            "loop",
            RecordOutput::Cycle {
                chain: vec!["loop".to_owned(), "loop".to_owned()],
            },
        ),
    ]
}

fn status_aligned() -> Vec<repoweave::status::AlignedStatus> {
    let project = repoweave::manifest::ProjectName::new("app").unwrap();
    vec![
        repoweave::status::AlignedStatus {
            project: project.clone(),
            branch: Some("main".to_owned()),
            detached: false,
            unreadable: None,
        },
        repoweave::status::AlignedStatus {
            project: project.clone(),
            branch: None,
            detached: true,
            unreadable: None,
        },
        repoweave::status::AlignedStatus {
            project,
            branch: None,
            detached: false,
            unreadable: Some("it does not parse: EOF while parsing".to_owned()),
        },
    ]
}

fn status_operator_branches() -> Vec<repoweave::status::OperatorBranch> {
    vec![repoweave::status::OperatorBranch {
        path: REPO.to_owned(),
        branch: "wip".to_owned(),
        unpushed: 1,
        commits: vec![repoweave::log::LogCommit {
            sha: common::doctor_corpus::commit(),
            short: "abcdef0".to_owned(),
            subject: "wip".to_owned(),
        }],
        gone_upstream: true,
    }]
}

fn status_backups() -> Vec<repoweave::backup::RepoBackupsOutput> {
    let backup = |name: &str, mark| repoweave::backup::BackupOutput {
        name: name.to_owned(),
        tip: common::doctor_corpus::commit(),
        mark,
        redundant: false,
        held: vec![repoweave::backup::HeldCommitOutput {
            sha: common::doctor_corpus::commit(),
            short: "abcdef0".to_owned(),
            subject: "kept".to_owned(),
        }],
    };
    vec![repoweave::backup::RepoBackupsOutput {
        path: REPO.to_owned(),
        backups: vec![
            backup("main/1", None),
            backup("main/2", Some(repoweave::vcs::BackupMark::Rebased)),
            backup("main/3", None),
            backup(
                "origin/main/superseded-1",
                Some(repoweave::vcs::BackupMark::Superseded),
            ),
            backup("main/aborted", Some(repoweave::vcs::BackupMark::Aborted)),
            backup(
                "main/aborted-working-copy",
                Some(repoweave::vcs::BackupMark::AbortedWorkingCopy),
            ),
        ],
    }]
}

fn status_corpus() -> Vec<(&'static str, Value)> {
    let mut docs = vec![
        (
            "every-relation-no-op",
            value(StatusJsonOutput {
                schema_url: STATUS_SCHEMA_URL.to_owned(),
                repos: status_repos(),
                unreadable_projects: Vec::new(),
                records: status_records(),
                aligned: status_aligned(),
                judged: Vec::new(),
                operator_branches: status_operator_branches(),
                backups: status_backups(),
                resolution: Some(resolution()),
                op: None,
                unreadable_op: None,
            }),
        ),
        (
            "judged-on-every-axis",
            value(StatusJsonOutput {
                schema_url: STATUS_SCHEMA_URL.to_owned(),
                repos: status_repos(),
                unreadable_projects: Vec::new(),
                records: Vec::new(),
                aligned: Vec::new(),
                judged: judged_on_every_axis(),
                operator_branches: Vec::new(),
                backups: Vec::new(),
                resolution: Some(resolution()),
                op: None,
                unreadable_op: None,
            }),
        ),
        (
            "unresolved-workspace",
            value(StatusJsonOutput {
                schema_url: STATUS_SCHEMA_URL.to_owned(),
                repos: Vec::new(),
                unreadable_projects: Vec::new(),
                records: Vec::new(),
                aligned: Vec::new(),
                judged: Vec::new(),
                operator_branches: Vec::new(),
                backups: Vec::new(),
                resolution: None,
                op: None,
                unreadable_op: None,
            }),
        ),
        (
            "unreadable-project",
            value(StatusJsonOutput {
                schema_url: STATUS_SCHEMA_URL.to_owned(),
                repos: Vec::new(),
                unreadable_projects: vec![repoweave::status::UnreadableProject {
                    project: repoweave::manifest::ProjectName::new("app").unwrap(),
                    error: "failed to parse rwv.toml".to_owned(),
                }],
                records: Vec::new(),
                aligned: Vec::new(),
                judged: Vec::new(),
                operator_branches: Vec::new(),
                backups: Vec::new(),
                resolution: Some(resolution()),
                op: None,
                unreadable_op: Some("the operation record at /w/.rwv-op cannot be read".into()),
            }),
        ),
    ];
    // Every phase, and both verbs, since `op` is the one place those
    // vocabularies reach `rwv status --json`.
    for (label, verb, phase) in [
        ("op-sync-replay", OpVerb::Sync, OpPhase::Replay),
        ("op-sync-relock", OpVerb::Sync, OpPhase::Relock),
        (
            "op-land-advance-target",
            OpVerb::Land,
            OpPhase::AdvanceTarget,
        ),
        ("op-land-retire", OpVerb::Land, OpPhase::Retire),
        ("op-pull-replay", OpVerb::Pull, OpPhase::Replay),
        ("op-cherry-pick-replay", OpVerb::CherryPick, OpPhase::Replay),
        (
            "op-update-remote-relock",
            OpVerb::UpdateRemote,
            OpPhase::Relock,
        ),
    ] {
        docs.push((
            label,
            value(StatusJsonOutput {
                schema_url: STATUS_SCHEMA_URL.to_owned(),
                repos: Vec::new(),
                unreadable_projects: Vec::new(),
                records: Vec::new(),
                aligned: Vec::new(),
                judged: Vec::new(),
                operator_branches: Vec::new(),
                backups: Vec::new(),
                resolution: Some(primary_resolution()),
                op: Some(op_status(verb, phase)),
                unreadable_op: None,
            }),
        ));
    }
    docs
}

// ---------------------------------------------------------------------------
// sync and land
// ---------------------------------------------------------------------------

/// Every [`VcsErrorOutput`] variant, which is what a sync failure's `cause`
/// carries. Written as a list so a variant added to the enum leaves the list
/// short, and as an exhaustive match in [`vcs_error_tag`] so the same addition
/// stops this file compiling.
fn every_vcs_error() -> Vec<VcsErrorOutput> {
    let repo = wire(ABS);
    let mut errors = vec![
        VcsErrorOutput::NotARepo { path: repo.clone() },
        VcsErrorOutput::RevisionNotFound {
            repo: repo.clone(),
            rev: sha('a'),
        },
        VcsErrorOutput::RebaseConflict {
            repo: repo.clone(),
            op: ConflictOp::Rebase,
        },
        VcsErrorOutput::RebaseConflict {
            repo: repo.clone(),
            op: ConflictOp::Merge,
        },
        VcsErrorOutput::RebaseConflict {
            repo: repo.clone(),
            op: ConflictOp::CherryPick,
        },
        VcsErrorOutput::RebaseConflict {
            repo: repo.clone(),
            op: ConflictOp::Revert,
        },
        VcsErrorOutput::StaleRefWitness {
            repo: repo.clone(),
            expected: sha('a'),
            observed: sha('b'),
        },
        VcsErrorOutput::HookRejected {
            repo: repo.clone(),
            stderr: "pre-push refused".to_owned(),
        },
        VcsErrorOutput::UntrackedCollision {
            repo: repo.clone(),
            paths: vec!["src/main.rs".to_owned()],
        },
        VcsErrorOutput::CheckoutUnrecorded {
            repo: repo.clone(),
            recording_off: false,
        },
        VcsErrorOutput::BranchRewritten {
            repo: repo.clone(),
            stood_at: sha('a'),
            head: sha('b'),
        },
        VcsErrorOutput::MoveTakenInPart {
            repo: repo.clone(),
            paths: vec!["src/main.rs".to_owned()],
        },
        VcsErrorOutput::Io {
            ctx: "read HEAD".to_owned(),
            message: "permission denied".to_owned(),
        },
        VcsErrorOutput::CommandFailed {
            args: vec!["rev-parse".to_owned(), "HEAD".to_owned()],
            repo: repo.clone(),
            stderr: "fatal".to_owned(),
        },
    ];
    errors.extend(
        MidOperation::all().map(|operation| VcsErrorOutput::MidOperation {
            repo: repo.clone(),
            operation,
        }),
    );
    errors
}

/// Exhaustive on purpose: a new `VcsErrorOutput` variant stops this file
/// compiling until whoever added it also adds a sample above.
fn vcs_error_tag(e: &VcsErrorOutput) -> &'static str {
    match e {
        VcsErrorOutput::NotARepo { .. } => "not-a-repo",
        VcsErrorOutput::RevisionNotFound { .. } => "revision-not-found",
        VcsErrorOutput::RebaseConflict { .. } => "rebase-conflict",
        VcsErrorOutput::StaleRefWitness { .. } => "stale-ref-witness",
        VcsErrorOutput::MidOperation { .. } => "mid-operation",
        VcsErrorOutput::HookRejected { .. } => "hook-rejected",
        VcsErrorOutput::UntrackedCollision { .. } => "untracked-collision",
        VcsErrorOutput::CheckoutUnrecorded { .. } => "checkout-unrecorded",
        VcsErrorOutput::BranchRewritten { .. } => "branch-rewritten",
        VcsErrorOutput::MoveTakenInPart { .. } => "move-taken-in-part",
        VcsErrorOutput::Io { .. } => "io",
        VcsErrorOutput::CommandFailed { .. } => "command-failed",
    }
}

/// Every [`SyncFailureOutput`] shape, `head-unreadable` both with and without
/// its optional `cause`, and every `cause` the wire can carry.
fn sync_failures() -> Vec<SyncFailureOutput> {
    let mut failures = vec![SyncFailureOutput::HeadUnreadable {
        message: "HEAD is unreadable".to_owned(),
        cause: None,
    }];
    for (n, cause) in every_vcs_error().into_iter().enumerate() {
        let message = format!("failed: {}", vcs_error_tag(&cause));
        failures.push(match n % 3 {
            0 => SyncFailureOutput::HeadUnreadable {
                message,
                cause: Some(cause),
            },
            1 => SyncFailureOutput::MergeFailed { message, cause },
            _ => SyncFailureOutput::RebaseFailed { message, cause },
        });
    }
    failures
}

/// Every [`SyncOutcomeOutput`] variant. `step3_advance` is sampled at both
/// settings on every variant that carries it, because `sync` omits it and
/// `land` supplies it — one envelope's normal shape is the other's.
/// Every containment verdict, each paired with a baseline, so both closed
/// vocabularies are sampled across one corpus document. Absence is sampled
/// separately by the outcomes that carry `None`.
fn containments() -> Vec<ContainmentOutput> {
    vec![
        ContainmentOutput {
            verdict: ContainmentVerdictOutput::Behind { commits: 2 },
            baseline: ReplayBaseline::SourceLockEntry,
        },
        ContainmentOutput {
            verdict: ContainmentVerdictOutput::Diverged {
                ahead: 1,
                behind: 4,
            },
            baseline: ReplayBaseline::SourceCommittedTip,
        },
        ContainmentOutput {
            verdict: ContainmentVerdictOutput::Ahead { commits: 3 },
            baseline: ReplayBaseline::SourceLockEntry,
        },
        ContainmentOutput {
            verdict: ContainmentVerdictOutput::Equal,
            baseline: ReplayBaseline::SourceCommittedTip,
        },
    ]
}

fn sync_outcomes(with_step3: bool) -> Vec<SyncOutcomeOutput> {
    let step3 = with_step3.then(advance);
    let c = containments();
    let mut outcomes = vec![
        SyncOutcomeOutput::Converged {
            path: REPO.to_owned(),
            absolute_path: wire(ABS),
            step3_advance: step3.clone(),
            derived_content_dropped: Vec::new(),
            containment: Some(c[0]),
        },
        SyncOutcomeOutput::Converged {
            path: REPO.to_owned(),
            absolute_path: wire(ABS),
            step3_advance: step3.clone(),
            derived_content_dropped: vec!["Cargo.lock".to_owned()],
            containment: Some(c[1]),
        },
        SyncOutcomeOutput::AlreadyAhead {
            path: REPO.to_owned(),
            absolute_path: wire(ABS),
            commits_ahead: 3,
            step3_advance: step3.clone(),
            containment: Some(c[2]),
            source_position: Some(ContainmentVerdictOutput::Ahead { commits: 5 }),
        },
        SyncOutcomeOutput::AlreadyAhead {
            path: REPO.to_owned(),
            absolute_path: wire(ABS),
            commits_ahead: 0,
            step3_advance: step3.clone(),
            containment: None,
            source_position: None,
        },
        SyncOutcomeOutput::NoOp {
            path: REPO.to_owned(),
            absolute_path: wire(ABS),
            step3_advance: step3.clone(),
            containment: Some(c[3]),
        },
    ];
    for (n, failure) in sync_failures().into_iter().enumerate() {
        outcomes.push(SyncOutcomeOutput::Failed {
            path: REPO.to_owned(),
            absolute_path: wire(ABS),
            failure,
            step3_advance: step3.clone(),
            // A failure decided before any pair was read carries no verdict,
            // and that shape has to be sampled too.
            containment: (n > 0).then(|| c[n % c.len()]),
        });
    }
    outcomes
}

/// Every member kind, and a refused prune both with and without a token.
/// One judged repo with a choice and a fact, so every optional part of the
/// record is on the wire.
fn judged(axis: Axis, state: &str, run: &str) -> JudgedOutput {
    JudgedOutput {
        path: REPO.to_owned(),
        workspace: Some(wire(ABS)),
        axis,
        state: state.to_owned(),
        subject: Some("subject".to_owned()),
        listed: vec!["listed".to_owned()],
        facts: vec![FactOutput {
            fact: FactKind::PinnedBy,
            about: Some("other".to_owned()),
        }],
        choices: vec![ChoiceOutput {
            id: Some("C7".to_owned()),
            phrase: "publish it".to_owned(),
            at: Some(wire(ABS)),
            run: vec![run.to_owned()],
        }],
    }
}

/// A judged repo on every axis, between them carrying every fact, the
/// optional parts absent on one: what any envelope's `refused` can hold.
fn judged_on_every_axis() -> Vec<JudgedOutput> {
    let facts = [
        FactKind::Rewritten,
        FactKind::Landed,
        FactKind::PinnedBy,
        FactKind::Shared,
        FactKind::PinLanded,
        FactKind::Checkout,
        FactKind::PatchEquivalent,
        FactKind::Ahead,
        FactKind::Behind,
    ];
    let mut out: Vec<JudgedOutput> = Axis::ALL
        .into_iter()
        .map(|axis| judged(axis, "a state", "rwv push"))
        .collect();
    out[0].facts = facts
        .into_iter()
        .map(|fact| FactOutput { fact, about: None })
        .collect();
    out[0].subject = None;
    out[0].workspace = None;
    out[0].choices[0].id = None;
    out[0].choices[0].at = None;
    out
}

fn refusal_corpus() -> Vec<(&'static str, Value)> {
    let mut bare = judged(Axis::Edits, "mid-operation", "git -C /ws/x status");
    bare.facts.clear();
    bare.listed.clear();
    bare.choices.clear();
    vec![
        (
            "every-axis-and-fact",
            value(RefusalJsonOutput {
                schema: REFUSAL_SCHEMA_URL.to_owned(),
                refused: judged_on_every_axis(),
            }),
        ),
        (
            "one-line-printing-no-choice",
            value(RefusalJsonOutput {
                schema: REFUSAL_SCHEMA_URL.to_owned(),
                refused: vec![bare],
            }),
        ),
    ]
}

fn members() -> Vec<MemberOutput> {
    vec![
        MemberOutput::Kept {
            path: REPO.to_owned(),
            absolute_path: wire(ABS),
        },
        MemberOutput::Pruned {
            path: REPO.to_owned(),
            absolute_path: wire(ABS),
        },
        MemberOutput::PruneRefused {
            path: REPO.to_owned(),
            absolute_path: wire(ABS),
            refusal: None,
            refused: vec![judged(Axis::Edits, "dirty", "git -C /ws/x commit")],
            message: "worktree has uncommitted changes".to_owned(),
        },
        MemberOutput::PruneRefused {
            path: REPO.to_owned(),
            absolute_path: wire(ABS),
            refusal: Some("store-claims-unreadable".to_owned()),
            refused: Vec::new(),
            message: "cannot enumerate the worktrees".to_owned(),
        },
        MemberOutput::PruneRefused {
            path: REPO.to_owned(),
            absolute_path: wire(ABS),
            refusal: None,
            refused: Vec::new(),
            message: "worktree remove failed".to_owned(),
        },
    ]
}

fn cherry_pick_corpus() -> Vec<(&'static str, Value)> {
    use repoweave::cherry_pick::{
        CarriedOutput, CherryPickJsonOutput, CopiedOutput, ExceptedOutput, MemberPickOutput,
        NotPortedOutput, CHERRY_PICK_JSON_SCHEMA_URL,
    };
    let id = |fill: char| fill.to_string().repeat(40);
    vec![
        (
            "ported",
            value(CherryPickJsonOutput {
                schema: CHERRY_PICK_JSON_SCHEMA_URL.to_owned(),
                rev: Some(id('a')),
                members: vec![MemberPickOutput {
                    path: REPO.to_owned(),
                    copied: vec![CopiedOutput {
                        from: id('b'),
                        to: id('c'),
                    }],
                    dropped: vec![id('d')],
                    left_out: vec![id('e')],
                }],
                carried: vec![CarriedOutput {
                    path: "github/acme/dep".to_owned(),
                    from: Some(id('f')),
                    to: id('1'),
                }],
                excepted: vec![ExceptedOutput {
                    path: "github/acme/other".to_owned(),
                    tip: id('2'),
                }],
                not_ported: vec![NotPortedOutput {
                    entry: REPO.to_owned(),
                    field: "branch".to_owned(),
                    parent: Some("main".to_owned()),
                    rev: Some("next".to_owned()),
                    here: Some("stable".to_owned()),
                }],
                commit: Some(id('3')),
                complete: true,
                refused: Vec::new(),
            }),
        ),
        (
            "refused",
            value(CherryPickJsonOutput {
                schema: CHERRY_PICK_JSON_SCHEMA_URL.to_owned(),
                refused: judged_on_every_axis(),
                ..CherryPickJsonOutput::default()
            }),
        ),
    ]
}

fn sync_corpus() -> Vec<(&'static str, Value)> {
    vec![
        (
            "every-outcome-with-advisory",
            value(SyncJsonOutput {
                schema: SYNC_JSON_SCHEMA_URL.to_owned(),
                outcomes: sync_outcomes(false),
                placements: placements(),
                members: members(),
                complete: true,
                refused: Vec::new(),
                advisories: advisories(),
                reports: landing_reports(),
                resolution: Some(resolution()),
            }),
        ),
        (
            "every-outcome-carrying-step3",
            value(SyncJsonOutput {
                schema: SYNC_JSON_SCHEMA_URL.to_owned(),
                outcomes: sync_outcomes(true),
                placements: Vec::new(),
                members: Vec::new(),
                complete: false,
                refused: judged_on_every_axis(),
                advisories: Vec::new(),
                reports: Vec::new(),
                resolution: None,
            }),
        ),
    ]
}

fn survivors_of_every_reason() -> Vec<Survivor> {
    [
        Reason::Unlanded,
        Reason::Pinned,
        Reason::OpenHead,
        Reason::Members,
        Reason::Unconfirmed,
    ]
    .into_iter()
    .map(|reason| Survivor {
        path: REPO.to_owned(),
        branch: "proj--feat-a".to_owned(),
        reason,
        detail: vec!["what the reason rests on".to_owned()],
        choices: Vec::new(),
    })
    .collect()
}

fn deleted_by_every_ground() -> Vec<Deleted> {
    vec![
        Deleted {
            path: REPO.to_owned(),
            branch: "proj--feat-a".to_owned(),
            site: "local".to_owned(),
            because: format!("landed on {}", sha('a')),
            backup: None,
        },
        Deleted {
            path: "projects/proj".to_owned(),
            branch: "proj--feat-a".to_owned(),
            site: "remote origin".to_owned(),
            because: "kept as backup proj--feat-a/aaaaaaaaaaaa".to_owned(),
            backup: Some("proj--feat-a/aaaaaaaaaaaa".to_owned()),
        },
    ]
}

fn branch_delete_corpus() -> Vec<(&'static str, Value)> {
    vec![
        (
            "deleted-and-survived",
            value(BranchDeleteJsonOutput {
                schema_url: BRANCH_DELETE_SCHEMA_URL.to_owned(),
                branch: "proj--feat-a".to_owned(),
                landed_on: "main".to_owned(),
                outcome: Outcome {
                    deleted: deleted_by_every_ground(),
                    survivors: survivors_of_every_reason(),
                    records_dropped: vec!["proj--feat-a".to_owned()],
                },
            }),
        ),
        (
            "nothing-to-delete",
            value(BranchDeleteJsonOutput {
                schema_url: BRANCH_DELETE_SCHEMA_URL.to_owned(),
                branch: "proj--feat-a".to_owned(),
                landed_on: "main".to_owned(),
                outcome: Outcome::default(),
            }),
        ),
    ]
}

fn workweave_corpus() -> Vec<(&'static str, Value)> {
    vec![
        (
            "everything-discarded-and-survived",
            value(WorkweaveDeleteJsonOutput {
                schema_url: WORKWEAVE_SCHEMA_URL.to_owned(),
                project: "proj".to_owned(),
                discarded: DiscardedOutput {
                    edits: vec![KeptWork {
                        path: REPO.to_owned(),
                        backup: "proj--feat-a/aaaaaaaaaaaa".to_owned(),
                    }],
                    detached_commits: vec![KeptWork {
                        path: REPO.to_owned(),
                        backup: "HEAD/bbbbbbbbbbbb".to_owned(),
                    }],
                    unlanded_branches: vec![UnlandedBranch {
                        path: REPO.to_owned(),
                        branch: "proj--feat-a".to_owned(),
                        backup: "proj--feat-a/cccccccccccc".to_owned(),
                    }],
                },
                outcome: Outcome {
                    deleted: deleted_by_every_ground(),
                    survivors: survivors_of_every_reason(),
                    records_dropped: vec!["proj--feat-a".to_owned()],
                },
            }),
        ),
        (
            "no-feature-branch",
            value(WorkweaveDeleteJsonOutput {
                schema_url: WORKWEAVE_SCHEMA_URL.to_owned(),
                project: "proj".to_owned(),
                discarded: DiscardedOutput {
                    edits: Vec::new(),
                    detached_commits: Vec::new(),
                    unlanded_branches: Vec::new(),
                },
                outcome: Outcome::default(),
            }),
        ),
    ]
}

fn left_behind() -> Vec<LeftBehindOutput> {
    [false, true]
        .map(|feature_pin| LeftBehindOutput {
            path: "local/remotes/lib".to_owned(),
            was: "main".to_owned(),
            now: "dev".to_owned(),
            feature_pin,
            commits: vec![LeftCommitOutput {
                sha: repoweave::vcs::CommitId::parse(&sha('d')).expect("a full commit id"),
                subject: "fix".to_owned(),
            }],
            choices: vec![ChoiceOutput {
                id: Some("C15".to_owned()),
                phrase: "copy these commits onto `dev`".to_owned(),
                at: None,
                run: vec!["git -C /ws/local/remotes/lib cherry-pick dddddddddddd".to_owned()],
            }],
        })
        .into()
}

fn landing_reports() -> Vec<LandingReportOutput> {
    let id = |byte| repoweave::vcs::CommitId::parse(&sha(byte)).expect("a full commit id");
    vec![
        LandingReportOutput::Created {
            path: "local/remotes/lib".to_owned(),
            branch: "dev".to_owned(),
            at: id('a'),
        },
        LandingReportOutput::Followed {
            path: "local/remotes/lib".to_owned(),
            to: id('a'),
            was: id('b'),
            backup: "proj--feat/bbbbbbbbbbbb".to_owned(),
        },
        LandingReportOutput::Relocked {
            path: "local/remotes/app".to_owned(),
            was: id('b'),
            now: id('c'),
        },
        LandingReportOutput::RePinned {
            path: "local/remotes/app".to_owned(),
            was: id('c'),
            now: id('d'),
        },
        LandingReportOutput::EntryChanged {
            path: "local/remotes/lib".to_owned(),
            field: "branch".to_owned(),
            was: "main".to_owned(),
            now: "dev".to_owned(),
        },
        LandingReportOutput::EntryRemoved {
            path: "local/remotes/docs".to_owned(),
        },
        LandingReportOutput::BeyondPin {
            path: "local/remotes/lib".to_owned(),
            into: "main".to_owned(),
            commits: vec![LeftCommitOutput {
                sha: id('e'),
                subject: "upstream fix".to_owned(),
            }],
        },
        LandingReportOutput::HistoryPinAbsent {
            path: "local/remotes/app".to_owned(),
            lock: id('a'),
            pin: id('b'),
        },
        LandingReportOutput::PushBranchAhead {
            path: "(project)".to_owned(),
            branch: "main".to_owned(),
        },
        LandingReportOutput::TakenIn {
            path: "(project)".to_owned(),
            branch: "main".to_owned(),
            from: id('e'),
            to: id('f'),
            commits: vec![LeftCommitOutput {
                sha: id('f'),
                subject: "lib onto dev".to_owned(),
            }],
            pins: vec![
                PinTakenInOutput {
                    path: "local/remotes/lib".to_owned(),
                    was: Some(sha('a')),
                    now: Some(sha('b')),
                    backward: false,
                },
                PinTakenInOutput {
                    path: "local/remotes/docs".to_owned(),
                    was: None,
                    now: Some(sha('c')),
                    backward: false,
                },
            ],
            member_changes: vec![
                MemberChangeOutput {
                    path: "local/remotes/lib".to_owned(),
                    change: EntryChange::Changed,
                    fields: vec!["branch: main -> dev".to_owned()],
                },
                MemberChangeOutput {
                    path: "local/remotes/docs".to_owned(),
                    change: EntryChange::Added,
                    fields: Vec::new(),
                },
                MemberChangeOutput {
                    path: "local/remotes/old".to_owned(),
                    change: EntryChange::Removed,
                    fields: Vec::new(),
                },
            ],
        },
        LandingReportOutput::Merged {
            path: "local/remotes/lib".to_owned(),
            merge: id('e'),
        },
        LandingReportOutput::Rebased {
            path: "local/remotes/app".to_owned(),
            was: id('f'),
            backup: "proj--feat/rebased-ffffffffffff".to_owned(),
        },
        LandingReportOutput::Carried {
            path: "local/remotes/app".to_owned(),
            branch: "w2".to_owned(),
            was: id('c'),
            now: id('d'),
            backup: "w2/rebased-cccccccccccc".to_owned(),
        },
        LandingReportOutput::Remapped {
            path: "local/remotes/app".to_owned(),
            was: id('a'),
            now: id('b'),
        },
        LandingReportOutput::Refreshed {
            path: "local/remotes/lib".to_owned(),
            absolute_path: repoweave::path_spelling::WirePath::of(std::path::Path::new(
                "/ws/local/remotes/lib",
            )),
            moved_from: id('a'),
            moved_to: id('b'),
            files: vec!["notes.txt".to_owned()],
            recorded_only: vec!["README.md".to_owned()],
        },
    ]
}

fn sync_to_corpus() -> Vec<(&'static str, Value)> {
    vec![
        (
            "from-a-workweave-advancing-the-project-repo",
            value(LandJsonOutput {
                schema: LAND_JSON_SCHEMA_URL.to_owned(),
                source_workweave: Some("feat-a".to_owned()),
                target: Some(repoweave::path_spelling::WirePath::of(
                    std::path::Path::new("/ws"),
                )),
                retired: true,
                survivors: survivors_of_every_reason(),
                // Fresh run: the absent shape of the `resumed` disclosure.
                resumed: None,
                outcomes: sync_outcomes(true),
                placements: placements(),
                members: members(),
                complete: true,
                refused: Vec::new(),
                project_repo_advance: Some(advance()),
                left_behind: left_behind(),
                reports: landing_reports(),
                resolution: Some(resolution()),
            }),
        ),
        (
            "from-the-primary-weave-no-op-advance",
            value(LandJsonOutput {
                schema: LAND_JSON_SCHEMA_URL.to_owned(),
                source_workweave: None,
                target: Some(repoweave::path_spelling::WirePath::of(
                    std::path::Path::new("/ws"),
                )),
                retired: false,
                survivors: Vec::new(),
                // Resumed at replay (the common interruption, a step-1
                // conflict): replay re-ran, so outcomes carries every repo.
                resumed: Some(OpPhase::Replay),
                outcomes: sync_outcomes(false),
                placements: Vec::new(),
                members: Vec::new(),
                complete: false,
                refused: judged_on_every_axis(),
                project_repo_advance: None,
                left_behind: Vec::new(),
                reports: Vec::new(),
                resolution: None,
            }),
        ),
        // The three post-replay resume shapes: replay had completed before the
        // interruption, so the resumed invocation re-entered downstream of it
        // and `outcomes` is empty by construction — the `resumed` value is
        // what tells a consumer the emptiness is a resume, not a zero-repo op.
        (
            "resumed-downstream-of-replay-at-relock",
            value(LandJsonOutput {
                schema: LAND_JSON_SCHEMA_URL.to_owned(),
                source_workweave: Some("feat-a".to_owned()),
                target: Some(repoweave::path_spelling::WirePath::of(
                    std::path::Path::new("/ws"),
                )),
                retired: false,
                survivors: Vec::new(),
                resumed: Some(OpPhase::Relock),
                outcomes: Vec::new(),
                placements: Vec::new(),
                members: Vec::new(),
                complete: false,
                refused: Vec::new(),
                project_repo_advance: Some(advance()),
                left_behind: Vec::new(),
                reports: Vec::new(),
                resolution: Some(resolution()),
            }),
        ),
        (
            "resumed-mid-advance-target",
            value(LandJsonOutput {
                schema: LAND_JSON_SCHEMA_URL.to_owned(),
                source_workweave: Some("feat-a".to_owned()),
                target: Some(repoweave::path_spelling::WirePath::of(
                    std::path::Path::new("/ws"),
                )),
                retired: false,
                survivors: Vec::new(),
                resumed: Some(OpPhase::AdvanceTarget),
                outcomes: Vec::new(),
                placements: placements(),
                members: members(),
                complete: false,
                refused: Vec::new(),
                project_repo_advance: None,
                left_behind: Vec::new(),
                reports: Vec::new(),
                resolution: Some(resolution()),
            }),
        ),
        (
            "resumed-at-retire-after-clearing-the-refusal",
            value(LandJsonOutput {
                schema: LAND_JSON_SCHEMA_URL.to_owned(),
                source_workweave: Some("feat-a".to_owned()),
                target: Some(repoweave::path_spelling::WirePath::of(
                    std::path::Path::new("/ws"),
                )),
                retired: true,
                survivors: Vec::new(),
                resumed: Some(OpPhase::Retire),
                outcomes: Vec::new(),
                placements: Vec::new(),
                members: Vec::new(),
                complete: true,
                refused: Vec::new(),
                project_repo_advance: None,
                left_behind: Vec::new(),
                reports: Vec::new(),
                resolution: Some(resolution()),
            }),
        ),
    ]
}

// ---------------------------------------------------------------------------
// update
// ---------------------------------------------------------------------------

fn update_records() -> Vec<RepoUpdateRecord> {
    [
        UpdateKind::Updated,
        UpdateKind::UpToDate,
        UpdateKind::Failed,
    ]
    .into_iter()
    .map(|kind| {
        let (old_sha, new_sha, error) = match kind {
            UpdateKind::Updated => (Some(sha('a')), Some(sha('b')), None),
            UpdateKind::UpToDate => (Some(sha('a')), Some(sha('a')), None),
            UpdateKind::Failed => (None, None, Some("not a fast-forward".to_owned())),
        };
        RepoUpdateRecord {
            path: REPO.to_owned(),
            absolute_path: wire(ABS),
            branch: "main".to_owned(),
            kind,
            old_sha,
            new_sha,
            error,
        }
    })
    .collect()
}

fn update_remote_corpus() -> Vec<(&'static str, Value)> {
    vec![
        (
            "every-kind",
            value(UpdateRemoteJsonOutput {
                schema_url: UPDATE_REMOTE_SCHEMA_URL.to_owned(),
                repos: update_records(),
                placements: placements(),
                resolution: Some(primary_resolution()),
                skipped: every_skip(),
            }),
        ),
        (
            "unresolved-workspace",
            value(UpdateRemoteJsonOutput {
                schema_url: UPDATE_REMOTE_SCHEMA_URL.to_owned(),
                repos: Vec::new(),
                placements: Vec::new(),
                resolution: None,
                skipped: Vec::new(),
            }),
        ),
    ]
}

fn each_outcomes() -> Vec<(&'static str, each_wire::Outcome)> {
    use each_wire::EachOutcomeStatus as S;
    use each_wire::Role as R;
    let roles = [R::Owned, R::Fork, R::Dependency, R::Reference];
    [
        ("ok", S::Ok, Some(0), None, Some(12)),
        ("failed-exit", S::Failed, Some(3), None, Some(40)),
        ("failed-signal", S::Failed, None, Some(9), Some(40)),
        ("failed-to-start", S::Failed, None, None, Some(1)),
        ("timed-out", S::TimedOut, None, Some(9), Some(1000)),
        ("not-run", S::NotRun, None, None, None),
        ("not-present", S::NotPresent, None, None, None),
    ]
    .into_iter()
    .enumerate()
    .map(|(i, (label, status, exit_code, signal, duration_ms))| {
        (
            label,
            each_wire::Outcome {
                path: "github/org/app".into(),
                role: roles[i % roles.len()],
                status,
                exit_code,
                signal,
                duration_ms,
                stdout: "out\n".into(),
                stderr: String::new(),
            },
        )
    })
    .collect()
}

fn each_corpus() -> Vec<(&'static str, Value)> {
    let outcomes: Vec<each_wire::Outcome> = each_outcomes().into_iter().map(|(_, o)| o).collect();
    vec![
        (
            "every-status",
            value(each_wire::EachJsonOutput {
                schema: each_schema_url::EACH_SCHEMA_URL.to_owned(),
                outcomes,
            }),
        ),
        (
            "empty",
            value(each_wire::EachJsonOutput {
                schema: each_schema_url::EACH_SCHEMA_URL.to_owned(),
                outcomes: Vec::new(),
            }),
        ),
    ]
}

// ---------------------------------------------------------------------------
// The pins
// ---------------------------------------------------------------------------

/// The check this file exists to make impossible to fail silently: a committed
/// artifact with nothing validated against it.
#[test]
fn every_committed_schema_has_a_registered_case() {
    let committed: BTreeSet<String> = json_schema::committed_verbs().into_iter().collect();
    let registered: BTreeSet<String> = cases().iter().map(|c| c.verb.to_owned()).collect();

    let unvalidated: Vec<&String> = committed.difference(&registered).collect();
    assert!(
        unvalidated.is_empty(),
        "committed under docs/reference/schemas/ with nothing validated against them: \
         {unvalidated:?} — add a VerbCase, or the artifact is a contract nothing keeps"
    );
    let phantom: Vec<&String> = registered.difference(&committed).collect();
    assert!(
        phantom.is_empty(),
        "registered here with no committed artifact: {phantom:?}"
    );
    assert_eq!(committed.len(), cases().len());
}

#[test]
fn every_corpus_document_validates_against_its_committed_schema() {
    for case in cases() {
        let schema = json_schema::committed_schema(case.verb);
        let docs = (case.corpus)();
        assert!(
            docs.len() >= 2,
            "{}: a one-document corpus samples one wire shape",
            case.verb
        );
        for (label, doc) in docs {
            let (errors, walk) = json_schema::conform(&doc, &schema);
            assert!(
                errors.is_empty(),
                "{} [{label}] does not satisfy {}:\n  {}",
                case.verb,
                json_schema::schema_path(case.verb),
                errors.join("\n  ")
            );
            // Non-vacuity: a traversal that stopped early reports the same
            // empty error list as a clean document.
            assert!(
                walk.properties_checked >= 2,
                "{} [{label}]: the walk never reached the envelope's properties: {walk:?}",
                case.verb
            );
        }
    }
}

/// An envelope field no document carries is unvalidated in its present shape,
/// and `skip_serializing_if` makes present and absent two different wire
/// shapes. This is what narrows doctor's exception to the struct-literal rule:
/// a new optional field on `DoctorJsonOutput` compiles here, but it cannot
/// reach the artifact and stay unsampled.
///
/// Top-level only. A nested optional field is a much larger space and this
/// would reject corpora that are reasonable; the envelope is where a field is
/// added.
#[test]
fn every_corpus_carries_every_envelope_field_its_schema_declares() {
    for case in cases() {
        let schema = json_schema::committed_schema(case.verb);
        let declared: BTreeSet<String> = schema["properties"]
            .as_object()
            .unwrap_or_else(|| panic!("{}: the artifact declares no properties", case.verb))
            .keys()
            .cloned()
            .collect();
        let mut observed = BTreeSet::new();
        for (_, doc) in (case.corpus)() {
            observed.extend(
                doc.as_object()
                    .expect("every envelope is an object")
                    .keys()
                    .cloned(),
            );
        }
        let unsampled: Vec<&String> = declared.difference(&observed).collect();
        assert!(
            unsampled.is_empty(),
            "{} declares envelope fields no document in its corpus carries, so their presence \
             on the wire is unvalidated: {unsampled:?}",
            case.verb
        );
    }
}

/// A corpus that never takes a shape the artifact permits validates and proves
/// nothing about that shape. The expectation is read out of the artifact so it
/// cannot fall behind it.
#[test]
fn every_corpus_exercises_every_member_its_schema_declares() {
    fn declares_a_wire_value(schema: &Value) -> bool {
        match schema {
            Value::Object(map) => {
                map.contains_key("enum")
                    || map.contains_key("const")
                    || map.values().any(declares_a_wire_value)
            }
            Value::Array(items) => items.iter().any(declares_a_wire_value),
            _ => false,
        }
    }
    for case in cases() {
        let schema = json_schema::committed_schema(case.verb);
        let declared = json_schema::declared_enum_values(&schema);
        if !declares_a_wire_value(&schema) {
            assert!(declared.is_empty(), "{}: {declared:?}", case.verb);
            continue;
        }
        assert!(
            !declared.is_empty(),
            "{}: no declared enum members were read — the coverage check is vacuous",
            case.verb
        );
        let mut observed = BTreeSet::new();
        for (_, doc) in (case.corpus)() {
            observed.extend(json_schema::observed_enum_values(&doc));
        }
        let unsampled: Vec<String> = declared
            .difference(&observed)
            .map(|(property, member)| format!("{property}: {member:?}"))
            .collect();
        assert!(
            unsampled.is_empty(),
            "{} declares wire values no document in its corpus takes, so conformance is \
             unmeasured for them: {unsampled:?}",
            case.verb
        );
    }
}

/// The committed artifacts must stay inside the validator's subset. This is
/// what turns "schemars emitted something new" into a failure here rather than
/// into silent under-validation of the real output.
///
/// The minimum keyword count guards against a truncated or empty schema
/// slipping through.
#[test]
fn every_committed_schema_stays_inside_the_validator_subset() {
    for verb in json_schema::committed_verbs() {
        let census = json_schema::census(&json_schema::committed_schema(&verb));
        assert!(
            census.seen > 20,
            "{}: the keyword walk read almost nothing: {census:?}",
            json_schema::schema_path(&verb)
        );
        assert!(
            census.unknown.is_empty(),
            "{} uses keywords the validator does not implement, so emitted output is only \
             partly checked: {:?}",
            json_schema::schema_path(&verb),
            census.unknown
        );
    }
}

#[test]
fn every_envelope_embeds_the_url_of_the_artifact_validated_here() {
    for case in cases() {
        let suffix = format!("/{}", json_schema::schema_path(case.verb));
        assert!(
            case.schema_url.ends_with(&suffix),
            "{} embeds {} which does not name {}",
            case.verb,
            case.schema_url,
            json_schema::schema_path(case.verb)
        );
        for (label, doc) in (case.corpus)() {
            assert_eq!(
                doc["$schema"].as_str(),
                Some(case.schema_url),
                "{} [{label}] points a consumer at a different artifact than the one it was \
                 validated against",
                case.verb
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Seeded failures
// ---------------------------------------------------------------------------
//
// The divergences a `--json` surface actually takes, applied to each verb's
// own envelope: a renamed key, an added key, a dropped required key, a
// retyped leaf. Every one of these is a well-formed JSON document, and every
// one strands a consumer reading the artifact the bytes name.

fn first_doc(case: &VerbCase) -> Value {
    (case.corpus)()
        .into_iter()
        .next()
        .expect("every case has a corpus")
        .1
}

/// The array key each envelope carries its per-repo records under.
fn records_key(verb: &str) -> &'static str {
    match verb {
        "doctor" => "violations",
        "status" | "update-remote" | "fetch" | "log" => "repos",
        "refusal" => "refused",
        "backup" => "repos",
        "branch" => "branches",
        "branch-delete" | "workweave" => "deleted",
        "cherry-pick" => "members",
        _ => "outcomes",
    }
}

#[test]
fn a_renamed_envelope_key_is_reported_for_every_verb() {
    for case in cases() {
        let schema = json_schema::committed_schema(case.verb);
        let key = records_key(case.verb);
        let mut doc = first_doc(&case);
        let records = doc[key].take();
        let object = doc.as_object_mut().expect("the envelope is an object");
        object.remove(key);
        object.insert("findings".to_owned(), records);

        let (errors, _) = json_schema::conform(&doc, &schema);
        assert!(
            errors.iter().any(|e| e.contains(&format!("`{key}`"))),
            "{}: renaming `{key}` must be reported, got {errors:?}",
            case.verb
        );
        assert!(
            errors.iter().any(|e| e.contains("`findings`")),
            "{}: the renamed key must be reported as undeclared, got {errors:?}",
            case.verb
        );
    }
}

#[test]
fn an_added_envelope_key_is_reported_for_every_verb() {
    for case in cases() {
        let schema = json_schema::committed_schema(case.verb);
        let mut doc = first_doc(&case);
        doc.as_object_mut()
            .expect("the envelope is an object")
            .insert("scope".to_owned(), json!("all"));
        let (errors, _) = json_schema::conform(&doc, &schema);
        assert!(
            errors
                .iter()
                .any(|e| e.contains("undeclared property `scope`")),
            "{}: a key the artifact does not declare must be reported, got {errors:?}",
            case.verb
        );
    }
}

#[test]
fn a_retyped_envelope_array_is_reported_for_every_verb() {
    for case in cases() {
        let schema = json_schema::committed_schema(case.verb);
        let key = records_key(case.verb);
        let mut doc = first_doc(&case);
        doc[key] = json!({ "count": 1 });
        let (errors, _) = json_schema::conform(&doc, &schema);
        assert!(
            errors.iter().any(|e| e.contains("expected type array")),
            "{}: `{key}` as an object must be reported, got {errors:?}",
            case.verb
        );
    }
}

#[test]
fn a_dropped_field_inside_a_record_is_reported_for_every_verb() {
    for case in cases() {
        let schema = json_schema::committed_schema(case.verb);
        let key = records_key(case.verb);
        let mut doc = first_doc(&case);
        let record = doc[key][0]
            .as_object_mut()
            .unwrap_or_else(|| panic!("{}: {key}[0] is an object", case.verb));
        let dropped = record
            .keys()
            .find(|k| k.as_str() != "kind")
            .expect("a record carries more than its tag")
            .clone();
        record.remove(&dropped);

        let (errors, _) = json_schema::conform(&doc, &schema);
        assert!(
            !errors.is_empty(),
            "{}: dropping `{dropped}` from {key}[0] was not reported",
            case.verb
        );
    }
}

/// `commits_ahead` is the only place a numeric width reaches these artifacts,
/// and it is the one member `type: integer` alone does not pin: a negative
/// count is an integer. The pin is `format: uint` plus `minimum: 0`.
#[test]
fn a_negative_commit_count_is_reported() {
    for verb in ["sync", "land"] {
        let schema = json_schema::committed_schema(verb);
        let case = cases()
            .into_iter()
            .find(|c| c.verb == verb)
            .expect("registered above");
        let mut doc = first_doc(&case);
        let outcomes = doc["outcomes"]
            .as_array_mut()
            .expect("outcomes is an array");
        let ahead = outcomes
            .iter_mut()
            .find(|o| o["kind"] == json!("already-ahead"))
            .expect("the corpus carries an already-ahead outcome");
        ahead["commits_ahead"] = json!(-1);

        let (errors, _) = json_schema::conform(&doc, &schema);
        assert!(
            errors.iter().any(|e| e.contains("format: uint")),
            "{verb}: a negative commits_ahead must be reported by `format`, got {errors:?}"
        );
        assert!(
            errors.iter().any(|e| e.contains("below minimum")),
            "{verb}: a negative commits_ahead must be reported by `minimum`, got {errors:?}"
        );
    }
}
