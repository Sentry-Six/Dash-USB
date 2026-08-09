//! OTA update: check for updates, run update, version info.

use std::collections::HashMap;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;

use crate::router::AppState;
use crate::status::get_sbc_model;

/// Written by `check_for_update`, read by `get_update_status`, so the Settings
/// page renders last-check results on load without a network round-trip.
const UPDATE_CHECK_CACHE: &str = "/tmp/dashusb-update-check.json";

static UPDATE_RUNNING: AtomicBool = AtomicBool::new(false);

/// Salt for the telemetry fingerprint hash. Changing it re-identifies every
/// device to the backend, so it must stay fixed.
const TELEMETRY_SALT: &str = "DASHUSB_2026_PROD";

/// SHA-256 of a stable hardware identifier plus the salt, cached. Prefers the
/// SBC serial number, which survives a reflash, then falls back to machine-id.
pub(crate) fn get_fingerprint() -> &'static str {
    static CACHED: OnceLock<String> = OnceLock::new();
    CACHED.get_or_init(|| {
        use ring::digest::{SHA256, digest};
        let mut id = String::new();
        for p in [
            "/sys/firmware/devicetree/base/serial-number",
            "/proc/device-tree/serial-number",
        ] {
            if let Ok(raw) = std::fs::read_to_string(p) {
                let trimmed = raw.trim_matches(|c: char| c == '\0' || c.is_whitespace());
                if !trimmed.is_empty() {
                    id = trimmed.to_string();
                    break;
                }
            }
        }
        if id.is_empty() {
            for p in ["/etc/machine-id", "/var/lib/dbus/machine-id"] {
                if let Ok(raw) = std::fs::read_to_string(p) {
                    let trimmed = raw.trim();
                    if !trimmed.is_empty() {
                        id = trimmed.to_string();
                        break;
                    }
                }
            }
        }
        if id.is_empty() {
            tracing::warn!("[telemetry] no fingerprint source available");
            return String::new();
        }
        let h = digest(&SHA256, format!("{}{}", id, TELEMETRY_SALT).as_bytes());
        hex::encode(h.as_ref())
    })
    .as_str()
}

pub async fn check_internet(State(_s): State<AppState>) -> (StatusCode, Json<serde_json::Value>) {
    use futures_util::future::select_ok;
    use std::time::Duration;
    use tokio::net::TcpStream;

    // Port 443 works on Pi-hole networks, which block port 53 for non-Pi-hole
    // DNS. Race two probes so the first success wins.
    let t = Duration::from_secs(2);
    let probes: Vec<std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<()>> + Send>>> = vec![
        Box::pin(async move {
            tokio::time::timeout(t, TcpStream::connect("8.8.8.8:443")).await
                .map_err(|_| anyhow::anyhow!("timeout"))?.map_err(anyhow::Error::from)?;
            Ok(())
        }),
        Box::pin(async move {
            tokio::time::timeout(t, TcpStream::connect("1.1.1.1:443")).await
                .map_err(|_| anyhow::anyhow!("timeout"))?.map_err(anyhow::Error::from)?;
            Ok(())
        }),
    ];
    let connected = select_ok(probes).await.is_ok();
    (StatusCode::OK, Json(serde_json::json!({"connected": connected})))
}

/// Body (optional): `{"version": "vX.Y.Z"}` installs a specific release. An
/// empty body or a missing version installs whatever `/releases/latest` points
/// to.
///
/// On success the daemon broadcasts `complete`, then `restarting`, then shells
/// out to `reboot` about 3 s later, so the new binary is running by the time
/// the user's tab reconnects. That 3 s gap is what lets the client mount the
/// restart modal before the WebSocket goes away.
pub async fn run_update(
    State(s): State<AppState>,
    body: String,
) -> (StatusCode, Json<serde_json::Value>) {
    if UPDATE_RUNNING.swap(true, Ordering::SeqCst) {
        return crate::json_error(StatusCode::CONFLICT, "Update already in progress");
    }

    // The frontend only attaches a body when targetVersion is set, so an empty
    // string is the "install latest" case.
    let target_version: Option<String> = if body.trim().is_empty() {
        None
    } else {
        serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| v.get("version").and_then(|s| s.as_str()).map(String::from))
            .filter(|s| !s.is_empty())
    };

    let hub = s.hub.clone();
    tokio::spawn(async move {
        hub.broadcast("update_status", &serde_json::json!({"status": "running"}));

        let result = self_update(&hub, target_version).await;

        UPDATE_RUNNING.store(false, Ordering::SeqCst);

        match result {
            Ok(msg) => {
                hub.broadcast("update_status", &serde_json::json!({
                    "status": "complete",
                    "output": msg
                }));

                // Let the completion message land before announcing the
                // restart.
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                hub.broadcast("update_status", &serde_json::json!({
                    "status": "restarting",
                    "message": "Restarting Pi to apply update…"
                }));
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;

                let _ = sentryusb_shell::run("reboot", &[]).await;
            }
            Err(e) => hub.broadcast("update_status", &serde_json::json!({
                "status": "error",
                "error": e.to_string()
            })),
        }
    });

    (StatusCode::OK, Json(serde_json::json!({"status": "started"})))
}

/// Resolve the `owner/repo` slug for OTA updates. `REPO` in the active
/// dashusb.conf overrides the owner, so a fork can point self-update at its own
/// releases from the wizard's Advanced Update Source field. The repo name
/// stays hardcoded: a fork must keep the original repo name. Release binaries
/// are always selected by tag; the configured BRANCH never enters these URLs.
fn update_repo() -> String {
    sentryusb_config::github_source().repo_slug
}

/// Detect the release suffix matching the currently-running CPU variant.
///
/// Three-tier resolution:
///   1. `/opt/dashusb/active-variant`, written by the boot picker
///      (dashusb-pick-binary). Authoritative when present: it names the
///      variant running right now, so re-downloading that suffix guarantees
///      the picker picks the update again.
///   2. Live CPU detection mirroring the picker's rules (HWCAP atomics gives
///      a76, CPU part 0xD08 gives a72, else a53). Used before the picker has
///      written active-variant, such as the first migration update off a
///      single-binary install.
///   3. Architecture-family fallback via dpkg/uname for armv7 and amd64, which
///      have no per-CPU variants.
///
/// On Pi OS a 64-bit kernel can be paired with a 32-bit (armhf) userspace,
/// where `uname -m` reports `aarch64` but the aarch64 binary cannot load: exec
/// returns ENOENT because `/lib/ld-linux-aarch64.so.1` isn't installed. Trust
/// dpkg first when determining the architecture family.
async fn detect_release_suffix() -> anyhow::Result<String> {
    // Tier 1: ask the picker what it chose at boot, but only trust values that
    // are real release suffixes. Old picker versions recorded whatever they
    // ended up RUNNING (an on-disk fallback, sometimes "legacy"), and a
    // download URL built from that either 404s or permanently installs the
    // wrong CPU variant (issue #88's second act). Anything else falls through
    // to live detection below.
    const KNOWN_SUFFIXES: &[&str] = &[
        "linux-arm64-a53",
        "linux-arm64-a72",
        "linux-arm64-a76",
        "linux-armv7",
        "linux-amd64",
    ];
    if let Ok(s) = std::fs::read_to_string("/opt/dashusb/active-variant") {
        let trimmed = s.trim();
        if KNOWN_SUFFIXES.contains(&trimmed) {
            return Ok(trimmed.to_string());
        }
        if !trimmed.is_empty() {
            tracing::warn!(
                "active-variant contains {:?} (not a release suffix) — \
                 ignoring it and re-detecting from CPU",
                trimmed
            );
        }
    }

    // Tier 3 runs first as a cheap arch-family check, gating whether per-CPU
    // detection is needed at all: armv7 and amd64 have one variant each. armv6
    // (armel, Pi Zero W, Pi 1) is unsupported and errors out here, so the user
    // gets a diagnosable failure instead of a 404 on the download.
    let family = if let Ok(out) = sentryusb_shell::run("dpkg", &["--print-architecture"]).await {
        match out.trim() {
            "arm64" => "aarch64",
            "armhf" => return Ok("linux-armv7".to_string()),
            "armel" => anyhow::bail!(
                "armv6 (armel / Pi Zero W / Pi 1) is no longer supported — \
                 DashUSB requires Pi Zero 2 W or newer"
            ),
            "amd64" => return Ok("linux-amd64".to_string()),
            other => anyhow::bail!("unsupported userspace architecture: {}", other),
        }
    } else {
        let arch = sentryusb_shell::run("uname", &["-m"]).await?;
        match arch.trim() {
            "aarch64" => "aarch64",
            "armv7l" => return Ok("linux-armv7".to_string()),
            "armv6l" => anyhow::bail!(
                "armv6 (Pi Zero W / Pi 1) is no longer supported — \
                 DashUSB requires Pi Zero 2 W or newer"
            ),
            "x86_64" => return Ok("linux-amd64".to_string()),
            other => anyhow::bail!("unsupported architecture: {}", other),
        }
    };

    // Tier 2: aarch64 per-CPU detection, mirroring dashusb-pick-binary's rules
    // so detection on a pre-picker install lands on the same variant the picker
    // would have chosen.
    debug_assert_eq!(family, "aarch64");
    if let Ok(cpuinfo) = std::fs::read_to_string("/proc/cpuinfo") {
        // HWCAP_ATOMICS = LSE = ARMv8.1+ = Cortex-A76 and newer. The a76 build
        // also keeps the ARMv8 crypto extension enabled (Pi 5 has it), so the
        // `aes` hwcap is required too: a v8.1+ board without crypto MUST get
        // the a72 build or it SIGILLs in SHA/AES.
        let has_hwcap = |cap: &str| {
            cpuinfo.lines().any(|line| {
                line.starts_with("Features")
                    && line.split_whitespace().any(|w| w == cap)
            })
        };
        if has_hwcap("atomics") && has_hwcap("aes") {
            return Ok("linux-arm64-a76".to_string());
        }
        // 0xD08 = Cortex-A72 (Pi 4 / RK3399 perf cluster).
        for line in cpuinfo.lines() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("CPU part") {
                let part = trimmed.split(':').nth(1).unwrap_or("").trim().to_ascii_lowercase();
                if part == "0xd08" {
                    return Ok("linux-arm64-a72".to_string());
                }
            }
        }
    }
    // Default for aarch64: Cortex-A53 (Pi 3, Pi Zero 2 W, Allwinner H618).
    Ok("linux-arm64-a53".to_string())
}

/// Stream a release binary to `dest`, broadcasting `downloading` progress
/// (percent when Content-Length is known, byte counts either way) over the
/// WS hub. Progress is advisory only — the caller still owns the atomic
/// stage-then-rename install, so a failure here never touches the live
/// binary.
async fn download_with_progress(
    hub: &sentryusb_ws::Hub,
    url: &str,
    dest: &str,
) -> anyhow::Result<()> {
    use futures_util::StreamExt;
    use tokio::io::AsyncWriteExt;

    let resp = crate::http_client()
        .get(url)
        .header("User-Agent", "dashusb-updater")
        .send()
        .await?
        .error_for_status()?;
    // GitHub redirects releases to a CDN; reqwest follows them, but the
    // final response can omit Content-Length — percent goes null and the
    // UI falls back to an indeterminate bar.
    let total = resp.content_length().filter(|t| *t > 0);
    let mut file = tokio::fs::File::create(dest).await?;
    let mut stream = resp.bytes_stream();
    let mut done: u64 = 0;
    let mut last_percent: i64 = -1;
    let mut last_emit = std::time::Instant::now();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        file.write_all(&chunk).await?;
        done += chunk.len() as u64;
        let percent = total.map(|t| ((done as f64 / t as f64) * 100.0) as i64);
        let emit = match percent {
            Some(p) => p != last_percent,
            None => last_emit.elapsed() >= std::time::Duration::from_millis(500),
        };
        if emit {
            if let Some(p) = percent {
                last_percent = p;
            }
            last_emit = std::time::Instant::now();
            hub.broadcast(
                "update_status",
                &serde_json::json!({
                    "status": "downloading",
                    "message": "Downloading update…",
                    "percent": percent,
                    "bytes_done": done,
                    "bytes_total": total,
                }),
            );
        }
    }
    file.flush().await?;
    Ok(())
}

/// Download `url` to `dest` and validate it as a runnable bash script.
///
/// False (with the staged file removed) if the download failed, the body was
/// empty — GitHub occasionally answers 200 with nothing — or the script does
/// not parse. Never touches the live script; the caller owns that swap.
async fn stage_patches_script(url: &str, dest: &str) -> bool {
    if sentryusb_shell::run_with_timeout(
        std::time::Duration::from_secs(20),
        "curl",
        &["-fsSL", "--max-time", "15", "-o", dest, url],
    )
    .await
    .is_err()
    {
        let _ = std::fs::remove_file(dest);
        return false;
    }
    let ok = std::fs::metadata(dest).map(|m| m.len() > 0).unwrap_or(false)
        && sentryusb_shell::run("bash", &["-n", dest]).await.is_ok();
    if !ok {
        let _ = std::fs::remove_file(dest);
    }
    ok
}

async fn self_update(
    hub: &sentryusb_ws::Hub,
    target_version: Option<String>,
) -> anyhow::Result<String> {
    hub.broadcast(
        "update_status",
        &serde_json::json!({"status": "checking", "message": "Checking release…"}),
    );
    let suffix = detect_release_suffix().await?;
    let repo = update_repo();

    // Tag-specific URL when a target version was requested (Revert to Stable,
    // Install Pre-release), otherwise the latest release.
    let url = if let Some(v) = &target_version {
        format!(
            "https://github.com/{}/releases/download/{}/dashusb-{}",
            repo, v, suffix
        )
    } else {
        format!(
            "https://github.com/{}/releases/latest/download/dashusb-{}",
            repo, suffix
        )
    };

    // HEAD-check the binary exists before downloading so a typo'd version or
    // a release that didn't get a binary uploaded surfaces as a clear error
    // instead of an empty mv'd file.
    sentryusb_shell::run_with_timeout(
        std::time::Duration::from_secs(15),
        "curl",
        &["-sfI", "--max-time", "10", &url],
    )
    .await
    .map_err(|_| {
        anyhow::anyhow!(
            "No release binary found at {}. Publish a release with the binary first.",
            url
        )
    })?;

    // Remount root read-write. These images mount `/` read-only with the
    // writable portion behind an overlay or a `remountfs_rw` helper, and the
    // layout varies per install, so all three forms are attempted and any one
    // succeeding is enough. If none does, every downstream `mv` into /root/bin
    // fails silently and the UI reports a new version while the old binary is
    // still on disk (the Rock Pi 4C+ "updated to v3.3.1 but still running
    // v3.3.0" failure).
    hub.broadcast(
        "update_status",
        &serde_json::json!({"status": "remounting", "message": "Preparing filesystem…"}),
    );
    let _ = sentryusb_shell::run("/root/bin/remountfs_rw", &[]).await;
    let _ = sentryusb_shell::run("mount", &["-o", "remount,rw", "/"]).await;
    let _ = sentryusb_shell::run("mount", &["/", "-o", "remount,rw"]).await;

    // Stage the download on the SAME filesystem as the destination so the `mv`
    // below is an atomic rename(2). Staging in /tmp (tmpfs) makes mv a
    // cross-filesystem unlink-dest plus copy, and a power cut mid-copy, routine
    // when the car cuts accessory power, leaves a partial or missing binary at
    // /opt/dashusb and a service that can't start on the next boot. Staged
    // here, a power cut only orphans the hidden .new file and the running
    // binary is untouched until the rename. The ~15 MB binary also stays out of
    // tmpfs RAM on a 1 GB device.
    sentryusb_shell::run("mkdir", &["-p", "/opt/dashusb"]).await?;
    let tmp = "/opt/dashusb/.dashusb-update.new";
    download_with_progress(hub, &url, tmp).await?;

    hub.broadcast(
        "update_status",
        &serde_json::json!({"status": "installing", "message": "Installing update…"}),
    );
    sentryusb_shell::run("chmod", &["+x", tmp]).await?;

    // Write to the per-variant path so the picker symlink keeps resolving to a
    // valid binary. Layout:
    //   /opt/dashusb/dashusb-{suffix}            <- written here
    //   /opt/dashusb/dashusb-current -> above    <- picker symlink
    //   /opt/dashusb/dashusb         -> -current <- back-compat symlink
    //
    // An existing /opt/dashusb/dashusb-current means the new layout, so use the
    // variant path. Otherwise this is a pre-multi-binary install and the legacy
    // /opt/dashusb/dashusb path is what the systemd unit looks for. The next
    // install-pi.sh run migrates the layout.
    let dest = if std::path::Path::new("/opt/dashusb/dashusb-current").exists() {
        format!("/opt/dashusb/dashusb-{}", suffix)
    } else {
        "/opt/dashusb/dashusb".to_string()
    };
    sentryusb_shell::run("mv", &[tmp, &dest]).await?;

    // Track install-step outcomes so the response says exactly what landed. A
    // read-only /root/bin must never report success while the old binary is
    // still on disk.
    let mut install_warnings: Vec<String> = Vec::new();
    hub.broadcast(
        "update_status",
        &serde_json::json!({"status": "installing", "message": "Installing components…"}),
    );

    // Record the tag: the requested target if there was one, since it matches
    // the binary just installed, else resolve /latest. Go through the shared
    // HTTP client rather than a shell pipeline, because the repo name is
    // config-controlled and must never be interpolated into a shell string.
    let tag = match target_version {
        Some(v) => v,
        None => {
            let api_url = format!(
                "https://api.github.com/repos/{}/releases/latest",
                repo
            );
            match crate::http_client()
                .get(&api_url)
                .header("User-Agent", "dashusb-updater")
                .timeout(std::time::Duration::from_secs(10))
                .send()
                .await
            {
                Ok(resp) => resp
                    .json::<serde_json::Value>()
                    .await
                    .ok()
                    .and_then(|v| {
                        v.get("tag_name")
                            .and_then(|t| t.as_str())
                            .map(|s| s.trim().to_string())
                    })
                    .unwrap_or_default(),
                Err(_) => String::new(),
            }
        }
    };

    if !tag.is_empty() {
        let _ = std::fs::write("/opt/dashusb/version", &tag);
    }

    // Re-apply install-time patches that MUST survive an OTA binary swap. The
    // standalone /usr/local/bin/dashusb-apply-runtime-patches script owns fixes
    // the binary can't, such as the BCM4345C0 non-fatal-adv patch to
    // /root/bin/dashusb-ble.py on Rock 4C+, without which the BLE daemon
    // crash-loops after every update. The script is idempotent and
    // detection-gated, so it no-ops on other boards and on already-patched
    // files.
    //
    // ALWAYS refresh the script body from the repo before running it.
    // Downloading only when absent leaves anyone with a stale on-disk copy
    // running the old script forever, so new patches never reach them. A failed
    // download falls back to whatever is on disk, warning only. The script
    // The helper is pinned to the RELEASE TAG being installed, not to a
    // branch tip, so the patch script always matches the binary it ships
    // with. An explicit BRANCH in dashusb.conf is a deliberate developer
    // override and still wins — this mirrors migrate.rs, so the pre-reboot
    // update and the post-reboot migration agree on the ref instead of one
    // silently overwriting the other's helper.
    let source = sentryusb_config::github_source();
    hub.broadcast(
        "update_status",
        &serde_json::json!({"status": "updating_scripts", "message": "Updating scripts…"}),
    );
    let patches_path = "/usr/local/bin/dashusb-apply-runtime-patches";
    let patches_ref = if source.branch_explicit {
        source.branch.clone()
    } else if tag.trim().is_empty() {
        // Never pass an empty ref onward: the helper's own
        // `${DASHUSB_REF:-main}` would silently resolve to main, which is the
        // branch drift this pinning exists to stop.
        tracing::warn!(
            "update.rs: empty release tag; using {} for support files",
            source.branch
        );
        source.branch.clone()
    } else {
        tag.clone()
    };
    // The ref the helper ACTUALLY came from. Reassigned if the branch
    // fallback below wins, so the script is never run with a ref that
    // disagrees with its own source — a branch helper executed with
    // DASHUSB_REF=<tag> would fetch tag payloads into branch logic.
    let mut effective_ref = patches_ref.clone();
    let patches_url = format!(
        "https://raw.githubusercontent.com/{}/{}/setup/pi/apply-runtime-patches.sh",
        source.repo_slug, patches_ref
    );
    // Staged beside the destination so the final rename cannot cross
    // filesystems (/tmp is a tmpfs; rename() would fail EXDEV).
    let patches_tmp = "/usr/local/bin/.dashusb-apply-runtime-patches.new";
    tracing::info!(
        "update.rs: refreshing runtime-patches script from {}",
        patches_url
    );
    let mut staged_ok = stage_patches_script(&patches_url, patches_tmp).await;

    // Tag fetch failed — an unknown or not-yet-pushed tag, or a hand-rolled
    // build. Bootstrap from the branch ONLY when there is no helper at all:
    // a device with a working helper must keep it rather than silently
    // jumping to the branch tip, which is the drift this pinning prevents.
    if !staged_ok
        && patches_ref != source.branch
        && !std::path::Path::new(patches_path).exists()
    {
        let fallback_url = format!(
            "https://raw.githubusercontent.com/{}/{}/setup/pi/apply-runtime-patches.sh",
            source.repo_slug, source.branch
        );
        tracing::warn!(
            "update.rs: tag-pinned patches fetch failed and no helper on disk; bootstrapping from {}",
            fallback_url
        );
        staged_ok = stage_patches_script(&fallback_url, patches_tmp).await;
        if staged_ok {
            // The helper is now the BRANCH copy, so it must run with the
            // branch ref. Leaving the tag here produced a mixed-source state:
            // branch helper, tag payloads.
            effective_ref = source.branch.clone();
        }
    }

    if staged_ok {
        // chmod the STAGED file, before it becomes live: a successful rename
        // followed by a failed chmod would leave a non-executable helper
        // installed, and the old code discarded the chmod result entirely.
        if sentryusb_shell::run("chmod", &["+x", patches_tmp]).await.is_err() {
            let _ = std::fs::remove_file(patches_tmp);
            install_warnings.push(
                "runtime-patches helper could not be made executable; keeping the existing \
                 script. Fixes added in this release may not apply."
                    .to_string(),
            );
        } else {
            match std::fs::rename(patches_tmp, patches_path) {
                Ok(()) => tracing::info!("update.rs: runtime-patches script refreshed"),
                Err(e) => {
                    // Never let a failed swap pass as success: the whole point
                    // of the refresh is that NEW patches reach existing installs.
                    let _ = std::fs::remove_file(patches_tmp);
                    tracing::error!(
                        "update.rs: runtime-patches swap FAILED ({e}); keeping existing script"
                    );
                    install_warnings.push(format!(
                        "runtime-patches script could not be replaced ({e}): this device will \
                         re-run its EXISTING patch script, so fixes added in this release may \
                         not apply. Re-run install-pi.sh manually."
                    ));
                }
            }
        }
    } else if !std::path::Path::new(patches_path).exists() {
        install_warnings.push(
            "runtime-patches download failed AND no existing script: board-specific fixes \
             (BCM4345C0 BLE on Rock 4C+, EATT disable, etc.) won't auto-reapply after this \
             update. Re-run install-pi.sh manually if BLE pairing breaks."
                .to_string(),
        );
    } else {
        tracing::warn!(
            "update.rs: runtime-patches refresh failed, falling back to existing on-disk script"
        );
    }

    if std::path::Path::new(patches_path).exists() {
        match sentryusb_shell::run_with_timeout(
            std::time::Duration::from_secs(30),
            "env",
            &[
                &format!("DASHUSB_REPO_SLUG={}", source.repo_slug),
                // The ref the helper on disk actually came from, which is not
                // necessarily patches_ref once the branch fallback has run.
                &format!("DASHUSB_REF={}", effective_ref),
                patches_path,
            ],
        )
        .await
        {
            Ok(_) => tracing::info!("update.rs: runtime-patches re-applied successfully"),
            Err(e) => install_warnings.push(format!(
                "runtime-patches re-apply FAILED: {e} — board-specific fixes \
                 (BCM4345C0 BLE on Rock 4C+, etc.) may not survive this update; \
                 if BLE pairing is broken after this update, re-run install-pi.sh"
            )),
        }
    }

    if install_warnings.is_empty() {
        Ok(format!(
            "Updated to {}.",
            if tag.is_empty() { "latest".to_string() } else { tag }
        ))
    } else {
        // Full detail goes to the journal for ops; the UI gets a condensed
        // version, capped at 4 kB so a flood of warnings can't blow up the
        // WebSocket message.
        for w in &install_warnings {
            tracing::warn!("update.rs: {}", w);
        }
        let joined = install_warnings.join("\n  • ");
        let mut msg = format!(
            "Updated to {} — but with warnings:\n  • {}",
            if tag.is_empty() {
                "latest".to_string()
            } else {
                tag.clone()
            },
            joined
        );
        if msg.len() > 4096 {
            msg.truncate(4093);
            msg.push_str("...");
        }
        Ok(msg)
    }
}

/// Kernel boot identifier — changes on every boot. Read per request (cheap)
/// so it can never go stale. None on non-Linux hosts; the UI treats a null
/// boot_id as "reboot unverified", never as proof of one.
fn read_boot_id() -> Option<String> {
    std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

pub async fn get_version(State(_s): State<AppState>) -> (StatusCode, Json<serde_json::Value>) {
    let version = env!("CARGO_PKG_VERSION");
    let sbc_model = get_sbc_model();

    // Read installed version tag if available (installer writes it here).
    let installed = std::fs::read_to_string("/opt/dashusb/version")
        .or_else(|_| std::fs::read_to_string("/root/.dashusb_version"))
        .unwrap_or_else(|_| version.to_string());

    (StatusCode::OK, Json(serde_json::json!({
        "version": installed.trim(),
        "binary_version": version,
        "sbc_model": sbc_model,
        // The version tag alone can't prove a reboot: the old daemon
        // rewrites /opt/dashusb/version BEFORE `reboot` fires and keeps
        // serving it. The updater UI compares boot_id instead.
        "boot_id": read_boot_id(),
    })))
}

/// Parse a semver tag ("v1.2.3", "v1.2.3-beta.1") into
/// (major, minor, patch, prerelease).
pub(crate) fn parse_semver(v: &str) -> Option<(u32, u32, u32, String)> {
    let v = v.trim().trim_start_matches('v');
    let (base, pre) = match v.find('-') {
        Some(i) => (&v[..i], v[i + 1..].to_string()),
        None => (v, String::new()),
    };
    let parts: Vec<&str> = base.split('.').collect();
    if parts.len() < 3 {
        return None;
    }
    let mut nums = [0u32; 3];
    for (i, p) in parts.iter().take(3).enumerate() {
        if p.is_empty() || !p.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        nums[i] = p.parse().ok()?;
    }
    Some((nums[0], nums[1], nums[2], pre))
}

/// True if `candidate` is newer than `current`. Prerelease-aware:
/// stable beats prerelease at the same base version.
pub(crate) fn is_version_newer(candidate: &str, current: &str) -> bool {
    let c = parse_semver(candidate);
    let u = parse_semver(current);
    let (c, u) = match (c, u) {
        (Some(c), Some(u)) => (c, u),
        _ => return candidate.trim() != current.trim(),
    };
    if c.0 != u.0 {
        return c.0 > u.0;
    }
    if c.1 != u.1 {
        return c.1 > u.1;
    }
    if c.2 != u.2 {
        return c.2 > u.2;
    }
    match (u.3.is_empty(), c.3.is_empty()) {
        (true, true) => false,
        (false, true) => true,   // user on prerelease, candidate stable → newer
        (true, false) => false,  // user on stable, candidate prerelease → older
        (false, false) => c.3 > u.3,
    }
}

fn read_current_version() -> String {
    std::fs::read_to_string("/opt/dashusb/version")
        .or_else(|_| std::fs::read_to_string("/root/.dashusb_version"))
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| env!("CARGO_PKG_VERSION").to_string())
}

/// Fetch and parse GitHub's release JSON. Transport and HTTP errors MUST
/// surface: a silent failure reports `available: false` and tells the user they
/// are up to date when they are not.
///
/// The response carries both the simple fields (`available`, `latest`,
/// `current`) that older clients read AND the richer ones the current web UI
/// reads (`update_available`, `latest_version`, `release_url`,
/// `release_notes`). Settings.tsx only looks at `update_available` and
/// `latest_version`; without them the UI defaults to "up to date" no matter
/// what the backend found.
pub async fn check_for_update(
    State(_s): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> (StatusCode, Json<serde_json::Value>) {
    let current = read_current_version();
    let can_update = !current.is_empty() && current != "dev";

    // Include prereleases if requested via query param OR if the user's
    // update_channel preference is set to "prerelease".
    let mut include_prerelease = params.get("include_prerelease").map(String::as_str) == Some("true");
    if !include_prerelease {
        let prefs = crate::preferences::load_prefs();
        if prefs.get("update_channel").and_then(|v| v.as_str()) == Some("prerelease") {
            include_prerelease = true;
        }
    }

    let releases = match fetch_releases().await {
        Ok(rs) => rs,
        Err(msg) => {
            // Fire a basic telemetry heartbeat so the support server still sees
            // the device when GitHub is unreachable.
            let cur_clone = current.clone();
            tokio::spawn(async move { send_telemetry(&cur_clone, false, "").await });

            return (
                StatusCode::OK,
                Json(serde_json::json!({
                    "available": false,
                    "update_available": false,
                    "error": msg,
                })),
            );
        }
    };

    let (latest_stable, latest_prerelease) = find_latest_releases(&releases);

    let mut result = serde_json::json!({
        "current_version": current,
        "checked_at": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    });

    let mut new_stable_version = String::new();

    // Being on a prerelease means the latest stable can be offered as a
    // downgrade when no forward upgrade exists.
    let on_prerelease = parse_semver(&current)
        .map(|(_, _, _, pre)| !pre.is_empty())
        .unwrap_or(false);

    if let Some(stable) = latest_stable {
        let stable_available = can_update && is_version_newer(&stable.tag_name, &current);
        result["update_available"] = serde_json::Value::Bool(stable_available);
        result["latest_version"] = serde_json::Value::String(stable.tag_name.clone());
        result["release_url"] = serde_json::Value::String(stable.html_url.clone());
        result["release_notes"] = serde_json::Value::String(stable.body.clone());
        result["stable"] = serde_json::json!({
            "version": stable.tag_name,
            "release_url": stable.html_url,
            "release_notes": stable.body,
            "available": stable_available,
        });
        if stable_available {
            new_stable_version = stable.tag_name.clone();
        }

        // On a prerelease whose base version outranks the latest stable, the
        // stable release is offered as a revert instead.
        if on_prerelease && can_update && !stable_available {
            result["revert_stable"] = serde_json::json!({
                "version": stable.tag_name,
                "release_url": stable.html_url,
                "release_notes": stable.body,
            });
        }
    } else {
        result["update_available"] = serde_json::Value::Bool(false);
    }

    if include_prerelease {
        if let Some(pre) = latest_prerelease {
            let pre_available = can_update && is_version_newer(&pre.tag_name, &current);
            result["prerelease"] = serde_json::json!({
                "version": pre.tag_name,
                "release_url": pre.html_url,
                "release_notes": pre.body,
                "available": pre_available,
            });
        }
    }

    // Cache the result so the Settings page load can render last-check info
    // without re-hitting GitHub.
    if let Ok(data) = serde_json::to_vec(&result) {
        let _ = std::fs::write(UPDATE_CHECK_CACHE, data);
    }

    // Telemetry reports stable updates only, never prereleases.
    let cur_clone = current.clone();
    let new_ver_clone = new_stable_version.clone();
    tokio::spawn(async move {
        send_telemetry(&cur_clone, !new_ver_clone.is_empty(), &new_ver_clone).await;
    });

    (StatusCode::OK, Json(result))
}

#[derive(Clone)]
struct ReleaseInfo {
    tag_name: String,
    html_url: String,
    body: String,
    prerelease: bool,
    draft: bool,
}

async fn fetch_releases() -> Result<Vec<ReleaseInfo>, String> {
    let url = format!("https://api.github.com/repos/{}/releases?per_page=20", update_repo());

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .user_agent(concat!("dashusb-updater/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| format!("http client init failed: {}", e))?;

    let resp = client.get(&url).send().await.map_err(|e| {
        if e.is_timeout() {
            "GitHub API request timed out".to_string()
        } else if e.is_connect() {
            format!("could not reach GitHub: {}", e)
        } else {
            format!("GitHub API request failed: {}", e)
        }
    })?;

    let status = resp.status();
    if !status.is_success() {
        return Err(if status.as_u16() == 403 || status.as_u16() == 429 {
            "GitHub API rate limit hit — wait about an hour and try again".to_string()
        } else {
            format!("GitHub API returned HTTP {}", status)
        });
    }

    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("GitHub API returned unparseable JSON: {}", e))?;

    let arr = body
        .as_array()
        .ok_or_else(|| "GitHub API response was not an array".to_string())?;

    Ok(arr
        .iter()
        .map(|v| ReleaseInfo {
            tag_name: v.get("tag_name").and_then(|s| s.as_str()).unwrap_or("").to_string(),
            html_url: v.get("html_url").and_then(|s| s.as_str()).unwrap_or("").to_string(),
            body: v.get("body").and_then(|s| s.as_str()).unwrap_or("").to_string(),
            prerelease: v.get("prerelease").and_then(|s| s.as_bool()).unwrap_or(false),
            draft: v.get("draft").and_then(|s| s.as_bool()).unwrap_or(false),
        })
        .filter(|r| !r.tag_name.is_empty())
        .collect())
}

/// First stable and first prerelease in the list, assuming the GitHub API
/// returns releases publish-newest-first. Drafts are skipped.
fn find_latest_releases(releases: &[ReleaseInfo]) -> (Option<&ReleaseInfo>, Option<&ReleaseInfo>) {
    let mut stable: Option<&ReleaseInfo> = None;
    let mut prerelease: Option<&ReleaseInfo> = None;
    for r in releases {
        if r.draft {
            continue;
        }
        if r.prerelease {
            if prerelease.is_none() {
                prerelease = Some(r);
            }
        } else if stable.is_none() {
            stable = Some(r);
        }
        if stable.is_some() && prerelease.is_some() {
            break;
        }
    }
    (stable, prerelease)
}

/// Once this exists the install beacon has fired and never fires again. It
/// lives under `/mutable/` so it survives updates but resets on a full SD-card
/// reflash, which is indistinguishable from a fresh install anyway.
const INSTALL_BEACON_MARKER: &str = "/mutable/.beaconed";

/// POST update-check telemetry to the support server. The payload always
/// carries `{current_version, update_available, new_version, arch, model}`.
///
/// A device fingerprint is included ONLY when the user has explicitly opted in
/// via the `analytics_opt_in` preference, set by the setup wizard or Settings.
/// That is the GDPR Art. 6(1)(a) consent gate: without an opt-in the backend
/// treats the call as an opted-out heartbeat, with no DB row and IP rate
/// limiting.
///
/// Best-effort: errors are logged, never surfaced to the caller.
pub async fn send_telemetry(current: &str, update_available: bool, new_version: &str) {
    let opt_in = crate::preferences::load_prefs()
        .get("analytics_opt_in")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let arch = sentryusb_shell::run("uname", &["-m"])
        .await
        .ok()
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| std::env::consts::ARCH.to_string());

    let mut payload = serde_json::json!({
        "current_version": current,
        "update_available": update_available,
        "new_version": new_version,
        "arch": arch,
        "model": get_sbc_model(),
    });

    if opt_in {
        let fp = get_fingerprint();
        if !fp.is_empty() {
            payload["fingerprint"] = serde_json::Value::String(fp.to_string());
        }
    }

    let url = "https://api.sentry-six.com/dashusb/telemetry";
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
    {
        Ok(c) => c,
        Err(_) => return,
    };
    match client.post(url).json(&payload).send().await {
        Ok(r) => tracing::info!(
            "[telemetry] sent (status {}, mode={})",
            r.status(),
            if opt_in { "opt-in" } else { "opted-out" }
        ),
        Err(e) => tracing::warn!("[telemetry] failed: {}", e),
    }
}

/// Fire the anonymous install beacon exactly once per install. It POSTs an
/// EMPTY body to `/dashusb/install-beacon`: no fingerprint, no identifier. The
/// backend only increments a daily counter, which measures gross install volume
/// independent of the opt-in cohort and carries no personal data.
///
/// `/mutable/.beaconed` guards it: once that file exists the beacon never fires
/// again for this install, until /mutable is wiped by a full reflash.
pub fn spawn_install_beacon() {
    tokio::spawn(async move {
        if std::path::Path::new(INSTALL_BEACON_MARKER).exists() {
            return;
        }
        // Retry transient errors so a cold DNS cache at first boot doesn't
        // drop the beacon. Three attempts, then give up and stay un-beaconed
        // until the next boot tries again.
        let url = "https://api.sentry-six.com/dashusb/install-beacon";
        let client = match reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
        {
            Ok(c) => c,
            Err(_) => return,
        };
        for attempt in 1..=3 {
            match client.post(url).send().await {
                Ok(r) if r.status().is_success() => {
                    let _ = std::fs::write(INSTALL_BEACON_MARKER, b"1");
                    tracing::info!("[beacon] install counted");
                    return;
                }
                Ok(r) => {
                    tracing::warn!("[beacon] non-success status {}", r.status());
                    // 4xx won't fix with retry; 5xx might.
                    if !r.status().is_server_error() {
                        return;
                    }
                }
                Err(e) => {
                    tracing::warn!("[beacon] attempt {} failed: {}", attempt, e);
                }
            }
            if attempt < 3 {
                tokio::time::sleep(std::time::Duration::from_secs(5 * attempt)).await;
            }
        }
    });
}

/// The cached result of the last `check_for_update`, so the Settings page can
/// render last-known release info without a fresh GitHub round-trip on every
/// page load.
///
/// Live install progress arrives on the `update_status` WebSocket channel (see
/// `run_update`), not here.
pub async fn get_update_status(State(_s): State<AppState>) -> (StatusCode, Json<serde_json::Value>) {
    match std::fs::read_to_string(UPDATE_CHECK_CACHE) {
        Ok(s) => match serde_json::from_str::<serde_json::Value>(&s) {
            Ok(v) => (StatusCode::OK, Json(v)),
            Err(_) => (
                StatusCode::OK,
                Json(serde_json::json!({"update_available": false})),
            ),
        },
        Err(_) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "update_available": false,
                "checked_at": "",
            })),
        ),
    }
}
