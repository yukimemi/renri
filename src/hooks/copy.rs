use std::path::Path;

use anyhow::{Context, Result};

/// Copy a file or directory tree from `src` into `dst`. Creates parent
/// directories as needed. For directories, recurses with `walkdir`-like
/// behavior using std.
pub fn copy_path(src: &Path, dst: &Path) -> Result<()> {
    let meta =
        std::fs::metadata(src).with_context(|| format!("source not found: {}", src.display()))?;

    if let Some(parent) = dst.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
    }

    if meta.is_file() {
        std::fs::copy(src, dst)
            .with_context(|| format!("copy {} -> {}", src.display(), dst.display()))?;
        Ok(())
    } else if meta.is_dir() {
        copy_dir_recursive(src, dst)
    } else {
        anyhow::bail!(
            "copy hook: {} is neither a file nor a directory",
            src.display()
        );
    }
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst).with_context(|| format!("creating {}", dst.display()))?;
    for entry in std::fs::read_dir(src).with_context(|| format!("reading {}", src.display()))? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        let ft = entry.file_type()?;
        if ft.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else if ft.is_file() {
            std::fs::copy(&from, &to)
                .with_context(|| format!("copy {} -> {}", from.display(), to.display()))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn copies_a_single_file() {
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("src.txt");
        let dst = tmp.path().join("dst.txt");
        std::fs::write(&src, "hello").unwrap();

        copy_path(&src, &dst).unwrap();

        assert_eq!(std::fs::read_to_string(&dst).unwrap(), "hello");
    }

    #[test]
    fn creates_dst_parent_directories() {
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("src.txt");
        let dst = tmp.path().join("a/b/c/dst.txt");
        std::fs::write(&src, "x").unwrap();

        copy_path(&src, &dst).unwrap();
        assert!(dst.exists());
    }

    #[test]
    fn recursively_copies_a_directory() {
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("src");
        std::fs::create_dir_all(src.join("nested")).unwrap();
        std::fs::write(src.join("nested/file.txt"), "data").unwrap();

        let dst = tmp.path().join("dst");
        copy_path(&src, &dst).unwrap();

        assert_eq!(
            std::fs::read_to_string(dst.join("nested/file.txt")).unwrap(),
            "data"
        );
    }

    #[test]
    fn missing_source_is_an_error() {
        let tmp = TempDir::new().unwrap();
        let err = copy_path(&tmp.path().join("nope"), &tmp.path().join("dst")).unwrap_err();
        assert!(err.to_string().contains("source not found"));
    }
}
