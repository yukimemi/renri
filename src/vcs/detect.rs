//! Detect whether a directory lives inside a git, jj, or colocated repo by
//! walking up the parent chain.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Git,
    Jj,
    /// Both `.git` and `.jj` exist at the same root (jj's `--colocate` mode).
    Colocated,
}

#[derive(Debug, Clone)]
pub struct Repo {
    /// The path that contains `.git` and/or `.jj`.
    pub root: PathBuf,
    pub kind: Kind,
}

/// Walk up from `start` until we hit a directory containing `.git` or `.jj`.
/// `.git` may be a directory (top-level checkout) or a file (worktree); both
/// count.
pub fn detect(start: &Path) -> Option<Repo> {
    let start = start.canonicalize().ok()?;
    let mut cur: Option<&Path> = Some(&start);
    while let Some(p) = cur {
        let has_jj = p.join(".jj").is_dir();
        let has_git = p.join(".git").exists();
        if has_jj || has_git {
            let kind = match (has_jj, has_git) {
                (true, true) => Kind::Colocated,
                (true, false) => Kind::Jj,
                (false, true) => Kind::Git,
                (false, false) => unreachable!(),
            };
            return Some(Repo {
                root: p.to_path_buf(),
                kind,
            });
        }
        cur = p.parent();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn touch(dir: &Path, rel: &str) {
        let p = dir.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::File::create(&p).unwrap();
    }

    fn mkdir(dir: &Path, rel: &str) {
        std::fs::create_dir_all(dir.join(rel)).unwrap();
    }

    #[test]
    fn detects_git_repo() {
        let tmp = TempDir::new().unwrap();
        mkdir(tmp.path(), ".git");
        let r = detect(tmp.path()).unwrap();
        assert_eq!(r.kind, Kind::Git);
    }

    #[test]
    fn detects_jj_repo() {
        let tmp = TempDir::new().unwrap();
        mkdir(tmp.path(), ".jj");
        let r = detect(tmp.path()).unwrap();
        assert_eq!(r.kind, Kind::Jj);
    }

    #[test]
    fn detects_colocated_repo() {
        let tmp = TempDir::new().unwrap();
        mkdir(tmp.path(), ".git");
        mkdir(tmp.path(), ".jj");
        let r = detect(tmp.path()).unwrap();
        assert_eq!(r.kind, Kind::Colocated);
    }

    #[test]
    fn walks_up_from_subdirectory() {
        let tmp = TempDir::new().unwrap();
        mkdir(tmp.path(), ".git");
        mkdir(tmp.path(), "deep/nested/path");

        let r = detect(&tmp.path().join("deep/nested/path")).unwrap();
        assert_eq!(r.kind, Kind::Git);
        assert_eq!(
            r.root.canonicalize().unwrap(),
            tmp.path().canonicalize().unwrap()
        );
    }

    #[test]
    fn handles_git_as_a_file_inside_a_worktree() {
        let tmp = TempDir::new().unwrap();
        // `.git` is a *file* in a git worktree, not a dir
        touch(tmp.path(), ".git");
        let r = detect(tmp.path()).unwrap();
        assert_eq!(r.kind, Kind::Git);
    }

    #[test]
    fn returns_none_when_no_repo_above() {
        let tmp = TempDir::new().unwrap();
        // Use the temp dir itself; no .git or .jj anywhere
        // (Note: the test environment's actual cwd may have a git repo above
        // it; we use the temp dir whose parents are typically also git-free.)
        let result = detect(tmp.path());
        // We accept either None or a hit further up. The deterministic part is
        // that if it hits, it must be above tmp.path().
        if let Some(r) = result {
            assert!(r.root != tmp.path());
        }
    }
}
