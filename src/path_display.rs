//! Cosmetic path-to-string conversion for CLI output.
//!
//! `Path::display()` is fine for one-offs but two papercuts surface in
//! `renri list` / `renri config show`:
//!
//! - Windows `canonicalize()` returns extended-path form (`\\?\C:\…`).
//!   Real and absolute, but ugly to read.
//! - Templates render `\` and `/` mixed (env-var-derived backslash + literal
//!   forward slash from the user's template string), so paths come out as
//!   `C:\Users\me/wt\…/renri/main`.
//!
//! `display_path` strips the UNC prefix and folds separators to the host's
//! native form. It is purely cosmetic — never use the result for filesystem
//! lookups.

use std::path::Path;

pub fn display_path(p: &Path) -> String {
    let s = p.to_string_lossy();

    // Strip Windows extended-path prefixes:
    //   `\\?\UNC\server\share` → `\\server\share` (network shares)
    //   `\\?\C:\…`             → `C:\…`           (local paths)
    let stripped: String = if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        rest.to_owned()
    } else {
        s.into_owned()
    };

    if cfg!(windows) {
        stripped.replace('/', "\\")
    } else {
        stripped
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    #[cfg(windows)]
    fn strips_unc_prefix_on_windows() {
        let out = display_path(&PathBuf::from(r"\\?\C:\Users\me\proj"));
        assert_eq!(out, r"C:\Users\me\proj");
    }

    #[test]
    #[cfg(windows)]
    fn folds_forward_to_back_on_windows() {
        let out = display_path(&PathBuf::from(r"C:\Users\me/wt/foo"));
        assert_eq!(out, r"C:\Users\me\wt\foo");
    }

    #[test]
    #[cfg(windows)]
    fn extended_unc_share_round_trips() {
        let out = display_path(&PathBuf::from(r"\\?\UNC\server\share\foo"));
        assert_eq!(out, r"\\server\share\foo");
    }

    #[test]
    #[cfg(windows)]
    fn plain_unc_share_passthrough() {
        let out = display_path(&PathBuf::from(r"\\server\share\foo"));
        assert_eq!(out, r"\\server\share\foo");
    }

    #[test]
    #[cfg(unix)]
    fn passthrough_on_unix() {
        let out = display_path(&PathBuf::from("/home/me/proj"));
        assert_eq!(out, "/home/me/proj");
    }
}
