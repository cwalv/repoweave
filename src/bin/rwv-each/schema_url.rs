//! The schema URLs `rwv each --json` output names. Spelled out here because
//! this plugin cannot reach the library's `schema_url!`; generate-explain
//! holds the two spellings to one template.

pub const EACH_SCHEMA_URL: &str =
    "https://raw.githubusercontent.com/cwalv/repoweave/main/docs/reference/schemas/each.json";

pub const EACH_RECORD_SCHEMA_URL: &str =
    "https://raw.githubusercontent.com/cwalv/repoweave/main/docs/reference/schemas/each-record.json";
