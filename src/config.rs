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
