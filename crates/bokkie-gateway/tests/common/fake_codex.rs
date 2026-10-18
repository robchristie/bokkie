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

use bokkie_codex::{Client, Incoming};
use bokkie_gateway::codex::Launcher;
use bokkie_gateway::codex::config::DISABLED_FEATURES;
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
    pub user: UserId,
    pub method: String,
    pub params: Value,
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
    /// The `account/rateLimits/read` result.
    pub rate_limits: Value,
    pub received: Vec<Received>,
    /// Each thread's token total, which grows by 120 per turn.
    pub tokens: HashMap<String, i64>,
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
            reject_turns: false,
            hang_up_on_turn: false,
            hang_turns: false,
            garble_turn_answers: false,
            usage_mid_turn: false,
            signed_in: true,
            rate_limits: json!({
                "rateLimits": {"limitId": "codex", "limitName": null, "planType": "pro",
                    "primary": {"usedPercent": 12, "windowDurationMins": 10080, "resetsAt": 1_800_600_000},
                    "secondary": null, "rateLimitReachedType": null},
                "rateLimitsByLimitId": null,
            }),
            received: Vec::new(),
            tokens: HashMap::new(),
            next_id: 0,
        }
    }
}

#[derive(Clone, Default)]
pub struct FakeCodex {
    pub behaviour: Arc<Mutex<Behaviour>>,
    /// Each user's outgoing line channel, to inject messages.
    outlets: Arc<Mutex<HashMap<UserId, mpsc::UnboundedSender<Value>>>>,
    /// How many app servers each user has had.
    pub starts: Arc<Mutex<HashMap<UserId, usize>>>,
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
        self.starts.lock().unwrap().get(&user).copied().unwrap_or(0)
    }

    /// Sends a raw message from the user's app server to the gateway.
    pub fn inject(&self, user: UserId, message: Value) {
        let outlet = self.outlets.lock().unwrap().get(&user).cloned();
        outlet
            .expect("the user's app server runs")
            .send(message)
            .unwrap();
    }

    /// Ends the user's app server, as if it crashed.
    pub fn crash(&self, user: UserId) {
        self.outlets.lock().unwrap().remove(&user);
    }

    pub fn launcher(&self, data_dir: &Path, catalog: &Path) -> Launcher {
        let fake = self.clone();
        let data_dir = data_dir.to_owned();
        let catalog = catalog.to_owned();
        Launcher::InProcess(Arc::new(move |user, paths| {
            fake.connect(
                user,
                data_dir.clone(),
                catalog.clone(),
                paths.workspace.clone(),
            )
        }))
    }

    fn connect(
        &self,
        user: UserId,
        data_dir: PathBuf,
        catalog: PathBuf,
        workspace: PathBuf,
    ) -> (Client, mpsc::UnboundedReceiver<Incoming>) {
        *self.starts.lock().unwrap().entry(user).or_default() += 1;
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
            user,
            data_dir,
            catalog,
            workspace,
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
                    user: context.user,
                    method: "<response>".into(),
                    params: message.clone(),
                })
            });
            return;
        };
        let params = message["params"].clone();
        self.with(|b| {
            b.received.push(Received {
                user: context.user,
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
                let id = self.with(|b| {
                    b.next_id += 1;
                    format!("thr-{}-{}", context.user, b.next_id)
                });
                self.session(&id, &params)
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
                self.crash(context.user);
                return;
            }
            "turn/start" => {
                let turn = self.with(|b| {
                    b.next_id += 1;
                    format!("turn-{}", b.next_id)
                });
                let thread = params["threadId"].as_str().unwrap().to_owned();
                if self.with(|b| b.finish_before_answer) {
                    for line in self.turn_lines(&thread, &turn) {
                        let _ = out.send(line);
                    }
                } else {
                    self.stream_turn(out.clone(), thread, turn.clone());
                }
                json!({"turn": {"id": turn, "items": [], "status": "inProgress", "error": null}})
            }
            "turn/steer" if self.with(|b| b.steer_fails) => {
                let _ = out.send(json!({"id": id, "error": {
                    "code": -32600, "message": "no active turn to steer"}}));
                return;
            }
            "turn/steer" => json!({"turnId": params["expectedTurnId"]}),
            "turn/interrupt" => {
                let _ = out.send(json!({"method": "turn/completed", "params": {
                    "threadId": params["threadId"],
                    "turn": {"id": params["turnId"], "items": [], "status": "interrupted", "error": null},
                }}));
                json!({})
            }
            "account/read" => {
                let signed_in = self.with(|b| b.signed_in);
                json!({
                    "account": if signed_in {
                        json!({"type": "chatgpt", "email": "owner@example.test", "planType": "pro"})
                    } else {
                        Value::Null
                    },
                    "requiresOpenaiAuth": true,
                })
            }
            "account/login/start" => json!({
                "type": "chatgptDeviceCode", "loginId": "login-1",
                "userCode": "ABCD-EFGH", "verificationUrl": "https://auth.example.test/device",
            }),
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
    user: UserId,
    data_dir: PathBuf,
    catalog: PathBuf,
    workspace: PathBuf,
}

fn model_entry(slug: &str) -> Value {
    json!({"id": slug, "model": slug, "displayName": slug, "description": "",
           "hidden": false, "isDefault": true})
}
