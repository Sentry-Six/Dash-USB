use std::time::Duration;

use anyhow::{bail, Result};
use tokio::process::Command;
use tracing::debug;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

pub async fn run(name: &str, args: &[&str]) -> Result<String> {
    run_with_timeout(DEFAULT_TIMEOUT, name, args).await
}

/// Returns stdout; a non-zero exit becomes an error carrying cleaned stderr.
pub async fn run_with_timeout(timeout: Duration, name: &str, args: &[&str]) -> Result<String> {
    debug!(cmd = name, ?args, "executing command");

    // kill_on_drop: the timeout drops this future, and without the flag the
    // child keeps running detached (a hung `cp --reflink` or fsck could hold
    // its loop device forever).
    let result = tokio::time::timeout(timeout, async {
        Command::new(name)
            .args(args)
            .kill_on_drop(true)
            .output()
            .await
    })
    .await;

    match result {
        Err(_) => bail!("command timed out after {:?}", timeout),
        Ok(Err(e)) => bail!("failed to execute command: {}", e),
        Ok(Ok(output)) => {
            if output.status.success() {
                Ok(String::from_utf8_lossy(&output.stdout).into_owned())
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                bail!(
                    "command failed (exit {}): {}",
                    output.status.code().unwrap_or(-1),
                    clean_stderr(&stderr)
                )
            }
        }
    }
}

/// Drops curl progress-meter lines and collapses blank runs.
pub fn clean_stderr(msg: &str) -> String {
    let mut result = String::with_capacity(msg.len());
    for line in msg.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("% Total") || is_curl_progress_line(trimmed) {
            continue;
        }
        if !result.is_empty() {
            result.push('\n');
        }
        result.push_str(line);
    }
    while result.contains("\n\n\n") {
        result = result.replace("\n\n\n", "\n\n");
    }
    result.trim().to_string()
}

fn is_curl_progress_line(line: &str) -> bool {
    // Example: "  0  1234    0  0    0     0      0      0 --:--:-- --:--:-- --:--:--     0"
    let parts: Vec<&str> = line.split_whitespace().collect();
    parts.len() >= 6 && parts.iter().take(6).all(|p| p.parse::<u64>().is_ok() || *p == "0")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clean_stderr_removes_curl_progress() {
        let input = "% Total    % Received\n  0  1234    0  0    0     0      0      0\nActual error message";
        let cleaned = clean_stderr(input);
        assert_eq!(cleaned, "Actual error message");
    }

    #[test]
    fn test_clean_stderr_preserves_real_errors() {
        let input = "Error: something went wrong";
        assert_eq!(clean_stderr(input), "Error: something went wrong");
    }
}
