//! A closed enum whose variants reach an operator in text and on `--json` is
//! spelled once: its `Display` is the token it serializes to, so a message
//! naming a verb or a phase reads the same as the field a script matches on.

use clap::ValueEnum;
use repoweave::check::KindFilter;
use repoweave::integration::IssueKind;
use repoweave::op_state::{OpPhase, OpVerb, SyncStrategy};
use repoweave::vcs::{ConflictOp, MidOperation};
use repoweave::workspace::AdvisoryKindOutput;
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeSet;
use std::fmt::{Debug, Display};

mod common;

fn wire<T: Serialize>(variant: &T) -> String {
    match serde_json::to_value(variant).unwrap() {
        Value::String(token) => token,
        other => panic!("a fieldless variant serializes to a string, got {other}"),
    }
}

fn assert_display_is_the_wire_token<T>(variants: &[T])
where
    T: JsonSchema + Serialize + Display + Debug,
{
    let mut spelled = BTreeSet::new();
    for variant in variants {
        let token = wire(variant);
        assert_eq!(
            variant.to_string(),
            token,
            "{variant:?}: text and --json spell it differently"
        );
        spelled.insert(token);
    }

    let schema = serde_json::to_value(schemars::schema_for!(T)).unwrap();
    assert_eq!(
        spelled,
        common::json_schema::enum_member_tokens(&schema),
        "the variants listed here are not the variants the type declares"
    );
}

#[test]
fn op_verb_displays_as_the_token_it_serializes_to() {
    assert_display_is_the_wire_token(&[OpVerb::Sync, OpVerb::SyncTo]);
}

#[test]
fn op_phase_displays_as_the_token_it_serializes_to() {
    assert_display_is_the_wire_token(&[
        OpPhase::Replay,
        OpPhase::Relock,
        OpPhase::AdvanceTarget,
        OpPhase::Retire,
    ]);
}

#[test]
fn conflict_op_displays_as_the_token_it_serializes_to() {
    assert_display_is_the_wire_token(&ConflictOp::ALL);
}

#[test]
fn mid_operation_displays_as_the_token_it_serializes_to() {
    let variants: Vec<MidOperation> = MidOperation::all().collect();
    assert_display_is_the_wire_token(&variants);

    let tokens: BTreeSet<String> = variants.iter().map(wire).collect();
    assert_eq!(
        tokens,
        BTreeSet::from(
            ["mid-rebase", "mid-merge", "mid-cherry-pick", "mid-bisect"].map(str::to_owned)
        ),
        "the tokens `rwv status --json` reports in `mid_op`"
    );
    for variant in variants {
        let parsed: MidOperation =
            serde_json::from_value(Value::String(variant.to_string())).unwrap();
        assert_eq!(parsed, variant, "parsed back from its own token");
    }
}

#[test]
fn a_mid_operation_token_without_its_prefix_or_name_does_not_parse() {
    for token in ["rebase", "bisect", "mid-", "mid-bogus", "Mid-Rebase"] {
        assert!(
            serde_json::from_value::<MidOperation>(Value::String(token.to_owned())).is_err(),
            "`{token}` parsed"
        );
    }
}

#[test]
fn sync_strategy_displays_reads_and_is_offered_as_the_token_it_serializes_to() {
    let variants = SyncStrategy::value_variants();
    assert!(variants.len() > 1, "{variants:?}");
    for variant in variants {
        let token = wire(variant);
        assert_eq!(variant.to_string(), token, "{variant:?}: Display");
        assert_eq!(
            variant.to_possible_value().unwrap().get_name(),
            token,
            "{variant:?}: the name `--strategy` accepts"
        );
        assert_eq!(
            serde_json::from_value::<SyncStrategy>(Value::String(token)).unwrap(),
            *variant,
            "{variant:?}: read back from its own token"
        );
    }
}

#[test]
fn advisory_kind_is_kebab_case_and_the_finding_it_reports() {
    let schema = serde_json::to_value(schemars::schema_for!(AdvisoryKindOutput)).unwrap();
    let declared = common::json_schema::enum_member_tokens(&schema);
    assert_eq!(
        declared,
        BTreeSet::from(["derived-state-stale".to_owned()]),
        "{schema:#}"
    );

    assert_eq!(
        wire(&AdvisoryKindOutput::DerivedStateStale),
        IssueKind::DerivedStateStale.tag(),
        "an advisory names the doctor finding it repeats by that finding's own kind"
    );
}

#[test]
fn every_issue_kind_tag_is_a_name_doctor_kind_accepts() {
    let accepted: BTreeSet<String> = KindFilter::valid_kinds().into_iter().collect();
    let issues = common::doctor_corpus::issue_corpus();
    assert!(issues.len() > 1, "the corpus samples the issue kinds");
    for issue in issues {
        let tag = issue.kind.tag();
        assert!(
            accepted.contains(&tag),
            "`{tag}` is the tag a finding is filtered by but no `--kind` accepts it"
        );
    }
}
