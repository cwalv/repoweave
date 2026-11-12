#![cfg(unix)]
//! `rwv` runs its command on a thread whose stack it sizes itself, so the host's
//! main-thread limit never decides whether it starts. `ulimit -s` caps only the
//! main thread, which makes a starved main thread reproducible here.

#[test]
fn rwv_starts_with_a_main_thread_stack_far_below_the_windows_default() {
    let bin = assert_cmd::cargo::cargo_bin("rwv");
    for args in ["--help", "--version"] {
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("ulimit -s 256 && exec \"$0\" {args}"))
            .arg(&bin)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "`rwv {args}` under a 256 KiB main-thread stack: {}\n{}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
