use std::collections::HashMap;
use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::sync::OnceLock;

use anyhow::{Context, Result};

pub type SetupConfig = HashMap<String, String>;

pub const DEFAULT_CONFIG_PATH: &str = "/root/dashusb.conf";

pub const BOOT_CONFIG_PATH: &str = "/boot/firmware/dashusb.conf";

const LEGACY_BOOT_PATH: &str = "/boot/dashusb.conf";

/// First existing config path.
///
/// `DASHUSB_CONFIG_PATH` replaces the on-device search chain for development.
pub fn find_config_path() -> &'static str {
    static ENV_OVERRIDE: OnceLock<Option<&'static str>> = OnceLock::new();
    let ov = ENV_OVERRIDE.get_or_init(|| {
        std::env::var("DASHUSB_CONFIG_PATH")
            .ok()
            .filter(|s| !s.is_empty())
            .map(|s| &*Box::leak(s.into_boxed_str()))
    });
    if let Some(p) = ov {
        return p;
    }
    for p in [DEFAULT_CONFIG_PATH, BOOT_CONFIG_PATH, LEGACY_BOOT_PATH] {
        if Path::new(p).exists() {
            return p;
        }
    }
    DEFAULT_CONFIG_PATH
}

/// Base directory for runtime state such as the preferences store.
/// `DASHUSB_MUTABLE_DIR` redirects it for off-Pi development; unset
/// means `/mutable`.
pub fn mutable_dir() -> &'static str {
    static DIR: OnceLock<&'static str> = OnceLock::new();
    DIR.get_or_init(|| {
        std::env::var("DASHUSB_MUTABLE_DIR")
            .ok()
            .filter(|s| !s.is_empty())
            .map(|s| &*Box::leak(s.trim_end_matches('/').to_owned().into_boxed_str()))
            .unwrap_or("/mutable")
    })
}

/// Returns (active exports, commented-out exports).
pub fn parse_file(path: &str) -> Result<(SetupConfig, SetupConfig)> {
    // The setup wizard creates the configuration on its first save.
    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            return Err(e).with_context(|| format!("failed to open config file: {}", path))
        }
    };

    let mut active = SetupConfig::new();
    let mut commented = SetupConfig::new();

    for line in content.lines() {
        if let Some((key, val)) = parse_export_line(line) {
            active.insert(key, val);
        } else if let Some((key, val)) = parse_commented_export_line(line) {
            commented.insert(key, val);
        }
    }

    Ok((active, commented))
}

/// Writes the config back, preserving comments and structure. Keys in
/// `new_config` become active exports; previously active keys absent
/// from it are commented out.
pub fn write_file(path: &str, new_config: &SetupConfig) -> Result<()> {
    // Keys are emitted unquoted; shell identifiers prevent export-line
    // injection. The parser enforces the same rule on read.
    if let Some(bad) = new_config.keys().find(|k| !is_valid_key(k)) {
        anyhow::bail!("refusing to write config: invalid key {:?}", bad);
    }
    // Line-based parsing cannot safely round-trip newline values.
    if let Some((k, _)) = new_config.iter().find(|(_, v)| v.contains(['\n', '\r'])) {
        anyhow::bail!("refusing to write config: value for {:?} contains a newline", k);
    }
    // Treat a not-yet-created configuration as an empty template.
    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            return Err(e).with_context(|| format!("failed to open config file: {}", path))
        }
    };

    let mut seen = HashMap::new();
    let mut output = Vec::new();

    for line in content.lines() {
        if let Some((key, _)) = parse_export_line(line) {
            seen.insert(key.clone(), true);
            if let Some(val) = new_config.get(&key) {
                output.push(format!("export {}={}", key, quote(val)));
            } else {
                output.push(format!("#{}", line));
            }
        } else if let Some((key, _)) = parse_commented_export_line(line) {
            seen.insert(key.clone(), true);
            if let Some(val) = new_config.get(&key) {
                output.push(format!("export {}={}", key, quote(val)));
            } else {
                output.push(line.to_string());
            }
        } else {
            output.push(line.to_string());
        }
    }

    for (key, val) in new_config {
        if !seen.contains_key(key) {
            output.push(format!("export {}={}", key, quote(val)));
        }
    }

    // Use tmp + fsync + rename to survive abrupt vehicle power loss.
    let tmp = format!("{}.tmp", path);
    {
        let mut file = fs::File::create(&tmp)
            .with_context(|| format!("failed to write config tmp file: {}", tmp))?;
        {
            let mut writer = io::BufWriter::new(&mut file);
            for line in &output {
                writeln!(writer, "{}", line)?;
            }
            writer.flush()?;
        }
        // Flush the writer before syncing the file to persistent storage.
        let _ = file.sync_all();
    }
    if let Err(e) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(e).with_context(|| format!("failed to rename config tmp into place: {}", path));
    }

    Ok(())
}

fn parse_export_line(line: &str) -> Option<(String, String)> {
    let trimmed = line.trim_start();
    let rest = trimmed.strip_prefix("export ")?;
    parse_key_value(rest)
}

fn parse_commented_export_line(line: &str) -> Option<(String, String)> {
    let trimmed = line.trim_start();
    let rest = trimmed.strip_prefix('#')?;
    let rest = rest.trim_start();
    let rest = rest.strip_prefix("export ")?;
    parse_key_value(rest)
}

/// True when `key` is a plain shell identifier: `[A-Za-z_][A-Za-z0-9_]*`.
/// Single source of truth for key validity on both the read and write paths.
fn is_valid_key(key: &str) -> bool {
    let mut bytes = key.bytes();
    match bytes.next() {
        Some(first) if first.is_ascii_alphabetic() || first == b'_' => {}
        _ => return false,
    }
    bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

fn parse_key_value(s: &str) -> Option<(String, String)> {
    let eq_pos = s.find('=')?;
    let key = &s[..eq_pos];

    if !is_valid_key(key) {
        return None;
    }

    let val = unquote(&s[eq_pos + 1..]);
    Some((key.to_string(), val))
}

/// Strips surrounding quotes, `$'...'` wrapping, and trailing inline comments.
fn unquote(s: &str) -> String {
    let s = s.trim();
    if s.len() >= 2 {
        let bytes = s.as_bytes();
        if (bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\'')
            || (bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"')
        {
            return s[1..s.len() - 1].to_string();
        }
    }
    // bash ANSI-C quoting.
    if s.starts_with("$'") && s.ends_with('\'') && s.len() >= 3 {
        return s[2..s.len() - 1].to_string();
    }
    // Strip inline comments for unquoted values
    let bytes = s.as_bytes();
    for i in 1..bytes.len() {
        if bytes[i] == b'#' && (bytes[i - 1] == b' ' || bytes[i - 1] == b'\t') {
            return s[..i - 1].trim().to_string();
        }
    }
    s.to_string()
}

/// Quotes a value for safe bash export; bare when it has no special characters.
fn quote(s: &str) -> String {
    if s.is_empty() {
        return "''".to_string();
    }
    // Keep this safe independently of write_file's newline rejection.
    const SPECIAL: &str = " \t\n\r'\"\\$!#&|;(){}[]<>?*~`";
    if !s.chars().any(|c| SPECIAL.contains(c)) {
        return s.to_string();
    }
    let escaped = s.replace('\'', "'\\''");
    format!("'{}'", escaped)
}

/// Active values take precedence over commented-out ones.
pub fn get_config_value(active: &SetupConfig, commented: &SetupConfig, key: &str) -> Option<String> {
    active.get(key).or_else(|| commented.get(key)).cloned()
}

/// Source for migration and runtime-patch files, never release binaries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHubSource {
    /// `owner/Dash-USB`; `REPO` overrides only the owner for forks.
    pub repo_slug: String,
    /// Tracking ref for non-binary fetches. `BRANCH` in the active conf,
    /// defaulting to `main`. Invalid values fall back to `main`.
    pub branch: String,
    /// Whether a non-default branch was explicitly selected. Default devices
    /// must retain tag-matched support files.
    pub branch_explicit: bool,
}

const DEFAULT_GITHUB_OWNER: &str = "Sentry-Six";
const GITHUB_REPO_NAME: &str = "Dash-USB";
const DEFAULT_GITHUB_BRANCH: &str = "main";

/// Restrict config-derived URL and privileged-shell ref components.
fn valid_ref_component(s: &str, allow_slash: bool) -> bool {
    !s.is_empty()
        && s.len() <= 100
        && !s.starts_with(['-', '.', '/'])
        && !s.ends_with('/')
        && !s.contains("..")
        && s.chars().all(|c| {
            c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' || (allow_slash && c == '/')
        })
}

fn resolve_github_source(active: &SetupConfig) -> GitHubSource {
    let owner = active
        .get("REPO")
        .map(|s| s.trim())
        .filter(|s| valid_ref_component(s, false))
        .unwrap_or(DEFAULT_GITHUB_OWNER);
    let branch = active
        .get("BRANCH")
        .map(|s| s.trim())
        .filter(|s| valid_ref_component(s, true))
        .unwrap_or(DEFAULT_GITHUB_BRANCH);
    GitHubSource {
        repo_slug: format!("{}/{}", owner, GITHUB_REPO_NAME),
        branch: branch.to_string(),
        branch_explicit: branch != DEFAULT_GITHUB_BRANCH,
    }
}

/// Resolve from the active conf on disk. See [`GitHubSource`].
pub fn github_source() -> GitHubSource {
    let (active, _commented) = parse_file(find_config_path()).unwrap_or_default();
    resolve_github_source(&active)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_source_defaults() {
        let src = resolve_github_source(&SetupConfig::new());
        assert_eq!(src.repo_slug, "Sentry-Six/Dash-USB");
        assert_eq!(src.branch, "main");
        assert!(!src.branch_explicit);

        // An explicit "main" is not an override.
        let mut c = SetupConfig::new();
        c.insert("BRANCH".into(), "main".into());
        assert!(!resolve_github_source(&c).branch_explicit);
    }

    #[test]
    fn github_source_overrides() {
        let mut c = SetupConfig::new();
        c.insert("REPO".into(), "ForkOwner".into());
        c.insert("BRANCH".into(), "feature/gm-test".into());
        let src = resolve_github_source(&c);
        assert_eq!(src.repo_slug, "ForkOwner/Dash-USB");
        assert_eq!(src.branch, "feature/gm-test");
        assert!(src.branch_explicit);
    }

    #[test]
    fn github_source_rejects_bad_values() {
        for bad in ["", "  ", "-lead", "..", "a b", "x;rm -rf /", "$(id)", "a/../b", "/abs", "trail/"] {
            let mut c = SetupConfig::new();
            c.insert("BRANCH".into(), bad.into());
            c.insert("REPO".into(), bad.into());
            let src = resolve_github_source(&c);
            assert_eq!(src.branch, "main", "branch {:?} must fall back", bad);
            assert_eq!(src.repo_slug, "Sentry-Six/Dash-USB", "owner {:?} must fall back", bad);
        }
        // Branches may contain slashes; owners may not.
        let mut c = SetupConfig::new();
        c.insert("REPO".into(), "a/b".into());
        assert_eq!(resolve_github_source(&c).repo_slug, "Sentry-Six/Dash-USB");
    }

    #[test]
    fn test_unquote_single_quotes() {
        assert_eq!(unquote("'hello world'"), "hello world");
    }

    #[test]
    fn test_unquote_double_quotes() {
        assert_eq!(unquote("\"hello world\""), "hello world");
    }

    #[test]
    fn test_unquote_dollar_quotes() {
        assert_eq!(unquote("$'hello world'"), "hello world");
    }

    #[test]
    fn test_unquote_inline_comment() {
        assert_eq!(unquote("3480 # this number is in seconds"), "3480");
    }

    #[test]
    fn test_unquote_bare() {
        assert_eq!(unquote("hello"), "hello");
    }

    #[test]
    fn test_quote_empty() {
        assert_eq!(quote(""), "''");
    }

    #[test]
    fn test_quote_bare() {
        assert_eq!(quote("hello"), "hello");
    }

    #[test]
    fn test_quote_special() {
        assert_eq!(quote("hello world"), "'hello world'");
    }

    #[test]
    fn test_quote_embedded_single_quote() {
        assert_eq!(quote("it's"), "'it'\\''s'");
    }

    #[test]
    fn test_parse_export_line() {
        assert_eq!(
            parse_export_line("export WIFI_SSID='MyNetwork'"),
            Some(("WIFI_SSID".to_string(), "MyNetwork".to_string()))
        );
    }

    #[test]
    fn test_parse_commented_export_line() {
        assert_eq!(
            parse_commented_export_line("# export WIFI_SSID='MyNetwork'"),
            Some(("WIFI_SSID".to_string(), "MyNetwork".to_string()))
        );
    }

    #[test]
    fn test_parse_invalid_key() {
        assert_eq!(parse_export_line("export 123=bad"), None);
    }

    #[test]
    fn write_file_rejects_injection_key() {
        // A newline key could inject a second shell export.
        let dir = std::env::temp_dir().join(format!(
            "dashusb-cfg-inject-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("dashusb.conf");
        std::fs::write(&path, "export GOOD=1\n").unwrap();

        let mut cfg = SetupConfig::new();
        cfg.insert("EVIL\nexport WEB_PASSWORD".to_string(), "x".to_string());
        let r = write_file(path.to_str().unwrap(), &cfg);
        assert!(r.is_err(), "injection key must be rejected");
        // Rejected writes must leave the file untouched.
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(!after.contains("WEB_PASSWORD"), "config must be unchanged");

        let mut ok = SetupConfig::new();
        ok.insert("GOOD".to_string(), "2".to_string());
        assert!(write_file(path.to_str().unwrap(), &ok).is_ok());

        // Newline values cannot round-trip the line-based format.
        let mut nl = SetupConfig::new();
        nl.insert("NOTIFICATION_COMMAND_START".to_string(), "line1\nline2".to_string());
        assert!(
            write_file(path.to_str().unwrap(), &nl).is_err(),
            "newline value must be rejected"
        );
        let after2 = std::fs::read_to_string(&path).unwrap();
        assert!(!after2.contains("line2"), "config must be unchanged on rejected value");

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }
}
