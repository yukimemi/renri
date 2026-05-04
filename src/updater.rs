//! Self-update support for renri, mirroring rvpm's strategy.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};

/// Minimum fields from GitHub releases API.
/// Information about the latest release fetched from GitHub.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct LatestRelease {
    /// The tag name of the release (e.g., "v0.1.0").
    pub tag_name: String,
    /// The URL to the release's HTML page on GitHub.
    #[serde(default)]
    pub html_url: String,
}

/// The method by which renri was installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallMethod {
    /// Installed via `cargo install`.
    CargoInstall,
    /// A development build (running from `target/`).
    DevBuild,
    /// Downloaded and run as a standalone binary.
    DirectBinary,
}

/// Persistent state for the background update check.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UpdateCheckState {
    /// Unix timestamp of the last successful check.
    pub last_checked_unix: u64,
    /// The last known latest version tag.
    pub last_known_latest: Option<String>,
    /// The last known latest version URL.
    pub last_known_url: Option<String>,
}

/// Returns the default interval between update checks (24 hours).
pub fn default_interval() -> Duration {
    Duration::from_secs(86400) // 24h
}

/// Detects the installation method of the current executable.
pub fn detect_install_method(exe: &Path) -> InstallMethod {
    let s = exe.to_string_lossy().replace('\\', "/").to_lowercase();
    if s.contains("/target/debug/") || s.contains("/target/release/") {
        return InstallMethod::DevBuild;
    }
    let cargo_bin = std::env::var("CARGO_HOME")
        .ok()
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".cargo")))
        .map(|p| {
            p.join("bin")
                .to_string_lossy()
                .replace('\\', "/")
                .to_lowercase()
        });
    if let Some(bin) = cargo_bin {
        if s.starts_with(&format!("{}/", bin)) {
            return InstallMethod::CargoInstall;
        }
    }
    if s.contains("/.cargo/bin/") || s.contains("/cargo/bin/") {
        return InstallMethod::CargoInstall;
    }
    InstallMethod::DirectBinary
}

/// Checks if a newer version is available compared to the current version.
pub fn is_update_available(current: &str, latest_tag: &str) -> Result<bool> {
    let cur = semver::Version::parse(current)
        .map_err(|e| anyhow!("invalid current version `{}`: {}", current, e))?;
    let lat_str = latest_tag.trim_start_matches('v');
    let lat = semver::Version::parse(lat_str)
        .map_err(|e| anyhow!("invalid latest tag `{}`: {}", latest_tag, e))?;
    Ok(lat > cur)
}

/// Fetches the latest release information from the GitHub API.
pub fn check_latest_release() -> Result<LatestRelease> {
    let url = "https://api.github.com/repos/yukimemi/renri/releases/latest";
    let client = reqwest::blocking::Client::builder()
        .user_agent(format!("renri/{}", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(5))
        .build()?;
    let res = client.get(url).send()?;
    if !res.status().is_success() {
        return Err(anyhow!("GitHub releases API returned {}", res.status()));
    }
    let release: LatestRelease = res.json()?;
    Ok(release)
}

/// Returns the path to the update check state file in the user's data directory.
fn state_path() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("renri").join("last_update_check.json"))
}

/// Loads the update check state from the persistent state file.
pub fn load_check_state() -> Option<UpdateCheckState> {
    let p = state_path()?;
    let content = std::fs::read_to_string(p).ok()?;
    serde_json::from_str(&content).ok()
}

/// Saves the update check state to the persistent state file atomically.
pub fn save_check_state(state: &UpdateCheckState) -> Result<()> {
    if let Some(p) = state_path() {
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)?;
            let json = serde_json::to_string(state)?;
            let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
            use std::io::Write;
            tmp.write_all(json.as_bytes())?;
            tmp.persist(&p)?;
        }
    }
    Ok(())
}

/// Determines if an automatic update check should be performed based on the interval.
pub fn should_auto_check(
    state: Option<&UpdateCheckState>,
    interval: Duration,
    now: SystemTime,
) -> bool {
    let Some(state) = state else {
        return true;
    };
    let Ok(now_unix) = now.duration_since(SystemTime::UNIX_EPOCH) else {
        return true;
    };
    let elapsed = now_unix.as_secs().saturating_sub(state.last_checked_unix);
    elapsed >= interval.as_secs()
}

/// Formats a banner message to notify the user of an available update.
pub fn format_update_banner(current: &str, latest: &LatestRelease) -> String {
    let tag = latest.tag_name.trim_start_matches('v');
    let mut s = format!(
        "\u{2699} renri {} available (current {}) — run `renri self-update` to upgrade",
        tag, current
    );
    if !latest.html_url.is_empty() {
        s.push_str(&format!("\n  release notes: {}", latest.html_url));
    }
    s
}

/// Runs the self-update process, either check-only or interactive/non-interactive install.
pub fn run_self_update(yes: bool, check_only: bool, non_interactive: bool) -> Result<()> {
    let current = env!("CARGO_PKG_VERSION");
    let latest = check_latest_release()?;

    let available = is_update_available(current, &latest.tag_name)?;
    if !available {
        println!("\u{2713} renri {} is already up to date.", current);
        return Ok(());
    }

    let latest_clean = latest.tag_name.trim_start_matches('v');
    if check_only {
        println!(
            "\u{2699} renri {} available (current {}). Run `renri self-update` to install.",
            latest_clean, current
        );
        if !latest.html_url.is_empty() {
            println!("  release notes: {}", latest.html_url);
        }
        return Ok(());
    }

    if !yes {
        use std::io::IsTerminal;
        if non_interactive || !std::io::stdin().is_terminal() {
            anyhow::bail!(
                "non-interactive mode: use `--yes` to proceed with update to v{}",
                latest_clean
            );
        }

        eprint!("Update to v{}? [y/N] ", latest_clean);
        use std::io::Write;
        std::io::stderr().flush().ok();
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        let answer = answer.trim().to_ascii_lowercase();
        if answer != "y" && answer != "yes" {
            eprintln!("aborted.");
            return Ok(());
        }
    }

    let exe = std::env::current_exe()?;
    let method = detect_install_method(&exe);
    match method {
        InstallMethod::DevBuild => {
            anyhow::bail!(
                "\u{26a0} `{}` looks like a development build. Refusing to self-update.",
                exe.display()
            );
        }
        InstallMethod::CargoInstall => {
            let tmp = tempfile::Builder::new()
                .prefix("renri-self-update-")
                .tempdir()?;
            let tmp_root = tmp.path().to_path_buf();
            println!(
                "running: cargo install renri --version {} --locked --force --root {}",
                latest_clean,
                tmp_root.display()
            );
            let status = std::process::Command::new("cargo")
                .arg("install")
                .arg("renri")
                .arg("--version")
                .arg(latest_clean)
                .arg("--locked")
                .arg("--force")
                .arg("--root")
                .arg(&tmp_root)
                .status()?;
            if !status.success() {
                anyhow::bail!("cargo install failed");
            }
            let bin_name = if cfg!(windows) { "renri.exe" } else { "renri" };
            let new_exe = tmp_root.join("bin").join(bin_name);
            self_update::self_replace::self_replace(&new_exe)?;
            println!("\u{2713} renri v{} installed.", latest_clean);
        }
        InstallMethod::DirectBinary => {
            let status = self_update::backends::github::Update::configure()
                .repo_owner("yukimemi")
                .repo_name("renri")
                .bin_name("renri")
                .show_download_progress(true)
                .current_version(current)
                .target_version_tag(&latest.tag_name)
                .build()
                .map_err(|e| anyhow!("build: {}", e))?
                .update()
                .map_err(|e| anyhow!("update: {}", e))?;
            match status {
                self_update::Status::UpToDate(v) => {
                    println!("\u{2713} renri {} is already up to date.", v)
                }
                self_update::Status::Updated(v) => println!("\u{2713} renri v{} installed.", v),
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_update_available() {
        assert!(is_update_available("0.1.0", "v0.1.1").unwrap());
        assert!(!is_update_available("0.1.1", "v0.1.1").unwrap());
        assert!(!is_update_available("0.1.2", "v0.1.1").unwrap());
    }

    #[test]
    fn test_detect_install_method() {
        let p = PathBuf::from("/home/u/.cargo/bin/renri");
        assert_eq!(detect_install_method(&p), InstallMethod::CargoInstall);

        let p = PathBuf::from(
            r"C:\Users\yukimemi\src\github.com\yukimemi\renri\target\debug\renri.exe",
        );
        assert_eq!(detect_install_method(&p), InstallMethod::DevBuild);

        // Use a path that is unlikely to overlap with CARGO_HOME or home_dir for DirectBinary test.
        let p = PathBuf::from("/opt/renri-bin/renri");
        assert_eq!(detect_install_method(&p), InstallMethod::DirectBinary);
    }
}
