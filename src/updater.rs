//! Self-update support for renri, using `kaishin` library.

use anyhow::Result;
use std::time::Duration;

/// Returns the default interval between update checks (24 hours).
pub fn default_interval() -> Duration {
    kaishin::default_interval()
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

/// A high-level handler for managing background update checks in a blocking context.
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
        let inner = kaishin::Checker::new(env!("CARGO_PKG_NAME"), opts);
        Ok(Self { inner })
    }

    pub fn interval(mut self, interval: Duration) -> Self {
        self.inner = self.inner.interval(interval);
        self
    }

    pub fn should_check(&self) -> bool {
        self.inner.should_check()
    }

    pub fn check_and_save(&self) -> Result<Option<kaishin::LatestRelease>> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        rt.block_on(async { self.inner.check_and_save().await })
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
