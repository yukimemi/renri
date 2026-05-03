//! jujutsu (jj) backend — wraps `jj workspace {add,list,forget,root}` and
//! related bookmark / git commands. The unique-to-jj concerns we cover:
//!
//! - `prune` analog: jj has no built-in equivalent of `git worktree prune`.
//!   We implement it by listing workspaces and `jj workspace forget`-ing
//!   the ones whose root directory has been deleted on disk.
//! - "Stale" detection: jj's stale state is a per-workspace WC concern;
//!   not surfaced in MVP `list` output.
//! - Branch-vs-bookmark: a renri "branch" maps to a jj bookmark with the
//!   same name, created (or moved-to) at workspace creation time.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use super::{AddBranch, Backend, Worktree};

pub struct JjBackend {
    root: PathBuf,
}

impl JjBackend {
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
        }
    }

    fn jj(&self) -> Command {
        let mut cmd = Command::new("jj");
        cmd.current_dir(&self.root);
        cmd
    }

    fn workspace_root(&self, name: &str) -> Option<PathBuf> {
        let output = self
            .jj()
            .args(["workspace", "root", "--name", name])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if s.is_empty() {
            None
        } else {
            Some(PathBuf::from(s))
        }
    }
}

impl Backend for JjBackend {
    fn name(&self) -> &str {
        "jj"
    }

    fn list(&self) -> Result<Vec<Worktree>> {
        let output = self
            .jj()
            .args(["workspace", "list"])
            .output()
            .context("failed to spawn `jj`")?;
        if !output.status.success() {
            bail!(
                "jj workspace list: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut wts = Vec::new();

        for line in stdout.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            // Format: `<name>: <change_id> <commit> ...`
            let Some((name, _)) = line.split_once(':') else {
                continue;
            };
            let name = name.trim().to_string();
            let path = self
                .workspace_root(&name)
                .unwrap_or_else(|| self.root.clone());
            wts.push(Worktree {
                name: name.clone(),
                path,
                branch: None, // bookmark lookup deferred — surfaced via current_branch
                head: None,
                is_main: name == "default",
                is_bare: false,
                is_stale: false,
                is_locked: false,
            });
        }

        Ok(wts)
    }

    fn add(&self, path: &Path, branch: AddBranch) -> Result<()> {
        // jj's `workspace add` defaults the base revision to the **default**
        // workspace's @, ignoring the cwd workspace. That surprises users
        // who run `renri add` from a secondary workspace expecting to fork
        // off their work-in-progress. We pass `-r` explicitly:
        //
        //   NewBranch + base=None      → fork off cwd workspace's @
        //   NewBranch + base=Some(ref) → fork off <ref>
        //   ExistingBranch(name)       → fork off <name> (the bookmark's tip)
        //
        // The git backend reaches the same outcome via
        // `git worktree add -b <new> <path> [<base>]`.
        let (workspace_name, base_rev) = match branch {
            AddBranch::NewBranch { name, base } => (name, base.unwrap_or("@")),
            AddBranch::ExistingBranch(name) => (name, name),
        };

        let status = self
            .jj()
            .args(["workspace", "add", "-r", base_rev])
            .arg(path)
            .args(["--name", workspace_name])
            .status()
            .context("failed to spawn `jj`")?;
        if !status.success() {
            bail!("jj workspace add failed");
        }

        // Position the new workspace's working copy and attach the bookmark.
        // The first `jj workspace add` ran in `self.root` and resolved a
        // relative `path` against that. The follow-up has its own
        // `current_dir`, which `std::process::Command` treats as relative
        // to the parent process's CWD — different base. Absolutize against
        // `self.root` so the two invocations always agree.
        let abs_path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.root.join(path)
        };
        let mut cmd = Command::new("jj");
        cmd.current_dir(&abs_path);
        match branch {
            AddBranch::NewBranch { name, .. } => {
                let status = cmd
                    .args(["bookmark", "create", name])
                    .status()
                    .context("failed to spawn `jj`")?;
                if !status.success() {
                    bail!("jj bookmark create failed");
                }
            }
            AddBranch::ExistingBranch(name) => {
                // Already created at `name`'s tip via `-r`; explicit `edit`
                // ensures the workspace's @ tracks the bookmark even if jj
                // updates the semantics in the future.
                let status = cmd
                    .args(["edit", name])
                    .status()
                    .context("failed to spawn `jj`")?;
                if !status.success() {
                    bail!("jj edit {name} failed");
                }
            }
        }

        Ok(())
    }

    fn remove(&self, path: &Path, _force: bool) -> Result<()> {
        let wts = self.list()?;
        let target = wts
            .iter()
            .find(|w| {
                w.path
                    .canonicalize()
                    .ok()
                    .zip(path.canonicalize().ok())
                    .is_some_and(|(a, b)| a == b)
                    || w.path == path
            })
            .ok_or_else(|| anyhow::anyhow!("no jj workspace at {}", path.display()))?;

        if target.is_main {
            bail!("cannot forget the default jj workspace");
        }

        let status = self
            .jj()
            .args(["workspace", "forget", &target.name])
            .status()
            .context("failed to spawn `jj`")?;
        if !status.success() {
            bail!("jj workspace forget {} failed", target.name);
        }

        if path.exists() {
            std::fs::remove_dir_all(path)
                .with_context(|| format!("removing {} after forget", path.display()))?;
        }
        Ok(())
    }

    fn origin_url(&self) -> Option<String> {
        let output = self.jj().args(["git", "remote", "list"]).output().ok()?;
        if !output.status.success() {
            return None;
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        for line in stdout.lines() {
            let line = line.trim();
            if let Some(rest) = line.strip_prefix("origin ") {
                return Some(rest.trim().to_string());
            }
            if let Some(rest) = line.strip_prefix("origin: ") {
                return Some(rest.trim().to_string());
            }
        }
        None
    }

    fn current_branch(&self) -> Option<String> {
        let output = self
            .jj()
            .args([
                "log",
                "-r",
                "@",
                "--no-graph",
                "-T",
                "self.bookmarks().map(|b| b.name()).join(\",\")",
            ])
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
            .jj()
            .args(["bookmark", "list", "-T", "self.name() ++ \"\\n\""])
            .output()
            .context("failed to spawn `jj`")?;
        if !output.status.success() {
            bail!(
                "jj bookmark list: {}",
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
        let Ok(output) = self.jj().args(["bookmark", "list", name]).output() else {
            return false;
        };
        output.status.success() && !output.stdout.is_empty()
    }

    fn prune(&self) -> Result<String> {
        let wts = self.list()?;
        let mut forgotten = Vec::new();
        for wt in wts {
            if wt.is_main {
                continue;
            }
            if !wt.path.exists() {
                let status = self
                    .jj()
                    .args(["workspace", "forget", &wt.name])
                    .status()
                    .context("failed to spawn `jj`")?;
                if status.success() {
                    forgotten.push(wt.name);
                }
            }
        }
        if forgotten.is_empty() {
            Ok(String::new())
        } else {
            Ok(format!(
                "forgot stale workspace(s): {}",
                forgotten.join(", ")
            ))
        }
    }
}
