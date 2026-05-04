//! Find renri-managed projects by walking the configured worktree root.
//!
//! Used when the user invokes a verb (`renri list`, `renri cd`, …) from a
//! cwd that is *not* inside any git/jj repo. Instead of failing with
//! "not in a repo", we offer them a picker over the projects already
//! materialized under their worktree root (default `~/wt`).
//!
//! Strategy: walk the root with bounded depth, stop descending into any
//! directory that already contains `.git` or `.jj` (its children would be
//! files inside the repo, not other projects), and group the resulting
//! worktrees by their shared store. Two worktrees of the same project share
//! a `git --git-common-dir` (or `jj root`); two worktrees of *different*
//! projects do not.
//!
//! The picker only needs `entry_path` per project — any worktree of the
//! project is a valid cwd to hand to `vcs::detect()`. `label` is for
//! display.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// One project surfaced by [`scan`]. Identified by its shared VCS store
/// (`git_common_dir` or `jj_root`) so that all worktrees of the same repo
/// collapse into a single picker entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    /// Human-readable label used in pickers. Looks like
    /// `yukimemi/renri` for the default `<owner>/<repo>/<branch>` layout;
    /// falls back to a relative path or the basename when the layout is
    /// shallower or non-default.
    pub label: String,

    /// Absolute paths of every worktree we found that belongs to this
    /// project. Sorted so output is deterministic.
    pub worktrees: Vec<PathBuf>,
}

impl Project {
    /// Pick a worktree to use as the effective cwd for the picked project.
    /// First non-stale entry wins; we don't care which since git/jj `list`
    /// returns the full set regardless.
    pub fn entry_path(&self) -> &Path {
        // `worktrees` is non-empty by construction (a project with no
        // worktrees would never have been pushed into the result list).
        &self.worktrees[0]
    }
}

/// How deep to descend below `worktree_root`. The default layout is
/// `<owner>/<repo>/<branch>` (depth 3); allowing up to 5 covers a few
/// reasonable customizations (`<host>/<owner>/<repo>/<branch>`,
/// `<owner>/<repo>/<category>/<branch>`) without turning a stray `node_modules`
/// near the worktree root into a multi-second walk.
const MAX_DEPTH: usize = 5;

/// Walk `root` for git/jj worktrees and group them by their shared VCS
/// store. Returns an empty list (not an error) when:
///   - `root` doesn't exist yet (fresh user, no worktrees created).
///   - `root` exists but contains no managed worktrees.
///   - any individual entry can't be read — best-effort, don't poison the
///     whole walk just because one subdir is unreadable.
pub fn scan(root: &Path) -> Vec<Project> {
    if !root.is_dir() {
        return Vec::new();
    }

    let mut worktrees: Vec<PathBuf> = Vec::new();
    visit(root, 0, &mut worktrees);

    // Group by repo-identity. We use the VCS-reported common dir / repo
    // root path as the key so two worktrees of the same repo map to the
    // same bucket. BTreeMap → deterministic ordering of pickers.
    let mut groups: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
    for wt in worktrees {
        let key = repo_identity(&wt).unwrap_or_else(|| wt.to_string_lossy().into_owned());
        groups.entry(key).or_default().push(wt);
    }

    let mut projects: Vec<Project> = groups
        .into_values()
        .map(|mut worktrees| {
            worktrees.sort();
            let label = derive_label(root, &worktrees);
            Project { label, worktrees }
        })
        .collect();

    projects.sort_by(|a, b| a.label.cmp(&b.label));
    projects
}

/// Recursive walk. Stops at `MAX_DEPTH` and at every directory that's
/// itself a worktree (no point descending into a repo's `src/` looking for
/// nested repos).
fn visit(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > MAX_DEPTH {
        return;
    }
    if is_worktree(dir) {
        out.push(dir.to_path_buf());
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        // Skip hidden / dotted dirs (`.cache`, `.tmp`, …) — they're not
        // expected to contain worktrees, and a literal `.git`/`.jj` is
        // already what `is_worktree` checks above.
        let basename = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if basename.starts_with('.') {
            continue;
        }
        let Ok(ft) = entry.file_type() else {
            continue;
        };
        // Symlinks: don't follow. Cycle protection is cheap this way and
        // a symlinked worktree would be discovered via its real path
        // anyway if it lives under the root.
        if ft.is_symlink() || !ft.is_dir() {
            continue;
        }
        visit(&path, depth + 1, out);
    }
}

/// True when `path` looks like a git worktree or jj workspace: either has
/// a `.git` (file or dir) or a `.jj/` directory directly inside it.
pub(crate) fn is_worktree(path: &Path) -> bool {
    path.join(".git").exists() || path.join(".jj").is_dir()
}

/// Stable identity of the underlying repo. Two worktrees of the same repo
/// return the same string; two unrelated repos return different ones.
///
/// Resolves the on-disk pointer rather than shelling out per-worktree:
/// `git rev-parse --git-common-dir` and `jj workspace root` both work but
/// each fork would be hundreds of milliseconds × N worktrees. Reading
/// `.git` / `.jj/repo` directly is microseconds.
///
/// **Normalization**: returns the parent dir of the store, not the store
/// itself, so a colocated git+jj repo where one user adds a worktree via
/// `--vcs git` and another via jj groups them as the same project (their
/// stores live at `<root>/.git` and `<root>/.jj/repo` respectively, but
/// `<root>` is shared).
///
/// Returns `None` only when both markers exist but neither is parseable
/// (corrupt repo). The caller substitutes the path itself in that case,
/// which keeps the worktree visible in the picker as its own project
/// rather than swallowing it.
fn repo_identity(worktree: &Path) -> Option<String> {
    // jj first: colocated repos have both `.git` and `.jj`, but renri's
    // policy is jj-priority for colocated repos.
    if worktree.join(".jj").is_dir() {
        if let Some(p) = resolve_jj_repo(worktree) {
            return Some(canonical(&normalize_to_repo_root(&p)));
        }
    }
    if let Some(p) = resolve_git_common_dir(worktree) {
        return Some(canonical(&normalize_to_repo_root(&p)));
    }
    None
}

/// Walk a store path back to the repo root that contains it. Both
/// `<root>/.git` and `<root>/.jj/repo` (and the secondary-worktree forms
/// that resolve into them) collapse to `<root>` here, so colocated repos
/// where one worktree is git-flavored and another is jj-flavored share an
/// identity.
///
/// Conservative on purpose: only strips a tail matching a *known* store
/// layout. A project legitimately named `repo` (i.e. its root is
/// `<wt>/repo`) is not mistakenly walked up — the basename has to be
/// adjacent to a `.git` / `.jj` parent component for stripping to fire.
fn normalize_to_repo_root(store: &Path) -> PathBuf {
    let basename =
        |p: &Path| -> Option<String> { p.file_name().and_then(|n| n.to_str()).map(str::to_owned) };

    // git secondary-worktree gitdir: `<root>/.git/worktrees/<name>` →
    // strip 3 components.
    if let Some(parent) = store.parent() {
        if let Some(grandparent) = parent.parent() {
            if basename(parent).as_deref() == Some("worktrees")
                && basename(grandparent).as_deref() == Some(".git")
            {
                if let Some(root) = grandparent.parent() {
                    return root.to_path_buf();
                }
            }
        }
    }

    // jj `<root>/.jj/repo` (file or dir): tail = `repo` under `.jj`.
    if basename(store).as_deref() == Some("repo") {
        if let Some(parent) = store.parent() {
            if basename(parent).as_deref() == Some(".jj") {
                if let Some(root) = parent.parent() {
                    return root.to_path_buf();
                }
            }
        }
    }

    // git main-checkout `<root>/.git`.
    if basename(store).as_deref() == Some(".git") {
        if let Some(root) = store.parent() {
            return root.to_path_buf();
        }
    }

    // Doesn't look like a known store path — leave it as-is.
    store.to_path_buf()
}

/// Find the shared jj repo dir for a workspace.
///
/// Layout:
///   - `.jj/repo` is a *directory* → this workspace IS the main one, that
///     directory is the shared store.
///   - `.jj/repo` is a *file* → secondary workspace; the file contains a
///     path (relative to its own location) pointing at the main store.
///     Format is a single line of UTF-8, no header.
fn resolve_jj_repo(workspace: &Path) -> Option<PathBuf> {
    let marker = workspace.join(".jj").join("repo");
    if marker.is_dir() {
        return Some(marker);
    }
    let contents = std::fs::read_to_string(&marker).ok()?;
    let pointer = contents.trim();
    if pointer.is_empty() {
        return None;
    }
    let target = Path::new(pointer);
    if target.is_absolute() {
        Some(target.to_path_buf())
    } else {
        // The path inside `.jj/repo` is relative to the file's parent dir.
        marker.parent().map(|p| p.join(target))
    }
}

/// Find the shared git store for a worktree.
///
/// Layout:
///   - `.git` is a *directory* → main checkout; that's the store.
///   - `.git` is a *file* → secondary worktree; first line is
///     `gitdir: <path>` pointing at `<main>/.git/worktrees/<name>/`. The
///     shared store is that path's `<main>/.git/`, i.e. two levels up.
fn resolve_git_common_dir(worktree: &Path) -> Option<PathBuf> {
    let dotgit = worktree.join(".git");
    let meta = std::fs::metadata(&dotgit).ok()?;
    if meta.is_dir() {
        return Some(dotgit);
    }
    let contents = std::fs::read_to_string(&dotgit).ok()?;
    let line = contents.lines().next()?;
    let pointer = line.strip_prefix("gitdir:").map(str::trim).unwrap_or(line);
    if pointer.is_empty() {
        return None;
    }
    let gitdir = Path::new(pointer);
    let gitdir = if gitdir.is_absolute() {
        gitdir.to_path_buf()
    } else {
        // git's relative gitdir is from the worktree, not from `.git`
        // itself — verified by `man gitrepository-layout`.
        worktree.join(gitdir)
    };
    // Secondary worktree gitdirs look like `<main>/.git/worktrees/<n>/`;
    // step back to `<main>/.git/`. Path components are stable across
    // platforms (git uses `/`, but `Path::parent` handles both).
    let common = gitdir
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf);
    common.or(Some(gitdir))
}

fn canonical(p: &Path) -> String {
    p.canonicalize()
        .unwrap_or_else(|_| p.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

/// Build a display label for a project from the relative path of its first
/// worktree. For the default `~/wt/<owner>/<repo>/<branch>` layout, this
/// strips the leaf so the label reads `<owner>/<repo>` — the project name
/// users see in `gh repo` URLs and on disk.
fn derive_label(root: &Path, worktrees: &[PathBuf]) -> String {
    let first = &worktrees[0];
    let rel = first.strip_prefix(root).unwrap_or(first);
    let parts: Vec<&str> = rel
        .components()
        .filter_map(|c| c.as_os_str().to_str())
        .collect();
    match parts.len() {
        0 => first
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("(unknown)")
            .to_string(),
        1 => parts[0].to_string(),
        // Drop the branch leaf — the project label is the *repo*, not the
        // worktree we happened to pick first.
        _ => parts[..parts.len() - 1].join("/"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn mkrepo(root: &Path, rel: &str, marker: &str) -> PathBuf {
        let p = root.join(rel);
        fs::create_dir_all(&p).unwrap();
        if marker == ".git-file" {
            fs::write(p.join(".git"), "gitdir: /tmp/whatever\n").unwrap();
        } else {
            fs::create_dir_all(p.join(marker)).unwrap();
        }
        p
    }

    #[test]
    fn scan_returns_empty_when_root_missing() {
        let tmp = TempDir::new().unwrap();
        let projects = scan(&tmp.path().join("does-not-exist"));
        assert!(projects.is_empty());
    }

    #[test]
    fn scan_returns_empty_when_no_worktrees() {
        let tmp = TempDir::new().unwrap();
        // an empty owner/repo dir with no .git/.jj inside
        fs::create_dir_all(tmp.path().join("owner/repo")).unwrap();
        let projects = scan(tmp.path());
        assert!(projects.is_empty());
    }

    #[test]
    fn scan_finds_default_layout_worktrees() {
        let tmp = TempDir::new().unwrap();
        // Layout: <root>/owner/repo/<branch>/
        mkrepo(tmp.path(), "yuki/renri/main", ".git");
        mkrepo(tmp.path(), "yuki/renri/feat-x", ".git");
        mkrepo(tmp.path(), "yuki/teravars/main", ".git");
        let projects = scan(tmp.path());
        // 3 worktrees, but git can't tell us they share a store (these
        // are bare marker dirs, not real repos). Without a working
        // `git rev-parse --git-common-dir`, every worktree becomes its
        // own project. Verify the labels collapse correctly.
        // We expect at least the three labels to be derived.
        let labels: Vec<&str> = projects.iter().map(|p| p.label.as_str()).collect();
        assert!(
            labels.iter().any(|l| l.contains("renri")),
            "labels: {labels:?}"
        );
        assert!(
            labels.iter().any(|l| l.contains("teravars")),
            "labels: {labels:?}"
        );
    }

    #[test]
    fn scan_handles_git_as_a_file() {
        let tmp = TempDir::new().unwrap();
        // git worktrees use `.git` file pointing at `<main>/.git/worktrees/<n>`
        mkrepo(tmp.path(), "owner/repo/branch", ".git-file");
        let projects = scan(tmp.path());
        assert_eq!(projects.len(), 1);
        assert!(projects[0].label.contains("owner"));
    }

    #[test]
    fn scan_does_not_descend_into_a_worktree() {
        let tmp = TempDir::new().unwrap();
        let wt = mkrepo(tmp.path(), "owner/repo/main", ".git");
        // Plant a fake nested `.git` deep inside the worktree — we must
        // NOT discover it as a separate project.
        fs::create_dir_all(wt.join("vendor/sub/.git")).unwrap();
        let projects = scan(tmp.path());
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].worktrees.len(), 1);
        assert_eq!(projects[0].worktrees[0], wt);
    }

    #[test]
    fn scan_skips_dot_directories_at_top_level() {
        let tmp = TempDir::new().unwrap();
        mkrepo(tmp.path(), ".cache/something/foo", ".git");
        mkrepo(tmp.path(), "real/repo/main", ".git");
        let projects = scan(tmp.path());
        // Only `real/repo/main` should be picked up.
        assert_eq!(projects.len(), 1);
        assert!(projects[0].label.contains("real"));
    }

    #[test]
    fn derive_label_default_layout() {
        let tmp = TempDir::new().unwrap();
        let wt = tmp.path().join("yuki/renri/main");
        fs::create_dir_all(&wt).unwrap();
        let label = derive_label(tmp.path(), &[wt]);
        assert_eq!(label, "yuki/renri");
    }

    #[test]
    fn resolve_jj_repo_dir_form() {
        let tmp = TempDir::new().unwrap();
        let ws = tmp.path().join("ws");
        fs::create_dir_all(ws.join(".jj/repo")).unwrap();
        let resolved = resolve_jj_repo(&ws).unwrap();
        assert_eq!(resolved, ws.join(".jj/repo"));
    }

    #[test]
    fn resolve_jj_repo_file_form_relative() {
        let tmp = TempDir::new().unwrap();
        // Layout:  <tmp>/main/.jj/repo (dir)         — main store
        //          <tmp>/wt/.jj/repo  (file → ../main/.jj/repo)
        let main_store = tmp.path().join("main/.jj/repo");
        fs::create_dir_all(&main_store).unwrap();
        let secondary = tmp.path().join("wt/.jj");
        fs::create_dir_all(&secondary).unwrap();
        // Relative path is from the .jj/repo file itself.
        fs::write(secondary.join("repo"), "../../main/.jj/repo").unwrap();

        let resolved_main = resolve_jj_repo(&tmp.path().join("main")).unwrap();
        let resolved_secondary = resolve_jj_repo(&tmp.path().join("wt")).unwrap();
        // Both should canonicalize to the same path.
        assert_eq!(
            resolved_main.canonicalize().unwrap(),
            resolved_secondary.canonicalize().unwrap(),
            "main + secondary must point at the same store"
        );
    }

    #[test]
    fn resolve_git_common_dir_dir_form() {
        let tmp = TempDir::new().unwrap();
        let main = tmp.path().join("main");
        fs::create_dir_all(main.join(".git")).unwrap();
        let resolved = resolve_git_common_dir(&main).unwrap();
        assert_eq!(resolved, main.join(".git"));
    }

    #[test]
    fn normalize_collapses_git_jj_to_same_root() {
        // Default colocated layout: <root>/.git and <root>/.jj live next
        // to each other. Both should normalize to <root>.
        let root = Path::new("/some/repo");
        let from_git = normalize_to_repo_root(&root.join(".git"));
        let from_jj_dir = normalize_to_repo_root(&root.join(".jj/repo"));
        let from_secondary_git = normalize_to_repo_root(&root.join(".git/worktrees/wt-foo"));
        assert_eq!(from_git, root);
        assert_eq!(from_jj_dir, root);
        assert_eq!(from_secondary_git, root);
    }

    #[test]
    fn resolve_git_common_dir_secondary_worktree() {
        let tmp = TempDir::new().unwrap();
        let main = tmp.path().join("main");
        fs::create_dir_all(main.join(".git/worktrees/wt-foo")).unwrap();
        // Secondary worktree: `.git` is a file pointing at the main's
        // per-worktree subdir.
        let wt = tmp.path().join("wt-foo");
        fs::create_dir_all(&wt).unwrap();
        fs::write(
            wt.join(".git"),
            format!("gitdir: {}\n", main.join(".git/worktrees/wt-foo").display()),
        )
        .unwrap();

        let resolved = resolve_git_common_dir(&wt).unwrap();
        assert_eq!(
            resolved.canonicalize().unwrap(),
            main.join(".git").canonicalize().unwrap(),
            "secondary worktree must resolve to the main `.git/`"
        );
    }

    #[test]
    fn derive_label_shallow() {
        let tmp = TempDir::new().unwrap();
        let wt = tmp.path().join("solo");
        fs::create_dir_all(&wt).unwrap();
        let label = derive_label(tmp.path(), &[wt]);
        assert_eq!(label, "solo");
    }
}
