# Privacy

This page documents Dash USB connections to Sentry Six services. Configured
archive and notification providers, update checks/downloads, and explicit
support actions make additional user-directed connections.

## Summary

By default, Dash USB sends **no device identifier** to Sentry Six services.
The "opt-in for analytics" toggle in the setup wizard and in
`Settings → System → Analytics opt-in` is the only switch that controls
whether a device-derived identifier ever leaves your Pi.

## Per-flow disclosure

### 1. Update checks

- **Endpoint:** `POST https://api.sentry-six.com/dashusb/telemetry`
- **Sent:** `current_version`, `arch`, `model`, `update_available` flag,
  `new_version` (when relevant)
- **Identifier:** None by default. **If you have opted in** to the
  analytics toggle, a one-way salted SHA-256 of your board's serial
  number is included as `fingerprint`.
- **Purpose:** Detect vulnerable builds, ship compatible binaries,
  and (for opted-in devices) count unique installs without double-
  counting reinstalls.
- **Legal basis:** Legitimate interest under Art. 6(1)(f) for the
  default (no fingerprint) version — Recital 49 explicitly recognizes
  security as a legitimate-interest purpose. For the opted-in
  fingerprinted variant, consent under Art. 6(1)(a).
- **Retention:** Opted-in rows remain until manually purged or deletion is
  requested. Non-fingerprinted calls create no database row; the application
  keeps only short-lived in-memory rate-limit counters.
- **Identifier control:** `Settings → System → Analytics opt-in → Opted out`
  stops the fingerprint on future update checks but does not delete an existing
  server row.

### 2. Anonymous install beacon

- **Endpoint:** `POST https://api.sentry-six.com/dashusb/install-beacon`
- **Sent:** **Nothing.** Empty body, no headers beyond standard HTTP.
- **Identifier:** None. The server only increments a daily counter.
- **Purpose:** Count aggregate installs without identifying devices.
- **Network metadata:** The source IP is used by an in-memory rate limiter but
  is not stored by this endpoint.
- **Retention:** Daily counts are kept indefinitely as aggregate
  numbers. No per-user data exists to retain.
- **How to disable:** Fires exactly once per install (gated by a
  `/mutable/.beaconed` marker). To suppress entirely, create that file
  before first boot: `sudo touch /mutable/.beaconed`. Network-block
  `api.sentry-six.com` if you want to be sure.

### 3. Mobile push notifications (opt-in)

- **Endpoint:** `https://notifications.sentry-six.com/*`
- **Sent:** Pairing sends a random `device_id`, `device_secret`, pairing code,
  and hostname. Notifications send that ID and secret plus the title, message,
  category, and archive progress when applicable.
- **Identifier:** `device_id` is generated locally from random bytes and is not
  derived from hardware.
- **Purpose:** Routing push notifications from your Pi to your phone.
- **Legal basis:** Consent — you actively enabled this feature.
- **Retention:** Pairings and APNS tokens remain until unpaired or invalidated.
  The device registration (`device_id`, secret, and hostname) remains while the
  Pi is active and is deleted on request.
- **How to disable:** Don't pair, or remove paired devices under
  `Settings → Notifications` to stop delivery. Request registration deletion
  through `privacy@sentry-six.com`.

### 4. Automatic time-zone lookup

- **Endpoint:** `GET https://sentry-six.com/api/geoip/me`
- **Sent:** No request body; the service necessarily receives the source IP.
- **Purpose:** Resolve `TIME_ZONE=auto` to an IANA time zone.
- **How to disable:** Select an explicit time zone in the Setup Wizard.

## Things Dash USB does **not** do

- Send a hardware fingerprint without explicit opt-in.
- Send diagnostics or crash reports in the background.
- Bundle multiple consents under one button. Each opt-in is a separate
  affirmative action.
- Use pre-ticked checkboxes — explicit click required.
- Upload footage automatically. Recordings go to the configured archive unless
  you explicitly attach media to a support ticket.

## Source code references

- Update-check telemetry: `crates/api/src/update.rs` → `send_telemetry()`.
  The `fingerprint` key is inserted only when the preference is `true`.
- Install beacon: same file → `spawn_install_beacon()`. The POST is
  bodyless and gated on `/mutable/.beaconed`.
- Notification pairing: `crates/api/src/notifications.rs` →
  `register_code_with_backend()`; delivery is in
  `crates/notify/src/sentry_connect.rs` and `run/send-live-activity`.
- Time-zone lookup: `crates/setup/src/system.rs` →
  `resolve_timezone_via_geoip()`.

## Reporting a privacy bug

Open an issue at
[github.com/Sentry-Six/Dash-USB/issues](https://github.com/Sentry-Six/Dash-USB/issues)
or email `privacy@sentry-six.com`. Include relevant packet-capture or journal
output without exposing secrets.
