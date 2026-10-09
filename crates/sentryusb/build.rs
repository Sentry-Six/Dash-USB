//! Ensure rust-embed always has a `static/` source. A backend-only build gets
//! a clear placeholder; frontend builds replace it before compiling.

use std::path::Path;

fn main() {
    let dir = Path::new("static");
    let index = dir.join("index.html");
    if !index.exists() {
        let _ = std::fs::create_dir_all(dir.join("assets"));
        let placeholder = "<!DOCTYPE html>\
<html lang=\"en\"><head><meta charset=\"utf-8\">\
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
<title>DashUSB — frontend not built</title></head>\
<body style=\"font-family:system-ui,sans-serif;background:#0b0e13;color:#e6e9ef;\
margin:0;display:flex;min-height:100vh;align-items:center;justify-content:center\">\
<div style=\"max-width:36rem;padding:2rem;line-height:1.55\">\
<h1 style=\"margin:0 0 .75rem\">Frontend not built</h1>\
<p>This binary was compiled without the web UI. The real frontend is built by \
<code>./build.sh</code> (which runs <code>npm run build</code> and copies \
<code>web/dist</code> into <code>crates/sentryusb/static</code>) and by the CI \
release job. Run <code>./build.sh</code> before building, or install an official \
release binary.</p></div></body></html>";
        let _ = std::fs::write(&index, placeholder);
    }
    // Recreate the placeholder after a later static-directory cleanup.
    println!("cargo:rerun-if-changed=static");
}
