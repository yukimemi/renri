//! Cross-platform symlink. On Windows, falls back to `cmd /c mklink /D`
//! (junction equivalent for directories) when ordinary `symlink_dir`
//! requires Developer Mode that the user may not have enabled.

use std::path::Path;

use anyhow::{Context, Result, bail};

pub fn create(target: &Path, link: &Path) -> Result<()> {
    if let Some(parent) = link.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
    }

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link)
            .with_context(|| format!("symlink {} -> {}", link.display(), target.display()))?;
        Ok(())
    }

    #[cfg(windows)]
    {
        windows_symlink(target, link)
    }

    #[cfg(not(any(unix, windows)))]
    {
        let _ = (target, link);
        bail!("symlink hook is unsupported on this platform")
    }
}

#[cfg(windows)]
fn windows_symlink(target: &Path, link: &Path) -> Result<()> {
    let meta = std::fs::metadata(target)
        .with_context(|| format!("symlink target {} does not exist", target.display()))?;

    let result = if meta.is_dir() {
        std::os::windows::fs::symlink_dir(target, link)
    } else {
        std::os::windows::fs::symlink_file(target, link)
    };

    if let Err(e) = result {
        // Without Developer Mode, non-admin users can't create symlinks. For
        // directories, fall back to junction (mklink /J) which doesn't need
        // any privilege. For files, surface the error with a hint.
        if meta.is_dir() {
            return junction_link(target, link)
                .with_context(|| format!("symlink fallback failed: original error: {e}"));
        }
        bail!(
            "symlink {} -> {} failed: {e}\n\
             on Windows, file symlinks require Developer Mode (Settings → Privacy & security → For developers)",
            link.display(),
            target.display()
        );
    }
    Ok(())
}

#[cfg(windows)]
fn junction_link(target: &Path, link: &Path) -> Result<()> {
    let status = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(link)
        .arg(target)
        .status()
        .context("failed to spawn cmd /C mklink /J")?;
    if !status.success() {
        bail!(
            "junction creation failed: mklink /J {} {}",
            link.display(),
            target.display()
        );
    }
    Ok(())
}
