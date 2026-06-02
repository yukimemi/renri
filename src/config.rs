//! Load and merge `renri.toml` via the teravars layer.
//!
//! Sources, in load order (later overrides earlier):
//!
//! 1. `<config_dir>/renri/config.toml` — global per-user defaults.
//! 2. `<repo_root>/renri.toml`         — project-local config (committed).
//!
//! teravars's `load_merged` does the per-file Tera rendering, vars
//! resolution, and deep merge; we just deserialize the result into our own
//! `Config` struct.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;
use teravars::{Engine, load_merged, system_context};
use toml::Value;

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub layout: LayoutConfig,

    #[serde(default)]
    pub hooks: HooksConfig,

    #[serde(default)]
    pub ui: UiConfig,
}

/// How renri handles a newer release detected in the background.
///
/// The default is [`AutoUpdateMode::Install`]: renri silently downloads and
/// swaps its own binary in the background, applying on the next launch. Opt
/// out per-run with the `RENRI_NO_AUTOUPDATE` env var, or persistently via
/// `[ui] auto_update = "off"` / `"notify"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AutoUpdateMode {
    /// Do nothing in the background.
    Off,
    /// Check only, print a one-line banner pointing at `renri self-update`.
    Notify,
    /// Silently download + install the newer binary in the background
    /// (default). The running process keeps the old binary; the new version
    /// applies on the next launch.
    #[default]
    Install,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UiConfig {
    /// When true, `renri list` adds a PR column populated from
    /// GitHub via `gh pr list`. Cached on disk; refreshed lazily once
    /// `pr_cache_ttl_hours` has elapsed.
    #[serde(default)]
    pub show_pr: bool,

    /// Hours before the PR cache is considered stale and refreshed on
    /// the next `renri list`. Defaults to 24.
    #[serde(default = "default_pr_cache_ttl_hours")]
    pub pr_cache_ttl_hours: u64,

    /// Background auto-update behaviour: `"off"` / `"notify"` / `"install"`.
    /// Defaults to `"install"` (silent background install). Resolve the
    /// effective mode through [`UiConfig::update_mode`], which also honours the
    /// deprecated [`UiConfig::auto_update_check`] alias.
    #[serde(default)]
    pub auto_update: Option<AutoUpdateMode>,

    /// Deprecated notify-only toggle. `true` maps to `notify`, `false` to
    /// `off`. `None` means the key was never written, so the alias stays
    /// silent. Prefer `auto_update` instead; this only takes effect when
    /// `auto_update` is unset.
    #[serde(default)]
    pub auto_update_check: Option<bool>,

    /// Interval between background update checks (e.g., "24h", "1d").
    #[serde(default)]
    pub update_check_interval: Option<String>,
}

impl UiConfig {
    /// Resolves the effective [`AutoUpdateMode`].
    ///
    /// Precedence: an explicit `auto_update` wins; otherwise the deprecated
    /// `auto_update_check` boolean is mapped (`true` → `notify`, `false` →
    /// `off`); otherwise the default [`AutoUpdateMode::Install`].
    ///
    /// This is a **pure** resolver with no side effects — it is safe to call
    /// any number of times. The one-shot deprecation warning for the legacy
    /// `auto_update_check` alias is emitted separately at config load time (see
    /// [`Config::load_with_engine`]), mirroring how rvpm warns in
    /// `parse_config` rather than inside the resolver.
    pub fn update_mode(&self) -> AutoUpdateMode {
        if let Some(mode) = self.auto_update {
            return mode;
        }
        match self.auto_update_check {
            Some(true) => AutoUpdateMode::Notify,
            Some(false) => AutoUpdateMode::Off,
            None => AutoUpdateMode::default(),
        }
    }

    /// Emits the one-line deprecation warning for the legacy
    /// `auto_update_check` alias, if it is the key actually driving the
    /// effective mode (i.e. `auto_update` is unset but `auto_update_check` is).
    ///
    /// Called exactly once during config load so the warning never duplicates
    /// and never pollutes test output (tests load configs through other paths,
    /// or assert on the pure [`update_mode`](Self::update_mode) resolver).
    fn warn_deprecated_auto_update_check(&self) {
        if self.auto_update.is_some() {
            return;
        }
        match self.auto_update_check {
            Some(true) => eprintln!(
                "\u{26a0} [ui] auto_update_check is deprecated; \
                 use auto_update = \"notify\" (or \"off\"/\"install\") instead"
            ),
            Some(false) => eprintln!(
                "\u{26a0} [ui] auto_update_check is deprecated; \
                 use auto_update = \"off\" (or \"notify\"/\"install\") instead"
            ),
            None => {}
        }
    }
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            show_pr: false,
            pr_cache_ttl_hours: default_pr_cache_ttl_hours(),
            auto_update: None,
            auto_update_check: None,
            update_check_interval: None,
        }
    }
}

fn default_pr_cache_ttl_hours() -> u64 {
    24
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct LayoutConfig {
    /// Tera template for the root directory holding all worktrees.
    /// `None` falls back to `layout::DEFAULT_WORKTREE_ROOT`.
    #[serde(default)]
    pub worktree_root: Option<String>,

    /// Tera template for the per-worktree sub-path under `worktree_root`.
    /// `None` falls back to `layout::DEFAULT_WORKTREE_PATH`.
    #[serde(default)]
    pub worktree_path: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct HooksConfig {
    #[serde(default)]
    pub post_create: Vec<HookSpec>,

    #[serde(default)]
    pub pre_remove: Vec<HookSpec>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HookSpec {
    /// Copy a list of files into the new worktree. Each entry is either
    /// `"src"` (copies to same relative path) or `"src -> dst"` (renamed).
    Copy { files: Vec<String> },

    /// Symlink (with junction fallback on Windows).
    Symlink { src: String, dst: String },

    /// Run an arbitrary command. `shell` defaults to `auto` (pwsh on
    /// Windows, bash on Unix).
    Command {
        run: String,
        #[serde(default)]
        shell: Option<String>,
    },
}

#[derive(Debug, Clone, Default)]
pub struct LoadedConfig {
    pub config: Config,
    pub sources: Vec<PathBuf>,
}

impl Config {
    /// Discover and load configuration. `repo_root` is the project root; if
    /// `None`, only the global config is consulted.
    pub fn load(repo_root: Option<&Path>) -> Result<LoadedConfig> {
        Self::load_with_engine(repo_root, &mut Engine::new())
    }

    pub fn load_with_engine(repo_root: Option<&Path>, engine: &mut Engine) -> Result<LoadedConfig> {
        let mut paths = Vec::new();

        if let Some(global_dir) = dirs::config_dir() {
            let global = global_dir.join("renri").join("config.toml");
            if global.exists() {
                paths.push(global);
            }
        }

        if let Some(root) = repo_root {
            let project = root.join("renri.toml");
            if project.exists() {
                paths.push(project);
            }
        }

        if paths.is_empty() {
            return Ok(LoadedConfig {
                config: Config::default(),
                sources: Vec::new(),
            });
        }

        // Pre-populate `vcs` with self-referential placeholders so that
        // `{{ vcs.repo }}`-style references inside layout templates survive
        // load-time rendering and are resolved later by `layout::render_path`
        // when the actual branch is known. teravars renders system / env /
        // {% if %} blocks at load time, but layout values are deferred.
        let mut ctx = system_context();
        ctx.insert(
            "vcs",
            &serde_json::json!({
                "owner": "{{ vcs.owner }}",
                "repo": "{{ vcs.repo }}",
                "host": "{{ vcs.host }}",
                "branch": "{{ vcs.branch }}",
            }),
        );

        let merged =
            load_merged(&paths, engine, &ctx).context("loading renri config via teravars")?;

        let cfg: Config = Value::Table(merged.config)
            .try_into()
            .context("deserializing renri config")?;

        // Surface the legacy-alias deprecation warning once here, at load time,
        // rather than as a side effect of the `update_mode` resolver. This
        // keeps the resolver pure and avoids duplicate / test-output noise.
        cfg.ui.warn_deprecated_auto_update_check();

        Ok(LoadedConfig {
            config: cfg,
            sources: paths,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn load_returns_default_when_no_files() {
        let tmp = TempDir::new().unwrap();
        let loaded = Config::load(Some(tmp.path())).unwrap();
        assert!(loaded.sources.is_empty());
        assert!(loaded.config.layout.worktree_root.is_none());
        assert!(loaded.config.hooks.post_create.is_empty());
    }

    /// Helper: load the `[ui]` section from an inline `renri.toml`.
    fn load_ui(toml: &str) -> UiConfig {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("renri.toml"), toml).unwrap();
        Config::load(Some(tmp.path())).unwrap().config.ui
    }

    #[test]
    fn update_mode_defaults_to_install() {
        // Neither key present anywhere → silent install by default.
        assert_eq!(UiConfig::default().update_mode(), AutoUpdateMode::Install);
        let ui = load_ui("[ui]\nshow_pr = true\n");
        assert_eq!(ui.auto_update, None);
        assert_eq!(ui.auto_update_check, None);
        assert_eq!(ui.update_mode(), AutoUpdateMode::Install);
    }

    #[test]
    fn auto_update_off_parses() {
        let ui = load_ui("[ui]\nauto_update = \"off\"\n");
        assert_eq!(ui.auto_update, Some(AutoUpdateMode::Off));
        assert_eq!(ui.update_mode(), AutoUpdateMode::Off);
    }

    #[test]
    fn auto_update_notify_parses() {
        let ui = load_ui("[ui]\nauto_update = \"notify\"\n");
        assert_eq!(ui.auto_update, Some(AutoUpdateMode::Notify));
        assert_eq!(ui.update_mode(), AutoUpdateMode::Notify);
    }

    #[test]
    fn auto_update_install_parses() {
        let ui = load_ui("[ui]\nauto_update = \"install\"\n");
        assert_eq!(ui.auto_update, Some(AutoUpdateMode::Install));
        assert_eq!(ui.update_mode(), AutoUpdateMode::Install);
    }

    #[test]
    fn legacy_auto_update_check_false_maps_to_off() {
        let ui = load_ui("[ui]\nauto_update_check = false\n");
        assert_eq!(ui.auto_update, None);
        assert_eq!(ui.auto_update_check, Some(false));
        assert_eq!(ui.update_mode(), AutoUpdateMode::Off);
    }

    #[test]
    fn legacy_auto_update_check_true_maps_to_notify() {
        let ui = load_ui("[ui]\nauto_update_check = true\n");
        assert_eq!(ui.auto_update_check, Some(true));
        assert_eq!(ui.update_mode(), AutoUpdateMode::Notify);
    }

    #[test]
    fn explicit_auto_update_overrides_legacy_bool() {
        // When both are set, the new `auto_update` wins and the legacy alias
        // is ignored (no warning, no Notify).
        let ui = load_ui("[ui]\nauto_update = \"install\"\nauto_update_check = false\n");
        assert_eq!(ui.auto_update, Some(AutoUpdateMode::Install));
        assert_eq!(ui.auto_update_check, Some(false));
        assert_eq!(ui.update_mode(), AutoUpdateMode::Install);
    }

    #[test]
    fn update_check_interval_still_parses() {
        let ui = load_ui("[ui]\nupdate_check_interval = \"12h\"\n");
        assert_eq!(ui.update_check_interval.as_deref(), Some("12h"));
    }

    #[test]
    fn load_reads_project_renri_toml() {
        let tmp = TempDir::new().unwrap();
        fs::write(
            tmp.path().join("renri.toml"),
            r#"
[layout]
worktree_root = "/srv/wt"
worktree_path = "{{ vcs.repo }}/{{ vcs.branch }}"

[[hooks.post_create]]
type = "copy"
files = [".env"]

[[hooks.post_create]]
type = "command"
run = "echo hi"
"#,
        )
        .unwrap();

        let loaded = Config::load(Some(tmp.path())).unwrap();
        assert_eq!(loaded.sources.len(), 1);
        assert_eq!(
            loaded.config.layout.worktree_root.as_deref(),
            Some("/srv/wt")
        );
        assert_eq!(loaded.config.hooks.post_create.len(), 2);
        assert!(matches!(
            loaded.config.hooks.post_create[0],
            HookSpec::Copy { .. }
        ));
        assert!(matches!(
            loaded.config.hooks.post_create[1],
            HookSpec::Command { .. }
        ));
    }

    #[test]
    fn config_supports_teravars_include_directive() {
        let tmp = TempDir::new().unwrap();
        fs::write(
            tmp.path().join("base.toml"),
            r#"
[layout]
worktree_root = "/from-base"
"#,
        )
        .unwrap();
        fs::write(
            tmp.path().join("renri.toml"),
            r#"
include = ["base.toml"]

[[hooks.post_create]]
type = "command"
run = "echo from-main"
"#,
        )
        .unwrap();

        let loaded = Config::load(Some(tmp.path())).unwrap();
        assert_eq!(
            loaded.config.layout.worktree_root.as_deref(),
            Some("/from-base"),
            "value should come from the included file"
        );
        assert_eq!(loaded.config.hooks.post_create.len(), 1);
    }

    #[test]
    fn config_supports_per_os_conditional_via_tera() {
        let tmp = TempDir::new().unwrap();
        fs::write(
            tmp.path().join("renri.toml"),
            r#"
[layout]
{% if system.os == "windows" %}
worktree_root = "C:/wt"
{% else %}
worktree_root = "/home/user/wt"
{% endif %}
"#,
        )
        .unwrap();

        let loaded = Config::load(Some(tmp.path())).unwrap();
        let root = loaded.config.layout.worktree_root.unwrap();
        if cfg!(windows) {
            assert_eq!(root, "C:/wt");
        } else {
            assert_eq!(root, "/home/user/wt");
        }
    }
}
