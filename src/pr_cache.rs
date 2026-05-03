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
}
