//! The checks the gateway makes before it serves a thread.
//!
//! An app server is used only after [`check_config`] and [`check_models`]
//! pass at startup. Each thread is served only after `config/read` passes
//! [`check_config`] again and its `thread/start` or `thread/resume` response
//! passes [`check_thread`]. While a turn runs, every item must pass
//! [`check_item`]. Any failure refuses the thread.

use std::collections::BTreeSet;
use std::path::Path;

use bokkie_codex::protocol::{ModelListResponse, ThreadItem, ThreadSessionResponse};
use serde_json::Value;

use super::config::{DISABLED_FEATURES, PROFILE};

/// Why the gateway refuses to serve an app server or thread. The reason is
/// logged and may be shown to an admin; it holds no secrets.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct Refusal(pub String);

impl Refusal {
    pub fn new(reason: impl std::fmt::Display) -> Self {
        Self(reason.to_string())
    }
}

/// What a user's app server must report.
#[derive(Debug, Clone)]
pub struct Expected {
    pub data_dir: String,
    pub catalog_path: String,
    pub catalog_slugs: BTreeSet<String>,
    pub workspace: String,
}

impl Expected {
    pub fn new(data_dir: &Path, catalog: &super::catalog::Catalog, workspace: &Path) -> Self {
        Self {
            data_dir: data_dir.display().to_string(),
            catalog_path: catalog.path.display().to_string(),
            catalog_slugs: catalog.slugs.clone(),
            workspace: workspace.display().to_string(),
        }
    }
}

fn require(ok: bool, reason: impl FnOnce() -> String) -> Result<(), Refusal> {
    if ok { Ok(()) } else { Err(Refusal(reason())) }
}

fn at<'a>(value: &'a Value, path: &[&str]) -> &'a Value {
    path.iter().fold(value, |node, key| &node[*key])
}

/// Checks the effective configuration from `config/read`.
pub fn check_config(config: &Value, expected: &Expected) -> Result<(), Refusal> {
    let text = |path: &[&str]| at(config, path).as_str().map(str::to_owned);
    require(
        text(&["default_permissions"]).as_deref() == Some(PROFILE),
        || {
            format!(
                "default_permissions is {}, not {PROFILE:?}",
                at(config, &["default_permissions"])
            )
        },
    )?;
    require(
        text(&["approval_policy"]).as_deref() == Some("never"),
        || format!("approval_policy is {}", at(config, &["approval_policy"])),
    )?;
    require(text(&["web_search"]).as_deref() == Some("disabled"), || {
        format!("web_search is {}", at(config, &["web_search"]))
    })?;
    require(
        text(&["model_catalog_json"]).as_deref() == Some(expected.catalog_path.as_str()),
        || {
            format!(
                "model_catalog_json is {}, not Bokkie's catalog",
                at(config, &["model_catalog_json"])
            )
        },
    )?;
    require(
        at(config, &["project_doc_max_bytes"]) == &Value::from(0),
        || {
            format!(
                "project_doc_max_bytes is {}, so AGENTS.md files would be read",
                at(config, &["project_doc_max_bytes"])
            )
        },
    )?;
    // A selected or defined config profile could override any of this.
    require(at(config, &["profile"]).is_null(), || {
        "a config profile is selected".into()
    })?;
    let profiles = at(config, &["profiles"]);
    require(
        profiles.is_null() || profiles.as_object().is_some_and(|p| p.is_empty()),
        || "config profiles are defined".into(),
    )?;
    for feature in DISABLED_FEATURES {
        let value = at(config, &["features", feature]);
        require(value == &Value::Bool(false), || {
            format!("feature {feature} is {value}, not false")
        })?;
    }
    let mcp = at(config, &["mcp_servers"]);
    require(
        mcp.is_null() || mcp.as_object().is_some_and(|m| m.is_empty()),
        || "MCP servers are configured".into(),
    )?;

    let profile = at(config, &["permissions", PROFILE]);
    let filesystem = profile["filesystem"]
        .as_object()
        .ok_or_else(|| Refusal::new(format!("permission profile {PROFILE} has no filesystem")))?;
    let wanted = [
        (":minimal", "read"),
        (expected.data_dir.as_str(), "deny"),
        (":workspace_roots", "write"),
    ];
    for (path, access) in wanted {
        require(
            filesystem.get(path).and_then(Value::as_str) == Some(access),
            || format!("profile {PROFILE} does not give {path} {access} access"),
        )?;
    }
    // Nothing else may be granted. Unset options read as null.
    for (path, access) in filesystem {
        require(
            access.is_null() || wanted.iter().any(|(p, _)| p == path),
            || format!("profile {PROFILE} also grants {path}"),
        )?;
    }
    require(profile["network"]["enabled"] == Value::Bool(false), || {
        format!("profile {PROFILE} enables the network")
    })?;
    require(profile["extends"].is_null(), || {
        format!("profile {PROFILE} extends another profile")
    })?;
    Ok(())
}

/// Checks `model/list`: every model comes from Bokkie's catalog, so the
/// catalog is in use rather than a bundled or remote one.
pub fn check_models(models: &ModelListResponse, expected: &Expected) -> Result<(), Refusal> {
    require(!models.data.is_empty(), || "model/list is empty".into())?;
    for model in &models.data {
        require(expected.catalog_slugs.contains(&model.model), || {
            format!("model {} is not in Bokkie's catalog", model.model)
        })?;
    }
    Ok(())
}

/// Checks a `thread/start` or `thread/resume` response.
pub fn check_thread(response: &ThreadSessionResponse, expected: &Expected) -> Result<(), Refusal> {
    let profile = response.active_permission_profile.as_ref();
    require(profile.is_some_and(|p| p.id == PROFILE), || {
        format!(
            "thread {} runs with permission profile {:?}, not {PROFILE:?}",
            response.thread.id,
            profile.map(|p| &p.id)
        )
    })?;
    require(profile.is_some_and(|p| p.extends.is_none()), || {
        format!(
            "thread {} runs with an extended profile",
            response.thread.id
        )
    })?;
    require(
        response.approval_policy == Value::String("never".into()),
        || {
            format!(
                "thread {} has approval policy {}",
                response.thread.id, response.approval_policy
            )
        },
    )?;
    require(expected.catalog_slugs.contains(&response.model), || {
        format!(
            "thread {} uses model {}, which is not in Bokkie's catalog",
            response.thread.id, response.model
        )
    })?;
    require(response.cwd == expected.workspace, || {
        format!(
            "thread {} runs outside the user's workspace",
            response.thread.id
        )
    })?;
    Ok(())
}

/// Checks one item of a running turn. Only conversation items and Bokkie's
/// own dynamic tools may appear while threads are chat-only.
pub fn check_item(item: &ThreadItem) -> Result<(), Refusal> {
    match item {
        ThreadItem::UserMessage { .. }
        | ThreadItem::AgentMessage { .. }
        | ThreadItem::Reasoning { .. }
        | ThreadItem::Plan { .. }
        | ThreadItem::ContextCompaction { .. }
        | ThreadItem::DynamicToolCall { .. } => Ok(()),
        ThreadItem::CommandExecution { command, .. } => Err(Refusal::new(format!(
            "the model ran a command ({} characters) while commands are disabled",
            command.len()
        ))),
        ThreadItem::McpToolCall { server, tool, .. } => Err(Refusal::new(format!(
            "the model called MCP tool {server}/{tool}"
        ))),
        ThreadItem::Unknown => Err(Refusal::new(
            "the model used a tool that is not allowed while threads are chat-only",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bokkie_codex::protocol::{ActivePermissionProfile, Model, Thread};
    use serde_json::json;

    fn expected() -> Expected {
        Expected {
            data_dir: "/data".into(),
            catalog_path: "/data/codex/models.json".into(),
            catalog_slugs: ["gpt-6.1-sol".to_owned()].into(),
            workspace: "/data/users/1/workspace".into(),
        }
    }

    /// A configuration as the pinned app server reports it for the
    /// gateway's `config.toml`.
    pub fn good_config() -> Value {
        let mut features = serde_json::Map::new();
        for feature in DISABLED_FEATURES {
            features.insert((*feature).into(), json!(false));
        }
        features.insert("mentions_v2".into(), json!(true));
        json!({
            "default_permissions": "bokkie",
            "approval_policy": "never",
            "web_search": "disabled",
            "model_catalog_json": "/data/codex/models.json",
            "project_doc_max_bytes": 0,
            "profile": null,
            "features": features,
            "mcp_servers": {},
            "permissions": {"bokkie": {
                "description": null, "extends": null, "workspace_roots": null,
                "filesystem": {"glob_scan_max_depth": null, "/data": "deny",
                               ":minimal": "read", ":workspace_roots": "write"},
                "network": {"enabled": false, "proxy_url": null},
            }},
        })
    }

    #[test]
    fn the_gateways_configuration_passes() {
        check_config(&good_config(), &expected()).unwrap();
    }

    #[test]
    fn each_deviation_is_refused() {
        type Change = Box<dyn Fn(&mut Value)>;
        let cases: Vec<(&str, Change)> = vec![
            (
                "default_permissions",
                Box::new(|c| c["default_permissions"] = json!(":workspace")),
            ),
            (
                "default_permissions",
                Box::new(|c| c["default_permissions"] = Value::Null),
            ),
            (
                "approval_policy",
                Box::new(|c| c["approval_policy"] = json!("on-request")),
            ),
            ("web_search", Box::new(|c| c["web_search"] = json!("live"))),
            (
                "model_catalog_json",
                Box::new(|c| c["model_catalog_json"] = Value::Null),
            ),
            (
                "shell_tool",
                Box::new(|c| c["features"]["shell_tool"] = json!(true)),
            ),
            (
                "unified_exec",
                Box::new(|c| {
                    c["features"]
                        .as_object_mut()
                        .unwrap()
                        .remove("unified_exec");
                }),
            ),
            (
                "MCP",
                Box::new(|c| c["mcp_servers"] = json!({"x": {"command": "sh"}})),
            ),
            (
                "grants",
                Box::new(|c| {
                    c["permissions"]["bokkie"]["filesystem"]["/home"] = json!("read");
                }),
            ),
            (
                "/data",
                Box::new(|c| {
                    c["permissions"]["bokkie"]["filesystem"]["/data"] = json!("read");
                }),
            ),
            (
                "network",
                Box::new(|c| {
                    c["permissions"]["bokkie"]["network"]["enabled"] = json!(true);
                }),
            ),
            (
                "extends",
                Box::new(|c| {
                    c["permissions"]["bokkie"]["extends"] = json!(":workspace");
                }),
            ),
            ("filesystem", Box::new(|c| c["permissions"] = json!({}))),
        ];
        for (name, change) in cases {
            let mut config = good_config();
            change(&mut config);
            let refusal = check_config(&config, &expected()).expect_err(name);
            assert!(!refusal.0.is_empty());
        }
    }

    fn model(slug: &str) -> Model {
        Model {
            id: slug.into(),
            model: slug.into(),
            display_name: slug.into(),
            description: String::new(),
            hidden: false,
            is_default: true,
        }
    }

    #[test]
    fn models_must_come_from_the_catalog() {
        let ok = ModelListResponse {
            data: vec![model("gpt-6.1-sol")],
            next_cursor: None,
        };
        check_models(&ok, &expected()).unwrap();
        let remote = ModelListResponse {
            data: vec![model("gpt-6.1-sol"), model("gpt-7-remote")],
            next_cursor: None,
        };
        assert!(check_models(&remote, &expected()).is_err());
        let empty = ModelListResponse {
            data: vec![],
            next_cursor: None,
        };
        assert!(check_models(&empty, &expected()).is_err());
    }

    fn thread(profile: Option<&str>) -> ThreadSessionResponse {
        ThreadSessionResponse {
            thread: Thread {
                id: "t1".into(),
                session_id: "t1".into(),
                preview: String::new(),
                ephemeral: false,
                model_provider: "openai".into(),
                cwd: "/data/users/1/workspace".into(),
                cli_version: "0.163.0".into(),
                created_at: 0,
                updated_at: 0,
                path: None,
                turns: vec![],
            },
            model: "gpt-6.1-sol".into(),
            model_provider: "openai".into(),
            cwd: "/data/users/1/workspace".into(),
            approval_policy: json!("never"),
            sandbox: json!({"type": "workspaceWrite"}),
            active_permission_profile: profile.map(|id| ActivePermissionProfile {
                id: id.into(),
                extends: None,
            }),
        }
    }

    #[test]
    fn threads_must_run_under_the_bokkie_profile() {
        check_thread(&thread(Some("bokkie")), &expected()).unwrap();
        assert!(check_thread(&thread(Some(":workspace")), &expected()).is_err());
        assert!(check_thread(&thread(None), &expected()).is_err());
        let mut extended = thread(Some("bokkie"));
        extended.active_permission_profile.as_mut().unwrap().extends = Some(":workspace".into());
        assert!(check_thread(&extended, &expected()).is_err());
        let mut approvals = thread(Some("bokkie"));
        approvals.approval_policy = json!("on-request");
        assert!(check_thread(&approvals, &expected()).is_err());
        let mut model = thread(Some("bokkie"));
        model.model = "gpt-7-remote".into();
        assert!(check_thread(&model, &expected()).is_err());
        let mut elsewhere = thread(Some("bokkie"));
        elsewhere.cwd = "/data/users/2/workspace".into();
        assert!(check_thread(&elsewhere, &expected()).is_err());
    }

    #[test]
    fn only_conversation_items_are_allowed() {
        let allowed = [
            json!({"type": "userMessage", "id": "1"}),
            json!({"type": "agentMessage", "id": "2", "text": "hi"}),
            json!({"type": "reasoning", "id": "3"}),
            json!({"type": "dynamicToolCall", "id": "4", "tool": "message_bot",
                   "namespace": null, "arguments": {}, "status": "inProgress",
                   "success": null}),
        ];
        for item in allowed {
            let item: ThreadItem = serde_json::from_value(item).unwrap();
            check_item(&item).unwrap();
        }
        let refused = [
            json!({"type": "commandExecution", "id": "5", "command": "cat /etc/passwd",
                   "cwd": "/", "status": "inProgress", "aggregatedOutput": null,
                   "exitCode": null}),
            json!({"type": "fileChange", "id": "6", "changes": [], "status": "inProgress"}),
            json!({"type": "webSearch", "id": "7", "query": "x"}),
            json!({"type": "imageView", "id": "8", "path": "/data/x.png"}),
            json!({"type": "collabAgentToolCall", "id": "9"}),
            json!({"type": "mcpToolCall", "id": "10", "server": "s", "tool": "t",
                   "arguments": {}, "status": "inProgress"}),
        ];
        for item in refused {
            let item: ThreadItem = serde_json::from_value(item).unwrap();
            let refusal = check_item(&item).unwrap_err();
            // The command itself is never repeated.
            assert!(!refusal.0.contains("passwd"), "{refusal}");
        }
    }
}
