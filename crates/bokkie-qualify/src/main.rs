//! `bokkie-qualify`: the Phase 0 qualification harness.
//!
//! It runs inside the qualification container, standing in for the gateway.
//! Each subcommand prints one JSON result line on standard output and exits
//! 0 only when every check in it passed. Progress goes to standard error.
//! Nothing here prints credentials or file contents.
//!
//! See `deploy/qualify/README.md` for the procedure.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bokkie_codex::protocol::*;
use bokkie_codex::{AppServer, AppServerCommand, Client, Incoming, RequestId, RpcError};
use bokkie_qualify::layout::{
    CANARY, Layout, PROFILE, PlanInput, SHARED_WRITE_PREFIX, codex_config, probe_spec,
};
use bokkie_qualify::{
    Expect, Finding, PeerSpec, ProbeReport, ProbeSpec, evaluate, read_cmdline, visible_pids,
};
use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedReceiver;

const DEFAULT_PROBE: &str = "/usr/local/bin/bokkie-sandbox-probe";

fn probe_path() -> String {
    std::env::var("BOKKIE_SANDBOX_PROBE").unwrap_or_else(|_| DEFAULT_PROBE.to_owned())
}
const TURN_TIMEOUT: Duration = Duration::from_secs(300);

type Result<T> = std::result::Result<T, String>;

struct Args {
    command: String,
    options: HashMap<String, String>,
}

impl Args {
    fn parse() -> Self {
        let mut args = std::env::args().skip(1);
        let command = args.next().unwrap_or_default();
        let mut options = HashMap::new();
        let rest: Vec<String> = args.collect();
        let mut index = 0;
        while index < rest.len() {
            let key = rest[index].trim_start_matches("--").to_owned();
            let value = rest
                .get(index + 1)
                .filter(|v| !v.starts_with("--"))
                .cloned();
            index += if value.is_some() { 2 } else { 1 };
            options.insert(key, value.unwrap_or_else(|| "true".into()));
        }
        Self { command, options }
    }

    fn get(&self, key: &str) -> Option<&str> {
        self.options.get(key).map(String::as_str)
    }

    fn required(&self, key: &str) -> Result<&str> {
        self.get(key).ok_or_else(|| format!("--{key} is required"))
    }

    fn flag(&self, key: &str) -> bool {
        self.get(key) == Some("true")
    }
}

struct Context {
    layout: Layout,
    codex: PathBuf,
}

impl Context {
    fn app_server_command(&self, user: &str) -> AppServerCommand {
        let mut command = AppServerCommand::new(&self.codex, self.layout.codex_home(user));
        command.current_dir = Some(self.layout.workspace(user));
        command
            .env
            .push(("HOME".into(), self.layout.home(user).into()));
        command.stderr = Some(self.layout.state(user).join("app-server.stderr"));
        command
    }

    async fn start(&self, user: &str) -> Result<(AppServer, UnboundedReceiver<Incoming>)> {
        let (server, incoming) =
            AppServer::spawn(&self.app_server_command(user)).map_err(|e| e.to_string())?;
        server
            .client
            .initialize(&InitializeParams {
                client_info: ClientInfo {
                    name: "bokkie-qualify".into(),
                    version: env!("CARGO_PKG_VERSION").into(),
                    title: None,
                },
                capabilities: Some(InitializeCapabilities {
                    experimental_api: true,
                    opt_out_notification_methods: None,
                }),
            })
            .await
            .map_err(|e| format!("initialize: {e}"))?;
        Ok((server, incoming))
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = Args::parse();
    let context = Context {
        layout: Layout::new(args.get("data").unwrap_or("/data")),
        codex: PathBuf::from(args.get("codex").unwrap_or("/usr/local/bin/codex")),
    };
    let result = match args.command.as_str() {
        "idle" => idle().await,
        "env" => environment(&context).await,
        "setup" => setup(&context, &args),
        "login" => login(&context, &args).await,
        "account" => account(&context, &args).await,
        "chat" => chat(&context, &args).await,
        "sandbox" => sandbox(&context, &args).await,
        "bwrap" => observe_bwrap(&context, &args).await,
        "orphans" => orphans(&context, &args).await,
        "orphan-hold" => orphan_hold(&context, &args).await,
        "find-sleeper" => find_sleeper(&args),
        "hold" => hold(&context, &args).await,
        "tool" => tool(&context, &args).await,
        "turn-exec" => turn_exec(&context, &args).await,
        other => Err(format!("unknown command {other:?}")),
    };
    match result {
        Ok(output) => {
            let pass = output["pass"].as_bool().unwrap_or(true);
            println!("{output}");
            if pass {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            }
        }
        Err(error) => {
            println!(
                "{}",
                json!({"step": args.command, "pass": false, "error": error})
            );
            ExitCode::from(2)
        }
    }
}

fn log(message: impl AsRef<str>) {
    eprintln!("bokkie-qualify: {}", message.as_ref());
}

fn nonce() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{:x}{:x}", nanos, std::process::id())
}

fn users(args: &Args) -> Vec<String> {
    args.get("users")
        .or(args.get("user"))
        .unwrap_or_default()
        .split(',')
        .filter(|u| !u.is_empty())
        .map(str::to_owned)
        .collect()
}

// ---------------------------------------------------------------------------
// idle, env, setup

async fn idle() -> Result<Value> {
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .map_err(|e| e.to_string())?;
    tokio::select! {
        _ = term.recv() => {}
        _ = tokio::signal::ctrl_c() => {}
    }
    Ok(json!({"step": "idle", "pass": true}))
}

async fn environment(context: &Context) -> Result<Value> {
    let version = |program: &str| {
        std::process::Command::new(program)
            .arg("--version")
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
            .unwrap_or_else(|e| format!("unavailable: {e}"))
    };
    let read = |path: &str| {
        std::fs::read_to_string(path)
            .map(|s| s.trim().to_owned())
            .unwrap_or_else(|e| format!("unreadable: {e}"))
    };
    let which_bwrap = std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .map(|dir| Path::new(dir).join("bwrap"))
        .find(|p| p.exists())
        .map(|p| p.display().to_string());
    let bundled = context
        .codex
        .canonicalize()
        .ok()
        .and_then(|p| {
            p.parent()?
                .parent()
                .map(|d| d.join("codex-resources/bwrap"))
        })
        .filter(|p| p.exists());
    Ok(json!({
        "step": "env",
        "pass": true,
        "identity": bokkie_qualify::identity(),
        "kernel": read("/proc/sys/kernel/osrelease"),
        "pid1": read_cmdline(Path::new("/proc/1/cmdline")),
        "max_user_namespaces": read("/proc/sys/user/max_user_namespaces"),
        "codex": version(&context.codex.display().to_string()),
        "bwrap_on_path": which_bwrap,
        "bwrap_on_path_version": which_bwrap.as_deref().map(version),
        "bwrap_bundled": bundled.as_ref().map(|p| p.display().to_string()),
        "bwrap_bundled_version": bundled.as_ref().map(|p| version(&p.display().to_string())),
        "proc_masks": proc_masks(),
    }))
}

/// Mounts layered over `/proc` by the container runtime (masked and
/// read-only system paths).
fn proc_masks() -> Vec<String> {
    std::fs::read_to_string("/proc/self/mountinfo")
        .unwrap_or_default()
        .lines()
        .filter_map(|line| line.split(' ').nth(4))
        .filter(|point| point.starts_with("/proc/"))
        .map(str::to_owned)
        .collect()
}

fn setup(context: &Context, args: &Args) -> Result<Value> {
    let layout = &context.layout;
    let user = args.required("user")?;
    let private = |path: &Path| -> Result<()> {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(path).map_err(|e| format!("{}: {e}", path.display()))?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("{}: {e}", path.display()))
    };
    for dir in [
        layout.codex_home(user),
        layout.workspace(user),
        layout.home(user),
        layout.state(user),
    ] {
        private(&dir)?;
    }
    let write = |path: PathBuf, contents: &str| {
        std::fs::write(&path, contents).map_err(|e| format!("{}: {e}", path.display()))
    };
    let extra_read: Vec<String> = args
        .get("extra-read")
        .unwrap_or_default()
        .split(',')
        .filter(|p| !p.is_empty())
        .map(str::to_owned)
        .collect();
    write(
        layout.codex_home(user).join("config.toml"),
        &codex_config(layout, &extra_read),
    )?;
    write(
        layout.codex_home(user).join(CANARY),
        "canary: not a credential\n",
    )?;
    write(
        layout.workspace(user).join("README.md"),
        &format!("{user}'s workspace\n"),
    )?;
    // The stand-in database belongs to whoever creates it first.
    if !layout.database().exists() {
        write(layout.database(), "synthetic database\n")?;
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(layout.database(), std::fs::Permissions::from_mode(0o600));
    }
    Ok(json!({"step": "setup", "user": user, "pass": true}))
}

// ---------------------------------------------------------------------------
// Sign-in, account and token refresh

async fn login(context: &Context, args: &Args) -> Result<Value> {
    let user = args.required("user")?;
    let (server, mut incoming) = context.start(user).await?;
    let client = &server.client;
    let before = client
        .account_read(&GetAccountParams::default())
        .await
        .map_err(|e| e.to_string())?;
    if let Some(account) = before.account {
        return Ok(json!({
            "step": "login", "user": user, "pass": true,
            "already_signed_in": true, "account": account_summary(&account),
        }));
    }
    let started = client
        .account_login_start(&LoginAccountParams::ChatgptDeviceCode)
        .await
        .map_err(|e| e.to_string())?;
    let LoginAccountResponse::ChatgptDeviceCode {
        login_id,
        user_code,
        verification_url,
    } = started
    else {
        return Err("the app server did not start a device-code login".into());
    };
    log(format!(
        "DEVICE CODE for {user}: open {verification_url} and enter {user_code}"
    ));
    let deadline = Instant::now() + Duration::from_secs(15 * 60);
    let completed = loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match tokio::time::timeout(remaining, incoming.recv()).await {
            Err(_) => {
                let _ = client
                    .account_login_cancel(&CancelLoginAccountParams { login_id })
                    .await;
                return Err("device-code login timed out".into());
            }
            Ok(None) => return Err("the app server exited during login".into()),
            Ok(Some(Incoming::Notification(ServerNotification::AccountLoginCompleted(n)))) => {
                break n;
            }
            Ok(Some(_)) => {}
        }
    };
    let after = client
        .account_read(&GetAccountParams::default())
        .await
        .map_err(|e| e.to_string())?;
    let auth = context.layout.codex_home(user).join("auth.json");
    Ok(json!({
        "step": "login", "user": user,
        "pass": completed.success && after.account.is_some() && auth.exists(),
        "completed": {"success": completed.success, "error": completed.error},
        "account": after.account.as_ref().map(account_summary),
        "auth_json_on_volume": auth.exists(),
        "auth_json_mode": file_mode(&auth),
    }))
}

fn account_summary(account: &Account) -> Value {
    match account {
        Account::Chatgpt { plan_type, email } => json!({
            "type": "chatgpt", "plan": plan_type,
            "email_present": email.is_some(),
        }),
        Account::ApiKey => json!({"type": "apiKey"}),
        Account::Unknown => json!({"type": "unknown"}),
    }
}

fn file_mode(path: &Path) -> Option<String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .ok()
        .map(|m| format!("{:o}", m.permissions().mode() & 0o7777))
}

/// The refresh bookkeeping in `auth.json`, without any token.
fn auth_refresh_state(path: &Path) -> Value {
    let modified = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs());
    let last_refresh = std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| v.get("last_refresh").cloned());
    json!({"last_refresh": last_refresh, "modified": modified})
}

async fn account(context: &Context, args: &Args) -> Result<Value> {
    let user = args.required("user")?;
    let (server, _incoming) = context.start(user).await?;
    let client = &server.client;
    let auth = context.layout.codex_home(user).join("auth.json");
    let account = client
        .account_read(&GetAccountParams::default())
        .await
        .map_err(|e| format!("account/read: {e}"))?;
    let models = client
        .model_list(&ModelListParams::default())
        .await
        .map_err(|e| format!("model/list: {e}"))?;
    let limits = client
        .account_rate_limits_read()
        .await
        .map_err(|e| format!("account/rateLimits/read: {e}"))?;
    let profiles = client
        .permission_profile_list(&PermissionProfileListParams {
            cwd: Some(context.layout.workspace(user).display().to_string()),
        })
        .await
        .map_err(|e| format!("permissionProfile/list: {e}"))?;
    let config = client
        .config_read(&ConfigReadParams {
            cwd: Some(context.layout.workspace(user).display().to_string()),
            include_layers: false,
        })
        .await
        .map_err(|e| format!("config/read: {e}"))?;

    let mut refresh = Value::Null;
    let mut refresh_pass = true;
    if args.flag("refresh") {
        let before = auth_refresh_state(&auth);
        let refreshed = client
            .account_read(&GetAccountParams {
                refresh_token: true,
            })
            .await
            .map_err(|e| format!("account/read with refresh: {e}"))?;
        let after = auth_refresh_state(&auth);
        refresh_pass = refreshed.account.is_some()
            && after["last_refresh"] != before["last_refresh"]
            && !after["last_refresh"].is_null();
        let state = context.layout.state(user).join("refresh.json");
        let _ = std::fs::write(&state, after.to_string());
        refresh = json!({"before": before, "after": after, "changed": refresh_pass});
    }
    // After a restart, the refreshed state must be what is on disk.
    let mut persisted = Value::Null;
    if args.flag("expect-persisted") {
        let saved = std::fs::read_to_string(context.layout.state(user).join("refresh.json"))
            .ok()
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
            .unwrap_or(Value::Null);
        let now = auth_refresh_state(&auth);
        let ok = !saved.is_null()
            && saved["last_refresh"] == now["last_refresh"]
            && account.account.is_some();
        refresh_pass &= ok;
        persisted = json!({"saved": saved, "now": now, "persisted": ok});
    }

    let window = |w: &Option<RateLimitWindow>| {
        w.as_ref()
            .map(|w| json!({"used_percent": w.used_percent, "window_mins": w.window_duration_mins}))
    };
    Ok(json!({
        "step": "account", "user": user,
        "pass": account.account.is_some() && !models.data.is_empty() && refresh_pass,
        "account": account.account.as_ref().map(account_summary),
        "models": models.data.len(),
        "default_model": models.data.iter().find(|m| m.is_default).map(|m| &m.id),
        "rate_limits": {
            "limit_id": limits.rate_limits.limit_id,
            "primary": window(&limits.rate_limits.primary),
            "secondary": window(&limits.rate_limits.secondary),
            "buckets": limits.rate_limits_by_limit_id.map(|b| b.len()),
        },
        "permission_profiles": profiles.data.iter().map(|p| (&p.id, p.allowed)).collect::<Vec<_>>(),
        "config": {
            "default_permissions": config.config.get("default_permissions"),
            "sandbox_mode": config.config.get("sandbox_mode"),
            "approval_policy": config.config.get("approval_policy"),
        },
        "auth_json_mode": file_mode(&auth),
        "refresh": refresh,
        "persisted": persisted,
    }))
}

// ---------------------------------------------------------------------------
// Turns

#[derive(Debug, Default)]
struct TurnOutcome {
    status: Option<TurnStatus>,
    error: Option<String>,
    text: String,
    deltas: usize,
    items: Vec<ThreadItem>,
}

/// Another thread that a `message_bot` call reaches.
struct BotRoute {
    name: String,
    thread_id: String,
}

/// Runs a turn on `thread_id` to completion. `message_bot` calls start a turn
/// on the named Bot's thread and answer with its reply. Approval requests are
/// declined.
async fn drive_turn(
    client: &Client,
    incoming: &mut UnboundedReceiver<Incoming>,
    thread_id: &str,
    text: &str,
    bots: &[BotRoute],
) -> Result<(TurnOutcome, Vec<Value>)> {
    struct Active {
        outcome: TurnOutcome,
        answer: Option<RequestId>,
    }
    let mut active: HashMap<String, Active> = HashMap::new();
    let mut tool_calls = Vec::new();
    client
        .turn_start(&TurnStartParams::text(thread_id, text))
        .await
        .map_err(|e| format!("turn/start: {e}"))?;
    active.insert(
        thread_id.to_owned(),
        Active {
            outcome: TurnOutcome::default(),
            answer: None,
        },
    );
    let deadline = Instant::now() + TURN_TIMEOUT;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let event = match tokio::time::timeout(remaining, incoming.recv()).await {
            Err(_) => return Err("turn timed out".into()),
            Ok(None) => return Err("the app server exited during the turn".into()),
            Ok(Some(event)) => event,
        };
        match event {
            Incoming::Notification(ServerNotification::AgentMessageDelta(delta)) => {
                if let Some(turn) = active.get_mut(&delta.thread_id) {
                    turn.outcome.deltas += 1;
                }
            }
            Incoming::Notification(ServerNotification::ItemCompleted(item)) => {
                if let Some(turn) = active.get_mut(&item.thread_id) {
                    if let ThreadItem::AgentMessage { text, .. } = &item.item {
                        turn.outcome.text.push_str(text);
                    }
                    turn.outcome.items.push(item.item);
                }
            }
            Incoming::Notification(ServerNotification::TurnCompleted(done)) => {
                let Some(mut turn) = active.remove(&done.thread_id) else {
                    continue;
                };
                turn.outcome.status = Some(done.turn.status);
                turn.outcome.error = done.turn.error.map(|e| e.message);
                if done.thread_id == thread_id {
                    return Ok((turn.outcome, tool_calls));
                }
                if let Some(id) = turn.answer {
                    let ok = turn.outcome.status == Some(TurnStatus::Completed);
                    let reply = if ok {
                        turn.outcome.text.clone()
                    } else {
                        format!("The Bot could not answer: {:?}", turn.outcome.error)
                    };
                    client
                        .respond(id, &DynamicToolCallResponse::text(ok, reply))
                        .await
                        .map_err(|e| e.to_string())?;
                }
            }
            Incoming::Request(request) => match request.kind {
                ServerRequestKind::DynamicToolCall(call) if call.tool == "message_bot" => {
                    let target = call.arguments["bot"].as_str().unwrap_or_default();
                    let message = call.arguments["message"].as_str().unwrap_or_default();
                    tool_calls.push(json!({"bot": target, "message_chars": message.len()}));
                    let route = bots.iter().find(|b| {
                        b.name.eq_ignore_ascii_case(target) && b.thread_id != call.thread_id
                    });
                    match route {
                        Some(route) => {
                            client
                                .turn_start(&TurnStartParams::text(&route.thread_id, message))
                                .await
                                .map_err(|e| format!("turn/start for {target}: {e}"))?;
                            active.insert(
                                route.thread_id.clone(),
                                Active {
                                    outcome: TurnOutcome::default(),
                                    answer: Some(request.id),
                                },
                            );
                        }
                        None => {
                            client
                                .respond(
                                    request.id,
                                    &DynamicToolCallResponse::text(
                                        false,
                                        format!("No Bot named {target}"),
                                    ),
                                )
                                .await
                                .map_err(|e| e.to_string())?;
                        }
                    }
                }
                ServerRequestKind::CommandExecutionApproval(_)
                | ServerRequestKind::FileChangeApproval(_) => {
                    client
                        .respond(
                            request.id,
                            &ApprovalResponse {
                                decision: ApprovalDecision::Decline,
                            },
                        )
                        .await
                        .map_err(|e| e.to_string())?;
                }
                _ => {
                    client
                        .respond_error(request.id, RpcError::method_not_found("unsupported"))
                        .await
                        .map_err(|e| e.to_string())?;
                }
            },
            Incoming::MalformedRequest { id, method, .. } => {
                client
                    .respond_error(id, RpcError::method_not_found(&method))
                    .await
                    .map_err(|e| e.to_string())?;
            }
            _ => {}
        }
    }
}

fn thread_params(context: &Context, user: &str, instructions: &str) -> ThreadStartParams {
    ThreadStartParams {
        cwd: Some(context.layout.workspace(user).display().to_string()),
        approval_policy: Some(AskForApproval::Never),
        permissions: Some(PROFILE.into()),
        developer_instructions: Some(instructions.into()),
        ephemeral: Some(false),
        ..ThreadStartParams::default()
    }
}

async fn chat(context: &Context, args: &Args) -> Result<Value> {
    let user = args.required("user")?;
    let phase = args.required("phase")?;
    let state_path = context.layout.state(user).join("chat.json");
    let (server, mut incoming) = context.start(user).await?;
    let client = server.client.clone();
    match phase {
        "start" => {
            let token = format!("bokkie-ready-{}", nonce());
            let started = client
                .thread_start(&thread_params(
                    context,
                    user,
                    "You are a Phase 0 qualification probe. Answer briefly and do not run commands.",
                ))
                .await
                .map_err(|e| format!("thread/start: {e}"))?;
            let thread_id = started.thread.id.clone();
            let (outcome, _) = drive_turn(
                &client,
                &mut incoming,
                &thread_id,
                &format!("Reply with exactly this token and nothing else: {token}"),
                &[],
            )
            .await?;
            std::fs::write(
                &state_path,
                json!({"thread_id": thread_id, "token": token}).to_string(),
            )
            .map_err(|e| e.to_string())?;
            Ok(json!({
                "step": "chat-start", "user": user,
                "pass": outcome.status == Some(TurnStatus::Completed)
                    && outcome.deltas > 0 && outcome.text.contains(&token),
                "thread": {"id": thread_id, "ephemeral": started.thread.ephemeral,
                           "path_present": started.thread.path.is_some()},
                "model": started.model,
                "active_permission_profile": started.active_permission_profile,
                "sandbox": started.sandbox,
                "approval_policy": started.approval_policy,
                "turn": {"status": outcome.status, "deltas": outcome.deltas,
                         "token_echoed": outcome.text.contains(&token), "error": outcome.error},
            }))
        }
        "resume" => {
            let state: Value = std::fs::read_to_string(&state_path)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .ok_or("no chat state: run --phase start first")?;
            let thread_id = state["thread_id"].as_str().unwrap_or_default().to_owned();
            let token = state["token"].as_str().unwrap_or_default().to_owned();
            let resumed = client
                .thread_resume(&ThreadResumeParams {
                    thread_id: thread_id.clone(),
                    permissions: Some(PROFILE.into()),
                    approval_policy: Some(AskForApproval::Never),
                    ..ThreadResumeParams::default()
                })
                .await
                .map_err(|e| format!("thread/resume: {e}"))?;
            let (outcome, _) = drive_turn(
                &client,
                &mut incoming,
                &thread_id,
                "What exact token did you reply with in your previous message? Reply with only that token.",
                &[],
            )
            .await?;
            Ok(json!({
                "step": "chat-resume", "user": user,
                "pass": resumed.thread.id == thread_id
                    && outcome.status == Some(TurnStatus::Completed)
                    && outcome.text.contains(&token),
                "resumed_turns": resumed.thread.turns.len(),
                "active_permission_profile": resumed.active_permission_profile,
                "turn": {"status": outcome.status, "deltas": outcome.deltas,
                         "token_recalled": outcome.text.contains(&token), "error": outcome.error},
            }))
        }
        other => Err(format!("unknown phase {other:?}")),
    }
}

/// The "message another Bot" route through experimental `dynamicTools`.
async fn tool(context: &Context, args: &Args) -> Result<Value> {
    let user = args.required("user")?;
    let (server, mut incoming) = context.start(user).await?;
    let client = server.client.clone();
    let secret = format!("paprika-{}", nonce());
    let chef = client
        .thread_start(&thread_params(
            context,
            user,
            &format!(
                "You are the Bot named Chef. When anyone asks for the secret word, reply with \
                 exactly {secret} and nothing else."
            ),
        ))
        .await
        .map_err(|e| format!("thread/start Chef: {e}"))?;
    let mut planner_params = thread_params(
        context,
        user,
        "You are the Bot named Planner. You can message the user's other Bots with the \
         message_bot tool. Do not run shell commands.",
    );
    planner_params.dynamic_tools = Some(vec![DynamicToolSpec::Function {
        name: "message_bot".into(),
        description: "Send a message to another of this user's Bots and wait for its reply.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "bot": {"type": "string", "description": "The other Bot's name."},
                "message": {"type": "string", "description": "The message to send."},
            },
            "required": ["bot", "message"],
            "additionalProperties": false,
        }),
    }]);
    let planner = client
        .thread_start(&planner_params)
        .await
        .map_err(|e| format!("thread/start Planner: {e}"))?;
    let bots = [
        BotRoute {
            name: "Chef".into(),
            thread_id: chef.thread.id.clone(),
        },
        BotRoute {
            name: "Planner".into(),
            thread_id: planner.thread.id.clone(),
        },
    ];
    let (outcome, calls) = drive_turn(
        &client,
        &mut incoming,
        &planner.thread.id,
        "Ask the Bot named Chef for the secret word using message_bot, then reply with only \
         the word Chef gave you.",
        &bots,
    )
    .await?;
    let tool_items = outcome
        .items
        .iter()
        .filter(|i| matches!(i, ThreadItem::DynamicToolCall { .. }))
        .count();
    Ok(json!({
        "step": "tool", "user": user, "route": "dynamicTools",
        "pass": outcome.status == Some(TurnStatus::Completed)
            && !calls.is_empty() && outcome.text.contains(&secret),
        "tool_calls": calls,
        "dynamic_tool_items": tool_items,
        "reply_has_secret": outcome.text.contains(&secret),
        "turn": {"status": outcome.status, "error": outcome.error},
    }))
}

// ---------------------------------------------------------------------------
// Sandbox invariants

fn outer_peers() -> Vec<PeerSpec> {
    visible_pids()
        .into_iter()
        .map(|(pid, comm)| PeerSpec {
            pid,
            comm,
            start_time: bokkie_qualify::start_time(pid),
        })
        .collect()
}

fn plan(context: &Context, args: &Args, user: &str) -> Result<ProbeSpec> {
    let net = args.get("net-expect").unwrap_or("deny");
    let expect = match net {
        "allow" => Expect::Allow,
        "deny" => Expect::Deny,
        other => return Err(format!("--net-expect must be allow or deny, not {other}")),
    };
    let target = args.get("connect").unwrap_or("1.1.1.1:443").to_owned();
    let all_users = users(args);
    Ok(probe_spec(&PlanInput {
        layout: &context.layout,
        user,
        users: &all_users,
        peers: outer_peers(),
        outer_pid1_cmdline: read_cmdline(Path::new("/proc/1/cmdline")).unwrap_or_default(),
        connect: Some((target, expect)),
    }))
}

/// Runs the probe through `codex sandbox` or the app server's `command/exec`.
async fn run_sandboxed(
    context: &Context,
    user: &str,
    via: &str,
    profile: &str,
    argv: Vec<String>,
    timeout: Duration,
) -> Result<(i32, String, String)> {
    match via {
        "cli" => {
            let mut command = tokio::process::Command::new(&context.codex);
            command
                .arg("sandbox")
                .args(["-P", profile, "-C"])
                .arg(context.layout.workspace(user))
                .arg("--")
                .args(&argv)
                .env_clear()
                .env("CODEX_HOME", context.layout.codex_home(user))
                .env("HOME", context.layout.home(user))
                .env("PATH", std::env::var("PATH").unwrap_or_default())
                .current_dir(context.layout.workspace(user))
                .kill_on_drop(true);
            let output = tokio::time::timeout(timeout, command.output())
                .await
                .map_err(|_| "codex sandbox timed out".to_owned())?
                .map_err(|e| e.to_string())?;
            Ok((
                output.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&output.stdout).into_owned(),
                String::from_utf8_lossy(&output.stderr).into_owned(),
            ))
        }
        "exec" => {
            let (server, _incoming) = context.start(user).await?;
            let response = tokio::time::timeout(
                timeout,
                server.client.command_exec(&CommandExecParams {
                    command: argv,
                    cwd: Some(context.layout.workspace(user).display().to_string()),
                    permission_profile: Some(profile.into()),
                    timeout_ms: Some(timeout.as_millis() as i64),
                    process_id: None,
                }),
            )
            .await
            .map_err(|_| "command/exec timed out".to_owned())?
            .map_err(|e| format!("command/exec: {e}"))?;
            Ok((response.exit_code, response.stdout, response.stderr))
        }
        other => Err(format!("--via must be cli or exec, not {other}")),
    }
}

async fn sandbox(context: &Context, args: &Args) -> Result<Value> {
    let user = args.required("user")?;
    let via = args.get("via").unwrap_or("cli");
    let profile = args.get("profile").unwrap_or(PROFILE);
    let spec = plan(context, args, user)?;
    let nonce = nonce();
    let argv = vec![
        probe_path(),
        "check".into(),
        "--spec".into(),
        serde_json::to_string(&spec).map_err(|e| e.to_string())?,
        "--nonce".into(),
        nonce.clone(),
    ];
    let (code, stdout, stderr) =
        run_sandboxed(context, user, via, profile, argv, Duration::from_secs(60)).await?;
    let report: Option<ProbeReport> = stdout
        .lines()
        .rev()
        .find_map(|line| serde_json::from_str(line).ok());
    let Some(report) = report else {
        return Ok(json!({
            "step": "sandbox", "user": user, "via": via, "profile": profile, "pass": false,
            "sandbox_started": false, "exit_code": code,
            "stderr_tail": tail(&stderr, 20),
        }));
    };
    let mut findings = evaluate(&report, &spec);
    findings.extend(shared_write_findings(&report, &nonce));
    let pass = findings.iter().all(|f| f.pass);
    Ok(json!({
        "step": "sandbox", "user": user, "via": via, "profile": profile,
        "pass": pass, "sandbox_started": true, "exit_code": code,
        "failed": findings.iter().filter(|f| !f.pass).collect::<Vec<_>>(),
        "findings": findings,
        "identity": report.identity,
        "pid1_cmdline": report.pid1_cmdline,
        "visible_pids": report.visible_pids,
        "proc_mounts": report.proc_mounts,
        "stderr_tail": tail(&stderr, 10),
    }))
}

/// A write to `/tmp` or `/dev/shm` passes when denied, or when the file is
/// not visible outside the sandbox (private storage). It fails when another
/// user's sandbox or the gateway could see it.
fn shared_write_findings(report: &ProbeReport, nonce: &str) -> Vec<Finding> {
    report
        .writes
        .iter()
        .filter(|w| w.label.starts_with(SHARED_WRITE_PREFIX))
        .map(|write| {
            let outside = Path::new(&write.path).join(format!(".bokkie-probe-{nonce}"));
            let visible = outside.exists();
            if visible {
                let _ = std::fs::remove_file(&outside);
            }
            Finding {
                invariant: "inv2".into(),
                label: format!("inv2:{}", write.label),
                pass: !visible,
                detail: format!(
                    "{}: write {}, visible outside the sandbox: {visible}",
                    write.path,
                    if write.outcome.ok {
                        "succeeded"
                    } else {
                        "denied"
                    }
                ),
            }
        })
        .collect()
}

fn tail(text: &str, lines: usize) -> Vec<&str> {
    let all: Vec<&str> = text.lines().collect();
    all[all.len().saturating_sub(lines)..].to_vec()
}

/// Records which Bubblewrap binary Codex actually runs.
async fn observe_bwrap(context: &Context, args: &Args) -> Result<Value> {
    let user = args.required("user")?;
    let profile = args.get("profile").unwrap_or(PROFILE);
    let task = {
        let codex = context.codex.clone();
        let layout = context.layout.clone();
        let user = user.to_owned();
        let profile = profile.to_owned();
        tokio::spawn(async move {
            let ctx = Context { layout, codex };
            run_sandboxed(
                &ctx,
                &user,
                "cli",
                &profile,
                vec!["/bin/sleep".into(), "3".into()],
                Duration::from_secs(30),
            )
            .await
        })
    };
    let mut seen = std::collections::BTreeMap::new();
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        for (pid, comm) in visible_pids() {
            if comm == "bwrap"
                && let Ok(exe) = std::fs::read_link(format!("/proc/{pid}/exe"))
            {
                seen.insert(exe.display().to_string(), pid);
            }
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let result = task.await.map_err(|e| e.to_string())?;
    let versions: Vec<Value> = seen
        .keys()
        .map(|exe| {
            let version = std::process::Command::new(exe)
                .arg("--version")
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
                .unwrap_or_else(|e| format!("unavailable: {e}"));
            json!({"exe": exe, "version": version})
        })
        .collect();
    Ok(json!({
        "step": "bwrap", "user": user,
        "pass": !seen.is_empty(),
        "bwrap_seen": versions,
        "sandbox_exit": result.map(|(code, _, err)| json!({"code": code, "stderr_tail": tail(&err, 5)})).unwrap_or_else(|e| json!(e)),
    }))
}

// ---------------------------------------------------------------------------
// Invariant 6: no orphaned sandbox processes

/// Every process tied to one lingering run: the sleeper, the probe that
/// started it, and any surviving `codex-linux-sandbox` or `bwrap` whose
/// command line names it. The harness itself is excluded.
fn sleepers(seconds: &str) -> Vec<u32> {
    let me = std::process::id();
    visible_pids()
        .into_iter()
        .filter(|(pid, comm)| {
            *pid != me
                && comm != "bokkie-qualify"
                && read_cmdline(Path::new(&format!("/proc/{pid}/cmdline"))).is_some_and(|c| {
                    c == format!("sleep {seconds}") || c.contains(&format!("linger {seconds}"))
                })
        })
        .map(|(pid, _)| pid)
        .collect()
}

fn sleeper_seconds() -> String {
    // Unique enough to identify this run's sleeper in the process table.
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    format!("{}", 900_000 + nanos % 90_000)
}

async fn wait_for_sleeper(seconds: &str) -> bool {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if !sleepers(seconds).is_empty() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
}

async fn orphans(context: &Context, args: &Args) -> Result<Value> {
    let user = args.required("user")?;
    let via = args.get("via").unwrap_or("cli");
    let profile = args.get("profile").unwrap_or(PROFILE);
    let seconds = sleeper_seconds();
    let probe = probe_path();
    let argv = [probe.as_str(), "linger", seconds.as_str()];
    let (started, killed) = match via {
        "cli" => {
            let mut child = tokio::process::Command::new(&context.codex)
                .arg("sandbox")
                .args(["-P", profile, "-C"])
                .arg(context.layout.workspace(user))
                .arg("--")
                .args(argv)
                .env_clear()
                .env("CODEX_HOME", context.layout.codex_home(user))
                .env("HOME", context.layout.home(user))
                .env("PATH", std::env::var("PATH").unwrap_or_default())
                .current_dir(context.layout.workspace(user))
                .stdout(std::process::Stdio::null())
                .spawn()
                .map_err(|e| e.to_string())?;
            let started = wait_for_sleeper(&seconds).await;
            let _ = child.start_kill();
            let _ = child.wait().await;
            (started, "codex sandbox (SIGKILL)")
        }
        "exec" => {
            let (mut server, _incoming) = context.start(user).await?;
            let client = server.client.clone();
            let argv: Vec<String> = argv.iter().map(|s| (*s).to_owned()).collect();
            let workspace = context.layout.workspace(user).display().to_string();
            let profile = profile.to_owned();
            let _exec = tokio::spawn(async move {
                client
                    .command_exec(&CommandExecParams {
                        command: argv,
                        cwd: Some(workspace),
                        permission_profile: Some(profile),
                        timeout_ms: Some(600_000),
                        process_id: Some("orphan-check".into()),
                    })
                    .await
            });
            let started = wait_for_sleeper(&seconds).await;
            let _ = server.child.start_kill();
            let _ = server.child.wait().await;
            (started, "codex app-server (SIGKILL)")
        }
        other => return Err(format!("--via must be cli or exec, not {other}")),
    };
    tokio::time::sleep(Duration::from_secs(3)).await;
    let survivors = describe(&sleepers(&seconds));
    kill_all(&sleepers(&seconds));
    Ok(json!({
        "step": "orphans", "user": user, "via": via, "profile": profile,
        "pass": started && survivors.is_empty(),
        "sleeper_started": started, "killed": killed,
        "survivors": survivors,
    }))
}

/// Starts an app server running a lingering sandboxed command, then waits to
/// be killed. The driver kills this process (the stand-in gateway) with
/// SIGKILL and then runs `find-sleeper`.
async fn orphan_hold(context: &Context, args: &Args) -> Result<Value> {
    let user = args.required("user")?;
    let seconds = args.required("seconds")?.to_owned();
    let profile = args.get("profile").unwrap_or(PROFILE).to_owned();
    let (server, _incoming) = context.start(user).await?;
    let client = server.client.clone();
    let workspace = context.layout.workspace(user).display().to_string();
    let argv = vec![probe_path(), "linger".into(), seconds.clone()];
    tokio::spawn(async move {
        client
            .command_exec(&CommandExecParams {
                command: argv,
                cwd: Some(workspace),
                permission_profile: Some(profile),
                timeout_ms: Some(600_000),
                process_id: Some("orphan-hold".into()),
            })
            .await
    });
    if !wait_for_sleeper(&seconds).await {
        return Err("the sleeper did not start".into());
    }
    std::fs::write(
        context.layout.state(user).join("orphan-hold.pid"),
        std::process::id().to_string(),
    )
    .map_err(|e| e.to_string())?;
    log(format!(
        "holding: gateway pid {}, app-server pid {:?}",
        std::process::id(),
        server.pid()
    ));
    idle().await?;
    Ok(json!({"step": "orphan-hold", "pass": true}))
}

/// Names surviving processes for the record.
fn describe(pids: &[u32]) -> Vec<Value> {
    pids.iter()
        .map(|pid| {
            let cmdline =
                read_cmdline(Path::new(&format!("/proc/{pid}/cmdline"))).unwrap_or_default();
            json!({"pid": pid, "cmdline": cmdline.chars().take(80).collect::<String>()})
        })
        .collect()
}

/// Cleans up so a failed check does not leave work running.
fn kill_all(pids: &[u32]) {
    for pid in pids {
        let _ = std::process::Command::new("/bin/sh")
            .args(["-c", &format!("kill -KILL {pid}")])
            .status();
    }
}

fn find_sleeper(args: &Args) -> Result<Value> {
    let seconds = args.required("seconds")?;
    let pids = sleepers(seconds);
    let found = describe(&pids);
    if args.flag("clean") {
        kill_all(&pids);
    }
    let expect_none = args.flag("expect-none");
    let app_servers: Vec<u32> = visible_pids()
        .into_iter()
        .filter(|(pid, _)| {
            read_cmdline(Path::new(&format!("/proc/{pid}/cmdline")))
                .is_some_and(|c| c.contains("app-server"))
        })
        .map(|(pid, _)| pid)
        .collect();
    Ok(json!({
        "step": "find-sleeper",
        "pass": if expect_none { found.is_empty() } else { !found.is_empty() },
        "sleepers": found,
        "app_servers_running": app_servers,
    }))
}

/// Keeps a live app server for `user`, so another user's sandbox has a real
/// process to try to reach.
async fn hold(context: &Context, args: &Args) -> Result<Value> {
    let user = args.required("user")?;
    let (server, _incoming) = context.start(user).await?;
    log(format!(
        "holding app server for {user}: pid {:?}",
        server.pid()
    ));
    idle().await?;
    drop(server);
    Ok(json!({"step": "hold", "pass": true}))
}

/// A real turn that executes the probe through the model's shell tool.
async fn turn_exec(context: &Context, args: &Args) -> Result<Value> {
    let user = args.required("user")?;
    let spec = plan(context, args, user)?;
    let nonce = nonce();
    let spec_file = context.layout.workspace(user).join("probe-spec.json");
    std::fs::write(
        &spec_file,
        serde_json::to_string(&spec).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let (server, mut incoming) = context.start(user).await?;
    let client = server.client.clone();
    let started = client
        .thread_start(&thread_params(
            context,
            user,
            "You are a Phase 0 qualification probe. Run exactly the command you are given, \
             once, without modification, then reply 'done'.",
        ))
        .await
        .map_err(|e| format!("thread/start: {e}"))?;
    let command = format!(
        "{} check --spec-file probe-spec.json --nonce {nonce}",
        probe_path()
    );
    let (outcome, _) = drive_turn(
        &client,
        &mut incoming,
        &started.thread.id,
        &format!("Run this shell command in the current directory: {command}"),
        &[],
    )
    .await?;
    let _ = std::fs::remove_file(&spec_file);
    let executions: Vec<&ThreadItem> = outcome
        .items
        .iter()
        .filter(|i| matches!(i, ThreadItem::CommandExecution { .. }))
        .collect();
    let report: Option<ProbeReport> = executions.iter().find_map(|item| match item {
        ThreadItem::CommandExecution {
            aggregated_output: Some(output),
            ..
        } => output
            .lines()
            .rev()
            .find_map(|line| serde_json::from_str(line).ok()),
        _ => None,
    });
    let Some(report) = report else {
        return Ok(json!({
            "step": "turn-exec", "user": user, "pass": false,
            "commands": executions.len(),
            "turn": {"status": outcome.status, "error": outcome.error},
            "executions": executions,
        }));
    };
    let mut findings = evaluate(&report, &spec);
    findings.extend(shared_write_findings(&report, &nonce));
    Ok(json!({
        "step": "turn-exec", "user": user,
        "pass": findings.iter().all(|f| f.pass),
        "active_permission_profile": started.active_permission_profile,
        "failed": findings.iter().filter(|f| !f.pass).collect::<Vec<_>>(),
        "findings": findings,
        "pid1_cmdline": report.pid1_cmdline,
        "visible_pids": report.visible_pids,
        "turn": {"status": outcome.status, "error": outcome.error},
    }))
}
