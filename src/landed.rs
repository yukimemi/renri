//! Name-independent "is this worktree's change already upstream?" probe.
//!
//! [`crate::pr_cache`] answers the same question by *name*: it matches a
//! worktree's branch / bookmark against a PR's head ref. That works only
//! when the two names agree, and in practice they often don't:
//!
//! - the worktree is cut as `identity-1260` (issue-numbered) but the PR is
//!   pushed as `feat/backend-signing-identity`,
//! - the worktree is detached (another tool made it — e.g. Claude Code's
//!   `.claude/worktrees/*`), so there is no branch name to match at all,
//! - the branch was never pushed: the change reached `main` from a
//!   different checkout.
//!
//! In every one of those cases the *content* is what settles it, and git
//! already knows: `git cherry <upstream> <head>` compares patch-ids and
//! prefixes each commit with `-` when an equivalent patch is already
//! upstream. That survives GitHub's squash-merge (the squashed commit has
//! the same diff, hence the same patch-id) and doesn't care what anything
//! was named.
//!
//! Deliberately fail-safe: a missing git store, an unresolvable upstream
//! ref, or any `git` failure yields "not landed", which is exactly the
//! behavior callers had before this module existed. It never reports
//! landed on a guess.
//!
//! **Works for jj too, without a jj dependency.** jj's git backend keeps
//! its store at `<root>/.git` (colocated repos share the checkout's `.git`;
//! non-colocated ones get a bare one there, pointed at by
//! `.jj/repo/store/git_target`), so plain `git` at the workspace root
//! resolves jj-authored commits and remote refs in both layouts.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Upstream refs tried, in order, when resolving what "landed" means.
///
/// `origin/HEAD` first: it's the remote's own answer to "what is the
/// default branch", so a repo on `develop` / `trunk` works without
/// configuration — but plenty of clones never fetch it, hence the
/// main/master fallbacks. Local `main` / `master` come last so a repo with
/// no remote (or an unfetched one) still gets a usable comparison.
const TRUNK_CANDIDATES: &[&str] = &[
    "refs/remotes/origin/HEAD",
    "refs/remotes/origin/main",
    "refs/remotes/origin/master",
    "refs/heads/main",
    "refs/heads/master",
];

/// A resolved upstream ref plus the repo to run `git` in.
#[derive(Debug, Clone)]
pub struct Probe {
    root: PathBuf,
    trunk: String,
}

impl Probe {
    /// Resolve the upstream ref once, so a sweep over N worktrees doesn't
    /// re-resolve it N times. `None` when no git store is reachable from
    /// `root` or none of [`TRUNK_CANDIDATES`] exists — callers then treat
    /// every row as not-landed.
    pub fn discover(root: &Path) -> Option<Self> {
        let trunk = TRUNK_CANDIDATES
            .iter()
            .find(|r| rev_parse(root, r).is_some())?;
        Some(Self {
            root: root.to_path_buf(),
            trunk: (*trunk).to_string(),
        })
    }

    /// The upstream ref this probe compares against, in the short form a
    /// user would type (`origin/main`, `main`) rather than the fully
    /// qualified ref it was resolved from.
    pub fn trunk(&self) -> &str {
        ["refs/remotes/", "refs/heads/", "refs/"]
            .iter()
            .find_map(|p| self.trunk.strip_prefix(p))
            .unwrap_or(&self.trunk)
    }

    /// True when every commit `commit` adds on top of the upstream ref is
    /// already upstream by patch-id — including the degenerate case where
    /// it adds nothing (`commit` is an ancestor of the ref).
    pub fn landed(&self, commit: &str) -> bool {
        let Some(out) = git(&self.root, &["cherry", &self.trunk, commit]) else {
            return false;
        };
        all_upstream(&out)
    }
}

/// Classify `git cherry` output.
///
/// Each line is `<sign> <sha>`, where `-` means "an equivalent patch is
/// already upstream" and `+` means "not upstream". Empty output means the
/// commit is an ancestor of the upstream ref: nothing to compare, so it is
/// upstream by definition.
///
/// Anything that isn't a recognized sign line (git noise, a future format)
/// counts as not-upstream — the fail-safe direction, since the result
/// gates worktree deletion.
fn all_upstream(stdout: &str) -> bool {
    // No lines at all → ancestor of the upstream ref → landed.
    stdout
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .all(|line| matches!(line.split_once(' '), Some(("-", _))))
}

/// `git rev-parse --verify <rev>`; `None` when the ref doesn't resolve.
///
/// `--quiet` keeps git from writing "unknown revision" to stderr for every
/// candidate we probe, which would otherwise spam the terminal with three
/// or four fatals on any repo whose default branch isn't `main`.
fn rev_parse(root: &Path, rev: &str) -> Option<String> {
    git(root, &["rev-parse", "--quiet", "--verify", rev])
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Run `git` in `root`, returning stdout on success only.
fn git(root: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_cherry_output_is_landed() {
        // `git cherry` prints nothing when <head> adds no commits on top of
        // the upstream ref, i.e. it is already an ancestor.
        assert!(all_upstream(""));
        assert!(all_upstream("\n\n"));
    }

    #[test]
    fn all_minus_lines_are_landed() {
        // The squash-merge case: same diff upstream under a different sha,
        // reached from a differently-named branch.
        let out = "- 15596815329293bb076cfc7a483edf9eca4fc199\n\
                   - b6109d058990aaaaaaaaaaaaaaaaaaaaaaaaaaaa\n";
        assert!(all_upstream(out));
    }

    #[test]
    fn any_plus_line_is_not_landed() {
        // A commit whose content changed before merging (review fixes) has
        // no patch-id twin upstream. Must not be swept on content alone —
        // the PR lookup is what settles those.
        let out = "- aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n\
                   + bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\n";
        assert!(!all_upstream(out));
    }

    #[test]
    fn unrecognized_output_is_not_landed() {
        // Fail-safe: the result gates deletion, so anything we don't
        // positively recognize as "upstream" must read as not-landed.
        assert!(!all_upstream("warning: something odd\n"));
        assert!(!all_upstream("-aaaa\n"), "sign must be its own field");
    }

    #[test]
    fn trunk_is_displayed_as_a_user_would_type_it() {
        // The status line quotes this back to the user, so it should read
        // like the ref they'd pass to git, not the fully qualified form we
        // resolved it from.
        let probe = |t: &str| Probe {
            root: PathBuf::new(),
            trunk: t.into(),
        };
        assert_eq!(probe("refs/remotes/origin/main").trunk(), "origin/main");
        assert_eq!(probe("refs/remotes/origin/HEAD").trunk(), "origin/HEAD");
        assert_eq!(probe("refs/heads/master").trunk(), "master");
        // Unrecognized shapes pass through rather than being mangled.
        assert_eq!(probe("origin/main").trunk(), "origin/main");
    }

    #[test]
    fn probe_discover_returns_none_outside_a_repo() {
        // Fail-safe path: no git store → no probe → every row reads as
        // not-landed, matching pre-landed-detection behavior.
        let tmp = std::env::temp_dir().join("renri-landed-probe-nonrepo");
        std::fs::create_dir_all(&tmp).unwrap();
        assert!(Probe::discover(&tmp).is_none());
    }
}
