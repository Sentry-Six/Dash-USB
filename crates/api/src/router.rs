use axum::extract::DefaultBodyLimit;
use axum::routing::{delete, get, post};
use axum::Router;

use crate::auth::AuthState;
use crate::status::NetSampler;

/// Shared application state available to all handlers.
#[derive(Clone)]
pub struct AppState {
    pub hub: sentryusb_ws::Hub,
    pub auth: AuthState,
    pub net_sampler: NetSampler,
}

/// Build the complete Axum router with all API routes.
pub fn build_router(state: AppState) -> Router {
    let api = Router::new()
        // Status & config
        .route("/api/status", get(crate::status::get_status))
        .route("/api/profile", get(crate::profile::get_profile))
        .route("/api/status/storage", get(crate::status::get_storage_breakdown))
        .route("/api/config", get(crate::status::get_config))
        .route("/api/wifi", get(crate::status::get_wifi_config))
        // Auth
        .route("/api/auth/login", post(crate::auth::handle_login))
        .route("/api/auth/logout", post(crate::auth::handle_logout))
        .route("/api/auth/check", get(crate::auth::handle_auth_check))
        // Setup
        .route("/api/setup/status", get(crate::setup::get_setup_status))
        .route("/api/setup/config", get(crate::setup::get_setup_config).put(crate::setup::save_setup_config))
        .route("/api/setup/run", post(crate::setup::run_setup))
        .route("/api/setup/phases", get(crate::setup::get_setup_phases))
        .route("/api/setup/test-archive", post(crate::setup::test_archive))
        .route("/api/setup/preflight", post(crate::setup::preflight))
        // Snapshot management — list / delete archived dashcam
        // snapshots, plus a free-space query for the UI's gauge.
        .route("/api/snapshots", get(crate::snapshots::list_snapshots))
        .route("/api/snapshots/{id}", delete(crate::snapshots::delete_snapshot))
        .route("/api/backingfiles/free-space", get(crate::snapshots::get_free_space))
        // Archive progress (archiveloop writes /tmp/archive_status.json)
        .route("/api/archive/status", get(crate::archive_state::get_archive_status))
        // Clips
        .route("/api/clips", get(crate::clips::get_clips))
        // Files
        .route("/api/files/ls", get(crate::files::list_files))
        .route("/api/files/mkdir", post(crate::files::create_dir))
        .route("/api/files/mv", post(crate::files::move_file))
        .route("/api/files/cp", post(crate::files::copy_file))
        .route("/api/files", delete(crate::files::delete_file))
        .route("/api/files/upload", post(crate::files::upload_file)
            .layer(DefaultBodyLimit::max(512 * 1024 * 1024)))
        .route("/api/files/download", get(crate::files::download_file))
        .route("/api/files/download-zip", get(crate::files::download_zip))
        .route("/api/files/download-zip-multi", post(crate::files::download_zip_multi))
        // Logs
        .route("/api/logs/{name}", get(crate::logs::get_log))
        // Diagnostics & health
        .route("/api/diagnostics/refresh", post(crate::healthcheck::refresh_diagnostics))
        .route("/api/diagnostics", get(crate::healthcheck::get_diagnostics))
        .route("/api/system/health-check", get(crate::healthcheck::health_check))
        // System
        .route("/api/system/reboot", post(crate::system::reboot))
        .route("/api/system/shutdown", post(crate::system::shutdown))
        .route("/api/system/toggle-drives", post(crate::system::toggle_drives))
        .route("/api/system/gadget-enable", post(crate::system::gadget_enable))
        .route("/api/system/gadget-disable", post(crate::system::gadget_disable))
        .route("/api/system/trigger-sync", post(crate::system::trigger_sync))
        // Phone-app (Dash Connect) GATT pairing reset — unrelated to any
        // vehicle BLE; clears app bonds/PIN and restarts the peripheral.
        .route("/api/system/ble-reset-pair", post(crate::system::ble_reset_pair))
        .route("/api/system/speedtest", get(crate::system::speedtest))
        .route("/api/system/rtc-status", get(crate::system::get_rtc_status))
        .route("/api/system/clock-status", get(crate::system::get_clock_status))
        .route("/api/system/ssh-pubkey", get(crate::system::get_ssh_pubkey))
        .route("/api/system/ssh-keygen", post(crate::system::generate_ssh_key))
        .route("/api/system/check-internet", get(crate::update::check_internet))
        .route("/api/system/update", post(crate::update::run_update))
        .route("/api/system/version", get(crate::update::get_version))
        .route("/api/system/check-update", post(crate::update::check_for_update))
        .route("/api/system/update-status", get(crate::update::get_update_status))
        .route("/api/system/block-devices", get(crate::devices::list_block_devices))
        // Storage repair — guided XFS backingfiles recovery (see storage_repair.rs)
        .route("/api/storage/health", get(crate::storage_repair::storage_health))
        .route("/api/storage/repair", post(crate::storage_repair::storage_repair))
        // Preferences
        .route("/api/config/preference", get(crate::preferences::get_preference).put(crate::preferences::set_preference))
        // Notifications
        .route("/api/notifications/generate-code", post(crate::notifications::generate_pairing_code))
        .route("/api/notifications/paired-devices", get(crate::notifications::list_paired_devices))
        .route("/api/notifications/paired-devices/{id}", delete(crate::notifications::remove_paired_device))
        .route("/api/notifications/test", post(crate::notifications::send_test_notification))
        .route("/api/notifications/send", post(crate::notifications::send_notification))
        .route("/api/notifications/settings", get(crate::notification_center::get_settings).put(crate::notification_center::update_settings))
        .route("/api/notifications/history", get(crate::notification_center::get_history).post(crate::notification_center::append_history).delete(crate::notification_center::clear_history))
        .route("/api/notifications/history/{id}", delete(crate::notification_center::delete_history_item))
        .route("/api/notifications/settings/check", get(crate::notification_center::check_notification_type))
        // Support
        .route("/api/support/check", get(crate::support::check_available))
        .route("/api/support/ticket", post(crate::support::create_ticket))
        .route("/api/support/ticket/{id}/message", post(crate::support::send_message))
        .route("/api/support/ticket/{id}/media", post(crate::support::upload_media))
        .route("/api/support/ticket/{id}/messages", get(crate::support::fetch_messages))
        .route("/api/support/ticket/{id}/close", post(crate::support::close_ticket))
        .route("/api/support/ticket/{id}/mark-read", post(crate::support::mark_read))
        .route("/api/support/ticket/{id}/register-device", post(crate::support::register_device))
        .route("/api/support/ticket/{id}/unregister-device", post(crate::support::unregister_device))
        // Memory debug
        .route("/api/memory", get(crate::memory::memory_stats))
        // Backup
        .route("/api/system/backup", post(crate::backup::create_backup))
        .route("/api/system/backups", get(crate::backup::list_backups))
        .route("/api/system/backup/{date}", get(crate::backup::get_backup))
        .route("/api/system/restore", post(crate::backup::restore_backup))
        // Terminal WebSocket
        .route("/api/terminal", get(crate::terminal::handle_terminal))
        // WebSocket
        .route("/api/ws", get(ws_handler))
        // Memory HTML page
        .route("/memory", get(crate::memory::memory_page));

    api.with_state(state)
}

/// WebSocket handler.
async fn ws_handler(
    ws: axum::extract::WebSocketUpgrade,
    axum::extract::State(state): axum::extract::State<AppState>,
) -> impl axum::response::IntoResponse {
    ws.on_upgrade(move |socket| handle_ws(socket, state.hub))
}

async fn handle_ws(socket: axum::extract::ws::WebSocket, hub: sentryusb_ws::Hub) {
    use axum::extract::ws::Message;
    use tokio::time::{interval, Duration};

    let mut rx = hub.subscribe();

    let (mut sender, mut receiver) = socket.split();
    use futures_util::{SinkExt, StreamExt};

    let hub_clone = hub.clone();

    // Writer task: forward broadcasts + periodic pings
    let mut send_task = tokio::spawn(async move {
        let mut ping_interval = interval(Duration::from_secs(30));
        loop {
            tokio::select! {
                msg = rx.recv() => {
                    match msg {
                        Ok(data) => {
                            if sender.send(Message::Text(String::from_utf8_lossy(&data).into_owned().into())).await.is_err() {
                                break;
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(_) => break,
                    }
                }
                _ = ping_interval.tick() => {
                    let ping_msg = serde_json::json!({"type": "ping", "data": null});
                    if sender.send(Message::Text(ping_msg.to_string().into())).await.is_err() {
                        break;
                    }
                }
            }
        }
    });

    // Reader task: any message (including pong) resets the 60-second read
    // deadline. Two missed pings (30s each) tears down the socket — matches
    // Go's `SetReadDeadline(60s)` in hub.go and means a JS tab that's been
    // paused by the browser stops holding a server-side goroutine within a
    // minute, instead of however long it takes the TCP send buffer to fill.
    let mut recv_task = tokio::spawn(async move {
        loop {
            match tokio::time::timeout(Duration::from_secs(60), receiver.next()).await {
                Ok(Some(Ok(_))) => continue,   // message/pong → deadline resets
                Ok(Some(Err(_))) | Ok(None) => break, // socket error / closed
                Err(_) => break,               // deadline — tear down
            }
        }
    });

    // Wait for either task to finish
    tokio::select! {
        _ = &mut send_task => { recv_task.abort(); }
        _ = &mut recv_task => { send_task.abort(); }
    }

    hub_clone.client_disconnected();
}
