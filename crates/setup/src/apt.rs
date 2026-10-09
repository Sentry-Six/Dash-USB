//! `apt-get install` with one index refresh and retry for stale or
//! temporarily inconsistent package mirrors.

use std::time::Duration;

use anyhow::{Context, Result};

/// Run `apt-get install -y <packages>`, refreshing the package index and
/// retrying once on failure. `progress` is called only when a retry happens.
pub async fn apt_install(
    progress: impl Fn(&str),
    packages: &[&str],
    timeout: Duration,
) -> Result<()> {
    let mut args: Vec<&str> = vec!["-y", "install"];
    args.extend(packages);

    if sentryusb_shell::run_with_timeout(timeout, "apt-get", &args).await.is_ok() {
        return Ok(());
    }

    progress("Refreshing package index and retrying...");
    let _ = sentryusb_shell::run_with_timeout(
        Duration::from_secs(300),
        "apt-get", &["update"],
    ).await;
    sentryusb_shell::run_with_timeout(timeout, "apt-get", &args).await
        .context("apt-get install failed after refresh + retry")?;
    Ok(())
}
