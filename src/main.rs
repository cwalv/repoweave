use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "rwv", version, about = "A cross-repo workspace manager")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Create, delete, or list weaves
    Weave {
        /// Project name
        project: String,
        /// Optional weave name
        name: Option<String>,
    },
    /// Clone a project and its repos
    Fetch {
        /// Source to fetch from
        source: String,
    },
    /// Add a repo to the current weave
    Add {
        /// Repository URL
        url: String,
    },
    /// Remove a repo from the current weave
    Remove {
        /// Path of the repo to remove
        path: String,
    },
    /// Snapshot repo versions
    Lock,
    /// Snapshot all projects
    LockAll,
    /// Convention enforcement
    Check,
    /// Print weave root path
    Resolve,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        None => {
            println!("rwv: showing current context (not yet implemented)");
        }
        Some(Commands::Weave { project, name }) => {
            println!(
                "rwv weave: project={}, name={} (not yet implemented)",
                project,
                name.unwrap_or_default()
            );
        }
        Some(Commands::Fetch { source }) => {
            println!("rwv fetch: source={source} (not yet implemented)");
        }
        Some(Commands::Add { url }) => {
            println!("rwv add: url={url} (not yet implemented)");
        }
        Some(Commands::Remove { path }) => {
            println!("rwv remove: path={path} (not yet implemented)");
        }
        Some(Commands::Lock) => {
            println!("rwv lock (not yet implemented)");
        }
        Some(Commands::LockAll) => {
            println!("rwv lock-all (not yet implemented)");
        }
        Some(Commands::Check) => {
            println!("rwv check (not yet implemented)");
        }
        Some(Commands::Resolve) => {
            println!("rwv resolve (not yet implemented)");
        }
    }

    Ok(())
}
