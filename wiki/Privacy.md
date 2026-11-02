# Privacy

This page documents Dash USB connections to Sentry Six services. Configured
archive and notification providers, update checks/downloads, and explicit
support actions make additional user-directed connections.

## Summary

By default, Dash USB sends **no device identifier** to Sentry Six services.
The analytics choice in the setup wizard and in
`Settings → System → Analytics opt-in` is the only switch that controls
whether a device-derived identifier is sent with update checks.

## Per-flow disclosure

### 1. Update checks

- **Endpoint:** `POST https://api.sentry-six.com/dashusb/telemetry`
- **When:** When you check for updates in Settings, even if GitHub can't be
  reached.
- **Sent:** `current_version`, `arch`, `model` (board model),
  `update_available` flag, `new_version` (when relevant)
- **Identifier:** None by default. **If you have opted in** to analytics, a
  salted SHA-256 hash of your board's serial number (or `/etc/machine-id`
  when there is no serial) is included as `fingerprint`. The salt is public
  in the source code, so the ID is pseudonymous.
- **Purpose:** Detect vulnerable builds, ship compatible binaries,
  and (for opted-in devices) count unique installs without double-
  counting reinstalls.
- **Legal basis:** Legitimate interest under Art. 6(1)(f) for the
  default (no fingerprint) version — Recital 49 explicitly recognizes
  security as a legitimate-interest purpose. For the opted-in
  fingerprinted variant, consent under Art. 6(1)(a).
- **Stored:** For opted-in checks, the server keeps the device ID, version,
  architecture, board model and previous version.
- **Discord:** For opted-in checks, the version, architecture, board model
  and update status are posted to a private maintainers' Discord channel.
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
- **When:** At first startup, before the setup wizard, regardless of the
  analytics choice. Retried on later boots until it succeeds once (tracked
  by a `/mutable/.beaconed` marker).
- **Retention:** Daily counts are kept indefinitely as aggregate
  numbers. No per-user data exists to retain.
- **How to disable:** To suppress it entirely, create the marker before first
  boot: `sudo touch /mutable/.beaconed`. Network-block
  `api.sentry-six.com` if you want to be sure.

### 3. Automatic time-zone lookup

- **Endpoint:** `GET https://sentryusb.com/api/geoip/me`
- **When:** During setup and at boot while the time zone is `auto` and the
  system time zone is still UTC.
- **Sent:** No request body. The endpoint sees your public IP address and
  returns an approximate city, region and time zone; it rate-limits by IP for
  24 hours.
- **Purpose:** Resolve `TIME_ZONE=auto` to an IANA time zone.
- **How to disable:** Select an explicit time zone in the Setup Wizard.

### 4. AI Support page

- **Endpoint:** `https://api.sentry-six.com/ai-support/...`, proxied by the
  Pi.
- **Sent:** Your chat messages, the Dash USB version, board model, vehicle
  profile ID, archive method and snapshot interval. The Pi's public IP is used
  for abuse prevention.
- **Where it goes:** Sentry Six servers in Canada; Anthropic processes the
  chat to write each answer (Sentry Assistant).
- **Retention:** Redacted chats are kept for up to 90 days after the last
  message and may be reviewed by authorized maintainers. **New chat** deletes
  the current chat.
- **Diagnostics:** A diagnostics report is generated and uploaded only after
  you approve a request in the chat, and is deleted within 7 days.
- **How to disable:** Don't start a chat. Discord remains available for help
  from a person.

## Things Dash USB does **not** do

- Send a hardware fingerprint without explicit opt-in.
- Send diagnostics or crash reports in the background.
- Bundle multiple consents under one button. Each opt-in is a separate
  affirmative action.
- Use pre-ticked checkboxes — explicit click required.
- Upload footage. Recordings go only to the archive you configure.

## Source code references

- Update-check telemetry: `crates/api/src/update.rs` → `send_telemetry()`.
  The `fingerprint` key is inserted only when the preference is `true`.
- Install beacon: same file → `spawn_install_beacon()`. The POST is
  bodyless and gated on `/mutable/.beaconed`.
- AI Support proxy: `crates/api/src/support.rs`.
- Time-zone lookup: `crates/setup/src/system.rs` →
  `resolve_timezone_via_geoip()`.

## Reporting a privacy bug

Open an issue at
[github.com/Sentry-Six/Dash-USB/issues](https://github.com/Sentry-Six/Dash-USB/issues)
or email `privacy@sentry-six.com`. Include relevant packet-capture or journal
output without exposing secrets.
