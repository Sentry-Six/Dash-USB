//! Setup error classification.
//!
//! `ConfigError` marks a failure caused by bad or contradictory user
//! configuration (two keep-awake providers, an invalid `SENTRY_CASE`, a
//! too-short `AP_PASS`, a missing required archive field), as distinct from a
//! transient or hardware failure.
//!
//! The web server downcasts to this type to decide whether to keep silently
//! auto-resuming setup on every boot (safe for transient failures: a network
//! blip clears itself) or to stop and surface a "fix your settings and retry"
//! state. Config failures retry identically forever, which is the setup boot
//! loop users hit.

/// A setup failure caused by invalid user configuration.
///
/// Recoverable only by the user changing settings: re-running setup unchanged
/// fails the same way, so the boot-loop auto-resume MUST halt on these rather
/// than spin forever.
#[derive(Debug, Clone)]
pub struct ConfigError(pub String);

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ConfigError {}
