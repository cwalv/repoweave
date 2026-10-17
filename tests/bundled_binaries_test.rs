//! Every release channel carries the same binaries: `rwv` and its bundled
//! plugins, and never the maintainer tool that regenerates this repo's docs.
//!
//! The list is `[dist.binaries]` in dist-workspace.toml, which cargo-dist
//! reads for the archives and the Homebrew formula. The wheel builder reads
//! it too; the install script and the published crate cannot, so each is
//! held to it here.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

mod common;

/// Binaries of this package that are built for this repo's own maintenance
/// and must not reach a user, keyed to the file the crate leaves out.
const MAINTAINER_TOOLS: &[(&str, &str)] = &[("generate-explain", "src/bin/generate-explain.rs")];

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_toml(rel: &str) -> toml_edit::DocumentMut {
    std::fs::read_to_string(root().join(rel))
        .unwrap()
        .parse()
        .unwrap_or_else(|e| panic!("{rel}: {e}"))
}

fn shipped() -> Vec<String> {
    read_toml("dist-workspace.toml")["dist"]["binaries"]["*"]
        .as_array()
        .expect("dist-workspace.toml declares [dist.binaries] \"*\"")
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect()
}

/// Every binary target cargo builds for this package: the explicit `[[bin]]`
/// entries, plus `src/bin/<name>.rs` and `src/bin/<name>/main.rs` that no
/// explicit entry claims (cargo's auto-discovery).
fn package_binaries() -> BTreeSet<String> {
    let manifest = read_toml("Cargo.toml");
    let mut names = BTreeSet::new();
    let mut claimed = BTreeSet::new();
    if let Some(bins) = manifest.get("bin").and_then(|b| b.as_array_of_tables()) {
        for bin in bins {
            names.insert(bin["name"].as_str().unwrap().to_owned());
            if let Some(path) = bin.get("path").and_then(|p| p.as_str()) {
                claimed.insert(path.to_owned());
            }
        }
    }
    for entry in std::fs::read_dir(root().join("src/bin")).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_stem().unwrap().to_string_lossy().into_owned();
        let target = if path.is_dir() {
            path.join("main.rs")
        } else if path.extension().is_some_and(|e| e == "rs") {
            path.clone()
        } else {
            continue;
        };
        let rel = target
            .strip_prefix(root())
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        if target.is_file() && !claimed.contains(&rel) {
            names.insert(name);
        }
    }
    names
}

#[test]
fn the_release_ships_every_binary_except_the_maintainer_tools() {
    let mut expected = package_binaries();
    for (tool, _) in MAINTAINER_TOOLS {
        assert!(
            expected.remove(*tool),
            "{tool} is no longer a binary of this package"
        );
    }
    let shipped: BTreeSet<String> = shipped().into_iter().collect();
    assert_eq!(
        shipped, expected,
        "dist-workspace.toml [dist.binaries] must name every binary a user gets — rwv and \
         each bundled plugin — and no maintainer tool"
    );
    assert!(shipped.contains("rwv") && shipped.contains("rwv-each"));
}

#[test]
fn the_published_crate_leaves_the_maintainer_tools_out() {
    let manifest = read_toml("Cargo.toml");
    let excluded: BTreeSet<&str> = manifest["package"]["exclude"]
        .as_array()
        .expect("Cargo.toml [package] exclude")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    for (tool, file) in MAINTAINER_TOOLS {
        assert!(
            excluded.contains(file),
            "`cargo install repoweave` would install {tool}: exclude {file} from the crate"
        );
    }
}

fn install_sh() -> String {
    std::fs::read_to_string(root().join("docs/install.sh")).unwrap()
}

#[test]
fn the_install_script_installs_what_the_release_ships() {
    let script = install_sh();
    let line = script
        .lines()
        .find_map(|l| l.strip_prefix("BINARIES=\""))
        .expect("docs/install.sh sets BINARIES=\"...\"");
    let listed: Vec<&str> = line.trim_end_matches('"').split_whitespace().collect();
    assert_eq!(listed, shipped());
}

/// A cargo-dist-shaped archive of `names`, each a small file holding its own
/// name, built by `python3` so it runs wherever the wheel builder can.
fn fake_archive(dir: &Path, file: &str, names: &[&str]) -> PathBuf {
    let path = dir.join(file);
    let stem = file.trim_end_matches(".tar.xz").trim_end_matches(".zip");
    let script = r#"
import io, sys, tarfile, zipfile
path, stem, names = sys.argv[1], sys.argv[2], sys.argv[3:]
if path.endswith(".zip"):
    with zipfile.ZipFile(path, "w") as z:
        for n in names:
            z.writestr(n, n)
else:
    with tarfile.open(path, "w:xz") as t:
        for n in names:
            data = n.encode()
            info = tarfile.TarInfo(f"{stem}/{n}")
            info.size = len(data)
            info.mode = 0o755
            t.addfile(info, io.BytesIO(data))
"#;
    let out = Command::new("python3")
        .arg("-c")
        .arg(script)
        .arg(&path)
        .arg(stem)
        .args(names)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    path
}

fn wheel_builder_available() -> bool {
    if common::skip_without_tool("python3") {
        return false;
    }
    let tomllib = Command::new("python3")
        .args(["-c", "import tomllib"])
        .status()
        .unwrap();
    if !tomllib.success() {
        common::report_skip("python3 has no tomllib (needs 3.11+), which build_wheels.py reads");
        return false;
    }
    true
}

fn build_wheel(archive: &Path, platform: &str, out: &Path) -> std::process::Output {
    Command::new("python3")
        .arg(root().join("python/build_wheels.py"))
        .args(["--version", "9.9.9", "--platform", platform, "--out"])
        .arg(out)
        .arg("--archive")
        .arg(archive)
        .output()
        .unwrap()
}

fn wheel_scripts(wheel: &Path) -> BTreeSet<String> {
    let out = Command::new("python3")
        .args([
            "-c",
            "import sys, zipfile; print('\\n'.join(zipfile.ZipFile(sys.argv[1]).namelist()))",
        ])
        .arg(wheel)
        .output()
        .unwrap();
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .filter_map(|l| l.strip_prefix("repoweave-9.9.9.data/scripts/"))
        .map(str::to_owned)
        .collect()
}

#[test]
fn each_wheel_carries_every_shipped_binary_and_nothing_else() {
    if !wheel_builder_available() {
        return;
    }
    let tmp = common::tempdir().unwrap();
    let shipped = shipped();
    for (file, platform, suffix) in [
        (
            "repoweave-x86_64-unknown-linux-gnu.tar.xz",
            "manylinux_2_17_x86_64.manylinux2014_x86_64",
            "",
        ),
        ("repoweave-x86_64-pc-windows-msvc.zip", "win_amd64", ".exe"),
    ] {
        let mut names: Vec<String> = shipped.iter().map(|n| format!("{n}{suffix}")).collect();
        names.push(format!("generate-explain{suffix}"));
        names.push("README.md".into());
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let archive = fake_archive(tmp.path(), file, &refs);
        let out_dir = tmp.path().join(platform);
        let out = build_wheel(&archive, platform, &out_dir);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let wheel = PathBuf::from(String::from_utf8(out.stdout).unwrap().trim());
        let expected: BTreeSet<String> = shipped.iter().map(|n| format!("{n}{suffix}")).collect();
        assert_eq!(wheel_scripts(&wheel), expected, "{platform}");
    }
}

#[test]
fn a_wheel_is_refused_when_the_archive_lacks_a_shipped_binary() {
    if !wheel_builder_available() {
        return;
    }
    let tmp = common::tempdir().unwrap();
    let archive = fake_archive(
        tmp.path(),
        "repoweave-x86_64-unknown-linux-gnu.tar.xz",
        &["rwv"],
    );
    let out = build_wheel(
        &archive,
        "manylinux_2_17_x86_64.manylinux2014_x86_64",
        &tmp.path().join("out"),
    );
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("rwv-each"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The install script against a stand-in `curl` that serves a local archive:
/// it must install every shipped binary and nothing else the archive holds.
#[test]
#[cfg(unix)]
fn the_install_script_installs_every_shipped_binary_from_the_archive() {
    use std::os::unix::fs::PermissionsExt;
    if !wheel_builder_available() || common::skip_without_tool("xz") {
        return;
    }
    let tmp = common::tempdir().unwrap();
    let triple = match std::env::consts::ARCH {
        "x86_64" => "x86_64",
        "aarch64" => "aarch64",
        other => {
            common::report_skip(&format!("install.sh does not support {other}"));
            return;
        }
    };
    let os = if cfg!(target_os = "macos") {
        "apple-darwin"
    } else {
        "unknown-linux-gnu"
    };
    let file = format!("repoweave-{triple}-{os}.tar.xz");
    let mut names: Vec<String> = shipped();
    names.push("generate-explain".into());
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let archive = fake_archive(tmp.path(), &file, &refs);

    let bin = tmp.path().join("stub");
    std::fs::create_dir_all(&bin).unwrap();
    let curl = bin.join("curl");
    std::fs::write(
        &curl,
        format!(
            "#!/bin/sh\nwhile [ $# -gt 0 ]; do case \"$1\" in -o) out=\"$2\"; shift 2;; *) shift;; esac; done\ncp '{}' \"$out\"\n",
            archive.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&curl, std::fs::Permissions::from_mode(0o755)).unwrap();
    let dest = tmp.path().join("installed");
    let path = format!("{}:/usr/bin:/bin", bin.display());
    let out = Command::new("sh")
        .arg(root().join("docs/install.sh"))
        .env("PATH", path)
        .env("VERSION", "9.9.9")
        .env("INSTALL_DIR", &dest)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let installed: BTreeSet<String> = std::fs::read_dir(&dest)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(installed, shipped().into_iter().collect::<BTreeSet<_>>());
}
