//! The three-way merge of a project's pins.
//!
//! One pure function over the merge base's lock, each parent's lock and the
//! merged `rwv.toml`, so every caller that joins two project histories reads
//! pins the same way and none depends on a verb's state.

use std::collections::BTreeMap;

use crate::manifest::RepoPath;
use crate::vcs::CommitId;

/// What one project commit holds: each member's pin and the branch its entry
/// declares. A pre-schema commit holds no declaration.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Held {
    pub pins: BTreeMap<RepoPath, CommitId>,
    pub declared: BTreeMap<RepoPath, String>,
}

/// Which parent of the merge a pin came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Parent {
    First,
    Second,
}

/// Whether one commit of a member contains another.
pub trait Reach {
    fn reaches(&self, member: &RepoPath, ancestor: &CommitId, of: &CommitId) -> Option<bool>;
}

/// What a merge of two project commits has to settle.
pub struct PinMerge<'a> {
    /// The merge base's pins; `None` where the parents share no history.
    pub base: Option<&'a Held>,
    pub first: &'a Held,
    pub second: &'a Held,
    /// The branch each member of the merged `rwv.toml` declares. Its keys are
    /// the merged member set.
    pub merged_declared: &'a BTreeMap<RepoPath, String>,
    /// The commit each member that ran a member merge ended at.
    pub member_merges: &'a BTreeMap<RepoPath, CommitId>,
}

/// A member the merge could not settle by itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PinFinding {
    /// Both parents changed the pin under one declared branch and neither pin
    /// contains the other. The first parent's pin is kept.
    Unmerged {
        member: RepoPath,
        first: CommitId,
        second: CommitId,
    },
    /// The parents declare the member differently: the pin of the side whose
    /// branch the merge keeps is kept, and the other side's is listed.
    DeclaredDiffer {
        member: RepoPath,
        kept: Parent,
        kept_pin: CommitId,
        other_pin: CommitId,
        base_pin: Option<CommitId>,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MergedPins {
    pub pins: BTreeMap<RepoPath, CommitId>,
    pub findings: Vec<PinFinding>,
}

pub fn merge_pins(merge: &PinMerge<'_>, reach: &dyn Reach) -> MergedPins {
    let mut merged = MergedPins::default();
    for member in merge.merged_declared.keys() {
        if let Some(tip) = merge.member_merges.get(member) {
            merged.pins.insert(member.clone(), tip.clone());
            continue;
        }
        let base = merge.base.and_then(|held| held.pins.get(member));
        let first = merge.first.pins.get(member);
        let second = merge.second.pins.get(member);
        let pin = if first == second {
            first.cloned()
        } else if first == base {
            second.cloned()
        } else if second == base {
            first.cloned()
        } else {
            match (first, second) {
                (Some(first), Some(second)) => {
                    let (pin, finding) = diverged(merge, reach, member, base, first, second);
                    merged.findings.extend(finding);
                    Some(pin)
                }
                (only, None) | (None, only) => only.cloned(),
            }
        };
        if let Some(pin) = pin {
            merged.pins.insert(member.clone(), pin);
        }
    }
    merged
}

fn diverged(
    merge: &PinMerge<'_>,
    reach: &dyn Reach,
    member: &RepoPath,
    base: Option<&CommitId>,
    first: &CommitId,
    second: &CommitId,
) -> (CommitId, Option<PinFinding>) {
    let one_branch = match (
        merge.first.declared.get(member),
        merge.second.declared.get(member),
    ) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    };
    if one_branch {
        if reach.reaches(member, first, second) == Some(true) {
            return (second.clone(), None);
        }
        if reach.reaches(member, second, first) == Some(true) {
            return (first.clone(), None);
        }
        return (
            first.clone(),
            Some(PinFinding::Unmerged {
                member: member.clone(),
                first: first.clone(),
                second: second.clone(),
            }),
        );
    }
    let kept_branch = merge.merged_declared.get(member);
    let kept = if kept_branch.is_some()
        && merge.second.declared.get(member) == kept_branch
        && merge.first.declared.get(member) != kept_branch
    {
        Parent::Second
    } else {
        Parent::First
    };
    let (kept_pin, other_pin) = match kept {
        Parent::First => (first, second),
        Parent::Second => (second, first),
    };
    (
        kept_pin.clone(),
        Some(PinFinding::DeclaredDiffer {
            member: member.clone(),
            kept,
            kept_pin: kept_pin.clone(),
            other_pin: other_pin.clone(),
            base_pin: base.cloned(),
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn commit(n: u8) -> CommitId {
        CommitId::parse(&format!("{n:02x}").repeat(20)).unwrap()
    }

    fn member(name: &str) -> RepoPath {
        RepoPath::new(name).unwrap()
    }

    struct Chain(Vec<(u8, u8)>);

    impl Reach for Chain {
        fn reaches(&self, _: &RepoPath, ancestor: &CommitId, of: &CommitId) -> Option<bool> {
            Some(
                ancestor == of
                    || self
                        .0
                        .iter()
                        .any(|(a, o)| commit(*a) == *ancestor && commit(*o) == *of),
            )
        }
    }

    fn held(pin: Option<u8>, branch: Option<&str>) -> Held {
        let mut held = Held::default();
        if let Some(pin) = pin {
            held.pins.insert(member("app"), commit(pin));
        }
        if let Some(branch) = branch {
            held.declared.insert(member("app"), branch.to_owned());
        }
        held
    }

    fn merge(
        base: Option<Held>,
        first: Held,
        second: Held,
        merged_branch: Option<&str>,
        ran: Option<u8>,
        reach: &Chain,
    ) -> MergedPins {
        let mut merged_declared = BTreeMap::new();
        if let Some(branch) = merged_branch {
            merged_declared.insert(member("app"), branch.to_owned());
        }
        let mut member_merges = BTreeMap::new();
        if let Some(tip) = ran {
            member_merges.insert(member("app"), commit(tip));
        }
        merge_pins(
            &PinMerge {
                base: base.as_ref(),
                first: &first,
                second: &second,
                merged_declared: &merged_declared,
                member_merges: &member_merges,
            },
            reach,
        )
    }

    fn pinned(merged: &MergedPins) -> Option<CommitId> {
        merged.pins.get(&member("app")).cloned()
    }

    #[test]
    fn the_side_that_changed_a_pin_wins() {
        let none = Chain(vec![]);
        let base = Some(held(Some(1), Some("main")));
        let kept_first = merge(
            base.clone(),
            held(Some(2), Some("main")),
            held(Some(1), Some("main")),
            Some("main"),
            None,
            &none,
        );
        assert_eq!(pinned(&kept_first), Some(commit(2)));
        let kept_second = merge(
            base,
            held(Some(1), Some("main")),
            held(Some(3), Some("main")),
            Some("main"),
            None,
            &none,
        );
        assert_eq!(pinned(&kept_second), Some(commit(3)));
        assert!(kept_first.findings.is_empty() && kept_second.findings.is_empty());
    }

    #[test]
    fn a_pin_moved_backward_by_one_side_survives_a_side_that_left_it() {
        let none = Chain(vec![]);
        let base = Some(held(Some(5), Some("main")));
        let merged = merge(
            base,
            held(Some(5), Some("main")),
            held(Some(2), Some("main")),
            Some("main"),
            None,
            &none,
        );
        assert_eq!(pinned(&merged), Some(commit(2)));
    }

    #[test]
    fn both_sides_changing_a_pin_alike_keep_that_pin() {
        let none = Chain(vec![]);
        let merged = merge(
            Some(held(Some(1), Some("main"))),
            held(Some(4), Some("main")),
            held(Some(4), Some("main")),
            Some("main"),
            None,
            &none,
        );
        assert_eq!(pinned(&merged), Some(commit(4)));
        assert!(merged.findings.is_empty());
    }

    #[test]
    fn both_sides_changing_a_pin_under_one_branch_keep_the_pin_containing_the_other() {
        let chain = Chain(vec![(2, 3)]);
        let base = Some(held(Some(1), Some("main")));
        let first_holds = merge(
            base.clone(),
            held(Some(3), Some("main")),
            held(Some(2), Some("main")),
            Some("main"),
            None,
            &chain,
        );
        assert_eq!(pinned(&first_holds), Some(commit(3)));
        let second_holds = merge(
            base,
            held(Some(2), Some("main")),
            held(Some(3), Some("main")),
            Some("main"),
            None,
            &chain,
        );
        assert_eq!(pinned(&second_holds), Some(commit(3)));
        assert!(first_holds.findings.is_empty() && second_holds.findings.is_empty());
    }

    #[test]
    fn diverged_pins_under_one_branch_keep_the_first_and_report_unmerged() {
        let none = Chain(vec![]);
        let merged = merge(
            Some(held(Some(1), Some("main"))),
            held(Some(2), Some("main")),
            held(Some(3), Some("main")),
            Some("main"),
            None,
            &none,
        );
        assert_eq!(pinned(&merged), Some(commit(2)));
        assert_eq!(
            merged.findings,
            vec![PinFinding::Unmerged {
                member: member("app"),
                first: commit(2),
                second: commit(3)
            }]
        );
    }

    #[test]
    fn parents_declaring_a_member_differently_keep_the_pin_of_the_side_whose_branch_the_merge_keeps(
    ) {
        let none = Chain(vec![]);
        let base = Some(held(Some(1), Some("main")));
        let merged = merge(
            base,
            held(Some(2), Some("main")),
            held(Some(3), Some("dev")),
            Some("dev"),
            None,
            &none,
        );
        assert_eq!(pinned(&merged), Some(commit(3)));
        assert_eq!(
            merged.findings,
            vec![PinFinding::DeclaredDiffer {
                member: member("app"),
                kept: Parent::Second,
                kept_pin: commit(3),
                other_pin: commit(2),
                base_pin: Some(commit(1)),
            }]
        );
    }

    #[test]
    fn a_side_that_declares_nothing_takes_the_differing_declaration_arm() {
        let none = Chain(vec![]);
        let merged = merge(
            Some(held(Some(1), None)),
            held(Some(2), None),
            held(Some(3), Some("main")),
            Some("main"),
            None,
            &none,
        );
        assert_eq!(pinned(&merged), Some(commit(3)));
        assert!(matches!(
            merged.findings[..],
            [PinFinding::DeclaredDiffer { .. }]
        ));
    }

    #[test]
    fn a_member_only_one_side_added_keeps_that_sides_pin() {
        let none = Chain(vec![]);
        let base = Some(held(None, None));
        let by_second = merge(
            base.clone(),
            held(None, None),
            held(Some(7), Some("main")),
            Some("main"),
            None,
            &none,
        );
        assert_eq!(pinned(&by_second), Some(commit(7)));
        let by_first = merge(
            base,
            held(Some(6), Some("main")),
            held(None, None),
            Some("main"),
            None,
            &none,
        );
        assert_eq!(pinned(&by_first), Some(commit(6)));
    }

    #[test]
    fn a_member_the_merged_manifest_drops_leaves_the_lock() {
        let none = Chain(vec![]);
        let merged = merge(
            Some(held(Some(1), Some("main"))),
            held(Some(2), Some("main")),
            held(Some(1), Some("main")),
            None,
            None,
            &none,
        );
        assert_eq!(merged.pins.keys().collect::<BTreeSet<_>>(), BTreeSet::new());
    }

    #[test]
    fn a_member_that_ran_a_member_merge_is_pinned_at_where_the_merge_ended() {
        let none = Chain(vec![]);
        let merged = merge(
            Some(held(Some(1), Some("main"))),
            held(Some(2), Some("main")),
            held(Some(3), Some("main")),
            Some("main"),
            Some(9),
            &none,
        );
        assert_eq!(pinned(&merged), Some(commit(9)));
        assert!(merged.findings.is_empty());
    }
}
