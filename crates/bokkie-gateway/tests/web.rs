//! Serving the production web package from `BOKKIE_WEB_ROOT`.
//!
//! The fixture has the shape `stobie build-web --production` writes for one
//! application directory: a mutable page, manifest and service worker at
//! the root, immutable files under `assets/<64 hex>/`, and `.br` and `.gz`
//! representations beside some of them. The representations' contents are
//! markers, because the gateway serves them as they are.

mod common;

use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use axum::http::{Method, StatusCode, header};
use bokkie_gateway::web::WebRoot;
use common::{Call, Gateway, HOST, ORIGIN, Reply};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

const HASH: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const IMMUTABLE: &str = "public, max-age=31536000, immutable";

struct Package {
    /// Holds the web root (`root/`) and a directory beside it (`outside/`).
    dir: tempfile::TempDir,
}

impl Package {
    fn root(&self) -> PathBuf {
        self.dir.path().join("root")
    }

    fn write(&self, relative: &str, contents: &str) {
        let path = self.root().join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    fn asset(relative: &str) -> String {
        format!("/assets/{HASH}/{relative}")
    }
}

fn package() -> Package {
    let package = Package {
        dir: tempfile::tempdir().unwrap(),
    };
    for (relative, contents) in [
        ("index.html", "<!doctype html><title>Bokkie</title>"),
        ("index.html.br", "br:index"),
        ("index.html.gz", "gz:index"),
        ("manifest.webmanifest", r#"{"name":"Bokkie"}"#),
        ("manifest.webmanifest.gz", "gz:manifest"),
        ("sw.js", "self.addEventListener('fetch', () => {});"),
        ("sw.js.br", "br:sw"),
        ("notes.txt", "a file outside assets"),
        ("assets/0123/app.js", "not a content hash"),
        (".secret", "dotfile"),
        ("assets/.hidden", "dotfile"),
        ("api/v1/version", "a file where an API route is"),
        ("api/v1/nope", "a file where no API route is"),
        ("api/v1/events", "a file where the event socket is"),
        ("healthz", "a file where the health check is"),
    ] {
        package.write(relative, contents);
    }
    for (relative, contents) in [
        ("app.js", "export const app = 1;"),
        ("app.js.br", "br:app"),
        ("app.js.gz", "gz:app"),
        ("app_bg.wasm", "\0asm"),
        ("app_bg.wasm.br", "br:wasm"),
        ("styles.css", "body {}"),
        ("icon.png", "png"),
        ("font.woff2", "woff2"),
        ("third-party-notices.txt", "Licences"),
        ("page.html", "<p>an HTML file among the assets</p>"),
        ("nested/worker.js", "export {};"),
    ] {
        package.write(&format!("assets/{HASH}/{relative}"), contents);
    }
    let outside = package.dir.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("secret.js"), "outside the root").unwrap();
    let root = package.root();
    symlink(outside.join("secret.js"), root.join("escape.js")).unwrap();
    symlink(&outside, root.join("linked")).unwrap();
    symlink(root.join("sw.js"), root.join("inside.js")).unwrap();
    package.write("leak.js", "inside");
    symlink(outside.join("secret.js"), root.join("leak.js.br")).unwrap();
    package
}

async fn gateway(package: &Package) -> Gateway {
    Gateway::with_web_root(Some(WebRoot::open(&package.root()).unwrap())).await
}

fn header(reply: &Reply, name: header::HeaderName) -> Option<&str> {
    reply.headers.get(name).map(|v| v.to_str().unwrap())
}

/// `body` as the test client decodes it: JSON when it parses, else text.
fn decoded(body: &str) -> serde_json::Value {
    serde_json::from_str(body).unwrap_or_else(|_| body.into())
}

fn assert_plain_404(reply: &Reply) {
    assert_eq!(reply.status, StatusCode::NOT_FOUND, "{:?}", reply.body);
    assert_eq!(reply.code(), "not_found", "{:?}", reply.body);
}

#[tokio::test]
async fn without_a_web_root_nothing_is_served() {
    let gateway = Gateway::new().await;
    for path in ["/", "/index.html", "/sw.js"] {
        assert_plain_404(&gateway.send(Call::get(path)).await);
    }
    assert_eq!(gateway.send(Call::get("/healthz")).await.body, "ok");
}

#[tokio::test]
async fn the_page_manifest_and_service_worker_revalidate() {
    let package = package();
    let gateway = gateway(&package).await;
    for (path, body, content_type) in [
        (
            "/",
            "<!doctype html><title>Bokkie</title>",
            "text/html; charset=utf-8",
        ),
        (
            "/index.html",
            "<!doctype html><title>Bokkie</title>",
            "text/html; charset=utf-8",
        ),
        (
            "/manifest.webmanifest",
            r#"{"name":"Bokkie"}"#,
            "application/manifest+json",
        ),
        (
            "/sw.js",
            "self.addEventListener('fetch', () => {});",
            "text/javascript; charset=utf-8",
        ),
        (
            "/notes.txt",
            "a file outside assets",
            "text/plain; charset=utf-8",
        ),
        (
            "/assets/0123/app.js",
            "not a content hash",
            "text/javascript; charset=utf-8",
        ),
        (
            &Package::asset("page.html"),
            "<p>an HTML file among the assets</p>",
            "text/html; charset=utf-8",
        ),
    ] {
        let reply = gateway.send(Call::get(path)).await;
        assert_eq!(reply.status, StatusCode::OK, "{path}");
        assert_eq!(reply.body, decoded(body), "{path}");
        assert_eq!(header(&reply, header::CONTENT_TYPE), Some(content_type));
        assert_eq!(header(&reply, header::CACHE_CONTROL), Some("no-cache"));
        assert_eq!(header(&reply, header::VARY), Some("accept-encoding"));
        assert_eq!(
            header(&reply, header::X_CONTENT_TYPE_OPTIONS),
            Some("nosniff")
        );
        assert_eq!(header(&reply, header::CONTENT_ENCODING), None);
        assert_eq!(header(&reply, header::LAST_MODIFIED), None);
        let etag = header(&reply, header::ETAG).unwrap().to_owned();
        assert!(
            etag.len() == 66 && etag.starts_with('"') && etag.ends_with('"'),
            "{etag}"
        );

        // Conditional requests revalidate against the ETag.
        for tag in [etag.clone(), format!("W/{etag}"), format!("\"x\", {etag}")] {
            let cached = gateway
                .send(Call::get(path).header("if-none-match", &tag))
                .await;
            assert_eq!(cached.status, StatusCode::NOT_MODIFIED, "{path} {tag}");
            assert_eq!(cached.body, serde_json::Value::Null);
            assert_eq!(header(&cached, header::ETAG), Some(etag.as_str()));
            assert_eq!(header(&cached, header::CACHE_CONTROL), Some("no-cache"));
            assert_eq!(header(&cached, header::VARY), Some("accept-encoding"));
        }
        let stale = gateway
            .send(Call::get(path).header("if-none-match", "\"stale\""))
            .await;
        assert_eq!(stale.status, StatusCode::OK);
        assert_eq!(stale.body, decoded(body));

        // HEAD carries the same headers and no body.
        let head = gateway.send(Call::new(Method::HEAD, path)).await;
        assert_eq!(head.status, StatusCode::OK);
        assert_eq!(head.body, serde_json::Value::Null);
        assert_eq!(header(&head, header::ETAG), Some(etag.as_str()));
        assert_eq!(header(&head, header::CACHE_CONTROL), Some("no-cache"));
        assert_eq!(
            header(&head, header::CONTENT_LENGTH),
            Some(body.len().to_string().as_str())
        );
    }
}

#[tokio::test]
async fn a_new_page_with_the_same_size_and_time_gets_a_new_etag() {
    // A rebuilt package's page differs only in its asset hash, so its size
    // stays the same; with normalised timestamps, so does its time.
    let package = package();
    let gateway = gateway(&package).await;
    let index = package.root().join("index.html");
    let time = std::fs::metadata(&index).unwrap().modified().unwrap();
    std::fs::remove_file(package.root().join("index.html.br")).unwrap();
    std::fs::remove_file(package.root().join("index.html.gz")).unwrap();
    let first = gateway.send(Call::get("/")).await;
    std::fs::write(&index, "<!doctype html><title>Bokkif</title>").unwrap();
    std::fs::File::options()
        .write(true)
        .open(&index)
        .unwrap()
        .set_modified(time)
        .unwrap();
    let etag = header(&first, header::ETAG).unwrap();
    let second = gateway
        .send(Call::get("/").header("if-none-match", etag))
        .await;
    assert_eq!(second.status, StatusCode::OK);
    assert_eq!(second.body, "<!doctype html><title>Bokkif</title>");
    assert_ne!(header(&second, header::ETAG), Some(etag));
}

#[tokio::test]
async fn hashed_assets_are_immutable_with_their_media_types() {
    let package = package();
    let gateway = gateway(&package).await;
    for (relative, body, content_type) in [
        (
            "app.js",
            "export const app = 1;",
            "text/javascript; charset=utf-8",
        ),
        (
            "nested/worker.js",
            "export {};",
            "text/javascript; charset=utf-8",
        ),
        ("app_bg.wasm", "\0asm", "application/wasm"),
        ("styles.css", "body {}", "text/css; charset=utf-8"),
        ("icon.png", "png", "image/png"),
        ("font.woff2", "woff2", "font/woff2"),
        // The font notices, which a production package keeps among its
        // assets, display as text rather than download.
        (
            "third-party-notices.txt",
            "Licences",
            "text/plain; charset=utf-8",
        ),
    ] {
        let path = Package::asset(relative);
        let reply = gateway.send(Call::get(&path)).await;
        assert_eq!(reply.status, StatusCode::OK, "{path}");
        assert_eq!(reply.body, body, "{path}");
        assert_eq!(header(&reply, header::CONTENT_TYPE), Some(content_type));
        assert_eq!(header(&reply, header::CACHE_CONTROL), Some(IMMUTABLE));
        assert_eq!(header(&reply, header::VARY), Some("accept-encoding"));
        assert_eq!(
            header(&reply, header::X_CONTENT_TYPE_OPTIONS),
            Some("nosniff")
        );
        let etag = header(&reply, header::ETAG).unwrap();
        let cached = gateway
            .send(Call::get(&path).header("if-none-match", etag))
            .await;
        assert_eq!(cached.status, StatusCode::NOT_MODIFIED, "{path}");
        assert_eq!(header(&cached, header::CACHE_CONTROL), Some(IMMUTABLE));
        let head = gateway.send(Call::new(Method::HEAD, &path)).await;
        assert_eq!(head.status, StatusCode::OK);
        assert_eq!(head.body, serde_json::Value::Null);
        assert_eq!(header(&head, header::CONTENT_TYPE), Some(content_type));
        assert_eq!(header(&head, header::CACHE_CONTROL), Some(IMMUTABLE));
    }

    // A range is cached like the whole file; a failed range is not cached.
    let path = Package::asset("app.js");
    let range = gateway
        .send(Call::get(&path).header("range", "bytes=0-5"))
        .await;
    assert_eq!(range.status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(range.body, "export");
    assert_eq!(header(&range, header::CACHE_CONTROL), Some(IMMUTABLE));
    let unsatisfiable = gateway
        .send(Call::get(&path).header("range", "bytes=1000-2000"))
        .await;
    assert_eq!(unsatisfiable.status, StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(
        header(&unsatisfiable, header::CACHE_CONTROL),
        Some("no-store")
    );
}

#[tokio::test]
async fn accept_encoding_selects_the_precompressed_representation() {
    let package = package();
    let gateway = gateway(&package).await;
    let app = Package::asset("app.js");
    let wasm = Package::asset("app_bg.wasm");
    let css = Package::asset("styles.css");
    for (path, accept, encoding, body) in [
        (app.as_str(), Some("br, gzip"), Some("br"), "br:app"),
        (&app, Some("gzip, deflate, br"), Some("br"), "br:app"),
        (&app, Some("gzip"), Some("gzip"), "gz:app"),
        (&app, Some("br;q=0, gzip"), Some("gzip"), "gz:app"),
        (&app, Some("gzip;q=0.8, br;q=0.5"), Some("gzip"), "gz:app"),
        (&app, Some("deflate"), None, "export const app = 1;"),
        (&app, Some("identity"), None, "export const app = 1;"),
        (&app, None, None, "export const app = 1;"),
        (&wasm, Some("gzip, br"), Some("br"), "br:wasm"),
        // Only Brotli exists, so gzip falls back to the file itself.
        (&wasm, Some("gzip"), None, "\0asm"),
        (&css, Some("br, gzip"), None, "body {}"),
        ("/", Some("br, gzip"), Some("br"), "br:index"),
        ("/", Some("gzip"), Some("gzip"), "gz:index"),
        (
            "/manifest.webmanifest",
            Some("br, gzip"),
            Some("gzip"),
            "gz:manifest",
        ),
        ("/sw.js", Some("br"), Some("br"), "br:sw"),
    ] {
        let mut call = Call::get(path);
        if let Some(accept) = accept {
            call = call.header("accept-encoding", accept);
        }
        let reply = gateway.send(call).await;
        assert_eq!(reply.status, StatusCode::OK, "{path} {accept:?}");
        assert_eq!(reply.body, body, "{path} {accept:?}");
        assert_eq!(
            header(&reply, header::CONTENT_ENCODING),
            encoding,
            "{path} {accept:?}"
        );
        assert_eq!(header(&reply, header::VARY), Some("accept-encoding"));
    }
    // The media type is the file's, not the representation's.
    let reply = gateway
        .send(Call::get(&wasm).header("accept-encoding", "br"))
        .await;
    assert_eq!(
        header(&reply, header::CONTENT_TYPE),
        Some("application/wasm")
    );
    // Each representation of a mutable file has its own ETag.
    let identity = gateway.send(Call::get("/")).await;
    let brotli = gateway
        .send(Call::get("/").header("accept-encoding", "br"))
        .await;
    assert_ne!(
        header(&identity, header::ETAG),
        header(&brotli, header::ETAG)
    );
    let revalidated = gateway
        .send(
            Call::get("/")
                .header("accept-encoding", "br")
                .header("if-none-match", header(&identity, header::ETAG).unwrap()),
        )
        .await;
    assert_eq!(revalidated.status, StatusCode::OK);
    assert_eq!(revalidated.body, "br:index");
}

#[tokio::test]
async fn the_api_health_check_and_event_socket_are_never_shadowed() {
    let package = package();
    let gateway = gateway(&package).await;
    let version = gateway.send(Call::get("/api/v1/version")).await;
    assert_eq!(version.status, StatusCode::OK);
    assert!(version.body["protocol"].is_number(), "{:?}", version.body);
    for path in ["/api/v1/nope", "/api", "/api/", "/api/v2/version"] {
        assert_plain_404(&gateway.send(Call::get(path)).await);
    }
    assert_eq!(gateway.send(Call::get("/healthz")).await.body, "ok");
    // Without a session the API answers, not the file.
    let events = gateway.send(Call::get("/api/v1/events")).await;
    assert_ne!(events.status, StatusCode::OK);
    assert_ne!(events.body, "a file where the event socket is");

    // The event WebSocket upgrades as usual.
    let cookie = gateway.admin("rob").await;
    let address = gateway.serve().await;
    let mut request = format!("ws://{address}/api/v1/events")
        .into_client_request()
        .unwrap();
    let headers = request.headers_mut();
    headers.insert("host", HOST.parse().unwrap());
    headers.insert("origin", ORIGIN.parse().unwrap());
    headers.insert("cookie", cookie.parse().unwrap());
    tokio_tungstenite::connect_async(request)
        .await
        .expect("the event socket upgrades");
}

#[tokio::test]
async fn missing_files_directories_and_escapes_are_the_plain_404() {
    let package = package();
    let gateway = gateway(&package).await;
    for path in [
        "/missing.js",
        "/assets",
        "/assets/",
        &format!("/assets/{HASH}"),
        &format!("/assets/{HASH}/"),
        &format!("/assets/{HASH}/missing.js"),
        "/index.html/",
        "//index.html",
        // Traversal, plain and encoded.
        "/../outside/secret.js",
        "/assets/../index.html",
        "/./index.html",
        "/%2e%2e/outside/secret.js",
        "/%2E%2E%2Foutside%2Fsecret.js",
        &format!("/assets/{HASH}/%2e%2e/%2e%2e/%2e%2e/outside/secret.js"),
        "/assets%5c..%5c..%5coutside%5csecret.js",
        "/index.html%00",
        "/%ff",
        // Symlinks out of the root, and a representation that is one.
        "/escape.js",
        "/linked/secret.js",
        "/leak.js",
        // Dotfiles and directly named representations.
        "/.secret",
        "/%2esecret",
        "/assets/.hidden",
        "/index.html.br",
        "/index.html.gz",
        // Encoded spellings of `/api` are still the API's namespace.
        "/%61pi/v1/nope",
        "/api%2fv1/nope",
        "/api%2Fv1/version",
    ] {
        let reply = gateway.send(Call::get(path)).await;
        assert_plain_404(&reply);
        assert_ne!(reply.body, "outside the root", "{path}");
    }
    // A symlink that stays inside the root is an ordinary file.
    let inside = gateway.send(Call::get("/inside.js")).await;
    assert_eq!(inside.status, StatusCode::OK);
    assert_eq!(inside.body, "self.addEventListener('fetch', () => {});");
    // `/healthz` is its route, never the file of that name.
    assert_eq!(gateway.send(Call::get("/healthz")).await.body, "ok");
}

#[tokio::test]
async fn static_files_obey_the_request_guard() {
    let package = package();
    let gateway = gateway(&package).await;
    let foreign = gateway
        .send(Call::get("/").host(Some("evil.example")))
        .await;
    assert_eq!(foreign.status, StatusCode::FORBIDDEN);
    assert_eq!(foreign.code(), "forbidden");
    let missing = gateway.send(Call::get("/sw.js").host(None)).await;
    assert_eq!(missing.status, StatusCode::FORBIDDEN);
    let extra = gateway
        .send(Call::get("/").host(Some("bokkie.tailnet.test")))
        .await;
    assert_eq!(extra.status, StatusCode::OK);

    // Only GET and HEAD read files; other methods still need the origin.
    let post = gateway.send(Call::post("/index.html")).await;
    assert_eq!(post.status, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(header(&post, header::ALLOW), Some("GET, HEAD"));
    let cross = gateway
        .send(Call::post("/index.html").origin(Some("https://evil.example")))
        .await;
    assert_eq!(cross.status, StatusCode::FORBIDDEN);
    assert_plain_404(&gateway.send(Call::post("/missing.js")).await);
}

#[test]
fn a_web_root_must_be_a_readable_directory_with_an_index() {
    let dir = tempfile::tempdir().unwrap();
    let error = |path: &Path| WebRoot::open(path).unwrap_err();

    let missing = dir.path().join("missing");
    let message = error(&missing);
    assert!(message.starts_with("BOKKIE_WEB_ROOT "), "{message}");
    assert!(
        message.contains(&missing.display().to_string()),
        "{message}"
    );

    let file = dir.path().join("file");
    std::fs::write(&file, "").unwrap();
    assert!(error(&file).ends_with("not a directory"));

    let empty = dir.path().join("empty");
    std::fs::create_dir(&empty).unwrap();
    assert!(error(&empty).contains("no index.html"), "{}", error(&empty));

    let index_dir = dir.path().join("index-dir");
    std::fs::create_dir_all(index_dir.join("index.html")).unwrap();
    assert!(error(&index_dir).contains("index.html is not a file"));

    let good = dir.path().join("good");
    std::fs::create_dir(&good).unwrap();
    std::fs::write(good.join("index.html"), "").unwrap();
    let root = WebRoot::open(&good).unwrap();
    assert_eq!(root.path(), std::fs::canonicalize(&good).unwrap());

    // Permissions do not bind root, so check only when they apply.
    let locked = dir.path().join("locked");
    std::fs::create_dir(&locked).unwrap();
    std::fs::write(locked.join("index.html"), "").unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read_dir(&locked).is_err() {
        assert!(
            error(&locked).contains("cannot read it"),
            "{}",
            error(&locked)
        );
    }
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::set_permissions(
        locked.join("index.html"),
        std::fs::Permissions::from_mode(0o000),
    )
    .unwrap();
    if std::fs::File::open(locked.join("index.html")).is_err() {
        assert!(error(&locked).contains("cannot read index.html"));
    }
}

#[test]
fn the_gateway_will_not_start_with_a_bad_web_root() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("web");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_bokkie-gateway"))
        .arg("serve")
        .env_clear()
        .env("BOKKIE_ORIGIN", ORIGIN)
        .env("BOKKIE_DATA", dir.path().join("data"))
        .env("BOKKIE_LISTEN", "127.0.0.1:0")
        .env("BOKKIE_CODEX_BIN", "/nonexistent/codex")
        .env("BOKKIE_WEB_ROOT", &missing)
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(&format!(
            "bokkie-gateway: BOKKIE_WEB_ROOT {}: cannot open it",
            missing.display()
        )),
        "{stderr}"
    );
    // It stopped before touching the data directory.
    assert!(!dir.path().join("data").exists());
}
