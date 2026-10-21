use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The program to run in each repo: shown as the operator typed it, run from
/// where it was found.
pub struct Program {
    spelled: String,
    located: Located,
}

enum Located {
    /// Absolute, so every checkout runs the one file the lookup found.
    OnPath(PathBuf),
    CheckoutRelative,
}

impl Program {
    pub fn locate<E: From<io::Error>>(
        spelled: String,
        find_on_path: impl FnOnce(&str) -> Result<PathBuf, E>,
    ) -> Result<Self, E> {
        let as_path = Path::new(&spelled);
        let located = if as_path.components().count() > 1 || as_path.is_absolute() {
            Located::CheckoutRelative
        } else {
            Located::OnPath(std::path::absolute(find_on_path(&spelled)?)?)
        };
        Ok(Self { spelled, located })
    }

    pub fn spelled(&self) -> &str {
        &self.spelled
    }

    pub fn command_in(&self, checkout: &Path) -> Command {
        let mut command = match &self.located {
            Located::OnPath(found) => Command::new(found),
            Located::CheckoutRelative => Command::new(checkout.join(&self.spelled)),
        };
        // A shell gives the child the word it was typed as, not the file that
        // word resolved to.
        #[cfg(unix)]
        std::os::unix::process::CommandExt::arg0(&mut command, &self.spelled);
        command.current_dir(checkout);
        command
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn never_looked_up(_: &str) -> io::Result<PathBuf> {
        panic!("a spelling with a directory in it is not looked up on PATH")
    }

    #[test]
    fn a_relative_path_lookup_result_is_read_from_here_not_from_the_checkout() {
        let program = Program::locate("mytool".into(), |_| {
            Ok::<_, io::Error>(PathBuf::from("tools").join("mytool"))
        })
        .unwrap_or_else(|e| panic!("{e:#}"));
        let checkout = std::env::temp_dir().join("checkout");
        assert_eq!(
            program.command_in(&checkout).get_program(),
            std::env::current_dir()
                .unwrap()
                .join("tools")
                .join("mytool"),
        );
    }

    #[test]
    fn a_program_spelled_with_a_directory_runs_relative_to_each_checkout() {
        let program = Program::locate("./scripts/test.sh".into(), never_looked_up)
            .unwrap_or_else(|e| panic!("{e:#}"));
        for checkout in ["app", "lib"].map(|repo| std::env::temp_dir().join(repo)) {
            assert_eq!(
                program.command_in(&checkout).get_program(),
                checkout.join("./scripts/test.sh"),
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_program_found_on_path_sees_its_spelling_as_argv0() {
        let program = Program::locate("spelled-word".into(), |_| std::env::current_exe())
            .unwrap_or_else(|e| panic!("{e:#}"));
        let out = program
            .command_in(&std::env::current_dir().unwrap())
            .args([
                "program::tests::argv0_child",
                "--exact",
                "--nocapture",
                "--test-threads=1",
            ])
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.lines().any(|line| line == "argv0=spelled-word"),
            "{stdout}"
        );
    }

    /// The child half of the test above; on its own it asserts nothing.
    #[cfg(unix)]
    #[test]
    fn argv0_child() {
        let argv0 = std::env::args_os().next().unwrap_or_default();
        println!("\nargv0={}", argv0.to_string_lossy());
    }
}
