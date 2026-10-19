//! `bokkie-gateway serve` runs the gateway. `bokkie-gateway bootstrap-admin
//! <username> <display name>` creates the first admin, reading the password
//! from standard input; it refuses once any admin exists.

use std::io::BufRead;
use std::net::SocketAddr;
use std::process::ExitCode;
use std::sync::Arc;

use bokkie_gateway::auth::LoginLimiter;
use bokkie_gateway::clock::Clock;
use bokkie_gateway::codex::{Launcher, RuntimeEvent, Supervisor, catalog};
use bokkie_gateway::config::Config;
use bokkie_gateway::{AppState, accounts, db, router};
use tokio::sync::mpsc;

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
    // Routines are scheduled in their owners' time zones, which come from
    // the system's tzdata. Without it, every zone would read as unknown.
    let default_zone = bokkie_protocol::api::DEFAULT_TIME_ZONE;
    if bokkie_gateway::routines::schedule::zone(default_zone).is_none() {
        return Err(format!(
            "the system's time-zone database has no {default_zone}; install tzdata"
        ));
    }
    let pool = db::open(&config.database_path())
        .await
        .map_err(|e| format!("database: {e}"))?;
    let (supervisor, events) = supervisor(&config).await?;
    // Any user's own earlier ChatGPT sign-in is signed out and linked to
    // the household's before anything else uses Codex.
    supervisor.sign_out_own_sign_ins().await;
    // The household's app server starts first and keeps its sign-in fresh.
    tokio::spawn(supervisor.clone().run_refresher());
    let clock = Clock::system();
    let mut state = AppState::new(config, pool, clock.clone(), supervisor);
    state.limiter = Arc::new(
        LoginLimiter::persistent(state.db.clone(), clock.now_ms())
            .await
            .map_err(|e| format!("loading sign-in limits: {e}"))?,
    );
    state.conversations.recover().await.map_err(|f| f.message)?;
    state.routines.recover().await.map_err(|f| f.message)?;
    tokio::spawn(state.conversations.clone().run(events));
    tokio::spawn(
        state
            .conversations
            .clone()
            .run_rollovers(bokkie_gateway::bots::service::ROLLOVER_INTERVAL),
    );
    tokio::spawn(
        state
            .routines
            .clone()
            .run(bokkie_gateway::routines::PASS_INTERVAL),
    );
    tracing::info!(listen = %state.config.listen, origin = state.config.origin.as_str(), "serving");
    if let Some(root) = &state.config.web_root {
        tracing::info!(web_root = %root.path().display(), "serving the web package");
    }
    let listener_address = state.config.listen;
    let app = router(state);
    let listener = tokio::net::TcpListener::bind(listener_address)
        .await
        .map_err(|e| format!("listen on {listener_address}: {e}"))?;
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
async fn supervisor(
    config: &Config,
) -> Result<(Arc<Supervisor>, mpsc::UnboundedReceiver<RuntimeEvent>), String> {
    let bundled = catalog::export_bundled(
        &config.codex_program,
        &config.data_dir.join("codex").join("catalog-home"),
    )
    .await
    .map_err(|e| format!("exporting the model catalog: {e}"))?;
    let catalog = catalog::write(&config.catalog_path(), bundled)
        .map_err(|e| format!("writing the model catalog: {e}"))?;
    // No overrides and no environment: an app server's endpoints are always
    // Codex's own.
    Ok(Supervisor::new(
        &config.data_dir,
        catalog,
        Launcher::Process {
            program: config.codex_program.clone(),
            overrides: Vec::new(),
            env: Vec::new(),
        },
    ))
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
