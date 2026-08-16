//! Policy for `renri remove --merged`: given what we know about a
//! worktree, is its work finished, and is it safe to delete?
//!
//! Split out of the command so the decision is a pure function over
//! (worktree, PR verdict, landed verdict) — the interesting cases (an open
//! PR on landed content, jj's "everything is dirty") are then testable
//! without a repo, a network, or a `gh` binary.

use crate::pr_cache::PrInfo;
use crate::vcs::{Kind, Worktree};

/// Why a worktree counts as finished. Drives the label the sweep prints,
/// and lets the details panel say *how* renri decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// A PR matched (by branch name or by commit) and is merged / closed.
    Pr,
    /// No usable PR verdict, but the content is already upstream.
    Landed,
}

/// Is this worktree finished, and on what evidence? `None` = leave it alone.
///
/// An OPEN PR vetoes both signals: work under review must survive the
/// sweep even if an identical patch is already upstream (a cherry-pick to
/// a release branch, say). Otherwise a merged / closed PR settles it, and
/// failing that, upstream content does.
pub fn reason(pr: Option<&PrInfo>, landed: bool) -> Option<Reason> {
    if let Some(pr) = pr {
        return match pr.state.as_str() {
            "OPEN" => None,
            "MERGED" | "CLOSED" => Some(Reason::Pr),
            // An unknown state is not evidence of anything; fall back to
            // content so a future GitHub vocabulary change degrades to the
            // local signal instead of vetoing the row.
            _ => landed.then_some(Reason::Landed),
        };
    }
    landed.then_some(Reason::Landed)
}

/// Working-copy conditions that block automatic removal, unless `force`.
///
/// `dirty` is ignored for landed **jj** rows. jj commits the working copy
/// into `@`, so its "dirty" means "`@` has content" — true of every
/// workspace holding real work, and already covered by the patch-id
/// comparison that declared it landed. Without this carve-out `--merged`
/// sweeps almost nothing in a jj repo. A git worktree's dirt is
/// *uncommitted* files, which that comparison cannot see, so the veto
/// stands there.
pub fn blockers(w: &Worktree, landed: bool, force: bool) -> Vec<&'static str> {
    let mut reasons = Vec::new();
    if force {
        return reasons;
    }
    if w.dirty && !(landed && w.vcs == Kind::Jj) {
        reasons.push("dirty");
    }
    if w.conflict {
        reasons.push("conflict");
    }
    if w.is_locked {
        reasons.push("locked");
    }
    reasons
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pr_state(state: &str) -> PrInfo {
        PrInfo {
            number: 42,
            state: state.into(),
            head_ref_name: "feat/x".into(),
        }
    }

    fn wt(vcs: Kind, dirty: bool) -> Worktree {
        Worktree {
            name: "w".into(),
            path: std::path::PathBuf::new(),
            branch: None,
            head: None,
            commit: Some("deadbeef".into()),
            desc: None,
            dirty,
            conflict: false,
            is_main: false,
            is_bare: false,
            is_stale: false,
            is_locked: false,
            vcs,
        }
    }

    #[test]
    fn merged_or_closed_pr_is_swept() {
        assert_eq!(reason(Some(&pr_state("MERGED")), false), Some(Reason::Pr));
        assert_eq!(reason(Some(&pr_state("CLOSED")), false), Some(Reason::Pr));
    }

    #[test]
    fn open_pr_vetoes_even_when_content_is_upstream() {
        // The case that must never regress: an identical patch upstream (a
        // cherry-pick to a release branch, say) while the PR is still in
        // review. Sweeping that deletes the only checkout of live work.
        assert_eq!(reason(Some(&pr_state("OPEN")), true), None);
        // And the ordinary in-review row, for completeness: the "OPEN"
        // arm is unconditional, and this pins it that way.
        assert_eq!(reason(Some(&pr_state("OPEN")), false), None);
    }

    #[test]
    fn landed_content_is_swept_without_any_pr() {
        // The whole point of the content signal: worktrees whose branch
        // name never matched a PR head ref (renamed branch, detached
        // worktree, never-pushed branch) but whose diff is upstream.
        assert_eq!(reason(None, true), Some(Reason::Landed));
    }

    #[test]
    fn unlanded_content_without_a_pr_is_left_alone() {
        assert_eq!(reason(None, false), None);
    }

    #[test]
    fn unknown_pr_state_falls_back_to_content() {
        // A GitHub vocabulary change must degrade to the local signal, not
        // veto every row.
        assert_eq!(reason(Some(&pr_state("DRAFT")), true), Some(Reason::Landed));
        assert_eq!(reason(Some(&pr_state("DRAFT")), false), None);
    }

    #[test]
    fn landed_jj_dirt_does_not_block_removal() {
        assert!(blockers(&wt(Kind::Jj, true), true, false).is_empty());
    }

    #[test]
    fn unlanded_jj_dirt_still_blocks_removal() {
        assert_eq!(blockers(&wt(Kind::Jj, true), false, false), vec!["dirty"]);
    }

    #[test]
    fn landed_git_dirt_still_blocks_removal() {
        // Git dirt is *uncommitted* files. `git cherry` compares commits,
        // so a landed verdict says nothing about them — the veto stands.
        assert_eq!(blockers(&wt(Kind::Git, true), true, false), vec!["dirty"]);
    }

    #[test]
    fn force_clears_every_blocker() {
        let mut w = wt(Kind::Git, true);
        w.conflict = true;
        w.is_locked = true;
        assert!(blockers(&w, false, true).is_empty());
        assert_eq!(
            blockers(&w, false, false),
            vec!["dirty", "conflict", "locked"]
        );
    }
}
