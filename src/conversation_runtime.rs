//! Bounded model proposals; trusted conversation handlers own all state changes.
use crate::process::{
    CancellationToken, EffectRisk, NoopHeartbeat, ProcessLimits, ProcessOutcome, ProcessSupervisor,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs,
    io::Read,
    path::{Component, Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConversationProfile {
    pub broker: PathBuf,
    pub codex: PathBuf,
    pub bwrap: PathBuf,
    pub model: String,
    pub effort: String,
    pub timezone: String,
    pub timeout_seconds: u64,
    pub max_context_bytes: usize,
    pub max_output_bytes: usize,
}

impl ConversationProfile {
    pub fn load(path: &Path) -> Result<Self, String> {
        let mut bytes = Vec::new();
        fs::File::open(path)
            .map_err(|e| e.to_string())?
            .take(16 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() > 16 * 1024 {
            return Err("conversation profile exceeded bound".into());
        }
        let profile: Self = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        profile.validate()?;
        Ok(profile)
    }

    pub fn validate(&self) -> Result<(), String> {
        for path in [&self.broker, &self.codex, &self.bwrap] {
            if !path.is_absolute()
                || !path.is_file()
                || path.components().any(|c| matches!(c, Component::ParentDir))
                || path.starts_with("/tmp")
            {
                return Err(
                    "conversation executable paths must be existing absolute paths outside /tmp"
                        .into(),
                );
            }
        }
        if self.model.is_empty()
            || self.model.len() > 128
            || !self
                .model
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
            || !matches!(self.effort.as_str(), "low" | "medium" | "high")
            || self.timezone.parse::<chrono_tz::Tz>().is_err()
            || !(1..=180).contains(&self.timeout_seconds)
            || !(1024..=131072).contains(&self.max_context_bytes)
            || !(1024..=32768).contains(&self.max_output_bytes)
        {
            return Err(
                "conversation profile requires a model, effort, timezone and finite bounds".into(),
            );
        }
        Ok(())
    }

    /// Checks effective installed capability settings without starting a model turn.
    pub fn preflight(&self) -> Result<Value, String> {
        self.invoke(json!({"profile": self, "preflight": true}))
    }

    /// Returns untrusted proposed JSON. The caller must deserialise and validate it
    /// against its domain contract before applying any operation.
    pub fn generate(&self, mut context: Value, output_schema: Value) -> Result<Value, String> {
        if serde_json::to_vec(&context)
            .map_err(|e| e.to_string())?
            .len()
            > self.max_context_bytes
            || serde_json::to_vec(&output_schema)
                .map_err(|e| e.to_string())?
                .len()
                > 32768
            || output_schema.get("type").and_then(Value::as_str) != Some("object")
        {
            return Err("conversation context or schema exceeded its contract".into());
        }
        let instructions = context
            .as_object_mut()
            .and_then(|object| object.remove("instruction"))
            .unwrap_or(Value::Null);
        if !instructions.is_null() && instructions.as_str().is_none_or(|text| text.len() > 16384) {
            return Err("conversation instructions exceeded their contract".into());
        }
        self.invoke(json!({"profile": self, "context": context, "output_schema": output_schema, "instructions": instructions}))
    }

    fn invoke(&self, request: Value) -> Result<Value, String> {
        self.validate()?;
        let limits = ProcessLimits {
            stdin_message_bytes: 256 * 1024,
            stdout_bytes: self.max_output_bytes + 8192,
            stderr_bytes: 8192,
            jsonl_line_bytes: self.max_output_bytes + 8192,
            final_message_bytes: self.max_output_bytes + 8192,
            ..ProcessLimits::default()
        };
        let supervisor =
            ProcessSupervisor::new(Duration::from_secs(10), limits, CancellationToken::new())?;
        let mut command = Command::new("/usr/bin/python3");
        command.arg("-I").arg(&self.broker).current_dir("/");
        // The broker retains only the small account/transport environment it needs.
        // Python isolated mode also excludes injected modules and startup code.
        let mut child = supervisor
            .spawn(
                &mut command,
                Instant::now() + Duration::from_secs(self.timeout_seconds + 5),
                EffectRisk::None,
            )
            .map_err(|e| e.to_string())?;
        let mut heartbeat = NoopHeartbeat;
        if child
            .write_json(&request, &mut heartbeat)
            .map_err(|e| e.to_string())?
            .is_some()
        {
            return Err("conversation request could not be written within bounds".into());
        }
        child.close_stdin();
        // This adapter accepts one finite response, so collect the bounded final
        // output. Streaming receive may report normal completion before handing
        // buffered lines to a caller when a broker exits quickly.
        match child.wait(&mut heartbeat).map_err(|e| e.to_string())? {
            ProcessOutcome::Completed { status, evidence } => {
                if evidence.stdout.truncated {
                    return Err("conversation broker exceeded its output bound".into());
                }
                let value: Value = serde_json::from_slice(&evidence.stdout.tail_bytes)
                    .map_err(|_| "conversation broker must return exactly one JSON response")?;
                if !status.success() {
                    let diagnostic = value
                        .get("error")
                        .and_then(Value::as_str)
                        .filter(|text| text.len() <= 512)
                        .unwrap_or("broker exited unsuccessfully");
                    return Err(format!(
                        "conversation runtime failed: {diagnostic}; no proposal was applied"
                    ));
                }
                if !value.is_object() {
                    return Err("conversation proposal must be an object".into());
                }
                Ok(value)
            }
            _ => Err(
                "conversation broker exceeded its process bounds; no proposal was applied".into(),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn profile() -> ConversationProfile {
        ConversationProfile {
            broker: "/usr/bin/python3".into(),
            codex: "/usr/bin/python3".into(),
            bwrap: "/usr/bin/python3".into(),
            model: "fixture-model".into(),
            effort: "medium".into(),
            timezone: "Australia/Adelaide".into(),
            timeout_seconds: 30,
            max_context_bytes: 1024,
            max_output_bytes: 1024,
        }
    }
    #[test]
    fn rejects_unbounded_or_ambiguous_profiles() {
        let mut p = profile();
        assert!(p.validate().is_ok());
        p.timeout_seconds = 0;
        assert!(p.validate().is_err());
        p = profile();
        p.timezone = "Local".into();
        assert!(p.validate().is_err());
        p = profile();
        p.codex = "/tmp/codex".into();
        assert!(p.validate().is_err());
    }
    #[test]
    fn rejects_oversized_context_before_process_start() {
        let p = profile();
        assert!(
            p.generate(json!({"text":"x".repeat(1025)}), json!({"type":"object"}))
                .unwrap_err()
                .contains("context")
        );
        assert!(p.generate(json!({}), json!({"type":"string"})).is_err());
    }
}
