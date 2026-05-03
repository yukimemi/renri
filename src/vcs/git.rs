//! Git backend — wraps `git worktree {list,add,remove}` via `Command`.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use super::{AddBranch, Backend, Worktree};

pub struct GitBackend {
    root: PathBuf,
}

impl GitBackend {
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
        }
    }

    fn git(&self) -> Command {
        let mut cmd = Command::new("git");
        cmd.current_dir(&self.root);
        cmd
    }
}

impl Backend for GitBackend {
    fn name(&self) -> &str {
        "git"
    }

    fn list(&self) -> Result<Vec<Worktree>> {
        let output = self
            .git()
            .args(["worktree", "list", "--porcelain"])
            .output()
            .context("failed to spawn `git`")?;

        if !output.status.success() {
            bail!(
                "git worktree list: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut wts = parse_porcelain(&stdout);
        // `parse_porcelain` populates `head` with the full 40-char hash from
        // `--porcelain` output, but the `head` contract is "short id". Reset
        // it before asking each worktree's repo for the short form, so a
        // failed `git_head_summary` (e.g. unborn branch) doesn't leave the
        // long form in place.
        for wt in wts.iter_mut() {
            wt.head = None;
            if wt.is_stale || wt.is_bare || !wt.path.exists() {
                continue;
            }
            if let Some((id, subject)) = git_head_summary(&wt.path) {
                wt.head = Some(id);
                wt.desc = subject;
            }
            wt.dirty = git_is_dirty(&wt.path);
        }
        Ok(wts)
    }

    fn add(&self, path: &Path, branch: AddBranch) -> Result<()> {
        let mut cmd = self.git();
        cmd.args(["worktree", "add"]);
        match branch {
            AddBranch::NewBranch { name, base } => {
                cmd.args(["-b", name]);
                cmd.arg(path);
                // `git worktree add -b <new> <path> <base>` forks off
                // <base> instead of HEAD. None → HEAD (the cwd worktree's
                // tip), which is git's default behavior.
                if let Some(base) = base {
                    cmd.arg(base);
                }
            }
            AddBranch::ExistingBranch(name) => {
                cmd.arg(path);
                cmd.arg(name);
            }
        }
        let status = cmd.status().context("failed to spawn `git`")?;
        if !status.success() {
            bail!("git worktree add failed");
        }
        Ok(())
    }

    fn remove(&self, path: &Path, force: bool) -> Result<()> {
        let mut cmd = self.git();
        cmd.args(["worktree", "remove"]);
        if force {
            cmd.arg("--force");
        }
        cmd.arg(path);
        let status = cmd.status().context("failed to spawn `git`")?;
        if !status.success() {
            bail!("git worktree remove failed");
        }
        Ok(())
    }

    fn origin_url(&self) -> Option<String> {
        let output = self
            .git()
            .args(["remote", "get-url", "origin"])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if s.is_empty() { None } else { Some(s) }
    }

    fn current_branch(&self) -> Option<String> {
        let output = self
            .git()
            .args(["branch", "--show-current"])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if s.is_empty() { None } else { Some(s) }
    }

    fn list_refs(&self) -> Result<Vec<String>> {
        let output = self
            .git()
            .args([
                "for-each-ref",
                "--format=%(refname:short)",
                "refs/heads",
                "refs/tags",
            ])
            .output()
            .context("failed to spawn `git`")?;
        if !output.status.success() {
            bail!(
                "git for-each-ref: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| l.to_string())
            .collect())
    }

    fn branch_exists(&self, name: &str) -> bool {
        let Ok(output) = self
            .git()
            .args(["rev-parse", "--verify", "--quiet"])
            .arg(format!("refs/heads/{name}"))
            .output()
        else {
            return false;
        };
        output.status.success()
    }

    fn prune(&self) -> Result<String> {
        let output = self
            .git()
            .args(["worktree", "prune", "--verbose"])
            .output()
            .context("failed to spawn `git`")?;
        if !output.status.success() {
            bail!(
                "git worktree prune: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    fn fetch(&self) -> Result<String> {
        let output = self
            .git()
            .args(["fetch", "origin"])
            .output()
            .context("failed to spawn `git`")?;
        if !output.status.success() {
            bail!(
                "git fetch origin: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        // git prints fetch progress / "From origin" lines on stderr.
        Ok(String::from_utf8_lossy(&output.stderr).into_owned())
    }
}

/// Parse the output of `git worktree list --porcelain`.
///
/// The format is a sequence of records separated by blank lines; each record
/// is a series of `key value` pairs (some keys are bare flags). The first
/// record is always the main worktree.
fn parse_porcelain(text: &str) -> Vec<Worktree> {
    let mut out = Vec::new();
    let mut current: Option<Worktree> = None;

    for line in text.lines() {
        if line.is_empty() {
            if let Some(wt) = current.take() {
                out.push(wt);
            }
            continue;
        }

        let (key, value) = match line.split_once(' ') {
            Some((k, v)) => (k, v),
            None => (line, ""),
        };

        match key {
            "worktree" => {
                if let Some(wt) = current.take() {
                    out.push(wt);
                }
                let path = PathBuf::from(value);
                current = Some(Worktree {
                    name: derive_name(&path),
                    path,
                    branch: None,
                    head: None,
                    desc: None,
                    dirty: false,
                    conflict: false,
                    is_main: false,
                    is_bare: false,
                    is_stale: false,
                    is_locked: false,
                });
            }
            "HEAD" => {
                if let Some(wt) = current.as_mut() {
                    wt.head = Some(value.to_string());
                }
            }
            "branch" => {
                if let Some(wt) = current.as_mut() {
                    let short = value.strip_prefix("refs/heads/").unwrap_or(value);
                    wt.branch = Some(short.to_string());
                    wt.name = short.to_string();
                }
            }
            "bare" => {
                if let Some(wt) = current.as_mut() {
                    wt.is_bare = true;
                }
            }
            "detached" => { /* branch stays None */ }
            "locked" => {
                if let Some(wt) = current.as_mut() {
                    wt.is_locked = true;
                }
            }
            "prunable" => {
                if let Some(wt) = current.as_mut() {
                    wt.is_stale = true;
                }
            }
            _ => {}
        }
    }

    if let Some(wt) = current.take() {
        out.push(wt);
    }

    if let Some(first) = out.first_mut() {
        first.is_main = true;
    }

    out
}

/// Run `git log -1 --format=%h<US>%s` inside `path` to fetch the short HEAD
/// id and one-line subject. Returns `None` if the worktree has no commits or
/// git fails (e.g. unborn branch).
fn git_head_summary(path: &Path) -> Option<(String, Option<String>)> {
    let output = Command::new("git")
        .current_dir(path)
        .args(["log", "-1", "--format=%h\x1f%s"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&output.stdout);
    let trimmed = s.trim_end_matches('\n');
    let mut parts = trimmed.splitn(2, '\x1f');
    let id = parts.next()?.to_string();
    if id.is_empty() {
        return None;
    }
    let subject = parts.next().map(str::to_owned).filter(|s| !s.is_empty());
    Some((id, subject))
}

/// `git status --porcelain` non-empty → working copy is dirty. Errors map
/// to "not dirty" (we'd rather under-report than spuriously flag).
fn git_is_dirty(path: &Path) -> bool {
    let Ok(output) = Command::new("git")
        .current_dir(path)
        .args(["status", "--porcelain"])
        .output()
    else {
        return false;
    };
    output.status.success() && !output.stdout.is_empty()
}

fn derive_name(path: &Path) -> String {
    path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("?")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_porcelain_basic() {
        let text = "\
worktree /home/me/proj
HEAD aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
branch refs/heads/main

worktree /home/me/proj-feature
HEAD bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
branch refs/heads/feature/x
";
        let wts = parse_porcelain(text);
        assert_eq!(wts.len(), 2);

        assert!(wts[0].is_main);
        assert_eq!(wts[0].path, PathBuf::from("/home/me/proj"));
        assert_eq!(wts[0].branch.as_deref(), Some("main"));
        assert_eq!(wts[0].name, "main");

        assert!(!wts[1].is_main);
        assert_eq!(wts[1].path, PathBuf::from("/home/me/proj-feature"));
        assert_eq!(wts[1].branch.as_deref(), Some("feature/x"));
        assert_eq!(wts[1].name, "feature/x");
    }

    #[test]
    fn parse_porcelain_handles_detached_bare_locked_prunable() {
        let text = "\
worktree /repo
bare

worktree /repo/wt-detached
HEAD cccccccccccccccccccccccccccccccccccccccc
detached

worktree /repo/wt-locked
HEAD dddddddddddddddddddddddddddddddddddddddd
branch refs/heads/locked-feature
locked some reason

worktree /repo/wt-stale
HEAD eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee
branch refs/heads/old
prunable gitdir file points to non-existent location
";
        let wts = parse_porcelain(text);
        assert_eq!(wts.len(), 4);

        assert!(wts[0].is_main && wts[0].is_bare);

        assert!(wts[1].branch.is_none(), "detached → no branch");

        assert!(wts[2].is_locked);
        assert_eq!(wts[2].branch.as_deref(), Some("locked-feature"));

        assert!(wts[3].is_stale);
    }

    #[test]
    fn parse_porcelain_empty() {
        assert_eq!(parse_porcelain(""), vec![]);
    }

    #[test]
    fn parse_porcelain_trailing_blank_line() {
        let text = "\
worktree /repo
HEAD aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
branch refs/heads/main

";
        let wts = parse_porcelain(text);
        assert_eq!(wts.len(), 1);
        assert_eq!(wts[0].name, "main");
    }
}
