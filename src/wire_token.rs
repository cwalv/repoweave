//! The one string a closed enum serializes to.

use serde::Serialize;

/// The token a fieldless enum serializes to, for a `Display` that must be the
/// wire spelling.
///
/// The type's `rename_all` is then the only place a variant becomes a string;
/// a second spelling — a `match` arm, a table, a doc heading typed by hand —
/// is a thing to keep in sync rather than a convenience.
pub(crate) fn wire_token<T: Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(token)) => token,
        other => panic!("a fieldless enum must serialize to a string, got {other:?}"),
    }
}
