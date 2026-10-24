//! Every release channel carries the same binaries: `rwv` and its bundled
//! plugins, and never the maintainer tool that regenerates this repo's docs.
//!
//! The list is `[dist.binaries]` in dist-workspace.toml, which cargo-dist
//! reads for the archives and the Homebrew formula. The wheel builder reads
//! it too; the install script, `cargo install` and the published crate
//! cannot, so each is held to it here.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

mod common;

/// Programs of this package that are built for this repo's own maintenance
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

fn cargo() -> Command {
    let mut cmd = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    for var in common::GIT_ENV_VARS {
        cmd.env_remove(var);
    }
    cmd
}

/// This package's entry in `cargo metadata`, which in a workspace of several
/// members is one of several.
fn package_metadata() -> serde_json::Value {
    let out = cargo()
        .args([
            "metadata",
            "--no-deps",
            "--format-version",
            "1",
            "--manifest-path",
        ])
        .arg(root().join("Cargo.toml"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let metadata: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let manifest = repoweave::path_spelling::resolve(&root().join("Cargo.toml")).unwrap();
    metadata["packages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| {
            repoweave::path_spelling::resolve(Path::new(p["manifest_path"].as_str().unwrap()))
                .unwrap()
                == manifest
        })
        .expect("cargo metadata lists this package")
        .clone()
}

fn strings(value: &serde_json::Value) -> Vec<String> {
    value
        .as_array()
        .map(|a| a.iter().map(|v| v.as_str().unwrap().to_owned()).collect())
        .unwrap_or_default()
}

/// The features `cargo install` turns on with no `--features`: `default`, and
/// every package feature it names, transitively.
fn default_features(package: &serde_json::Value) -> BTreeSet<String> {
    let table = &package["features"];
    let mut on = BTreeSet::new();
    let mut pending = vec!["default".to_owned()];
    while let Some(feature) = pending.pop() {
        if table.get(&feature).is_some() && on.insert(feature.clone()) {
            pending.extend(strings(&table[&feature]));
        }
    }
    on
}

/// Every target `cargo install` installs from this package: a `bin` whose
/// required features are all on by default.
fn installable_binaries(package: &serde_json::Value) -> BTreeSet<String> {
    let defaults = default_features(package);
    package["targets"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| strings(&t["kind"]).iter().any(|k| k == "bin"))
        .filter(|t| {
            strings(&t["required-features"])
                .iter()
                .all(|f| defaults.contains(f))
        })
        .map(|t| t["name"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn cargo_install_installs_exactly_the_shipped_binaries() {
    let package = package_metadata();
    let shipped: BTreeSet<String> = shipped().into_iter().collect();
    assert_eq!(
        installable_binaries(&package),
        shipped,
        "`cargo install` must install what every other channel ships — rwv and each \
         bundled plugin — and no maintainer tool"
    );
    assert!(shipped.contains("rwv") && shipped.contains("rwv-each"));
    let targets: BTreeSet<String> = package["targets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_owned())
        .collect();
    for (tool, _) in MAINTAINER_TOOLS {
        assert!(
            targets.contains(*tool),
            "{tool} is no longer a target of this package"
        );
    }
}

/// `git ls-files` for what a commit of this checkout would hold: the index,
/// plus untracked files not ignored.
fn committable_files() -> Vec<String> {
    let out = common::git_in(
        root(),
        &["ls-files", "--cached", "--others", "--exclude-standard"],
    );
    out.lines()
        .filter(|rel| root().join(rel).is_file())
        .map(str::to_owned)
        .collect()
}

/// `cargo install --git <this repo>` with no package named searches every
/// `Cargo.toml` in the repository and refuses when more than one package has
/// a binary. Run against a commit of this checkout, with every registry
/// replaced by an empty directory so nothing is fetched: past package
/// selection, the install fails resolving its first dependency instead.
#[test]
fn cargo_install_git_finds_one_package_with_binaries() {
    let tmp = common::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let files = committable_files();
    assert!(files.iter().any(|f| f == "Cargo.toml"), "{files:?}");
    for rel in &files {
        let dest = repo.join(rel);
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        std::fs::copy(root().join(rel), &dest).unwrap();
    }
    common::git_in(&repo, &["init", "-q"]);
    common::git_in(&repo, &["add", "-A"]);
    common::git_in(&repo, &["commit", "-q", "-m", "snapshot"]);

    let empty_registry = tmp.path().join("empty-registry");
    std::fs::create_dir_all(&empty_registry).unwrap();
    let out = cargo()
        .current_dir(tmp.path())
        .env("CARGO_HOME", tmp.path().join("cargo-home"))
        .env("CARGO_TARGET_DIR", tmp.path().join("target"))
        .args(["install", "--git", &common::file_url(&repo), "--root"])
        .arg(tmp.path().join("root"))
        .args(["--config", "source.crates-io.replace-with='empty'"])
        .arg("--config")
        .arg(format!(
            "source.empty.directory='{}'",
            empty_registry.display().to_string().replace('\\', "/")
        ))
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{stderr}");
    assert!(
        !stderr.contains("multiple packages with binaries"),
        "the documented `cargo install --git https://github.com/cwalv/repoweave` needs \
         this repository to hold one package with binaries; a fixture crate under tests/ \
         is one more:\n{stderr}"
    );
    assert!(
        stderr.contains("failed to compile `repoweave v") && stderr.contains("no matching package"),
        "cargo selected the package and then failed on something other than the empty \
         registry:\n{stderr}"
    );
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
            "the packaged crate would ship {tool}: exclude {file} from the crate"
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

/// What the install script runs besides `tar`, `xz` and `curl`, plus the `cp`
/// the stand-in `curl` runs.
#[cfg(unix)]
const INSTALL_SCRIPT_TOOLS: &[&str] = &["uname", "mkdir", "mktemp", "rm", "install", "cp"];

#[cfg(unix)]
fn stub(dir: &Path, name: &str, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// A directory of links to the host's own `tools`, for a PATH that holds
/// those and nothing else.
#[cfg(unix)]
fn toolbox(dir: &Path, tools: &[&str]) -> String {
    std::fs::create_dir_all(dir).unwrap();
    for tool in tools {
        std::os::unix::fs::symlink(which::which(tool).unwrap(), dir.join(tool)).unwrap();
    }
    dir.display().to_string()
}

/// GNU tar unpacks `.tar.xz` through an `xz` on PATH; other tars need none.
#[cfg(unix)]
fn host_tar_lacks_its_xz() -> bool {
    let version = Command::new("tar").arg("--version").output().unwrap();
    String::from_utf8_lossy(&version.stdout).contains("GNU tar") && common::skip_without_tool("xz")
}

/// The install script against a stand-in `curl` that serves a local archive
/// of every shipped binary and a maintainer tool, with the stand-ins in
/// `stubs` ahead of `path`. `None` when the host's architecture has no
/// release archive.
#[cfg(unix)]
fn run_install_script(
    tmp: &Path,
    stubs: &Path,
    path: &str,
) -> Option<(std::process::Output, PathBuf)> {
    let triple = match std::env::consts::ARCH {
        "x86_64" => "x86_64",
        "aarch64" => "aarch64",
        other => {
            common::report_skip(&format!("install.sh does not support {other}"));
            return None;
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
    let archive = fake_archive(tmp, &file, &refs);

    std::fs::create_dir_all(stubs).unwrap();
    stub(
        stubs,
        "curl",
        &format!(
            "while [ $# -gt 0 ]; do case \"$1\" in -o) out=\"$2\"; shift 2;; *) shift;; esac; done\ncp '{}' \"$out\"\n",
            archive.display()
        ),
    );
    let dest = tmp.join("installed");
    let out = Command::new("/bin/sh")
        .arg(root().join("docs/install.sh"))
        .env("PATH", format!("{}:{path}", stubs.display()))
        .env("VERSION", "9.9.9")
        .env("INSTALL_DIR", &dest)
        .output()
        .unwrap();
    Some((out, dest))
}

#[cfg(unix)]
fn assert_installed_every_shipped_binary(out: &std::process::Output, dest: &Path) {
    assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let installed: BTreeSet<String> = std::fs::read_dir(dest)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(installed, shipped().into_iter().collect::<BTreeSet<_>>());
}

/// It must install every shipped binary and nothing else the archive holds.
#[test]
#[cfg(unix)]
fn the_install_script_installs_every_shipped_binary_from_the_archive() {
    if !wheel_builder_available() || host_tar_lacks_its_xz() {
        return;
    }
    let tmp = common::tempdir().unwrap();
    let Some((out, dest)) =
        run_install_script(tmp.path(), &tmp.path().join("stub"), "/usr/bin:/bin")
    else {
        return;
    };
    assert_installed_every_shipped_binary(&out, &dest);
}

/// macOS ships a tar that unpacks `.tar.xz` itself and no `xz`.
#[test]
#[cfg(unix)]
fn a_tar_that_unpacks_xz_itself_installs_with_no_xz_on_path() {
    if !wheel_builder_available() || host_tar_lacks_its_xz() {
        return;
    }
    let tmp = common::tempdir().unwrap();
    let stubs = tmp.path().join("stub");
    std::fs::create_dir_all(&stubs).unwrap();
    stub(
        &stubs,
        "tar",
        &format!(
            "if [ \"$1\" = --version ]; then echo 'bsdtar 3.7.4 - libarchive 3.7.4'; exit 0; fi\nPATH='{}' exec '{}' \"$@\"\n",
            std::env::var("PATH").unwrap(),
            which::which("tar").unwrap().display()
        ),
    );
    let path = toolbox(&tmp.path().join("tools"), INSTALL_SCRIPT_TOOLS);
    let Some((out, dest)) = run_install_script(tmp.path(), &stubs, &path) else {
        return;
    };
    assert_installed_every_shipped_binary(&out, &dest);
}

/// GNU tar without `xz` would fail mid-extraction naming neither; the script
/// refuses first and names the missing tool.
#[test]
#[cfg(unix)]
fn a_gnu_tar_with_no_xz_on_path_refuses_naming_xz() {
    if !wheel_builder_available() {
        return;
    }
    let tmp = common::tempdir().unwrap();
    let stubs = tmp.path().join("stub");
    std::fs::create_dir_all(&stubs).unwrap();
    stub(
        &stubs,
        "tar",
        "if [ \"$1\" = --version ]; then echo 'tar (GNU tar) 1.35'; exit 0; fi\necho 'extracted without xz' >&2; exit 2\n",
    );
    let path = toolbox(&tmp.path().join("tools"), INSTALL_SCRIPT_TOOLS);
    let Some((out, _)) = run_install_script(tmp.path(), &stubs, &path) else {
        return;
    };
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("need 'xz'"), "{stderr}");
}
