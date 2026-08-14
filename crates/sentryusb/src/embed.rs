use std::fmt::Write;

use axum::http::{HeaderMap, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use rust_embed::{Embed, EmbeddedFile};

#[derive(Embed)]
#[folder = "static/"]
struct StaticFiles;

/// MIME table for the SPA's closed set of embedded file types.
fn mime_for(path: &str) -> &'static str {
    // Content-Encoding describes compression; preserve the original MIME.
    let stem = path
        .strip_suffix(".br")
        .or_else(|| path.strip_suffix(".gz"))
        .unwrap_or(path);
    let ext = stem.rsplit('.').next().unwrap_or("");
    match ext {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" => "application/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json; charset=utf-8",
        "svg" => "image/svg+xml",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "ttf" => "font/ttf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "gif" => "image/gif",
        "txt" => "text/plain; charset=utf-8",
        "wasm" => "application/wasm",
        _ => "application/octet-stream",
    }
}

/// Serve embedded files with pre-compression and SPA fallback. Vite's hashed
/// assets are immutable; entry files revalidate. Encoding-specific ETags keep
/// compressed and identity responses distinct.
pub async fn spa_handler(uri: Uri, headers: HeaderMap) -> Response {
    let path = uri.path().trim_start_matches('/');

    if let Some((file, encoding)) = pick_encoding(path, &headers) {
        return serve_embedded(path, file, encoding, &headers);
    }

    if let Some(file) = StaticFiles::get(path) {
        return serve_embedded(path, file, None, &headers);
    }

    // Let the compression layer handle the short, release-specific entry file.
    match StaticFiles::get("index.html") {
        Some(file) => serve_embedded("index.html", file, None, &headers),
        None => (StatusCode::NOT_FOUND, "Not Found").into_response(),
    }
}

/// Returns (file, content-encoding) for an accepted pre-compressed sibling.
/// Brotli takes precedence over gzip.
fn pick_encoding(path: &str, req_headers: &HeaderMap) -> Option<(EmbeddedFile, Option<&'static str>)> {
    let accept = req_headers
        .get(header::ACCEPT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    if accept.contains("br") {
        if let Some(f) = StaticFiles::get(&format!("{path}.br")) {
            return Some((f, Some("br")));
        }
    }
    if accept.contains("gzip") {
        if let Some(f) = StaticFiles::get(&format!("{path}.gz")) {
            return Some((f, Some("gzip")));
        }
    }
    None
}

fn serve_embedded(
    path: &str,
    file: EmbeddedFile,
    encoding: Option<&'static str>,
    req_headers: &HeaderMap,
) -> Response {
    let etag = etag_for(&file, encoding);
    let cache_control = cache_control_for(path);

    if let Some(if_none_match) = req_headers.get(header::IF_NONE_MATCH) {
        if if_none_match.as_bytes() == etag.as_bytes() {
            let mut resp = Response::builder()
                .status(StatusCode::NOT_MODIFIED)
                .header(header::CACHE_CONTROL, cache_control)
                .header(header::ETAG, &etag);
            if let Some(enc) = encoding {
                resp = resp.header(header::CONTENT_ENCODING, enc);
            }
            return resp.body(axum::body::Body::empty()).unwrap();
        }
    }

    let mut resp = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, mime_for(path))
        .header(header::CACHE_CONTROL, cache_control)
        .header(header::ETAG, &etag);
    if let Some(enc) = encoding {
        // Prevent caches from serving encoded bytes to identity-only clients.
        resp = resp.header(header::CONTENT_ENCODING, enc);
        resp = resp.header(header::VARY, "Accept-Encoding");
    }
    resp.body(axum::body::Body::from(file.data)).unwrap()
}

fn etag_for(file: &EmbeddedFile, encoding: Option<&str>) -> String {
    let hash = file.metadata.sha256_hash();
    let mut s = String::with_capacity(40);
    s.push('"');
    for b in &hash[..16] {
        let _ = write!(s, "{:02x}", b);
    }
    if let Some(enc) = encoding {
        s.push('-');
        s.push_str(enc);
    }
    s.push('"');
    s
}

fn cache_control_for(path: &str) -> &'static str {
    if path.starts_with("assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    }
}
