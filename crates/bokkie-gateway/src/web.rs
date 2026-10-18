//! Serving Stobie's production web package from `BOKKIE_WEB_ROOT`, at the
//! same origin as the API.
//!
//! The root is one application directory of a production package: a
//! mutable `index.html`, `manifest.webmanifest` and `sw.js` at the top, and
//! immutable files under `assets/<64 hex>/`, each with optional `.br` and
//! `.gz` representations beside it. Static serving is the router's
//! fallback, so it never shadows a route, and it never answers under
//! `/api`, so an unknown API path stays the API's own 404. Everything the
//! request guard checks, including `Host`, applies here too.
//!
//! [`tower_http::services::ServeFile`] reads the files and negotiates
//! `Accept-Encoding` among the precompressed representations. This module
//! decides which file a path names, refuses the rest, and sets the caching
//! headers. Mutable files get a strong `ETag` hashed from the representation
//! sent, because the `ETag` and `Last-Modified` that `ServeFile` derives from
//! the file's size and modification time would stay the same when a package
//! with normalised timestamps changes only its asset hash.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::extract::Request;
use axum::http::header::{
    ACCEPT_ENCODING, ACCEPT_RANGES, ALLOW, CACHE_CONTROL, CONTENT_TYPE, ETAG, IF_NONE_MATCH,
    LAST_MODIFIED, VARY,
};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use sha2::{Digest, Sha256};
use tower_http::services::ServeFile;

use crate::error::Failure;

/// `Cache-Control` for the page, the manifest, the service worker and any
/// other file outside `assets/<hash>/`: always revalidate.
pub const MUTABLE: &str = "no-cache";

/// `Cache-Control` for files under `assets/<64 hex>/`, whose path changes
/// whenever their contents do.
pub const IMMUTABLE: &str = "public, max-age=31536000, immutable";

/// The largest mutable file served. Mutable files are read whole to hash
/// them; a package's page, manifest and service worker are a few kilobytes.
const MUTABLE_LIMIT: usize = 16 * 1024 * 1024;

/// A checked web root: an existing, readable directory with an
/// `index.html`, held as its canonical path.
#[derive(Debug, Clone)]
pub struct WebRoot {
    root: Arc<PathBuf>,
}

impl WebRoot {
    /// Checks `path` as `BOKKIE_WEB_ROOT`. The message names the variable,
    /// for the gateway's startup error.
    pub fn open(path: &Path) -> Result<Self, String> {
        let fail = |what: String| format!("BOKKIE_WEB_ROOT {}: {what}", path.display());
        let root = std::fs::canonicalize(path).map_err(|e| fail(format!("cannot open it: {e}")))?;
        if !root.is_dir() {
            return Err(fail("not a directory".into()));
        }
        std::fs::read_dir(&root).map_err(|e| fail(format!("cannot read it: {e}")))?;
        let index = root.join("index.html");
        match std::fs::metadata(&index) {
            Ok(meta) if meta.is_file() => {}
            Ok(_) => return Err(fail("index.html is not a file".into())),
            Err(e) => return Err(fail(format!("no index.html: {e}"))),
        }
        std::fs::File::open(&index).map_err(|e| fail(format!("cannot read index.html: {e}")))?;
        Ok(Self {
            root: Arc::new(root),
        })
    }

    /// The canonical root directory.
    pub fn path(&self) -> &Path {
        &self.root
    }

    /// The file that the request path `path` names, if it may be served.
    fn resolve(&self, path: &str) -> Option<Resolved> {
        if path == "/api" || path.starts_with("/api/") {
            return None;
        }
        let decoded = percent_encoding::percent_decode_str(path)
            .decode_utf8()
            .ok()?;
        let relative = decoded.strip_prefix('/')?;
        let relative = if relative.is_empty() {
            "index.html"
        } else {
            relative
        };
        let segments: Vec<&str> = relative.split('/').collect();
        // This refuses `.`, `..`, dotfiles, empty segments (so a trailing
        // slash, which would name a directory) and Windows separators.
        if segments
            .iter()
            .any(|s| s.is_empty() || s.starts_with('.') || s.contains(['\\', '\0']))
        {
            return None;
        }
        let name = segments.last()?;
        // Precompressed representations are only served by negotiation.
        if name.ends_with(".br") || name.ends_with(".gz") {
            return None;
        }
        let file = self.contained(&self.root.join(relative))?;
        // `ServeFile` opens these beside the file it serves. Refuse the file
        // rather than follow one out of the root.
        for suffix in [".br", ".gz"] {
            let mut sibling = file.clone().into_os_string();
            sibling.push(suffix);
            let sibling = PathBuf::from(sibling);
            if std::fs::symlink_metadata(&sibling).is_ok() && self.contained(&sibling).is_none() {
                tracing::warn!(path = %sibling.display(), "refusing a representation outside the web root");
                return None;
            }
        }
        let immutable = segments.len() > 2
            && segments[0] == "assets"
            && segments[1].len() == 64
            && segments[1]
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            && !name.ends_with(".html");
        Some(Resolved {
            content_type: content_type(name),
            file,
            immutable,
        })
    }

    /// `path`'s canonical form, if it is a regular file inside the root.
    fn contained(&self, path: &Path) -> Option<PathBuf> {
        let canonical = std::fs::canonicalize(path).ok()?;
        (canonical.starts_with(&*self.root) && canonical.is_file()).then_some(canonical)
    }
}

#[derive(Debug)]
struct Resolved {
    file: PathBuf,
    content_type: &'static str,
    immutable: bool,
}

/// The media type for a file name, matching Stobie's reference server.
fn content_type(name: &str) -> &'static str {
    let extension = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase());
    match extension.as_deref() {
        Some("html") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("wasm") => "application/wasm",
        Some("json") => "application/json",
        Some("webmanifest") => "application/manifest+json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        Some("woff") => "font/woff",
        Some("ttf") => "font/ttf",
        Some("otf") => "font/otf",
        Some("txt") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// The router's fallback when a web root is configured: serves the file a
/// `GET` or `HEAD` names, or the gateway's ordinary 404.
pub async fn serve(root: WebRoot, request: Request) -> Response {
    let path = request.uri().path().to_owned();
    // Resolving touches the file system, so keep it off the async workers.
    let resolved = match tokio::task::spawn_blocking(move || root.resolve(&path)).await {
        Ok(resolved) => resolved,
        Err(error) => {
            return Failure::internal(format!("resolving a path: {error}")).into_response();
        }
    };
    let Some(resolved) = resolved else {
        return Failure::not_found().into_response();
    };
    if !matches!(*request.method(), Method::GET | Method::HEAD) {
        let mut response = StatusCode::METHOD_NOT_ALLOWED.into_response();
        response
            .headers_mut()
            .insert(ALLOW, HeaderValue::from_static("GET, HEAD"));
        return response;
    }
    let result = if resolved.immutable {
        serve_immutable(&resolved, request).await
    } else {
        serve_mutable(&resolved, request).await
    };
    let mut response = match result {
        Ok(response) => response,
        Err(error) => {
            return Failure::internal(format!("serving {}: {error}", resolved.file.display()))
                .into_response();
        }
    };
    if matches!(
        response.status(),
        StatusCode::OK | StatusCode::PARTIAL_CONTENT
    ) {
        response.headers_mut().insert(
            CONTENT_TYPE,
            HeaderValue::from_static(resolved.content_type),
        );
    }
    let headers = response.headers_mut();
    headers.insert(
        CACHE_CONTROL,
        HeaderValue::from_static(if resolved.immutable {
            IMMUTABLE
        } else {
            MUTABLE
        }),
    );
    headers.insert(VARY, HeaderValue::from_static("accept-encoding"));
    headers.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    response
}

fn file(resolved: &Resolved) -> ServeFile {
    ServeFile::new(&resolved.file)
        .precompressed_br()
        .precompressed_gzip()
}

/// Streams an immutable file. `ServeFile` handles ranges and conditional
/// requests.
async fn serve_immutable(resolved: &Resolved, request: Request) -> std::io::Result<Response> {
    let response = file(resolved).try_call(request).await?;
    Ok(response.map(Body::new))
}

/// Reads a mutable file whole and answers with a content `ETag`, or `304`
/// when `If-None-Match` names it.
async fn serve_mutable(resolved: &Resolved, request: Request) -> std::io::Result<Response> {
    // Only `Accept-Encoding` reaches `ServeFile`, so it always answers 200
    // with the whole representation; validators and ranges are decided
    // here.
    let mut inner = Request::new(Body::empty());
    if let Some(accept) = request.headers().get(ACCEPT_ENCODING) {
        inner.headers_mut().insert(ACCEPT_ENCODING, accept.clone());
    }
    let response = file(resolved).try_call(inner).await?;
    if response.status() != StatusCode::OK {
        return Err(std::io::Error::other(format!(
            "unexpected status {}",
            response.status()
        )));
    }
    let (mut parts, body) = response.map(Body::new).into_parts();
    let bytes = axum::body::to_bytes(body, MUTABLE_LIMIT)
        .await
        .map_err(std::io::Error::other)?;
    let etag = format!("\"{:x}\"", Sha256::digest(&bytes));
    let etag = HeaderValue::from_str(&etag).expect("hex is a valid header value");
    for name in [ETAG, LAST_MODIFIED, ACCEPT_RANGES] {
        parts.headers.remove(name);
    }
    if none_match(request.headers(), &etag) {
        let mut not_modified = StatusCode::NOT_MODIFIED.into_response();
        not_modified.headers_mut().insert(ETAG, etag);
        return Ok(not_modified);
    }
    parts.headers.insert(ETAG, etag);
    let body = if request.method() == Method::HEAD {
        Body::empty()
    } else {
        Body::from(bytes)
    };
    Ok(Response::from_parts(parts, body))
}

/// Whether `If-None-Match` matches `etag`, by the weak comparison RFC 9110
/// requires for it.
fn none_match(headers: &HeaderMap, etag: &HeaderValue) -> bool {
    let etag = etag.as_bytes();
    headers
        .get_all(IF_NONE_MATCH)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .any(|tag| tag == "*" || tag.trim_start_matches("W/").as_bytes() == etag)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_types() {
        for (name, expected) in [
            ("index.html", "text/html; charset=utf-8"),
            ("app.js", "text/javascript; charset=utf-8"),
            ("app.MJS", "text/javascript; charset=utf-8"),
            ("app_bg.wasm", "application/wasm"),
            ("manifest.webmanifest", "application/manifest+json"),
            ("styles.css", "text/css; charset=utf-8"),
            ("icon.png", "image/png"),
            ("font.woff2", "font/woff2"),
            ("font.woff", "font/woff"),
            ("README", "application/octet-stream"),
        ] {
            assert_eq!(content_type(name), expected, "{name}");
        }
    }

    #[test]
    fn if_none_match_lists_and_weak_tags() {
        let etag = HeaderValue::from_static("\"abc\"");
        let check = |value: &str| {
            let mut headers = HeaderMap::new();
            headers.insert(IF_NONE_MATCH, HeaderValue::from_str(value).unwrap());
            none_match(&headers, &etag)
        };
        assert!(check("\"abc\""));
        assert!(check("W/\"abc\""));
        assert!(check("\"x\", \"abc\""));
        assert!(check("*"));
        assert!(!check("\"abcd\""));
        assert!(!check("abc"));
        assert!(!none_match(&HeaderMap::new(), &etag));
    }
}
