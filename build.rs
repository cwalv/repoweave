fn main() {
    // Embed git describe output so dev builds show e.g. "0.1.1-3-ge5bfa9f"
    if let Ok(output) = std::process::Command::new("git")
        .args(["describe", "--tags", "--always", "--dirty"])
        .output()
    {
        if output.status.success() {
            let describe = String::from_utf8_lossy(&output.stdout).trim().to_string();
            println!("cargo:rustc-env=RWV_VERSION={describe}");
        }
    }
    // Rebuild if git state changes
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/refs/");
}
