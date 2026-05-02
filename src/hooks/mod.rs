//! Hook executor — runs the typed hooks declared in `[[hooks.post_create]]`
//! / `[[hooks.pre_remove]]` against a worktree.
//!
//! Every hook field that's a string goes through Tera (via teravars's
//! `Engine`), with the same `system.*` / `env(...)` helpers as elsewhere
//! plus `vcs.*` and a couple of `worktree.*` / `repo.*` paths in scope.

use std::path::Path;

use anyhow::{Context, Result};
use teravars::{Context as TeraCtx, Engine};
use tracing::info;

use crate::config::HookSpec;
use crate::layout::VcsContext;

mod command;
mod copy;
mod symlink;

/// Everything a hook needs in order to run.
pub struct HookRun<'a> {
    pub repo_root: &'a Path,
    pub worktree_path: &'a Path,
    pub vcs: &'a VcsContext,
    pub engine: &'a mut Engine,
    pub base_ctx: &'a TeraCtx,
}

impl<'a> HookRun<'a> {
    /// Render `s` as a Tera template with the hook context in scope.
    pub fn render(&mut self, s: &str) -> Result<String> {
        let mut ctx = self.base_ctx.clone();
        ctx.insert("vcs", self.vcs);
        ctx.insert(
            "worktree",
            &serde_json::json!({ "path": self.worktree_path.to_string_lossy() }),
        );
        ctx.insert(
            "repo",
            &serde_json::json!({ "root": self.repo_root.to_string_lossy() }),
        );
        Ok(self.engine.render(s, &ctx)?)
    }
}

/// Run each hook in order. The first failing hook stops the chain.
pub fn run_all(specs: &[HookSpec], hr: &mut HookRun) -> Result<()> {
    for spec in specs {
        run_one(spec, hr).with_context(|| match spec {
            HookSpec::Copy { .. } => "copy hook failed".to_string(),
            HookSpec::Symlink { .. } => "symlink hook failed".to_string(),
            HookSpec::Command { run, .. } => format!("command hook failed: {run}"),
        })?;
    }
    Ok(())
}

fn run_one(spec: &HookSpec, hr: &mut HookRun) -> Result<()> {
    match spec {
        HookSpec::Copy { files } => {
            for entry in files {
                let rendered = hr.render(entry)?;
                let (src_rel, dst_rel) = parse_arrow(&rendered);
                let src = hr.repo_root.join(src_rel);
                let dst = hr.worktree_path.join(dst_rel);
                info!(?src, ?dst, "copy");
                copy::copy_path(&src, &dst)?;
            }
            Ok(())
        }
        HookSpec::Symlink { src, dst } => {
            let src_rendered = hr.render(src)?;
            let dst_rendered = hr.render(dst)?;
            let target = if Path::new(&src_rendered).is_absolute() {
                std::path::PathBuf::from(&src_rendered)
            } else {
                hr.repo_root.join(&src_rendered)
            };
            let link = hr.worktree_path.join(&dst_rendered);
            info!(?target, ?link, "symlink");
            symlink::create(&target, &link)
        }
        HookSpec::Command { run, shell } => {
            let rendered = hr.render(run)?;
            info!(cmd = %rendered, "command");
            command::run(&rendered, shell.as_deref(), hr.worktree_path)
        }
    }
}

fn parse_arrow(s: &str) -> (&str, &str) {
    if let Some((a, b)) = s.split_once("->") {
        (a.trim(), b.trim())
    } else {
        let t = s.trim();
        (t, t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_arrow_with_rename() {
        assert_eq!(
            parse_arrow(".env.example -> .env"),
            (".env.example", ".env")
        );
    }

    #[test]
    fn parse_arrow_without_rename() {
        assert_eq!(parse_arrow(".env"), (".env", ".env"));
    }

    #[test]
    fn parse_arrow_handles_extra_whitespace() {
        assert_eq!(
            parse_arrow("  src.toml  ->  dst.toml  "),
            ("src.toml", "dst.toml")
        );
    }
}
