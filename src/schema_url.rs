//! The `docs/reference/schemas/<verb>.json` raw-GitHub URL template every
//! `--json`-capable verb's schema URL const is built from.

/// Expands to the `&'static str` URL for `docs/reference/schemas/<verb>.json`.
macro_rules! schema_url {
    ($verb:literal) => {
        concat!(
            "https://raw.githubusercontent.com/cwalv/repoweave/main/docs/reference/schemas/",
            $verb,
            ".json"
        )
    };
}
pub(crate) use schema_url;
