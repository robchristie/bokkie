//! SQLite owns editable roles; deployment owns containment and hard ceilings.
use crate::{
    Store, StoreError,
    conversation::{decode, encode},
    conversation_runtime::ConversationProfile,
};
use bokkie_operator_api::*;
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AcceptedAgentProfile {
    pub profile: AgentProfileRevision,
    pub runtime: ConversationProfile,
    pub mandatory_instructions: String,
    pub permissions: String,
    pub deadline_unix: i64,
}

pub fn deployment_settings(p: &ConversationProfile) -> AgentRoleSettings {
    AgentRoleSettings {
        model: p.model.clone(),
        effort: p.effort.clone(),
        additional_instructions: String::new(),
        timeout_seconds: p.timeout_seconds,
        max_context_bytes: p.max_context_bytes,
        max_output_bytes: p.max_output_bytes,
        max_model_calls: 2,
    }
}

pub fn catalogue(value: Value) -> Result<Vec<AgentModelOption>, StoreError> {
    let rows = value
        .get("models")
        .and_then(Value::as_array)
        .ok_or_else(|| StoreError::Invalid("Runtime model catalogue is unavailable".into()))?;
    if rows.is_empty() || rows.len() > 128 {
        return Err(StoreError::Invalid(
            "Runtime returned an empty or oversized model catalogue".into(),
        ));
    }
    rows.iter()
        .map(|row| {
            let string = |key: &str| {
                row.get(key)
                    .and_then(Value::as_str)
                    .filter(|v| !v.is_empty() && v.len() <= 256)
                    .map(str::to_owned)
                    .ok_or_else(|| {
                        StoreError::Invalid("Runtime returned invalid model capabilities".into())
                    })
            };
            let efforts = row
                .get("supportedReasoningEfforts")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    StoreError::Invalid("Runtime omitted supported thinking levels".into())
                })?
                .iter()
                .map(|v| {
                    v.get("reasoningEffort")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty() && s.len() <= 32)
                        .map(str::to_owned)
                        .ok_or_else(|| {
                            StoreError::Invalid("Runtime returned an invalid thinking level".into())
                        })
                })
                .collect::<Result<Vec<_>, _>>()?;
            let model = AgentModelOption {
                model: string("model")?,
                display_name: string("displayName")?,
                supported_efforts: efforts,
                default_effort: string("defaultReasoningEffort")?,
            };
            if !model.supported_efforts.is_empty()
                && !model.supported_efforts.contains(&model.default_effort)
            {
                return Err(StoreError::Invalid(
                    "Runtime default thinking level is unsupported".into(),
                ));
            }
            Ok(model)
        })
        .collect()
}

pub fn validate_role(
    role: &AgentRoleSettings,
    ceiling: &ConversationProfile,
    models: &[AgentModelOption],
) -> Result<(), StoreError> {
    let model = models
        .iter()
        .find(|m| m.model == role.model)
        .ok_or_else(|| {
            StoreError::Invalid("Choose a model available to this runtime account".into())
        })?;
    if !model.supported_efforts.contains(&role.effort) {
        return Err(StoreError::Invalid(
            "That thinking level is not supported by the selected model".into(),
        ));
    }
    if role.additional_instructions.len() > 8192 || role.additional_instructions.contains('\0') {
        return Err(StoreError::Invalid(
            "Additional instructions must fit within 8 KiB and contain no NUL".into(),
        ));
    }
    if role.timeout_seconds == 0 || role.timeout_seconds > ceiling.timeout_seconds {
        return Err(StoreError::Invalid(format!(
            "Time per model call must be between 1 and {} seconds",
            ceiling.timeout_seconds
        )));
    }
    if !(1024..=ceiling.max_context_bytes).contains(&role.max_context_bytes) {
        return Err(StoreError::Invalid(format!(
            "Context size must be between 1024 and {} bytes",
            ceiling.max_context_bytes
        )));
    }
    if !(1024..=ceiling.max_output_bytes).contains(&role.max_output_bytes) {
        return Err(StoreError::Invalid(format!(
            "Response size must be between 1024 and {} bytes",
            ceiling.max_output_bytes
        )));
    }
    if !(1..=2).contains(&role.max_model_calls) {
        return Err(StoreError::Invalid(
            "Model calls per request must be 1 or 2".into(),
        ));
    }
    Ok(())
}

pub(crate) fn bootstrap(
    tx: &rusqlite::Transaction<'_>,
    deployment: &ConversationProfile,
    now: i64,
) -> Result<(), StoreError> {
    let exists: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM agent_profile_active)",
        [],
        |r| r.get(0),
    )?;
    if !exists {
        let profile = AgentProfileRevision {
            revision: 1,
            contract_version: 1,
            main: deployment_settings(deployment),
        };
        tx.execute(
            "INSERT INTO agent_profile_revisions VALUES(1,?1,?2)",
            params![encode(&profile)?, now],
        )?;
        tx.execute("INSERT INTO agent_profile_active VALUES(1,1)", [])?;
    }
    Ok(())
}
pub(crate) fn active(tx: &rusqlite::Transaction<'_>) -> Result<AgentProfileRevision, StoreError> {
    let raw:String=tx.query_row("SELECT profile_json FROM agent_profile_revisions JOIN agent_profile_active USING(revision)",[],|r|r.get(0))?;
    decode(&raw)
}
impl Store {
    pub fn agent_settings(
        &mut self,
        deployment: Option<&ConversationProfile>,
        now: i64,
    ) -> Result<Option<AgentProfileRevision>, StoreError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(p) = deployment {
            bootstrap(&tx, p, now)?;
        }
        let raw:Option<String>=tx.query_row("SELECT profile_json FROM agent_profile_revisions JOIN agent_profile_active USING(revision)",[],|r|r.get(0)).optional()?;
        tx.commit()?;
        raw.map(|s| decode(&s)).transpose()
    }
    pub fn agent_settings_save(
        &mut self,
        request: &AgentSettingsSaveRequest,
        deployment: &ConversationProfile,
        models: &[AgentModelOption],
        now: i64,
    ) -> Result<AgentProfileRevision, StoreError> {
        if request.command_id.is_empty()
            || request.command_id.len() > 128
            || request.command_id.contains('\0')
        {
            return Err(StoreError::Invalid(
                "Settings command identity is invalid".into(),
            ));
        }
        let payload = encode(request)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some((stored,raw))=tx.query_row("SELECT payload_json,profile_json FROM agent_settings_commands JOIN agent_profile_revisions USING(revision) WHERE command_id=?1",[&request.command_id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?))).optional()? {
            if stored!=payload {return Err(StoreError::Conflict("Settings command reused with changed values".into()));}
            return decode(&raw);
        }
        validate_role(&request.main, deployment, models)?;
        bootstrap(&tx, deployment, now)?;
        let previous = active(&tx)?;
        if previous.revision != request.expected_revision {
            return Err(StoreError::Conflict(
                "Settings changed elsewhere. Reload the saved revision before saving your edits"
                    .into(),
            ));
        }
        let profile = AgentProfileRevision {
            revision: previous.revision + 1,
            contract_version: 1,
            main: request.main.clone(),
        };
        tx.execute(
            "INSERT INTO agent_profile_revisions VALUES(?1,?2,?3)",
            params![profile.revision, encode(&profile)?, now],
        )?;
        tx.execute(
            "UPDATE agent_profile_active SET revision=?1 WHERE singleton=1",
            [profile.revision],
        )?;
        tx.execute(
            "INSERT INTO agent_settings_commands VALUES(?1,?2,?3)",
            params![request.command_id, payload, profile.revision],
        )?;
        tx.execute("INSERT INTO domain_events(entity_kind,entity_id,event_type,occurred_at,details_json) VALUES('agent_settings','main','agent_settings_saved',?1,?2)",params![now,encode(&profile)?])?;
        tx.commit()?;
        Ok(profile)
    }
    pub fn conversation_replay(
        &self,
        request: &ConversationTurnRequest,
    ) -> Result<bool, StoreError> {
        let stored: Option<String> = self
            .connection
            .query_row(
                "SELECT payload_json FROM conversation_requests WHERE command_id=?1",
                [&request.command_id],
                |r| r.get(0),
            )
            .optional()?;
        match stored {
            Some(raw) if raw == encode(request)? => Ok(true),
            Some(_) => Err(StoreError::Conflict(
                "Conversation command ID reused with changed payload".into(),
            )),
            None => Ok(false),
        }
    }
    pub fn accepted_agent_profile(&self, id: &str) -> Result<AcceptedAgentProfile, StoreError> {
        let raw: Option<String> = self.connection.query_row(
            "SELECT accepted_profile_json FROM conversation_requests WHERE command_id=?1",
            [id],
            |r| r.get(0),
        )?;
        decode(&raw.ok_or_else(|| {
            StoreError::Invalid(
                "Legacy request has no recorded profile and will not be relaunched".into(),
            )
        })?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn deployment() -> ConversationProfile {
        ConversationProfile {
            broker: "/usr/bin/true".into(),
            codex: "/usr/bin/true".into(),
            bwrap: "/usr/bin/true".into(),
            model: "one".into(),
            effort: "medium".into(),
            timezone: "Australia/Adelaide".into(),
            timeout_seconds: 90,
            max_context_bytes: 65536,
            max_output_bytes: 16384,
        }
    }
    fn models() -> Vec<AgentModelOption> {
        vec![
            AgentModelOption {
                model: "one".into(),
                display_name: "One".into(),
                supported_efforts: vec!["medium".into()],
                default_effort: "medium".into(),
            },
            AgentModelOption {
                model: "two".into(),
                display_name: "Two".into(),
                supported_efforts: vec!["high".into()],
                default_effort: "high".into(),
            },
        ]
    }
    fn turn() -> ConversationTurnRequest {
        ConversationTurnRequest {
            command_id: "request".into(),
            conversation_id: "chat".into(),
            expected_revision: 0,
            text: "Hello".into(),
        }
    }
    #[test]
    fn bootstrap_save_restart_and_request_snapshot_are_atomic() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("state.sqlite");
        let mut store = Store::open(&db).unwrap();
        let d = deployment();
        let m = models();
        let original = store.agent_settings(Some(&d), 100).unwrap().unwrap();
        assert_eq!(original.main, deployment_settings(&d));
        assert!(
            store
                .conversation_begin_profiled(&turn(), "session", 100, &d, &m, "mandatory contract")
                .unwrap()
        );
        let mut role = original.main.clone();
        role.model = "two".into();
        role.effort = "high".into();
        role.additional_instructions = "Please be concise".into();
        role.timeout_seconds = 20;
        role.max_context_bytes = 8192;
        role.max_output_bytes = 4096;
        role.max_model_calls = 1;
        let save = AgentSettingsSaveRequest {
            command_id: "save".into(),
            expected_revision: 1,
            main: role.clone(),
        };
        let saved = store.agent_settings_save(&save, &d, &m, 101).unwrap();
        assert_eq!(saved.revision, 2);
        let accepted = store.accepted_agent_profile("request").unwrap();
        assert_eq!(accepted.profile, original);
        assert_eq!(accepted.runtime.model, "one");
        assert_eq!(accepted.mandatory_instructions, "mandatory contract");
        assert_eq!(accepted.deadline_unix, 280);
        assert!(store.conversation_replay(&turn()).unwrap());
        assert!(
            !store
                .conversation_begin_profiled(&turn(), "new", 102, &d, &[], "different")
                .unwrap()
        );
        assert_eq!(
            store.agent_settings_save(&save, &d, &[], 102).unwrap(),
            saved
        );
        let mut stale = save.clone();
        stale.command_id = "stale".into();
        assert!(matches!(
            store.agent_settings_save(&stale, &d, &m, 102),
            Err(StoreError::Conflict(_))
        ));
        drop(store);
        let mut store = Store::open(&db).unwrap();
        let mut changed_deployment = d.clone();
        changed_deployment.model = "two".into();
        assert_eq!(
            store
                .agent_settings(Some(&changed_deployment), 103)
                .unwrap(),
            Some(saved.clone())
        );
        let request = ConversationTurnRequest {
            command_id: "new".into(),
            conversation_id: "newchat".into(),
            ..turn()
        };
        store
            .conversation_begin_profiled(&request, "session", 104, &d, &m, "contract")
            .unwrap();
        let new = store.accepted_agent_profile("new").unwrap();
        assert_eq!(new.profile.main, role);
        assert_eq!(new.runtime.model, "two");
        assert_eq!(new.runtime.effort, "high");
        assert_eq!(new.runtime.timeout_seconds, 20);
        store.conversation_model_dispatch(&request, 0, 104).unwrap();
        store
            .conversation_invocation_outcome("new", 0, Ok(&serde_json::json!({"answer":"done"})))
            .unwrap();
        assert!(store.conversation_model_dispatch(&request, 0, 104).is_err());
        assert!(store.conversation_model_dispatch(&request, 1, 104).is_err());
        store.conversation_interrupt("next", 105).unwrap();
        assert!(store.conversation_replay(&request).unwrap());
        assert_eq!(store.accepted_agent_profile("new").unwrap().profile, saved);
    }
    #[test]
    fn unsupported_settings_and_ceilings_never_partially_activate() {
        let mut store = Store::open_in_memory().unwrap();
        let d = deployment();
        let m = models();
        let original = store.agent_settings(Some(&d), 100).unwrap().unwrap();
        let mut role = original.main.clone();
        let mut save = AgentSettingsSaveRequest {
            command_id: "bad".into(),
            expected_revision: 1,
            main: role.clone(),
        };
        for field in 0..7 {
            role = original.main.clone();
            match field {
                0 => role.model = "unknown".into(),
                1 => role.effort = "high".into(),
                2 => role.timeout_seconds = 91,
                3 => role.max_context_bytes = 65537,
                4 => role.max_output_bytes = 16385,
                5 => role.max_model_calls = 3,
                _ => role.additional_instructions = "é".repeat(4097),
            };
            save.main = role;
            assert!(store.agent_settings_save(&save, &d, &m, 101).is_err());
            assert_eq!(
                store.agent_settings(Some(&d), 101).unwrap(),
                Some(original.clone())
            );
        }
        assert!(store.agent_settings_save(&save, &d, &[], 101).is_err());
    }
}
