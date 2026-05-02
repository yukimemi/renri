//! renri — unified manager for git worktrees and jujutsu workspaces.
//!
//! See ROADMAP.md for the design and the staged work plan.

use std::path::Path;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};

use renri::vcs;

#[derive(Parser, Debug)]
#[command(name = "renri", version, about, long_about = None)]
struct Cli {
    /// Force a specific VCS instead of auto-detecting from the current repo.
    #[arg(long, global = true, value_enum)]
    vcs: Option<Vcs>,

    /// Disable interactive fallback. Required-but-missing arguments fail the
    /// command instead of opening a picker.
    #[arg(long, global = true)]
    non_interactive: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Vcs {
    Git,
    Jj,
}

fn vcs_choice(v: Option<Vcs>) -> vcs::VcsChoice {
    match v {
        None => vcs::VcsChoice::Auto,
        Some(Vcs::Git) => vcs::VcsChoice::Git,
        Some(Vcs::Jj) => vcs::VcsChoice::Jj,
    }
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Create a new worktree (git) or workspace (jj).
    Add {
        /// Branch / bookmark name. If omitted, prompt interactively.
        name: Option<String>,
    },

    /// List existing worktrees / workspaces.
    #[command(alias = "ls")]
    List,

    /// Remove a worktree / forget a workspace.
    #[command(alias = "rm")]
    Remove {
        /// Worktree name. If omitted, open a fuzzy picker.
        name: Option<String>,
    },

    /// Print the absolute path of a worktree (designed to be used from a
    /// shell function: `cd "$(renri cd foo)"`).
    Cd {
        /// Worktree name. If omitted, open a fuzzy picker.
        name: Option<String>,
    },

    /// Run a command inside a worktree.
    Exec {
        /// Worktree name. If omitted, open a fuzzy picker.
        name: Option<String>,

        /// Command + args to run.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        argv: Vec<String>,
    },

    /// Garbage-collect worktrees / stale jj workspaces.
    Prune,

    /// Manage configuration.
    Config,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn,renri=info")),
        )
        .init();

    let cli = Cli::parse();

    let choice = vcs_choice(cli.vcs);

    match cli.command {
        Command::List => cmd_list(choice),
        Command::Add { .. } => not_yet("add"),
        Command::Remove { .. } => not_yet("remove"),
        Command::Cd { .. } => not_yet("cd"),
        Command::Exec { .. } => not_yet("exec"),
        Command::Prune => not_yet("prune"),
        Command::Config => not_yet("config"),
    }
}

fn cmd_list(choice: vcs::VcsChoice) -> Result<()> {
    let backend = open_repo_backend(choice)?;

    let worktrees = backend.list()?;
    if worktrees.is_empty() {
        return Ok(());
    }

    let name_w = worktrees.iter().map(|w| w.name.len()).max().unwrap_or(0);
    let path_w = worktrees
        .iter()
        .map(|w| w.path.display().to_string().len())
        .max()
        .unwrap_or(0);

    for w in &worktrees {
        let marker = if w.is_main { "*" } else { " " };
        let branch = w.branch.as_deref().unwrap_or("(detached)");
        let mut flags = Vec::new();
        if w.is_bare {
            flags.push("bare");
        }
        if w.is_locked {
            flags.push("locked");
        }
        if w.is_stale {
            flags.push("stale");
        }
        let suffix = if flags.is_empty() {
            String::new()
        } else {
            format!(" [{}]", flags.join(","))
        };
        println!(
            "{marker} {name:name_w$}  {path:path_w$}  {branch}{suffix}",
            name = w.name,
            path = w.path.display(),
        );
    }
    Ok(())
}

fn open_repo_backend(choice: vcs::VcsChoice) -> Result<Box<dyn vcs::Backend>> {
    let cwd = std::env::current_dir().context("could not read current directory")?;
    let repo = vcs::detect(&cwd).context("not inside a git or jj repository")?;
    let kind = vcs::select_kind(repo.kind, choice)?;
    let backend = vcs::open_backend(&repo, kind)?;
    let _ = Path::new("");
    Ok(backend)
}

fn not_yet(verb: &str) -> Result<()> {
    anyhow::bail!("`renri {verb}` is not yet implemented — see ROADMAP.md")
}
