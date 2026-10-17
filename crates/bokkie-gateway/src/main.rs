//! `bokkie-gateway serve` runs the gateway. `bokkie-gateway bootstrap-admin
//! <username> <display name>` creates the first admin, reading the password
//! from standard input; it refuses once any admin exists.

use std::io::BufRead;
use std::net::SocketAddr;
use std::process::ExitCode;
use std::sync::Arc;

use bokkie_gateway::clock::Clock;
use bokkie_gateway::codex::{Launcher, Supervisor, catalog};
use bokkie_gateway::config::Config;
use bokkie_gateway::{AppState, accounts, db, router};

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("BOKKIE_LOG")
                .unwrap_or_else(|_| "info".into()),
        )
        .init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("serve") => serve().await,
        Some("bootstrap-admin") if args.len() == 3 => bootstrap(&args[1], &args[2]).await,
        _ => Err("usage: bokkie-gateway serve | bootstrap-admin <username> <display name>".into()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("bokkie-gateway: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn serve() -> Result<(), String> {
    let config = Config::from_env()?;
    let pool = db::open(&config.database_path())
        .await
        .map_err(|e| format!("database: {e}"))?;
    let listener = tokio::net::TcpListener::bind(config.listen)
        .await
        .map_err(|e| format!("listen on {}: {e}", config.listen))?;
    let supervisor = supervisor(&config).await?;
    tracing::info!(listen = %config.listen, origin = config.origin.as_str(), "serving");
    let app = router(AppState::new(config, pool, Clock::system(), supervisor));
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await
    .map_err(|e| e.to_string())
}

/// Builds Bokkie's model catalog from the pinned binary and the supervisor
/// that uses it. Refuses to serve if the catalog cannot be made safe.
async fn supervisor(config: &Config) -> Result<Arc<Supervisor>, String> {
    let bundled = catalog::export_bundled(
        &config.codex_program,
        &config.data_dir.join("codex").join("catalog-home"),
    )
    .await
    .map_err(|e| format!("exporting the model catalog: {e}"))?;
    let catalog = catalog::write(&config.catalog_path(), bundled)
        .map_err(|e| format!("writing the model catalog: {e}"))?;
    let (supervisor, mut events) = Supervisor::new(
        &config.data_dir,
        catalog,
        Launcher::Process {
            program: config.codex_program.clone(),
            overrides: Vec::new(),
            env: Vec::new(),
        },
    );
    // Bots route these from a later increment; until then, record them.
    tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            tracing::debug!(user = %event.user, kind = ?event.kind, "app-server event");
        }
    });
    Ok(Arc::new(supervisor))
}

async fn bootstrap(username: &str, display_name: &str) -> Result<(), String> {
    let config = Config::from_env()?;
    let pool = db::open(&config.database_path())
        .await
        .map_err(|e| format!("database: {e}"))?;
    let mut password = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut password)
        .map_err(|e| e.to_string())?;
    let password = password.trim_end_matches(['\n', '\r']);
    let user = accounts::bootstrap_admin(
        &pool,
        Clock::system().now(),
        username,
        display_name,
        password,
    )
    .await
    .map_err(|f| f.message)?;
    println!("created admin {} ({})", user.username, user.id);
    Ok(())
}
