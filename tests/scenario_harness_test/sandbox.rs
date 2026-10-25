use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::common;

pub const IDENTITY: &[(&str, &str)] = &[
    ("user.name", "Sandbox"),
    ("user.email", "sandbox@example.invalid"),
];

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Machine {
    A,
    B,
    C,
}

impl Machine {
    fn dir_name(self) -> &'static str {
        match self {
            Machine::A => "machine-a",
            Machine::B => "machine-b",
            Machine::C => "machine-c",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Place {
    Primary(Machine),
    Workweave(Machine, Repo, String),
}

impl Place {
    pub fn ww(machine: Machine, name: &str) -> Place {
        Place::Workweave(machine, Repo::Proj, name.to_owned())
    }

    pub fn machine(&self) -> Machine {
        match self {
            Place::Primary(m) | Place::Workweave(m, _, _) => *m,
        }
    }
}

/// What a second project shares with `proj`: `lib`, declared on `branch` and
/// pinned at `lib`'s first commit.
#[derive(Debug, Clone, Copy)]
pub struct SecondProject {
    pub branch: &'static str,
}

/// A hermetic set of remotes and the machines that clone them.
///
/// Every remote is a bare repository on branch `main`; `proj` declares `app`,
/// `lib` and `docs` on `main` and commits an `rwv.lock` pinning each remote's
/// `main`. `HOME` points into the sandbox for every git and rwv it runs, so no
/// config of the host reaches a commit, and the identity comes from
/// [`IDENTITY`]. `lib` carries a second commit on `main` and a `dev` branch, so
/// a project can be pinned behind or declared elsewhere; nothing else differs
/// between members.
pub struct Sandbox {
    _tmp: tempfile::TempDir,
    pub root: PathBuf,
    home: PathBuf,
}

impl Sandbox {
    pub fn new(second: Option<SecondProject>) -> Sandbox {
        let tmp = common::tempdir().expect("a sandbox root");
        let root = tmp.path().to_path_buf();
        let home = root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let sandbox = Sandbox {
            _tmp: tmp,
            root,
            home,
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
            false,
        );
        if let Some(second) = second {
            sandbox.seed_project(Repo::Proj2, &[(Repo::Lib, second.branch)], true);
        }
        sandbox
    }

    pub fn remote(&self, repo: Repo) -> PathBuf {
        self.root
            .join("remotes")
            .join(format!("{}.git", repo.name()))
    }

    pub fn remote_url(&self, repo: Repo) -> String {
        common::file_url(self.remote(repo))
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
        }
    }

    pub fn checkout(&self, place: &Place, repo: Repo) -> PathBuf {
        common::under(self.place_root(place), &repo.rel_path())
    }

    /// The repository whose object store and refs back every checkout of
    /// `repo` on `machine`: the canonical clone, which each workweave's
    /// worktree shares.
    pub fn canonical(&self, machine: Machine, repo: Repo) -> PathBuf {
        self.checkout(&Place::Primary(machine), repo)
    }

    pub fn machines(&self) -> Vec<Machine> {
        [Machine::A, Machine::B, Machine::C]
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

    /// Every repository holding refs for `repo`: its remote and each
    /// machine's canonical clone.
    pub fn ref_stores(&self, repo: Repo) -> Vec<PathBuf> {
        std::iter::once(self.remote(repo))
            .chain(
                self.machines()
                    .into_iter()
                    .map(|m| self.canonical(m, repo))
                    .filter(|p| p.is_dir()),
            )
            .filter(|p| p.is_dir())
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

    pub fn rwv(&self, dir: &Path, args: &[&str]) -> Output {
        self.hermetic(common::rwv_std())
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap_or_else(|e| panic!("rwv {args:?} in {} failed to start: {e}", dir.display()))
    }

    pub fn fetch_command(&self, project: Repo) -> Vec<String> {
        vec!["fetch".to_owned(), self.remote_url(project)]
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

    fn init_remote(&self, repo: Repo) -> PathBuf {
        let remote = self.remote(repo);
        std::fs::create_dir_all(remote.parent().unwrap()).unwrap();
        self.git_ok(
            remote.parent().unwrap(),
            &[
                "init",
                "--bare",
                "--initial-branch=main",
                remote.to_str().unwrap(),
            ],
        );
        let seed = self.seed_dir(repo);
        std::fs::create_dir_all(seed.parent().unwrap()).unwrap();
        self.git_ok(
            seed.parent().unwrap(),
            &["clone", remote.to_str().unwrap(), seed.to_str().unwrap()],
        );
        seed
    }

    fn seed_commit(&self, seed: &Path, file: &str, content: &str, message: &str) {
        std::fs::write(seed.join(file), content).unwrap();
        self.git_ok(seed, &["add", file]);
        self.git_ok(seed, &["commit", "-m", message]);
    }

    fn seed_member(&self, repo: Repo) {
        let seed = self.init_remote(repo);
        self.seed_commit(&seed, "README", &format!("{}\n", repo.name()), "initial");
        if repo == Repo::Lib {
            self.git_ok(&seed, &["branch", "dev"]);
            self.seed_commit(&seed, "lib.txt", "second\n", "lib: second");
            self.git_ok(&seed, &["push", "origin", "dev"]);
        }
        self.git_ok(&seed, &["push", "origin", "main"]);
    }

    fn seed_project(&self, project: Repo, members: &[(Repo, &str)], pin_first_commit: bool) {
        let seed = self.init_remote(project);
        let mut manifest =
            String::from("[integrations.vscode-workspace]\nenabled = false\n\n[repositories]\n");
        let mut pins = Vec::new();
        for (member, branch) in members {
            let url = self.remote_url(*member);
            manifest.push_str(&format!(
                "\n[repositories.\"{key}\"]\ntype = \"git\"\nurl = \"{url}\"\nversion = \"{branch}\"\nrole = \"owned\"\n",
                key = member.rel_path(),
            ));
            let tip = format!("refs/heads/{branch}");
            let pin = if pin_first_commit {
                self.git_ok(
                    &self.remote(*member),
                    &["rev-list", "--max-parents=0", &tip],
                )
            } else {
                self.git_ok(&self.remote(*member), &["rev-parse", &tip])
            };
            pins.push((member.rel_path(), url, pin));
        }
        std::fs::write(seed.join("rwv.toml"), manifest).unwrap();
        std::fs::write(seed.join(".gitattributes"), "rwv.lock merge=rwv-ours\n").unwrap();
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
