//! A member checkout rwv creates has its submodules checked out from the
//! copies of their repositories the canonical clone already holds, never from
//! the network, unless its manifest entry says `submodules = "skip"`.
//!
//! Every fixture cuts the network off: the upstream repositories are deleted
//! once the canonical clone has its submodules, and every URL that could
//! reach them points at a [`Trap`] that counts connections. A test passing
//! with the trap at zero is what shows nothing was fetched.

use std::io::Read;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use repoweave::workweave::scan_uninitialized_submodules;

mod common;

use common::LineEndings;

const PROJECT: &str = "proj";
const MEMBER: &str = "github/org/app";
const SUB: &str = "libs/sub";
const NESTED: &str = "libs/sub/deep";
const SUB_FILES: &[&str] = &["lib.txt", "src/a.txt"];

/// A local listener every remote URL in a fixture points at. A connection
/// is a fetch attempt.
struct Trap {
    url: String,
    hits: Arc<AtomicUsize>,
}

impl Trap {
    fn new() -> Trap {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let hits = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&hits);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                counted.fetch_add(1, Ordering::SeqCst);
                if let Ok(mut stream) = stream {
                    let _ = stream.read(&mut [0u8; 64]);
                }
            }
        });
        Trap { url, hits }
    }

    fn assert_untouched(&self) {
        assert_eq!(
            self.hits.load(Ordering::SeqCst),
            0,
            "something connected to the remote at {}: a fetch was attempted",
            self.url
        );
    }
}

/// `git` with `protocol.file.allow=always`, which `git submodule add` from a
/// local path needs and the code under test must never rely on, and with
/// `config`.
fn git_allowing_file_protocol(dir: &Path, config: &[(&str, &str)], args: &[&str]) {
    let settings: Vec<(&str, &str)> = std::iter::once(("protocol.file.allow", "always"))
        .chain(config.iter().copied())
        .collect();
    let output = common::git()
        .envs(common::git_config_env(&settings))
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@test.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@test.com")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} in {} failed:\n{}",
        dir.display(),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn init_repo(path: &Path, files: &[&str]) {
    std::fs::create_dir_all(path).unwrap();
    common::git_in(path, &["init", "-b", "main"]);
    for file in files {
        let at = path.join(file);
        std::fs::create_dir_all(at.parent().unwrap()).unwrap();
        std::fs::write(at, format!("{file}\n")).unwrap();
    }
    common::git_in(path, &["add", "."]);
    common::git_in(path, &["commit", "-m", "initial"]);
}

/// How the canonical clone's copy of the submodule is left once the network
/// is cut.
#[derive(Clone, Copy, PartialEq)]
enum Store {
    /// A full copy, its nested submodule included.
    Complete,
    /// A partial clone holding no file contents, whose promisor remote is
    /// the trap; the canonical clone's own checkout of it is emptied, so
    /// nothing local holds the files.
    WithoutContents,
}

struct Weave {
    _tmp: tempfile::TempDir,
    primary: PathBuf,
    trap: Trap,
    /// Over the machine's own config for every checkout the fixture and its
    /// seats make, and for every git run through [`Weave::git`].
    config: &'static [(&'static str, &'static str)],
}

impl Weave {
    fn canonical(&self) -> PathBuf {
        self.primary.join(MEMBER)
    }

    fn git(&self, dir: impl AsRef<Path>, args: &[&str]) -> String {
        common::git_in_configured(dir, self.config, args)
    }

    fn module_store(&self) -> PathBuf {
        self.canonical().join(".git/modules").join(SUB)
    }

    fn seat_path(&self, name: &str) -> PathBuf {
        self.primary
            .parent()
            .unwrap()
            .join(".workweaves")
            .join(format!("{PROJECT}--{name}"))
    }

    /// Create seat `name`; returns its root and create's stderr.
    fn seat(&self, name: &str) -> (PathBuf, String) {
        let output = common::rwv()
            .envs(common::git_config_env(self.config))
            .args(["workweave", PROJECT, "create", name])
            .current_dir(&self.primary)
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        assert!(output.status.success(), "create {name} failed:\n{stderr}");
        (self.seat_path(name), stderr)
    }

    fn findings(&self) -> Vec<serde_json::Value> {
        let output = common::rwv()
            .envs(common::git_config_env(self.config))
            .args(["doctor", "--json"])
            .current_dir(&self.primary)
            .output()
            .unwrap();
        let report: serde_json::Value =
            serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
                panic!(
                    "doctor --json printed no report ({e}):\n{}",
                    String::from_utf8_lossy(&output.stderr)
                )
            });
        report["violations"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|v| v["kind"] == "uninitialized-submodule")
            .collect()
    }
}

fn rwv(args: &[&str], cwd: &Path) -> String {
    let output = common::rwv().args(args).current_dir(cwd).output().unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.status.success(), "rwv {args:?} failed:\n{text}");
    text
}

/// A primary whose one project lists [`MEMBER`] with `role`, plus
/// `extra_entry_line` under its entry; the member records [`SUB`], which
/// records [`NESTED`].
fn weave(role: &str, extra_entry_line: &str, store: Store) -> Weave {
    configured_weave(role, extra_entry_line, store, &[])
}

/// [`weave`], with every checkout the fixture and its seats make writing
/// line endings as `endings` says, whatever the machine's config.
fn weave_checked_out(role: &str, endings: LineEndings) -> Weave {
    configured_weave(role, "", Store::Complete, endings.settings())
}

fn configured_weave(
    role: &str,
    extra_entry_line: &str,
    store: Store,
    config: &'static [(&'static str, &'static str)],
) -> Weave {
    let tmp = common::tempdir().unwrap();
    let trap = Trap::new();
    let upstream = tmp.path().join("upstream");
    let deep_up = upstream.join("deep");
    init_repo(&deep_up, &["deep.txt"]);
    let sub_up = upstream.join("sub");
    init_repo(&sub_up, SUB_FILES);
    common::git_in(&sub_up, &["config", "uploadpack.allowFilter", "true"]);
    git_allowing_file_protocol(
        &sub_up,
        config,
        &["submodule", "add", &common::file_url(&deep_up), "deep"],
    );
    common::git_in(&sub_up, &["commit", "-m", "add deep"]);

    let primary = tmp.path().join("primary");
    let canonical = primary.join(MEMBER);
    init_repo(&canonical, &["README"]);
    git_allowing_file_protocol(
        &canonical,
        config,
        &["submodule", "add", &common::file_url(&sub_up), SUB],
    );
    common::git_in(&canonical, &["commit", "-m", "add sub"]);
    git_allowing_file_protocol(
        &canonical,
        config,
        &["submodule", "update", "--init", "--recursive"],
    );

    let store_dir = canonical.join(".git/modules").join(SUB);
    if store == Store::WithoutContents {
        std::fs::remove_dir_all(&store_dir).unwrap();
        let partial = tmp.path().join("partial.git");
        git_allowing_file_protocol(
            tmp.path(),
            config,
            &[
                "clone",
                "--bare",
                "--filter=blob:none",
                &common::file_url(&sub_up),
                partial.to_str().unwrap(),
            ],
        );
        std::fs::rename(&partial, &store_dir).unwrap();
        common::git_in(&store_dir, &["config", "core.bare", "false"]);
        for entry in std::fs::read_dir(canonical.join(SUB)).unwrap() {
            let entry = entry.unwrap().path();
            if entry.file_name().unwrap() != ".git" {
                if entry.is_dir() {
                    std::fs::remove_dir_all(entry).unwrap();
                } else {
                    std::fs::remove_file(entry).unwrap();
                }
            }
        }
    }
    common::git_in(
        &canonical,
        &[
            "config",
            &format!("submodule.{SUB}.url"),
            &format!("{}/sub", trap.url),
        ],
    );
    common::git_in(
        &store_dir,
        &["config", "remote.origin.url", &format!("{}/sub", trap.url)],
    );
    let nested_store = store_dir.join("modules/deep");
    if nested_store.is_dir() {
        common::git_in(
            &nested_store,
            &["config", "remote.origin.url", &format!("{}/deep", trap.url)],
        );
    }
    std::fs::remove_dir_all(&upstream).unwrap();

    let sha = common::git_in(&canonical, &["rev-parse", "HEAD"]);
    let project_dir = primary.join("projects").join(PROJECT);
    std::fs::create_dir_all(&project_dir).unwrap();
    common::git_in(&project_dir, &["init", "-b", "main"]);
    std::fs::write(
        project_dir.join(".gitattributes"),
        "rwv.lock merge=rwv-ours\n",
    )
    .unwrap();
    std::fs::write(
        project_dir.join("rwv.toml"),
        format!(
            "[repositories.\"{MEMBER}\"]\ntype = \"git\"\nurl = \"file://{}\"\n\
             version = \"main\"\nrole = \"{role}\"\n{extra_entry_line}\n",
            common::url_path(&canonical)
        ),
    )
    .unwrap();
    common::fixture_lock(
        &project_dir,
        &[(MEMBER, common::file_url(&canonical).as_str(), sha.as_str())],
    );
    common::git_in(&project_dir, &["add", "."]);
    common::git_in(&project_dir, &["commit", "-m", "lock: initial"]);
    std::fs::write(primary.join(".rwv-active"), format!("{PROJECT}\n")).unwrap();
    Weave {
        _tmp: tmp,
        primary,
        trap,
        config,
    }
}

fn is_empty(dir: &Path) -> bool {
    std::fs::read_dir(dir).map_or(true, |mut entries| entries.next().is_none())
}

/// Whether this filesystem clones files: where it does, every submodule
/// file is placed from the canonical clone's checkout and none is written.
fn reflinks(dir: &Path) -> bool {
    let from = dir.join(".reflink-probe-from");
    let to = dir.join(".reflink-probe-to");
    std::fs::write(&from, "probe").unwrap();
    let cloned = reflink_copy::reflink(&from, &to).is_ok();
    let _ = std::fs::remove_file(&from);
    let _ = std::fs::remove_file(&to);
    cloned
}

/// The submodule and the one nested in it are checked out in `member` at the
/// commits the member records, from repositories of its own, clean, with the
/// remotes the canonical clone's copies have.
fn assert_checked_out(weave: &Weave, member: &Path, role: &str) {
    for file in SUB_FILES {
        let path = member.join(SUB).join(file);
        assert_eq!(
            common::read_normalized(&path),
            format!("{file}\n"),
            "{} should hold the submodule's content",
            path.display()
        );
    }
    assert_eq!(
        common::read_normalized(member.join(NESTED).join("deep.txt")),
        "deep.txt\n",
        "the nested submodule should be checked out too"
    );
    assert_eq!(
        weave.git(member.join(SUB), &["rev-parse", "HEAD"]),
        weave.git(member, &["rev-parse", &format!("HEAD:{SUB}")]),
    );
    assert_eq!(
        weave.git(member, &["status", "--porcelain"]),
        "",
        "the member should read clean, submodules included"
    );
    let store = weave.git(
        member.join(SUB),
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    );
    assert_ne!(
        repoweave::path_spelling::resolve(&PathBuf::from(&store)).unwrap(),
        repoweave::path_spelling::resolve(&weave.module_store()).unwrap(),
        "the submodule must have a repository of its own, not the canonical clone's"
    );
    assert_eq!(
        weave.git(member.join(SUB), &["config", "remote.origin.url"]),
        format!("{}/sub", weave.trap.url),
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(member.join(SUB).join(SUB_FILES[0]))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o222 == 0,
            role == "reference",
            "a submodule's files hold its member's role"
        );
    }
    #[cfg(not(unix))]
    let _ = role;
}

/// Remove the member's worktree as git does, so the next creation path has
/// it to create.
fn remove_member(weave: &Weave, seat: &Path) {
    common::git_in(
        weave.canonical(),
        &[
            "worktree",
            "remove",
            "--force",
            seat.join(MEMBER).to_str().unwrap(),
        ],
    );
}

/// `(reflinked, hardlinked, written)` from create's line for [`SUB`].
fn sub_placement(stderr: &str) -> (usize, usize, usize) {
    let line = stderr
        .lines()
        .find(|l| l.contains(&format!("{MEMBER}/{SUB}: submodule checked out")))
        .unwrap_or_else(|| panic!("create should report the submodule:\n{stderr}"));
    let count = |mode: &str| -> usize {
        line.split(['(', ','])
            .find_map(|part| part.trim().strip_suffix(mode)?.trim().parse().ok())
            .unwrap_or_else(|| panic!("no {mode} count in: {line}"))
    };
    (count("reflinked"), count("hardlinked"), count("written)"))
}

/// Every path the submodule's index records, gitlinks included.
fn sub_tracked(weave: &Weave, member: &Path) -> usize {
    weave.git(member.join(SUB), &["ls-files"]).lines().count()
}

/// Fixture check: the submodule's files in `member` are on disk as `endings`
/// writes them, so the arm under test is the one its name says.
fn assert_written_as(member: &Path, endings: LineEndings) {
    let path = member.join(SUB).join(SUB_FILES[0]);
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(
        bytes.ends_with(b"\r\n"),
        endings == LineEndings::Converted,
        "{} is not written as {endings:?} writes it: {bytes:?}",
        path.display()
    );
}

#[test]
fn workweave_create_checks_out_submodules_from_local_data_in_each_role() {
    for endings in [LineEndings::Verbatim, LineEndings::Converted] {
        for role in ["owned", "reference"] {
            let weave = weave_checked_out(role, endings);
            let store_config = std::fs::read(weave.module_store().join("config")).unwrap();

            let (seat, stderr) = weave.seat("one");

            assert_checked_out(&weave, &seat.join(MEMBER), role);
            assert_written_as(&seat.join(MEMBER), endings);
            let (reflinked, hardlinked, written) = sub_placement(&stderr);
            match endings {
                LineEndings::Verbatim if reflinks(&seat) => assert_eq!(
                    reflinked,
                    SUB_FILES.len() + 1,
                    "every file, .gitmodules included, is cloned from the canonical checkout"
                ),
                LineEndings::Verbatim => {}
                LineEndings::Converted => assert_eq!(
                    (reflinked, hardlinked, written),
                    (0, 0, sub_tracked(&weave, &seat.join(MEMBER))),
                    "a checkout that converts line endings writes every file, as {role}"
                ),
            }
            assert_eq!(
                std::fs::read(weave.module_store().join("config")).unwrap(),
                store_config,
                "the canonical clone's copy is read, never written"
            );
            assert!(weave.findings().is_empty(), "{:?}", weave.findings());
            weave.trap.assert_untouched();
        }
    }
}

#[test]
fn fetch_into_a_workweave_checks_out_the_submodules_of_the_member_it_creates() {
    for role in ["owned", "reference"] {
        let weave = weave(role, "", Store::Complete);
        let (seat, _) = weave.seat("one");
        remove_member(&weave, &seat);

        rwv(&["fetch"], &seat);

        assert_checked_out(&weave, &seat.join(MEMBER), role);
        weave.trap.assert_untouched();
    }
}

#[test]
fn doctor_fix_checks_out_the_submodules_of_the_member_it_creates() {
    for role in ["owned", "reference"] {
        let weave = weave(role, "", Store::Complete);
        let (seat, _) = weave.seat("one");
        remove_member(&weave, &seat);

        rwv(&["doctor", "--fix"], &seat);

        assert_checked_out(&weave, &seat.join(MEMBER), role);
        weave.trap.assert_untouched();
    }
}

#[test]
fn doctor_fix_checks_out_the_empty_submodules_of_an_existing_member() {
    for role in ["owned", "reference"] {
        let weave = weave(role, "", Store::Complete);
        let (seat, _) = weave.seat("one");
        let sub = seat.join(MEMBER).join(SUB);
        let repository =
            common::git_in(&sub, &["rev-parse", "--path-format=absolute", "--git-dir"]);
        std::fs::remove_dir_all(&sub).unwrap();
        std::fs::create_dir(&sub).unwrap();
        std::fs::remove_dir_all(&repository).unwrap();
        let findings = weave.findings();
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert_eq!(findings[0]["sub_kind"], "fetched");

        rwv(&["doctor", "--fix"], &weave.primary);

        assert_checked_out(&weave, &seat.join(MEMBER), role);
        assert!(weave.findings().is_empty(), "{:?}", weave.findings());
        weave.trap.assert_untouched();
    }
}

#[test]
fn a_member_that_skips_submodules_keeps_them_empty_and_unreported() {
    let weave = weave("owned", "submodules = \"skip\"", Store::Complete);
    let (seat, _) = weave.seat("one");
    let member = seat.join(MEMBER);

    assert!(
        is_empty(&member.join(SUB)),
        "the opt-out leaves the submodule empty"
    );
    assert_eq!(scan_uninitialized_submodules(&member), vec![SUB.to_owned()]);
    assert!(weave.findings().is_empty(), "{:?}", weave.findings());

    rwv(&["doctor", "--fix"], &weave.primary);
    assert!(is_empty(&member.join(SUB)), "--fix honors the opt-out too");
    weave.trap.assert_untouched();
}

#[test]
fn a_misspelled_opt_out_is_refused_rather_than_ignored() {
    let weave = weave("owned", "submodule = \"skip\"", Store::Complete);
    let output = common::rwv()
        .args(["workweave", PROJECT, "create", "one"])
        .current_dir(&weave.primary)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "a misspelled key must not parse");
    assert!(stderr.contains("submodule"), "{stderr}");
}

#[test]
fn a_recorded_commit_the_canonical_copy_lacks_stays_uninitialized_and_reported() {
    let weave = weave("owned", "", Store::Complete);
    let canonical = weave.canonical();
    let absent = "1234567890abcdef1234567890abcdef12345678";
    common::git_in(
        &canonical,
        &[
            "update-index",
            "--cacheinfo",
            &format!("160000,{absent},{SUB}"),
        ],
    );
    common::git_in(
        &canonical,
        &["commit", "-m", "bump sub to an unfetched commit"],
    );
    let sha = common::git_in(&canonical, &["rev-parse", "HEAD"]);
    let project_dir = weave.primary.join("projects").join(PROJECT);
    common::fixture_lock(
        &project_dir,
        &[(MEMBER, common::file_url(&canonical).as_str(), sha.as_str())],
    );
    common::git_in(&project_dir, &["commit", "-am", "lock: bump"]);

    let (seat, stderr) = weave.seat("one");

    assert!(is_empty(&seat.join(MEMBER).join(SUB)));
    assert!(
        stderr.contains(&format!("{MEMBER}/{SUB}: submodule left uninitialized")),
        "{stderr}"
    );
    let findings = weave.findings();
    assert_eq!(findings.len(), 1, "{findings:?}");
    assert_eq!(findings[0]["sub_kind"], "unfetched");
    rwv(&["doctor", "--fix"], &weave.primary);
    assert!(is_empty(&seat.join(MEMBER).join(SUB)));
    weave.trap.assert_untouched();
}

#[test]
fn a_partial_copy_is_never_asked_for_a_commit_it_lacks() {
    let weave = weave("owned", "", Store::WithoutContents);
    let canonical = weave.canonical();
    let absent = "1234567890abcdef1234567890abcdef12345678";
    common::git_in(
        &canonical,
        &[
            "update-index",
            "--cacheinfo",
            &format!("160000,{absent},{SUB}"),
        ],
    );
    common::git_in(
        &canonical,
        &["commit", "-m", "bump sub to an unfetched commit"],
    );
    let sha = common::git_in(&canonical, &["rev-parse", "HEAD"]);
    let project_dir = weave.primary.join("projects").join(PROJECT);
    common::fixture_lock(
        &project_dir,
        &[(MEMBER, common::file_url(&canonical).as_str(), sha.as_str())],
    );
    common::git_in(&project_dir, &["commit", "-am", "lock: bump"]);

    let (seat, _) = weave.seat("one");

    assert!(is_empty(&seat.join(MEMBER).join(SUB)));
    assert_eq!(weave.findings()[0]["sub_kind"], "unfetched");
    weave.trap.assert_untouched();
}

#[test]
fn a_partial_copy_is_never_asked_for_contents_it_lacks() {
    let weave = weave("owned", "", Store::WithoutContents);

    let (seat, stderr) = weave.seat("one");

    let sub = seat.join(MEMBER).join(SUB);
    assert!(is_empty(&sub), "a checkout missing contents is undone");
    assert!(
        !Path::new(&common::git_in(
            seat.join(MEMBER),
            &[
                "rev-parse",
                "--path-format=absolute",
                "--git-path",
                &format!("modules/{SUB}")
            ],
        ))
        .exists(),
        "the repository made for it is removed with it"
    );
    assert!(
        stderr.contains(&format!("{MEMBER}/{SUB}: submodule left uninitialized")),
        "{stderr}"
    );
    weave.trap.assert_untouched();
}

#[test]
fn a_second_seat_places_submodule_files_from_the_first() {
    for endings in [LineEndings::Verbatim, LineEndings::Converted] {
        let weave = weave_checked_out("reference", endings);
        let (one, _) = weave.seat("one");
        let (two, stderr) = weave.seat("two");

        assert_checked_out(&weave, &two.join(MEMBER), "reference");
        assert_written_as(&two.join(MEMBER), endings);
        let (reflinked, hardlinked, written) = sub_placement(&stderr);
        match endings {
            LineEndings::Verbatim if cfg!(target_os = "linux") => assert_eq!(
                reflinked + hardlinked,
                SUB_FILES.len() + 1,
                "every file is cloned, or linked to a read-only reference copy"
            ),
            LineEndings::Verbatim => {}
            LineEndings::Converted => assert_eq!(
                (reflinked, hardlinked, written),
                (0, 0, sub_tracked(&weave, &two.join(MEMBER))),
                "a checkout that converts line endings writes every file"
            ),
        }
        assert!(one.join(MEMBER).join(SUB).join(SUB_FILES[0]).is_file());
        weave.trap.assert_untouched();
    }
}

#[test]
fn deleting_the_workweave_removes_the_submodule_repositories_it_made() {
    for role in ["owned", "reference"] {
        let weave = weave(role, "", Store::Complete);
        let (seat, _) = weave.seat("one");
        let repository = PathBuf::from(common::git_in(
            seat.join(MEMBER).join(SUB),
            &["rev-parse", "--path-format=absolute", "--git-dir"],
        ));
        assert!(repository.is_dir());

        rwv(&["workweave", PROJECT, "delete", "one"], &weave.primary);

        assert!(!seat.exists(), "the workweave should be gone");
        assert!(
            !repository.exists(),
            "{} should go with the member worktree it belongs to",
            repository.display()
        );
        assert_checked_out_in_canonical(&weave);
    }
}

/// The canonical clone's own checkout of the submodule still reads as git
/// left it.
fn assert_checked_out_in_canonical(weave: &Weave) {
    assert_eq!(
        common::git_in(weave.canonical(), &["status", "--porcelain"]),
        "",
        "the canonical clone, submodules included, should read clean"
    );
    assert!(weave.canonical().join(SUB).join(SUB_FILES[0]).is_file());
}

#[test]
fn create_leaves_a_repo_without_submodules_alone() {
    let tmp = common::tempdir().unwrap();
    let primary = tmp.path().join("primary");
    let canonical = primary.join(MEMBER);
    init_repo(&canonical, &["README"]);
    let sha = common::git_in(&canonical, &["rev-parse", "HEAD"]);
    let project_dir = primary.join("projects").join(PROJECT);
    std::fs::create_dir_all(&project_dir).unwrap();
    common::git_in(&project_dir, &["init", "-b", "main"]);
    std::fs::write(
        project_dir.join(".gitattributes"),
        "rwv.lock merge=rwv-ours\n",
    )
    .unwrap();
    std::fs::write(
        project_dir.join("rwv.toml"),
        format!(
            "[repositories.\"{MEMBER}\"]\ntype = \"git\"\nurl = \"file://{}\"\n\
             version = \"main\"\nrole = \"owned\"\n",
            common::url_path(&canonical)
        ),
    )
    .unwrap();
    common::fixture_lock(
        &project_dir,
        &[(MEMBER, common::file_url(&canonical).as_str(), sha.as_str())],
    );
    common::git_in(&project_dir, &["add", "."]);
    common::git_in(&project_dir, &["commit", "-m", "lock: initial"]);
    std::fs::write(primary.join(".rwv-active"), format!("{PROJECT}\n")).unwrap();

    let output = common::rwv()
        .args(["workweave", PROJECT, "create", "one"])
        .current_dir(&primary)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(!stderr.contains("submodule"), "{stderr}");
}

#[test]
fn scan_no_gitmodules_returns_empty() {
    let tmp = common::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    assert!(scan_uninitialized_submodules(&repo).is_empty());
}

#[test]
fn scan_reports_an_empty_or_absent_submodule_dir_and_not_a_populated_one() {
    let tmp = common::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(repo.join("libs/empty")).unwrap();
    std::fs::create_dir_all(repo.join("libs/full")).unwrap();
    std::fs::write(repo.join("libs/full/lib.h"), "// content").unwrap();
    std::fs::write(
        repo.join(".gitmodules"),
        "[submodule \"empty\"]\n\tpath = libs/empty\n\turl = https://example.com/e\n\
         [submodule \"full\"]\n\tpath = libs/full\n\turl = https://example.com/f\n\
         [submodule \"absent\"]\n\tpath = vendor/absent\n\turl = https://example.com/a\n",
    )
    .unwrap();
    assert_eq!(
        scan_uninitialized_submodules(&repo),
        vec!["libs/empty".to_owned(), "vendor/absent".to_owned()]
    );
}
