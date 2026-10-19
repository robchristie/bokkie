//! A scripted, in-process stand-in for `codex app-server`.
//!
//! By default it behaves like the pinned app server with the gateway's
//! configuration: it reports the `bokkie` profile, Bokkie's catalog and every
//! tool disabled. Tests break one property at a time to prove the gateway
//! refuses, and inject server requests and notifications.

#![allow(dead_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use base64::Engine;
use bokkie_codex::{Client, Incoming};
use bokkie_gateway::codex::config::DISABLED_FEATURES;
use bokkie_gateway::codex::{Launcher, Owner};
use bokkie_protocol::UserId;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

pub const MODEL: &str = "gpt-6.1-sol";

/// The bundled catalog the tests start from.
pub fn bundled_catalog() -> Value {
    json!({"models": [
        {"slug": MODEL, "display_name": "GPT-6.1-Sol", "apply_patch_tool_type": "freeform",
         "tool_mode": "code_mode_only", "multi_agent_version": "v2",
         "experimental_supported_tools": ["clock"], "supports_search_tool": true,
         "shell_type": "unified_exec"},
    ]})
}

/// One request the fake received.
#[derive(Debug, Clone)]
pub struct Received {
    pub owner: Owner,
    pub method: String,
    pub params: Value,
}

impl Received {
    /// The user whose app server received it; panics for the household's.
    pub fn user(&self) -> UserId {
        match self.owner {
            Owner::User(user) => user,
            Owner::Household => panic!("the household's app server received {}", self.method),
        }
    }
}

/// An `auth.json` whose access token expires `expires_in` seconds from now,
/// by the system clock. Synthetic: nothing reads it but the gateway's
/// expiry check.
pub fn household_auth(expires_in: i64) -> String {
    let exp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
        + expires_in;
    let encode = |v: Value| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(v.to_string());
    let access = format!(
        "{}.{}.synthetic",
        encode(json!({"alg": "none"})),
        encode(json!({"exp": exp}))
    );
    json!({"auth_mode": "chatgpt", "OPENAI_API_KEY": null,
           "tokens": {"id_token": access, "access_token": access,
                      "refresh_token": "synthetic", "account_id": "acct"}})
    .to_string()
}

/// Writes the household's sign-in under `data_dir`, as a device-code
/// sign-in would.
pub fn sign_in_household(data_dir: &Path, expires_in: i64) {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    let codex_home = data_dir.join("household/codex");
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&codex_home)
        .unwrap();
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(codex_home.join("auth.json"))
        .unwrap();
    std::io::Write::write_all(&mut file, household_auth(expires_in).as_bytes()).unwrap();
}

/// What the fake does, shared with the test.
#[derive(Debug)]
pub struct Behaviour {
    /// The permission profile that thread responses report.
    pub profile: String,
    /// A configuration key path and the value to report there instead.
    pub config_override: Option<(Vec<String>, Value)>,
    /// The model that `model/list` reports besides the catalog's.
    pub extra_model: Option<String>,
    /// The text of each Bot reply, streamed in these pieces.
    pub reply: Vec<String>,
    /// Whether turns finish on their own after streaming the reply.
    pub finish_turns: bool,
    /// Whether a turn streams and finishes before `turn/start` answers.
    pub finish_before_answer: bool,
    /// Whether `turn/steer` fails, as it does once the turn has ended.
    pub steer_fails: bool,
    /// Whether `thread/start` waits for a permit from
    /// [`FakeCodex::thread_starts`] before it answers.
    pub hold_thread_starts: bool,
    /// App servers whose `thread/start` waits as `hold_thread_starts` makes
    /// every one wait.
    pub hold_thread_starts_of: Vec<Owner>,
    /// Whether `turn/start` starts the turn but waits for a permit from
    /// [`FakeCodex::turn_answers`] before it answers.
    pub hold_turn_answers: bool,
    /// Whether `turn/start` answers with an error, starting nothing.
    pub reject_turns: bool,
    /// Whether the app server starts a turn and then exits before
    /// answering `turn/start`.
    pub hang_up_on_turn: bool,
    /// Whether `turn/start` is never answered.
    pub hang_turns: bool,
    /// Whether `turn/start` starts the turn but answers with a line the
    /// client cannot decode.
    pub garble_turn_answers: bool,
    /// Whether a turn reports its tokens while it is still streaming.
    pub usage_mid_turn: bool,
    pub signed_in: bool,
    /// Whether the household's refreshes fail, leaving the file as it is.
    pub refresh_fails: bool,
    /// How long a refreshed household token lasts, in seconds.
    pub refreshed_lifetime: i64,
    /// The `account/rateLimits/read` result.
    pub rate_limits: Value,
    pub received: Vec<Received>,
    /// Each thread's token total, which grows by 120 per turn.
    pub tokens: HashMap<String, i64>,
    /// Turns that have finished; Codex refuses to interrupt them.
    pub finished: std::collections::HashSet<String>,
    /// Every turn started, by its thread and turn ids, in order.
    pub turns: Vec<(String, String)>,
    next_id: u64,
}

impl Default for Behaviour {
    fn default() -> Self {
        Self {
            profile: "bokkie".into(),
            config_override: None,
            extra_model: None,
            reply: vec!["Hello".into(), " there".into()],
            finish_turns: true,
            finish_before_answer: false,
            steer_fails: false,
            hold_thread_starts: false,
            hold_thread_starts_of: Vec::new(),
            hold_turn_answers: false,
            reject_turns: false,
            hang_up_on_turn: false,
            hang_turns: false,
            garble_turn_answers: false,
            usage_mid_turn: false,
            signed_in: true,
            refresh_fails: false,
            refreshed_lifetime: 3600,
            rate_limits: json!({
                "rateLimits": {"limitId": "codex", "limitName": null, "planType": "pro",
                    "primary": {"usedPercent": 12, "windowDurationMins": 10080, "resetsAt": 1_800_600_000},
                    "secondary": null, "rateLimitReachedType": null},
                "rateLimitsByLimitId": null,
            }),
            received: Vec::new(),
            tokens: HashMap::new(),
            finished: Default::default(),
            turns: Vec::new(),
            next_id: 0,
        }
    }
}

#[derive(Clone)]
pub struct FakeCodex {
    pub behaviour: Arc<Mutex<Behaviour>>,
    /// Each app server's outgoing line channel, to inject messages.
    outlets: Arc<Mutex<HashMap<Owner, mpsc::UnboundedSender<Value>>>>,
    /// How many app servers each owner has had.
    pub starts: Arc<Mutex<HashMap<Owner, usize>>>,
    /// Permits for held `thread/start` requests to answer, one each.
    pub thread_starts: Arc<tokio::sync::Semaphore>,
    /// Permits for held `turn/start` answers, one each.
    pub turn_answers: Arc<tokio::sync::Semaphore>,
}

impl Default for FakeCodex {
    fn default() -> Self {
        Self {
            behaviour: Default::default(),
            outlets: Default::default(),
            starts: Default::default(),
            thread_starts: Arc::new(tokio::sync::Semaphore::new(0)),
            turn_answers: Arc::new(tokio::sync::Semaphore::new(0)),
        }
    }
}

impl FakeCodex {
    pub fn with<R>(&self, f: impl FnOnce(&mut Behaviour) -> R) -> R {
        f(&mut self.behaviour.lock().unwrap())
    }

    pub fn received(&self, method: &str) -> Vec<Received> {
        self.with(|b| {
            b.received
                .iter()
                .filter(|r| r.method == method)
                .cloned()
                .collect()
        })
    }

    pub fn starts(&self, user: UserId) -> usize {
        self.owner_starts(Owner::User(user))
    }

    pub fn owner_starts(&self, owner: Owner) -> usize {
        self.starts
            .lock()
            .unwrap()
            .get(&owner)
            .copied()
            .unwrap_or(0)
    }

    /// Sends a raw message from the user's app server to the gateway.
    pub fn inject(&self, user: UserId, message: Value) {
        self.inject_into(Owner::User(user), message);
    }

    /// Sends a raw message from an app server to the gateway.
    pub fn inject_into(&self, owner: Owner, message: Value) {
        let outlet = self.outlets.lock().unwrap().get(&owner).cloned();
        outlet.expect("the app server runs").send(message).unwrap();
    }

    /// Ends the user's app server, as if it crashed.
    pub fn crash(&self, user: UserId) {
        self.crash_owner(Owner::User(user));
    }

    pub fn crash_owner(&self, owner: Owner) {
        self.outlets.lock().unwrap().remove(&owner);
    }

    pub fn launcher(&self, data_dir: &Path, catalog: &Path) -> Launcher {
        let fake = self.clone();
        let data_dir = data_dir.to_owned();
        let catalog = catalog.to_owned();
        Launcher::InProcess(Arc::new(move |owner, paths| {
            fake.connect(
                owner,
                data_dir.clone(),
                catalog.clone(),
                paths.workspace.clone(),
                paths.codex_home.clone(),
            )
        }))
    }

    fn connect(
        &self,
        owner: Owner,
        data_dir: PathBuf,
        catalog: PathBuf,
        workspace: PathBuf,
        codex_home: PathBuf,
    ) -> (Client, mpsc::UnboundedReceiver<Incoming>) {
        let user = owner;
        *self.starts.lock().unwrap().entry(owner).or_default() += 1;
        let (client_side, server_side) = tokio::io::duplex(1 << 20);
        let (client_read, client_write) = tokio::io::split(client_side);
        let (server_read, mut server_write) = tokio::io::split(server_side);
        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Value>();
        self.outlets.lock().unwrap().insert(user, out_tx.clone());
        let outlets = self.outlets.clone();
        tokio::spawn(async move {
            while let Some(value) = out_rx.recv().await {
                let mut line = value.to_string();
                line.push('\n');
                if server_write.write_all(line.as_bytes()).await.is_err() {
                    break;
                }
            }
        });
        let fake = self.clone();
        let context = Context {
            owner,
            data_dir,
            catalog,
            workspace,
            codex_home,
        };
        tokio::spawn(async move {
            let mut lines = BufReader::new(server_read).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                // A crash drops the outlet; stop answering.
                if !outlets.lock().unwrap().contains_key(&user) {
                    break;
                }
                let message: Value = serde_json::from_str(&line).unwrap();
                fake.handle(&context, &out_tx, message);
                // The handler hung up.
                if !outlets.lock().unwrap().contains_key(&user) {
                    break;
                }
            }
        });
        Client::new(client_read, client_write)
    }

    fn handle(&self, context: &Context, out: &mpsc::UnboundedSender<Value>, message: Value) {
        let Some(method) = message["method"].as_str() else {
            // A response to an injected server request; record it.
            self.with(|b| {
                b.received.push(Received {
                    owner: context.owner,
                    method: "<response>".into(),
                    params: message.clone(),
                })
            });
            return;
        };
        let params = message["params"].clone();
        self.with(|b| {
            b.received.push(Received {
                owner: context.owner,
                method: method.into(),
                params: params.clone(),
            })
        });
        let Some(id) = message.get("id").cloned() else {
            return; // A notification such as `initialized`.
        };
        let result = match method {
            "initialize" => json!({
                "codexHome": context.data_dir.display().to_string(),
                "platformFamily": "unix", "platformOs": "linux", "userAgent": "fake",
            }),
            "config/read" => json!({"config": self.config(context), "origins": {}}),
            "model/list" => {
                let mut models = vec![model_entry(MODEL)];
                if let Some(extra) = self.with(|b| b.extra_model.clone()) {
                    models.push(model_entry(&extra));
                }
                json!({"data": models, "nextCursor": null})
            }
            "thread/start" => {
                let thread = self.with(|b| {
                    b.next_id += 1;
                    let user = match context.owner {
                        Owner::User(user) => user.to_string(),
                        Owner::Household => "household".into(),
                    };
                    format!("thr-{user}-{}", b.next_id)
                });
                let session = self.session(&thread, &params);
                if self.with(|b| {
                    b.hold_thread_starts || b.hold_thread_starts_of.contains(&context.owner)
                }) {
                    let (gate, out) = (self.thread_starts.clone(), out.clone());
                    tokio::spawn(async move {
                        if let Ok(permit) = gate.acquire().await {
                            permit.forget();
                        }
                        let _ = out.send(json!({"id": id, "result": session}));
                    });
                    return;
                }
                session
            }
            "thread/resume" => {
                let id = params["threadId"].as_str().unwrap().to_owned();
                self.session(&id, &params)
            }
            "turn/start" if self.with(|b| b.reject_turns) => {
                let _ = out.send(json!({"id": id, "error": {
                    "code": -32600, "message": "turn refused"}}));
                return;
            }
            "turn/start" if self.with(|b| b.hang_turns) => return,
            "turn/start" if self.with(|b| b.garble_turn_answers) => {
                let thread = params["threadId"].as_str().unwrap();
                let _ = out.send(json!({"method": "turn/started", "params": {"threadId": thread,
                    "turn": {"id": "turn-garbled", "items": [], "status": "inProgress", "error": null}}}));
                let _ = out.send(
                    json!({"id": id, "result": {"turn": {"id": "turn-garbled"}}, "error": 5}),
                );
                return;
            }
            "turn/start" if self.with(|b| b.hang_up_on_turn) => {
                let turn = self.with(|b| {
                    b.next_id += 1;
                    format!("turn-{}", b.next_id)
                });
                let thread = params["threadId"].as_str().unwrap();
                let _ = out.send(
                    json!({"method": "turn/started", "params": {"threadId": thread,
                    "turn": {"id": turn, "items": [], "status": "inProgress", "error": null}}}),
                );
                self.crash_owner(context.owner);
                return;
            }
            "turn/start" => {
                let turn = self.with(|b| {
                    b.next_id += 1;
                    format!("turn-{}", b.next_id)
                });
                let thread = params["threadId"].as_str().unwrap().to_owned();
                self.with(|b| b.turns.push((thread.clone(), turn.clone())));
                if self.with(|b| b.finish_before_answer) {
                    for line in self.turn_lines(&thread, &turn) {
                        let _ = out.send(line);
                    }
                } else {
                    self.stream_turn(out.clone(), thread, turn.clone());
                }
                let answer = json!({"turn": {"id": turn, "items": [], "status": "inProgress", "error": null}});
                if self.with(|b| b.hold_turn_answers) {
                    let (gate, out) = (self.turn_answers.clone(), out.clone());
                    tokio::spawn(async move {
                        if let Ok(permit) = gate.acquire().await {
                            permit.forget();
                        }
                        let _ = out.send(json!({"id": id, "result": answer}));
                    });
                    return;
                }
                answer
            }
            "turn/steer" if self.with(|b| b.steer_fails) => {
                let _ = out.send(json!({"id": id, "error": {
                    "code": -32600, "message": "no active turn to steer"}}));
                return;
            }
            "turn/steer" => json!({"turnId": params["expectedTurnId"]}),
            "turn/interrupt"
                if self.with(|b| b.finished.contains(params["turnId"].as_str().unwrap_or(""))) =>
            {
                let _ = out.send(json!({"id": id, "error": {
                    "code": -32600, "message": "no active turn to interrupt"}}));
                return;
            }
            "turn/interrupt" => {
                let turn = params["turnId"].as_str().unwrap_or("").to_owned();
                self.with(|b| b.finished.insert(turn));
                let _ = out.send(json!({"method": "turn/completed", "params": {
                    "threadId": params["threadId"],
                    "turn": {"id": params["turnId"], "items": [], "status": "interrupted", "error": null},
                }}));
                json!({})
            }
            "account/read" => {
                if params["refreshToken"] == true
                    && context.owner == Owner::Household
                    && !self.with(|b| b.refresh_fails)
                {
                    // A refresh rotates the household's tokens in place.
                    let auth = context.codex_home.join("auth.json");
                    if auth.exists() {
                        let lifetime = self.with(|b| b.refreshed_lifetime);
                        std::fs::write(&auth, household_auth(lifetime)).unwrap();
                    }
                }
                let signed_in =
                    self.with(|b| b.signed_in) && context.codex_home.join("auth.json").exists();
                json!({
                    "account": if signed_in {
                        json!({"type": "chatgpt", "email": "owner@example.test", "planType": "pro"})
                    } else {
                        Value::Null
                    },
                    "requiresOpenaiAuth": true,
                })
            }
            "account/login/start" => {
                let login = self.with(|b| {
                    b.next_id += 1;
                    format!("login-{}", b.next_id)
                });
                json!({
                    "type": "chatgptDeviceCode", "loginId": login,
                    "userCode": "ABCD-EFGH", "verificationUrl": "https://auth.example.test/device",
                })
            }
            "account/login/cancel" => {
                // As Codex does: the cancelled sign-in then reports failure.
                let _ = out.send(json!({"id": id, "result": {"status": "canceled"}}));
                let _ = out.send(json!({"method": "account/login/completed", "params": {
                    "loginId": params["loginId"], "success": false,
                    "error": "Login was not completed"}}));
                return;
            }
            "account/logout" => {
                // Codex revokes the sign-in, then deletes `auth.json`: for a
                // link, the link alone.
                let _ = std::fs::remove_file(context.codex_home.join("auth.json"));
                json!({})
            }
            "account/rateLimits/read" => self.with(|b| b.rate_limits.clone()),
            _ => {
                let _ = out.send(json!({"id": id, "error": {"code": -32601, "message": "no"}}));
                return;
            }
        };
        let _ = out.send(json!({"id": id, "result": result}));
    }

    fn config(&self, context: &Context) -> Value {
        let mut features = serde_json::Map::new();
        for feature in DISABLED_FEATURES {
            features.insert((*feature).into(), json!(false));
        }
        let mut config = json!({
            "default_permissions": "bokkie",
            "approval_policy": "never",
            "web_search": "disabled",
            "model_catalog_json": context.catalog.display().to_string(),
            "features": features,
            "mcp_servers": {},
            "project_doc_max_bytes": 0,
            "cli_auth_credentials_store": "file",
            "profile": null,
            "permissions": {"bokkie": {
                "description": null, "extends": null,
                "filesystem": {
                    context.data_dir.display().to_string(): "deny",
                    ":minimal": "read", ":workspace_roots": "write",
                    "glob_scan_max_depth": null,
                },
                "network": {"enabled": false},
            }},
        });
        if let Some((path, value)) = self.with(|b| b.config_override.clone()) {
            let mut node = &mut config;
            for key in &path {
                node = &mut node[key.as_str()];
            }
            *node = value;
        }
        config
    }

    fn session(&self, thread_id: &str, params: &Value) -> Value {
        let profile = self.with(|b| b.profile.clone());
        let cwd = params["cwd"].clone();
        json!({
            "thread": {"id": thread_id, "sessionId": thread_id, "preview": "", "ephemeral": false,
                       "modelProvider": "openai", "cwd": cwd, "cliVersion": "0.163.0",
                       "createdAt": 0, "updatedAt": 0, "path": null, "turns": []},
            "model": MODEL, "modelProvider": "openai", "cwd": cwd,
            "approvalPolicy": params["approvalPolicy"],
            "sandbox": {"type": "workspaceWrite"},
            "activePermissionProfile": {"id": profile, "extends": null},
        })
    }

    /// Every message of a finished turn, in order.
    fn turn_lines(&self, thread: &str, turn: &str) -> Vec<Value> {
        let reply = self.with(|b| b.reply.clone());
        let item = format!("{turn}-msg");
        let mut lines = vec![
            json!({"method": "turn/started", "params": {"threadId": thread,
                "turn": {"id": turn, "items": [], "status": "inProgress", "error": null}}}),
            json!({"method": "item/started", "params": {"threadId": thread, "turnId": turn,
                "item": {"type": "agentMessage", "id": item, "text": ""}}}),
        ];
        for piece in &reply {
            lines.push(json!({"method": "item/agentMessage/delta", "params": {
                "threadId": thread, "turnId": turn, "itemId": item, "delta": piece}}));
        }
        lines.push(
            json!({"method": "item/completed", "params": {"threadId": thread, "turnId": turn,
            "item": {"type": "agentMessage", "id": item, "text": reply.concat()}}}),
        );
        lines.push(
            json!({"method": "turn/completed", "params": {"threadId": thread,
            "turn": {"id": turn, "items": [], "status": "completed", "error": null}}}),
        );
        lines
    }

    /// Streams the scripted reply as a real turn would.
    fn stream_turn(&self, out: mpsc::UnboundedSender<Value>, thread: String, turn: String) {
        let (reply, finish, total, usage_mid_turn) = self.with(|b| {
            let total = b.tokens.entry(thread.clone()).or_default();
            *total += 120;
            (b.reply.clone(), b.finish_turns, *total, b.usage_mid_turn)
        });
        let fake = self.clone();
        tokio::spawn(async move {
            let item = format!("{turn}-msg");
            let send = |value: Value| {
                let _ = out.send(value);
            };
            send(
                json!({"method": "turn/started", "params": {"threadId": thread,
                "turn": {"id": turn, "items": [], "status": "inProgress", "error": null}}}),
            );
            send(
                json!({"method": "item/started", "params": {"threadId": thread, "turnId": turn,
                "item": {"type": "agentMessage", "id": item, "text": ""}}}),
            );
            let usage = json!({"method": "thread/tokenUsage/updated", "params": {
                "threadId": thread, "turnId": turn, "tokenUsage": {
                    "total": {"totalTokens": total, "inputTokens": total - 20,
                              "cachedInputTokens": 0, "outputTokens": 20,
                              "reasoningOutputTokens": 0},
                    "last": {"totalTokens": 120, "inputTokens": 100, "cachedInputTokens": 0,
                             "outputTokens": 20, "reasoningOutputTokens": 0},
                    "modelContextWindow": 272000}}});
            for piece in &reply {
                tokio::task::yield_now().await;
                send(json!({"method": "item/agentMessage/delta", "params": {
                    "threadId": thread, "turnId": turn, "itemId": item, "delta": piece}}));
            }
            if usage_mid_turn {
                send(usage.clone());
            }
            if !finish {
                return;
            }
            // As Codex does, the turn reports its tokens once the model's
            // response is complete, just before the turn ends.
            fake.with(|b| b.finished.insert(turn.clone()));
            let text: String = reply.concat();
            send(
                json!({"method": "item/completed", "params": {"threadId": thread, "turnId": turn,
                "item": {"type": "agentMessage", "id": item, "text": text}}}),
            );
            send(usage);
            send(
                json!({"method": "turn/completed", "params": {"threadId": thread,
                "turn": {"id": turn, "items": [], "status": "completed", "error": null}}}),
            );
        });
    }
}

struct Context {
    owner: Owner,
    data_dir: PathBuf,
    catalog: PathBuf,
    workspace: PathBuf,
    codex_home: PathBuf,
}

fn model_entry(slug: &str) -> Value {
    json!({"id": slug, "model": slug, "displayName": slug, "description": "",
           "hidden": false, "isDefault": true})
}
