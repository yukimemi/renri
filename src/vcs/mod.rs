//! VCS abstraction: detect whether the current repo is git, jj, or colocated,
//! then dispatch worktree operations to the right backend.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

pub mod detect;
pub mod git;

pub use detect::{Kind, Repo, detect};

/// Caller-side override for which backend to use.
#[derive(Debug, Clone, Copy)]
pub enum VcsChoice {
    Auto,
    Git,
    Jj,
}

/// One row in `renri list` — a worktree (git) or workspace (jj).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    /// Short name — typically the branch / bookmark, falls back to the
    /// directory's basename.
    pub name: String,
    /// Absolute path of the worktree.
    pub path: PathBuf,
    /// Branch (git) or bookmark (jj) name; `None` if detached / anonymous.
    pub branch: Option<String>,
    /// Commit hash (40-char) of the worktree's HEAD / @-commit.
    pub head: Option<String>,
    /// True for the original / main worktree (the one git/jj was init'd in).
    pub is_main: bool,
    pub is_bare: bool,
    /// Git: marked prunable. jj: stale (working copy changed by another
    /// workspace, needs `update-stale`).
    pub is_stale: bool,
    pub is_locked: bool,
}

pub trait Backend {
    /// Display name of the backend ("git" / "jj").
    fn name(&self) -> &str;

    fn list(&self) -> Result<Vec<Worktree>>;

    fn add(&self, path: &Path, branch: Option<&str>) -> Result<()>;

    fn remove(&self, path: &Path, force: bool) -> Result<()>;
}

/// Pick which backend to use given the detected repo kind and the user's
/// `--vcs` override.
pub fn select_kind(repo_kind: Kind, choice: VcsChoice) -> Result<Kind> {
    match (repo_kind, choice) {
        (_, VcsChoice::Auto) => Ok(match repo_kind {
            // Colocated repos default to jj — the jj working-copy semantics
            // are the user's source of truth.
            Kind::Colocated => Kind::Jj,
            other => other,
        }),

        (Kind::Git, VcsChoice::Git) | (Kind::Colocated, VcsChoice::Git) => Ok(Kind::Git),
        (Kind::Jj, VcsChoice::Jj) | (Kind::Colocated, VcsChoice::Jj) => Ok(Kind::Jj),

        (Kind::Git, VcsChoice::Jj) => bail!(
            "this repo is git-managed but --vcs jj was forced; \
             initialize jj on top with `jj git init --colocate` first"
        ),
        (Kind::Jj, VcsChoice::Git) => {
            bail!("this repo is jj-managed (no .git/) but --vcs git was forced")
        }
    }
}

/// Open the right backend for the chosen kind.
pub fn open_backend(repo: &Repo, kind: Kind) -> Result<Box<dyn Backend>> {
    match kind {
        Kind::Git => Ok(Box::new(git::GitBackend::new(&repo.root))),
        Kind::Jj | Kind::Colocated => bail!(
            "the jj backend is not implemented yet; \
             use --vcs git on a colocated repo, or wait for the next release"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn select_auto_passes_through_pure_kinds() {
        assert_eq!(select_kind(Kind::Git, VcsChoice::Auto).unwrap(), Kind::Git);
        assert_eq!(select_kind(Kind::Jj, VcsChoice::Auto).unwrap(), Kind::Jj);
    }

    #[test]
    fn select_auto_prefers_jj_for_colocated() {
        assert_eq!(
            select_kind(Kind::Colocated, VcsChoice::Auto).unwrap(),
            Kind::Jj
        );
    }

    #[test]
    fn select_explicit_overrides_colocated() {
        assert_eq!(
            select_kind(Kind::Colocated, VcsChoice::Git).unwrap(),
            Kind::Git
        );
        assert_eq!(
            select_kind(Kind::Colocated, VcsChoice::Jj).unwrap(),
            Kind::Jj
        );
    }

    #[test]
    fn select_rejects_incompatible_overrides() {
        assert!(select_kind(Kind::Git, VcsChoice::Jj).is_err());
        assert!(select_kind(Kind::Jj, VcsChoice::Git).is_err());
    }
}
