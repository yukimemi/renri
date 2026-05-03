//! renri — unified manager for git worktrees and jujutsu workspaces.
//!
//! See ROADMAP.md for the design and the staged work plan.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use teravars::{Engine, system_context};

use renri::{config, hooks, layout, picker, shell_init, vcs};

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

        /// Fork the new branch off this revision instead of the cwd
        /// worktree's current HEAD. Accepts whatever the backend
        /// understands: a commit hash, branch / bookmark name, tag, or
        /// (for jj) a revset like `@-`. Pass the flag without a value
        /// to open a fuzzy picker over local refs.
        #[arg(
            long,
            value_name = "REF",
            num_args = 0..=1,
            default_missing_value = ""
        )]
        from: Option<String>,
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

    /// Write a starter `renri.toml` in the current directory.
    Init {
        /// Overwrite an existing renri.toml.
        #[arg(long)]
        force: bool,
    },

    /// Print a shell snippet that makes `renri cd` actually `cd` the
    /// parent shell. Source it from your shell's startup file.
    ShellInit {
        #[arg(value_enum)]
        shell: shell_init::Shell,
    },

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
    let non_interactive = cli.non_interactive;

    match cli.command {
        Command::List => cmd_list(choice),
        Command::Config {
            sub: ConfigCommand::Show,
        } => cmd_config_show(choice),
        Command::Add { name, from } => cmd_add(choice, name, from, non_interactive),
        Command::Remove { name } => cmd_remove(choice, name, non_interactive),
        Command::Cd { name } => cmd_cd(choice, name, non_interactive),
        Command::Exec { name, argv } => cmd_exec(choice, name, argv, non_interactive),
        Command::Prune => cmd_prune(choice),
        Command::Init { force } => cmd_init(force),
        Command::ShellInit { shell } => {
            print!("{}", shell_init::snippet(shell));
            Ok(())
        }
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

fn cmd_add(
    choice: vcs::VcsChoice,
    name: Option<String>,
    from: Option<String>,
    non_interactive: bool,
) -> Result<()> {
    // Trim the user-supplied `--from`. An empty string after trim is the
    // signal for "open the picker" (clap's `default_missing_value = ""`),
    // so preserve `Some("")` here instead of filtering it out.
    let from = from.map(|s| s.trim().to_string());

    let name = match name {
        Some(n) => n.trim().to_string(),
        None => prompt_branch_name(non_interactive)?,
    };
    if name.is_empty() {
        anyhow::bail!("branch / bookmark name must not be empty");
    }

    let opened = open_repo_backend(choice)?;
    let mut engine = Engine::new();

    let loaded = config::Config::load_with_engine(Some(&opened.repo.root), &mut engine)?;

    let vcs_ctx = layout::discover_vcs_context(opened.backend.as_ref(), &opened.repo.root, &name);
    let base_ctx = system_context();

    let path = layout::render_path(
        &mut engine,
        &base_ctx,
        &vcs_ctx,
        loaded.config.layout.worktree_root.as_deref(),
        loaded.config.layout.worktree_path.as_deref(),
    )?;

    // Resolve to an absolute path before any IO. The backends run with
    // `current_dir = repo_root`, so a relative path (e.g. when the user
    // configured `worktree_root = "./wt"` in renri.toml) would otherwise
    // mean two different things at `path.exists()` / `create_dir_all`
    // (process CWD) versus `backend.add` (repo root).
    let path = if path.is_absolute() {
        path
    } else {
        opened.repo.root.join(&path)
    };

    if path.exists() {
        anyhow::bail!(
            "target path already exists: {}\n\
             remove it manually or pick a different branch name",
            path.display()
        );
    }

    // Both `git worktree add` and `jj workspace add` require the parent
    // directory to exist; neither creates intermediate directories. Without
    // this, a fresh user with no `~/wt/<owner>/<repo>/` would see a confusing
    // OS-level "path not found" error instead of a clean creation.
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("creating worktree-parent directory {}", parent.display())
            })?;
        }
    }

    // Resolve `--from` semantics:
    //   None       → no flag passed → fork off cwd HEAD (default)
    //   Some("")   → flag passed without value → open fuzzy picker
    //   Some(ref)  → use ref as-is
    let from_resolved = match from.as_deref() {
        None => None,
        Some("") => Some(prompt_base_ref(opened.backend.as_ref(), non_interactive)?),
        Some(ref_str) => Some(ref_str.to_string()),
    };

    let strategy = if opened.backend.branch_exists(&name) {
        if from_resolved.is_some() {
            tracing::warn!(
                branch = %name,
                "--from is ignored when attaching to an existing branch"
            );
        }
        tracing::info!(branch = %name, "attaching to existing branch");
        vcs::AddBranch::ExistingBranch(&name)
    } else {
        let base = from_resolved.as_deref();
        tracing::info!(
            branch = %name,
            base = base.unwrap_or("(cwd HEAD)"),
            "creating new branch"
        );
        vcs::AddBranch::NewBranch { name: &name, base }
    };

    println!("creating worktree at {}", path.display());
    opened.backend.add(&path, strategy)?;

    let post = &loaded.config.hooks.post_create;
    if !post.is_empty() {
        println!("running {} post_create hook(s)", post.len());
        let mut hr = hooks::HookRun {
            repo_root: &opened.repo.root,
            worktree_path: &path,
            vcs: &vcs_ctx,
            engine: &mut engine,
            base_ctx: &base_ctx,
        };
        hooks::run_all(post, &mut hr)?;
    }

    println!("done. {}", path.display());
    Ok(())
}

fn cmd_cd(choice: vcs::VcsChoice, name: Option<String>, non_interactive: bool) -> Result<()> {
    let opened = open_repo_backend(choice)?;
    let worktrees = opened.backend.list()?;
    let picked = picker::resolve(&worktrees, name.as_deref(), non_interactive, "switch to:")?;
    println!("{}", picked.path.display());
    Ok(())
}

fn cmd_remove(choice: vcs::VcsChoice, name: Option<String>, non_interactive: bool) -> Result<()> {
    let opened = open_repo_backend(choice)?;
    let mut engine = Engine::new();
    let loaded = config::Config::load_with_engine(Some(&opened.repo.root), &mut engine)?;

    let worktrees = opened.backend.list()?;
    let picked = picker::resolve(&worktrees, name.as_deref(), non_interactive, "remove:")?.clone();

    if picked.is_main {
        anyhow::bail!(
            "{} is the main worktree and cannot be removed via renri",
            picked.name
        );
    }

    let pre = &loaded.config.hooks.pre_remove;
    if !pre.is_empty() {
        let branch = picked.branch.clone().unwrap_or_else(|| picked.name.clone());
        let vcs_ctx =
            layout::discover_vcs_context(opened.backend.as_ref(), &opened.repo.root, &branch);
        let base_ctx = system_context();
        let mut hr = hooks::HookRun {
            repo_root: &opened.repo.root,
            worktree_path: &picked.path,
            vcs: &vcs_ctx,
            engine: &mut engine,
            base_ctx: &base_ctx,
        };
        println!("running {} pre_remove hook(s)", pre.len());
        hooks::run_all(pre, &mut hr)?;
    }

    println!("removing {}", picked.path.display());
    opened.backend.remove(&picked.path, false)?;
    Ok(())
}

fn cmd_exec(
    choice: vcs::VcsChoice,
    name: Option<String>,
    argv: Vec<String>,
    non_interactive: bool,
) -> Result<()> {
    if argv.is_empty() {
        anyhow::bail!("no command was given (use `renri exec <name> -- cmd args...`)");
    }

    let opened = open_repo_backend(choice)?;
    let worktrees = opened.backend.list()?;
    let picked = picker::resolve(&worktrees, name.as_deref(), non_interactive, "exec in:")?;

    let status = std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .current_dir(&picked.path)
        .status()
        .with_context(|| format!("failed to spawn `{}`", argv[0]))?;

    if !status.success() {
        anyhow::bail!("`{}` exited with {status}", argv[0]);
    }
    Ok(())
}

fn cmd_init(force: bool) -> Result<()> {
    let cwd = std::env::current_dir().context("could not read current directory")?;
    let target = cwd.join("renri.toml");

    if target.exists() && !force {
        anyhow::bail!(
            "{} already exists; pass --force to overwrite",
            target.display()
        );
    }

    std::fs::write(&target, INIT_TEMPLATE)
        .with_context(|| format!("writing {}", target.display()))?;
    println!("wrote {}", target.display());
    Ok(())
}

const INIT_TEMPLATE: &str = r#"# renri.toml — generated by `renri init`
# See https://github.com/yukimemi/renri for the full schema.

# Pull other config files into this one (relative paths resolve against
# this file's directory). The included file is loaded *first*, so this
# file's values override anything it sets.
# include = ["base.toml"]

# --- layout -----------------------------------------------------------
# Where new worktrees / workspaces land.
# Defaults are equivalent to:
#   worktree_root = "{{ env(name='HOME') }}/wt"   (Unix)
#                 = "{{ env(name='USERPROFILE') }}/wt"   (Windows)
#   worktree_path = "{{ vcs.owner }}/{{ vcs.repo }}/{{ vcs.branch | replace(from='/', to='-') }}"

# [layout]
# worktree_root = "{{ env(name='HOME') }}/wt"
# worktree_path = "{{ vcs.owner }}/{{ vcs.repo }}/{{ vcs.branch | replace(from='/', to='-') }}"

# Per-host / per-OS override:
# {% if system.os == "windows" %}
# [layout]
# worktree_root = "C:/wt"
# {% endif %}

# --- hooks ------------------------------------------------------------
# Each hook is one of:
#   { type = "copy",    files = [".env.example -> .env", "scripts/local.sh"] }
#   { type = "symlink", src = "...", dst = "..." }
#   { type = "command", run = "...", shell = "auto" | "bash" | "pwsh" | "sh" | "zsh" | "cmd" }
#
# String fields go through Tera, so `vcs.*` and `system.*` are in scope.

# [[hooks.post_create]]
# type = "copy"
# files = [".env.example -> .env"]
#
# [[hooks.post_create]]
# type = "command"
# run = "echo 'welcome to {{ vcs.branch }}'"
#
# [[hooks.pre_remove]]
# type = "command"
# run = "echo 'cleaning up {{ vcs.branch }}'"
"#;

fn cmd_prune(choice: vcs::VcsChoice) -> Result<()> {
    let opened = open_repo_backend(choice)?;
    let output = opened.backend.prune()?;
    let trimmed = output.trim();
    if trimmed.is_empty() {
        println!("nothing to prune");
    } else {
        println!("{trimmed}");
    }
    Ok(())
}

fn prompt_branch_name(non_interactive: bool) -> Result<String> {
    if non_interactive {
        anyhow::bail!("--non-interactive set and no branch name was given");
    }
    let answer = inquire::Text::new("branch / bookmark name?")
        .prompt()
        .context("interactive prompt cancelled")?;
    Ok(answer.trim().to_string())
}

fn prompt_base_ref(backend: &dyn vcs::Backend, non_interactive: bool) -> Result<String> {
    if non_interactive {
        anyhow::bail!("--non-interactive set and `--from` had no value");
    }
    let refs = backend.list_refs()?;
    if refs.is_empty() {
        anyhow::bail!(
            "no branches / bookmarks / tags found to pick from; pass an explicit `--from <REF>`"
        );
    }
    let picked = inquire::Select::new("base ref for the new branch?", refs)
        .prompt()
        .context("interactive pick cancelled")?;
    Ok(picked)
}
