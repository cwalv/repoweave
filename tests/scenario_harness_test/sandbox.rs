use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use repoweave::manifest::Role;

use crate::common;

pub const IDENTITY: &[(&str, &str)] = &[
    ("user.name", "Sandbox"),
    ("user.email", "sandbox@example.invalid"),
];

/// The key a member entry declares its branch under. The seed and every hand
/// edit of an entry spell it from here.
pub const DECLARED_BRANCH_KEY: &str = "branch";

/// The `.gitattributes` line every seeded weave branch commits for its lock.
pub const LOCK_MERGE_ATTRIBUTE: &str = "rwv.lock merge=rwv-ours";

pub const RECORDS_BRANCH: &str = "rwv-records";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Repo {
    Proj,
    Proj2,
    App,
    Lib,
    Docs,
}

impl Repo {
    pub const MEMBERS: [Repo; 3] = [Repo::App, Repo::Lib, Repo::Docs];
    pub const ALL: [Repo; 5] = [Repo::Proj, Repo::Proj2, Repo::App, Repo::Lib, Repo::Docs];

    pub fn name(self) -> &'static str {
        match self {
            Repo::Proj => "proj",
            Repo::Proj2 => "proj2",
            Repo::App => "app",
            Repo::Lib => "lib",
            Repo::Docs => "docs",
        }
    }

    pub fn is_project(self) -> bool {
        matches!(self, Repo::Proj | Repo::Proj2)
    }

    /// Where a checkout of this repo sits below a weave root or a workweave
    /// root, `/`-separated; for a member, its key in `rwv.toml`.
    pub fn rel_path(self) -> String {
        if self.is_project() {
            format!("projects/{}", self.name())
        } else {
            format!("local/remotes/{}", self.name())
        }
    }

    pub fn from_member_key(key: &str) -> Repo {
        Repo::MEMBERS
            .into_iter()
            .find(|repo| repo.rel_path() == key)
            .unwrap_or_else(|| panic!("no sandbox member is keyed {key:?}"))
    }
}

/// The text the seed writes for the end of `member`'s url line and its
/// declared branch, unique in a seeded `rwv.toml`: the anchor a hand edit of
/// that member's declared branch replaces.
pub fn declared_branch_text(member: Repo, branch: &str) -> String {
    format!(
        "{}.git\"\n{DECLARED_BRANCH_KEY} = \"{branch}\"",
        member.name()
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Machine {
    A,
    B,
    C,
}

impl Machine {
    pub const ALL: [Machine; 3] = [Machine::A, Machine::B, Machine::C];

    fn dir_name(self) -> &'static str {
        match self {
            Machine::A => "machine-a",
            Machine::B => "machine-b",
            Machine::C => "machine-c",
        }
    }
}

/// Where a step runs: a machine's primary or one of its workweaves, or the
/// forge's own clone of each remote, which belongs to no machine and whose
/// refs no invariant reads.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Place {
    Primary(Machine),
    Workweave(Machine, Repo, String),
    Forge,
}

impl Place {
    pub fn ww(machine: Machine, name: &str) -> Place {
        Place::Workweave(machine, Repo::Proj, name.to_owned())
    }

    pub fn machine(&self) -> Option<Machine> {
        match self {
            Place::Primary(m) | Place::Workweave(m, _, _) => Some(*m),
            Place::Forge => None,
        }
    }
}

/// Which of a repo's bare remotes. Every repo has its origin; a member also
/// has the rest when the seed asks for them, each a copy of its origin at
/// seed time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RemoteKind {
    Origin,
    /// A second remote someone forked the original into, which moves only
    /// when pushed to.
    Fork,
    /// Takes every branch and tag of the origin when the forge syncs it, and
    /// keeps the branches the origin no longer has.
    KeepingMirror,
    /// Becomes an exact copy of the origin's branches and tags when the forge
    /// syncs it.
    PruningMirror,
}

impl RemoteKind {
    pub const ALL: [RemoteKind; 4] = [
        RemoteKind::Origin,
        RemoteKind::Fork,
        RemoteKind::KeepingMirror,
        RemoteKind::PruningMirror,
    ];

    /// Distinct per kind, so each kind's URLs have an owner segment of their
    /// own.
    fn dir_name(self) -> &'static str {
        match self {
            RemoteKind::Origin => "remotes",
            RemoteKind::Fork => "fork",
            RemoteKind::KeepingMirror => "mirror-keeping",
            RemoteKind::PruningMirror => "mirror-pruning",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Remote {
    pub repo: Repo,
    pub kind: RemoteKind,
}

impl Remote {
    pub fn origin(repo: Repo) -> Remote {
        Remote {
            repo,
            kind: RemoteKind::Origin,
        }
    }

    pub fn fork(repo: Repo) -> Remote {
        Remote {
            repo,
            kind: RemoteKind::Fork,
        }
    }

    pub fn keeping_mirror(repo: Repo) -> Remote {
        Remote {
            repo,
            kind: RemoteKind::KeepingMirror,
        }
    }

    pub fn pruning_mirror(repo: Repo) -> Remote {
        Remote {
            repo,
            kind: RemoteKind::PruningMirror,
        }
    }
}

/// A repository holding refs for some repo of the sandbox: one of its
/// remotes, or a machine's canonical clone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Store {
    Remote(RemoteKind),
    Clone(Machine),
}

/// What a second project shares with `proj`: `lib`, declared on `branch` and
/// pinned at `lib`'s first commit.
#[derive(Debug, Clone, Copy)]
pub struct SecondProject {
    pub branch: &'static str,
}

/// What a sandbox holds beyond its fixed shape.
#[derive(Debug, Clone, Default)]
pub struct Seed {
    pub second_project: Option<SecondProject>,
    /// Members seeded with a role other than `owned`.
    pub roles: Vec<(Repo, Role)>,
    pub forks: bool,
    pub mirrors: bool,
    /// Remotes whose `update` hook refuses `refs/heads/rwv-records`.
    pub refusing_records: Vec<Remote>,
}

impl Seed {
    pub fn second_project(mut self, second: SecondProject) -> Seed {
        self.second_project = Some(second);
        self
    }

    pub fn role(mut self, member: Repo, role: Role) -> Seed {
        self.roles.push((member, role));
        self
    }

    pub fn forks(mut self) -> Seed {
        self.forks = true;
        self
    }

    pub fn mirrors(mut self) -> Seed {
        self.mirrors = true;
        self
    }

    pub fn refusing_records(mut self, remote: Remote) -> Seed {
        self.refusing_records.push(remote);
        self
    }

    fn role_of(&self, member: Repo) -> Role {
        self.roles
            .iter()
            .rev()
            .find(|(m, _)| *m == member)
            .map_or(Role::Owned, |(_, role)| *role)
    }
}

/// An `update` hook refusing the records branch and accepting every other
/// ref, so a push carrying both lands the rest.
fn refuse_records_hook() -> String {
    let records = format!("refs/heads/{RECORDS_BRANCH}");
    format!(
        "#!/bin/sh\nif [ \"$1\" = {records} ]; then\n    echo \"this remote refuses {records}\" >&2\n    exit 1\nfi\nexit 0\n"
    )
}

/// A hermetic set of remotes and the machines that clone them.
///
/// Every origin is a bare repository on branch `main`; `proj` declares `app`,
/// `lib` and `docs` on `main`, each `owned` unless the [`Seed`] gives it
/// another role, and commits an `rwv.lock` pinning each origin's `main`.
/// `HOME` points into the sandbox for every git and rwv it runs, so no config
/// of the host reaches a commit, and the identity comes from [`IDENTITY`].
/// `lib` carries a second commit on `main` and a `dev` branch, so a project
/// can be pinned behind or declared elsewhere; nothing else differs between
/// members. Forks and mirrors, when seeded, exist for members only, and every
/// `url` the seed writes names an origin.
pub struct Sandbox {
    _tmp: tempfile::TempDir,
    pub root: PathBuf,
    home: PathBuf,
    remotes: Vec<Remote>,
}

impl Sandbox {
    pub fn new(seed: Seed) -> Sandbox {
        let tmp = common::tempdir().expect("a sandbox root");
        let root = tmp.path().to_path_buf();
        let home = root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let mut sandbox = Sandbox {
            _tmp: tmp,
            root,
            home,
            remotes: Vec::new(),
        };
        for member in Repo::MEMBERS {
            sandbox.seed_member(member);
        }
        sandbox.seed_project(
            Repo::Proj,
            &[
                (Repo::App, "main"),
                (Repo::Lib, "main"),
                (Repo::Docs, "main"),
            ],
            &seed,
            false,
        );
        if let Some(second) = seed.second_project {
            sandbox.seed_project(Repo::Proj2, &[(Repo::Lib, second.branch)], &seed, true);
        }
        for member in Repo::MEMBERS {
            if seed.forks {
                sandbox.copy_origin(Remote::fork(member));
            }
            if seed.mirrors {
                sandbox.copy_origin(Remote::keeping_mirror(member));
                sandbox.copy_origin(Remote::pruning_mirror(member));
            }
        }
        for remote in &seed.refusing_records {
            sandbox.refuse_records(*remote);
        }
        sandbox
    }

    pub fn remotes(&self) -> &[Remote] {
        &self.remotes
    }

    pub fn has_remote(&self, remote: Remote) -> bool {
        self.remotes.contains(&remote)
    }

    /// Where `remote` lives, and where its URL points.
    pub fn remote_path(&self, remote: Remote) -> PathBuf {
        self.root
            .join(remote.kind.dir_name())
            .join(format!("{}.git", remote.repo.name()))
    }

    pub fn remote_url(&self, remote: Remote) -> String {
        common::file_url(self.remote_path(remote))
    }

    pub fn remote_for_url(&self, url: &str) -> Option<Remote> {
        self.remotes
            .iter()
            .copied()
            .find(|remote| self.remote_url(*remote) == url)
    }

    fn unreachable_path(&self, remote: Remote) -> PathBuf {
        self.root
            .join("unreachable")
            .join(remote.kind.dir_name())
            .join(format!("{}.git", remote.repo.name()))
    }

    /// The repository holding `remote`'s refs now: where it lives, or where
    /// [`Sandbox::unreachable`] moved it.
    pub fn remote_store(&self, remote: Remote) -> PathBuf {
        let aside = self.unreachable_path(remote);
        if aside.is_dir() {
            aside
        } else {
            self.remote_path(remote)
        }
    }

    pub fn is_reachable(&self, remote: Remote) -> bool {
        !self.unreachable_path(remote).is_dir()
    }

    /// Moves `remote` away from its URL until the guard drops, so every fetch
    /// and push to it fails while its refs stay where the invariants read
    /// them.
    pub fn unreachable(&self, remote: Remote) -> Unreachable<'_> {
        assert!(self.has_remote(remote), "{remote:?} was not seeded");
        let aside = self.unreachable_path(remote);
        std::fs::create_dir_all(aside.parent().unwrap()).unwrap();
        std::fs::rename(self.remote_path(remote), &aside)
            .unwrap_or_else(|e| panic!("cannot take {remote:?} offline: {e}"));
        Unreachable {
            sandbox: self,
            remote,
        }
    }

    /// Installs an `update` hook on `remote` refusing `refs/heads/rwv-records`.
    /// Git for Windows runs a hook through its own `sh` by its `#!` line, so
    /// the script needs no executable bit there.
    pub fn refuse_records(&self, remote: Remote) {
        let hooks = self.remote_store(remote).join("hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        let hook = hooks.join("update");
        std::fs::write(&hook, refuse_records_hook()).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    pub fn weave_root(&self, machine: Machine) -> PathBuf {
        self.root.join(machine.dir_name()).join("weave")
    }

    pub fn workweave_container(&self, machine: Machine) -> PathBuf {
        self.root.join(machine.dir_name()).join(".workweaves")
    }

    pub fn place_root(&self, place: &Place) -> PathBuf {
        match place {
            Place::Primary(m) => self.weave_root(*m),
            Place::Workweave(m, project, name) => self
                .workweave_container(*m)
                .join(format!("{}--{name}", project.name())),
            Place::Forge => self.root.join("forge"),
        }
    }

    pub fn checkout(&self, place: &Place, repo: Repo) -> PathBuf {
        match place {
            Place::Forge => self.place_root(place).join(repo.name()),
            _ => common::under(self.place_root(place), &repo.rel_path()),
        }
    }

    /// The repository whose object store and refs back every checkout of
    /// `repo` on `machine`: the canonical clone, which each workweave's
    /// worktree shares.
    pub fn canonical(&self, machine: Machine, repo: Repo) -> PathBuf {
        self.checkout(&Place::Primary(machine), repo)
    }

    pub fn machines(&self) -> Vec<Machine> {
        Machine::ALL
            .into_iter()
            .filter(|m| self.weave_root(*m).join("projects").is_dir())
            .collect()
    }

    pub fn workweaves(&self, machine: Machine) -> Vec<Place> {
        let Ok(entries) = std::fs::read_dir(self.workweave_container(machine)) else {
            return Vec::new();
        };
        let mut places: Vec<Place> = entries
            .filter_map(Result::ok)
            .filter(|e| e.path().join(".rwv-workweave").is_file())
            .filter_map(|e| {
                let file_name = e.file_name().to_string_lossy().into_owned();
                let (project, name) = file_name.split_once("--")?;
                let project = [Repo::Proj, Repo::Proj2]
                    .into_iter()
                    .find(|p| p.name() == project)?;
                Some(Place::Workweave(machine, project, name.to_owned()))
            })
            .collect();
        places.sort();
        places
    }

    pub fn live_places(&self) -> Vec<Place> {
        self.machines()
            .into_iter()
            .flat_map(|m| std::iter::once(Place::Primary(m)).chain(self.workweaves(m)))
            .collect()
    }

    /// Every repository holding refs for `repo`: each of its remotes and
    /// each machine's canonical clone.
    pub fn stores(&self, repo: Repo) -> Vec<Store> {
        let remotes = self
            .remotes
            .iter()
            .filter(|r| r.repo == repo)
            .map(|r| Store::Remote(r.kind));
        let clones = self
            .machines()
            .into_iter()
            .filter(|m| self.canonical(*m, repo).is_dir())
            .map(Store::Clone);
        remotes.chain(clones).collect()
    }

    pub fn store_dir(&self, repo: Repo, store: Store) -> PathBuf {
        match store {
            Store::Remote(kind) => self.remote_store(Remote { repo, kind }),
            Store::Clone(machine) => self.canonical(machine, repo),
        }
    }

    pub fn ref_stores(&self, repo: Repo) -> Vec<PathBuf> {
        self.stores(repo)
            .into_iter()
            .map(|store| self.store_dir(repo, store))
            .collect()
    }

    fn hermetic(&self, mut cmd: Command) -> Command {
        cmd.env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join(".config"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .envs(common::git_config_env(IDENTITY));
        cmd
    }

    pub fn git(&self, dir: &Path, args: &[&str]) -> Output {
        self.hermetic(common::git())
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap_or_else(|e| panic!("git {args:?} in {} failed to start: {e}", dir.display()))
    }

    /// [`Sandbox::git`] that must succeed, returning trimmed stdout.
    pub fn git_ok(&self, dir: &Path, args: &[&str]) -> String {
        let out = self.git(dir, args);
        assert!(
            out.status.success(),
            "git {args:?} in {} failed:\n{}",
            dir.display(),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout)
            .expect("git output is UTF-8")
            .trim()
            .to_owned()
    }

    /// [`Sandbox::git`] that must succeed, returning stdout untouched.
    pub fn git_ok_raw(&self, dir: &Path, args: &[&str]) -> Vec<u8> {
        let out = self.git(dir, args);
        assert!(
            out.status.success(),
            "git {args:?} in {} failed:\n{}",
            dir.display(),
            String::from_utf8_lossy(&out.stderr)
        );
        out.stdout
    }

    pub fn git_with_stdin(&self, dir: &Path, args: &[&str], stdin: &[u8]) -> String {
        use std::io::Write;
        let mut child = self
            .hermetic(common::git())
            .args(args)
            .current_dir(dir)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap_or_else(|e| panic!("git {args:?} failed to start: {e}"));
        child.stdin.take().unwrap().write_all(stdin).unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "git {args:?} in {} failed:\n{}",
            dir.display(),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).expect("git output is UTF-8")
    }

    /// Commits `files`, each a `/`-separated path and its content, over
    /// `branch`'s tip in the repository at `dir`, or as a root commit when
    /// `branch` does not exist, without touching any working tree or index:
    /// the commit a hand edit made with git's plumbing would be. Returns it.
    pub fn hand_commit(&self, dir: &Path, branch: &str, files: &[(String, String)]) -> String {
        let refname = format!("refs/heads/{branch}");
        let tip = self.git(dir, &["rev-parse", "--verify", "-q", &refname]);
        let tip = tip
            .status
            .success()
            .then(|| String::from_utf8(tip.stdout).unwrap().trim().to_owned());
        let base = tip.as_ref().map(|t| format!("{t}^{{tree}}"));
        let files: Vec<(&str, &str)> = files
            .iter()
            .map(|(path, content)| (path.as_str(), content.as_str()))
            .collect();
        let tree = self.overlay_tree(dir, base.as_deref(), &files);
        let mut args = vec!["commit-tree", tree.as_str(), "-m", "hand commit"];
        if let Some(parent) = &tip {
            args.extend(["-p", parent.as_str()]);
        }
        let commit = self.git_ok(dir, &args);
        let old = tip.unwrap_or_default();
        self.git_ok(dir, &["update-ref", &refname, &commit, &old]);
        commit
    }

    fn overlay_tree(&self, dir: &Path, base: Option<&str>, files: &[(&str, &str)]) -> String {
        let mut entries: std::collections::BTreeMap<String, (String, String, String)> = base
            .map(|tree| self.git_ok_raw(dir, &["ls-tree", "-z", tree]))
            .unwrap_or_default()
            .split(|b| *b == 0)
            .filter(|line| !line.is_empty())
            .map(|line| {
                let line = String::from_utf8(line.to_vec()).unwrap();
                let (meta, name) = line.split_once('\t').unwrap();
                let mut meta = meta.split(' ').map(str::to_owned);
                let (mode, kind, oid) = (
                    meta.next().unwrap(),
                    meta.next().unwrap(),
                    meta.next().unwrap(),
                );
                (name.to_owned(), (mode, kind, oid))
            })
            .collect();
        let mut subdirs: std::collections::BTreeMap<&str, Vec<(&str, &str)>> =
            std::collections::BTreeMap::new();
        for (path, content) in files {
            match path.split_once('/') {
                Some((first, rest)) => subdirs.entry(first).or_default().push((rest, content)),
                None => {
                    let oid = self
                        .git_with_stdin(dir, &["hash-object", "-w", "--stdin"], content.as_bytes())
                        .trim()
                        .to_owned();
                    entries.insert(
                        (*path).to_owned(),
                        ("100644".to_owned(), "blob".to_owned(), oid),
                    );
                }
            }
        }
        for (name, nested) in subdirs {
            let base = entries
                .get(name)
                .filter(|(_, kind, _)| kind == "tree")
                .map(|(_, _, oid)| oid.clone());
            let oid = self.overlay_tree(dir, base.as_deref(), &nested);
            entries.insert(
                name.to_owned(),
                ("040000".to_owned(), "tree".to_owned(), oid),
            );
        }
        let listing: String = entries
            .iter()
            .map(|(name, (mode, kind, oid))| format!("{mode} {kind} {oid}\t{name}\0"))
            .collect();
        self.git_with_stdin(dir, &["mktree", "-z"], listing.as_bytes())
            .trim()
            .to_owned()
    }

    /// The hermetic `rwv` command for `dir`, for a caller that spawns it
    /// itself.
    pub fn rwv_command(&self, dir: &Path, args: &[&str]) -> Command {
        let mut cmd = self.hermetic(common::rwv_std());
        cmd.args(args).current_dir(dir);
        cmd
    }

    pub fn rwv(&self, dir: &Path, args: &[&str]) -> Output {
        self.rwv_command(dir, args)
            .output()
            .unwrap_or_else(|e| panic!("rwv {args:?} in {} failed to start: {e}", dir.display()))
    }

    pub fn fetch_command(&self, project: Repo) -> Vec<String> {
        vec!["fetch".to_owned(), self.remote_url(Remote::origin(project))]
    }

    /// An empty weave root for `machine`, which `rwv fetch` then fills as a
    /// second computer would be set up.
    pub fn prepare_machine(&self, machine: Machine) -> PathBuf {
        let root = self.weave_root(machine);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn seed_dir(&self, repo: Repo) -> PathBuf {
        self.root.join("seed").join(repo.name())
    }

    fn init_remote(&mut self, repo: Repo) -> PathBuf {
        let origin = Remote::origin(repo);
        let path = self.remote_path(origin);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        self.git_ok(
            path.parent().unwrap(),
            &[
                "init",
                "--bare",
                "--initial-branch=main",
                path.to_str().unwrap(),
            ],
        );
        self.remotes.push(origin);
        let seed = self.seed_dir(repo);
        std::fs::create_dir_all(seed.parent().unwrap()).unwrap();
        self.git_ok(
            seed.parent().unwrap(),
            &["clone", path.to_str().unwrap(), seed.to_str().unwrap()],
        );
        seed
    }

    fn copy_origin(&mut self, remote: Remote) {
        let path = self.remote_path(remote);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let origin = self.remote_path(Remote::origin(remote.repo));
        self.git_ok(
            path.parent().unwrap(),
            &[
                "clone",
                "--bare",
                "--quiet",
                origin.to_str().unwrap(),
                path.to_str().unwrap(),
            ],
        );
        self.git_ok(&path, &["remote", "remove", "origin"]);
        self.remotes.push(remote);
    }

    fn seed_commit(&self, seed: &Path, file: &str, content: &str, message: &str) {
        std::fs::write(seed.join(file), content).unwrap();
        self.git_ok(seed, &["add", file]);
        self.git_ok(seed, &["commit", "-m", message]);
    }

    fn seed_member(&mut self, repo: Repo) {
        let seed = self.init_remote(repo);
        self.seed_commit(&seed, "README", &format!("{}\n", repo.name()), "initial");
        if repo == Repo::Lib {
            self.git_ok(&seed, &["branch", "dev"]);
            self.seed_commit(&seed, "lib.txt", "second\n", "lib: second");
            self.git_ok(&seed, &["push", "origin", "dev"]);
        }
        self.git_ok(&seed, &["push", "origin", "main"]);
    }

    fn seed_project(
        &mut self,
        project: Repo,
        members: &[(Repo, &str)],
        spec: &Seed,
        pin_first_commit: bool,
    ) {
        let seed = self.init_remote(project);
        let mut manifest =
            String::from("[integrations.vscode-workspace]\nenabled = false\n\n[repositories]\n");
        let mut pins = Vec::new();
        for (member, branch) in members {
            let url = self.remote_url(Remote::origin(*member));
            manifest.push_str(&format!(
                "\n[repositories.\"{key}\"]\ntype = \"git\"\nurl = \"{url}\"\n{DECLARED_BRANCH_KEY} = \"{branch}\"\nrole = \"{role}\"\n",
                key = member.rel_path(),
                role = spec.role_of(*member),
            ));
            let tip = format!("refs/heads/{branch}");
            let origin = self.remote_path(Remote::origin(*member));
            let pin = if pin_first_commit {
                self.git_ok(&origin, &["rev-list", "--max-parents=0", &tip])
            } else {
                self.git_ok(&origin, &["rev-parse", &tip])
            };
            pins.push((member.rel_path(), url, pin));
        }
        std::fs::write(seed.join("rwv.toml"), manifest).unwrap();
        std::fs::write(
            seed.join(".gitattributes"),
            format!("{LOCK_MERGE_ATTRIBUTE}\n"),
        )
        .unwrap();
        let entries: Vec<(&str, &str, &str)> = pins
            .iter()
            .map(|(k, u, p)| (k.as_str(), u.as_str(), p.as_str()))
            .collect();
        common::fixture_lock(&seed, &entries);
        self.git_ok(&seed, &["add", "rwv.toml", ".gitattributes", "rwv.lock"]);
        self.git_ok(&seed, &["commit", "-m", "manifest and lock"]);
        self.git_ok(&seed, &["push", "origin", "main"]);
    }
}

/// A remote [`Sandbox::unreachable`] moved away; dropping it moves the remote
/// back, so a scenario that ends or fails mid-outage leaves no remote missing.
#[must_use = "the remote is reachable again as soon as this drops"]
pub struct Unreachable<'a> {
    sandbox: &'a Sandbox,
    remote: Remote,
}

impl Drop for Unreachable<'_> {
    fn drop(&mut self) {
        let restored = std::fs::rename(
            self.sandbox.unreachable_path(self.remote),
            self.sandbox.remote_path(self.remote),
        );
        if let Err(e) = restored {
            if !std::thread::panicking() {
                panic!("cannot bring {:?} back: {e}", self.remote);
            }
        }
    }
}
