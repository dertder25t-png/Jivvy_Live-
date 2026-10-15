//! The web app, served by the daemon itself on its WebSocket port, so a church can run a
//! service from any device on its network with no cloud account and no internet: the same
//! static files Cloudflare Pages serves at app.jivvy.org (`apps/web/build`). A request that
//! asks to upgrade becomes the command channel; any other GET gets a file. Same port, same
//! certificate, same origin, so the page may open the channel without being listed.

use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

/// Longest request head read before deciding; browsers send far less.
const MAX_HEAD: usize = 16 * 1024;

/// What the daemon needs from a request's head.
#[derive(Debug, PartialEq)]
pub struct Head {
    pub method: String,
    /// The path without its query string.
    pub path: String,
    /// It asks to become a WebSocket.
    pub upgrade: bool,
}

/// Reads a request head (up to the blank line) and returns every byte read, so the WebSocket
/// handshake can read it again. None if the head is too long or the client stops first.
pub fn read_head<R: Read>(r: &mut R) -> std::io::Result<Option<Vec<u8>>> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
        if buf.len() > MAX_HEAD {
            return Ok(None);
        }
        let n = r.read(&mut chunk)?;
        if n == 0 {
            return Ok(None);
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    Ok(Some(buf))
}

pub fn parse(head: &[u8]) -> Option<Head> {
    let text = std::str::from_utf8(head).ok()?;
    let mut lines = text.split("\r\n");
    let mut first = lines.next()?.split(' ');
    let (method, target) = (first.next()?.to_string(), first.next()?);
    let path = target.split(['?', '#']).next().unwrap_or("/").to_string();
    let upgrade = lines.take_while(|l| !l.is_empty()).any(|l| {
        l.split_once(':').is_some_and(|(k, v)| {
            k.trim().eq_ignore_ascii_case("upgrade") && v.trim().eq_ignore_ascii_case("websocket")
        })
    });
    Some(Head { method, path, upgrade })
}

/// The file for a URL path under `root`: the file itself, or the app's `index.html` for a
/// path without an extension (a page of the single-page app). None for anything outside
/// `root` or missing.
pub fn resolve(root: &Path, url_path: &str) -> Option<PathBuf> {
    // Only plain names: no "..", no drive letters or backslashes, no escapes or control bytes.
    if url_path.bytes().any(|b| b == b'\\' || b == b'%' || b == 0 || b < 0x20) {
        return None;
    }
    let rel = Path::new(url_path.trim_start_matches('/'));
    if !rel.components().all(|c| matches!(c, Component::Normal(_))) {
        return None;
    }
    let file = root.join(rel);
    if file.is_file() {
        return Some(file);
    }
    let page = rel.extension().is_none();
    page.then(|| root.join("index.html")).filter(|f| f.is_file())
}

fn content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "webmanifest" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "wasm" => "application/wasm",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// What `/.well-known/jivvy` answers: how the web app knows a church computer served it, and
/// so connects to the address it came from.
const WELL_KNOWN: &str = "/.well-known/jivvy";

/// Answers one plain HTTP request (the connection then closes).
pub fn respond<W: Write>(w: &mut W, root: Option<&Path>, head: &Head) -> std::io::Result<()> {
    if head.method != "GET" && head.method != "HEAD" {
        return reply(w, head, "405 Method Not Allowed", "text/plain; charset=utf-8", "", b"only GET\n");
    }
    if head.path == WELL_KNOWN {
        let body = br#"{"service":"jivvy-live"}"#;
        return reply(w, head, "200 OK", "application/json", "no-store", body);
    }
    let Some(root) = root else {
        let body = b"This church computer isn't serving the Jivvy Live app (no --web-dir).\n";
        return reply(w, head, "404 Not Found", "text/plain; charset=utf-8", "", body);
    };
    match resolve(root, &head.path).and_then(|f| std::fs::read(&f).ok().map(|b| (f, b))) {
        Some((file, body)) => {
            // Built files are named by their content, so they never change; anything else
            // (index.html, the version file) is checked again every time.
            let immutable = head.path.starts_with("/_app/immutable/");
            let cache = if immutable { "public, max-age=31536000, immutable" } else { "no-cache" };
            reply(w, head, "200 OK", content_type(&file), cache, &body)
        }
        None => reply(w, head, "404 Not Found", "text/plain; charset=utf-8", "", b"not found\n"),
    }
}

fn reply<W: Write>(w: &mut W, head: &Head, status: &str, kind: &str, cache: &str, body: &[u8]) -> std::io::Result<()> {
    let mut out = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n",
        body.len()
    );
    if !cache.is_empty() {
        out.push_str(&format!("Cache-Control: {cache}\r\n"));
    }
    out.push_str("\r\n");
    w.write_all(out.as_bytes())?;
    if head.method != "HEAD" {
        w.write_all(body)?;
    }
    w.flush()
}

/// A stream that first gives back bytes already read from it (the request head).
pub struct Replay<S> {
    pub head: std::io::Cursor<Vec<u8>>,
    pub inner: S,
}

impl<S: Read> Read for Replay<S> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.head.read(buf)?;
        if n > 0 { Ok(n) } else { self.inner.read(buf) }
    }
}

impl<S: Write> Write for Replay<S> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.inner.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serves_files_pages_fall_back_to_the_app_and_nothing_escapes_the_folder() {
        let root = std::env::temp_dir().join(format!("jivvy-web-{}-{}", std::process::id(), crate::now_ms()));
        std::fs::create_dir_all(root.join("_app/immutable")).unwrap();
        std::fs::write(root.join("index.html"), "<!doctype html>").unwrap();
        std::fs::write(root.join("_app/immutable/app.js"), "x").unwrap();
        let get = |path: &str| {
            let head = parse(format!("GET {path} HTTP/1.1\r\nHost: a\r\n\r\n").as_bytes()).unwrap();
            let mut out = Vec::new();
            respond(&mut out, Some(&root), &head).unwrap();
            String::from_utf8(out).unwrap()
        };
        assert!(get("/").contains("text/html") && get("/").ends_with("<!doctype html>"));
        assert!(get("/_app/immutable/app.js?v=1").contains("text/javascript"));
        assert!(get("/_app/immutable/app.js").contains("immutable"));
        assert!(get("/operate").ends_with("<!doctype html>"), "a page of the app");
        assert!(get("/missing.js").starts_with("HTTP/1.1 404"), "a missing file isn't the app");
        for bad in ["/../secret.txt", "/_app/..%2f..%2fsecret", "/C:/Windows/win.ini", "/a\\..\\b"] {
            assert!(get(bad).starts_with("HTTP/1.1 404"), "{bad}");
        }
        assert!(get("/.well-known/jivvy").ends_with(r#"{"service":"jivvy-live"}"#));
        let up = parse(b"GET / HTTP/1.1\r\nHost: a\r\nUpgrade: WebSocket\r\nConnection: Upgrade\r\n\r\n").unwrap();
        assert!(up.upgrade);
        let _ = std::fs::remove_dir_all(&root);
    }
}
