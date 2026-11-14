//! The ref namespaces rwv keeps below `refs/rwv/`: one variant each, with the
//! file that owns it and how long a ref in it lives. Every place that names a
//! namespace reads it from here, so a ref below `refs/rwv/` that no variant
//! claims is one nothing of rwv's wrote.

pub(super) const ROOT: &str = "refs/rwv";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RwvRef {
    PreOp,
    Backup,
}

/// How long a ref in a namespace stands.
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Lifetime {
    /// Until the operation that wrote it ends: it consumes the ref, or a
    /// later `rwv doctor --fix` drops it as orphaned.
    OneOperation,
    /// Until the operator drops it, or prunes the ones a branch reaches.
    UntilDropped,
}

impl RwvRef {
    pub(super) const ALL: [Self; 2] = [Self::PreOp, Self::Backup];

    pub(super) fn namespace(self) -> &'static str {
        match self {
            Self::PreOp => "refs/rwv/pre-op",
            Self::Backup => "refs/rwv/backup",
        }
    }

    #[cfg(test)]
    pub(super) fn owner(self) -> &'static str {
        match self {
            Self::PreOp => "src/git.rs",
            Self::Backup => "src/git/backup.rs",
        }
    }

    #[cfg(test)]
    pub(super) fn lifetime(self) -> Lifetime {
        match self {
            Self::PreOp => Lifetime::OneOperation,
            Self::Backup => Lifetime::UntilDropped,
        }
    }

    /// The prefix of every ref in the namespace, to hand to `for-each-ref`.
    pub(super) fn prefix(self) -> String {
        format!("{}/", self.namespace())
    }

    /// The `--glob` argument that selects every ref in the namespace.
    pub(super) fn glob(self) -> String {
        format!("--glob={}/*", self.namespace())
    }

    /// Whether `refname` is below `refs/rwv/` and in no namespace.
    pub(super) fn is_unclaimed(refname: &str) -> bool {
        refname
            .strip_prefix(ROOT)
            .is_some_and(|below| below.starts_with('/'))
            && !Self::ALL
                .iter()
                .any(|namespace| refname.starts_with(&namespace.prefix()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_namespace_is_below_the_root_and_none_contains_another() {
        for a in RwvRef::ALL {
            assert!(a.prefix().starts_with(&format!("{ROOT}/")));
            for b in RwvRef::ALL {
                assert!(a == b || !a.prefix().starts_with(&b.prefix()));
            }
        }
    }

    #[test]
    fn a_ref_in_no_namespace_is_unclaimed() {
        assert!(RwvRef::is_unclaimed("refs/rwv/pre-abort/x"));
        assert!(RwvRef::is_unclaimed("refs/rwv/pre-opx/x"));
        assert!(!RwvRef::is_unclaimed("refs/rwv/pre-op/x"));
        assert!(RwvRef::is_unclaimed("refs/rwv/superseded/o/b/1"));
        assert!(!RwvRef::is_unclaimed("refs/heads/rwv"));
    }

    #[test]
    fn the_internals_page_states_each_namespace_and_how_long_it_lives() {
        let page = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/internals/state.md"),
        )
        .unwrap();
        for namespace in RwvRef::ALL {
            let lifetime = match namespace.lifetime() {
                Lifetime::OneOperation => "one operation",
                Lifetime::UntilDropped => "until dropped",
            };
            let row = page
                .lines()
                .find(|line| line.contains(namespace.namespace()))
                .unwrap_or_else(|| panic!("{} is not on the page", namespace.namespace()));
            assert!(row.contains(lifetime), "{row}");
        }
    }

    #[test]
    fn each_namespace_is_named_in_the_file_that_owns_it() {
        for namespace in RwvRef::ALL {
            let source = std::fs::read_to_string(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(namespace.owner()),
            )
            .unwrap();
            assert!(
                source.contains(&format!("RwvRef::{namespace:?}")),
                "{namespace:?} is not used in {}",
                namespace.owner()
            );
        }
    }
}
