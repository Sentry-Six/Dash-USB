use std::fmt::Write;

use axum::http::{HeaderMap, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use rust_embed::{Embed, EmbeddedFile};

#[derive(Embed)]
#[folder = "static/"]
struct StaticFiles;

/// Hand-rolled MIME table: the embedded SPA bundle and its pre-compressed
/// siblings are a closed set of extensions, so `mime_guess`'s thousands-entry
/// database is not worth its size. Falls back to `application/octet-stream`,
/// the same default `mime_guess::first_or_octet_stream()` gives.
fn mime_for(path: &str) -> &'static str {
    // Advertise the MIME of the *original* resource for a pre-compressed
    // sibling; Content-Encoding covers the wrapping. `foo.js.br` →
    // `application/javascript`.
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

/// SPA fallback: serve a static file, else index.html for client-side routing.
///
/// Caching keeps repeat page loads off the network for a car parked outside on
/// flaky WiFi. `/assets/*` is content-hashed by Vite (`index-CThdLhPi.js`) and
/// so immutable: cache forever. index.html and other entry files get
/// `no-cache`, so a soft reload picks up a new bundle after an OTA update
/// without a hard refresh.
///
/// When `build.sh` produced `.br` / `.gz` siblings, the pre-compressed bytes
/// are served directly, avoiding per-request compression CPU on the Pi Zero
/// 2W. Clients that do not advertise br/gzip get the raw bytes.
///
/// The ETag is the first 16 bytes (hex) of the sha256 hash rust-embed computes
/// at compile time, suffixed with the encoding so a client downgrading from br
/// to identity gets a fresh body instead of a stale 304. A matching
/// `If-None-Match` returns 304 without re-sending the asset.
pub async fn spa_handler(uri: Uri, headers: HeaderMap) -> Response {
    let path = uri.path().trim_start_matches('/');

    if let Some((file, encoding)) = pick_encoding(path, &headers) {
        return serve_embedded(path, file, encoding, &headers);
    }

    if let Some(file) = StaticFiles::get(path) {
        return serve_embedded(path, file, None, &headers);
    }

    // SPA fallback. index.html is short and changes per release; let
    // tower-http's CompressionLayer handle its (small) gzip.
    match StaticFiles::get("index.html") {
        Some(file) => serve_embedded("index.html", file, None, &headers),
        None => (StatusCode::NOT_FOUND, "Not Found").into_response(),
    }
}

/// Returns (file, content-encoding) if the client accepts a pre-compressed
/// sibling we have on disk. Brotli first (better ratio), then gzip.
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
        // Tell intermediaries the encoded body varies by Accept-Encoding
        // so a proxy doesn't hand the br bytes to a client that asked
        // for identity.
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
