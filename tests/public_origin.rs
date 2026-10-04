use std::process::Command;

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use bokkie::{
    DbExecutor, Store,
    http::router_with_ui_executor,
    http_security::{ApiRuntime, MUTATION_TOKEN_HEADER, PublicOrigin},
};
use serde_json::Value;
use tempfile::TempDir;
use tower::ServiceExt;

#[test]
fn public_origin_accepts_only_canonical_https_dns_origins() {
    for origin in [
        "https://bokkie.example.org",
        "https://bokkie.example.org:8443",
    ] {
        assert!(origin.parse::<PublicOrigin>().is_ok(), "{origin}");
    }
    for origin in [
        "http://bokkie.example.org",
        "HTTPS://bokkie.example.org",
        "https://Bokkie.example.org",
        "https://bokkie.example.org/",
        "https://bokkie.example.org/ui",
        "https://bokkie.example.org?query",
        "https://bokkie.example.org#fragment",
        "https://user@bokkie.example.org",
        "https://bokkie.example.org:443",
        "https://bokkie.example.org:0",
        "https://bokkie.example.org:08443",
        "https://bokkie.example.org:+8443",
        "https://bokkie.example.org:65536",
        "https://bokkie.example.org:",
        "https://127.0.0.1",
        "https://2130706433",
        "https://0x7f000001",
        "https://0x7f.0.0.0x1",
        "https://bokkie.0x123",
        "https://[::1]",
        "https://localhost",
        "https://bokkie.localhost",
        "https://bokkie.example.org.",
        "https://bokkie..org",
        "https://-bokkie.example.org",
        "https://bokkie-.example.org",
        "https://bokkie_example.org",
        "https://bökkie.example.org",
        "https://",
        " https://bokkie.example.org",
        "https://bokkie.example.org\n",
        "https://bokkie.example.org\\attacker.invalid",
    ] {
        assert!(origin.parse::<PublicOrigin>().is_err(), "{origin}");
    }
    assert!(
        format!("https://{}.org", "a".repeat(64))
            .parse::<PublicOrigin>()
            .is_err()
    );
}

#[test]
fn invalid_public_origin_or_bind_cannot_create_the_database() {
    let temporary = TempDir::new().unwrap();
    let database = temporary.path().join("must-not-exist.sqlite");
    for (bind, origin) in [
        ("127.0.0.1:0", "http://bokkie.example.org"),
        ("127.0.0.1:0", "https://bokkie.example.org/ui"),
        ("0.0.0.0:0", "https://bokkie.example.org"),
        ("192.0.2.1:0", "https://bokkie.example.org"),
        ("localhost:7744", "https://bokkie.example.org"),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_bokkie"))
            .arg("--database")
            .arg(&database)
            .args(["serve", "--bind", bind, "--public-origin", origin])
            .output()
            .unwrap();
        assert!(!output.status.success(), "{bind} {origin}");
        assert!(
            !database.exists(),
            "invalid configuration must precede migration"
        );
    }
}

#[tokio::test]
async fn public_origin_fences_bootstrap_ui_and_mutations() {
    for authority in ["bokkie.example.org", "bokkie.example.org:8443"] {
        let origin = format!("https://{authority}");
        let temporary = TempDir::new().unwrap();
        let database = temporary.path().join("public-origin.sqlite");
        let ui = temporary.path().join("ui");
        std::fs::create_dir(&ui).unwrap();
        std::fs::write(
            ui.join("index.html"),
            "<!doctype html><title>Bokkie</title>",
        )
        .unwrap();
        drop(Store::open(&database).unwrap());
        let runtime = ApiRuntime::with_public_origin(
            origin.parse().unwrap(),
            bokkie::SUPPORTED_SCHEMA_VERSION,
        )
        .unwrap();
        let stale_token = ApiRuntime::with_public_origin(
            origin.parse().unwrap(),
            bokkie::SUPPORTED_SCHEMA_VERSION,
        )
        .unwrap()
        .bootstrap()
        .mutation_token;
        let application =
            router_with_ui_executor(DbExecutor::start(database.clone()).unwrap(), ui, runtime);

        let bootstrap = application
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/bootstrap")
                    .header("host", authority)
                    .header("origin", &origin)
                    .header("sec-fetch-site", "same-origin")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(bootstrap.status(), StatusCode::OK);
        assert_eq!(bootstrap.headers()["cache-control"], "no-store");
        let bootstrap: Value =
            serde_json::from_slice(&to_bytes(bootstrap.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        let token = bootstrap["mutation_token"].as_str().unwrap();

        // The same middleware protects bootstrap, API reads, static assets and fallbacks.
        for path in [
            "/bootstrap",
            "/health",
            "/ui/",
            "/ui/missing.js",
            "/missing",
        ] {
            for (host, supplied_origin, expected) in [
                (
                    "127.0.0.1:7744",
                    origin.as_str(),
                    StatusCode::MISDIRECTED_REQUEST,
                ),
                (
                    "localhost:7744",
                    origin.as_str(),
                    StatusCode::MISDIRECTED_REQUEST,
                ),
                (
                    "bokkie.example.org.attacker.invalid",
                    origin.as_str(),
                    StatusCode::MISDIRECTED_REQUEST,
                ),
                (
                    authority,
                    "http://bokkie.example.org",
                    StatusCode::FORBIDDEN,
                ),
                (
                    authority,
                    "https://bokkie.example.org.attacker.invalid",
                    StatusCode::FORBIDDEN,
                ),
                (
                    authority,
                    "https://bokkie.example.org/ui",
                    StatusCode::FORBIDDEN,
                ),
                (authority, "http://127.0.0.1:7744", StatusCode::FORBIDDEN),
            ] {
                let response = application
                    .clone()
                    .oneshot(
                        Request::builder()
                            .uri(path)
                            .header("host", host)
                            .header("origin", supplied_origin)
                            .header("forwarded", format!("host={authority};proto=https"))
                            .header("x-forwarded-host", authority)
                            .header("x-forwarded-proto", "https")
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                assert_eq!(
                    response.status(),
                    expected,
                    "{path} {host} {supplied_origin}"
                );
            }
            for (duplicate, value, expected) in [
                ("host", authority, StatusCode::MISDIRECTED_REQUEST),
                ("origin", origin.as_str(), StatusCode::FORBIDDEN),
                ("sec-fetch-site", "same-origin", StatusCode::FORBIDDEN),
            ] {
                let response = application
                    .clone()
                    .oneshot(
                        Request::builder()
                            .uri(path)
                            .header("host", authority)
                            .header("origin", &origin)
                            .header("sec-fetch-site", "same-origin")
                            .header(duplicate, value)
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                assert_eq!(
                    response.status(),
                    expected,
                    "duplicate {duplicate} on {path}"
                );
            }
        }
        let response = application
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/ui/")
                    .header("host", authority)
                    .header("sec-fetch-site", "none")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        for (supplied_token, fetch_site, duplicate, expected) in [
            (None, "same-origin", None, StatusCode::FORBIDDEN),
            (
                Some(stale_token.as_str()),
                "same-origin",
                None,
                StatusCode::FORBIDDEN,
            ),
            (Some(token), "cross-site", None, StatusCode::FORBIDDEN),
            (Some(token), "same-site", None, StatusCode::FORBIDDEN),
            (Some(token), "none", None, StatusCode::FORBIDDEN),
            (
                Some(token),
                "same-origin",
                Some(MUTATION_TOKEN_HEADER),
                StatusCode::FORBIDDEN,
            ),
            (
                Some(token),
                "same-origin",
                Some("content-type"),
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
            ),
            (Some(token), "same-origin", None, StatusCode::CREATED),
        ] {
            let mut request = Request::builder()
                .method("POST")
                .uri("/obligations")
                .header("host", authority)
                .header("origin", &origin)
                .header("content-type", "application/json")
                .header("sec-fetch-site", fetch_site);
            if let Some(value) = supplied_token {
                request = request.header(MUTATION_TOKEN_HEADER, value);
            }
            if let Some(header) = duplicate {
                request = request.header(
                    header,
                    if header == MUTATION_TOKEN_HEADER {
                        token
                    } else {
                        "application/json"
                    },
                );
            }
            let response = application.clone().oneshot(request.body(Body::from(r#"{"id":"canonical-origin","description":"Authenticated proxy mutation"}"#)).unwrap()).await.unwrap();
            assert_eq!(response.status(), expected);
        }
        assert_eq!(Store::open(&database).unwrap().list().unwrap().len(), 1);
    }
}
