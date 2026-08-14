//! Log file viewer. Responses are raw `text/plain`; the frontend parses the
//! text itself, so never wrap it in JSON.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
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

    // Keep the bounded SD-card read off async workers.
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
        // Known but not-yet-created logs are empty; unknown names remain 404.
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

// Paged reads

/// Default and maximum lines per page. 50 keeps the first paint cheap on a
/// slow transport, where the 512 KiB tail above cannot finish inside the
/// client's deadline.
const DEFAULT_PAGE_LINES: usize = 50;
const MAX_PAGE_LINES: usize = 2000;

#[derive(Deserialize)]
pub struct LogPageQuery {
    lines: Option<usize>,
    /// Byte offset returned by a previous page. Reads the lines immediately
    /// before it; absent means start at the end of the file.
    before: Option<u64>,
}

/// Cursor-based JSON log paging for constrained transports; the existing raw
/// text endpoint remains unchanged.
pub async fn get_log_page(
    State(_s): State<AppState>,
    Path(name): Path<String>,
    Query(q): Query<LogPageQuery>,
) -> Response {
    if name.contains("..") || name.contains('/') || name.contains('\\') {
        return page_error(StatusCode::BAD_REQUEST, "invalid log name");
    }

    let lines = q.lines.unwrap_or(DEFAULT_PAGE_LINES).clamp(1, MAX_PAGE_LINES);
    let before = q.before;

    tokio::task::spawn_blocking(move || read_log_page(name, lines, before))
        .await
        .unwrap_or_else(|_| {
            page_error(StatusCode::INTERNAL_SERVER_ERROR, "log read task failed")
        })
}

fn page_error(status: StatusCode, msg: &str) -> Response {
    crate::json_error(status, msg).into_response()
}

fn page_response(content: String, start: u64) -> Response {
    let has_more = start > 0;
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "content": content,
            // Feed back as `before` to walk further into the past.
            "before": if has_more { Some(start) } else { None },
            "has_more": has_more,
        })),
    )
        .into_response()
}

fn read_log_page(name: String, lines: usize, before: Option<u64>) -> Response {
    let known = log_path(&name).is_some();
    let path = match log_path(&name) {
        Some(p) => p.to_string(),
        None => format!("/var/log/{}", name),
    };

    let mut file = match std::fs::File::open(&path) {
        Ok(f) => f,
        // Known but not-yet-created logs return an empty page.
        Err(_) if known => return page_response(String::new(), 0),
        Err(_) => return page_error(StatusCode::NOT_FOUND, "Log file not found"),
    };

    match page_window(&mut file, lines, before) {
        Ok((content, start)) => page_response(content, start),
        Err(PageError::StaleCursor) => page_error(
            StatusCode::CONFLICT,
            "log rotated since that page; reload from the newest page",
        ),
        Err(PageError::Io) => {
            page_error(StatusCode::INTERNAL_SERVER_ERROR, "Failed to read log file")
        }
    }
}

#[derive(Debug)]
enum PageError {
    /// `before` points past EOF — the log rotated or was truncated.
    StaleCursor,
    Io,
}

/// Returns the last `lines` lines ending at `before` (default EOF), plus the
/// byte offset the window starts at. Split out from the handler so the
/// backwards line walk is unit-testable.
fn page_window(
    file: &mut std::fs::File,
    lines: usize,
    before: Option<u64>,
) -> Result<(String, u64), PageError> {
    let len = file.metadata().map_err(|_| PageError::Io)?.len();

    let end = before.unwrap_or(len);
    if end > len {
        return Err(PageError::StaleCursor);
    }
    if end == 0 {
        return Ok((String::new(), 0));
    }

    // A trailing newline terminates rather than creates a line.
    let mut scan_end = end;
    let mut one = [0u8; 1];
    if file.seek(SeekFrom::Start(end - 1)).is_ok()
        && file.read_exact(&mut one).is_ok()
        && one[0] == b'\n'
    {
        scan_end -= 1;
    }

    const CHUNK: u64 = 8 * 1024;
    let mut buf = vec![0u8; CHUNK as usize];
    let mut start = scan_end;
    let mut found = 0usize;

    'outer: while start > 0 && end - start < MAX_TAIL_BYTES {
        let step = CHUNK.min(start);
        let pos = start - step;
        if file.seek(SeekFrom::Start(pos)).is_err() {
            break;
        }
        let slice = &mut buf[..step as usize];
        if file.read_exact(slice).is_err() {
            break;
        }
        for i in (0..step as usize).rev() {
            if slice[i] == b'\n' {
                found += 1;
                if found == lines {
                    start = pos + i as u64 + 1;
                    break 'outer;
                }
            }
        }
        start = pos;
    }

    // Return a bounded fragment when one line exceeds the byte cap.
    if end - start > MAX_TAIL_BYTES {
        start = end - MAX_TAIL_BYTES;
    }

    let take = (end - start) as usize;
    let mut out = vec![0u8; take];
    file.seek(SeekFrom::Start(start)).map_err(|_| PageError::Io)?;
    file.read_exact(&mut out).map_err(|_| PageError::Io)?;

    Ok((String::from_utf8_lossy(&out).into_owned(), start))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn fixture(body: &str) -> std::fs::File {
        let mut f = tempfile::tempfile().unwrap();
        f.write_all(body.as_bytes()).unwrap();
        f
    }

    #[test]
    fn returns_last_n_lines_with_trailing_newline() {
        let mut f = fixture("a\nb\nc\nd\n");
        let (content, start) = page_window(&mut f, 2, None).unwrap();
        assert_eq!(content, "c\nd\n");
        assert_eq!(start, 4);
    }

    #[test]
    fn returns_last_n_lines_without_trailing_newline() {
        let mut f = fixture("a\nb\nc\nd");
        let (content, start) = page_window(&mut f, 2, None).unwrap();
        assert_eq!(content, "c\nd");
        assert_eq!(start, 4);
    }

    #[test]
    fn walks_backwards_through_pages_to_the_start() {
        let mut f = fixture("a\nb\nc\nd\n");
        let (_, first) = page_window(&mut f, 2, None).unwrap();
        let (content, start) = page_window(&mut f, 2, Some(first)).unwrap();
        assert_eq!(content, "a\nb\n");
        assert_eq!(start, 0);
    }

    #[test]
    fn asking_for_more_lines_than_exist_returns_the_whole_file() {
        let mut f = fixture("a\nb\n");
        let (content, start) = page_window(&mut f, 500, None).unwrap();
        assert_eq!(content, "a\nb\n");
        assert_eq!(start, 0);
    }

    #[test]
    fn empty_file_is_an_empty_page() {
        let mut f = fixture("");
        let (content, start) = page_window(&mut f, 50, None).unwrap();
        assert!(content.is_empty());
        assert_eq!(start, 0);
    }

    #[test]
    fn spans_the_chunk_boundary() {
        // Forces the backwards walk across more than one 8 KiB read.
        let body: String = (0..4000).map(|i| format!("line {i}\n")).collect();
        let mut f = fixture(&body);
        let (content, _) = page_window(&mut f, 3, None).unwrap();
        assert_eq!(content, "line 3997\nline 3998\nline 3999\n");
    }

    #[test]
    fn cursor_past_eof_is_rejected() {
        let mut f = fixture("a\nb\n");
        assert!(matches!(
            page_window(&mut f, 2, Some(9_999)),
            Err(PageError::StaleCursor)
        ));
    }
}
