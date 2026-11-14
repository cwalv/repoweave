pub(super) const DEFAULT_SIZE: usize = 7;

#[derive(Clone, Copy, PartialEq, Eq)]
enum At {
    Outside,
    Head,
    Base,
    Other,
}

/// The marker `line` is, `size` long, as git writes it: `<`, `|` and `>`
/// followed by nothing or a space and a label, `=` by nothing.
fn marker(line: &str, size: usize) -> Option<char> {
    let line = line.trim_end_matches(['\n', '\r']);
    let first = line.chars().next().filter(|c| "<|=>".contains(*c))?;
    let rest = line.strip_prefix(&first.to_string().repeat(size))?;
    match first {
        '=' => rest.is_empty(),
        _ => rest.is_empty() || rest.starts_with(' '),
    }
    .then_some(first)
}

/// `text` with every region git marked in conflict taken from HEAD's half,
/// then from the other's; `None` where it holds no region, or a marker out
/// of place. With `based`, the conflict style writes a base section in every
/// region; without, a line that reads as its marker is the file's own.
pub(super) fn halves(text: &str, size: usize, based: bool) -> Option<[String; 2]> {
    let mut halves = [String::new(), String::new()];
    let mut at = At::Outside;
    let mut regions = 0;
    for line in text.split_inclusive('\n') {
        let marker = marker(line, size).filter(|&m| based || m != '|');
        at = match (at, marker) {
            (At::Outside, None) => {
                halves.iter_mut().for_each(|half| half.push_str(line));
                At::Outside
            }
            (At::Head, None) => {
                halves[0].push_str(line);
                At::Head
            }
            (At::Other, None) => {
                halves[1].push_str(line);
                At::Other
            }
            (At::Base, None) | (At::Head, Some('|')) => At::Base,
            (At::Outside, Some('<')) => At::Head,
            (At::Base, Some('=')) => At::Other,
            (At::Head, Some('=')) if !based => At::Other,
            (At::Other, Some('>')) => {
                regions += 1;
                At::Outside
            }
            _ => return None,
        };
    }
    (at == At::Outside && regions > 0).then_some(halves)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::{git_command_in_test, GitVcs, LineEndings};
    use crate::vcs::Vcs;
    use std::collections::BTreeMap;
    use std::io::Write;
    use std::path::Path;

    #[test]
    fn each_half_takes_its_side_of_every_region_and_drops_a_base_section() {
        let text = "a\n<<<<<<< HEAD\nb\n||||||| base\nc\n=======\nd\n>>>>>>> i\ne\r\n<<<<<<< HEAD\r\n||||||| base\r\n=======\r\nf\r\n>>>>>>> i\r\n";
        assert_eq!(
            halves(text, DEFAULT_SIZE, true),
            Some(["a\nb\ne\r\n".to_owned(), "a\nd\ne\r\nf\r\n".to_owned()])
        );
    }

    #[test]
    fn no_region_or_a_marker_out_of_place_has_no_halves() {
        for text in [
            "a\nb\n",
            "<<<<<<< HEAD\na\n=======\nb\n",
            "a\n=======\nb\n",
            "<<<<<<< HEAD\na\n>>>>>>> i\n",
            "<<<<<<< HEAD\na\n=======\nb\n=======\nc\n>>>>>>> i\n",
        ] {
            assert_eq!(halves(text, DEFAULT_SIZE, false), None, "{text}");
        }
        assert_eq!(
            halves("<<<<<<<< HEAD\na\n========\nb\n>>>>>>>> i\n", 8, false),
            Some(["a\n".to_owned(), "b\n".to_owned()])
        );
        assert_eq!(
            halves(
                "<<<<<<<< HEAD\na\n========\nb\n>>>>>>>> i\n",
                DEFAULT_SIZE,
                true
            ),
            None
        );
    }

    #[test]
    fn a_line_of_the_files_own_that_reads_as_a_marker_never_splits_a_region_wrong() {
        let pair = |head: &str, other: &str| Some([head.to_owned(), other.to_owned()]);
        for own in ["<<<<<<<\n", "=======\n", ">>>>>>>\n", "|||||||\n"] {
            let content = own == "|||||||\n";
            let based = "a\n<<<<<<< HEAD\nb\n||||||| base\nx\n=======\nc\n>>>>>>> i\nz\n";
            let plain = "a\n<<<<<<< HEAD\nb\n=======\nc\n>>>>>>> i\nz\n";
            for at in ["a\n", "b\n", "x\n", "c\n"] {
                let with_own = |text: &str| text.replacen(at, &format!("{at}{own}"), 1);
                assert_eq!(
                    halves(&with_own(based), DEFAULT_SIZE, true),
                    None,
                    "{own:?} after {at:?}"
                );
            }
            let own_line = own.trim_end();
            let expected = |at: &str| match (content, at) {
                (true, "a\n") => pair(
                    &format!("a\n{own_line}\nb\nz\n"),
                    &format!("a\n{own_line}\nc\nz\n"),
                ),
                (true, "b\n") => pair(&format!("a\nb\n{own_line}\nz\n"), "a\nc\nz\n"),
                (true, _) => pair("a\nb\nz\n", &format!("a\nc\n{own_line}\nz\n")),
                (false, _) => None,
            };
            for at in ["a\n", "b\n", "c\n"] {
                let text = plain.replacen(at, &format!("{at}{own}"), 1);
                assert_eq!(halves(&text, DEFAULT_SIZE, false), expected(at), "{text:?}");
            }
        }
    }

    #[test]
    fn the_picks_read_markers_of_the_size_the_attributes_set() {
        let dir = crate::tempdir().unwrap();
        let repo = dir.path();
        assert!(git(repo, &["init", "-q"]).status.success());
        LineEndings::Verbatim.pin(repo);
        std::fs::write(
            repo.join(".gitattributes"),
            "rwv.toml conflict-marker-size=11\n",
        )
        .unwrap();
        import(
            repo,
            &[[
                "a\nb\nc\n".to_owned(),
                "a\nyours\nc\n".to_owned(),
                "a\nincoming\nc\n".to_owned(),
            ]],
        );
        git(repo, &["checkout", "-qf", "--detach", "y0"]);
        assert!(!git(repo, &["merge", "-q", "--no-edit", "i0"])
            .status
            .success());
        let text = working(repo);
        assert!(text.contains("<<<<<<<<<<< "), "{text}");
        assert_eq!(
            GitVcs.conflict_picks(repo, "rwv.toml", &text).unwrap(),
            Some(["a\nyours\nc\n".to_owned(), "a\nincoming\nc\n".to_owned()])
        );
    }

    struct Lcg(u64);

    impl Lcg {
        fn below(&mut self, n: usize) -> usize {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            ((self.0 >> 33) % n as u64) as usize
        }
    }

    const NAMES: [&str; 5] = ["app", "docs", "lib", "zed", "zee"];
    const ROLES: [&str; 3] = ["fork", "reference", "owned"];

    #[derive(Clone, PartialEq)]
    struct Member {
        role: &'static str,
        no_submodules: bool,
    }

    type Members = BTreeMap<&'static str, Member>;

    fn render(members: &Members) -> String {
        let mut out = "# Repoweave manifest — run `rwv member add <url> --role <role>` to add \
                       repos.\n[repositories]\n"
            .to_owned();
        for (name, member) in members {
            out.push_str(&format!(
                "\n[repositories.\"local/remotes/{name}\"]\ntype = \"git\"\nurl = \
                 \"file:///r/{name}.git\"\nbranch = \"main\"\nrole = \"{}\"\n",
                member.role
            ));
            if member.no_submodules {
                out.push_str("recurse-submodules = false\n");
            }
        }
        out
    }

    /// One to three setter steps: a role set, `recurse-submodules` set (to
    /// its default, which removes the key) or unset, a member added or
    /// removed.
    fn mutate(rng: &mut Lcg, members: &Members) -> Members {
        let mut members = members.clone();
        for _ in 0..=rng.below(3) {
            let step = rng.below(6);
            if step == 4 {
                let absent: Vec<_> = NAMES.iter().filter(|n| !members.contains_key(*n)).collect();
                if !absent.is_empty() {
                    let name = absent[rng.below(absent.len())];
                    members.insert(
                        name,
                        Member {
                            role: "owned",
                            no_submodules: false,
                        },
                    );
                }
                continue;
            }
            let names: Vec<&'static str> = members.keys().copied().collect();
            let name = names[rng.below(names.len())];
            match step {
                0 => members.get_mut(name).unwrap().role = ROLES[rng.below(3)],
                1 | 2 => members.get_mut(name).unwrap().no_submodules = rng.below(2) == 0,
                3 if members.len() > 1 => {
                    members.remove(name);
                }
                5 => members.get_mut(name).unwrap().no_submodules = false,
                _ => {}
            }
        }
        members
    }

    fn cases(seed: u64, count: usize) -> Vec<[String; 3]> {
        let mut rng = Lcg(seed);
        let mut cases = Vec::new();
        for _ in 0..count {
            let mut pool = NAMES.to_vec();
            let mut base = Members::new();
            for _ in 0..2 + rng.below(3) {
                let name = pool.remove(rng.below(pool.len()));
                base.insert(
                    name,
                    Member {
                        role: "owned",
                        no_submodules: rng.below(10) < 3,
                    },
                );
            }
            let [b, y, i] = [
                base.clone(),
                mutate(&mut rng, &base),
                mutate(&mut rng, &base),
            ]
            .map(|m| render(&m));
            if y != b && i != b && y != i {
                cases.push([b, y, i]);
            }
        }
        cases
    }

    fn git(repo: &Path, args: &[&str]) -> std::process::Output {
        git_command_in_test()
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .current_dir(repo)
            .output()
            .expect("run git")
    }

    fn import(repo: &Path, cases: &[[String; 3]]) {
        let mut stream = Vec::new();
        for (n, sides) in cases.iter().enumerate() {
            for (side, text) in ["b", "y", "i"].iter().zip(sides) {
                write!(
                    stream,
                    "commit refs/heads/{side}{n}\ncommitter t <t@t> 0 +0000\ndata 1\n{side}\n"
                )
                .unwrap();
                if *side != "b" {
                    writeln!(stream, "from refs/heads/b{n}").unwrap();
                }
                write!(
                    stream,
                    "M 100644 inline rwv.toml\ndata {}\n{text}\n",
                    text.len()
                )
                .unwrap();
            }
        }
        let mut child = git_command_in_test()
            .args(["fast-import", "--quiet"])
            .current_dir(repo)
            .stdin(std::process::Stdio::piped())
            .spawn()
            .expect("run git fast-import");
        child.stdin.take().unwrap().write_all(&stream).unwrap();
        assert!(child.wait().unwrap().success());
    }

    fn working(repo: &Path) -> String {
        std::fs::read_to_string(repo.join("rwv.toml")).unwrap()
    }

    fn taking(repo: &Path, how: &[&str], side: &str, yours: &str, incoming: &str) -> String {
        git(repo, &["checkout", "-qf", "--detach", yours]);
        let args: Vec<&str> = how.iter().copied().chain(["-X", side, incoming]).collect();
        assert!(git(repo, &args).status.success(), "{args:?}");
        working(repo)
    }

    /// The picks of every conflicted working file git leaves for one case:
    /// what the merge, or a third of the time the replay, left; and each
    /// side git resolves every region to with `-X`. Returns the cases that
    /// differ, and how many conflicted.
    fn compare(repo: &Path, cases: &[[String; 3]]) -> (Vec<String>, usize) {
        let mut wrong = Vec::new();
        let mut conflicts = 0;
        for n in 0..cases.len() {
            let (yours, incoming) = (format!("y{n}"), format!("i{n}"));
            let style = ["merge", "diff3", "zdiff3"][n % 3];
            let replay = (n / 3) % 2 == 1;
            git(repo, &["config", "merge.conflictStyle", style]);
            git(repo, &["checkout", "-qf", "--detach", &yours]);
            let how: &[&str] = if replay {
                &["rebase", "-q"]
            } else {
                &["merge", "-q", "--no-edit"]
            };
            let args: Vec<&str> = how.iter().copied().chain([incoming.as_str()]).collect();
            let stopped = !git(repo, &args).status.success();
            let picks = GitVcs
                .conflict_picks(repo, "rwv.toml", &working(repo))
                .unwrap();
            git(repo, &[if replay { "rebase" } else { "merge" }, "--abort"]);
            let expected = stopped.then(|| {
                conflicts += 1;
                let [ours, theirs] =
                    ["ours", "theirs"].map(|side| taking(repo, how, side, &yours, &incoming));
                if replay {
                    [theirs, ours]
                } else {
                    [ours, theirs]
                }
            });
            if picks != expected {
                wrong.push(format!(
                    "case {n} ({style}, {}):\n--- base\n{}--- yours\n{}--- incoming\n{}",
                    if replay { "replay" } else { "merge" },
                    cases[n][0],
                    cases[n][1],
                    cases[n][2]
                ));
            }
        }
        (wrong, conflicts)
    }

    #[test]
    fn the_picks_are_the_regions_git_left_in_setter_shaped_manifests() {
        let cases = cases(51, 2500);
        let chunks: Vec<&[[String; 3]]> = cases.chunks(cases.len().div_ceil(8)).collect();
        let results: Vec<(Vec<String>, usize)> = std::thread::scope(|scope| {
            chunks
                .into_iter()
                .map(|chunk| {
                    scope.spawn(move || {
                        let dir = crate::tempdir().unwrap();
                        let repo = dir.path();
                        assert!(git(repo, &["init", "-q"]).status.success());
                        LineEndings::Verbatim.pin(repo);
                        import(repo, chunk);
                        compare(repo, chunk)
                    })
                })
                .collect::<Vec<_>>()
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .collect()
        });
        let conflicts: usize = results.iter().map(|(_, c)| c).sum();
        let wrong: Vec<&String> = results.iter().flat_map(|(w, _)| w).collect();
        assert!(
            conflicts > 0 && conflicts < cases.len(),
            "{conflicts} of {}",
            cases.len()
        );
        assert!(
            wrong.is_empty(),
            "{} of {} cases: {}",
            wrong.len(),
            cases.len(),
            wrong.iter().take(3).map(|w| w.as_str()).collect::<String>()
        );
    }
}
