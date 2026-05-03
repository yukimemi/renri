//! Resolve the on-disk path of a worktree from a Tera template.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Serialize;
use teravars::{Context as TeraCtx, Engine};

use crate::vcs::Backend;

/// Default root directory for new worktrees. Cross-platform via teravars'
/// `home()` helper — wraps `dirs::home_dir()` so we don't have to care
/// whether the user is on a Windows shell that exports HOME or not.
pub const DEFAULT_WORKTREE_ROOT: &str = "{{ home() }}/wt";

/// Default sub-path under the worktree root.
pub const DEFAULT_WORKTREE_PATH: &str =
    "{{ vcs.owner }}/{{ vcs.repo }}/{{ vcs.branch | replace(from='/', to='-') }}";

/// What we know about the parent repo, exposed to templates as `vcs.*`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct VcsContext {
    pub owner: String,
    pub repo: String,
    pub host: Option<String>,
    pub branch: String,
}

/// Discover the parent repo's identity from `origin` URL + repo root path,
/// falling back to the local user when there's no remote.
pub fn discover_vcs_context(backend: &dyn Backend, repo_root: &Path, branch: &str) -> VcsContext {
    if let Some(url) = backend.origin_url() {
        let mut info = parse_origin(&url);
        info.branch = branch.to_string();
        return info;
    }

    let repo = repo_root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("repo")
        .to_string();
    VcsContext {
        owner: whoami::username(),
        repo,
        host: None,
        branch: branch.to_string(),
    }
}

/// Parse an `origin` URL into owner / repo / host.
///
/// Supports the SCP-like form `git@host:path` and the URL forms
/// `https://host/path`, `ssh://[user@]host[:port]/path`, `git://host/path`.
pub fn parse_origin(url: &str) -> VcsContext {
    let trimmed = url.trim();
    let no_git = trimmed.strip_suffix(".git").unwrap_or(trimmed);

    let (host, path) = split_host_and_path(no_git);
    let (owner, repo) = split_owner_repo(path);

    VcsContext {
        owner: owner.to_string(),
        repo: repo.to_string(),
        host,
        branch: String::new(),
    }
}

fn split_host_and_path(url: &str) -> (Option<String>, &str) {
    if let Some(scheme_end) = url.find("://") {
        let after = &url[scheme_end + 3..];
        let after = after.rsplit_once('@').map_or(after, |x| x.1);
        if let Some(slash) = after.find('/') {
            let host_part = &after[..slash];
            let host = host_part.split(':').next().unwrap_or(host_part);
            return (Some(host.to_string()), &after[slash + 1..]);
        }
        return (Some(after.to_string()), "");
    }

    if let Some(colon) = url.find(':') {
        let before_colon = &url[..colon];
        if !before_colon.contains('/') {
            let host_part = before_colon.rsplit_once('@').map_or(before_colon, |x| x.1);
            return (Some(host_part.to_string()), &url[colon + 1..]);
        }
    }

    (None, url)
}

fn split_owner_repo(path: &str) -> (&str, &str) {
    let path = path.trim_start_matches('/');
    match path.rsplit_once('/') {
        Some((owner, repo)) => (owner, repo),
        None => ("", path),
    }
}

/// Render the worktree path templates with the given context, returning the
/// joined absolute path.
pub fn render_path(
    engine: &mut Engine,
    base_ctx: &TeraCtx,
    vcs: &VcsContext,
    root_template: Option<&str>,
    path_template: Option<&str>,
) -> Result<PathBuf> {
    let root_t = root_template.unwrap_or(DEFAULT_WORKTREE_ROOT);
    let path_t = path_template.unwrap_or(DEFAULT_WORKTREE_PATH);

    let mut ctx = base_ctx.clone();
    ctx.insert("vcs", vcs);

    let root = engine
        .render(root_t, &ctx)
        .context("rendering layout.worktree_root")?;
    let sub = engine
        .render(path_t, &ctx)
        .context("rendering layout.worktree_path")?;

    Ok(PathBuf::from(root.trim()).join(sub.trim()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_origin_scp_like() {
        let v = parse_origin("git@github.com:yukimemi/teravars.git");
        assert_eq!(v.host.as_deref(), Some("github.com"));
        assert_eq!(v.owner, "yukimemi");
        assert_eq!(v.repo, "teravars");
    }

    #[test]
    fn parse_origin_https() {
        let v = parse_origin("https://github.com/foo/bar.git");
        assert_eq!(v.host.as_deref(), Some("github.com"));
        assert_eq!(v.owner, "foo");
        assert_eq!(v.repo, "bar");
    }

    #[test]
    fn parse_origin_ssh_with_port() {
        let v = parse_origin("ssh://git@gitlab.example.com:2222/group/proj.git");
        assert_eq!(v.host.as_deref(), Some("gitlab.example.com"));
        assert_eq!(v.owner, "group");
        assert_eq!(v.repo, "proj");
    }

    #[test]
    fn parse_origin_nested_owner_path() {
        let v = parse_origin("git@gitlab.com:team/group/subproj.git");
        assert_eq!(v.host.as_deref(), Some("gitlab.com"));
        assert_eq!(v.owner, "team/group");
        assert_eq!(v.repo, "subproj");
    }

    #[test]
    fn parse_origin_no_owner() {
        let v = parse_origin("git@host:repo.git");
        assert_eq!(v.host.as_deref(), Some("host"));
        assert_eq!(v.owner, "");
        assert_eq!(v.repo, "repo");
    }

    #[test]
    fn parse_origin_strips_git_suffix() {
        assert_eq!(parse_origin("https://x.y/a/b.git").repo, "b");
        assert_eq!(parse_origin("https://x.y/a/b").repo, "b");
    }

    #[test]
    fn render_path_uses_defaults_and_resolves_branch_with_slash() {
        let mut engine = Engine::new();
        let ctx = TeraCtx::new();
        let vcs = VcsContext {
            owner: "yuki".into(),
            repo: "renri".into(),
            host: Some("github.com".into()),
            branch: "feature/auth".into(),
        };
        let path = render_path(&mut engine, &ctx, &vcs, None, None).unwrap();
        let s = path.to_string_lossy().replace('\\', "/");
        assert!(s.ends_with("yuki/renri/feature-auth"), "got: {s}");
    }

    #[test]
    fn render_path_honors_custom_templates() {
        let mut engine = Engine::new();
        let ctx = TeraCtx::new();
        let vcs = VcsContext {
            owner: "o".into(),
            repo: "r".into(),
            host: None,
            branch: "b".into(),
        };
        let p = render_path(
            &mut engine,
            &ctx,
            &vcs,
            Some("/tmp/worktrees"),
            Some("{{ vcs.repo }}/{{ vcs.branch }}"),
        )
        .unwrap();
        let s = p.to_string_lossy().replace('\\', "/");
        assert_eq!(s, "/tmp/worktrees/r/b");
    }
}
