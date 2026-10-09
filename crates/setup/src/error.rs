//! Distinguishes user-correctable configuration failures from transient setup
//! failures so boot auto-resume does not retry invalid settings indefinitely.

/// A setup failure that requires configuration changes before retrying.
#[derive(Debug, Clone)]
pub struct ConfigError(pub String);

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ConfigError {}
