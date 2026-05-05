//! renri — unified manager for git worktrees and jujutsu workspaces.
//!
//! See ROADMAP.md for the design and the staged work plan.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use teravars::{Engine, system_context};

use renri::{
    config, discovery, hooks, layout, path_display::display_path, picker, shell_init, updater, vcs,
};

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

    /// Run as if renri was started in `<PATH>` instead of the actual current
    /// directory. Mirrors `git -C`. Repeated uses are not supported (last
    /// wins). When the resolved path is *outside* any repo, renri walks the
    /// configured worktree root for managed projects and offers an
    /// interactive picker (skip with `--non-interactive`).
    #[arg(short = 'C', long = "cwd", global = true, value_name = "PATH")]
    cwd: Option<PathBuf>,

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
    List {
        /// Bypass the PR cache and re-fetch from GitHub. No effect unless
        /// `[ui] show_pr = true` in renri.toml.
        #[arg(long)]
        refresh: bool,
    },

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

    /// Print (or install) a shell snippet that makes `renri cd` actually
    /// `cd` the parent shell instead of spawning a subshell.
    ShellInit {
        #[arg(value_enum)]
        shell: shell_init::Shell,

        /// Append the snippet to your shell's startup file
        /// (~/.bashrc / ~/.zshrc / ~/.config/fish/config.fish / $PROFILE).
        /// Idempotent — re-running is a no-op if the snippet is already
        /// present.
        #[arg(long)]
        install: bool,
    },

    /// Manage configuration.
    Config {
        #[command(subcommand)]
        sub: ConfigCommand,
    },

    /// Run `git fetch origin` / `jj git fetch` in the current repo so all
    /// worktrees see the latest remote refs.
    Sync,

    /// Print a shell-completion script for the given shell. Pipe into your
    /// shell's completion-loader, e.g.
    /// `renri completions bash > ~/.local/share/bash-completion/completions/renri`.
    Completions {
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },

    /// Print `<owner>/<repo>` for the current repo's `origin` remote, or
    /// nothing when the cwd is not in a repo or has no parseable origin.
    ///
    /// Designed for shell wrappers that want to export `GH_REPO` so the
    /// `gh` CLI can find the right repo even from a jj workspace (which
    /// has no `.git/` directory and so isn't auto-detected by `gh`).
    /// The `renri cd` wrapper from `renri shell-init` calls this after
    /// changing directory.
    ///
    /// Always exits 0; an empty stdout means "no repo / no origin" so
    /// callers can `unset GH_REPO` cleanly.
    GhRepo,

    /// Update the renri binary itself to the latest GitHub release.
    SelfUpdate {
        /// Skip the confirmation prompt
        #[arg(long, short = 'y')]
        yes: bool,

        /// Print availability and exit without installing
        #[arg(long)]
        check: bool,
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

    // Spawn background update check if not running self-update or completions.
    let update_check_handle = match cli.command {
        Command::SelfUpdate { .. } | Command::Completions { .. } => None,
        _ => maybe_spawn_auto_update_check(),
    };

    let choice = vcs_choice(cli.vcs);
    let non_interactive = cli.non_interactive;
    let cwd_override = cli.cwd;
    let ctx = CmdCtx {
        choice,
        non_interactive,
        cwd_override: cwd_override.as_deref().map(std::path::Path::to_path_buf),
    };

    let result = match cli.command {
        Command::List { refresh } => cmd_list(&ctx, refresh),
        Command::Config {
            sub: ConfigCommand::Show,
        } => cmd_config_show(&ctx),
        Command::Add { name, from } => cmd_add(&ctx, name, from),
        Command::Remove { name } => cmd_remove(&ctx, name),
        Command::Cd { name } => cmd_cd(&ctx, name),
        Command::Exec { name, argv } => cmd_exec(&ctx, name, argv),
        Command::Prune => cmd_prune(&ctx),
        Command::Init { force } => cmd_init(cwd_override.as_deref(), force),
        Command::ShellInit { shell, install } => {
            if install {
                let target = shell_init::install(shell)?;
                println!("renri shell wrapper installed → {}", display_path(&target));
                println!("restart your shell (or `source` the file) to activate.");
            } else {
                print!("{}", shell_init::snippet(shell));
            }
            Ok(())
        }
        Command::Sync => cmd_sync(&ctx),
        Command::Completions { shell } => {
            let mut cmd = Cli::command();
            let bin = cmd.get_name().to_string();
            clap_complete::generate(shell, &mut cmd, bin, &mut std::io::stdout());
            Ok(())
        }
        Command::GhRepo => cmd_gh_repo(&ctx),
        Command::SelfUpdate { yes, check } => {
            updater::run_self_update(yes, check, ctx.non_interactive)
        }
    };

    if let Some(handle) = update_check_handle {
        finalize_auto_update_check(handle);
    }

    result
}

/// Handle for an ongoing or cached background update check.
enum AutoUpdateHandle {
    /// A newer version was found in the local cache from a previous run.
    CachedAvailable {
        current: String,
        latest: updater::LatestRelease,
    },
    /// A background check is currently in progress.
    Pending {
        current: String,
        rx: std::sync::mpsc::Receiver<Result<updater::LatestRelease>>,
    },
}

/// Spawns a background thread to check for updates if the interval has elapsed.
fn maybe_spawn_auto_update_check() -> Option<AutoUpdateHandle> {
    // Load config to check if auto_update_check is enabled.
    // Try to detect repo root for project-local config.
    let cwd = std::env::current_dir().ok()?;
    let repo_root = vcs::detect(&cwd).map(|r| r.root);
    let loaded = config::Config::load(repo_root.as_deref()).unwrap_or_default();
    if !loaded.config.ui.auto_update_check {
        return None;
    }

    let state = updater::load_check_state();
    let now = std::time::SystemTime::now();
    let current = env!("CARGO_PKG_VERSION").to_string();

    let interval = match loaded.config.ui.update_check_interval.as_deref() {
        Some(s) => match humantime::parse_duration(s) {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!(value = %s, error = %e, "invalid ui.update_check_interval; using default");
                updater::default_interval()
            }
        },
        None => updater::default_interval(),
    };

    if !updater::should_auto_check(state.as_ref(), interval, now) {
        if let Some(state) = state {
            if let Some(cached_tag) = state.last_known_latest.as_ref() {
                if updater::is_update_available(&current, cached_tag).unwrap_or(false) {
                    return Some(AutoUpdateHandle::CachedAvailable {
                        current,
                        latest: updater::LatestRelease {
                            tag_name: cached_tag.clone(),
                            html_url: state.last_known_url.unwrap_or_default(),
                        },
                    });
                }
            }
        }
        return None;
    }

    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(updater::check_latest_release());
    });

    Some(AutoUpdateHandle::Pending { current, rx })
}

/// Waits for the background update check to complete (with a short timeout) and prints a banner if an update is available.
fn finalize_auto_update_check(handle: AutoUpdateHandle) {
    match handle {
        AutoUpdateHandle::CachedAvailable { current, latest } => {
            eprintln!("\n{}", updater::format_update_banner(&current, &latest));
        }
        AutoUpdateHandle::Pending { current, rx } => {
            // Wait for 1 second.
            let res = rx.recv_timeout(std::time::Duration::from_secs(1));
            let now_unix = std::time::SystemTime::now()
                .duration_since(std::time::SystemTime::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);

            let mut state = updater::load_check_state().unwrap_or(updater::UpdateCheckState {
                last_checked_unix: 0,
                last_known_latest: None,
                last_known_url: None,
            });

            state.last_checked_unix = now_unix;

            if let Ok(Ok(latest)) = res {
                state.last_known_latest = Some(latest.tag_name.clone());
                state.last_known_url = Some(latest.html_url.clone());
                let _ = updater::save_check_state(&state);
                if updater::is_update_available(&current, &latest.tag_name).unwrap_or(false) {
                    eprintln!("\n{}", updater::format_update_banner(&current, &latest));
                }
            } else {
                // Even on timeout or error, update the last_checked_unix to avoid constant checking.
                let _ = updater::save_check_state(&state);
            }
        }
    }
}

/// Bundle of process-level flags every command needs. Lets us add new
/// global flags (`--cwd`, future things) without rippling through every
/// `cmd_*` signature.
struct CmdCtx {
    choice: vcs::VcsChoice,
    non_interactive: bool,
    cwd_override: Option<PathBuf>,
}

impl CmdCtx {
    fn effective_cwd(&self) -> Result<PathBuf> {
        match self.cwd_override.as_deref() {
            Some(p) => {
                if !p.exists() {
                    anyhow::bail!("--cwd path does not exist: {}", p.display());
                }
                Ok(p.to_path_buf())
            }
            None => std::env::current_dir().context("could not read current directory"),
        }
    }
}

fn cmd_list(ctx: &CmdCtx, refresh: bool) -> Result<()> {
    use owo_colors::OwoColorize;
    use renri::pr_cache;

    let opened = open_repo_backend(ctx)?;
    let worktrees = opened.list_all()?;
    if worktrees.is_empty() {
        return Ok(());
    }

    // Pull `[ui]` config so we know whether to add the PR column. The PR
    // cache is GitHub-only and fully optional; failures here downgrade to
    // an empty map rather than aborting the whole list.
    let loaded = config::Config::load(Some(&opened.repo.root)).unwrap_or_default();
    let show_pr = loaded.config.ui.show_pr;
    // Show the VCS column whenever more than one backend is in play. In
    // colocated repos this lets the user tell at a glance which side a row
    // came from (the main concrete UX bug this column was added for).
    let show_vcs = opened.is_multi();
    let prs = if show_pr {
        // Origin / branch are the same across both backends in a colocated
        // repo (they share the git store), so primary() is fine here.
        let branch = opened
            .primary()
            .current_branch()
            .unwrap_or_else(|| "main".into());
        let vcs_ctx = layout::discover_vcs_context(opened.primary(), &opened.repo.root, &branch);
        pr_cache::load_or_refresh(
            &vcs_ctx.owner,
            &vcs_ctx.repo,
            vcs_ctx.host.as_deref(),
            loaded.config.ui.pr_cache_ttl_hours,
            refresh,
        )
    } else {
        Default::default()
    };

    let rows: Vec<ListRow> = worktrees
        .iter()
        .map(|w| {
            let mut r = ListRow::from(w);
            if show_pr {
                if let Some(pr) = pr_cache::lookup_for_worktree(w, &prs) {
                    r.pr = Some(format!("#{}", pr.number));
                    r.pr_state = Some(pr.state.clone());
                }
            }
            r
        })
        .collect();

    // `chars().count()` so multi-byte names align correctly. Doesn't account
    // for east-asian wide characters; that's a follow-up if it bites.
    let name_w = rows
        .iter()
        .map(|r| r.name.chars().count())
        .max()
        .unwrap_or(0)
        .max(4);
    let pr_w = if show_pr {
        rows.iter()
            .map(|r| r.pr.as_deref().map_or(0, |s| s.chars().count()))
            .max()
            .unwrap_or(0)
            .max(2)
    } else {
        0
    };

    // Header on stdout so the whole table is on one stream — piping or
    // redirecting `renri list` keeps the column legend.
    let header_name = format!("{:name_w$}", "NAME").dimmed().to_string();
    let header_st = "ST".dimmed().to_string();
    let header_desc = "DESCRIPTION".dimmed().to_string();
    let header_vcs = if show_vcs {
        format!("  {}", "VCS".dimmed())
    } else {
        String::new()
    };
    if show_pr {
        let header_pr = format!("{:pr_w$}", "PR").dimmed().to_string();
        println!("  {header_name}{header_vcs}  {header_st}  {header_pr}  {header_desc}");
    } else {
        println!("  {header_name}{header_vcs}  {header_st}  {header_desc}");
    }

    for row in &rows {
        // Leading marker: highlights the *role* of the row (main / stale).
        let marker = if row.stale {
            "⚠".yellow().to_string()
        } else if row.main {
            "★".green().to_string()
        } else {
            " ".to_string()
        };

        let name = if row.main {
            row.name.green().bold().to_string()
        } else if row.stale {
            row.name.yellow().to_string()
        } else {
            row.name.clone()
        };

        // STATUS icon: state of the working copy.
        //   ✓ clean (no WC changes)
        //   ● has uncommitted changes
        //   ‼ conflict — outranks dirty
        //   ⋯ stale / unknown
        let status = if row.stale {
            "⋯".dimmed().to_string()
        } else if row.conflict {
            "‼".red().bold().to_string()
        } else if row.dirty {
            "●".yellow().to_string()
        } else {
            "✓".green().to_string()
        };

        let desc = if row.stale {
            "(stale — directory missing)".yellow().italic().to_string()
        } else if row.desc.is_empty() {
            "(no description)".dimmed().italic().to_string()
        } else {
            row.desc.clone()
        };

        let name_pad = " ".repeat(name_w.saturating_sub(row.name.chars().count()));

        // VCS cell: dim, fixed width 3 (`jj ` / `git`). Pad BEFORE
        // dimming — `dimmed()` wraps in ANSI escape codes which the
        // `{:N}` width specifier counts as visible characters, breaking
        // alignment of every column to its right.
        let vcs_cell = if show_vcs {
            let short = format!("{:3}", vcs::kind_short(row.vcs));
            format!("  {}", short.dimmed())
        } else {
            String::new()
        };

        if show_pr {
            // PR cell: number colored by state, dim placeholder when absent.
            let pr_cell = match (row.pr.as_deref(), row.pr_state.as_deref()) {
                (Some(n), Some("OPEN")) => n.green().to_string(),
                (Some(n), Some("MERGED")) => n.dimmed().to_string(),
                (Some(n), Some("CLOSED")) => n.red().dimmed().to_string(),
                (Some(n), _) => n.to_string(),
                _ => "—".dimmed().to_string(),
            };
            let pr_raw_len = row.pr.as_deref().map_or(1, |s| s.chars().count());
            let pr_pad = " ".repeat(pr_w.saturating_sub(pr_raw_len));
            println!("{marker} {name}{name_pad}{vcs_cell}  {status}   {pr_cell}{pr_pad}  {desc}");
        } else {
            println!("{marker} {name}{name_pad}{vcs_cell}  {status}   {desc}");
        }
    }
    Ok(())
}

struct ListRow {
    name: String,
    desc: String,
    main: bool,
    stale: bool,
    dirty: bool,
    conflict: bool,
    pr: Option<String>,
    pr_state: Option<String>,
    vcs: vcs::Kind,
}

impl From<&vcs::Worktree> for ListRow {
    fn from(w: &vcs::Worktree) -> Self {
        Self {
            name: w.name.clone(),
            desc: w.desc.clone().unwrap_or_default(),
            main: w.is_main,
            stale: w.is_stale,
            dirty: w.dirty,
            conflict: w.conflict,
            pr: None,
            pr_state: None,
            vcs: w.vcs,
        }
    }
}

/// One repo opened against potentially multiple backends. Colocated repos
/// under `--vcs auto` carry **two** backends (jj first, git second) so
/// union-aware verbs (`list` / `prune` / `sync`) can show both stores
/// without losing the long-standing jj-priority policy: `primary()`
/// returns `backends[0]` and that's what single-store verbs (`add`,
/// `config show`, `gh-repo`) use.
struct OpenedRepo {
    repo: vcs::Repo,
    backends: Vec<(vcs::Kind, Box<dyn vcs::Backend>)>,
}

impl OpenedRepo {
    /// First-choice backend. Always present (the constructor refuses to
    /// build an empty `backends`). For colocated + Auto this is jj,
    /// matching the policy documented in CLAUDE.md.
    fn primary(&self) -> &dyn vcs::Backend {
        self.backends[0].1.as_ref()
    }

    /// True when more than one backend is in play (colocated + Auto).
    /// Drives whether `list` shows a VCS column and whether the picker
    /// prefixes rows with their backend.
    fn is_multi(&self) -> bool {
        self.backends.len() > 1
    }

    /// Union of every backend's `list()`. Rows are already tagged via
    /// `Worktree::vcs` so the caller can dispatch per-row.
    ///
    /// Bails on the first backend failure rather than silently masking
    /// half the view — a missing `jj` binary in a colocated repo is a
    /// configuration problem worth surfacing, not papering over.
    fn list_all(&self) -> Result<Vec<vcs::Worktree>> {
        let mut all = Vec::new();
        for (_, b) in &self.backends {
            all.extend(b.list()?);
        }
        Ok(all)
    }

    /// Backend that produced a particular row. Used by `remove` (and
    /// anything else that has to dispatch to the right store given a
    /// specific worktree).
    fn backend_for(&self, kind: vcs::Kind) -> Option<&dyn vcs::Backend> {
        self.backends
            .iter()
            .find(|(k, _)| *k == kind)
            .map(|(_, b)| b.as_ref())
    }
}

fn open_repo_backend(ctx: &CmdCtx) -> Result<OpenedRepo> {
    let cwd = ctx.effective_cwd()?;

    // Fast path: cwd (or `--cwd <path>`) is inside a git/jj repo.
    let repo = if let Some(repo) = vcs::detect(&cwd) {
        repo
    } else {
        // Slow path: cwd is *outside* any repo. Walk the configured worktree
        // root for projects renri already manages and let the user pick one.
        let picked_path = pick_managed_project(ctx)?;
        vcs::detect(&picked_path).with_context(|| {
            format!(
                "discovered project {} is no longer a git/jj repo (was it removed?)",
                picked_path.display()
            )
        })?
    };

    let kinds = vcs::select_kinds(repo.kind, ctx.choice)?;
    let backends = kinds
        .into_iter()
        .map(|k| vcs::open_backend(&repo, k).map(|b| (k, b)))
        .collect::<Result<Vec<_>>>()?;
    Ok(OpenedRepo { repo, backends })
}

/// Resolve a worktree root from the user's *global* config (or the built-in
/// default), then walk it for managed projects and prompt for one.
///
/// Project-local `renri.toml` is intentionally not consulted here — we're
/// outside the repo, so there's no project to read it from. The global
/// config + built-in default are enough to know where worktrees live.
fn pick_managed_project(ctx: &CmdCtx) -> Result<PathBuf> {
    let mut engine = Engine::new();
    // No repo root → only global config + defaults are considered.
    let loaded = config::Config::load_with_engine(None, &mut engine).unwrap_or_default();
    let root = layout::render_worktree_root(
        &mut engine,
        &system_context(),
        loaded.config.layout.worktree_root.as_deref(),
    )?;

    let projects = discovery::scan(&root);
    if projects.is_empty() {
        anyhow::bail!(
            "not inside a git or jj repository, and no managed projects found under {}.\n\
             tip: cd into a repo, pass `-C <path>`, or `renri add <branch>` from a repo first.",
            display_path(&root)
        );
    }

    if ctx.non_interactive {
        anyhow::bail!(
            "not inside a git or jj repository and --non-interactive set; \
             pass `-C <path>` or cd into a repo"
        );
    }

    let picked = pick_project_interactive(&projects)?;
    Ok(picked.entry_path().to_path_buf())
}

fn pick_project_interactive(projects: &[discovery::Project]) -> Result<&discovery::Project> {
    let items: Vec<ProjectItem<'_>> = projects.iter().map(ProjectItem).collect();
    let picked = inquire::Select::new("project:", items)
        .prompt()
        .context("interactive pick cancelled")?;
    Ok(picked.0)
}

struct ProjectItem<'a>(&'a discovery::Project);

impl std::fmt::Display for ProjectItem<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let n = self.0.worktrees.len();
        let suffix = if n == 1 { "worktree" } else { "worktrees" };
        write!(f, "{}  ({n} {suffix})", self.0.label)
    }
}

fn cmd_config_show(ctx: &CmdCtx) -> Result<()> {
    use owo_colors::OwoColorize;

    let opened = open_repo_backend(ctx)?;
    let mut engine = Engine::new();

    let loaded = config::Config::load_with_engine(Some(&opened.repo.root), &mut engine)?;

    let branch_opt = opened.primary().current_branch();
    // The placeholder we hand the layout renderer when there's no current
    // branch. Renders as `…/(none)` in the resolved path, which we suppress
    // below since the path isn't meaningful without a real branch.
    let branch_display = branch_opt.clone().unwrap_or_else(|| "(none)".into());
    let vcs_ctx =
        layout::discover_vcs_context(opened.primary(), &opened.repo.root, &branch_display);

    let resolved_path = layout::render_path(
        &mut engine,
        &system_context(),
        &vcs_ctx,
        loaded.config.layout.worktree_root.as_deref(),
        loaded.config.layout.worktree_path.as_deref(),
    )?;

    let worktree_root = loaded
        .config
        .layout
        .worktree_root
        .as_deref()
        .unwrap_or(layout::DEFAULT_WORKTREE_ROOT);
    let worktree_path_tmpl = loaded
        .config
        .layout
        .worktree_path
        .as_deref()
        .unwrap_or(layout::DEFAULT_WORKTREE_PATH);

    println!("{}", "repo".dimmed());
    let backends_label = opened
        .backends
        .iter()
        .map(|(_, b)| b.name())
        .collect::<Vec<_>>()
        .join(", ");
    if opened.is_multi() {
        // Surface both stores so the user can see at a glance that the
        // colocated repo will be unioned by `list` / `prune` etc.
        println!(
            "  backends:          {backends_label} (primary: {})",
            opened.primary().name()
        );
    } else {
        println!("  backend:           {backends_label}");
    }
    println!("  root:              {}", display_path(&opened.repo.root));

    println!();
    println!("{}", "vcs context".dimmed());
    println!(
        "  host:              {}",
        vcs_ctx.host.as_deref().unwrap_or("(none)")
    );
    println!("  owner:             {}", vcs_ctx.owner);
    println!("  repo:              {}", vcs_ctx.repo);
    match branch_opt.as_deref() {
        Some(b) => println!("  branch:            {b}"),
        None => println!("  branch:            {}", "(no bookmark at @)".dimmed()),
    }

    println!();
    println!("{}", "layout (template)".dimmed());
    println!("  worktree_root:     {worktree_root}");
    println!("  worktree_path:     {worktree_path_tmpl}");

    if branch_opt.is_some() {
        println!();
        println!("{}", "layout (resolved for current branch)".dimmed());
        println!("  → {}", display_path(&resolved_path));
    }

    println!();
    println!("{}", "hooks".dimmed());
    println!(
        "  post_create:       {}",
        loaded.config.hooks.post_create.len()
    );
    println!(
        "  pre_remove:        {}",
        loaded.config.hooks.pre_remove.len()
    );

    println!();
    if loaded.sources.is_empty() {
        println!(
            "{}        {}",
            "config sources".dimmed(),
            "(none — using built-in defaults)".dimmed()
        );
    } else {
        println!("{}", "config sources".dimmed());
        for s in &loaded.sources {
            println!("  {}", display_path(s));
        }
    }
    Ok(())
}

fn cmd_add(ctx: &CmdCtx, name: Option<String>, from: Option<String>) -> Result<()> {
    // Trim the user-supplied `--from`. An empty string after trim is the
    // signal for "open the picker" (clap's `default_missing_value = ""`),
    // so preserve `Some("")` here instead of filtering it out.
    let from = from.map(|s| s.trim().to_string());

    let name = match name {
        Some(n) => n.trim().to_string(),
        None => prompt_branch_name(ctx.non_interactive)?,
    };
    if name.is_empty() {
        anyhow::bail!("branch / bookmark name must not be empty");
    }

    let opened = open_repo_backend(ctx)?;
    let mut engine = Engine::new();

    let loaded = config::Config::load_with_engine(Some(&opened.repo.root), &mut engine)?;

    let vcs_ctx = layout::discover_vcs_context(opened.primary(), &opened.repo.root, &name);
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
        Some("") => Some(prompt_base_ref(opened.primary(), ctx.non_interactive)?),
        Some(ref_str) => Some(ref_str.to_string()),
    };

    let strategy = if opened.primary().branch_exists(&name) {
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

    println!("creating worktree at {}", display_path(&path));
    opened.primary().add(&path, strategy)?;

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

    println!("done. {}", display_path(&path));
    Ok(())
}

fn cmd_cd(ctx: &CmdCtx, name: Option<String>) -> Result<()> {
    let opened = open_repo_backend(ctx)?;
    let worktrees = opened.list_all()?;
    let picked = picker::resolve(
        &worktrees,
        name.as_deref(),
        ctx.non_interactive,
        "switch to:",
    )?;

    // Two modes:
    //   1. Inside the shell wrapper (`RENRI_SHELL_WRAPPER=1`): print the
    //      path so the wrapper's parent shell can `cd` to it.
    //   2. Outside the wrapper: spawn the user's `$SHELL` (or `pwsh` on
    //      Windows) with cwd = worktree path, so plain `renri cd <name>`
    //      Just Works without rc-file setup. The user `exit`s to come back.
    if std::env::var_os("RENRI_SHELL_WRAPPER").is_some() {
        println!("{}", display_path(&picked.path));
        return Ok(());
    }

    spawn_subshell_in(&picked.path)
}

fn spawn_subshell_in(path: &std::path::Path) -> Result<()> {
    let shell = pick_subshell();
    eprintln!(
        "renri: spawning {shell} in {path}\n\
         renri: tip — install the shell wrapper for direct cd in your current shell:\n\
         renri:       eval \"$(renri shell-init bash)\"        # or zsh / fish / powershell\n\
         renri:       (or `renri shell-init --install bash` to write it to your rc file)",
        shell = shell,
        path = display_path(path),
    );
    let status = std::process::Command::new(&shell)
        .current_dir(path)
        .status()
        .with_context(|| format!("failed to spawn `{shell}`"))?;
    if !status.success() {
        // The user's exit code; pass through but don't error-bail.
        std::process::exit(status.code().unwrap_or(1));
    }
    Ok(())
}

fn pick_subshell() -> String {
    if cfg!(windows) {
        std::env::var("COMSPEC").unwrap_or_else(|_| "pwsh".into())
    } else {
        std::env::var("SHELL").unwrap_or_else(|_| "bash".into())
    }
}

fn cmd_remove(ctx: &CmdCtx, name: Option<String>) -> Result<()> {
    let opened = open_repo_backend(ctx)?;
    let mut engine = Engine::new();
    let loaded = config::Config::load_with_engine(Some(&opened.repo.root), &mut engine)?;

    let worktrees = opened.list_all()?;
    let picked =
        picker::resolve(&worktrees, name.as_deref(), ctx.non_interactive, "remove:")?.clone();

    if picked.is_main {
        anyhow::bail!(
            "{} is the main worktree and cannot be removed via renri",
            picked.name
        );
    }

    let pre = &loaded.config.hooks.pre_remove;
    if !pre.is_empty() {
        let branch = picked.branch.clone().unwrap_or_else(|| picked.name.clone());
        let vcs_ctx = layout::discover_vcs_context(opened.primary(), &opened.repo.root, &branch);
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

    // Dispatch to the backend that produced this row. In a colocated repo,
    // a `git worktree remove` against a jj workspace (or vice versa) would
    // either fail or hit the wrong store; the per-row tag is what makes
    // the union safe to act on.
    let backend = opened.backend_for(picked.vcs).ok_or_else(|| {
        anyhow::anyhow!(
            "internal: row tagged {:?} but no matching backend is open",
            picked.vcs
        )
    })?;
    println!("removing {}", display_path(&picked.path));
    backend.remove(&picked.path, false)?;
    Ok(())
}

fn cmd_exec(ctx: &CmdCtx, name: Option<String>, argv: Vec<String>) -> Result<()> {
    if argv.is_empty() {
        anyhow::bail!("no command was given (use `renri exec <name> -- cmd args...`)");
    }

    let opened = open_repo_backend(ctx)?;
    let worktrees = opened.list_all()?;
    let picked = picker::resolve(&worktrees, name.as_deref(), ctx.non_interactive, "exec in:")?;

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

fn cmd_init(cwd_override: Option<&std::path::Path>, force: bool) -> Result<()> {
    let cwd = match cwd_override {
        Some(p) => p.to_path_buf(),
        None => std::env::current_dir().context("could not read current directory")?,
    };
    let target = cwd.join("renri.toml");

    if target.exists() && !force {
        anyhow::bail!(
            "{} already exists; pass --force to overwrite",
            target.display()
        );
    }

    std::fs::write(&target, INIT_TEMPLATE)
        .with_context(|| format!("writing {}", target.display()))?;
    println!("wrote {}", display_path(&target));
    Ok(())
}

const INIT_TEMPLATE: &str = r#"# renri.toml
# Schema and examples: https://github.com/yukimemi/renri
"#;

fn cmd_prune(ctx: &CmdCtx) -> Result<()> {
    let opened = open_repo_backend(ctx)?;
    // Run prune on every open backend. Per-backend failure is logged and
    // the loop continues — pruning is best-effort (matches the existing
    // resilience policy for `prune` in CLAUDE.md), and a busted jj
    // shouldn't prevent git-side cleanup. But if *every* backend fails
    // we can't honestly say "nothing to prune", so bail at the end.
    let mut any_output = false;
    let mut any_success = false;
    let mut any_failure = false;
    let label_each = opened.is_multi();
    for (kind, backend) in &opened.backends {
        match backend.prune() {
            Ok(output) => {
                any_success = true;
                let trimmed = output.trim();
                if !trimmed.is_empty() {
                    if label_each {
                        println!("[{}] {trimmed}", vcs::kind_short(*kind));
                    } else {
                        println!("{trimmed}");
                    }
                    any_output = true;
                }
            }
            Err(e) => {
                any_failure = true;
                tracing::error!(backend = backend.name(), error = %e, "prune failed");
                eprintln!("[{}] prune failed: {e}", vcs::kind_short(*kind));
            }
        }
    }
    if any_failure && !any_success {
        anyhow::bail!("prune failed on every backend");
    }
    if !any_output && !any_failure {
        println!("nothing to prune");
    }
    Ok(())
}

fn cmd_sync(ctx: &CmdCtx) -> Result<()> {
    let opened = open_repo_backend(ctx)?;
    // Fetch from every open backend. In a colocated repo `git fetch` and
    // `jj git fetch` both reach into the same git store, but they update
    // jj-bookmarks vs git-refs through different code paths so calling
    // both is the safe thing to keep both views consistent.
    //
    // Bail when *every* backend errors so shell wrappers like
    // `renri sync && deploy` get a non-zero exit instead of running
    // deploy on a fully-failed fetch (the previous single-backend
    // implementation propagated failure via `?`).
    let mut any_output = false;
    let mut any_success = false;
    let mut any_failure = false;
    let label_each = opened.is_multi();
    for (kind, backend) in &opened.backends {
        match backend.fetch() {
            Ok(output) => {
                any_success = true;
                let trimmed = output.trim();
                if !trimmed.is_empty() {
                    if label_each {
                        println!("[{}] {trimmed}", vcs::kind_short(*kind));
                    } else {
                        println!("{trimmed}");
                    }
                    any_output = true;
                }
            }
            Err(e) => {
                any_failure = true;
                tracing::error!(backend = backend.name(), error = %e, "fetch failed");
                eprintln!("[{}] fetch failed: {e}", vcs::kind_short(*kind));
            }
        }
    }
    if any_failure && !any_success {
        anyhow::bail!("fetch failed on every backend");
    }
    if !any_output && !any_failure {
        println!("fetched (nothing changed)");
    }
    Ok(())
}

/// Print `<owner>/<repo>` for the cwd's repo, or nothing on any failure
/// (no repo, no origin, unparseable origin URL).
///
/// Deliberately silent + always exits 0 so the shell wrapper can splat
/// the result into `GH_REPO` (or `unset` it cleanly when empty) without
/// noise. We also bypass `open_repo_backend` so a hook context never
/// triggers an interactive picker — `vcs::detect()` is a pure walk-up
/// that returns `None` when there's nothing to find.
fn cmd_gh_repo(ctx: &CmdCtx) -> Result<()> {
    let Ok(cwd) = ctx.effective_cwd() else {
        return Ok(());
    };
    let Some(repo) = vcs::detect(&cwd) else {
        return Ok(());
    };
    let Ok(kind) = vcs::select_kind(repo.kind, ctx.choice) else {
        return Ok(());
    };
    let Ok(backend) = vcs::open_backend(&repo, kind) else {
        return Ok(());
    };
    let Some(origin) = backend.origin_url() else {
        return Ok(());
    };
    let parsed = layout::parse_origin(&origin);
    if parsed.owner.is_empty() || parsed.repo.is_empty() {
        return Ok(());
    }
    println!("{}/{}", parsed.owner, parsed.repo);
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
