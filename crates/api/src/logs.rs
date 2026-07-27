//! Log file viewer. Responses are raw `text/plain`; the frontend parses the
//! text itself, so never wrap it in JSON.

use axum::extract::{Path, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use std::io::{Read, Seek, SeekFrom};

use crate::router::AppState;

fn log_path(name: &str) -> Option<&'static str> {
    match name {
        "archiveloop" => Some("/mutable/archiveloop.log"),
        "setup" => Some("/dashusb/dashusb-setup.log"),
        "diagnostics" => Some("/tmp/diagnostics.txt"),
        "syslog" => Some("/var/log/syslog"),
        "kern" => Some("/var/log/kern.log"),
        "auth" => Some("/var/log/auth.log"),
        "daemon" => Some("/var/log/daemon.log"),
        "dashusb" => Some("/var/log/dashusb.log"),
        "dashusb-ble" => Some("/var/log/dashusb-ble.log"),
        _ => None,
    }
}

/// Cap on bytes returned. Prevents OOM on 512 MB Pi devices, where unrotated
/// syslog/kern can reach 200 MB.
const MAX_TAIL_BYTES: u64 = 512 * 1024;

pub async fn get_log(
    State(_s): State<AppState>,
    Path(name): Path<String>,
) -> Response {
    if name.contains("..") || name.contains('/') || name.contains('\\') {
        return (StatusCode::BAD_REQUEST, "invalid log name").into_response();
    }

    // Reads up to 512 KB off the SD card: keep it off the reactor so a slow
    // read can't stall the WebSocket heartbeat.
    tokio::task::spawn_blocking(move || read_log_tail(name))
        .await
        .unwrap_or_else(|_| {
            (StatusCode::INTERNAL_SERVER_ERROR, "log read task failed").into_response()
        })
}

fn read_log_tail(name: String) -> Response {
    let known = log_path(&name).is_some();
    let path = match log_path(&name) {
        Some(p) => p.to_string(),
        None => format!("/var/log/{}", name),
    };

    let mut file = match std::fs::File::open(&path) {
        Ok(f) => f,
        // Known logs may legitimately be absent (archiveloop.log before a NAS
        // is configured), so return an empty 200 and let the UI show "no log
        // output". Unknown names still 404.
        Err(_) if known => {
            return (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                String::new(),
            ).into_response();
        }
        Err(_) => return (StatusCode::NOT_FOUND, "Log file not found").into_response(),
    };

    let meta = match file.metadata() {
        Ok(m) => m,
        Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR, "Cannot stat log file").into_response(),
    };

    // After seeking, discard the first partial line so output starts on a
    // line boundary.
    if meta.len() > MAX_TAIL_BYTES {
        let _ = file.seek(SeekFrom::End(-(MAX_TAIL_BYTES as i64)));
        let mut one = [0u8; 1];
        loop {
            match file.read(&mut one) {
                Ok(1) if one[0] == b'\n' => break,
                Ok(1) => continue,
                _ => break,
            }
        }
    }

    let mut buf = String::new();
    if let Err(_) = file.read_to_string(&mut buf) {
        return (StatusCode::INTERNAL_SERVER_ERROR, "Failed to read log file").into_response();
    }

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        buf,
    )
        .into_response()
}
