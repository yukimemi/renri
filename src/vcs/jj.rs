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

    /// Look up the on-disk root of a workspace.
    ///
    /// `Err`        — couldn't even spawn `jj` (binary missing, fs error). Bubble up.
    /// `Ok(None)`   — `jj` ran but couldn't locate the workspace root. The
    ///                workspace is genuinely stale (its directory has been
    ///                removed out from under jj).
    /// `Ok(Some(p))` — current root path.
    fn workspace_root(&self, name: &str) -> Result<Option<PathBuf>> {
        let output = self
            .jj()
            .args(["workspace", "root", "--name", name])
            .output()
            .context("failed to spawn `jj`")?;
        if !output.status.success() {
            return Ok(None);
        }
        let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if s.is_empty() {
            Ok(None)
        } else {
            Ok(Some(PathBuf::from(s)))
        }
    }

    /// Look up the @-commit's short change id, bookmark list, description
    /// first line, and dirty / conflict flags in a single `jj log` call.
    fn workspace_status(&self, name: &str) -> Option<JjStatus> {
        // Sentinel \x1f (ASCII unit separator) keeps fields apart even if
        // descriptions contain tabs or commas. Order: id, bookmarks, dirty
        // ("1" = non-empty @ = WC has changes), conflict, desc.
        let template = "self.change_id().short() ++ \"\\x1f\" \
                        ++ self.bookmarks().map(|b| b.name()).join(\",\") ++ \"\\x1f\" \
                        ++ if(self.empty(), \"0\", \"1\") ++ \"\\x1f\" \
                        ++ if(self.conflict(), \"1\", \"0\") ++ \"\\x1f\" \
                        ++ self.description().first_line()";
        let output = self
            .jj()
            .args([
                "log",
                "-r",
                &format!("{name}@"),
                "--no-graph",
                "-T",
                template,
            ])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let s = String::from_utf8_lossy(&output.stdout);
        let mut parts = s.splitn(5, '\x1f');
        let id = parts.next()?.trim().to_string();
        let bookmarks = parts
            .next()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        let dirty = parts.next().map(str::trim) == Some("1");
        let conflict = parts.next().map(str::trim) == Some("1");
        let desc = parts
            .next()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        if id.is_empty() {
            None
        } else {
            Some(JjStatus {
                id,
                bookmarks,
                dirty,
                conflict,
                desc,
            })
        }
    }
}

struct JjStatus {
    id: String,
    bookmarks: Option<String>,
    dirty: bool,
    conflict: bool,
    desc: Option<String>,
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

            // `workspace_root` returns `Ok(None)` when jj ran cleanly but
            // couldn't locate the workspace's directory (stale). Spawn errors
            // / IO errors propagate via `?` so we don't conflate them with
            // genuine staleness.
            let (path, is_stale) = match self.workspace_root(&name)? {
                Some(p) => (p, false),
                None => (PathBuf::new(), true),
            };

            let (head, branch, desc, dirty, conflict) = if is_stale {
                (None, None, None, false, false)
            } else {
                match self.workspace_status(&name) {
                    Some(st) => (Some(st.id), st.bookmarks, st.desc, st.dirty, st.conflict),
                    None => (None, None, None, false, false),
                }
            };

            wts.push(Worktree {
                name: name.clone(),
                path,
                branch,
                head,
                desc,
                dirty,
                conflict,
                is_main: name == "default",
                is_bare: false,
                is_stale,
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
            // Forget when either:
            //   - jj already considers it stale (workspace_root failed → on-disk
            //     dir is gone or jj's metadata can't resolve it), or
            //   - the path resolves but the directory was rm-rf'd manually.
            let needs_forget = wt.is_stale || !wt.path.as_os_str().is_empty() && !wt.path.exists();
            if !needs_forget {
                continue;
            }
            let status = self
                .jj()
                .args(["workspace", "forget", &wt.name])
                .status()
                .context("failed to spawn `jj`")?;
            if status.success() {
                forgotten.push(wt.name);
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
