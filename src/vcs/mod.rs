//! VCS abstraction: detect whether the current repo is git, jj, or colocated,
//! then dispatch worktree operations to the right backend.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

pub mod detect;
pub mod git;
pub mod jj;

pub use detect::{Kind, Repo, detect};

/// Caller-side override for which backend to use.
#[derive(Debug, Clone, Copy)]
pub enum VcsChoice {
    Auto,
    Git,
    Jj,
}

/// One row in `renri list` — a worktree (git) or workspace (jj).
///
/// Marked `#[non_exhaustive]` so adding new fields (a new metric a backend
/// can populate) isn't a breaking change for external consumers.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Worktree {
    /// Short name — typically the branch / bookmark, falls back to the
    /// directory's basename.
    pub name: String,
    /// Absolute path of the worktree.
    pub path: PathBuf,
    /// Branch (git) or bookmark (jj) name; `None` if detached / anonymous.
    pub branch: Option<String>,
    /// Short commit / change id of the worktree's HEAD / @-commit.
    pub head: Option<String>,
    /// First line of the @-commit / HEAD's description, for `renri list`.
    pub desc: Option<String>,
    /// Working copy has uncommitted changes (git: `status --porcelain`
    /// non-empty, jj: `@` is non-empty vs its parent).
    pub dirty: bool,
    /// `@` / HEAD has unresolved conflicts. jj only — git surfaces conflicts
    /// only during merge.
    pub conflict: bool,
    /// True for the original / main worktree (the one git/jj was init'd in).
    pub is_main: bool,
    pub is_bare: bool,
    /// Git: marked prunable. jj: stale (working copy changed by another
    /// workspace, needs `update-stale`).
    pub is_stale: bool,
    pub is_locked: bool,
    /// Which backend produced this row. Used by the CLI to dispatch
    /// per-row operations (e.g. `remove`) back to the right backend in a
    /// colocated repo where `list` unions both sides.
    ///
    /// Always `Kind::Git` for [`git::GitBackend`] and `Kind::Jj` for
    /// [`jj::JjBackend`] — `Kind::Colocated` is never produced because a
    /// single row always comes from exactly one of the two stores.
    pub vcs: Kind,
}

/// How a worktree should be hooked up to a branch when adding it.
#[derive(Debug, Clone, Copy)]
pub enum AddBranch<'a> {
    /// Create a new branch with this name. `base` selects the start commit:
    /// `None` means "fork off the cwd worktree's current HEAD" (the
    /// expected default); `Some(ref)` lets the caller pin an explicit
    /// commit / branch / tag / revset.
    NewBranch {
        name: &'a str,
        base: Option<&'a str>,
    },
    /// Attach to an already-existing branch.
    ExistingBranch(&'a str),
}

pub trait Backend {
    /// Display name of the backend ("git" / "jj").
    fn name(&self) -> &str;

    fn list(&self) -> Result<Vec<Worktree>>;

    fn add(&self, path: &Path, branch: AddBranch) -> Result<()>;

    fn remove(&self, path: &Path, force: bool) -> Result<()>;

    /// URL of the origin remote, if one is configured. Used by the layout
    /// renderer to extract owner / repo / host. Default impl returns `None`.
    fn origin_url(&self) -> Option<String> {
        None
    }

    /// Current branch (git) / bookmark at @-commit (jj). Default: `None`.
    fn current_branch(&self) -> Option<String> {
        None
    }

    /// List branches / bookmarks (and tags, where the backend has them) so
    /// callers can offer a fuzzy picker for `--from`-style base selection.
    /// Default: empty list.
    fn list_refs(&self) -> Result<Vec<String>> {
        Ok(Vec::new())
    }

    /// Does a branch / bookmark with this name already exist?
    fn branch_exists(&self, _name: &str) -> bool {
        false
    }

    /// Garbage-collect stale worktree metadata. For git: removes entries
    /// whose on-disk directory has been deleted. For jj: forgets workspaces
    /// whose root path is gone. Returns whatever stdout the underlying
    /// command produced, so the CLI can surface it to the user.
    fn prune(&self) -> Result<String> {
        Ok(String::new())
    }

    /// Update remote refs from origin. `git fetch origin` for git,
    /// `jj git fetch` for jj. Repo-wide — all worktrees see the new refs
    /// since they share the same git store.
    fn fetch(&self) -> Result<String> {
        Ok(String::new())
    }

    /// Fetch a single named remote: `git fetch <remote>` /
    /// `jj git fetch --remote <remote>`. Used by `add` so a `--from
    /// <remote>/<branch>` base that names a non-`origin` remote actually
    /// updates *that* remote before the base is resolved. Default: no-op.
    fn fetch_remote(&self, _remote: &str) -> Result<String> {
        Ok(String::new())
    }

    /// If `rev` references a remote-tracking ref (so resolving it against the
    /// locally-cached refs could pick up a stale tip), return the **remote's
    /// name** so `add` can `fetch` exactly that remote before forking
    /// `--from <rev>`. Git remote form is `<remote>/<branch>`; jj's is
    /// `<bookmark>@<remote>`. `None` = not a remote ref. Default: `None`.
    fn referenced_remote(&self, _rev: &str) -> Option<String> {
        None
    }
}

/// Pick the set of backends to query given the detected repo kind and the
/// user's `--vcs` override.
///
/// Returns 1 element in every case except colocated + Auto, which returns
/// `[Jj, Git]` so verbs that union (`list`, `prune`, `sync`) can surface
/// both sets and verbs that need a single primary (`add`, `config show`)
/// can take `[0]` and get jj — matching the long-standing jj-priority
/// policy for colocated repos.
///
/// **Why two backends here**: in colocated repos, `git worktree add` and
/// `jj workspace add` create independent secondary checkouts that don't
/// see each other (jj-vcs/jj#8052 — secondary colocation isn't supported).
/// Showing only one side hides the other from list/prune and surprises
/// users into thinking worktrees vanished.
pub fn select_kinds(repo_kind: Kind, choice: VcsChoice) -> Result<Vec<Kind>> {
    match (repo_kind, choice) {
        (Kind::Git, VcsChoice::Auto) => Ok(vec![Kind::Git]),
        (Kind::Jj, VcsChoice::Auto) => Ok(vec![Kind::Jj]),
        // jj first so single-primary callers (cmd_add, cmd_config_show)
        // get the same backend the in-repo policy has always used.
        (Kind::Colocated, VcsChoice::Auto) => Ok(vec![Kind::Jj, Kind::Git]),

        (Kind::Git, VcsChoice::Git) | (Kind::Colocated, VcsChoice::Git) => Ok(vec![Kind::Git]),
        (Kind::Jj, VcsChoice::Jj) | (Kind::Colocated, VcsChoice::Jj) => Ok(vec![Kind::Jj]),

        (Kind::Git, VcsChoice::Jj) => bail!(
            "this repo is git-managed but --vcs jj was forced; \
             initialize jj on top with `jj git init --colocate` first"
        ),
        (Kind::Jj, VcsChoice::Git) => {
            bail!("this repo is jj-managed (no .git/) but --vcs git was forced")
        }
    }
}

/// Convenience wrapper for callers that want a single backend (commands
/// that operate on one worktree at a time: `add`, `config show`,
/// `gh-repo`). Returns the first element of [`select_kinds`] — for
/// colocated + Auto that is jj.
pub fn select_kind(repo_kind: Kind, choice: VcsChoice) -> Result<Kind> {
    Ok(select_kinds(repo_kind, choice)?[0])
}

/// Short label for a backend kind, used in pickers and the `list`
/// VCS column. `Colocated` should never appear on a [`Worktree`] (rows
/// always come from exactly one of the two stores), but we render it as
/// `?` rather than panicking to keep the UI honest.
pub fn kind_short(kind: Kind) -> &'static str {
    match kind {
        Kind::Git => "git",
        Kind::Jj => "jj",
        Kind::Colocated => "?",
    }
}

/// Open the right backend for the chosen kind.
pub fn open_backend(repo: &Repo, kind: Kind) -> Result<Box<dyn Backend>> {
    match kind {
        Kind::Git => Ok(Box::new(git::GitBackend::new(&repo.root))),
        Kind::Jj => Ok(Box::new(jj::JjBackend::new(&repo.root))),
        // select_kind never returns Colocated.
        Kind::Colocated => unreachable!("select_kind resolves Colocated to Git or Jj"),
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

    #[test]
    fn select_kinds_passthrough_for_pure_kinds() {
        assert_eq!(
            select_kinds(Kind::Git, VcsChoice::Auto).unwrap(),
            vec![Kind::Git]
        );
        assert_eq!(
            select_kinds(Kind::Jj, VcsChoice::Auto).unwrap(),
            vec![Kind::Jj]
        );
    }

    #[test]
    fn select_kinds_returns_both_for_colocated_auto() {
        // jj first so primary() lands on jj per the long-standing policy.
        assert_eq!(
            select_kinds(Kind::Colocated, VcsChoice::Auto).unwrap(),
            vec![Kind::Jj, Kind::Git]
        );
    }

    #[test]
    fn select_kinds_narrows_under_explicit_override() {
        assert_eq!(
            select_kinds(Kind::Colocated, VcsChoice::Git).unwrap(),
            vec![Kind::Git]
        );
        assert_eq!(
            select_kinds(Kind::Colocated, VcsChoice::Jj).unwrap(),
            vec![Kind::Jj]
        );
    }

    #[test]
    fn select_kinds_rejects_incompatible_overrides() {
        assert!(select_kinds(Kind::Git, VcsChoice::Jj).is_err());
        assert!(select_kinds(Kind::Jj, VcsChoice::Git).is_err());
    }

    #[test]
    fn select_kind_compat_shim_picks_jj_for_colocated_auto() {
        // The single-backend wrapper must keep returning jj so cmd_add etc.
        // don't change behavior in colocated repos.
        assert_eq!(
            select_kind(Kind::Colocated, VcsChoice::Auto).unwrap(),
            Kind::Jj
        );
    }
}
