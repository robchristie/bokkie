//! Checks against a real Codex binary, named by `BOKKIE_CODEX_BIN`.
//!
//! Without that variable these tests pass without running, so the default
//! `cargo test` needs no network. CI installs the pinned release and sets it.
//! Nothing here signs in or calls a model.

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use bokkie_codex::protocol::*;
use bokkie_codex::{AppServer, AppServerCommand, CODEX_VERSION, SCHEMA_BUNDLE};

fn codex() -> Option<PathBuf> {
    let bin = std::env::var_os("BOKKIE_CODEX_BIN").map(PathBuf::from);
    if bin.is_none() {
        eprintln!("BOKKIE_CODEX_BIN is not set; skipping the live Codex check");
    }
    bin
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "bokkie-codex-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn the_binary_is_the_pinned_version() {
    let Some(codex) = codex() else { return };
    let output = Command::new(codex).arg("--version").output().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        format!("codex-cli {CODEX_VERSION}")
    );
}

#[test]
fn the_vendored_schema_matches_the_pinned_binary() {
    let Some(codex) = codex() else { return };
    let home = scratch("schema-home");
    let out = scratch("schema");
    let status = Command::new(codex)
        .env("CODEX_HOME", &home)
        .args([
            "app-server",
            "generate-json-schema",
            "--experimental",
            "--out",
        ])
        .arg(&out)
        .status()
        .unwrap();
    assert!(status.success());
    let generated =
        std::fs::read_to_string(out.join("codex_app_server_protocol.schemas.json")).unwrap();
    assert!(
        generated == SCHEMA_BUNDLE,
        "the vendored schema differs from `codex app-server generate-json-schema --experimental`; \
         regenerate it as CONTRIBUTING.md describes"
    );
    let _ = std::fs::remove_dir_all(home);
    let _ = std::fs::remove_dir_all(out);
}

#[tokio::test]
async fn a_signed_out_app_server_answers_the_startup_queries() {
    let Some(codex) = codex() else { return };
    let home = scratch("home");
    let workspace = scratch("workspace");
    let mut command = AppServerCommand::new(codex, &home);
    command.current_dir = Some(workspace.clone());
    // Signed out, with Codex's defaults, it asks other hosts (its plugins
    // feature asks github.com and chatgpt.com). A local proxy refuses them,
    // so no request leaves the machine.
    let (proxy, refused) = deny_proxy().await;
    for name in ["HTTPS_PROXY", "HTTP_PROXY", "ALL_PROXY"] {
        command.env.push((name.into(), proxy.clone().into()));
        command
            .env
            .push((name.to_lowercase().into(), proxy.clone().into()));
    }
    command
        .env
        .push(("NO_PROXY".into(), "127.0.0.1,localhost".into()));
    let (server, _incoming) = AppServer::spawn(&command).unwrap();
    let client = server.client.clone();
    let checks = async {
        let init = client
            .initialize(&InitializeParams {
                client_info: ClientInfo {
                    name: "bokkie-codex-test".into(),
                    version: env!("CARGO_PKG_VERSION").into(),
                    title: None,
                },
                capabilities: Some(InitializeCapabilities {
                    experimental_api: true,
                    opt_out_notification_methods: None,
                }),
            })
            .await
            .unwrap();
        assert_eq!(
            std::fs::canonicalize(&init.codex_home).unwrap(),
            std::fs::canonicalize(&home).unwrap()
        );
        assert!(
            init.user_agent.contains(CODEX_VERSION),
            "{}",
            init.user_agent
        );

        let account = client
            .account_read(&GetAccountParams::default())
            .await
            .unwrap();
        assert_eq!(account.account, None);

        let config = client
            .config_read(&ConfigReadParams {
                cwd: Some(workspace.display().to_string()),
                include_layers: true,
            })
            .await
            .unwrap();
        assert!(config.config.is_object());

        let profiles = client
            .permission_profile_list(&PermissionProfileListParams::default())
            .await
            .unwrap();
        assert!(
            profiles.data.iter().any(|p| p.id == ":workspace"),
            "{profiles:?}"
        );
    };
    tokio::time::timeout(Duration::from_secs(60), checks)
        .await
        .expect("app server answered in time");
    drop(server);
    eprintln!("the deny proxy refused {:?}", refused.lock().unwrap());
    let _ = std::fs::remove_dir_all(home);
    let _ = std::fs::remove_dir_all(workspace);
}

/// A local proxy that refuses every request and records its first line.
async fn deny_proxy() -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let refused = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let record = refused.clone();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let mut buffer = vec![0; 4096];
            let n = stream.read(&mut buffer).await.unwrap_or(0);
            let line = String::from_utf8_lossy(&buffer[..n])
                .lines()
                .next()
                .unwrap_or_default()
                .to_owned();
            record.lock().unwrap().push(line);
            let _ = stream
                .write_all(b"HTTP/1.1 403 Forbidden\r\ncontent-length: 0\r\n\r\n")
                .await;
        }
    });
    (url, refused)
}
