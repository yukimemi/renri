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

use super::{AddBranch, Backend, Kind, Worktree};

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

    /// Resolve a revset and return the result's short change_id, or `None`
    /// if the revset is empty / jj fails.
    fn resolve_rev(&self, revset: &str) -> Option<String> {
        let output = self
            .jj()
            .args([
                "log",
                "-r",
                revset,
                "--no-graph",
                "-T",
                "self.change_id().short()",
                "--limit",
                "1",
            ])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if s.is_empty() { None } else { Some(s) }
    }

    /// True if the commit at `<rev>` has no diff vs its parent (jj's
    /// per-workspace WC placeholder is the typical case).
    fn is_empty(&self, rev: &str) -> bool {
        let output = self
            .jj()
            .args([
                "log",
                "-r",
                rev,
                "--no-graph",
                "-T",
                "if(self.empty(), \"1\", \"0\")",
                "--limit",
                "1",
            ])
            .output();
        match output {
            Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim() == "1",
            _ => false,
        }
    }

    /// Pick the base for a `NewBranch` add. Heuristic, applied only when the
    /// user didn't pin one with `--from`:
    ///
    /// 1. If cwd's `@` has actual content (non-empty), respect it — the
    ///    user is forking off in-progress work and wants those changes.
    /// 2. Otherwise (`@` is jj's per-workspace empty WC placeholder), use
    ///    `trunk()` — jj's configured trunk revset, typically
    ///    `main@origin` / `master@origin`. Pushing the new branch then
    ///    doesn't drag the empty placeholder along as an intermediate
    ///    parent in the git history.
    /// 3. Final fallback: leave it as `@`, same as before this fix.
    fn default_new_branch_base(&self, user_input: &str) -> String {
        if user_input != "@" {
            return user_input.to_string();
        }
        if !self.is_empty("@") {
            return "@".to_string();
        }
        self.resolve_rev("trunk()")
            .unwrap_or_else(|| "@".to_string())
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
                vcs: Kind::Jj,
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
        let (workspace_name, base_rev_input) = match branch {
            AddBranch::NewBranch { name, base } => (name, base.unwrap_or("@")),
            AddBranch::ExistingBranch(name) => (name, name),
        };

        // For NewBranch with no explicit `--from`, prefer `trunk()` over
        // an empty `@` so the new workspace doesn't inherit jj's
        // per-workspace empty WC placeholder as its parent. See
        // `default_new_branch_base` for the heuristic.
        //
        // ExistingBranch is taken verbatim — `--name <bookmark>` means
        // "attach to this exact bookmark", even if its commit is empty.
        let resolved;
        let base_rev: &str = match branch {
            AddBranch::NewBranch { .. } => {
                resolved = self.default_new_branch_base(base_rev_input);
                resolved.as_str()
            }
            AddBranch::ExistingBranch(_) => base_rev_input,
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

    fn fetch(&self) -> Result<String> {
        let output = self
            .jj()
            .args(["git", "fetch"])
            .output()
            .context("failed to spawn `jj`")?;
        if !output.status.success() {
            bail!(
                "jj git fetch: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        // jj git fetch is mostly silent; show whichever of stderr/stdout had
        // content so the user gets feedback when bookmarks moved.
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        if !stderr.trim().is_empty() {
            Ok(stderr.into_owned())
        } else {
            Ok(stdout.into_owned())
        }
    }

    fn fetch_remote(&self, remote: &str) -> Result<String> {
        let output = self
            .jj()
            .args(["git", "fetch", "--remote", remote])
            .output()
            .context("failed to spawn `jj`")?;
        if !output.status.success() {
            bail!(
                "jj git fetch --remote {remote}: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        if !stderr.trim().is_empty() {
            Ok(stderr.into_owned())
        } else {
            Ok(stdout.into_owned())
        }
    }

    fn referenced_remote(&self, rev: &str) -> Option<String> {
        let output = self.jj().args(["git", "remote", "list"]).output().ok()?;
        if !output.status.success() {
            return None;
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        // `jj git remote list` rows are `<name> <url>` — the remote name is the
        // first whitespace-delimited token.
        let remotes: Vec<&str> = stdout
            .lines()
            .filter_map(|line| line.split_whitespace().next())
            .collect();
        referenced_remote(rev, &remotes).map(str::to_owned)
    }
}

/// If `rev` references a remote bookmark for one of `remotes`, return that
/// remote. jj's form is `<bookmark>@<remote>` (e.g. `main@origin`). The remote
/// must sit at the end of the rev or right before a `..` range operator, so
/// `@` / `@-` (the working-copy rev) don't match, and a remote named `a`
/// doesn't match `main@abc`. Revsets embedding a remote bookmark
/// (`main@origin..@`) still resolve.
fn referenced_remote<'a>(rev: &str, remotes: &[&'a str]) -> Option<&'a str> {
    remotes.iter().copied().find(|r| {
        let suffix = format!("@{r}");
        rev.ends_with(&suffix) || rev.contains(&format!("{suffix}.."))
    })
}

#[cfg(test)]
mod tests {
    use super::referenced_remote;

    #[test]
    fn referenced_remote_matches_remote_bookmark_forms() {
        let remotes = ["origin", "upstream"];
        assert_eq!(referenced_remote("main@origin", &remotes), Some("origin"));
        assert_eq!(
            referenced_remote("main@upstream", &remotes),
            Some("upstream")
        );
        // Revsets embedding a remote bookmark still resolve against it.
        assert_eq!(
            referenced_remote("main@origin..@", &remotes),
            Some("origin")
        );
    }

    #[test]
    fn referenced_remote_ignores_working_copy_and_local_revs() {
        let remotes = ["origin"];
        // The working-copy rev and its ancestors are not remote refs.
        assert_eq!(referenced_remote("@", &remotes), None);
        assert_eq!(referenced_remote("@-", &remotes), None);
        // A local bookmark name is not a remote ref.
        assert_eq!(referenced_remote("main", &remotes), None);
        assert_eq!(referenced_remote("trunk()", &remotes), None);
        // No configured remotes → nothing references a remote.
        assert_eq!(referenced_remote("main@origin", &[]), None);
    }

    #[test]
    fn referenced_remote_no_false_positive_for_short_remote_name() {
        // A remote named `a` must not match `main@abc` just because `@abc`
        // contains `@a`.
        let remotes = ["a"];
        assert_eq!(referenced_remote("main@abc", &remotes), None);
        // …but a genuine `<bookmark>@a` ref still resolves.
        assert_eq!(referenced_remote("main@a", &remotes), Some("a"));
    }
}
