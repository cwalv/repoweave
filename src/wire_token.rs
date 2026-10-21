//! The one string an enum variant is written to and read from.

use serde::de::DeserializeOwned;
use serde::Serialize;

/// The token an enum variant serializes under, for a `Display` or a tag that
/// must be the wire spelling: the string a fieldless variant is, or the one key
/// of an externally tagged variant that carries fields.
///
/// The type's `rename_all` is then the only place a variant becomes a string;
/// a second spelling — a `match` arm, a table, a doc heading typed by hand —
/// is a thing to keep in sync rather than a convenience.
pub(crate) fn wire_token<T: Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(token)) => token,
        Ok(serde_json::Value::Object(map)) if map.len() == 1 => map
            .into_iter()
            .next()
            .map(|(token, _)| token)
            .unwrap_or_default(),
        other => {
            panic!("an enum variant must serialize to a string or a one-key object, got {other:?}")
        }
    }
}

/// The variant a token deserializes to, for a `FromStr` that must read exactly
/// what [`wire_token`] writes.
pub(crate) fn parse_wire_token<T: DeserializeOwned>(token: &str) -> Option<T> {
    serde_json::from_value(serde_json::Value::String(token.to_owned())).ok()
}
