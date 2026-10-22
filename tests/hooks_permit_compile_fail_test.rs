//! The install hooks run only with a `HooksPermit`, and only the gate that asks
//! whether they may run can make one. Pinned by diagnostic code, from an
//! external crate, where every visibility narrower than `pub` looks alike.
//!
//! Residue: privacy stops at `activate.rs`, where the tuple literal is
//! writable; `tests/hook_withhold_scope_test.rs` holds it to the mint.

mod common;

use common::compile_probe::{assert_fails_with, compile};

const PRELUDE: &str = r#"
    use repoweave::activate::HooksPermit;
    use repoweave::integration::{Integration, IntegrationContextBase};
    use repoweave::integration_runner::run_activate_hooks;
    use repoweave::manifest::Manifest;
"#;

fn probe(body: &str) -> String {
    format!("{PRELUDE}\n{body}")
}

#[test]
fn the_harness_can_run_the_hooks_with_a_permit() {
    // Control: the call the probes below break, spelled legally.
    let (compiled, stderr) = compile(&probe(
        r#"
        pub fn run(
            permit: HooksPermit,
            i: &[&dyn Integration],
            m: &Manifest,
            c: &IntegrationContextBase<'_>,
        ) {
            let _ = run_activate_hooks(permit, i, m, c);
        }
        "#,
    ));
    assert!(compiled, "control snippet must compile; got:\n{stderr}");
}

#[test]
fn the_hooks_do_not_run_without_a_permit() {
    assert_fails_with(
        "E0061",
        "run_activate_hooks takes a HooksPermit",
        &probe(
            r#"
            pub fn run(i: &[&dyn Integration], m: &Manifest, c: &IntegrationContextBase<'_>) {
                let _ = run_activate_hooks(i, m, c);
            }
            "#,
        ),
    );
}

#[test]
fn one_permit_runs_the_hooks_once() {
    assert_fails_with(
        "E0382",
        "a permit is consumed by the run it allows",
        &probe(
            r#"
            pub fn run(
                permit: HooksPermit,
                i: &[&dyn Integration],
                m: &Manifest,
                c: &IntegrationContextBase<'_>,
            ) {
                let _ = run_activate_hooks(permit, i, m, c);
                let _ = run_activate_hooks(permit, i, m, c);
            }
            "#,
        ),
    );
}

#[test]
fn a_permit_cannot_be_forged_by_tuple_literal() {
    assert_fails_with(
        "E0423",
        "the field is private to activate.rs",
        &probe(
            r#"
            pub fn forge() -> HooksPermit {
                HooksPermit(())
            }
            "#,
        ),
    );
}

#[test]
fn a_permit_has_no_unconditional_mint() {
    assert_fails_with(
        "E0599",
        "granted() exists only in a test build of the crate",
        &probe(
            r#"
            pub fn forge() -> HooksPermit {
                HooksPermit::granted()
            }
            "#,
        ),
    );
}

#[test]
fn the_gate_is_not_callable_outside_activate() {
    assert_fails_with(
        "E0603",
        "only activate.rs asks the gate's questions",
        &probe(
            r#"
            pub fn mint(
                i: &[&dyn Integration],
                m: &Manifest,
                c: &IntegrationContextBase<'_>,
            ) -> Option<HooksPermit> {
                repoweave::activate::permit_hooks(i, m, c).ok().flatten()
            }
            "#,
        ),
    );
}
