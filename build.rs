use std::path::PathBuf;
use std::process::Command;

fn main() {
    // Embed git describe output so dev builds show e.g. "0.1.1-3-ge5bfa9f"
    if let Ok(output) = Command::new("git")
        .args(["describe", "--tags", "--always", "--dirty"])
        .output()
    {
        if output.status.success() {
            let describe = String::from_utf8_lossy(&output.stdout).trim().to_string();
            println!("cargo:rustc-env=RWV_VERSION={describe}");
        }
    }
    // A worktree checkout's `.git` is a file pointing at the real git dir
    // elsewhere, so HEAD and refs must be resolved through git rather than
    // assumed at a path relative to the crate root.
    for path in ["HEAD", "refs"] {
        if let Some(resolved) = git_path(path) {
            println!("cargo:rerun-if-changed={}", resolved.display());
        }
    }
}

fn git_path(path: &str) -> Option<PathBuf> {
    let output = Command::new("git")
        .args(["rev-parse", "--git-path", path])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| PathBuf::from(String::from_utf8_lossy(&output.stdout).trim().to_string()))
}
