//! Proves the client's types against the vendored Codex schema.
//!
//! - Every method, notification and server request Bokkie uses exists in the
//!   schema with the expected parameter definition.
//! - Outgoing values validate against their definitions, and every field name
//!   they emit is declared there.
//! - Incoming types decode a generated minimal instance (required fields
//!   only, null where allowed) and maximal instance (every field) for every
//!   top-level variant, and every field name they read is declared in the
//!   schema. Nested unions take their first non-null branch, enums their
//!   first value and arrays one item, so closed enums that Bokkie decodes
//!   are also checked value by value.

use std::sync::OnceLock;

use bokkie_codex::protocol::*;
use bokkie_codex::{CLIENT_METHODS, SCHEMA_BUNDLE};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};

// ---------------------------------------------------------------------------
// Schema helpers

fn bundle() -> &'static Value {
    static BUNDLE: OnceLock<Value> = OnceLock::new();
    BUNDLE.get_or_init(|| serde_json::from_str(SCHEMA_BUNDLE).expect("bundle parses"))
}

/// Looks up a definition by path, such as `InitializeParams` or
/// `v2/ThreadStartParams`.
fn definition(path: &str) -> &'static Value {
    let mut node = &bundle()["definitions"];
    for part in path.split('/') {
        node = node
            .get(part)
            .unwrap_or_else(|| panic!("schema has no definition {path}"));
    }
    node
}

fn resolve(mut node: &'static Value) -> &'static Value {
    while let Some(reference) = node.get("$ref").and_then(Value::as_str) {
        let path = reference
            .strip_prefix("#/definitions/")
            .unwrap_or_else(|| panic!("unexpected reference {reference}"));
        node = definition(path);
    }
    node
}

fn validate(path: &str, instance: &Value) -> Vec<String> {
    let schema = json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "definitions": bundle()["definitions"],
        "allOf": [{ "$ref": format!("#/definitions/{path}") }],
    });
    let validator = jsonschema::draft7::new(&schema).expect("schema compiles");
    validator
        .iter_errors(instance)
        .map(|error| format!("{} at {}", error, error.instance_path()))
        .collect()
}

fn assert_valid(path: &str, instance: &Value) {
    let errors = validate(path, instance);
    assert!(
        errors.is_empty(),
        "{path} rejects {instance}:\n{}",
        errors.join("\n")
    );
}

/// Reports object keys in `value` that `node` does not declare. The schema
/// does not forbid additional properties, so a misspelt field would otherwise
/// pass validation and silently never be read or sent.
fn undeclared_keys(node: &'static Value, value: &Value, at: &str, out: &mut Vec<String>) {
    if value.is_null() {
        return;
    }
    let node = resolve(node);
    let Some(object) = node.as_object() else {
        return; // `true`: anything goes.
    };
    for keyword in ["anyOf", "oneOf"] {
        if let Some(branches) = object.get(keyword).and_then(Value::as_array) {
            let mut best: Option<Vec<String>> = None;
            for branch in branches {
                let mut errors = Vec::new();
                undeclared_keys(branch, value, at, &mut errors);
                if !branch_admits_type(resolve(branch), value) {
                    errors.push(format!("{at}: wrong type for branch"));
                }
                if best.as_ref().is_none_or(|b| errors.len() < b.len()) {
                    best = Some(errors);
                }
            }
            let best = best.unwrap_or_default();
            if !best.is_empty() && !object.contains_key("properties") {
                out.extend(best);
                return;
            }
        }
    }
    if let Some(parts) = object.get("allOf").and_then(Value::as_array) {
        for part in parts {
            undeclared_keys(part, value, at, out);
        }
    }
    match value {
        Value::Object(fields) => {
            let properties = object.get("properties").and_then(Value::as_object);
            let additional = object.get("additionalProperties");
            for (key, field) in fields {
                let path = format!("{at}/{key}");
                match (properties.and_then(|p| p.get(key)), additional) {
                    (Some(schema), _) => undeclared_keys(schema, field, &path, out),
                    (None, Some(Value::Object(_))) => {
                        undeclared_keys(additional.unwrap(), field, &path, out);
                    }
                    (None, Some(Value::Bool(true))) => {}
                    (None, _) if properties.is_some() => {
                        out.push(format!("{path}: not declared by the schema"));
                    }
                    (None, _) => {}
                }
            }
        }
        Value::Array(items) => {
            if let Some(schema) = object.get("items") {
                for (index, item) in items.iter().enumerate() {
                    undeclared_keys(schema, item, &format!("{at}/{index}"), out);
                }
            }
        }
        _ => {}
    }
}

fn branch_admits_type(branch: &Value, value: &Value) -> bool {
    let Some(types) = branch.get("type") else {
        return true;
    };
    let name = match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(n) if n.is_i64() || n.is_u64() => "integer",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    };
    let matches = |t: &Value| t == name || (t == "number" && name == "integer");
    match types {
        Value::Array(types) => types.iter().any(matches),
        single => matches(single),
    }
}

fn assert_declared(path: &str, value: &Value) {
    let mut errors = Vec::new();
    undeclared_keys(definition(path), value, "", &mut errors);
    assert!(
        errors.is_empty(),
        "{path} does not declare fields in {value}:\n{}",
        errors.join("\n")
    );
}

#[derive(Clone, Copy, PartialEq)]
enum Fill {
    /// Only required properties, and `null` wherever the schema allows it.
    Minimal,
    /// Every property, with a non-null value wherever possible.
    Maximal,
}

const MAX_DEPTH: usize = 10;

/// Builds an instance of `node` that the schema accepts.
fn instance(node: &'static Value, fill: Fill, depth: usize) -> Value {
    let fill = if depth > MAX_DEPTH {
        Fill::Minimal
    } else {
        fill
    };
    let node = resolve(node);
    let Some(object) = node.as_object() else {
        return json!({}); // `true`
    };
    if let Some(value) = object.get("const") {
        return value.clone();
    }
    if let Some(values) = object.get("enum").and_then(Value::as_array) {
        return values[0].clone();
    }
    for keyword in ["oneOf", "anyOf"] {
        if let Some(branches) = object.get(keyword).and_then(Value::as_array) {
            let null = json!({"type": "null"});
            let branch = match fill {
                Fill::Minimal if branches.contains(&null) => return Value::Null,
                _ => branches
                    .iter()
                    .find(|b| **b != null)
                    .unwrap_or(&branches[0]),
            };
            let mut value = instance(branch, fill, depth + 1);
            if object.contains_key("properties") {
                merge(&mut value, object_instance(object, fill, depth));
            }
            return value;
        }
    }
    if let Some(parts) = object.get("allOf").and_then(Value::as_array) {
        let mut value = Value::Null;
        for part in parts {
            merge(&mut value, instance(part, fill, depth + 1));
        }
        if object.contains_key("properties") {
            merge(&mut value, object_instance(object, fill, depth));
        }
        return value;
    }
    let types: Vec<&str> = match object.get("type") {
        Some(Value::String(t)) => vec![t.as_str()],
        Some(Value::Array(ts)) => ts.iter().filter_map(Value::as_str).collect(),
        _ if object.contains_key("properties") => vec!["object"],
        _ => return json!({}),
    };
    if fill == Fill::Minimal && types.contains(&"null") {
        return Value::Null;
    }
    match types
        .iter()
        .find(|t| **t != "null")
        .copied()
        .unwrap_or("null")
    {
        "object" => object_instance(object, fill, depth),
        "array" => match (fill, object.get("items")) {
            (Fill::Maximal, Some(items)) => json!([instance(items, fill, depth + 1)]),
            _ => json!([]),
        },
        "string" => json!("x"),
        "integer" => json!(object.get("minimum").and_then(Value::as_f64).unwrap_or(1.0) as i64),
        "number" => json!(1.5),
        "boolean" => json!(fill == Fill::Maximal),
        _ => Value::Null,
    }
}

fn object_instance(object: &'static Map<String, Value>, fill: Fill, depth: usize) -> Value {
    let required: Vec<&str> = object
        .get("required")
        .and_then(Value::as_array)
        .map(|r| r.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let mut out = Map::new();
    if let Some(properties) = object.get("properties").and_then(Value::as_object) {
        for (name, schema) in properties {
            if fill == Fill::Maximal || required.contains(&name.as_str()) {
                out.insert(name.clone(), instance(schema, fill, depth + 1));
            }
        }
    }
    if fill == Fill::Maximal
        && let Some(additional @ Value::Object(_)) = object.get("additionalProperties")
    {
        let additional: &'static Value = additional;
        out.insert("extra".into(), instance(additional, fill, depth + 1));
    }
    Value::Object(out)
}

fn merge(into: &mut Value, from: Value) {
    match (into, from) {
        (Value::Object(a), Value::Object(b)) => a.extend(b),
        (slot, from) => *slot = from,
    }
}

/// Instances covering each top-level variant at both fill levels.
fn instances(path: &str) -> Vec<Value> {
    let node = resolve(definition(path));
    let branches: Vec<&'static Value> = node
        .get("oneOf")
        .and_then(Value::as_array)
        .map(|b| b.iter().collect())
        .unwrap_or_else(|| vec![node]);
    let mut out = Vec::new();
    for branch in branches {
        for fill in [Fill::Minimal, Fill::Maximal] {
            let value = instance(branch, fill, 0);
            if !value.is_null() {
                assert_valid(path, &value);
                out.push(value);
            }
        }
    }
    out
}

/// An incoming type decodes every generated instance, and re-encoding it
/// emits only declared field names.
fn check_incoming<T: DeserializeOwned + Serialize>(path: &str) {
    for value in instances(path) {
        let decoded: T = serde_json::from_value(value.clone())
            .unwrap_or_else(|e| panic!("{path} instance does not decode: {e}\n{value}"));
        assert_declared(path, &serde_json::to_value(&decoded).unwrap());
    }
}

/// An outgoing value validates and emits only declared field names.
fn check_outgoing<T: Serialize>(path: &str, value: &T) {
    let value = serde_json::to_value(value).unwrap();
    assert_valid(path, &value);
    assert_declared(path, &value);
}

/// The union entry for `method` in a request or notification union.
fn union_entry(union: &str, method: &str) -> &'static Value {
    definition(union)["oneOf"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["properties"]["method"]["enum"][0] == method)
        .unwrap_or_else(|| panic!("{union} has no method {method}"))
}

/// The parameter definition of a union entry, and whether the server
/// requires parameters. `anyOf` with `null` marks optional parameters.
fn params_of(entry: &Value) -> (Option<String>, bool) {
    let Some(params) = entry["properties"].get("params") else {
        return (None, false);
    };
    let required = entry["required"]
        .as_array()
        .is_some_and(|r| r.iter().any(|name| name == "params"));
    let reference = params
        .get("$ref")
        .or_else(|| {
            params["anyOf"]
                .as_array()?
                .iter()
                .find_map(|branch| branch.get("$ref"))
        })
        .and_then(Value::as_str)
        .map(|r| r.trim_start_matches("#/definitions/").to_owned());
    (reference, required)
}

fn params_ref(entry: &Value) -> Option<String> {
    params_of(entry).0
}

// ---------------------------------------------------------------------------
// Method inventory

#[test]
fn client_methods_exist_with_their_parameter_definitions() {
    for method in CLIENT_METHODS {
        let entry = union_entry("ClientRequest", method.method);
        let (reference, required) = params_of(entry);
        match method.params {
            Some(params) => assert_eq!(reference.as_deref(), Some(params), "{}", method.method),
            // Bokkie sends no parameters, so the server must not require any.
            None => assert!(!required, "{} requires parameters", method.method),
        }
        definition(method.result);
    }
}

#[test]
fn typed_notifications_exist_with_their_definitions() {
    for (method, path) in TYPED_NOTIFICATIONS {
        let entry = union_entry("ServerNotification", method);
        assert_eq!(params_ref(entry).as_deref(), Some(*path), "{method}");
    }
}

#[test]
fn typed_server_requests_exist_with_their_definitions() {
    for (method, params, result) in TYPED_SERVER_REQUESTS {
        let entry = union_entry("ServerRequest", method);
        assert_eq!(params_ref(entry).as_deref(), Some(*params), "{method}");
        definition(result);
    }
}

// ---------------------------------------------------------------------------
// Outgoing

#[test]
fn initialize_params_conform() {
    check_outgoing(
        "InitializeParams",
        &InitializeParams {
            client_info: ClientInfo {
                name: "bokkie".into(),
                version: "0.1.0".into(),
                title: Some("Bokkie".into()),
            },
            capabilities: Some(InitializeCapabilities {
                experimental_api: true,
                opt_out_notification_methods: Some(vec!["item/reasoning/textDelta".into()]),
            }),
        },
    );
}

#[test]
fn account_params_conform() {
    check_outgoing(
        "v2/GetAccountParams",
        &GetAccountParams {
            refresh_token: true,
        },
    );
    check_outgoing(
        "v2/LoginAccountParams",
        &LoginAccountParams::ChatgptDeviceCode,
    );
    check_outgoing(
        "v2/CancelLoginAccountParams",
        &CancelLoginAccountParams {
            login_id: "l".into(),
        },
    );
}

#[test]
fn list_and_config_params_conform() {
    check_outgoing(
        "v2/ModelListParams",
        &ModelListParams {
            cursor: Some("c".into()),
            include_hidden: Some(true),
            limit: Some(10),
        },
    );
    check_outgoing(
        "v2/ConfigReadParams",
        &ConfigReadParams {
            cwd: Some("/data".into()),
            include_layers: true,
        },
    );
    check_outgoing(
        "v2/PermissionProfileListParams",
        &PermissionProfileListParams {
            cwd: Some("/data".into()),
        },
    );
}

#[test]
fn thread_and_turn_params_conform() {
    let mut config = Map::new();
    config.insert("features.shell_tool".into(), json!(false));
    check_outgoing(
        "v2/ThreadStartParams",
        &ThreadStartParams {
            model: Some("m".into()),
            cwd: Some("/data/users/u/workspace".into()),
            approval_policy: Some(AskForApproval::Never),
            permissions: Some("bokkie".into()),
            sandbox: Some(SandboxMode::ReadOnly),
            developer_instructions: Some("Be brief.".into()),
            ephemeral: Some(false),
            dynamic_tools: Some(vec![DynamicToolSpec::Function {
                name: "message_bot".into(),
                description: "Send a message to another Bot.".into(),
                input_schema: json!({"type": "object", "properties": {}}),
            }]),
            config: Some(config.clone()),
        },
    );
    check_outgoing(
        "v2/ThreadResumeParams",
        &ThreadResumeParams {
            thread_id: "t".into(),
            cwd: Some("/w".into()),
            approval_policy: Some(AskForApproval::OnRequest),
            permissions: Some("bokkie".into()),
            developer_instructions: Some("d".into()),
            config: Some(config),
        },
    );
    let mut turn = TurnStartParams::text("t", "hello");
    turn.cwd = Some("/w".into());
    turn.approval_policy = Some(AskForApproval::Untrusted);
    turn.permissions = Some("bokkie".into());
    turn.model = Some("m".into());
    turn.additional_context = Some(
        [(
            "bokkie-profile".to_owned(),
            AdditionalContextEntry {
                kind: AdditionalContextKind::Application,
                value: "v".into(),
            },
        )]
        .into(),
    );
    check_outgoing("v2/TurnStartParams", &turn);
    for kind in [
        AdditionalContextKind::Application,
        AdditionalContextKind::Untrusted,
    ] {
        assert_valid(
            "v2/AdditionalContextKind",
            &serde_json::to_value(kind).unwrap(),
        );
    }
    check_outgoing(
        "v2/TurnInterruptParams",
        &TurnInterruptParams {
            thread_id: "t".into(),
            turn_id: "u".into(),
        },
    );
    check_outgoing(
        "v2/TurnSteerParams",
        &TurnSteerParams {
            thread_id: "t".into(),
            expected_turn_id: "u".into(),
            input: vec![UserInput::Text {
                text: "also this".into(),
            }],
            additional_context: Some(
                [(
                    "bokkie-profile".to_owned(),
                    AdditionalContextEntry {
                        kind: AdditionalContextKind::Untrusted,
                        value: "v".into(),
                    },
                )]
                .into(),
            ),
        },
    );
}

#[test]
fn command_exec_params_conform() {
    check_outgoing(
        "v2/CommandExecParams",
        &CommandExecParams {
            command: vec!["true".into()],
            cwd: Some("/w".into()),
            permission_profile: Some("bokkie".into()),
            timeout_ms: Some(1000),
            process_id: Some("p".into()),
        },
    );
}

#[test]
fn server_request_answers_conform() {
    check_outgoing(
        "DynamicToolCallResponse",
        &DynamicToolCallResponse::text(true, "delivered"),
    );
    for decision in [
        ApprovalDecision::Accept,
        ApprovalDecision::AcceptForSession,
        ApprovalDecision::Decline,
        ApprovalDecision::Cancel,
    ] {
        let answer = ApprovalResponse { decision };
        check_outgoing("CommandExecutionRequestApprovalResponse", &answer);
        check_outgoing("FileChangeRequestApprovalResponse", &answer);
    }
}

// ---------------------------------------------------------------------------
// Incoming

#[test]
fn method_results_decode() {
    check_incoming::<InitializeResponse>("InitializeResponse");
    check_incoming::<ModelListResponse>("v2/ModelListResponse");
    check_incoming::<GetAccountResponse>("v2/GetAccountResponse");
    check_incoming::<LoginAccountResponse>("v2/LoginAccountResponse");
    check_incoming::<CancelLoginAccountResponse>("v2/CancelLoginAccountResponse");
    check_incoming::<LogoutAccountResponse>("v2/LogoutAccountResponse");
    check_incoming::<GetAccountRateLimitsResponse>("v2/GetAccountRateLimitsResponse");
    check_incoming::<ConfigReadResponse>("v2/ConfigReadResponse");
    check_incoming::<PermissionProfileListResponse>("v2/PermissionProfileListResponse");
    check_incoming::<ThreadStartResponse>("v2/ThreadStartResponse");
    check_incoming::<ThreadResumeResponse>("v2/ThreadResumeResponse");
    check_incoming::<TurnStartResponse>("v2/TurnStartResponse");
    check_incoming::<TurnInterruptResponse>("v2/TurnInterruptResponse");
    check_incoming::<TurnSteerResponse>("v2/TurnSteerResponse");
    check_incoming::<CommandExecResponse>("v2/CommandExecResponse");
}

#[test]
fn every_turn_status_decodes() {
    // The generator takes the first enum value, so cover every value of the
    // closed enums Bokkie decodes.
    let values = definition("v2/TurnStatus")["enum"].as_array().unwrap();
    assert!(!values.is_empty());
    for value in values {
        let _: TurnStatus = serde_json::from_value(value.clone())
            .unwrap_or_else(|e| panic!("TurnStatus {value} does not decode: {e}"));
    }
}

#[test]
fn every_thread_item_variant_decodes() {
    check_incoming::<ThreadItem>("v2/ThreadItem");
    check_incoming::<Account>("v2/Account");
    // The typed variants decode to themselves, not to `Unknown`.
    for value in instances("v2/ThreadItem") {
        let item: ThreadItem = serde_json::from_value(value.clone()).unwrap();
        let typed = [
            "userMessage",
            "agentMessage",
            "reasoning",
            "plan",
            "contextCompaction",
            "commandExecution",
            "dynamicToolCall",
            "mcpToolCall",
        ];
        assert_eq!(
            typed.contains(&value["type"].as_str().unwrap()),
            item != ThreadItem::Unknown,
            "{value}"
        );
    }
}

#[test]
fn typed_notifications_decode() {
    for (method, path) in TYPED_NOTIFICATIONS {
        for value in instances(path) {
            let decoded = ServerNotification::decode((*method).into(), Some(value.clone()))
                .unwrap_or_else(|e| panic!("{method} does not decode: {e}\n{value}"));
            assert!(
                !matches!(decoded, ServerNotification::Unknown { .. }),
                "{method}"
            );
        }
    }
    check_incoming::<TurnNotification>("v2/TurnCompletedNotification");
    check_incoming::<ItemNotification>("v2/ItemCompletedNotification");
    check_incoming::<AgentMessageDeltaNotification>("v2/AgentMessageDeltaNotification");
    check_incoming::<AccountLoginCompletedNotification>("v2/AccountLoginCompletedNotification");
    check_incoming::<AccountUpdatedNotification>("v2/AccountUpdatedNotification");
    check_incoming::<AccountRateLimitsUpdatedNotification>(
        "v2/AccountRateLimitsUpdatedNotification",
    );
    check_incoming::<ThreadTokenUsageUpdatedNotification>("v2/ThreadTokenUsageUpdatedNotification");
    check_incoming::<ErrorNotification>("v2/ErrorNotification");
}

#[test]
fn typed_server_requests_decode() {
    for (method, path, _) in TYPED_SERVER_REQUESTS {
        for value in instances(path) {
            let decoded = ServerRequestKind::decode((*method).into(), Some(value.clone()))
                .unwrap_or_else(|e| panic!("{method} does not decode: {e}\n{value}"));
            assert!(
                !matches!(decoded, ServerRequestKind::Unknown { .. }),
                "{method}"
            );
        }
    }
    check_incoming::<DynamicToolCallParams>("DynamicToolCallParams");
    check_incoming::<CommandExecutionRequestApprovalParams>(
        "CommandExecutionRequestApprovalParams",
    );
    check_incoming::<FileChangeRequestApprovalParams>("FileChangeRequestApprovalParams");
}

// ---------------------------------------------------------------------------
// The checker itself

#[test]
fn the_checker_rejects_misspelt_and_mistyped_fields() {
    let mut errors = Vec::new();
    undeclared_keys(
        definition("v2/TurnInterruptParams"),
        &json!({"threadId": "t", "turnID": "u"}),
        "",
        &mut errors,
    );
    assert_eq!(errors, ["/turnID: not declared by the schema"]);
    assert!(
        !validate(
            "v2/TurnInterruptParams",
            &json!({"threadId": 1, "turnId": "u"})
        )
        .is_empty()
    );
    assert!(!validate("v2/TurnInterruptParams", &json!({"threadId": "t"})).is_empty());
}
