//! The exit statuses a verb ends with besides success and failure.

/// A verb finished everything it could and left part of the work to the
/// operator. `rwv clone` ends so when a member is left with no pin or at a
/// pin no ref reaches; the deletion rule ends so when branches survive it.
///
/// Distinct from `1`, where the verb stopped, and from `2`, where the
/// arguments did not parse.
pub const COMPLETED_WITH_REMAINDER: u8 = 3;

/// The error a verb returns to end with [`COMPLETED_WITH_REMAINDER`], once
/// it has printed what it left. It prints nothing further.
#[derive(Debug)]
pub struct CompletedWithRemainder;

impl std::fmt::Display for CompletedWithRemainder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("completed, with part of the work left")
    }
}

impl std::error::Error for CompletedWithRemainder {}

/// Whether `err` is a verb's [`CompletedWithRemainder`], wherever in its chain
/// it sits.
pub fn is_remainder(err: &anyhow::Error) -> bool {
    err.chain()
        .any(|link| link.downcast_ref::<CompletedWithRemainder>().is_some())
}
