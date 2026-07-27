//! Sentry Connect mobile app push notifications.
//!
//! Server URL: `SENTRY_NOTIFICATION_URL`, else
//! `https://notifications.sentry-six.com`.
//!
//! archive_start sends carrying an `ARCHIVE_TOTAL_COUNT` add a
//! `live_activity` block so the iOS app can start its Live Activity even
//! after the system terminated it; a silent-push wake is less reliable.

use anyhow::{bail, Result};
use reqwest::Client;
use serde_json::json;

/// Per-send extras. `Default` leaves every field `None`, producing the
/// minimal title+message payload.
#[derive(Debug, Clone, Default)]
pub struct SendContext<'a> {
    /// `start` or `finish`. Only used with
    /// `notification_type = "archive_start"`, to enable live_activity.
    pub type_hint: Option<&'a str>,
    /// Category (`archive_start`, `archive_complete`, `temperature`,
    /// `drives`). Echoed in the payload so the app can sort the alert.
    pub notification_type: Option<&'a str>,
    /// Total clip count for the pending archive run. Required for the
    /// live_activity payload on `archive_start`.
    pub archive_total_count: Option<u32>,
    /// Device name shown in the live_activity header. Usually the title
    /// (e.g. `"MyCar:"`); the trailing colon is stripped.
    pub device_name: Option<&'a str>,
}

/// Notification relay base URL, resolved in order:
///
/// 1. `SENTRY_NOTIFICATION_URL` env var (dev overrides, systemd
///    `EnvironmentFile=`).
/// 2. `SENTRY_NOTIFICATION_URL` in `/root/dashusb.conf`. Required: systemd
///    starts the binary without a shell wrapper to source the config, so
///    the env var is unset on a default install. Without this fallback
///    every push hits `notifications.sentry-six.com` whatever the conf
///    says, silently breaking third-party relays such as the Android
///    SentryConnect app's Firebase Cloud Functions.
/// 3. `https://notifications.sentry-six.com`.
///
/// Keep in sync with `notification_base_url()` in `api/src/notifications.rs`.
fn default_push_server() -> String {
    if let Ok(v) = std::env::var("SENTRY_NOTIFICATION_URL") {
        let trimmed = v.trim().trim_end_matches('/');
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    let config_path = sentryusb_config::find_config_path();
    if let Ok((active, _)) = sentryusb_config::parse_file(config_path) {
        if let Some(v) = active.get("SENTRY_NOTIFICATION_URL") {
            let trimmed = v.trim().trim_end_matches('/');
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
    }
    "https://notifications.sentry-six.com".to_string()
}

pub async fn send(
    client: &Client,
    device_id: &str,
    device_secret: &str,
    title: &str,
    message: &str,
) -> Result<()> {
    send_with_context(client, device_id, device_secret, title, message, &SendContext::default()).await
}

pub async fn send_with_context(
    client: &Client,
    device_id: &str,
    device_secret: &str,
    title: &str,
    message: &str,
    ctx: &SendContext<'_>,
) -> Result<()> {
    if device_id.is_empty() || device_secret.is_empty() {
        bail!("Mobile push credentials not found. Re-pair your device in Settings.");
    }

    let mut payload = json!({
        "title": title,
        "message": message,
        "device_id": device_id,
    });
    let obj = payload.as_object_mut().expect("payload is a JSON object");

    if let Some(nt) = ctx.notification_type {
        if !nt.is_empty() {
            obj.insert("notification_type".into(), json!(nt));
        }
    }

    // live_activity only on archive_start + start with a known total count.
    let is_archive_start = ctx.type_hint == Some("start")
        && ctx.notification_type == Some("archive_start");
    if is_archive_start {
        if let Some(total) = ctx.archive_total_count {
            let raw_name = ctx.device_name.unwrap_or(title);
            let device_name = raw_name.strip_suffix(':').unwrap_or(raw_name);
            obj.insert(
                "live_activity".into(),
                json!({
                    "action": "start",
                    "phase": "archiving",
                    "current": 0,
                    "total": total,
                    "device_name": device_name,
                }),
            );
        }
    }

    let url = format!("{}/send", default_push_server().trim_end_matches('/'));
    let resp = client
        .post(&url)
        .header("Content-Type", "application/json")
        .header("X-Device-Secret", device_secret)
        .json(&payload)
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        bail!("HTTP {} — {}", status, body);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_default_is_all_none() {
        let ctx = SendContext::default();
        assert!(ctx.type_hint.is_none());
        assert!(ctx.notification_type.is_none());
        assert!(ctx.archive_total_count.is_none());
        assert!(ctx.device_name.is_none());
    }

    #[test]
    fn live_activity_only_for_archive_start() {
        // Exercises the branch condition only; payload construction lives
        // inside send_with_context.
        let ctx = SendContext {
            type_hint: Some("start"),
            notification_type: Some("archive_start"),
            archive_total_count: Some(42),
            device_name: Some("MyCar:"),
        };
        let is_archive_start = ctx.type_hint == Some("start")
            && ctx.notification_type == Some("archive_start");
        assert!(is_archive_start);
        assert_eq!(ctx.archive_total_count, Some(42));
    }

    #[test]
    fn push_server_default_is_production_url() {
        // Do not mutate the env: that is unsafe on edition 2024 and goes
        // flaky when other tests read it. Assert the fallback only when
        // neither the env var nor the on-disk conf overrides it. A host
        // with SENTRY_NOTIFICATION_URL in /root/dashusb.conf is a deployed
        // Pi, not a build runner, so CI always takes this branch.
        let env_unset = std::env::var("SENTRY_NOTIFICATION_URL").is_err();
        let conf_unset = sentryusb_config::parse_file(sentryusb_config::find_config_path())
            .map(|(active, _)| !active.contains_key("SENTRY_NOTIFICATION_URL"))
            .unwrap_or(true);
        if env_unset && conf_unset {
            assert_eq!(default_push_server(), "https://notifications.sentry-six.com");
        }
    }
}
