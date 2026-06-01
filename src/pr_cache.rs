//! Cached lookup of GitHub PRs by branch name.
//!
//! `renri list` only renders the PR column when `[ui] show_pr = true` in
//! `renri.toml`. Behind that flag, this module:
//!
//! 1. Loads a per-repo cache file from `<cache_dir>/renri/<owner>__<repo>/pr-cache.json`.
//! 2. If the cache is missing or older than `pr_cache_ttl_hours`, spawns
//!    `gh pr list --state all --limit 200 --json number,state,headRefName`
//!    and rewrites the cache.
//! 3. Returns a `HashMap<branch_name, PrInfo>` for the renderer.
//!
//! GitHub-only by design — non-GitHub remotes silently get an empty map and
//! the column shows blanks. `gh` is checked at fetch time and a missing
//! `gh` binary downgrades to "no PR data" rather than erroring the whole
//! list command.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::vcs::Worktree;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrInfo {
    pub number: u64,
    /// `OPEN` | `MERGED` | `CLOSED` — straight from `gh`.
    pub state: String,
    pub head_ref_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CacheFile {
    /// Unix timestamp (seconds) of the last refresh.
    fetched_at: u64,
    prs: Vec<PrInfo>,
}

/// Look up PRs by branch name. Lazy-refreshes via `gh` when the cached file
/// is missing or older than `ttl_hours`. Returns an empty map (without
/// erroring) when `gh` is unavailable or the host isn't GitHub.
pub fn load_or_refresh(
    owner: &str,
    repo: &str,
    host: Option<&str>,
    ttl_hours: u64,
    force: bool,
) -> HashMap<String, PrInfo> {
    if host.is_none_or(|h| h != "github.com") {
        return HashMap::new();
    }
    let path = match cache_path(owner, repo) {
        Some(p) => p,
        None => return HashMap::new(),
    };

    let needs_refresh = force
        || match read_cache(&path) {
            Some(c) => is_stale(c.fetched_at, ttl_hours),
            None => true,
        };

    if needs_refresh {
        if let Ok(prs) = fetch_via_gh(owner, repo) {
            let _ = write_cache(&path, &prs);
            return index_by_branch(prs);
        }
    }

    read_cache(&path)
        .map(|c| index_by_branch(c.prs))
        .unwrap_or_default()
}

/// Build a `https://github.com/<owner>/<repo>/pull/<number>` URL.
///
/// `host` is informational — only `github.com` is fetched (see
/// [`load_or_refresh`]) and that's hardcoded in the URL prefix. When/if
/// GitHub Enterprise support lands, swap the prefix on host. We keep the
/// `host` parameter so the call site is honest about which value would
/// be plugged in.
pub fn pr_url(_host: Option<&str>, owner: &str, repo: &str, number: u64) -> String {
    format!("https://github.com/{owner}/{repo}/pull/{number}")
}

/// Wrap `text` in OSC 8 hyperlink escape codes pointing at `url`.
///
/// OSC 8 (`ESC ] 8 ;; <url> ST <text> ESC ] 8 ;; ST`) is a terminal
/// extension supported by wezterm, kitty, iTerm2, Windows Terminal,
/// VTE-based terminals, and most modern emulators. Terminals that don't
/// recognize the sequence silently drop it and render `text` plain — so
/// it's safe to always emit. We use `\x1b\\` (ST) over `\x07` (BEL)
/// because tmux is documented to handle ST correctly.
pub fn osc8_hyperlink(url: &str, text: &str) -> String {
    format!("\x1b]8;;{url}\x1b\\{text}\x1b]8;;\x1b\\")
}

/// Look up a PR for a worktree row, trying the bookmark / branch first
/// and falling back to the workspace (or worktree) name.
///
/// **Why the fallback**: when a PR merges and GitHub deletes the head
/// branch, `jj git fetch` removes the local remote-tracking bookmark.
/// The jj workspace's `@`-commit then has no bookmark, so `Worktree::branch`
/// becomes `None` and the obvious lookup misses — even though the cache
/// still remembers the PR by its `head_ref_name`. renri's `add <n>`
/// always names the workspace and bookmark identically, so `Worktree::name`
/// is a reliable second key. Branch wins when both match (the user may
/// have intentionally moved the bookmark to a different name).
///
/// **Why the slash/dash normalization**: renri's default layout renders the
/// worktree *path* with `vcs.branch | replace(from='/', to='-')`, so a
/// workspace created from a dashed name (or by an older renri) is named e.g.
/// `feat-x` while the GitHub PR head ref stays `feat/x`. Once that PR merges
/// the bookmark is dropped (`branch` is `None`) and the exact name lookup
/// above misses too — leaving the row with a blank PR column and invisible to
/// `remove --merged`. When the exact keys miss we retry with `/` flattened to
/// `-` on both sides so the merged PR is still found.
pub fn lookup_for_worktree<'a>(
    w: &Worktree,
    prs: &'a HashMap<String, PrInfo>,
) -> Option<&'a PrInfo> {
    if let Some(branch) = w.branch.as_deref() {
        if let Some(pr) = prs.get(branch) {
            return Some(pr);
        }
    }
    if let Some(pr) = prs.get(w.name.as_str()) {
        return Some(pr);
    }
    lookup_normalized(w, prs)
}

/// Slash/dash-normalized fallback for [`lookup_for_worktree`]. Compares the
/// worktree's branch / name against every PR head ref with `/` flattened to
/// `-`. When several head refs collapse to the same normalized key, pick the
/// best-ranked state (OPEN > MERGED > CLOSED), breaking ties by lower PR
/// number so the choice is deterministic regardless of `HashMap` iteration
/// order.
fn lookup_normalized<'a>(w: &Worktree, prs: &'a HashMap<String, PrInfo>) -> Option<&'a PrInfo> {
    // Compare allocation-free: this runs for every worktree row in `list`
    // against up to 200 cached PRs, so we don't want a `String::replace` per
    // iteration. `/` and `-` are ASCII, so they never appear inside a
    // multi-byte UTF-8 sequence — byte-by-byte comparison with the two
    // treated as equal is correct.
    let eq_normalized = |a: &str, b: &str| {
        a.len() == b.len()
            && a.bytes().zip(b.bytes()).all(|(x, y)| {
                let fold = |c: u8| if c == b'/' { b'-' } else { c };
                fold(x) == fold(y)
            })
    };

    let mut best: Option<&PrInfo> = None;
    for pr in prs.values() {
        let matches_branch = w
            .branch
            .as_deref()
            .is_some_and(|b| eq_normalized(b, &pr.head_ref_name));
        if !matches_branch && !eq_normalized(&w.name, &pr.head_ref_name) {
            continue;
        }
        best = Some(match best {
            None => pr,
            Some(cur) => {
                let (rank, cur_rank) = (state_rank(&pr.state), state_rank(&cur.state));
                if rank < cur_rank || (rank == cur_rank && pr.number < cur.number) {
                    pr
                } else {
                    cur
                }
            }
        });
    }
    best
}

fn index_by_branch(prs: Vec<PrInfo>) -> HashMap<String, PrInfo> {
    // Multiple PRs per branch can exist (e.g. closed-then-reopened); prefer
    // OPEN, then MERGED, then anything else. Stable for predictable display.
    let mut map: HashMap<String, PrInfo> = HashMap::new();
    for pr in prs {
        let better = match map.get(&pr.head_ref_name) {
            None => true,
            Some(existing) => state_rank(&pr.state) < state_rank(&existing.state),
        };
        if better {
            map.insert(pr.head_ref_name.clone(), pr);
        }
    }
    map
}

fn state_rank(state: &str) -> u8 {
    match state {
        "OPEN" => 0,
        "MERGED" => 1,
        "CLOSED" => 2,
        _ => 3,
    }
}

fn cache_path(owner: &str, repo: &str) -> Option<PathBuf> {
    let base = dirs::cache_dir()?;
    Some(
        base.join("renri")
            .join(format!("{owner}__{repo}"))
            .join("pr-cache.json"),
    )
}

fn read_cache(path: &std::path::Path) -> Option<CacheFile> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn write_cache(path: &std::path::Path, prs: &[PrInfo]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let cache = CacheFile {
        fetched_at: now,
        prs: prs.to_vec(),
    };
    let json = serde_json::to_vec_pretty(&cache).context("serialize PR cache")?;
    std::fs::write(path, json).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

fn is_stale(fetched_at: u64, ttl_hours: u64) -> bool {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let age = now.saturating_sub(fetched_at);
    age > Duration::from_secs(ttl_hours * 3600).as_secs()
}

fn fetch_via_gh(owner: &str, repo: &str) -> Result<Vec<PrInfo>> {
    // Pass `--repo <owner>/<repo>` so `gh` doesn't try to auto-detect from
    // the current dir's `.git/`. renri worktrees created by `jj workspace
    // add` have no `.git/` (it lives in the colocated main checkout), so
    // the auto-detect path fails with "not a git repository".
    //
    // We rename `headRefName` → `head_ref_name` server-side via jq so the
    // JSON matches our snake-case Rust field. Otherwise we'd need a serde
    // alias on every field.
    let slug = format!("{owner}/{repo}");
    let output = Command::new("gh")
        .args([
            "pr",
            "list",
            "--repo",
            &slug,
            "--state",
            "all",
            "--limit",
            "200",
            "--json",
            "number,state,headRefName",
            "--jq",
            r#"map({number, state, head_ref_name: .headRefName})"#,
        ])
        .output()
        .context("failed to spawn `gh` (install GitHub CLI to enable PR display)")?;
    if !output.status.success() {
        anyhow::bail!(
            "gh pr list failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let prs: Vec<PrInfo> =
        serde_json::from_slice(&output.stdout).context("parsing gh pr list output")?;
    Ok(prs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_picks_open_over_closed_for_same_branch() {
        let prs = vec![
            PrInfo {
                number: 1,
                state: "CLOSED".into(),
                head_ref_name: "feat/x".into(),
            },
            PrInfo {
                number: 2,
                state: "OPEN".into(),
                head_ref_name: "feat/x".into(),
            },
        ];
        let map = index_by_branch(prs);
        assert_eq!(map["feat/x"].number, 2);
    }

    #[test]
    fn is_stale_detects_old_cache() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert!(is_stale(now.saturating_sub(2 * 3600), 1));
        assert!(!is_stale(now.saturating_sub(60), 24));
    }

    fn wt(name: &str, branch: Option<&str>) -> Worktree {
        Worktree {
            name: name.into(),
            path: std::path::PathBuf::new(),
            branch: branch.map(String::from),
            head: None,
            desc: None,
            dirty: false,
            conflict: false,
            is_main: false,
            is_bare: false,
            is_stale: false,
            is_locked: false,
            vcs: crate::vcs::Kind::Jj,
        }
    }

    fn pr(number: u64, state: &str, head: &str) -> PrInfo {
        PrInfo {
            number,
            state: state.into(),
            head_ref_name: head.into(),
        }
    }

    #[test]
    fn lookup_for_worktree_prefers_branch_when_present() {
        let prs = index_by_branch(vec![pr(1, "OPEN", "feat-foo"), pr(2, "MERGED", "feat-bar")]);
        // workspace was renamed: name = "feat-bar" (original), branch was
        // moved to "feat-foo" intentionally. Branch wins.
        let row = wt("feat-bar", Some("feat-foo"));
        let found = lookup_for_worktree(&row, &prs).unwrap();
        assert_eq!(found.number, 1);
    }

    #[test]
    fn lookup_for_worktree_falls_back_to_name_when_branch_missing() {
        // The actual UX bug: PR merged, branch deleted upstream, jj fetch
        // dropped the local bookmark, so the workspace's @ has no
        // bookmark → branch is None. The cache still has the merged PR
        // keyed by the original head_ref_name == workspace name.
        let prs = index_by_branch(vec![pr(19, "MERGED", "feat-discover-pj")]);
        let row = wt("feat-discover-pj", None);
        let found = lookup_for_worktree(&row, &prs).unwrap();
        assert_eq!(found.number, 19);
        assert_eq!(found.state, "MERGED");
    }

    #[test]
    fn lookup_for_worktree_falls_back_when_branch_present_but_no_match() {
        // Branch is set but doesn't match anything (e.g. anonymous local
        // bookmark). Fall through to the name lookup.
        let prs = index_by_branch(vec![pr(7, "OPEN", "feat-baz")]);
        let row = wt("feat-baz", Some("some-other-bookmark"));
        let found = lookup_for_worktree(&row, &prs).unwrap();
        assert_eq!(found.number, 7);
    }

    #[test]
    fn lookup_for_worktree_returns_none_when_neither_matches() {
        let prs = index_by_branch(vec![pr(1, "OPEN", "feat-foo")]);
        let row = wt("unrelated", None);
        assert!(lookup_for_worktree(&row, &prs).is_none());
    }

    #[test]
    fn lookup_for_worktree_normalizes_dash_name_to_slash_head_ref() {
        // The real-world kanade bug: workspace named `feat-explode-spec-cache`
        // (dashes, from renri's path layout / an older add), PR head ref is
        // `feat/explode-spec-cache` (slash), and the bookmark is gone after
        // merge so branch is None. The dashed name must still resolve to the
        // merged PR via slash/dash normalization.
        let prs = index_by_branch(vec![pr(120, "MERGED", "feat/explode-spec-cache")]);
        let row = wt("feat-explode-spec-cache", None);
        let found = lookup_for_worktree(&row, &prs).unwrap();
        assert_eq!(found.number, 120);
        assert_eq!(found.state, "MERGED");
    }

    #[test]
    fn lookup_for_worktree_normalizes_via_dashed_branch_too() {
        // Same normalization applies when the branch (bookmark) is present
        // but dashed, e.g. a release worktree `release-v0.34.0` tracking a
        // bookmark of the same dashed shape while the PR head ref is sliced.
        let prs = index_by_branch(vec![pr(115, "MERGED", "release/v0.34.0")]);
        let row = wt("release-v0.34.0", Some("release-v0.34.0"));
        let found = lookup_for_worktree(&row, &prs).unwrap();
        assert_eq!(found.number, 115);
    }

    #[test]
    fn lookup_normalized_prefers_open_over_merged_on_collision() {
        // Two distinct head refs collapse to the same normalized key
        // (`feat-x`). Prefer the OPEN one so the row reflects live work.
        let prs = index_by_branch(vec![pr(2, "MERGED", "feat/x"), pr(5, "OPEN", "feat-x")]);
        let row = wt("feat-x", None);
        let found = lookup_for_worktree(&row, &prs).unwrap();
        assert_eq!(found.number, 5);
        assert_eq!(found.state, "OPEN");
    }

    #[test]
    fn lookup_normalized_breaks_state_ties_by_lower_number() {
        // Both normalize to `feat-x` and are MERGED — lower PR number wins so
        // the pick is stable across HashMap iteration order.
        let prs = index_by_branch(vec![pr(9, "MERGED", "feat/x"), pr(3, "MERGED", "feat-x")]);
        let row = wt("feat-x", None);
        let found = lookup_for_worktree(&row, &prs).unwrap();
        assert_eq!(found.number, 3);
    }

    #[test]
    fn lookup_for_worktree_exact_match_wins_over_normalized() {
        // An exact dashed head ref must not be shadowed by the normalized
        // fallback: `feat-foo` resolves to its own PR, not a `feat/foo`.
        let prs = index_by_branch(vec![pr(1, "OPEN", "feat-foo"), pr(2, "MERGED", "feat/foo")]);
        let row = wt("feat-foo", None);
        let found = lookup_for_worktree(&row, &prs).unwrap();
        assert_eq!(found.number, 1);
    }

    #[test]
    fn pr_url_builds_github_url() {
        assert_eq!(
            pr_url(Some("github.com"), "yukimemi", "renri", 27),
            "https://github.com/yukimemi/renri/pull/27"
        );
        // host = None still produces the github.com URL — host is
        // informational while we only support github.com.
        assert_eq!(
            pr_url(None, "yukimemi", "renri", 1),
            "https://github.com/yukimemi/renri/pull/1"
        );
    }

    #[test]
    fn osc8_hyperlink_wraps_text_with_escape_codes() {
        let s = osc8_hyperlink("https://example.com/x", "click");
        assert!(s.starts_with("\x1b]8;;https://example.com/x\x1b\\"));
        assert!(s.ends_with("click\x1b]8;;\x1b\\"));
        // The visible text must be present uninterrupted between the two
        // OSC 8 sequences so terminals that ignore the escape still show
        // it correctly.
        assert!(s.contains("\x1b\\click\x1b]8;;"));
    }
}
