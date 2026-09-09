//! Codex adapter and durable broker reconciliation, outside the obligation kernel.
use crate::{Store, StoreError, engineering::*};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::io::AsRawFd;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

pub type RuntimeResult<T> = Result<T, Box<dyn std::error::Error>>;
const MAX_FILE: u64 = 2 * 1024 * 1024;
const MAX_SPOOL: u64 = 16 * 1024 * 1024;
const MAX_EVIDENCE_PAGE: u64 = 32 * 1024;
// Leave room for the broker's wire framing as well as JSON encoding.
const MAX_TOOL_REPLY: u64 = MAX_FILE - 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineeringRuntimeProfile {
    pub workspace: PathBuf,
    pub broker_root: PathBuf,
    pub broker: PathBuf,
    pub codex: PathBuf,
    pub bwrap: PathBuf,
    pub supervisor_instructions: PathBuf,
    pub worker_instructions: PathBuf,
    pub model: String,
    pub effort: String,
    pub subagent_model: String,
    pub subagent_effort: String,
    pub max_subagents: u32,
    pub concurrency: u32,
    pub supervisor_seconds: i64,
    pub worker_seconds: i64,
    pub max_packages: u32,
    pub max_repairs: u32,
    pub max_recoveries: u32,
    pub max_turns: u32,
    pub deadline_seconds: i64,
    /// Explicit task decision: ordinary dependency downloads and local UI/CDP
    /// need network access. This does not grant publication or deployment.
    pub worker_network_access: bool,
    pub worker_scratch: Option<PathBuf>,
    pub supervisor_tools: Vec<String>,
    pub worker_tools: Vec<String>,
    /// Only the documented read-only documentation MCP is supported initially;
    /// repository/UI integrations use ordinary gh/Lantern CLI tools.
    pub readonly_mcp_servers: Vec<String>,
    pub allow_single_pwd_approval: bool,
}

impl EngineeringRuntimeProfile {
    pub fn load(path: &Path) -> RuntimeResult<Self> {
        let profile: Self = read_json(path)?;
        profile.validate()?;
        Ok(profile)
    }
    pub fn validate(&self) -> RuntimeResult<()> {
        for path in [
            &self.workspace,
            &self.broker_root,
            &self.broker,
            &self.codex,
            &self.bwrap,
            &self.supervisor_instructions,
            &self.worker_instructions,
        ] {
            if !path.is_absolute() || path.components().any(|c| matches!(c, Component::ParentDir)) {
                return Err("profile paths must be absolute and normalised".into());
            }
        }
        let workspace = fs::canonicalize(&self.workspace)?;
        let broker_root = fs::canonicalize(&self.broker_root)?;
        if workspace != self.workspace
            || broker_root != self.broker_root
            || broker_root.starts_with(&workspace)
            || workspace.starts_with(&broker_root)
        {
            return Err("canonical broker storage and worker workspace must be disjoint".into());
        }
        for (value, cap) in [
            (self.concurrency, 2),
            (self.max_subagents, 2),
            (self.max_packages, 64),
            (self.max_repairs, 16),
            (self.max_recoveries, 16),
            (self.max_turns, 128),
        ] {
            if value == 0 || value > cap {
                return Err("finite profile count outside bounds".into());
            }
        }
        if !(1..=600).contains(&self.supervisor_seconds)
            || !(1..=2700).contains(&self.worker_seconds)
            || !(1..=14400).contains(&self.deadline_seconds)
        {
            return Err("finite profile time outside bounds".into());
        }
        for value in [&self.model, &self.subagent_model] {
            if value.is_empty() || value.len() > 256 || value.contains('\0') {
                return Err("invalid model identity".into());
            }
        }
        for effort in [&self.effort, &self.subagent_effort] {
            if !["low", "medium", "high", "xhigh"].contains(&effort.as_str()) {
                return Err("unsupported effort".into());
            }
        }
        if let Some(path) = &self.worker_scratch {
            let canonical = fs::canonicalize(path)?;
            if &canonical != path || !canonical.starts_with(&workspace) || !canonical.is_dir() {
                return Err(
                    "scratch must be a canonical directory within the reserved workspace".into(),
                );
            }
        }
        let known = dynamic_tools()
            .iter()
            .filter_map(|t| t["name"].as_str().map(String::from))
            .collect::<Vec<_>>();
        for selected in [&self.supervisor_tools, &self.worker_tools] {
            if selected.len() > known.len()
                || !selected.iter().any(|s| s == "bokkie_snapshot")
                || !selected.iter().any(|s| s == "bokkie_command")
                || selected.iter().any(|s| !known.contains(s))
            {
                return Err("tool profile must select snapshot/command and only supported bounded capabilities".into());
            }
        }
        if self
            .readonly_mcp_servers
            .iter()
            .any(|s| s != "openaiDeveloperDocs")
            || self.readonly_mcp_servers.len() > 1
        {
            return Err(
                "unsupported MCP capability; use bounded repository/UI CLI equivalents".into(),
            );
        }
        Ok(())
    }
    /// Check the database before opening SQLite or dispatching any writer.
    /// New files are allowed only beneath an existing canonical parent.
    pub fn validate_database(&self, path: &Path) -> RuntimeResult<()> {
        if !path.is_absolute() || path.components().any(|c| matches!(c, Component::ParentDir)) {
            return Err("database path must be absolute and normalised".into());
        }
        let parent = path.parent().ok_or("database path requires a parent")?;
        if fs::canonicalize(parent)? != parent {
            return Err("database parent must be canonical, without symlink aliases".into());
        }
        let canonical = match fs::symlink_metadata(path) {
            Ok(metadata) => {
                if !metadata.is_file() || fs::canonicalize(path)? != path {
                    return Err("database must be a canonical regular file".into());
                }
                fs::canonicalize(path)?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => path.to_path_buf(),
            Err(error) => return Err(error.into()),
        };
        if canonical.starts_with(fs::canonicalize(&self.workspace)?) {
            return Err("database must be outside the mutable worker workspace and scratch".into());
        }
        Ok(())
    }
    /// Server-owned bootstrap template. Intake sets only the natural-language
    /// intent; criteria remain empty until bounded supervisor formalisation.
    pub fn contract_template(&self, now: i64) -> RuntimeResult<EngineeringContract> {
        self.validate()?;
        Ok(EngineeringContract { intent:String::new(),criteria:vec![],
            permitted_scope:vec![self.workspace.to_string_lossy().into_owned()],
            prohibited_effects:vec!["No deployment, release, remote publication, credentials, global configuration changes, destructive data operations or additional authority".into()],
            authority:vec![],supervisor:self.instructions(false)?,worker:self.instructions(true)?,budget:self.budget(now,false) })
    }
    pub fn budget(&self, now: i64, worker: bool) -> EngineeringBudget {
        EngineeringBudget {
            max_turns: self.max_turns,
            max_packages: self.max_packages,
            max_repairs: self.max_repairs,
            max_recoveries: self.max_recoveries,
            max_checkpoints: 512,
            max_questions: 64,
            max_concurrent_workers: self.concurrency,
            turn_seconds: if worker {
                self.worker_seconds
            } else {
                self.supervisor_seconds
            },
            deadline: now.saturating_add(self.deadline_seconds),
        }
    }
    fn instructions(&self, worker: bool) -> RuntimeResult<EngineeringInstructions> {
        let path = if worker {
            &self.worker_instructions
        } else {
            &self.supervisor_instructions
        };
        let bytes = bounded_read(path, MAX_FILE)?;
        let text = String::from_utf8(bytes.clone())?;
        Ok(EngineeringInstructions {
            text,
            digest: sha(&bytes),
            context_digests: vec![sha(&bytes)],
            profile_digest: sha(&serde_json::to_vec(self)?),
            adapter_id: "codex-broker-v1".into(),
        })
    }
}

pub fn intake(
    store: &mut Store,
    profile: &EngineeringRuntimeProfile,
    intent: String,
    command_id: String,
    now: i64,
) -> RuntimeResult<EngineeringCommandReceipt> {
    if let Some(receipt) =
        store.engineering_operator_intake_receipt(&command_id, &intent, "local-intake")?
    {
        return Ok(receipt);
    }
    let mut contract = profile.contract_template(now)?;
    contract.intent = intent;
    Ok(store.engineering_command(
        EngineeringActor::Operator {
            name: "local-intake".into(),
        },
        EngineeringCommandEnvelope {
            command_id,
            expected: None,
            command: EngineeringCommand::CreateOutcome { contract },
        },
        now,
    )?)
}

pub fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn bounded_read(path: &Path, bound: u64) -> RuntimeResult<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)?.take(bound + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > bound {
        return Err("bounded artefact exceeded; use a smaller retained artefact".into());
    }
    Ok(bytes)
}
fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> RuntimeResult<T> {
    Ok(serde_json::from_slice(&bounded_read(path, MAX_FILE)?)?)
}
fn atomic<T: Serialize>(path: &Path, value: &T) -> RuntimeResult<()> {
    let bytes = serde_json::to_vec(value)?;
    if bytes.len() as u64 > MAX_FILE {
        return Err("bounded adapter record exceeded".into());
    }
    let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    fs::rename(&temporary, path)?;
    File::open(path.parent().ok_or("missing parent")?)?.sync_all()?;
    Ok(())
}
fn snapshot(store: &Store, id: &str) -> RuntimeResult<EngineeringOutcomeSnapshot> {
    store
        .engineering_outcome(id)?
        .ok_or_else(|| "outcome disappeared".into())
}
fn actor(execution: &EngineeringExecution) -> EngineeringActor {
    match execution.role {
        EngineeringRole::Supervisor => EngineeringActor::Supervisor {
            execution_id: execution.id.clone(),
            claim: execution.claim.clone(),
        },
        EngineeringRole::Worker => EngineeringActor::Worker {
            execution_id: execution.id.clone(),
            claim: execution.claim.clone(),
        },
    }
}
#[derive(Clone, Debug, Deserialize, Serialize)]
struct Event {
    sequence: u64,
    kind: String,
    value: Value,
}
#[derive(Serialize)]
struct ObservedReview<'a> {
    reviewer_thread_id: &'a str,
    reviewer_turn_id: Option<&'a str>,
    agent_path: Option<&'a str>,
    report_digest: String,
    #[serde(skip)]
    message: &'a str,
    #[serde(skip)]
    provenance: Vec<&'a Event>,
}

fn observed_reviews(log: &[Event]) -> Vec<ObservedReview<'_>> {
    let Some(root) = log
        .iter()
        .find(|event| event.kind == "thread_identity")
        .and_then(|event| event.value["thread_id"].as_str())
    else {
        return vec![];
    };
    let mut reviews = vec![];
    for completed in log.iter().rev().filter(|event| {
        event.kind == "turn/completed" && event.value["turn"]["status"] == "completed"
    }) {
        let (Some(thread), Some(turn)) = (
            completed.value["threadId"].as_str(),
            completed.value["turn"]["id"].as_str(),
        ) else {
            continue;
        };
        if thread == root {
            continue;
        }
        let Some(parent) = log.iter().find(|event| {
            matches!(event.kind.as_str(), "item/started" | "item/completed")
                && event.value["threadId"] == root
                && event.value["item"]["type"] == "subAgentActivity"
                && event.value["item"]["kind"] == "started"
                && event.value["item"]["agentThreadId"] == thread
                && event.sequence < completed.sequence
        }) else {
            continue;
        };
        let Some(final_item) = log.iter().rev().find(|event| {
            event.kind == "item/completed"
                && event.value["threadId"] == thread
                && event.value["turnId"] == turn
                && event.value["item"]["type"] == "agentMessage"
                && event.value["item"]["phase"] == "final_answer"
                && event.sequence > parent.sequence
                && event.sequence < completed.sequence
        }) else {
            continue;
        };
        let Some(message) = final_item.value["item"]["text"].as_str() else {
            continue;
        };
        reviews.push(ObservedReview {
            reviewer_thread_id: thread,
            reviewer_turn_id: Some(turn),
            agent_path: parent.value["item"]["agentPath"].as_str(),
            report_digest: sha(message.as_bytes()),
            message,
            provenance: vec![parent, final_item, completed],
        });
    }
    // Older app-server versions include the completed child's own report in
    // the root's collaboration receipt instead of separate child turn events.
    for event in log.iter().rev().filter(|event| {
        event.kind == "item/completed"
            && event.value["threadId"] == root
            && event.value["item"]["type"] == "collabAgentToolCall"
    }) {
        let Some(states) = event.value["item"]["agentsStates"].as_object() else {
            continue;
        };
        for (thread, state) in states {
            if thread == root
                || state["status"] != "completed"
                || reviews
                    .iter()
                    .any(|review| review.reviewer_thread_id == thread)
            {
                continue;
            }
            let Some(message) = state["message"].as_str() else {
                continue;
            };
            reviews.push(ObservedReview {
                reviewer_thread_id: thread,
                reviewer_turn_id: None,
                agent_path: None,
                report_digest: sha(message.as_bytes()),
                message,
                provenance: vec![event],
            });
        }
    }
    reviews.truncate(64);
    reviews
}

fn events(directory: &Path) -> RuntimeResult<Vec<Event>> {
    Ok(read_events(directory)?.0)
}
fn read_events(directory: &Path) -> RuntimeResult<(Vec<Event>, Vec<u8>)> {
    let path = directory.join("events.jsonl");
    if !path.exists() {
        return Ok((vec![], vec![]));
    }
    let bytes = bounded_read(&path, MAX_SPOOL)?;
    let mut result = vec![];
    // Ignore only an in-progress final append. A stopped broker with a torn tail
    // has no cessation event and therefore retains ownership.
    for line in bytes.split_inclusive(|b| *b == b'\n') {
        if !line.ends_with(b"\n") {
            break;
        }
        let event: Event = serde_json::from_slice(line)?;
        if event.sequence != result.len() as u64 + 1 {
            return Err("broker event sequence gap".into());
        }
        result.push(event);
    }
    Ok((result, bytes))
}
fn file_inspection(artefact: EngineeringArtefact, bytes: &[u8]) -> Value {
    match std::str::from_utf8(bytes) {
        Ok(text) if !bytes.contains(&0) => {
            json!({"artefact":artefact,"content_kind":"text","encoding":"utf8","content":text})
        }
        text => {
            let reason = if text.is_err() {
                "not_utf8"
            } else {
                "contains_nul"
            };
            json!({"artefact":artefact,"content_kind":"binary","content":null,
                "identity_verified":true,"binary_reason":reason,"raw_evidence_digest":sha(bytes),
                "inspection_note":"All bytes were read and their length and SHA-256 verified. Assess binary assets through provenance, exact identities and relevant validation; do not read their raw bytes wholesale. Explicit byte-range access remains available through bokkie_evidence when required."})
        }
    }
}
fn tool(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    json!({"type":"function", "name":name, "description":description,
        "inputSchema":{"type":"object", "properties":properties, "required":required,"additionalProperties":false}})
}
fn dynamic_tools() -> Vec<Value> {
    vec![
        tool(
            "bokkie_snapshot",
            "Read durable intent, contract, messages, packages, questions, submissions and precondition. Read again after every mutation or conflict; reconsider your decision.",
            json!({}),
            &[],
        ),
        tool(
            "bokkie_command",
            "Issue a typed engineering command with the exact precondition you read. No actor field. Worker submit_result queues a submission and stops this execution; completion is never acceptance. Supervisor may formalise_contract, create_package, resolve_question, assess_result, create_repair, request_cancellation for a current-contract package_id, yield_supervisor, finish_outcome or ask_question. Whole-outcome cancellation requires the operator.",
            json!({"expected":{"type":"object"},"command":{"type":"object"}}),
            &["expected", "command"],
        ),
        tool(
            "bokkie_question",
            "Ask Bokkie a durable question in any Codex mode. Routine questions wait for the scheduled supervisor's saved answer; Supervisor missing_information routes an unavailable fact to the operator without widening authority; new_authority requests additional rights. Both stop the execution for operator attention. No originating chat required.",
            json!({"prompt":{"type":"string"},"kind":{"type":"string","enum":["routine","missing_information","new_authority"]},"options":{"type":"array","items":{"type":"string"},"maxItems":3}}),
            &["prompt", "kind", "options"],
        ),
        tool(
            "bokkie_file",
            "Inspect an exact relative workspace file. Read and retain all bytes; return actual digest/size plus complete UTF-8 text or compact binary identity metadata. Binary assets need provenance and validation, not wholesale raw-byte reading; explicit ranges remain available through bokkie_evidence. No invented hashes.",
            json!({"path":{"type":"string"}}),
            &["path"],
        ),
        tool(
            "bokkie_inspect",
            "Verify an exact EngineeringArtefact (file or Git commit/tree). File identity is checked against all bytes. Return complete UTF-8 text or compact binary metadata without lossy decoding. Inspect binary provenance and relevant validation, not wholesale font/image bytes. Textual source/review still requires exact content inspection.",
            json!({"artefact":{"type":"object"}}),
            &["artefact"],
        ),
        tool(
            "bokkie_evidence",
            "Read an exact bounded page of retained evidence by SHA-256 digest. Optional byte_offset defaults to 0 and max_bytes to 32768 (range 4..32768). Follow next_byte_offset until null; partial pages are not complete evidence. The encoding is utf8 or base64, with exact byte length and total size. Assessment requires inspecting all relevant evidence.",
            json!({"digest":{"type":"string"},"byte_offset":{"type":"integer","minimum":0},"max_bytes":{"type":"integer","minimum":4,"maximum":MAX_EVIDENCE_PAGE}}),
            &["digest"],
        ),
        tool(
            "bokkie_review",
            "Discover observed reviewer IDs with bokkie_commands, then register by reviewer_thread_id and optional reviewer_turn_id. Canonical task paths such as /root/review are not thread IDs. Its final message must be bare JSON with exact artefacts, verdict (pass or repair), and findings (string list). Report text comes from actual collaboration events.",
            json!({"reviewer_thread_id":{"type":"string"},"reviewer_turn_id":{"type":"string"}}),
            &["reviewer_thread_id"],
        ),
        tool(
            "bokkie_commands",
            "Return commands (actual completed command item IDs, commands and exit codes) and reviewer_candidates (observed independent thread/turn IDs and task paths). Use these actual protocol identities for validation and review registration.",
            json!({}),
            &[],
        ),
        tool(
            "bokkie_validation",
            "Retain actual completed command execution evidence from this broker. Supply command item ID, exact inspected artefact and criterion ID; returns measured command/output hashes and exit status.",
            json!({"item_id":{"type":"string"},"artefact":{"type":"object"},"criterion_id":{"type":"string"}}),
            &["item_id", "artefact", "criterion_id"],
        ),
    ]
}

/// One controller at a time per private spool. Brokers are separately locked and
/// survive this controller, including its lock release after a crash.
pub struct EngineeringRuntime {
    pub profile: EngineeringRuntimeProfile,
    _lock: File,
}
impl EngineeringRuntime {
    pub fn new(profile: EngineeringRuntimeProfile) -> RuntimeResult<Self> {
        profile.validate()?;
        fs::set_permissions(&profile.broker_root, fs::Permissions::from_mode(0o700))?;
        let lock = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(profile.broker_root.join("controller.lock"))?;
        // SAFETY: flock operates on the live descriptor retained for our lifetime.
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err("another engineering controller owns this profile".into());
        }
        fs::create_dir_all(profile.broker_root.join("blobs"))?;
        fs::create_dir_all(profile.broker_root.join("reviews"))?;
        File::open(&profile.broker_root)?.sync_all()?;
        Ok(Self {
            profile,
            _lock: lock,
        })
    }
    fn blob(&self, bytes: &[u8]) -> RuntimeResult<String> {
        let hash = sha(bytes);
        let path = self.profile.broker_root.join("blobs").join(&hash);
        if path.exists() {
            if sha(&bounded_read(&path, MAX_SPOOL)?) != hash {
                return Err("retained blob is corrupt".into());
            }
        } else {
            let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .mode(0o600)
                .open(&temporary)?;
            file.write_all(bytes)?;
            file.sync_all()?;
            fs::rename(&temporary, &path)?;
            File::open(path.parent().unwrap())?.sync_all()?;
        }
        Ok(hash)
    }
    fn evidence(&self, hash: &str) -> RuntimeResult<Vec<u8>> {
        self.evidence_with_bound(hash, MAX_FILE)
    }
    fn evidence_with_bound(&self, hash: &str, bound: u64) -> RuntimeResult<Vec<u8>> {
        if hash.len() != 64 || !hash.bytes().all(|c| c.is_ascii_hexdigit()) {
            return Err("invalid evidence digest".into());
        }
        let bytes = bounded_read(&self.profile.broker_root.join("blobs").join(hash), bound)?;
        if sha(&bytes) != hash {
            return Err("evidence digest mismatch".into());
        }
        Ok(bytes)
    }
    fn evidence_page(&self, args: &Value) -> RuntimeResult<Value> {
        let hash = args["digest"].as_str().ok_or("missing digest")?;
        let number = |name: &str, default: u64| -> RuntimeResult<u64> {
            args.get(name)
                .map(|value| {
                    value
                        .as_u64()
                        .ok_or_else(|| format!("{name} must be a non-negative integer").into())
                })
                .unwrap_or(Ok(default))
        };
        let offset = number("byte_offset", 0)?;
        let limit = number("max_bytes", MAX_EVIDENCE_PAGE)?;
        if !(4..=MAX_EVIDENCE_PAGE).contains(&limit) {
            return Err("max_bytes must be between 4 and 32768".into());
        }
        // Broker journals already have a 16 MiB retention bound. Verify the
        // complete identity before returning any page, without widening the
        // ordinary full-record or source artefact limits.
        let bytes = self.evidence_with_bound(hash, MAX_SPOOL)?;
        if offset > bytes.len() as u64 {
            return Err("byte_offset exceeds retained evidence length".into());
        }
        let start = offset as usize;
        let mut end = bytes.len().min(start + limit as usize);
        let (encoding, content) = if let Ok(text) = std::str::from_utf8(&bytes) {
            if !text.is_char_boundary(start) {
                let mut boundary = start;
                while !text.is_char_boundary(boundary) {
                    boundary -= 1;
                }
                return Err(
                    format!("byte_offset splits UTF-8; retry with byte_offset {boundary}").into(),
                );
            }
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            ("utf8", text[start..end].to_owned())
        } else {
            ("base64", STANDARD.encode(&bytes[start..end]))
        };
        Ok(json!({"digest":hash,"encoding":encoding,"content":content,
            "byte_offset":start,"byte_length":end-start,"total_bytes":bytes.len(),
            "partial":start != 0 || end != bytes.len(),
            "next_byte_offset":if end < bytes.len() {Some(end)} else {None}}))
    }
    fn tool_reply(
        &self,
        path: &Path,
        id: &Value,
        result: RuntimeResult<Value>,
    ) -> RuntimeResult<()> {
        let (mut success, body) = match result {
            Ok(value) => (true, value),
            Err(error) => (false, json!({"error":error.to_string()})),
        };
        let bytes = serde_json::to_vec(&body)?;
        let response = |success, text: String| {
            json!({"id":id,"result":{"success":success,
            "contentItems":[{"type":"inputText","text":text}]}})
        };
        let mut reply = response(success, body.to_string());
        if serde_json::to_vec(&reply)?.len() as u64 > MAX_TOOL_REPLY {
            let descriptor = if bytes.len() as u64 <= MAX_SPOOL {
                json!({"response_paged":true,"digest":self.blob(&bytes)?,"total_bytes":bytes.len(),
                    "instruction":"Complete tool response retained. Read it with bokkie_evidence using this digest and follow next_byte_offset until null. A mutation may already be committed; inspect the retained result before retrying."})
            } else {
                success = false;
                json!({"error":"Tool response exceeds the retained record bound. Request a smaller result; no partial response was returned. A mutation may already be committed; inspect its durable state before retrying."})
            };
            reply = response(success, descriptor.to_string());
        }
        if serde_json::to_vec(&reply)?.len() as u64 > MAX_TOOL_REPLY {
            return Err("request identity leaves no room for a bounded tool reply".into());
        }
        atomic(path, &reply)
    }
    fn file(&self, relative: &str) -> RuntimeResult<(EngineeringArtefact, Vec<u8>)> {
        let path = Path::new(relative);
        if path.is_absolute()
            || path
                .components()
                .any(|c| !matches!(c, Component::Normal(_)))
        {
            return Err("artefact must be a relative in-workspace file".into());
        }
        let absolute = fs::canonicalize(self.profile.workspace.join(path))?;
        if !absolute.starts_with(&self.profile.workspace) || !absolute.is_file() {
            return Err("artefact escaped workspace".into());
        }
        let bytes = bounded_read(&absolute, MAX_FILE)?;
        let hash = self.blob(&bytes)?;
        Ok((
            EngineeringArtefact::File {
                store: self.profile.workspace.to_string_lossy().into_owned(),
                path: relative.into(),
                bytes: bytes.len() as u64,
                sha256: hash,
            },
            bytes,
        ))
    }
    fn inspect(&self, artefact: &EngineeringArtefact) -> RuntimeResult<Value> {
        match artefact {
            EngineeringArtefact::File { store, path, .. } => {
                if Path::new(store) != self.profile.workspace {
                    return Err("unregistered artefact store".into());
                }
                let (actual, bytes) = self.file(path)?;
                if &actual != artefact {
                    return Err("source file no longer matches submission".into());
                }
                Ok(file_inspection(actual, &bytes))
            }
            EngineeringArtefact::Git {
                repository,
                commit,
                tree,
            } => {
                if Path::new(repository) != self.profile.workspace
                    || ![40, 64].contains(&commit.len())
                    || !commit.bytes().all(|c| c.is_ascii_hexdigit())
                {
                    return Err("unregistered Git repository or invalid exact commit".into());
                }
                let run = |arg: &str| -> RuntimeResult<Vec<u8>> {
                    let output = Command::new("git")
                        .env("GIT_NO_REPLACE_OBJECTS", "1")
                        .args([
                            "--no-pager",
                            "-c",
                            "core.hooksPath=/dev/null",
                            "-C",
                            repository,
                            "rev-parse",
                            "--verify",
                            arg,
                        ])
                        .output()?;
                    if !output.status.success() {
                        return Err("exact Git artefact unavailable".into());
                    }
                    Ok(output.stdout)
                };
                let actual_tree = run(&format!("{commit}^{{tree}}"))?;
                let actual_commit = run(&format!("{commit}^{{commit}}"))?;
                if String::from_utf8(actual_tree)?.trim() != tree
                    || String::from_utf8(actual_commit)?.trim() != commit
                {
                    return Err("Git identity mismatch".into());
                }
                let output = Command::new("git")
                    .env("GIT_NO_REPLACE_OBJECTS", "1")
                    .args([
                        "--no-pager",
                        "-C",
                        repository,
                        "show",
                        "--no-ext-diff",
                        "--no-textconv",
                        "--format=fuller",
                        "--stat",
                        commit,
                    ])
                    .output()?;
                if !output.status.success() || output.stdout.len() as u64 > MAX_FILE {
                    return Err("Git inspection failed or exceeded bound".into());
                }
                let hash = self.blob(&output.stdout)?;
                Ok(
                    json!({"artefact":artefact,"inspection_digest":hash,"content":String::from_utf8_lossy(&output.stdout)}),
                )
            }
        }
    }
    fn verify_submission(&self, input: &EngineeringSubmissionInput) -> RuntimeResult<()> {
        for artefact in &input.artefacts {
            self.inspect(artefact)?;
        }
        for evidence in &input.evidence {
            if !input.artefacts.contains(&evidence.artefact) {
                return Err("validation source not in submission".into());
            }
            self.inspect(&evidence.artefact)?;
            self.evidence(&evidence.command_digest)?;
            self.evidence(&evidence.output_digest)?;
            let mut observed = false;
            for directory in fs::read_dir(&self.profile.broker_root)? {
                let receipts = directory?.path().join("receipts");
                if !receipts.is_dir() {
                    continue;
                }
                for entry in fs::read_dir(receipts)? {
                    let path = entry?.path();
                    if path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with("validation-"))
                    {
                        let actual: EngineeringCriterionEvidence = read_json(&path)?;
                        if &actual == evidence {
                            observed = true;
                            break;
                        }
                    }
                }
                if observed {
                    break;
                }
            }
            if !observed {
                return Err("validation is not an actual recorded command observation".into());
            }
        }
        Ok(())
    }
    fn verify_review(
        &self,
        review: &EngineeringReviewEvidence,
        execution: &EngineeringExecution,
    ) -> RuntimeResult<()> {
        if review.reviewer_identity == execution.id || review.reviewer_identity.is_empty() {
            return Err("independent review must have a separate identity".into());
        }
        for artefact in &review.artefacts {
            self.inspect(artefact)?;
        }
        self.evidence(&review.evidence_digest)?;
        let registered: EngineeringReviewEvidence = read_json(
            &self
                .profile
                .broker_root
                .join("reviews")
                .join(format!("{}.json", review.evidence_digest)),
        )?;
        if &registered != review {
            return Err(
                "review is not attributed to a retained independent Codex execution".into(),
            );
        }
        Ok(())
    }
    fn execution_limits(
        &self,
        state: &EngineeringOutcomeSnapshot,
        execution: &EngineeringExecution,
        now: i64,
    ) -> RuntimeResult<(i64, i64)> {
        let budget = if let Some(id) = &execution.package_id {
            &state
                .packages
                .iter()
                .find(|p| p.id == *id)
                .ok_or("missing execution package")?
                .input
                .budget
        } else {
            &state.contract().budget
        };
        // The initial claim reserves the complete bounded turn. A delayed
        // dispatch or reconnect must never reset its start time.
        let deadline = budget
            .deadline
            .min(state.contract().budget.deadline)
            .min(execution.claim.lease_expires_at);
        let profile_seconds = if execution.role == EngineeringRole::Worker {
            self.profile.worker_seconds
        } else {
            self.profile.supervisor_seconds
        };
        let seconds = profile_seconds
            .min(budget.turn_seconds)
            .min(deadline.saturating_sub(now));
        if seconds <= 0 {
            return Err("execution deadline exhausted before dispatch".into());
        }
        Ok((seconds, deadline))
    }
    fn dispatch(
        &self,
        state: &EngineeringOutcomeSnapshot,
        execution: &EngineeringExecution,
        now: i64,
    ) -> RuntimeResult<()> {
        let directory = self.profile.broker_root.join(&execution.id);
        fs::create_dir_all(directory.join("replies"))?;
        fs::create_dir_all(directory.join("receipts"))?;
        File::open(&self.profile.broker_root)?.sync_all()?;
        if !directory.join("dispatch.json").exists() {
            if execution.contract_revision != state.contract_revision || execution.fenced {
                return Err("cannot dispatch fenced intent".into());
            }
            let worker = execution.role == EngineeringRole::Worker;
            if worker && execution.workspace.as_deref() != self.profile.workspace.to_str() {
                return Err("package workspace is not registered in this profile".into());
            }
            if execution.instructions.profile_digest != sha(&serde_json::to_vec(&self.profile)?)
                || execution.instructions.digest != sha(execution.instructions.text.as_bytes())
            {
                return Err("execution instruction/profile identity mismatch".into());
            }
            let (seconds, deadline) = self.execution_limits(state, execution, now)?;
            let params = json!({"cwd":self.profile.workspace,"model":self.profile.model,
                "allowProviderModelFallback":false,"approvalPolicy":"on-request","approvalsReviewer":"user",
                "sandbox":if worker {"workspace-write"} else {"read-only"},
                "runtimeWorkspaceRoots":[self.profile.workspace],
                "config":{"model_reasoning_effort":self.profile.effort},
                "developerInstructions":execution.instructions.text,"dynamicTools":dynamic_tools().into_iter().filter(|t|if worker {&self.profile.worker_tools}else{&self.profile.supervisor_tools}.iter().any(|name|t["name"]==name.as_str())).collect::<Vec<_>>()});
            let prompt = json!({"role":execution.role,"execution_id":execution.id,"package_id":execution.package_id,
                "snapshot":state,"expected":state.precondition(),"workspace":self.profile.workspace,
                "worker_budget_template":self.profile.budget(now,true),"task_profile":self.profile,
                "protocol":"Use bokkie_snapshot and bokkie_command for all durable decisions. Commands are EngineeringCommand JSON tagged kind/input, expected is the snapshot precondition. Read the engineering domain schema supplied below. Do not invent actors, evidence, authority or cessation. Worker submit_result stops your turn and queues evidence for supervisor assessment.",
                "command_types": include_str!("engineering.rs")}).to_string();
            let manifest = json!({"execution_id":execution.id,"dispatch_key":execution.dispatch_key,
                "role":execution.role,"workspace":self.profile.workspace,"model":self.profile.model,
                "effort":self.profile.effort,"subagent_model":self.profile.subagent_model,
                "subagent_effort":self.profile.subagent_effort,"max_subagents":self.profile.max_subagents,
                "turn_seconds":seconds,"deadline":deadline,"codex":self.profile.codex,"bwrap":self.profile.bwrap,
                "worker_network_access":self.profile.worker_network_access,"worker_scratch":self.profile.worker_scratch,
                "readonly_mcp_servers":self.profile.readonly_mcp_servers,"allow_single_pwd_approval":self.profile.allow_single_pwd_approval,
                "thread_params":params,"prompt":prompt,"instructions":execution.instructions,
                "codex_digest":sha(&bounded_read(&self.profile.codex,512*1024*1024)?),
                "broker_digest":sha(&bounded_read(&self.profile.broker,MAX_FILE)?)});
            atomic(&directory.join("observed.json"), &state.precondition())?;
            atomic(&directory.join("dispatch.json"), &manifest)?;
        }
        if !events(&directory)?
            .iter()
            .any(|e| e.kind == "launch_committed")
        {
            let status = Command::new("python3")
                .arg(&self.profile.broker)
                .arg("launch")
                .arg(&directory)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .status()?;
            if !status.success() {
                return Err("broker launcher failed; retain dispatch intent".into());
            }
        }
        Ok(())
    }
    /// Drive an explicit isolated database. No global scheduler, daemon or HTTP mutation.
    pub fn tick(&self, store: &mut Store, now: i64) -> RuntimeResult<Value> {
        let ids = store.engineering_outcome_ids(500)?;
        if ids.len() == 500 {
            return Err("outcome enumeration reached its bound; paginated Store integration required before dispatch".into());
        }
        let profile_digest = sha(&serde_json::to_vec(&self.profile)?);
        for id in &ids {
            let state = snapshot(store, id)?;
            if !state.root.state.is_terminal()
                && state.contract().supervisor.profile_digest != profile_digest
            {
                return Err("database contains a different task profile; isolate the database or add profile-filtered Store claims".into());
            }
        }
        store.recover_expired_leases(now)?;
        let mut errors = vec![];
        for id in &ids {
            let state = snapshot(store, id)?;
            for execution in &state.executions {
                if let Err(error) = self.reconcile(store, &state, execution, now) {
                    errors.push(json!({"execution_id":execution.id,"error":error.to_string()}));
                }
            }
        }
        // Backend gates concurrency, dependency readiness, reservations and budget.
        let workers = store.claim_due_engineering(
            EngineeringRole::Worker,
            now,
            self.profile.worker_seconds,
            self.profile.concurrency as usize,
        )?;
        let mut supervisor_active = false;
        for id in &ids {
            let state = snapshot(store, id)?;
            supervisor_active |= state
                .executions
                .iter()
                .any(|e| e.role == EngineeringRole::Supervisor && !e.cessation_verified);
        }
        let supervisors = if supervisor_active {
            vec![]
        } else {
            store.claim_due_engineering(
                EngineeringRole::Supervisor,
                now,
                self.profile.supervisor_seconds,
                1,
            )?
        };
        let dispatched = workers.len() + supervisors.len();
        for claim in workers.into_iter().chain(supervisors) {
            let state = snapshot(store, &claim.outcome_id)?;
            let execution = state
                .executions
                .iter()
                .find(|e| e.id == claim.execution_id)
                .ok_or("missing dispatch")?;
            if let Err(error) = self.dispatch(&state, execution, now) {
                errors.push(json!({"execution_id":execution.id,"error":error.to_string()}));
            }
        }
        Ok(json!({"dispatched":dispatched,"errors":errors}))
    }
    #[allow(clippy::too_many_arguments)] // One immutable execution/event activity capsule.
    fn activity(
        &self,
        store: &mut Store,
        directory: &Path,
        execution: &EngineeringExecution,
        key: &str,
        command: EngineeringCommand,
        reconciler: bool,
        now: i64,
    ) -> RuntimeResult<EngineeringCommandReceipt> {
        // Activity retries are scoped to one external event. Persist the exact
        // envelope before Store, and replay it before considering a new fence.
        for attempt in 0..3 {
            let path = directory
                .join("receipts")
                .join(format!("{key}-{attempt}.json"));
            let envelope = if path.exists() {
                read_json(&path)?
            } else {
                let state = snapshot(store, &execution_outcome(directory)?)?;
                let envelope = EngineeringCommandEnvelope {
                    command_id: format!("{}:{key}:{attempt}", execution.id),
                    expected: Some(state.precondition()),
                    command: command.clone(),
                };
                atomic(&path, &envelope)?;
                envelope
            };
            let trusted = if reconciler {
                EngineeringActor::Reconciler {
                    adapter_id: execution.instructions.adapter_id.clone(),
                }
            } else {
                actor(execution)
            };
            match store.engineering_command(trusted, envelope, now) {
                Ok(receipt) => return Ok(receipt),
                Err(StoreError::Fenced) if attempt < 2 => continue,
                Err(error) => return Err(error.into()),
            }
        }
        Err("activity retry budget exhausted".into())
    }
    fn command(
        &self,
        store: &mut Store,
        directory: &Path,
        execution: &EngineeringExecution,
        key: &str,
        args: Value,
        now: i64,
    ) -> RuntimeResult<Value> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Decision {
            expected: EngineeringPrecondition,
            command: EngineeringCommand,
        }
        let mut decision: Decision = serde_json::from_value(args)?;
        if let EngineeringCommand::AskQuestion { request_key, .. } = &mut decision.command {
            *request_key = key.into();
        }
        let path = directory
            .join("receipts")
            .join(format!("decision-{key}.json"));
        let command_id = format!("{}:{key}", execution.id);
        let envelope = EngineeringCommandEnvelope {
            command_id,
            expected: Some(decision.expected.clone()),
            command: decision.command.clone(),
        };
        if path.exists() {
            let saved: EngineeringCommandEnvelope = read_json(&path)?;
            if serde_json::to_vec(&saved)? != serde_json::to_vec(&envelope)? {
                return Err("protocol request replay conflict".into());
            }
            // Store checks receipts before fences. Do not re-inspect mutable files
            // before retrieving an already committed decision receipt.
            match store.engineering_command(actor(execution), saved, now) {
                Ok(receipt) => return Ok(json!(receipt)),
                Err(error) => return Err(error.into()),
            }
        }
        let observed: EngineeringPrecondition = read_json(&directory.join("observed.json"))?;
        if decision.expected != observed {
            return Err(
                "decision does not bind the snapshot actually read; call bokkie_snapshot".into(),
            );
        }
        let state = snapshot(store, &decision.expected.outcome_id)?;
        if state.precondition() != decision.expected {
            return Err(
                "snapshot changed; read and reconsider decision, do not reuse acceptance".into(),
            );
        }
        match &decision.command {
            EngineeringCommand::SubmitResult(input)
                if execution.role == EngineeringRole::Worker =>
            {
                store.engineering_submission_preflight(&state.id, &execution.id, input)?;
                self.verify_submission(input)?;
                // Durable result intent is separate from a successful Store result.
                atomic(&directory.join("submission.json"), input)?;
                atomic(
                    &directory.join("cancel.json"),
                    &json!({"reason":"submission queued; reap writer before import"}),
                )?;
                return Ok(
                    json!({"submitted":false,"queued":true,"acceptance":"pending supervisor assessment after cessation"}),
                );
            }
            EngineeringCommand::AskQuestion { .. } => {}
            EngineeringCommand::FormaliseContract { .. }
            | EngineeringCommand::ResolveQuestion { .. }
            | EngineeringCommand::RequestCancellation {
                package_id: Some(_),
            }
            | EngineeringCommand::YieldSupervisor { .. }
                if execution.role == EngineeringRole::Supervisor => {}
            EngineeringCommand::CreatePackage(input)
            | EngineeringCommand::CreateRepair {
                replacement: input, ..
            } if execution.role == EngineeringRole::Supervisor => {
                if Path::new(&input.workspace) != self.profile.workspace {
                    return Err("package must use the registered canonical workspace".into());
                }
                if input.budget.turn_seconds > self.profile.worker_seconds
                    || input.budget.deadline > state.contract().budget.deadline
                {
                    return Err("package exceeds runtime profile".into());
                }
                for artefact in &input.inputs {
                    self.inspect(artefact)?;
                }
            }
            EngineeringCommand::AssessResult(input)
                if execution.role == EngineeringRole::Supervisor =>
            {
                let submission = state
                    .submissions
                    .iter()
                    .find(|s| s.id == input.submission_id)
                    .ok_or("unknown submission")?;
                self.verify_submission(&submission.input)?;
                self.verify_review(&input.review, execution)?;
            }
            EngineeringCommand::FinishOutcome { review, .. }
                if execution.role == EngineeringRole::Supervisor =>
            {
                self.verify_review(review, execution)?;
            }
            _ => return Err("command is outside this model role's adapter authority".into()),
        }
        atomic(&path, &envelope)?;
        let receipt = store.engineering_command(actor(execution), envelope, now)?;
        if matches!(
            decision.command,
            EngineeringCommand::FormaliseContract { .. }
                | EngineeringCommand::AskQuestion {
                    kind: EngineeringQuestionKind::NewAuthority
                        | EngineeringQuestionKind::MissingInformation,
                    ..
                }
                | EngineeringCommand::YieldSupervisor { .. }
                | EngineeringCommand::FinishOutcome { .. }
        ) {
            atomic(
                &directory.join("cancel.json"),
                &json!({"reason":"supervisor command ended current authority"}),
            )?;
        }
        Ok(json!(receipt))
    }
    #[allow(clippy::too_many_arguments)] // Protocol dispatch retains the exact event and execution.
    fn dynamic(
        &self,
        store: &mut Store,
        directory: &Path,
        execution: &EngineeringExecution,
        key: &str,
        params: &Value,
        log: &[Event],
        now: i64,
    ) -> RuntimeResult<Value> {
        let args = &params["arguments"];
        let name = params["tool"].as_str().ok_or("missing tool name")?;
        let permitted = if execution.role == EngineeringRole::Supervisor {
            &self.profile.supervisor_tools
        } else {
            &self.profile.worker_tools
        };
        if !permitted.iter().any(|tool| tool == name) {
            return Err("tool excluded by task profile".into());
        }
        match name {
            "bokkie_snapshot" => {
                let state = snapshot(store, &execution_outcome(directory)?)?;
                atomic(&directory.join("observed.json"), &state.precondition())?;
                let mut reviews = vec![];
                for entry in fs::read_dir(self.profile.broker_root.join("reviews"))?.take(64) {
                    let review: EngineeringReviewEvidence = read_json(&entry?.path())?;
                    if state
                        .submissions
                        .iter()
                        .any(|s| s.input.artefacts == review.artefacts)
                    {
                        reviews.push(review);
                    }
                }
                Ok(
                    json!({"snapshot":state,"expected":state.precondition(),"registered_reviews":reviews,"reviewer_candidates":observed_reviews(log)}),
                )
            }
            "bokkie_command" => self.command(store, directory, execution, key, args.clone(), now),
            "bokkie_file" => {
                let (artefact, bytes) = self.file(args["path"].as_str().ok_or("missing path")?)?;
                Ok(file_inspection(artefact, &bytes))
            }
            "bokkie_inspect" => self.inspect(&serde_json::from_value(args["artefact"].clone())?),
            "bokkie_evidence" => self.evidence_page(args),
            "bokkie_review" => {
                let reviewer = args["reviewer_thread_id"]
                    .as_str()
                    .ok_or("missing independent thread identity")?;
                let candidates = observed_reviews(log);
                let selected_turn = args["reviewer_turn_id"].as_str();
                let candidate = candidates.iter().find(|candidate| candidate.reviewer_thread_id == reviewer
                    && selected_turn.is_none_or(|turn| candidate.reviewer_turn_id == Some(turn)))
                    .ok_or_else(|| format!("Independent reviewer has no observed completed report for that identity. Use bokkie_commands reviewer_candidates; observed candidates: {}",serde_json::to_string(&candidates).unwrap_or_default()))?;
                let message = candidate.message;
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct ReviewReport {
                    artefacts: Vec<EngineeringArtefact>,
                    verdict: String,
                    findings: Vec<String>,
                }
                let report: ReviewReport = serde_json::from_str(message)?;
                if report.artefacts.is_empty()
                    || report.artefacts.len() > 64
                    || report.findings.len() > 64
                    || !["pass", "repair"].contains(&report.verdict.as_str())
                {
                    return Err("invalid bounded independent review report".into());
                }
                for artefact in &report.artefacts {
                    self.inspect(artefact)?;
                }
                let provenance_digest = self.blob(&serde_json::to_vec(&candidate.provenance)?)?;
                atomic(
                    &directory
                        .join("receipts")
                        .join(format!("review-provenance-{key}.json")),
                    &json!({"reviewer_thread_id":reviewer,"reviewer_turn_id":candidate.reviewer_turn_id,
                        "report_digest":candidate.report_digest,"provenance_digest":provenance_digest}),
                )?;
                let review = EngineeringReviewEvidence {
                    reviewer_identity: candidate.reviewer_turn_id.map_or_else(
                        || format!("codex-thread:{reviewer}"),
                        |turn| format!("codex-thread:{reviewer}:turn:{turn}"),
                    ),
                    artefacts: report.artefacts,
                    evidence_digest: self.blob(message.as_bytes())?,
                };
                atomic(
                    &self
                        .profile
                        .broker_root
                        .join("reviews")
                        .join(format!("{}.json", review.evidence_digest)),
                    &review,
                )?;
                Ok(json!(review))
            }
            "bokkie_commands" => Ok(
                json!({"commands":log.iter().filter(|e|e.kind=="item/completed" && e.value["item"]["type"]=="commandExecution").map(|e|json!({"item_id":e.value["item"]["id"],"command":e.value["item"]["command"],"exit_code":e.value["item"]["exitCode"]})).collect::<Vec<_>>(),"reviewer_candidates":observed_reviews(log)}),
            ),
            "bokkie_validation" => {
                let item_id = args["item_id"].as_str().ok_or("missing command item ID")?;
                let item = log
                    .iter()
                    .find(|e| {
                        e.kind == "item/completed"
                            && e.value["item"]["id"] == item_id
                            && e.value["item"]["type"] == "commandExecution"
                    })
                    .ok_or("completed command item unavailable")?;
                let artefact: EngineeringArtefact =
                    serde_json::from_value(args["artefact"].clone())?;
                self.inspect(&artefact)?;
                let sources = ["item/started", "item/completed"].map(|phase| {
                    log.iter()
                        .find(|event| {
                            event.kind == "command_source"
                                && event.value["phase"] == phase
                                && event.value["item_id"] == item_id
                                && event.value["thread_id"] == item.value["threadId"]
                                && event.value["turn_id"] == item.value["turnId"]
                        })
                        .map(|event| &event.value["source"])
                });
                let [Some(before), Some(after)] = sources else {
                    return Err("validation requires broker source identities at command start and completion; rerun the check".into());
                };
                if before != after || before.get("unavailable").is_some() {
                    return Err(
                        "source changed during validation or its identity is unavailable".into(),
                    );
                }
                let matches = match &artefact {
                    EngineeringArtefact::File {
                        path,
                        sha256,
                        bytes,
                        ..
                    } => {
                        before["files"][path]["sha256"] == *sha256
                            && before["files"][path]["byte_length"] == *bytes
                    }
                    EngineeringArtefact::Git { commit, tree, .. } => {
                        before["clean"] == true
                            && before["commit"] == *commit
                            && before["tree"] == *tree
                    }
                };
                if !matches {
                    return Err(
                        "validation command did not observe the submitted source revision".into(),
                    );
                }
                let command = item.value["item"]["command"]
                    .as_str()
                    .ok_or("missing actual command")?;
                let output = item.value["item"]["aggregatedOutput"]
                    .as_str()
                    .ok_or("missing actual output")?;
                let exit_code = i32::try_from(
                    item.value["item"]["exitCode"]
                        .as_i64()
                        .ok_or("command has no exit result")?,
                )?;
                let evidence = EngineeringCriterionEvidence {
                    criterion_id: args["criterion_id"]
                        .as_str()
                        .ok_or("missing criterion")?
                        .into(),
                    artefact,
                    command_digest: self.blob(command.as_bytes())?,
                    output_digest: self.blob(output.as_bytes())?,
                    exit_code,
                };
                // Retain provenance separately from model-supplied hashes.
                atomic(
                    &directory
                        .join("receipts")
                        .join(format!("validation-{key}.json")),
                    &evidence,
                )?;
                Ok(json!(evidence))
            }
            _ => Err("unsupported dynamic tool".into()),
        }
    }
    fn request(
        &self,
        store: &mut Store,
        directory: &Path,
        execution: &EngineeringExecution,
        event: &Event,
        log: &[Event],
        now: i64,
    ) -> RuntimeResult<()> {
        let key = event.value["key"]
            .as_str()
            .ok_or("request missing stable key")?;
        if key.len() != 64 || !key.bytes().all(|c| c.is_ascii_hexdigit()) {
            return Err("invalid broker request key".into());
        }
        if let Err(error) = self.request_inner(store, directory, execution, event, log, now) {
            let reply = directory.join("replies").join(format!("{key}.json"));
            // Preserve a reply committed before a receipt-write failure.
            if !reply.exists() {
                let message = &event.value["message"];
                if message["method"] == "item/tool/call" {
                    self.tool_reply(&reply, &message["id"], Err(error))?;
                } else {
                    let detail = error.to_string();
                    let diagnostic = if detail.len() <= 4096 {
                        detail
                    } else {
                        "Adapter request failed; diagnostic exceeds the bounded error limit".into()
                    };
                    atomic(
                        &reply,
                        &json!({"id":message["id"],"error":{"code":-32602,"message":diagnostic}}),
                    )?;
                }
            }
            atomic(
                &directory
                    .join("receipts")
                    .join(format!("request-{key}.json")),
                &json!({"handled":true}),
            )?;
        }
        Ok(())
    }
    fn request_inner(
        &self,
        store: &mut Store,
        directory: &Path,
        execution: &EngineeringExecution,
        event: &Event,
        log: &[Event],
        now: i64,
    ) -> RuntimeResult<()> {
        let key = event.value["key"]
            .as_str()
            .ok_or("request missing stable key")?;
        if key.len() != 64 || !key.bytes().all(|c| c.is_ascii_hexdigit()) {
            return Err("invalid broker request key".into());
        }
        let message = &event.value["message"];
        let method = message["method"].as_str().ok_or("request missing method")?;
        let reply = directory.join("replies").join(format!("{key}.json"));
        let done = directory
            .join("receipts")
            .join(format!("request-{key}.json"));
        if done.exists() {
            return Ok(());
        }
        let question_tool =
            method == "item/tool/call" && message["params"]["tool"] == "bokkie_question";
        if method == "item/tool/call" && !question_tool {
            if reply.exists() {
                atomic(&done, &json!({"handled":true}))?;
                return Ok(());
            }
            // Reject requests from nested agents: only this root claim can use
            // lifecycle tools. Subagents retain normal bounded read/work tools.
            let thread = log
                .iter()
                .find(|e| e.kind == "thread_identity")
                .map(|e| &e.value["thread_id"]);
            if thread != Some(&message["params"]["threadId"]) {
                return Err("nested agent cannot inherit root Bokkie authority".into());
            }
            let result = self.dynamic(
                store,
                directory,
                execution,
                key,
                &message["params"],
                log,
                now,
            );
            self.tool_reply(&reply, &message["id"], result)?;
            atomic(&done, &json!({"handled":true}))?;
            return Ok(());
        }
        if log.iter().any(|e| {
            e.kind == "approval_decision"
                && e.value["key"] == key
                && e.value["decision"] == "accept"
        }) {
            atomic(&done, &json!({"handled":true,"policy":"literal-pwd-v1"}))?;
            return Ok(());
        }
        if question_tool {
            let selected = if execution.role == EngineeringRole::Supervisor {
                &self.profile.supervisor_tools
            } else {
                &self.profile.worker_tools
            };
            if !selected.iter().any(|s| s == "bokkie_question") {
                return Err("question capability excluded by profile".into());
            }
        }
        let question_kind = if question_tool {
            match message["params"]["arguments"]["kind"].as_str() {
                Some("routine") => EngineeringQuestionKind::Routine,
                Some("missing_information") => EngineeringQuestionKind::MissingInformation,
                Some("new_authority") => EngineeringQuestionKind::NewAuthority,
                _ => return Err("unsupported question kind".into()),
            }
        } else if method == "item/tool/requestUserInput" {
            EngineeringQuestionKind::Routine
        } else {
            EngineeringQuestionKind::NewAuthority
        };
        let routine = question_kind == EngineeringQuestionKind::Routine;
        let questions: Vec<Value> = if question_tool {
            let args = &message["params"]["arguments"];
            vec![
                json!({"id":"question","question":args["prompt"],"options":args["options"].as_array().ok_or("missing question options")?.iter().map(|o|json!({"label":o})).collect::<Vec<_>>()}),
            ]
        } else if routine {
            message["params"]["questions"]
                .as_array()
                .ok_or("missing questions")?
                .clone()
        } else {
            vec![
                json!({"id":"authority","question":format!("Codex requested {method}. The adapter declined unsupported escalation. Decide the minimum additional authority or revise the task to continue within the current workspace. Exact request: {}",message["params"]),"options":[]}),
            ]
        };
        if questions.len() > 3 {
            return Err("protocol question group exceeds three".into());
        }
        let mut answers = serde_json::Map::new();
        let mut unanswered = false;
        for (index, question) in questions.iter().enumerate() {
            let request_key = format!("{key}-{index}");
            let options = question["options"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|o| o["label"].as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            let receipt = self.activity(
                store,
                directory,
                execution,
                &request_key,
                EngineeringCommand::AskQuestion {
                    request_key: request_key.clone(),
                    kind: question_kind,
                    prompt: question["question"]
                        .as_str()
                        .ok_or("missing question text")?
                        .into(),
                    options,
                },
                false,
                now,
            )?;
            if routine {
                let state = snapshot(store, &receipt.outcome_id)?;
                let resolved = state
                    .questions
                    .iter()
                    .find(|q| Some(&q.id) == receipt.record_id.as_ref())
                    .and_then(|q| q.resolution.as_ref());
                let Some(resolution) = resolved else {
                    unanswered = true;
                    continue;
                };
                if resolution.contract_revision != execution.contract_revision
                    || state.contract_revision != execution.contract_revision
                {
                    return Err("late answer belongs to superseded contract".into());
                }
                answers.insert(
                    question["id"].as_str().ok_or("question lacks ID")?.into(),
                    json!({"answers":[resolution.answer]}),
                );
            }
        }
        if unanswered {
            return Ok(());
        }
        if routine {
            let result = if question_tool {
                json!({"success":true,"contentItems":[{"type":"inputText","text":json!({"answers":answers}).to_string()}]})
            } else {
                json!({"answers":answers})
            };
            atomic(&reply, &json!({"id":message["id"],"result":result}))?;
        } else {
            atomic(
                &directory.join("cancel.json"),
                &json!({"reason":"operator question saved; stop and reap"}),
            )?;
            if question_tool {
                atomic(
                    &reply,
                    &json!({"id":message["id"],"result":{"success":true,"contentItems":[{"type":"inputText","text":"Question saved for operator; execution stopping"}]}}),
                )?;
            }
        }
        atomic(&done, &json!({"handled":true}))?;
        Ok(())
    }
    fn reconcile(
        &self,
        store: &mut Store,
        initial: &EngineeringOutcomeSnapshot,
        execution: &EngineeringExecution,
        now: i64,
    ) -> RuntimeResult<()> {
        let directory = self.profile.broker_root.join(&execution.id);
        if execution.cessation_verified {
            return Ok(());
        }
        if !directory.join("dispatch.json").exists() {
            if !execution.fenced
                && !initial.cancellation_requested
                && initial.contract_revision == execution.contract_revision
                && execution.claim.lease_expires_at > now
            {
                self.dispatch(initial, execution, now)?;
            } else {
                // The controller lock excludes another dispatcher. No manifest
                // means no broker launch was possible for this immutable intent.
                let evidence_digest = self.blob(&serde_json::to_vec(&json!({
                    "execution_id":execution.id,"dispatch_manifest_absent":true,
                    "controller_lock_held":true
                }))?)?;
                store.engineering_command(EngineeringActor::Reconciler {
                    adapter_id: execution.instructions.adapter_id.clone(),
                }, EngineeringCommandEnvelope {
                    command_id:format!("{}:not-started",execution.id),
                    expected:Some(initial.precondition()),
                    command:EngineeringCommand::RecordReconciliation(EngineeringReconciliationInput {
                        execution_id:execution.id.clone(),runtime_identity:execution.id.clone(),
                        observation:"Controller recovered an expired or fenced intent without a dispatch manifest; no broker was started".into(),
                        evidence_digest,reaped_boundary:None,recovered_submission:None,
                        runtime_failure:None,not_started:true,
                    }),
                },now)?;
            }
            return Ok(());
        }
        let (log, journal_bytes) = read_events(&directory)?;
        let reaped = log.iter().find(|e| e.kind == "boundary_reaped");
        if execution.fenced
            || initial.cancellation_requested
            || initial.contract_revision != execution.contract_revision
        {
            atomic(
                &directory.join("cancel.json"),
                &json!({"reason":"Store fenced execution; stop and reap"}),
            )?;
        } else if reaped.is_none() {
            // A renewed kernel lease never extends the broker's fixed deadline.
            let manifest: Value = read_json(&directory.join("dispatch.json"))?;
            let deadline = manifest["deadline"]
                .as_i64()
                .unwrap_or(execution.claim.lease_expires_at);
            let remaining = deadline.saturating_sub(now).min(120);
            if remaining <= 0 || store.renew_lease(&execution.claim, now, remaining).is_err() {
                atomic(
                    &directory.join("cancel.json"),
                    &json!({"reason":"lease renewal refused; stop and reconcile"}),
                )?;
            }
        }
        if !execution.fenced
            && reaped.is_none()
            && log.iter().any(|e| e.kind == "guidance_identities")
        {
            let settings = log
                .iter()
                .filter(|e| {
                    [
                        "launch_committed",
                        "boundary_started",
                        "effective_settings",
                        "effective_capabilities",
                        "guidance_identities",
                        "thread_identity",
                        "turn_identity",
                    ]
                    .contains(&e.kind.as_str())
                })
                .collect::<Vec<_>>();
            let evidence_digest = self.blob(&serde_json::to_vec(&settings)?)?;
            self.activity(store,&directory,execution,"settings",EngineeringCommand::RecordCheckpoint {
                runtime_identity:execution.id.clone(),request_identity:None,cursor:"settings-v1".into(),
                summary:"Effective task-scoped Codex profile, tools, guidance identities and process/thread/turn identities retained".into(),evidence_digest
            },false,now)?;
        }
        for event in log.iter().filter(|e| e.kind == "request") {
            // Completed replies are immutable and replayable. Expired requests
            // stay in the spool and must not acquire fresh worker authority.
            if !execution.fenced && !initial.cancellation_requested && reaped.is_none() {
                if let Err(error) = self.request(store, &directory, execution, event, &log, now) {
                    atomic(
                        &directory.join("request-error.json"),
                        &json!({"sequence":event.sequence,"error":error.to_string()}),
                    )?;
                    if reaped.is_none() {
                        return Err(error);
                    }
                }
            }
        }
        if let Some(boundary) = reaped {
            let mut submission = if directory.join("submission.json").exists() {
                Some(read_json::<EngineeringSubmissionInput>(
                    &directory.join("submission.json"),
                )?)
            } else {
                None
            };
            if submission.is_none() && execution.role == EngineeringRole::Worker {
                // Only the completed root turn can submit through its final
                // message. A nested agent or commentary item cannot impersonate it.
                let thread = log
                    .iter()
                    .find(|e| e.kind == "thread_identity")
                    .map(|e| &e.value["thread_id"]);
                let turn = log
                    .iter()
                    .find(|e| e.kind == "turn_identity")
                    .map(|e| &e.value["turn_id"]);
                let completed = thread.is_some()
                    && turn.is_some()
                    && log.iter().any(|e| {
                        e.kind == "turn/completed"
                            && Some(&e.value["threadId"]) == thread
                            && Some(&e.value["turn"]["id"]) == turn
                            && e.value["turn"]["status"] == "completed"
                    });
                if completed {
                    if let Some(event) = log.iter().rev().find(|e| {
                        e.kind == "item/completed"
                            && e.value["item"]["type"] == "agentMessage"
                            && Some(&e.value["threadId"]) == thread
                            && Some(&e.value["turnId"]) == turn
                    }) {
                        if let Some(text) = event.value["item"]["text"].as_str() {
                            submission =
                                serde_json::from_str::<EngineeringSubmissionInput>(text).ok();
                        }
                    }
                }
            }
            let current = snapshot(store, &initial.id)?;
            let package_ineligible = execution.package_id.as_ref().is_some_and(|id| {
                current
                    .packages
                    .iter()
                    .any(|p| p.id == *id && (p.cancellation_requested || p.superseded_by.is_some()))
            });
            let newer_execution = current.executions.iter().any(|other| {
                other.obligation_id == execution.obligation_id
                    && other.claim.lease_generation > execution.claim.lease_generation
            });
            if current.cancellation_requested
                || current.contract_revision != execution.contract_revision
                || package_ineligible
                || newer_execution
            {
                submission = None;
            }
            let mut validation_error = None;
            if let Some(input) = &submission {
                let validation: RuntimeResult<()> = (|| {
                    store.engineering_submission_preflight(&current.id, &execution.id, input)?;
                    self.verify_submission(input)
                })();
                if let Err(error) = validation {
                    validation_error = Some(error.to_string());
                    submission = None;
                }
            }
            let intentional_stop = directory.join("cancel.json").exists()
                || current.cancellation_requested
                || package_ineligible
                || newer_execution
                || current.contract_revision != execution.contract_revision;
            let failed_turn = log.iter().any(|event| {
                event.kind == "turn/completed" && event.value["turn"]["status"] == "failed"
            });
            let runtime_failure = if !intentional_stop
                && ((!log.iter().any(|event| event.kind == "turn_identity")
                    && log.iter().any(|event| event.kind == "failure"))
                    || failed_turn)
            {
                let diagnostic = log
                    .iter()
                    .find(|event| event.kind == "stderr_diagnostic")
                    .map(|event| event.value.to_string())
                    .unwrap_or_else(|| "no stderr diagnostic".into());
                let failure = log
                    .iter()
                    .find(|event| event.kind == "failure")
                    .map(|event| event.value.to_string())
                    .unwrap_or_else(|| "root turn failed".into());
                Some(format!(
                    "Codex execution failed without a usable result; repair the runtime/profile before retry. {failure}; {diagnostic}"
                ))
            } else {
                None
            };
            // Retain exactly the bounded journal we parsed. Re-encoding a full
            // journal can exceed its byte bound and obscure the retained proof.
            let hash = self.blob(&journal_bytes)?;
            let journal_exhausted = log.iter().any(|event| {
                event.kind == "failure"
                    && event.value["message"] == "event spool exhausted; stop and reconcile"
            });
            self.activity(store,&directory,execution,"reaped",EngineeringCommand::RecordReconciliation(EngineeringReconciliationInput {
                execution_id:execution.id.clone(),runtime_identity:execution.id.clone(),
                observation:if let Some(error)=validation_error { format!("Broker reaped namespace; submission rejected: {error}") } else if submission.is_some(){"Broker reaped the namespace; import exact offline submission for assessment".into()}else if journal_exhausted {"Broker journal exhausted; namespace reaped; supervisor retains the next decision within the remaining finite budget".into()}else{"Broker reaped the namespace; supervisor must decide the next bounded action".into()},
                runtime_failure, not_started:false, evidence_digest:hash,reaped_boundary:Some(boundary.value["boundary"].as_str().ok_or("missing reaping identity")?.into()),recovered_submission:submission }),true,now)?;
        } else {
            let output = Command::new("python3")
                .arg(&self.profile.broker)
                .arg("status")
                .arg(&directory)
                .output()?;
            if !output.status.success() {
                return Err("broker status unavailable; ownership uncertain".into());
            }
            let status: Value = serde_json::from_slice(&output.stdout)?;
            if status["active"] == false && status["launched"] == true {
                let hash =
                    self.blob(&serde_json::to_vec(&json!({"status":status,"events":log}))?)?;
                self.activity(store,&directory,execution,"uncertain",EngineeringCommand::RecordReconciliation(EngineeringReconciliationInput {
                    execution_id:execution.id.clone(),runtime_identity:execution.id.clone(),
                    observation:if log.iter().any(|e| e.kind == "not_started") {
                        "Broker proved it did not start a boundary; release this dispatch reservation using explicit not-started evidence. No running child or namespace reap is claimed.".into()
                    } else {
                        "Broker died without a retained namespace reap receipt. Ownership uncertain; do not replace this writer. Operator must establish boundary cessation.".into()
                    },
                    runtime_failure:None,not_started:log.iter().any(|e| e.kind == "not_started"),evidence_digest:hash,reaped_boundary:None,recovered_submission:None }),true,now)?;
            }
        }
        Ok(())
    }
}
fn execution_outcome(directory: &Path) -> RuntimeResult<String> {
    let manifest: Value = read_json(&directory.join("dispatch.json"))?;
    let prompt: Value =
        serde_json::from_str(manifest["prompt"].as_str().ok_or("missing saved context")?)?;
    Ok(prompt["snapshot"]["id"]
        .as_str()
        .ok_or("missing outcome identity")?
        .into())
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture {
        _temp: tempfile::TempDir,
        runtime: EngineeringRuntime,
        store: Store,
        id: String,
        supervisor: EngineeringExecution,
        worker: EngineeringExecution,
    }
    impl Fixture {
        fn new() -> Self {
            let temp = tempfile::tempdir().unwrap();
            let workspace = temp.path().join("workspace");
            let root = temp.path().join("broker");
            fs::create_dir(&workspace).unwrap();
            fs::create_dir(&root).unwrap();
            let instruction = temp.path().join("instructions.md");
            fs::write(&instruction, "Preserve the original intent.").unwrap();
            let profile = EngineeringRuntimeProfile {
                workspace,
                broker_root: root,
                broker: PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("tools/engineering-runtime/broker.py"),
                codex: PathBuf::from("/bin/true"),
                bwrap: PathBuf::from("/bin/true"),
                supervisor_instructions: instruction.clone(),
                worker_instructions: instruction,
                model: "gpt-6-astra".into(),
                effort: "medium".into(),
                subagent_model: "gpt-5.6-terra".into(),
                subagent_effort: "medium".into(),
                max_subagents: 2,
                concurrency: 2,
                supervisor_seconds: 600,
                worker_seconds: 2700,
                max_packages: 8,
                max_repairs: 3,
                max_recoveries: 3,
                max_turns: 24,
                deadline_seconds: 14400,
                worker_network_access: false,
                worker_scratch: None,
                supervisor_tools: dynamic_tools()
                    .iter()
                    .map(|t| t["name"].as_str().unwrap().into())
                    .collect(),
                worker_tools: dynamic_tools()
                    .iter()
                    .map(|t| t["name"].as_str().unwrap().into())
                    .collect(),
                readonly_mcp_servers: vec![],
                allow_single_pwd_approval: false,
            };
            let runtime = EngineeringRuntime::new(profile).unwrap();
            let mut store = Store::open_in_memory().unwrap();
            let saved = intake(
                &mut store,
                &runtime.profile,
                "Build the requested local reader, preserving source pages.".into(),
                "intake".into(),
                100,
            )
            .unwrap();
            let id = saved.outcome_id;
            let initial = snapshot(&store, &id).unwrap();
            assert!(initial.contract().criteria.is_empty());
            let mut contract = initial.contract().clone();
            contract.criteria.push(EngineeringCriterion {
                id: "reader".into(),
                description: "Reader opens a page".into(),
            });
            store
                .engineering_command(
                    EngineeringActor::Operator {
                        name: "fixture".into(),
                    },
                    EngineeringCommandEnvelope {
                        command_id: "criteria".into(),
                        expected: Some(initial.precondition()),
                        command: EngineeringCommand::ReviseContract { contract },
                    },
                    101,
                )
                .unwrap();
            store
                .claim_due_engineering(EngineeringRole::Supervisor, 102, 600, 1)
                .unwrap();
            let state = snapshot(&store, &id).unwrap();
            let supervisor = state.executions[0].clone();
            store
                .engineering_command(
                    actor(&supervisor),
                    EngineeringCommandEnvelope {
                        command_id: "package".into(),
                        expected: Some(state.precondition()),
                        command: EngineeringCommand::CreatePackage(NewEngineeringPackage {
                            parent_id: None,
                            dependencies: vec![],
                            instructions: "Implement reader".into(),
                            criteria: vec!["reader".into()],
                            inputs: vec![],
                            workspace: runtime.profile.workspace.to_string_lossy().into(),
                            budget: runtime.profile.budget(100, true),
                        }),
                    },
                    103,
                )
                .unwrap();
            store
                .claim_due_engineering(EngineeringRole::Worker, 104, 2700, 1)
                .unwrap();
            let state = snapshot(&store, &id).unwrap();
            let worker = state
                .executions
                .iter()
                .find(|e| e.role == EngineeringRole::Worker)
                .unwrap()
                .clone();
            let fixture = Self {
                _temp: temp,
                runtime,
                store,
                id,
                supervisor,
                worker,
            };
            fixture.prepare(&fixture.supervisor);
            fixture.prepare(&fixture.worker);
            fixture
        }
        fn prepare(&self, execution: &EngineeringExecution) {
            let directory = self.directory(execution);
            fs::create_dir_all(directory.join("receipts")).unwrap();
            fs::create_dir_all(directory.join("replies")).unwrap();
            let state = snapshot(&self.store, &self.id).unwrap();
            atomic(
                &directory.join("dispatch.json"),
                &json!({"prompt":json!({"snapshot":state}).to_string()}),
            )
            .unwrap();
            atomic(&directory.join("observed.json"), &state.precondition()).unwrap();
        }
        fn directory(&self, execution: &EngineeringExecution) -> PathBuf {
            self.runtime.profile.broker_root.join(&execution.id)
        }
        fn event(&self, execution: &EngineeringExecution, sequence: u64, kind: &str, value: Value) {
            let path = self.directory(execution).join("events.jsonl");
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .unwrap();
            writeln!(
                file,
                "{}",
                serde_json::to_string(&Event {
                    sequence,
                    kind: kind.into(),
                    value
                })
                .unwrap()
            )
            .unwrap();
            file.sync_all().unwrap();
        }
    }
    #[test]
    fn large_evidence_pages_preserve_exact_bytes_and_bound_encoded_replies() {
        let f = Fixture::new();
        let bytes = "🦘\\\"\n\0".repeat(270_000).into_bytes();
        assert!(bytes.len() as u64 > MAX_FILE);
        let digest = f.runtime.blob(&bytes).unwrap();
        let path = f.directory(&f.supervisor).join("replies/page.json");
        let mut offset = 0;
        let mut restored = Vec::new();
        loop {
            let page = f
                .runtime
                .evidence_page(&json!({"digest":digest,"byte_offset":offset,"max_bytes":32767}))
                .unwrap();
            assert_eq!(page["digest"], digest);
            assert_eq!(page["encoding"], "utf8");
            assert_eq!(page["total_bytes"], bytes.len());
            assert_eq!(page["byte_offset"], offset);
            assert_eq!(page["partial"], true);
            let content = page["content"].as_str().unwrap();
            assert_eq!(page["byte_length"], content.len());
            restored.extend_from_slice(content.as_bytes());
            f.runtime
                .tool_reply(&path, &json!(1), Ok(page.clone()))
                .unwrap();
            assert!(fs::metadata(&path).unwrap().len() < MAX_TOOL_REPLY);
            let reply: Value = read_json(&path).unwrap();
            let decoded: Value =
                serde_json::from_str(reply["result"]["contentItems"][0]["text"].as_str().unwrap())
                    .unwrap();
            assert_eq!(decoded, page);
            match page["next_byte_offset"].as_u64() {
                Some(next) => {
                    assert!(next > offset);
                    offset = next;
                }
                None => break,
            }
        }
        assert_eq!(restored, bytes);
        assert_eq!(sha(&restored), digest);
        // Direct tail access need not read all preceding diagnostic pages.
        let tail = f
            .runtime
            .evidence_page(&json!({"digest":digest,"byte_offset":bytes.len()-8}))
            .unwrap();
        assert_eq!(tail["content"], "🦘\\\"\n\0");
        assert!(tail["next_byte_offset"].is_null());
        assert!(
            f.runtime
                .evidence_page(&json!({"digest":digest,"byte_offset":1}))
                .unwrap_err()
                .to_string()
                .contains("retry with byte_offset 0")
        );
        assert!(
            f.runtime
                .evidence_page(&json!({"digest":digest,"byte_offset":u64::MAX}))
                .is_err()
        );
        assert!(
            f.runtime
                .evidence_page(&json!({"digest":digest,"max_bytes":MAX_FILE}))
                .is_err()
        );
        fs::write(
            f.runtime.profile.broker_root.join("blobs").join(&digest),
            b"changed",
        )
        .unwrap();
        assert!(
            f.runtime
                .evidence_page(&json!({"digest":digest}))
                .unwrap_err()
                .to_string()
                .contains("digest mismatch")
        );
    }

    #[test]
    fn binary_evidence_pages_are_lossless_and_explicitly_encoded() {
        let f = Fixture::new();
        let bytes = [0xff, 0x00, 0xfe, 0x80, 0x01, 0x02];
        let digest = f.runtime.blob(&bytes).unwrap();
        let mut restored = vec![];
        for offset in [0, 4] {
            let page = f
                .runtime
                .evidence_page(&json!({"digest":digest,"byte_offset":offset,"max_bytes":4}))
                .unwrap();
            assert_eq!(page["encoding"], "base64");
            restored.extend(STANDARD.decode(page["content"].as_str().unwrap()).unwrap());
        }
        assert_eq!(restored, bytes);
    }

    fn read_reply(directory: &Path, key: &str) -> (Value, Value) {
        let path = directory.join("replies").join(format!("{key}.json"));
        assert!(fs::metadata(&path).unwrap().len() < MAX_FILE);
        let reply: Value = read_json(&path).unwrap();
        let body =
            serde_json::from_str(reply["result"]["contentItems"][0]["text"].as_str().unwrap())
                .unwrap();
        (reply, body)
    }

    #[test]
    fn expanded_reply_and_bad_request_do_not_block_later_requests_or_reaping() {
        let mut f = Fixture::new();
        // Raw source fits its 2 MiB bound, but nested JSON escaping does not.
        let text = "\"\\\n".repeat(270_000);
        assert!((text.len() as u64) < MAX_FILE);
        fs::write(f.runtime.profile.workspace.join("large.txt"), &text).unwrap();
        fs::write(
            f.runtime.profile.workspace.join("small.txt"),
            "next request works",
        )
        .unwrap();
        f.event(
            &f.worker,
            1,
            "thread_identity",
            json!({"thread_id":"root-thread"}),
        );
        for (sequence, tool, args) in [
            (2, "bokkie_file", json!({"path":"large.txt"})),
            (3, "bokkie_question", json!({"kind":"invalid"})),
            (4, "bokkie_file", json!({"path":"small.txt"})),
        ] {
            f.event(&f.worker, sequence, "request", json!({"key":format!("{sequence:064x}"),"message":{
                "id":sequence,"method":"item/tool/call","params":{"threadId":"root-thread","tool":tool,"arguments":args}}}));
        }
        let state = snapshot(&f.store, &f.id).unwrap();
        f.runtime
            .reconcile(&mut f.store, &state, &f.worker, 105)
            .unwrap();
        let directory = f.directory(&f.worker);
        let (reply, paged) = read_reply(&directory, &format!("{:064x}", 2));
        assert_eq!(reply["result"]["success"], true);
        assert_eq!(paged["response_paged"], true);
        let full = f
            .runtime
            .evidence_with_bound(paged["digest"].as_str().unwrap(), MAX_SPOOL)
            .unwrap();
        let full: Value = serde_json::from_slice(&full).unwrap();
        assert_eq!(full["content"], text);
        let (reply, error) = read_reply(&directory, &format!("{:064x}", 3));
        assert_eq!(reply["result"]["success"], false);
        assert!(
            error["error"]
                .as_str()
                .unwrap()
                .contains("unsupported question kind")
        );
        let (reply, next) = read_reply(&directory, &format!("{:064x}", 4));
        assert_eq!(reply["result"]["success"], true);
        assert_eq!(next["content"], "next request works");
        for sequence in [2, 3, 4] {
            assert!(
                directory
                    .join("receipts")
                    .join(format!("request-{sequence:064x}.json"))
                    .exists()
            );
        }
        fs::write(
            f.runtime.profile.workspace.join("large.txt"),
            "changed after response",
        )
        .unwrap();
        let state = snapshot(&f.store, &f.id).unwrap();
        f.runtime
            .reconcile(&mut f.store, &state, &f.worker, 106)
            .unwrap();
        assert_eq!(read_reply(&directory, &format!("{:064x}", 2)).1, paged);
        // Even an unserviceable pending request cannot obstruct proven cessation.
        f.event(
            &f.worker,
            5,
            "request",
            json!({"key":"invalid","message":{"id":5}}),
        );
        f.event(
            &f.worker,
            6,
            "boundary_reaped",
            json!({"boundary":"fixture:namespace"}),
        );
        let state = snapshot(&f.store, &f.id).unwrap();
        f.runtime
            .reconcile(&mut f.store, &state, &f.worker, 107)
            .unwrap();
        assert!(!directory.join("request-error.json").exists());
        let state = snapshot(&f.store, &f.id).unwrap();
        assert!(
            state
                .executions
                .iter()
                .find(|e| e.id == f.worker.id)
                .unwrap()
                .cessation_verified
        );
        assert_eq!(
            f.store
                .claim_due_engineering(EngineeringRole::Worker, 108, 600, 1)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn binary_asset_inspection_verifies_all_bytes_without_dumping_content() {
        let mut f = Fixture::new();
        let bytes = [0, 0xff, 0x80, b'A'].repeat(262_144);
        let path = "font.zlib2";
        fs::write(f.runtime.profile.workspace.join(path), &bytes).unwrap();
        let (artefact, _) = f.runtime.file(path).unwrap();
        let inspected = f.runtime.inspect(&artefact).unwrap();
        assert_eq!(inspected["artefact"], json!(artefact));
        assert_eq!(inspected["content_kind"], "binary");
        assert_eq!(inspected["identity_verified"], true);
        assert_eq!(inspected["binary_reason"], "not_utf8");
        assert!(inspected["content"].is_null());
        assert!(serde_json::to_vec(&inspected).unwrap().len() < 2048);
        let directory = f.directory(&f.supervisor);
        let discovered = f
            .runtime
            .dynamic(
                &mut f.store,
                &directory,
                &f.supervisor,
                "binary-file",
                &json!({"tool":"bokkie_file","arguments":{"path":path}}),
                &[],
                105,
            )
            .unwrap();
        assert_eq!(discovered, inspected);
        let reply_path = directory.join("replies/binary.json");
        f.runtime
            .tool_reply(&reply_path, &json!(1), Ok(inspected.clone()))
            .unwrap();
        assert!(fs::metadata(&reply_path).unwrap().len() < 4096);
        let (_, reply) = read_reply(&directory, "binary");
        assert!(reply.get("response_paged").is_none());
        assert_eq!(reply, inspected);
        let digest = inspected["raw_evidence_digest"].as_str().unwrap();
        assert_eq!(digest, sha(&bytes));
        let page = f
            .runtime
            .evidence_page(&json!({"digest":digest,"byte_offset":1234,"max_bytes":16}))
            .unwrap();
        assert_eq!(page["encoding"], "base64");
        assert_eq!(
            STANDARD.decode(page["content"].as_str().unwrap()).unwrap(),
            bytes[1234..1250]
        );
        let mut changed = bytes.clone();
        changed[500_000] ^= 1;
        fs::write(f.runtime.profile.workspace.join(path), &changed).unwrap();
        assert!(
            f.runtime
                .inspect(&artefact)
                .unwrap_err()
                .to_string()
                .contains("no longer matches")
        );
        fs::write(f.runtime.profile.workspace.join(path), b"\0font").unwrap();
        let (nul_artefact, _) = f.runtime.file(path).unwrap();
        assert_eq!(
            f.runtime.inspect(&nul_artefact).unwrap()["binary_reason"],
            "contains_nul"
        );
    }

    #[test]
    fn textual_asset_content_remains_exact_even_with_a_binary_filename() {
        let f = Fixture::new();
        let text = "Licence and provenance 🦘\n\tReview every line.\r\n";
        fs::write(f.runtime.profile.workspace.join("font.woff2"), text).unwrap();
        let (artefact, _) = f.runtime.file("font.woff2").unwrap();
        let inspected = f.runtime.inspect(&artefact).unwrap();
        assert_eq!(inspected["content_kind"], "text");
        assert_eq!(inspected["encoding"], "utf8");
        assert_eq!(inspected["content"], text);
    }

    #[test]
    fn exhausted_large_journal_retains_exact_cessation_and_bounded_continuation() {
        let mut f = Fixture::new();
        let directory = f.directory(&f.supervisor);
        let mut journal = vec![];
        for sequence in 1..=2046 {
            let (kind, value) = match sequence {
                1 => ("thread_identity", json!({"thread_id":"root-thread"})),
                2 => (
                    "turn_identity",
                    json!({"thread_id":"root-thread","turn_id":"root-turn"}),
                ),
                2045 => (
                    "failure",
                    json!({"type":"ValueError","message":"event spool exhausted; stop and reconcile"}),
                ),
                2046 => (
                    "boundary_reaped",
                    json!({"boundary":"fixture:exhausted","exit_code":-9}),
                ),
                _ => ("item/completed", json!({"diagnostic":"x".repeat(7500)})),
            };
            journal.extend(
                serde_json::to_vec(&Event {
                    sequence,
                    kind: kind.into(),
                    value,
                })
                .unwrap(),
            );
            journal.push(b'\n');
        }
        assert!(journal.len() as u64 > 14 * 1024 * 1024);
        assert!((journal.len() as u64) < MAX_SPOOL);
        fs::write(directory.join("events.jsonl"), &journal).unwrap();
        let before = snapshot(&f.store, &f.id).unwrap();
        f.runtime
            .reconcile(&mut f.store, &before, &f.supervisor, 105)
            .unwrap();
        let state = snapshot(&f.store, &f.id).unwrap();
        assert!(
            state
                .executions
                .iter()
                .find(|e| e.id == f.supervisor.id)
                .unwrap()
                .cessation_verified
        );
        assert!(state.acceptance.is_none());
        assert_eq!(state.root.state, crate::ObligationState::Pending);
        assert_eq!(state.turns_used, before.turns_used);
        let input = &state.reconciliations.last().unwrap().input;
        assert!(input.observation.contains("journal exhausted"));
        assert!(input.runtime_failure.is_none());
        assert_eq!(input.reaped_boundary.as_deref(), Some("fixture:exhausted"));
        assert_eq!(input.evidence_digest, sha(&journal));
        let tail = f
            .runtime
            .evidence_page(&json!({"digest":input.evidence_digest,"byte_offset":journal.len()-512}))
            .unwrap();
        assert_eq!(tail["total_bytes"], journal.len());
        assert!(
            tail["content"]
                .as_str()
                .unwrap()
                .contains("boundary_reaped")
        );
        f.runtime
            .reconcile(&mut f.store, &state, &f.supervisor, 106)
            .unwrap();
        assert_eq!(snapshot(&f.store, &f.id).unwrap().reconciliations.len(), 1);
        let claims = f
            .store
            .claim_due_engineering(EngineeringRole::Supervisor, 107, 600, 1)
            .unwrap();
        assert_eq!(claims.len(), 1);
        assert_eq!(
            snapshot(&f.store, &f.id).unwrap().turns_used,
            before.turns_used + 1
        );
    }

    #[test]
    fn dispatch_limits_honour_package_deadline_and_original_claim() {
        let f = Fixture::new();
        let mut state = snapshot(&f.store, &f.id).unwrap();
        state.packages[0].input.budget.turn_seconds = 30;
        state.packages[0].input.budget.deadline = 120;
        assert_eq!(
            f.runtime.execution_limits(&state, &f.worker, 105).unwrap(),
            (15, 120)
        );
        assert!(f.runtime.execution_limits(&state, &f.worker, 120).is_err());
        state.packages[0].input.budget.deadline = 999999;
        assert_eq!(
            f.runtime
                .execution_limits(&state, &f.worker, 105)
                .unwrap()
                .1,
            f.worker.claim.lease_expires_at
        );
    }
    #[test]
    fn renewal_near_fixed_deadline_does_not_cancel_early() {
        let mut f = Fixture::new();
        let state = snapshot(&f.store, &f.id).unwrap();
        let now = f.worker.claim.lease_expires_at - 30;
        f.runtime
            .reconcile(&mut f.store, &state, &f.worker, now)
            .unwrap();
        assert!(!f.directory(&f.worker).join("cancel.json").exists());
    }
    #[test]
    fn expired_claim_without_manifest_has_not_started_proof() {
        let mut f = Fixture::new();
        fs::remove_file(f.directory(&f.worker).join("dispatch.json")).unwrap();
        let now = f.worker.claim.lease_expires_at + 1;
        f.store.recover_expired_leases(now).unwrap();
        let state = snapshot(&f.store, &f.id).unwrap();
        let worker = state
            .executions
            .iter()
            .find(|e| e.id == f.worker.id)
            .unwrap()
            .clone();
        f.runtime
            .reconcile(&mut f.store, &state, &worker, now)
            .unwrap();
        let state = snapshot(&f.store, &f.id).unwrap();
        assert!(
            state
                .executions
                .iter()
                .find(|e| e.id == worker.id)
                .unwrap()
                .cessation_verified
        );
        let proof = &state.reconciliations.last().unwrap().input;
        assert!(proof.not_started);
        assert!(proof.reaped_boundary.is_none());
    }
    #[test]
    fn failed_pre_turn_start_parks_attention_and_charges_once() {
        let mut f = Fixture::new();
        f.event(
            &f.supervisor,
            1,
            "failure",
            json!({"type":"EOFError","message":"transport closed"}),
        );
        f.event(
            &f.supervisor,
            2,
            "stderr_diagnostic",
            json!({"classes":["invalid_mcp_transport_configuration"]}),
        );
        f.event(
            &f.supervisor,
            3,
            "boundary_reaped",
            json!({"boundary":"supervisor:reaped"}),
        );
        let state = snapshot(&f.store, &f.id).unwrap();
        f.runtime
            .reconcile(&mut f.store, &state, &f.supervisor, 105)
            .unwrap();
        let state = snapshot(&f.store, &f.id).unwrap();
        assert_eq!(state.root.state, crate::ObligationState::Attention);
        assert_eq!(state.recoveries_used, 1);
        assert!(
            state
                .reconciliations
                .last()
                .unwrap()
                .input
                .runtime_failure
                .as_ref()
                .unwrap()
                .contains("invalid_mcp_transport_configuration")
        );
        f.runtime
            .reconcile(&mut f.store, &state, &f.supervisor, 106)
            .unwrap();
        assert_eq!(snapshot(&f.store, &f.id).unwrap().recoveries_used, 1);
        assert!(
            f.store
                .claim_due_engineering(EngineeringRole::Supervisor, 107, 600, 1)
                .unwrap()
                .is_empty()
        );
    }
    #[test]
    fn validation_rejects_checks_run_against_an_older_source() {
        let mut f = Fixture::new();
        fs::write(f.runtime.profile.workspace.join("reader.txt"), "new").unwrap();
        let (artefact, _) = f.runtime.file("reader.txt").unwrap();
        let mut log = vec![Event {
            sequence: 1,
            kind: "item/completed".into(),
            value: json!({"threadId":"thread","turnId":"turn","item":{"id":"check","type":"commandExecution","command":"test reader","aggregatedOutput":"PASS","exitCode":0}}),
        }];
        for phase in ["item/started", "item/completed"] {
            log.push(Event { sequence:log.len() as u64 + 1,kind:"command_source".into(),
                value:json!({"phase":phase,"item_id":"check","thread_id":"thread","turn_id":"turn","source":{"files":{"reader.txt":{"sha256":sha(b"old"),"byte_length":3}}}})});
        }
        let directory = f.directory(&f.worker);
        let params = json!({"tool":"bokkie_validation","arguments":{"item_id":"check","artefact":artefact,"criterion_id":"reader"}});
        assert!(
            f.runtime
                .dynamic(
                    &mut f.store,
                    &directory,
                    &f.worker,
                    "old-check",
                    &params,
                    &log,
                    105
                )
                .unwrap_err()
                .to_string()
                .contains("submitted source revision")
        );
        for event in log.iter_mut().skip(1) {
            event.value["source"]["files"]["reader.txt"]["sha256"] = json!(sha(b"new"));
        }
        assert!(
            f.runtime
                .dynamic(
                    &mut f.store,
                    &directory,
                    &f.worker,
                    "current-check",
                    &params,
                    &log,
                    105
                )
                .is_ok()
        );
    }
    #[test]
    fn profile_rejects_infinite_bounds_and_overlapping_storage() {
        let fixture = Fixture::new();
        let mut profile = fixture.runtime.profile.clone();
        profile.max_turns = 0;
        assert!(profile.validate().is_err());
        profile.max_turns = 24;
        profile.broker_root = profile.workspace.clone();
        assert!(profile.validate().is_err());
    }
    #[test]
    fn database_rejects_workspace_and_symlink_aliases_before_open() {
        use std::os::unix::fs::symlink;
        let f = Fixture::new();
        let profile = &f.runtime.profile;
        assert!(
            profile
                .validate_database(&profile.workspace.join("new.sqlite"))
                .is_err()
        );
        assert!(
            profile
                .validate_database(&profile.broker_root.join("new.sqlite"))
                .is_ok()
        );
        assert!(
            profile
                .validate_database(Path::new("relative.sqlite"))
                .is_err()
        );
        let alias = profile.broker_root.join("workspace-alias");
        symlink(&profile.workspace, &alias).unwrap();
        assert!(
            profile
                .validate_database(&alias.join("new.sqlite"))
                .is_err()
        );
        let inside = profile.workspace.join("existing.sqlite");
        fs::write(&inside, "").unwrap();
        let alias_file = profile.broker_root.join("database-alias.sqlite");
        symlink(&inside, &alias_file).unwrap();
        assert!(profile.validate_database(&alias_file).is_err());
    }
    #[test]
    fn supervisor_dynamic_missing_information_routes_fact_without_new_authority() {
        let mut f = Fixture::new();
        let directory = f.directory(&f.supervisor);
        let key = "d".repeat(64);
        let request = Event {
            sequence: 1,
            kind: "request".into(),
            value: json!({"key":key,"message":{"id":13,"method":"item/tool/call","params":{"tool":"bokkie_question","arguments":{"prompt":"Which local source directory should the reader use?","kind":"missing_information","options":[]}}}}),
        };
        f.runtime
            .request(&mut f.store, &directory, &f.supervisor, &request, &[], 105)
            .unwrap();
        let state = snapshot(&f.store, &f.id).unwrap();
        assert_eq!(state.questions.len(), 1);
        assert_eq!(
            state.questions[0].kind,
            EngineeringQuestionKind::MissingInformation
        );
        assert!(directory.join("cancel.json").exists());
        assert!(
            directory
                .join("replies")
                .join(format!("{key}.json"))
                .exists()
        );
        assert!(state.contract().authority.is_empty());
    }
    #[test]
    fn supervisor_adapter_cancels_delegated_package_but_not_outcome() {
        let mut f = Fixture::new();
        let state = snapshot(&f.store, &f.id).unwrap();
        let directory = f.directory(&f.supervisor);
        let cancel = |package_id: Option<String>| {
            json!({
                "expected": state.precondition(),
                "command": {"kind":"request_cancellation", "input":{"package_id":package_id}},
            })
        };
        let error = f
            .runtime
            .command(
                &mut f.store,
                &directory,
                &f.supervisor,
                "cancel-outcome",
                cancel(None),
                105,
            )
            .unwrap_err();
        assert!(error.to_string().contains("adapter authority"));
        let worker_directory = f.directory(&f.worker);
        let error = f
            .runtime
            .command(
                &mut f.store,
                &worker_directory,
                &f.worker,
                "worker-cancel",
                cancel(f.worker.package_id.clone()),
                105,
            )
            .unwrap_err();
        assert!(error.to_string().contains("adapter authority"));
        f.runtime
            .command(
                &mut f.store,
                &directory,
                &f.supervisor,
                "cancel-package",
                cancel(f.worker.package_id.clone()),
                105,
            )
            .unwrap();
        let state = snapshot(&f.store, &f.id).unwrap();
        assert!(!state.cancellation_requested);
        assert!(state.packages[0].cancellation_requested);
        assert_eq!(state.root.state, crate::ObligationState::Running);
        assert!(
            state
                .executions
                .iter()
                .find(|e| e.id == f.worker.id)
                .unwrap()
                .fenced
        );
        assert!(
            !state
                .executions
                .iter()
                .find(|e| e.id == f.worker.id)
                .unwrap()
                .cessation_verified
        );
    }

    #[test]
    fn stale_model_decision_cannot_refresh_its_snapshot() {
        let mut f = Fixture::new();
        let old = snapshot(&f.store, &f.id).unwrap();
        f.store
            .engineering_command(
                EngineeringActor::Operator {
                    name: "operator".into(),
                },
                EngineeringCommandEnvelope {
                    command_id: "followup".into(),
                    expected: Some(old.precondition()),
                    command: EngineeringCommand::FollowUp {
                        text: "Preserve attachments too".into(),
                    },
                },
                105,
            )
            .unwrap();
        let directory = f.directory(&f.supervisor);
        let error=f.runtime.command(&mut f.store,&directory,&f.supervisor,"stale",json!({"expected":old.precondition(),"command":{"kind":"yield_supervisor","input":{"next_wake_at":200,"reason":"waiting","processed_message_count":1}}}),106).unwrap_err();
        assert!(error.to_string().contains("snapshot changed"));
        assert_eq!(
            snapshot(&f.store, &f.id).unwrap().contract().intent,
            old.contract().intent
        );
    }
    #[test]
    fn grouped_questions_and_saved_answers_survive_controller_replay() {
        let mut f = Fixture::new();
        let directory = f.directory(&f.worker);
        let key = "a".repeat(64);
        let request = Event {
            sequence: 1,
            kind: "request".into(),
            value: json!({"key":key,"message":{"id":9,"method":"item/tool/requestUserInput","params":{"questions":[{"id":"colour","question":"Which colour?","options":[]},{"id":"name","question":"Which name?","options":[]}]}}}),
        };
        f.runtime
            .request(&mut f.store, &directory, &f.worker, &request, &[], 105)
            .unwrap();
        let state = snapshot(&f.store, &f.id).unwrap();
        assert_eq!(state.questions.len(), 2);
        assert!(
            !directory
                .join("replies")
                .join(format!("{key}.json"))
                .exists()
        );
        for question in state.questions {
            let current = snapshot(&f.store, &f.id).unwrap();
            f.store
                .engineering_command(
                    actor(&f.supervisor),
                    EngineeringCommandEnvelope {
                        command_id: format!("answer-{}", question.id),
                        expected: Some(current.precondition()),
                        command: EngineeringCommand::ResolveQuestion {
                            question_id: question.id,
                            answer: "teal".into(),
                            authority_grants: vec![],
                        },
                    },
                    106,
                )
                .unwrap();
        }
        f.runtime
            .request(&mut f.store, &directory, &f.worker, &request, &[], 107)
            .unwrap();
        let reply: Value =
            read_json(&directory.join("replies").join(format!("{key}.json"))).unwrap();
        assert_eq!(reply["result"]["answers"]["colour"]["answers"][0], "teal");
        f.runtime
            .request(&mut f.store, &directory, &f.worker, &request, &[], 108)
            .unwrap();
        assert_eq!(snapshot(&f.store, &f.id).unwrap().questions.len(), 2);
    }
    #[test]
    fn post_reap_submission_is_pending_assessment_and_replay_is_free() {
        let mut f = Fixture::new();
        let directory = f.directory(&f.worker);
        fs::write(
            f.runtime.profile.workspace.join("reader.txt"),
            "page opened",
        )
        .unwrap();
        let (artefact, _) = f.runtime.file("reader.txt").unwrap();
        let input = EngineeringSubmissionInput {
            artefacts: vec![artefact],
            evidence: vec![],
            limitations: "Verification still required".into(),
        };
        atomic(&directory.join("submission.json"), &input).unwrap();
        f.event(
            &f.worker,
            1,
            "boundary_reaped",
            json!({"boundary":"fixture:namespace","generation":"fixture"}),
        );
        let state = snapshot(&f.store, &f.id).unwrap();
        f.runtime
            .reconcile(&mut f.store, &state, &f.worker, 105)
            .unwrap();
        let state = snapshot(&f.store, &f.id).unwrap();
        assert_eq!(state.submissions.len(), 1);
        assert!(state.acceptance.is_none());
        assert!(!state.root.state.is_terminal());
        assert!(
            state
                .executions
                .iter()
                .find(|e| e.id == f.worker.id)
                .unwrap()
                .cessation_verified
        );
        f.runtime
            .reconcile(&mut f.store, &state, &f.worker, 106)
            .unwrap();
        assert_eq!(snapshot(&f.store, &f.id).unwrap().submissions.len(), 1);
        assert!(
            f.store
                .claim_due_engineering(EngineeringRole::Worker, 106, 600, 1)
                .unwrap()
                .is_empty()
        );
    }
    #[test]
    fn invented_validation_and_changed_artefacts_are_rejected() {
        let f = Fixture::new();
        fs::write(f.runtime.profile.workspace.join("reader.txt"), "before").unwrap();
        let (artefact, _) = f.runtime.file("reader.txt").unwrap();
        let evidence = EngineeringCriterionEvidence {
            criterion_id: "reader".into(),
            artefact: artefact.clone(),
            command_digest: f.runtime.blob(b"test").unwrap(),
            output_digest: f.runtime.blob(b"PASS").unwrap(),
            exit_code: 0,
        };
        assert!(
            f.runtime
                .verify_submission(&EngineeringSubmissionInput {
                    artefacts: vec![artefact.clone()],
                    evidence: vec![evidence],
                    limitations: String::new()
                })
                .unwrap_err()
                .to_string()
                .contains("actual recorded")
        );
        fs::write(f.runtime.profile.workspace.join("reader.txt"), "after").unwrap();
        assert!(f.runtime.inspect(&artefact).is_err());
    }
    #[test]
    fn unknown_broker_death_never_releases_the_writer() {
        let mut f = Fixture::new();
        f.event(
            &f.worker,
            1,
            "launch_committed",
            json!({"generation":"dead"}),
        );
        let state = snapshot(&f.store, &f.id).unwrap();
        f.runtime
            .reconcile(&mut f.store, &state, &f.worker, 105)
            .unwrap();
        let state = snapshot(&f.store, &f.id).unwrap();
        assert!(
            !state
                .executions
                .iter()
                .find(|e| e.id == f.worker.id)
                .unwrap()
                .cessation_verified
        );
        assert!(
            state
                .reconciliations
                .last()
                .unwrap()
                .input
                .reaped_boundary
                .is_none()
        );
        assert!(
            f.store
                .claim_due_engineering(EngineeringRole::Worker, 106, 600, 1)
                .unwrap()
                .is_empty()
        );
    }
    #[test]
    fn intake_replay_preserves_original_deadline_and_intent() {
        let mut f = Fixture::new();
        let receipt = intake(
            &mut f.store,
            &f.runtime.profile,
            "Build the requested local reader, preserving source pages.".into(),
            "intake".into(),
            10000,
        )
        .unwrap();
        assert_eq!(receipt.outcome_id, f.id);
        assert_eq!(
            snapshot(&f.store, &f.id).unwrap().contracts[0]
                .contract
                .budget
                .deadline,
            14500
        );
        assert!(
            intake(
                &mut f.store,
                &f.runtime.profile,
                "Different intent".into(),
                "intake".into(),
                10000
            )
            .is_err()
        );
    }
    #[test]
    fn default_mode_question_tool_waits_for_durable_supervisor_answer() {
        let mut f = Fixture::new();
        let directory = f.directory(&f.worker);
        let key = "c".repeat(64);
        let request = Event {
            sequence: 1,
            kind: "request".into(),
            value: json!({"key":key,"message":{"id":12,"method":"item/tool/call","params":{"tool":"bokkie_question","arguments":{"prompt":"Use teal?","kind":"routine","options":["teal","blue"]}}}}),
        };
        f.runtime
            .request(&mut f.store, &directory, &f.worker, &request, &[], 105)
            .unwrap();
        let state = snapshot(&f.store, &f.id).unwrap();
        assert_eq!(state.questions.len(), 1);
        f.store
            .engineering_command(
                actor(&f.supervisor),
                EngineeringCommandEnvelope {
                    command_id: "dynamic-answer".into(),
                    expected: Some(state.precondition()),
                    command: EngineeringCommand::ResolveQuestion {
                        question_id: state.questions[0].id.clone(),
                        answer: "teal".into(),
                        authority_grants: vec![],
                    },
                },
                106,
            )
            .unwrap();
        f.runtime
            .request(&mut f.store, &directory, &f.worker, &request, &[], 107)
            .unwrap();
        let reply: Value =
            read_json(&directory.join("replies").join(format!("{key}.json"))).unwrap();
        assert_eq!(reply["result"]["success"], true);
        assert!(
            reply["result"]["contentItems"][0]["text"]
                .as_str()
                .unwrap()
                .contains("teal")
        );
    }
    fn child_review_log(message: &str) -> Vec<Event> {
        vec![
            Event {
                sequence: 1,
                kind: "thread_identity".into(),
                value: json!({"thread_id":"root-thread"}),
            },
            Event {
                sequence: 2,
                kind: "item/started".into(),
                value: json!({"threadId":"root-thread","turnId":"root-turn","item":{"id":"spawn","type":"subAgentActivity","kind":"started","agentPath":"/root/review","agentThreadId":"child-thread"}}),
            },
            Event {
                sequence: 3,
                kind: "item/completed".into(),
                value: json!({"threadId":"root-thread","turnId":"root-turn","item":{"id":"spawn","type":"subAgentActivity","kind":"started","agentPath":"/root/review","agentThreadId":"child-thread"}}),
            },
            Event {
                sequence: 4,
                kind: "item/completed".into(),
                value: json!({"threadId":"child-thread","turnId":"child-turn","item":{"id":"report","type":"agentMessage","phase":"final_answer","text":message}}),
            },
            Event {
                sequence: 5,
                kind: "turn/completed".into(),
                value: json!({"threadId":"child-thread","turn":{"id":"child-turn","status":"completed"}}),
            },
            Event {
                sequence: 6,
                kind: "item/completed".into(),
                value: json!({"threadId":"root-thread","turnId":"root-turn","item":{"type":"collabAgentToolCall","tool":"wait","agentsStates":{},"receiverThreadIds":[]}}),
            },
        ]
    }
    #[test]
    fn actual_child_review_is_discoverable_and_retains_exact_turn_provenance() {
        let mut f = Fixture::new();
        fs::write(f.runtime.profile.workspace.join("reader.txt"), "page").unwrap();
        let (artefact, _) = f.runtime.file("reader.txt").unwrap();
        let message = json!({"artefacts":[artefact],"verdict":"repair","findings":["Missing required multiplication"]}).to_string();
        let log = child_review_log(&message);
        let directory = f.directory(&f.worker);
        let discovered = f
            .runtime
            .dynamic(
                &mut f.store,
                &directory,
                &f.worker,
                "discover",
                &json!({"tool":"bokkie_commands","arguments":{}}),
                &log,
                105,
            )
            .unwrap();
        assert!(discovered["commands"].is_array());
        assert_eq!(
            discovered["reviewer_candidates"][0]["reviewer_thread_id"],
            "child-thread"
        );
        assert_eq!(
            discovered["reviewer_candidates"][0]["reviewer_turn_id"],
            "child-turn"
        );
        assert_eq!(
            discovered["reviewer_candidates"][0]["agent_path"],
            "/root/review"
        );
        let error = f
            .runtime
            .dynamic(
                &mut f.store,
                &directory,
                &f.worker,
                "bad-alias",
                &json!({"tool":"bokkie_review","arguments":{"reviewer_thread_id":"/root/review"}}),
                &log,
                105,
            )
            .unwrap_err();
        assert!(error.to_string().contains("child-thread"));
        let registered = f.runtime.dynamic(&mut f.store,&directory,&f.worker,"child-review",
            &json!({"tool":"bokkie_review","arguments":{"reviewer_thread_id":"child-thread","reviewer_turn_id":"child-turn","message":"invented replacement text"}}),&log,105).unwrap();
        let review: EngineeringReviewEvidence = serde_json::from_value(registered).unwrap();
        assert_eq!(
            review.reviewer_identity,
            "codex-thread:child-thread:turn:child-turn"
        );
        assert_eq!(review.evidence_digest, sha(message.as_bytes()));
        f.runtime.verify_review(&review, &f.supervisor).unwrap();
        let provenance: Value =
            read_json(&directory.join("receipts/review-provenance-child-review.json")).unwrap();
        let bytes = f
            .runtime
            .evidence(provenance["provenance_digest"].as_str().unwrap())
            .unwrap();
        let retained: Vec<Event> = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(retained.len(), 3);
        assert_eq!(retained[2].kind, "turn/completed");
        fs::write(f.runtime.profile.workspace.join("reader.txt"), "changed").unwrap();
        assert!(f.runtime.verify_review(&review, &f.supervisor).is_err());
    }
    #[test]
    fn child_review_rejects_missing_parent_failed_turn_and_mismatched_final() {
        let base = child_review_log("{}");
        assert_eq!(observed_reviews(&base).len(), 1);
        let mut missing_parent = base.clone();
        missing_parent.retain(|e| e.value["item"]["type"] != "subAgentActivity");
        assert!(observed_reviews(&missing_parent).is_empty());
        for (event_index, field, replacement) in
            [(4, "status", "failed"), (4, "id", "different-turn")]
        {
            let mut log = base.clone();
            log[event_index].value["turn"][field] = json!(replacement);
            assert!(observed_reviews(&log).is_empty());
        }
        let mut wrong_thread = base.clone();
        wrong_thread[3].value["threadId"] = json!("unrelated-thread");
        assert!(observed_reviews(&wrong_thread).is_empty());
        let mut commentary = base.clone();
        commentary[3].value["item"]["phase"] = json!("commentary");
        assert!(observed_reviews(&commentary).is_empty());
        let mut self_review = base.clone();
        self_review[1].value["item"]["agentThreadId"] = json!("root-thread");
        self_review[2].value["item"]["agentThreadId"] = json!("root-thread");
        self_review[3].value["threadId"] = json!("root-thread");
        self_review[4].value["threadId"] = json!("root-thread");
        assert!(observed_reviews(&self_review).is_empty());
    }
    #[test]
    fn child_review_discovery_does_not_grant_root_tools_to_the_child() {
        let mut f = Fixture::new();
        let log = child_review_log("{}");
        let directory = f.directory(&f.worker);
        let request = Event {
            sequence: 7,
            kind: "request".into(),
            value: json!({"key":"a".repeat(64),"message":{"id":12,"method":"item/tool/call","params":{"threadId":"child-thread","turnId":"child-turn","tool":"bokkie_review","arguments":{"reviewer_thread_id":"child-thread"}}}}),
        };
        f.runtime
            .request(&mut f.store, &directory, &f.worker, &request, &log, 105)
            .unwrap();
        let reply: Value = read_json(
            &directory
                .join("replies")
                .join(format!("{}.json", "a".repeat(64))),
        )
        .unwrap();
        assert_eq!(reply["result"]["success"], false);
        assert!(
            reply["result"]["contentItems"][0]["text"]
                .as_str()
                .unwrap()
                .contains("nested agent cannot inherit root Bokkie authority")
        );
    }
    #[test]
    fn independent_review_requires_observed_subagent_report() {
        let mut f = Fixture::new();
        fs::write(f.runtime.profile.workspace.join("reader.txt"), "page").unwrap();
        let (artefact, _) = f.runtime.file("reader.txt").unwrap();
        let message = json!({"artefacts":[artefact],"verdict":"pass","findings":[]}).to_string();
        let guessed = EngineeringReviewEvidence {
            reviewer_identity: "invented".into(),
            artefacts: vec![artefact.clone()],
            evidence_digest: f.runtime.blob(message.as_bytes()).unwrap(),
        };
        assert!(f.runtime.verify_review(&guessed, &f.supervisor).is_err());
        let log = vec![
            Event {
                sequence: 1,
                kind: "thread_identity".into(),
                value: json!({"thread_id":"root-thread"}),
            },
            Event {
                sequence: 2,
                kind: "item/completed".into(),
                value: json!({"threadId":"root-thread","item":{"type":"collabAgentToolCall","agentsStates":{"review-thread":{"status":"completed","message":message}}}}),
            },
        ];
        let directory = f.directory(&f.worker);
        let registered = f
            .runtime
            .dynamic(
                &mut f.store,
                &directory,
                &f.worker,
                "review",
                &json!({"tool":"bokkie_review","arguments":{"reviewer_thread_id":"review-thread"}}),
                &log,
                105,
            )
            .unwrap();
        let review: EngineeringReviewEvidence = serde_json::from_value(registered).unwrap();
        f.runtime.verify_review(&review, &f.supervisor).unwrap();
        assert_eq!(review.reviewer_identity, "codex-thread:review-thread");
    }
}
