//! renri — unified manager for git worktrees and jujutsu workspaces.
//!
//! See ROADMAP.md for the design and the staged work plan.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use teravars::{Engine, system_context};

use renri::{config, layout, vcs};

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
    Config {
        #[command(subcommand)]
        sub: ConfigCommand,
    },
}

#[derive(Subcommand, Debug)]
enum ConfigCommand {
    /// Show the resolved configuration and the path that would be used for
    /// the current branch.
    Show,
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
        Command::Config {
            sub: ConfigCommand::Show,
        } => cmd_config_show(choice),
        Command::Add { .. } => not_yet("add"),
        Command::Remove { .. } => not_yet("remove"),
        Command::Cd { .. } => not_yet("cd"),
        Command::Exec { .. } => not_yet("exec"),
        Command::Prune => not_yet("prune"),
    }
}

fn cmd_list(choice: vcs::VcsChoice) -> Result<()> {
    let opened = open_repo_backend(choice)?;
    let backend = opened.backend;

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

struct OpenedRepo {
    repo: vcs::Repo,
    backend: Box<dyn vcs::Backend>,
}

fn open_repo_backend(choice: vcs::VcsChoice) -> Result<OpenedRepo> {
    let cwd = std::env::current_dir().context("could not read current directory")?;
    let repo = vcs::detect(&cwd).context("not inside a git or jj repository")?;
    let kind = vcs::select_kind(repo.kind, choice)?;
    let backend = vcs::open_backend(&repo, kind)?;
    Ok(OpenedRepo { repo, backend })
}

fn cmd_config_show(choice: vcs::VcsChoice) -> Result<()> {
    let opened = open_repo_backend(choice)?;
    let mut engine = Engine::new();

    let loaded = config::Config::load_with_engine(Some(&opened.repo.root), &mut engine)?;

    let branch = opened
        .backend
        .current_branch()
        .unwrap_or_else(|| "(none)".into());
    let vcs_ctx = layout::discover_vcs_context(opened.backend.as_ref(), &opened.repo.root, &branch);

    let path = layout::render_path(
        &mut engine,
        &system_context(),
        &vcs_ctx,
        loaded.config.layout.worktree_root.as_deref(),
        loaded.config.layout.worktree_path.as_deref(),
    )?;

    println!("backend:           {}", opened.backend.name());
    println!("repo root:         {}", opened.repo.root.display());
    print!("vcs.host:          ");
    match vcs_ctx.host.as_deref() {
        Some(h) => println!("{h}"),
        None => println!("(none)"),
    }
    println!("vcs.owner:         {}", vcs_ctx.owner);
    println!("vcs.repo:          {}", vcs_ctx.repo);
    println!("vcs.branch:        {}", vcs_ctx.branch);
    println!();
    println!(
        "worktree_root:     {}",
        loaded
            .config
            .layout
            .worktree_root
            .as_deref()
            .unwrap_or(layout::DEFAULT_WORKTREE_ROOT)
    );
    println!(
        "worktree_path:     {}",
        loaded
            .config
            .layout
            .worktree_path
            .as_deref()
            .unwrap_or(layout::DEFAULT_WORKTREE_PATH)
    );
    println!("→ resolved path:   {}", path.display());
    println!();
    println!(
        "post_create hooks: {}",
        loaded.config.hooks.post_create.len()
    );
    println!(
        "pre_remove hooks:  {}",
        loaded.config.hooks.pre_remove.len()
    );
    println!();
    if loaded.sources.is_empty() {
        println!("config sources:    (none — using built-in defaults)");
    } else {
        println!("config sources:");
        for s in &loaded.sources {
            println!("  - {}", s.display());
        }
    }
    Ok(())
}

fn not_yet(verb: &str) -> Result<()> {
    anyhow::bail!("`renri {verb}` is not yet implemented — see ROADMAP.md")
}
