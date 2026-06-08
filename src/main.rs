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
    ///
    /// Prints a details panel (branch / HEAD / dirty / conflict / PR / …)
    /// for every target before doing anything so the user can sanity-check
    /// the choice; the panel is also what `--merged` uses to summarize a
    /// batch. Pass `-y` to skip the confirmation prompt.
    #[command(alias = "rm")]
    Remove {
        /// Worktree name. If omitted, open a fuzzy picker.
        /// Ignored when `--merged` is set.
        name: Option<String>,

        /// Skip the interactive confirmation prompt.
        #[arg(long, short = 'y')]
        yes: bool,

        /// Pass `--force` to the underlying backend, allowing removal of
        /// worktrees with uncommitted changes / conflicts. Also unblocks
        /// dirty / conflict / locked rows in `--merged` mode (which
        /// otherwise skips them).
        #[arg(long, short = 'f')]
        force: bool,

        /// Remove every worktree whose GitHub PR is merged or closed.
        /// Dirty / conflicted / locked / main rows are skipped with a
        /// warning unless `--force` is also passed. Requires the `gh`
        /// CLI and `[ui] show_pr = true` (or just a GitHub origin).
        #[arg(long)]
        merged: bool,

        /// Bypass the PR cache and re-fetch from GitHub before reading
        /// PR state, so the details panel reflects current state. Matches
        /// `renri list --refresh`. Ignored with `--merged`, which always
        /// re-fetches (acting on a stale cache would skip just-merged
        /// worktrees).
        #[arg(long)]
        refresh: bool,
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

    /// Open the GitHub PR for a worktree in the browser.
    ///
    /// Looks up the PR in the same on-disk cache `renri list` uses
    /// (`<cache_dir>/renri/<owner>__<repo>/pr-cache.json`), so it
    /// inherits whatever staleness the cache has. Pass `--refresh` to
    /// force-refetch via `gh` first.
    Pr {
        /// Worktree name. If omitted, open a fuzzy picker over the
        /// union of every backend's worktrees.
        name: Option<String>,

        /// Bypass the PR cache and re-fetch from GitHub via `gh` before
        /// looking up. Mirrors `renri list --refresh`.
        #[arg(long)]
        refresh: bool,
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
        Command::Remove {
            name,
            yes,
            force,
            merged,
            refresh,
        } => cmd_remove(&ctx, name, yes, force, merged, refresh),
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
        Command::Pr { name, refresh } => cmd_pr(&ctx, name, refresh),
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
        checker: updater::Checker,
        latest: kaishin::LatestRelease,
    },
    /// A background check is currently in progress.
    Pending {
        checker: updater::Checker,
        rx: std::sync::mpsc::Receiver<Result<Option<kaishin::LatestRelease>>>,
        /// A newer version already known from the local cache.
        cached_latest: Option<kaishin::LatestRelease>,
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

    let checker = updater::Checker::new().ok()?;
    let interval = loaded
        .config
        .ui
        .update_check_interval
        .as_deref()
        .and_then(|s| kaishin::parse_interval(s).ok())
        .unwrap_or_else(updater::default_interval);

    let checker = checker.interval(interval);

    if !checker.should_check() {
        if let Some(latest) = checker.cached_update() {
            return Some(AutoUpdateHandle::CachedAvailable { checker, latest });
        }
        return None;
    }

    let cached_latest = checker.cached_update();
    let (tx, rx) = std::sync::mpsc::channel();
    let checker_clone = updater::Checker::new().ok()?.interval(interval);
    std::thread::spawn(move || {
        let _ = tx.send(checker_clone.check_and_save());
    });

    Some(AutoUpdateHandle::Pending {
        checker,
        rx,
        cached_latest,
    })
}

/// Waits for the background update check to complete (with a short timeout) and prints a banner if an update is available.
fn finalize_auto_update_check(handle: AutoUpdateHandle) {
    match handle {
        AutoUpdateHandle::CachedAvailable { checker, latest } => {
            eprintln!("\n{}", checker.format_banner(&latest));
        }
        AutoUpdateHandle::Pending {
            checker,
            rx,
            cached_latest,
        } => {
            // Wait for 1 second.
            // kaishin 0.4 で check_and_save が Option を返すようになったので、
            // Ok(Ok(None)) = 「fetch 成功で更新無し」を区別できる。
            let res = rx.recv_timeout(std::time::Duration::from_secs(1));
            match res {
                Ok(Ok(Some(latest))) => {
                    eprintln!("\n{}", checker.format_banner(&latest));
                }
                Ok(Ok(None)) => {
                    // fetch は成功したがアップデート無し。 cache へのフォールバック
                    // も不要（最新が現在版に追いついた直後など）。
                }
                _ => {
                    // タイムアウトや fetch エラー時のみ cache を試す。
                    if let Some(latest) = cached_latest {
                        eprintln!("\n{}", checker.format_banner(&latest));
                    }
                }
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
    // Resolve the GitHub identity of the repo once when the PR column
    // is on. Held outside the cache lookup so the OSC 8 hyperlink path
    // below can build PR URLs from the same owner/repo.
    let vcs_ctx_for_pr = if show_pr {
        // Origin / branch are the same across both backends in a colocated
        // repo (they share the git store), so primary() is fine here.
        let branch = opened
            .primary()
            .current_branch()
            .unwrap_or_else(|| "main".into());
        Some(layout::discover_vcs_context(
            opened.primary(),
            &opened.repo.root,
            &branch,
        ))
    } else {
        None
    };
    let prs = if let Some(ctx) = vcs_ctx_for_pr.as_ref() {
        pr_cache::load_or_refresh(
            &ctx.owner,
            &ctx.repo,
            ctx.host.as_deref(),
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
                    r.pr_number = Some(pr.number);
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
            // Wrap real PRs in an OSC 8 hyperlink so terminals that
            // support it (wezterm / kitty / iTerm2 / Windows Terminal /
            // VTE) make `#42` Ctrl-clickable. Terminals that don't
            // recognize the sequence drop it and render the cell as
            // before. The padding goes OUTSIDE the hyperlink so the
            // clickable region only covers the visible `#N`.
            let pr_cell = match (row.pr_number, vcs_ctx_for_pr.as_ref()) {
                (Some(n), Some(ctx)) if !ctx.owner.is_empty() && !ctx.repo.is_empty() => {
                    let url = pr_cache::pr_url(ctx.host.as_deref(), &ctx.owner, &ctx.repo, n);
                    pr_cache::osc8_hyperlink(&url, &pr_cell)
                }
                _ => pr_cell,
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
    /// Raw PR number (without `#`) used to build the OSC 8 hyperlink
    /// URL. Held alongside `pr` so the renderer doesn't have to parse
    /// `#42` back out of the display string.
    pr_number: Option<u64>,
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
            pr_number: None,
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

fn cmd_pr(ctx: &CmdCtx, name: Option<String>, refresh: bool) -> Result<()> {
    use renri::pr_cache;

    let opened = open_repo_backend(ctx)?;
    let worktrees = opened.list_all()?;
    let picked = picker::resolve(
        &worktrees,
        name.as_deref(),
        ctx.non_interactive,
        "open PR for:",
    )?;

    // Resolve owner / repo / host from origin via the primary backend
    // (origin is shared across both backends in a colocated repo).
    let branch_for_ctx = picked.branch.clone().unwrap_or_else(|| picked.name.clone());
    let vcs_ctx =
        layout::discover_vcs_context(opened.primary(), &opened.repo.root, &branch_for_ctx);
    if vcs_ctx.owner.is_empty() || vcs_ctx.repo.is_empty() {
        anyhow::bail!(
            "could not determine GitHub owner/repo from origin remote; \
             `renri pr` only works on GitHub-hosted repos"
        );
    }

    // Use the same on-disk PR cache `renri list` uses so behavior is
    // consistent and we don't pay the gh-fork unless asked.
    let loaded = config::Config::load(Some(&opened.repo.root)).unwrap_or_default();
    let prs = pr_cache::load_or_refresh(
        &vcs_ctx.owner,
        &vcs_ctx.repo,
        vcs_ctx.host.as_deref(),
        loaded.config.ui.pr_cache_ttl_hours,
        refresh,
    );
    let pr = pr_cache::lookup_for_worktree(picked, &prs).ok_or_else(|| {
        anyhow::anyhow!(
            "no PR found for `{}` in the cache. \
             try `renri pr {} --refresh` to re-fetch from GitHub, \
             or check that the branch was actually pushed and a PR was opened.",
            picked.name,
            picked.name
        )
    })?;

    let url = pr_cache::pr_url(
        vcs_ctx.host.as_deref(),
        &vcs_ctx.owner,
        &vcs_ctx.repo,
        pr.number,
    );
    eprintln!("opening PR #{} ({}) — {url}", pr.number, pr.state);
    open_in_browser(&url)
}

/// Hand a URL off to the OS to open in the user's default browser.
/// Uses platform-native commands so we don't add a dependency just for
/// this one call: `rundll32 url.dll,FileProtocolHandler` on Windows
/// (Microsoft's documented programmatic URL opener), `open` on macOS,
/// `xdg-open` and friends on Linux.
#[cfg(target_os = "windows")]
fn open_in_browser(url: &str) -> Result<()> {
    // `rundll32 url.dll,FileProtocolHandler <url>` is the documented
    // way to open a URL with the user's default protocol handler
    // without going through `cmd /C start`. Avoiding cmd means we
    // don't have to think about cmd's metacharacter handling at all
    // (e.g. an `&` in a query string getting interpreted as a command
    // separator) — defensive even though `pr_cache::pr_url` only ever
    // produces `https://github.com/<owner>/<repo>/pull/<number>`,
    // none of which can contain shell-significant characters.
    let status = std::process::Command::new("rundll32")
        .args(["url.dll,FileProtocolHandler", url])
        .status()
        .with_context(|| format!("spawning `rundll32 url.dll,FileProtocolHandler {url}`"))?;
    if !status.success() {
        anyhow::bail!("failed to open `{url}` in browser");
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn open_in_browser(url: &str) -> Result<()> {
    let status = std::process::Command::new("open")
        .arg(url)
        .status()
        .with_context(|| format!("spawning `open {url}`"))?;
    if !status.success() {
        anyhow::bail!("failed to open `{url}` in browser");
    }
    Ok(())
}

#[cfg(all(unix, not(target_os = "macos")))]
fn open_in_browser(url: &str) -> Result<()> {
    // Try in order: xdg-open (most desktops), then gio open (newer
    // GNOME), then wslview (WSL bridges to the host browser).
    for opener in ["xdg-open", "gio", "wslview"] {
        let mut cmd = std::process::Command::new(opener);
        if opener == "gio" {
            cmd.arg("open");
        }
        cmd.arg(url);
        if let Ok(status) = cmd.status() {
            if status.success() {
                return Ok(());
            }
        }
    }
    anyhow::bail!("no suitable URL opener found (install xdg-utils, or open manually): {url}");
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

fn cmd_remove(
    ctx: &CmdCtx,
    name: Option<String>,
    yes: bool,
    force: bool,
    merged: bool,
    refresh: bool,
) -> Result<()> {
    if merged {
        if name.is_some() {
            anyhow::bail!("--merged operates on the full list; pass either <name> or --merged");
        }
        // `--merged` always re-fetches the PR cache regardless of `--refresh`
        // (see cmd_remove_merged) — a stale cache silently drops freshly
        // merged PRs and makes every worktree "not a candidate".
        return cmd_remove_merged(ctx, yes, force);
    }

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

    // Resolve VCS context once — used both for the PR lookup and (if pre_remove
    // hooks exist) the hook runner. Lifting it above the PR fetch keeps the
    // call sites in sync about which branch identifies the worktree.
    let branch_for_ctx = picked.branch.clone().unwrap_or_else(|| picked.name.clone());
    let vcs_ctx =
        layout::discover_vcs_context(opened.primary(), &opened.repo.root, &branch_for_ctx);

    // Best-effort PR lookup: even when `[ui] show_pr = false` we still try
    // the cache so a user who has list-with-PRs configured elsewhere gets
    // the same signal here. No `gh` / no cache → silently no PR info.
    // `--refresh` forwards through so the details panel reflects current
    // state, matching `renri list --refresh` semantics.
    let prs = load_pr_cache_for_repo(&opened, &loaded.config, &vcs_ctx, refresh);
    let pr_info = renri::pr_cache::lookup_for_worktree(&picked, &prs);
    let pr_url = pr_info.map(|p| {
        renri::pr_cache::pr_url(
            vcs_ctx.host.as_deref(),
            &vcs_ctx.owner,
            &vcs_ctx.repo,
            p.number,
        )
    });

    println!();
    print_worktree_details(&picked, pr_info, pr_url.as_deref());
    println!();

    if !yes && !confirm_remove(ctx, "remove this worktree?")? {
        println!("aborted");
        return Ok(());
    }

    remove_one(
        &opened,
        &picked,
        &loaded.config.hooks.pre_remove,
        &mut engine,
        force,
    )?;
    Ok(())
}

/// Auto-remove every worktree whose GitHub PR is `MERGED` or `CLOSED`.
///
/// Hard skips: the main worktree, and (unless `--force`) anything dirty /
/// conflicted / locked. Those land in a warning block printed before the
/// summary so the user knows what was *not* swept.
///
/// Bails before touching anything when (a) the repo's origin isn't on
/// GitHub, (b) the PR cache is empty (no `gh`, or genuinely no PRs), or
/// (c) `--non-interactive` is set without `--yes`.
///
/// Unlike single-target removal, this always re-fetches the PR cache (the
/// `--refresh` flag is implied). A TTL-fresh-but-behind cache would silently
/// miss PRs merged since the last fetch, filtering out every worktree and
/// printing a misleading "nothing to remove".
fn cmd_remove_merged(ctx: &CmdCtx, yes: bool, force: bool) -> Result<()> {
    use owo_colors::OwoColorize;
    use renri::pr_cache;

    let opened = open_repo_backend(ctx)?;
    let mut engine = Engine::new();
    let loaded = config::Config::load_with_engine(Some(&opened.repo.root), &mut engine)?;

    let worktrees = opened.list_all()?;
    if worktrees.is_empty() {
        println!("no worktrees");
        return Ok(());
    }

    let branch = opened
        .primary()
        .current_branch()
        .unwrap_or_else(|| "main".into());
    let vcs_ctx = layout::discover_vcs_context(opened.primary(), &opened.repo.root, &branch);
    if vcs_ctx.owner.is_empty() || vcs_ctx.repo.is_empty() {
        anyhow::bail!(
            "could not determine GitHub owner/repo from origin remote; \
             --merged only works on GitHub-hosted repos"
        );
    }

    // Empty `prs` is ambiguous (no `gh` / network failure / genuinely zero
    // open PRs), so we deliberately don't bail here. The downstream
    // "nothing to remove" message after candidate filtering is accurate
    // either way, and conflating "tool missing" with "no PRs to act on"
    // produces misleading errors in a fresh repo.
    //
    // Always refresh (`true`): the swept set is derived entirely from PR
    // state, so acting on a stale cache would skip just-merged worktrees.
    let prs = load_pr_cache_for_repo(&opened, &loaded.config, &vcs_ctx, true);

    let mut candidates: Vec<(vcs::Worktree, pr_cache::PrInfo, String)> = Vec::new();
    let mut skipped: Vec<(vcs::Worktree, Option<pr_cache::PrInfo>, String)> = Vec::new();
    for w in worktrees.iter() {
        if w.is_main {
            // main is never a remove candidate — skip silently so it
            // doesn't clutter the warning block in every run.
            continue;
        }
        let Some(pr) = pr_cache::lookup_for_worktree(w, &prs) else {
            continue;
        };
        if pr.state != "MERGED" && pr.state != "CLOSED" {
            continue;
        }
        let mut reasons: Vec<&str> = Vec::new();
        if !force {
            if w.dirty {
                reasons.push("dirty");
            }
            if w.conflict {
                reasons.push("conflict");
            }
            if w.is_locked {
                reasons.push("locked");
            }
        }
        let url = pr_cache::pr_url(
            vcs_ctx.host.as_deref(),
            &vcs_ctx.owner,
            &vcs_ctx.repo,
            pr.number,
        );
        if reasons.is_empty() {
            candidates.push((w.clone(), pr.clone(), url));
        } else {
            skipped.push((w.clone(), Some(pr.clone()), reasons.join(",")));
        }
    }

    if !skipped.is_empty() {
        eprintln!("{} (pass --force to include):", "skipping".yellow().bold());
        for (w, pr, reasons) in &skipped {
            let pr_label = pr
                .as_ref()
                .map(|p| format!("#{} {}", p.number, p.state))
                .unwrap_or_else(|| "(no PR)".to_string());
            eprintln!("  {} {} — {}", w.name, pr_label.dimmed(), reasons.yellow());
        }
        eprintln!();
    }

    if candidates.is_empty() {
        println!("nothing to remove (no merged/closed PRs with a removable worktree)");
        return Ok(());
    }

    println!(
        "{} {} worktree(s) match merged/closed PRs:",
        "found".green().bold(),
        candidates.len()
    );
    for (w, pr, url) in &candidates {
        println!();
        print_worktree_details(w, Some(pr), Some(url));
    }
    println!();

    if !yes {
        let n = candidates.len();
        let plural = if n == 1 { "worktree" } else { "worktrees" };
        let prompt = format!("remove {n} {plural}?");
        if !confirm_remove(ctx, &prompt)? {
            println!("aborted");
            return Ok(());
        }
    }

    let mut ok = 0usize;
    let mut failed = 0usize;
    for (w, pr, _url) in &candidates {
        println!(
            "removing {} ({} {})",
            w.name,
            format!("#{}", pr.number).dimmed(),
            pr.state.dimmed()
        );
        match remove_one(
            &opened,
            w,
            &loaded.config.hooks.pre_remove,
            &mut engine,
            force,
        ) {
            Ok(()) => ok += 1,
            Err(e) => {
                eprintln!("  {}: {e}", "failed".red().bold());
                failed += 1;
            }
        }
    }
    println!(
        "done — removed {}, failed {}",
        ok.to_string().green(),
        if failed == 0 {
            failed.to_string().dimmed().to_string()
        } else {
            failed.to_string().red().to_string()
        }
    );
    if failed > 0 {
        anyhow::bail!("{failed} removal(s) failed");
    }
    Ok(())
}

/// Pretty-print everything we know about a worktree, structured like a
/// short YAML block. Used by both the single-pick and `--merged` flows so
/// the user sees the same shape of info either way.
fn print_worktree_details(
    w: &vcs::Worktree,
    pr: Option<&renri::pr_cache::PrInfo>,
    pr_url: Option<&str>,
) {
    use owo_colors::OwoColorize;

    let label = |s: &str| format!("  {:>9}:", s.dimmed());

    let name_styled = if w.is_main {
        w.name.green().bold().to_string()
    } else if w.is_stale {
        w.name.yellow().to_string()
    } else {
        w.name.clone()
    };
    println!("{} {}", label("name"), name_styled);

    if let Some(b) = &w.branch {
        println!("{} {}", label("branch"), b);
    } else {
        println!(
            "{} {}",
            label("branch"),
            "(detached / no bookmark)".dimmed()
        );
    }

    println!("{} {}", label("path"), display_path(&w.path));
    println!("{} {}", label("vcs"), vcs::kind_short(w.vcs));

    match (&w.head, &w.desc) {
        (Some(h), Some(d)) => println!("{} {} {}", label("head"), h, d.dimmed()),
        (Some(h), None) => println!("{} {}", label("head"), h),
        (None, _) => println!("{} {}", label("head"), "(unknown)".dimmed()),
    }

    let mut flags: Vec<String> = Vec::new();
    if w.is_main {
        flags.push("main".green().to_string());
    }
    if w.is_stale {
        flags.push("stale".yellow().to_string());
    }
    if w.dirty {
        flags.push("dirty".yellow().to_string());
    }
    if w.conflict {
        flags.push("conflict".red().bold().to_string());
    }
    if w.is_locked {
        flags.push("locked".yellow().to_string());
    }
    if w.is_bare {
        flags.push("bare".dimmed().to_string());
    }
    if flags.is_empty() {
        flags.push("clean".green().to_string());
    }
    println!("{} {}", label("status"), flags.join(" "));

    if let Some(pr) = pr {
        let state_colored = match pr.state.as_str() {
            "OPEN" => pr.state.green().to_string(),
            "MERGED" => pr.state.magenta().to_string(),
            "CLOSED" => pr.state.red().to_string(),
            _ => pr.state.clone(),
        };
        let url_part = pr_url
            .map(|u| format!("  {}", u.dimmed()))
            .unwrap_or_default();
        println!(
            "{} #{} ({}){url_part}",
            label("pr"),
            pr.number,
            state_colored
        );
    } else {
        println!("{} {}", label("pr"), "(none)".dimmed());
    }
}

/// Wrap `inquire::Confirm` so the call sites stay readable and the
/// non-interactive policy lives in exactly one place: `--non-interactive`
/// without `--yes` is a hard error rather than a silent abort, because
/// proceeding would skip the safety prompt we explicitly want to enforce.
fn confirm_remove(ctx: &CmdCtx, prompt: &str) -> Result<bool> {
    if ctx.non_interactive {
        anyhow::bail!("--non-interactive set; pass --yes to confirm the removal");
    }
    inquire::Confirm::new(prompt)
        .with_default(false)
        .prompt()
        .context("confirmation prompt cancelled")
}

/// Run pre_remove hooks then dispatch the backend's remove. Factored out
/// of `cmd_remove` so the `--merged` loop can reuse it without diverging
/// on hook semantics (a single hook spec applies to every removal).
fn remove_one(
    opened: &OpenedRepo,
    w: &vcs::Worktree,
    pre_hooks: &[config::HookSpec],
    engine: &mut Engine,
    force: bool,
) -> Result<()> {
    if !pre_hooks.is_empty() {
        let branch = w.branch.clone().unwrap_or_else(|| w.name.clone());
        let vcs_ctx = layout::discover_vcs_context(opened.primary(), &opened.repo.root, &branch);
        let base_ctx = system_context();
        let mut hr = hooks::HookRun {
            repo_root: &opened.repo.root,
            worktree_path: &w.path,
            vcs: &vcs_ctx,
            engine,
            base_ctx: &base_ctx,
        };
        println!("running {} pre_remove hook(s)", pre_hooks.len());
        hooks::run_all(pre_hooks, &mut hr)?;
    }

    // Dispatch to the backend that produced this row. In a colocated repo,
    // a `git worktree remove` against a jj workspace (or vice versa) would
    // either fail or hit the wrong store; the per-row tag is what makes
    // the union safe to act on.
    let backend = opened.backend_for(w.vcs).ok_or_else(|| {
        anyhow::anyhow!(
            "internal: row tagged {:?} but no matching backend is open",
            w.vcs
        )
    })?;
    println!("  → {}", display_path(&w.path));
    backend.remove(&w.path, force)?;
    Ok(())
}

/// Wrap `pr_cache::load_or_refresh` so both the single-pick and `--merged`
/// flows resolve PRs the same way. Returns an empty map when the repo
/// isn't on GitHub or `gh` is missing — both flows handle that downstream.
fn load_pr_cache_for_repo(
    _opened: &OpenedRepo,
    config: &config::Config,
    vcs_ctx: &layout::VcsContext,
    refresh: bool,
) -> std::collections::HashMap<String, renri::pr_cache::PrInfo> {
    if vcs_ctx.owner.is_empty() || vcs_ctx.repo.is_empty() {
        return Default::default();
    }
    renri::pr_cache::load_or_refresh(
        &vcs_ctx.owner,
        &vcs_ctx.repo,
        vcs_ctx.host.as_deref(),
        config.ui.pr_cache_ttl_hours,
        refresh,
    )
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
