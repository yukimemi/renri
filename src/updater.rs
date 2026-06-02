//! Self-update support for renri, using `kaishin` library.

use anyhow::Result;
use std::path::PathBuf;
use std::time::Duration;

/// Returns the default interval between update checks (24 hours).
pub fn default_interval() -> Duration {
    kaishin::default_interval()
}

/// Resolves the path to the background update-check throttle state file
/// (`last_update_check.json`).
///
/// This file is transient throttle bookkeeping, so it belongs under the OS
/// **cache** dir (XDG `~/.cache`), not the data dir — mirroring where
/// [`crate::pr_cache`] keeps its `pr-cache.json`. kaishin's default
/// (`Checker::new`) would otherwise resolve it under `dirs::data_dir()`, so we
/// override it here. `None` falls back to that kaishin default when the cache
/// dir can't be resolved.
fn cache_state_path() -> Option<PathBuf> {
    dirs::cache_dir().map(|d| d.join("renri").join("last_update_check.json"))
}

/// Runs the self-update process, either check-only or interactive/non-interactive install.
pub fn run_self_update(yes: bool, check_only: bool, non_interactive: bool) -> Result<()> {
    let opts = kaishin::KaishinOptions::new(
        "yukimemi",
        env!("CARGO_PKG_NAME"),
        env!("CARGO_PKG_NAME"),
        env!("CARGO_PKG_VERSION"),
    );
    let upd_opts = kaishin::UpdateOptions::new()
        .yes(yes)
        .check_only(check_only)
        .non_interactive(non_interactive);

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async { kaishin::run_self_update(&opts, upd_opts).await })
}

/// A high-level handler for managing background update checks.
///
/// The async methods ([`check_and_save`](Self::check_and_save) /
/// [`auto_update`](Self::auto_update)) are meant to be driven as `tokio` tasks
/// spawned on `main`'s runtime so they overlap command execution, rather than
/// on a raw OS worker thread. The type is cheaply [`Clone`]able (kaishin's
/// `Checker` is) so it can be moved into a spawned task.
#[derive(Clone)]
pub struct Checker {
    inner: kaishin::Checker,
}

impl Checker {
    pub fn new() -> Result<Self> {
        let opts = kaishin::KaishinOptions::new(
            "yukimemi",
            env!("CARGO_PKG_NAME"),
            env!("CARGO_PKG_NAME"),
            env!("CARGO_PKG_VERSION"),
        );
        let mut inner = kaishin::Checker::new(env!("CARGO_PKG_NAME"), opts);
        // Keep the throttle state file under the OS cache dir (transient,
        // XDG), alongside the PR cache, instead of kaishin's data-dir default.
        if let Some(state_path) = cache_state_path() {
            inner = inner.state_path(state_path);
        }
        Ok(Self { inner })
    }

    pub fn interval(mut self, interval: Duration) -> Self {
        self.inner = self.inner.interval(interval);
        self
    }

    pub fn should_check(&self) -> bool {
        self.inner.should_check()
    }

    /// `notify` mode: fetch the latest release and persist the throttle state.
    /// `Ok(Some)` iff a newer release exists; `Ok(None)` on a clean no-update
    /// fetch. Driven on `main`'s tokio runtime as a spawned task.
    pub async fn check_and_save(&self) -> Result<Option<kaishin::LatestRelease>> {
        self.inner.check_and_save().await
    }

    /// `install` mode: silently check for and install a newer release.
    ///
    /// kaishin handles the throttle, the cross-process lock, the dev-build
    /// skip, and the actual self-replace. `Ok(Some(latest))` means the binary
    /// was actually replaced; `Ok(None)` means nothing was installed
    /// (throttled, no newer release, dev build, or another process holds the
    /// lock); `Err` means the install failed. Driven on `main`'s tokio runtime
    /// as a spawned task so it overlaps command execution.
    pub async fn auto_update(&self) -> Result<Option<kaishin::LatestRelease>> {
        self.inner.auto_update().await
    }

    pub fn cached_update(&self) -> Option<kaishin::LatestRelease> {
        self.inner.cached_update()
    }

    pub fn format_banner(&self, latest: &kaishin::LatestRelease) -> String {
        self.inner.format_banner(latest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_checker_init() {
        // Test that checker can be initialized without panic.
        let _ = Checker::new().unwrap();
    }
}
