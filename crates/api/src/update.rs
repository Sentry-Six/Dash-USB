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

/// Resolve the OTA repository; config can override the owner but not its name.
fn update_repo() -> String {
    sentryusb_config::github_source().repo_slug
}

/// Detect the running release suffix from the picker's active variant, live
/// CPU features, then userspace architecture. Prefer dpkg because a 64-bit
/// kernel may host an armhf userspace that cannot load aarch64 binaries.
async fn detect_release_suffix() -> anyhow::Result<String> {
    // Trust only release suffixes; older pickers could record fallback names.
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

    // armv7/amd64 need no CPU split; reject unsupported armv6 before download.
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

    // Mirror dashusb-pick-binary for pre-picker aarch64 installs.
    debug_assert_eq!(family, "aarch64");
    if let Ok(cpuinfo) = std::fs::read_to_string("/proc/cpuinfo") {
        // a76 also compiles AES/SHA, so require both LSE and AES hardware caps.
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

/// Stream a release to staging and broadcast advisory download progress.
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
    // CDN responses may omit Content-Length; the UI then uses indeterminate progress.
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

/// Stage and syntax-check a non-empty bash script without touching the live copy.
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

    // Install layouts differ; try each supported root remount form.
    hub.broadcast(
        "update_status",
        &serde_json::json!({"status": "remounting", "message": "Preparing filesystem…"}),
    );
    let _ = sentryusb_shell::run("/root/bin/remountfs_rw", &[]).await;
    let _ = sentryusb_shell::run("mount", &["-o", "remount,rw", "/"]).await;
    let _ = sentryusb_shell::run("mount", &["/", "-o", "remount,rw"]).await;

    // Stage beside the destination for an atomic rename across power loss; /tmp
    // is a different filesystem and limited RAM on small devices.
    sentryusb_shell::run("mkdir", &["-p", "/opt/dashusb"]).await?;
    let tmp = "/opt/dashusb/.dashusb-update.new";
    download_with_progress(hub, &url, tmp).await?;

    hub.broadcast(
        "update_status",
        &serde_json::json!({"status": "installing", "message": "Installing update…"}),
    );
    sentryusb_shell::run("chmod", &["+x", tmp]).await?;

    // Update the selected variant when the picker layout exists; otherwise
    // preserve the legacy path used by older systemd units.
    let dest = if std::path::Path::new("/opt/dashusb/dashusb-current").exists() {
        format!("/opt/dashusb/dashusb-{}", suffix)
    } else {
        "/opt/dashusb/dashusb".to_string()
    };
    sentryusb_shell::run("mv", &[tmp, &dest]).await?;

    // Report any component that failed to land.
    let mut install_warnings: Vec<String> = Vec::new();
    hub.broadcast(
        "update_status",
        &serde_json::json!({"status": "installing", "message": "Installing components…"}),
    );

    // Use the requested tag or resolve latest through the HTTP client; the
    // repository is configuration-controlled and must not enter shell input.
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

    // Refresh and run detection-gated hardware patches after each binary swap.
    // Pin support files to the installed tag unless an explicit BRANCH override
    // is configured, matching the post-reboot migration behavior.
    let source = sentryusb_config::github_source();
    hub.broadcast(
        "update_status",
        &serde_json::json!({"status": "updating_scripts", "message": "Updating scripts…"}),
    );
    let patches_path = "/usr/local/bin/dashusb-apply-runtime-patches";
    let patches_ref = if source.branch_explicit {
        source.branch.clone()
    } else if tag.trim().is_empty() {
        // Never let the helper silently default an empty ref to main.
        tracing::warn!(
            "update.rs: empty release tag; using {} for support files",
            source.branch
        );
        source.branch.clone()
    } else {
        tag.clone()
    };
    // Track the helper's actual source ref for its own payload downloads.
    let mut effective_ref = patches_ref.clone();
    let patches_url = format!(
        "https://raw.githubusercontent.com/{}/{}/setup/pi/apply-runtime-patches.sh",
        source.repo_slug, patches_ref
    );
    // Stage beside the destination to keep rename on one filesystem.
    let patches_tmp = "/usr/local/bin/.dashusb-apply-runtime-patches.new";
    tracing::info!(
        "update.rs: refreshing runtime-patches script from {}",
        patches_url
    );
    let mut staged_ok = stage_patches_script(&patches_url, patches_tmp).await;

    // Bootstrap from the branch only when a tag fetch fails and no helper exists.
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
            // Branch helper payloads must use the same branch.
            effective_ref = source.branch.clone();
        }
    }

    if staged_ok {
        // Set executable mode before replacing the working helper.
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
                    // A failed swap must remain visible to the user.
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
                // Use the helper's actual source after any branch fallback.
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
        // Keep full journal detail and cap the WebSocket summary at 4 KiB.
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
/// The response retains the simple legacy fields and the richer fields used by
/// `UpdateSection.tsx`. Omitting `update_available` or `latest_version` makes
/// the current UI report "up to date" regardless of the backend result.
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

/// Persistent marker for the once-per-install beacon.
const INSTALL_BEACON_MARKER: &str = "/mutable/.beaconed";

/// Send best-effort update-check telemetry. A device fingerprint is included
/// only when `analytics_opt_in` is true; the default payload has no identifier.
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

/// Send an empty, identifier-free install beacon once per persistent install.
pub fn spawn_install_beacon() {
    tokio::spawn(async move {
        if std::path::Path::new(INSTALL_BEACON_MARKER).exists() {
            return;
        }
        // Leave the marker absent after transient failures so the next boot retries.
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
