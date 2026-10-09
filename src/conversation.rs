//! Durable drafting and operator reviews. Models never receive mutation authority.
use crate::{Store, StoreError};
use bokkie_operator_api::*;
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::json;

pub(crate) fn encode<T: Serialize>(value: &T) -> Result<String, StoreError> {
    serde_json::to_string(value).map_err(|e| StoreError::Invalid(e.to_string()))
}
pub(crate) fn decode<T: DeserializeOwned>(value: &str) -> Result<T, StoreError> {
    serde_json::from_str(value).map_err(|e| StoreError::Invalid(e.to_string()))
}
fn bounded(value: &str, max: usize) -> Result<(), StoreError> {
    if value.is_empty() || value.contains('\0') || value.chars().count() > max {
        return Err(StoreError::Invalid(format!(
            "text must contain 1..={max} characters and no NUL"
        )));
    }
    Ok(())
}
fn event(tx: &rusqlite::Transaction<'_>, id: &str, now: i64) -> Result<(), StoreError> {
    tx.execute("INSERT INTO domain_events(entity_kind,entity_id,event_type,occurred_at,details_json) VALUES ('conversation',?1,'conversation_changed',?2,'{}')",params![id,now])?;
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum ConversationOperation {
    PrepareWorkspace {
        project_query: String,
        brief: Box<HandoffBrief>,
        #[serde(default)]
        result_contract: Option<crate::WorkspaceResultContract>,
        #[serde(default)]
        repository_scope: Option<Vec<String>>,
        #[serde(default)]
        trigger: Option<crate::ManagedTrigger>,
    },
    PrepareHandoff {
        project_query: String,
        brief: Box<HandoffBrief>,
    },
    Discuss {
        message: String,
    },
    Consult {
        question: String,
        requirements: [String; 2],
    },
    Lookup {
        query: String,
    },
    SaveDefinition {
        definition: Box<ManagedTaskDefinition>,
        message: String,
    },
    Preview,
    Propose {
        action: ConversationAction,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvocationPurpose {
    Main,
    EmptyLookupContinuation,
    AdviserManual,
    AdviserConflictingRequirements,
    AfterAdvice,
}
impl InvocationPurpose {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Main => "main",
            Self::EmptyLookupContinuation => "empty_lookup_continuation",
            Self::AdviserManual => "adviser_manual",
            Self::AdviserConflictingRequirements => "adviser_conflicting_requirements",
            Self::AfterAdvice => "after_advice",
        }
    }
    pub fn is_adviser(self) -> bool {
        matches!(
            self,
            Self::AdviserManual | Self::AdviserConflictingRequirements
        )
    }
}

type ConversationRow = (i64, Option<String>, String, Option<String>, Option<String>);

impl Store {
    pub fn conversation_list(&self) -> Result<Vec<ConversationSummary>, StoreError> {
        let mut s=self.connection.prepare("SELECT c.id,c.selected_task_id,COALESCE((SELECT text FROM conversation_messages m WHERE m.conversation_id=c.id ORDER BY sequence DESC LIMIT 1),'') FROM conversations c ORDER BY updated_at DESC,id LIMIT 50")?;
        Ok(s.query_map([], |r| {
            Ok(ConversationSummary {
                id: r.get(0)?,
                selected_task_id: r.get(1)?,
                last_text: r.get::<_, String>(2)?.chars().take(160).collect(),
            })
        })?
        .collect::<Result<Vec<_>, _>>()?)
    }
    pub fn conversation_view(
        &self,
        id: &str,
        service: ServiceIdentity,
        runtime_available: bool,
        notes_available: bool,
    ) -> Result<ConversationView, StoreError> {
        bounded(id, 128)?;
        self.with_deferred_read(|store| {
            let row:Option<ConversationRow>=store.connection.query_row("SELECT revision,selected_task_id,candidates_json,proposal_id,receipt_json FROM conversations WHERE id=?1",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?;
            let (revision,selected_task_id,candidates,proposal,receipt)=row.unwrap_or((0,None,"[]".into(),None,None));
            let mut statement=store.connection.prepare("SELECT role,text,request_id FROM (SELECT sequence,role,text,request_id FROM conversation_messages WHERE conversation_id=?1 ORDER BY sequence DESC LIMIT 24) ORDER BY sequence")?;
            let messages=statement.query_map([id],|r|Ok(ConversationMessage{role:r.get(0)?,text:r.get(1)?,request_id:r.get(2)?}))?.collect::<Result<Vec<_>,_>>()?;
            let review=proposal.map(|p|->Result<ConversationReview,StoreError>{let raw:String=store.connection.query_row("SELECT review_json FROM conversation_proposals WHERE id=?1",[p],|r|r.get(0))?;decode(&raw)}).transpose()?;
            let busy=store.connection.query_row("SELECT EXISTS(SELECT 1 FROM conversation_requests WHERE conversation_id=?1 AND status='running')",[id],|r|r.get(0))?;
            let request_error=store.connection.query_row("SELECT error FROM conversation_requests WHERE conversation_id=?1 ORDER BY rowid DESC LIMIT 1",[id],|r|r.get::<_,Option<String>>(0)).optional()?.flatten();
            let task=if let Some(selected)=&selected_task_id { match store.managed_detail_in_read(selected){Ok(t)=>Some(t),Err(StoreError::NotFound(_))=>None,Err(e)=>return Err(e)} } else {None};
            let activity = if busy {
                let latest: Option<(String,String)> = store.connection.query_row("SELECT i.purpose,i.status FROM conversation_invocations i JOIN conversation_requests r ON r.command_id=i.request_id WHERE r.conversation_id=?1 AND r.status='running' ORDER BY i.ordinal DESC LIMIT 1", [id], |r| Ok((r.get(0)?,r.get(1)?))).optional()?;
                Some(if latest.is_some_and(|(purpose,status)| purpose.starts_with("adviser_") && status == "dispatched") { "Consulting Astra" } else { "Bokkie continues" }.into())
            } else { None };
            let adviser_row: Option<(String,i64,String,Option<String>)> = store.connection.query_row("SELECT i.request_id,i.profile_revision,i.status,i.outcome_json FROM conversation_invocations i JOIN conversation_requests r ON r.command_id=i.request_id WHERE r.conversation_id=?1 AND i.purpose IN ('adviser_manual','adviser_conflicting_requirements') ORDER BY r.rowid DESC,i.ordinal DESC LIMIT 1", [id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
            let adviser_outcome = adviser_row.map(|(request_id,profile_revision,status,raw)| -> Result<_,StoreError> {
                let output = raw.as_deref().map(decode::<serde_json::Value>).transpose()?;
                let bounded_field = |key: &str| output.as_ref().and_then(|v| v[key].as_str()).map(|s| s.chars().take(4096).collect());
                let error: Option<String> = bounded_field("error");
                let status = if error.as_deref().is_some_and(|e| e.contains("timed out") || e.contains("timeout") || e.contains("time limit") || e.contains("conversation deadline exceeded")) { "timeout".into() } else { status };
                Ok(ConversationAdviserOutcome { request_id,profile_revision,status,advice:bounded_field("advice"),error })
            }).transpose()?;
            Ok(ConversationView{handoff_draft:store.conversation_handoff(id)?,service,id:id.into(),revision,selected_task_id,messages,candidates:decode(&candidates)?,review,task,busy,request_error,runtime_available,adviser_available:false,activity,adviser_outcome,notes_available,workspace_available:false,reminders_available:false,receipt:receipt.map(|r|decode(&r)).transpose()?})
        })
    }
    /// Durable dispatch precedes model execution. Identical retries do not launch again.
    pub fn conversation_begin(
        &mut self,
        request: &ConversationTurnRequest,
        session: &str,
        now: i64,
    ) -> Result<bool, StoreError> {
        self.conversation_begin_inner(request, session, now, None)
    }
    pub fn conversation_begin_profiled(
        &mut self,
        request: &ConversationTurnRequest,
        session: &str,
        now: i64,
        deployment: &crate::conversation_runtime::ConversationProfile,
        models: &[AgentModelOption],
        instructions: &str,
    ) -> Result<bool, StoreError> {
        self.conversation_begin_inner(
            request,
            session,
            now,
            Some((deployment, models, instructions)),
        )
    }
    fn conversation_begin_inner(
        &mut self,
        request: &ConversationTurnRequest,
        session: &str,
        now: i64,
        settings: Option<(
            &crate::conversation_runtime::ConversationProfile,
            &[AgentModelOption],
            &str,
        )>,
    ) -> Result<bool, StoreError> {
        bounded(&request.command_id, 128)?;
        bounded(&request.conversation_id, 128)?;
        bounded(&request.text, 8192)?;
        let raw = encode(request)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(stored) = tx
            .query_row(
                "SELECT payload_json FROM conversation_requests WHERE command_id=?1",
                [&request.command_id],
                |r| r.get::<_, String>(0),
            )
            .optional()?
        {
            if stored != raw {
                return Err(StoreError::Conflict(
                    "conversation command ID reused with changed payload".into(),
                ));
            }
            return Ok(false);
        }
        tx.execute(
            "INSERT OR IGNORE INTO conversations(id,updated_at) VALUES (?1,?2)",
            params![request.conversation_id, now],
        )?;
        let revision: i64 = tx.query_row(
            "SELECT revision FROM conversations WHERE id=?1",
            [&request.conversation_id],
            |r| r.get(0),
        )?;
        if revision != request.expected_revision {
            return Err(StoreError::Conflict(
                "conversation changed; refresh before sending".into(),
            ));
        }
        let busy:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM conversation_requests WHERE conversation_id=?1 AND status='running')",[&request.conversation_id],|r|r.get(0))?;
        if busy {
            return Err(StoreError::Conflict(
                "conversation request is already in progress".into(),
            ));
        }
        let accepted = if let Some((deployment, models, instructions)) = settings {
            crate::agent_settings::bootstrap(&tx, deployment, now)?;
            let profile = crate::agent_settings::active(&tx)?;
            crate::agent_settings::validate_profile(
                &profile.main,
                profile.adviser.as_ref(),
                deployment,
                models,
            )?;
            if request.consult_adviser && profile.adviser.is_none() {
                return Err(StoreError::Invalid(
                    "Configure the optional Astra adviser before requesting consultation".into(),
                ));
            }
            let runtime = crate::agent_settings::role_runtime(deployment, &profile.main);
            let adviser_runtime = profile
                .adviser
                .as_ref()
                .map(|adviser| crate::agent_settings::role_runtime(deployment, &adviser.role));
            let adviser_instructions = adviser_runtime
                .as_ref()
                .map(|_| crate::agent_settings::ADVISER_INSTRUCTIONS.to_owned());
            let deadline_unix =
                now + (runtime.timeout_seconds * u64::from(profile.main.max_model_calls)) as i64;
            Some(encode(&crate::agent_settings::AcceptedAgentProfile{profile,runtime,mandatory_instructions:instructions.into(),adviser_runtime,adviser_instructions,permissions:"read-only tool sandbox; tool network access denied; provider networking retained; no execution environments; proposal-only main tools; schema-only adviser with no tools or delegation; operator confirmation required".into(),deadline_unix})?)
        } else {
            None
        };
        tx.execute("INSERT INTO conversation_requests(command_id,conversation_id,payload_json,session_id,status,created_at) VALUES (?1,?2,?3,?4,'running',?5)",params![request.command_id,request.conversation_id,raw,session,now])?;
        tx.execute(
            "UPDATE conversation_requests SET accepted_profile_json=?2 WHERE command_id=?1",
            params![request.command_id, accepted],
        )?;
        tx.execute("INSERT INTO conversation_messages(conversation_id,request_id,role,text,created_at) VALUES (?1,?2,'user',?3,?4)",params![request.conversation_id,request.command_id,request.text,now])?;
        tx.execute("UPDATE conversations SET revision=revision+1,proposal_id=NULL,updated_at=?2 WHERE id=?1",params![request.conversation_id,now])?;
        event(&tx, &request.conversation_id, now)?;
        tx.commit()?;
        Ok(true)
    }
    pub fn conversation_interrupt(&mut self, session: &str, now: i64) -> Result<(), StoreError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let ids = {
            let mut s=tx.prepare("SELECT DISTINCT conversation_id FROM conversation_requests WHERE status='running' AND session_id<>?1")?;
            s.query_map([session], |r| r.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?
        };
        tx.execute("UPDATE conversation_invocations SET status='interrupted',outcome_json='{\"error\":\"Service restarted after dispatch; no automatic replay\"}' WHERE status='dispatched' AND request_id IN(SELECT command_id FROM conversation_requests WHERE status='running' AND session_id<>?1)",[session])?;
        tx.execute("UPDATE conversation_requests SET status='interrupted',error='Conversation interrupted by service restart. Saved drafts remain available; send a new message to continue. No activation was inferred.' WHERE status='running' AND session_id<>?1",[session])?;
        for id in ids {
            event(&tx, &id, now)?;
        }
        tx.commit()?;
        Ok(())
    }
    /// Record each bounded dispatch before leaving the database boundary. Recovery
    /// never automatically repeats it; the operator can start a fresh request.
    pub fn conversation_model_dispatch(
        &mut self,
        request: &ConversationTurnRequest,
        step: u8,
        now: i64,
    ) -> Result<(), StoreError> {
        self.conversation_dispatch(
            request,
            step,
            if step == 0 {
                InvocationPurpose::Main
            } else {
                InvocationPurpose::EmptyLookupContinuation
            },
            now,
        )
    }
    pub fn conversation_dispatch(
        &mut self,
        request: &ConversationTurnRequest,
        step: u8,
        purpose: InvocationPurpose,
        now: i64,
    ) -> Result<(), StoreError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let running: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM conversation_requests WHERE command_id=?1 AND conversation_id=?2 AND status='running')", params![request.command_id, request.conversation_id], |r|r.get(0))?;
        if !running {
            return Err(StoreError::Conflict(
                "conversation request is no longer running".into(),
            ));
        }
        let payload: String = tx.query_row(
            "SELECT payload_json FROM conversation_requests WHERE command_id=?1",
            [&request.command_id],
            |r| r.get(0),
        )?;
        if payload != encode(request)? {
            return Err(StoreError::Conflict(
                "Conversation dispatch must use its exact accepted request".into(),
            ));
        }
        let accepted: Option<String> = tx.query_row(
            "SELECT accepted_profile_json FROM conversation_requests WHERE command_id=?1",
            [&request.command_id],
            |r| r.get(0),
        )?;
        let profile = accepted
            .map(|raw| decode::<crate::agent_settings::AcceptedAgentProfile>(&raw))
            .transpose()?;
        let prior = {
            let mut statement = tx.prepare("SELECT ordinal,purpose,status,outcome_json FROM conversation_invocations WHERE request_id=?1 ORDER BY ordinal")?;
            statement
                .query_map([&request.command_id], |r| {
                    Ok((
                        r.get::<_, u8>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, Option<String>>(3)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?
        };
        if prior.len() != usize::from(step)
            || prior.iter().enumerate().any(|(ordinal, row)| {
                usize::from(row.0) != ordinal
                    || matches!(row.2.as_str(), "dispatched" | "interrupted")
            })
        {
            return Err(StoreError::Conflict(
                "Conversation dispatch is repeated, uncertain or out of sequence".into(),
            ));
        }
        let last = prior.last();
        let had_adviser = prior.iter().any(|r| r.1.starts_with("adviser_"));
        let had_lookup = prior.iter().any(|r| r.1 == "empty_lookup_continuation");
        let valid = match purpose {
            InvocationPurpose::Main => step == 0 && !request.consult_adviser,
            InvocationPurpose::AdviserManual => {
                step == 0
                    && request.consult_adviser
                    && profile
                        .as_ref()
                        .is_some_and(|p| p.profile.adviser.is_some())
            }
            InvocationPurpose::AdviserConflictingRequirements => {
                !had_adviser
                    && profile.as_ref().is_some_and(|p| {
                        p.profile
                            .adviser
                            .as_ref()
                            .is_some_and(|a| a.automatic_consultation)
                    })
                    && last.is_some_and(|r| {
                        r.2 == "completed"
                            && matches!(r.1.as_str(), "main" | "empty_lookup_continuation")
                            && r.3.as_ref().is_some_and(|raw| {
                                let offered = crate::conversation_tools::tools_with_adviser(
                                    false, false, true,
                                );
                                decode::<serde_json::Value>(raw).ok().is_some_and(|output| {
                                    matches!(
                                        crate::conversation_tools::operation_with_adviser(
                                            output,
                                            &offered,
                                            None,
                                            &[],
                                            &request.text
                                        ),
                                        Ok(ConversationOperation::Consult { .. })
                                    )
                                })
                            })
                    })
            }
            InvocationPurpose::AfterAdvice => last.is_some_and(|r| {
                r.1.starts_with("adviser_") && matches!(r.2.as_str(), "completed" | "failed")
            }),
            InvocationPurpose::EmptyLookupContinuation => {
                !had_lookup
                    && last.is_some_and(|r| {
                        r.2 == "completed"
                            && matches!(r.1.as_str(), "main" | "after_advice")
                            && r.3.as_ref().is_some_and(|raw| {
                                decode::<serde_json::Value>(raw)
                                    .ok()
                                    .is_some_and(|v| v["tool"] == "bokkie_lookup")
                            })
                    })
            }
        };
        if !valid || (profile.is_none() && step > 1) {
            return Err(StoreError::Invalid(
                "Conversation invocation purpose is unavailable or exhausted".into(),
            ));
        }
        if let Some(p) = &profile {
            if now >= p.deadline_unix {
                return Err(StoreError::Invalid(
                    "This request has reached its saved time limit".into(),
                ));
            }
            if step >= p.profile.main.max_model_calls || step >= 4 {
                return Err(StoreError::Invalid(
                    "This request has reached its saved execution limit".into(),
                ));
            }
            if purpose.is_adviser()
                && (step + 1 >= p.profile.main.max_model_calls
                    || p.deadline_unix - now <= p.runtime.timeout_seconds as i64)
            {
                return Err(StoreError::Invalid(
                    "This request has insufficient saved budget for Astra and Bokkie's return call"
                        .into(),
                ));
            }
        }
        tx.execute("INSERT INTO conversation_invocations(request_id,ordinal,purpose,profile_revision,status,dispatched_at) VALUES(?1,?2,?3,?4,'dispatched',?5)",params![request.command_id,step,purpose.as_str(),profile.map(|p|p.profile.revision),now]).map_err(|e| match e {rusqlite::Error::SqliteFailure(_,_)=>StoreError::Conflict("Conversation dispatch already recorded".into()),_=>e.into()})?;
        let details = encode(
            &json!({"request_id":request.command_id,"step":step,"purpose":purpose.as_str()}),
        )?;
        let duplicate: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM domain_events WHERE entity_kind='conversation' AND entity_id=?1 AND event_type='conversation_model_dispatch' AND details_json=?2)",params![request.conversation_id,details],|r|r.get(0))?;
        if duplicate {
            return Err(StoreError::Conflict(
                "conversation dispatch already recorded".into(),
            ));
        }
        tx.execute("INSERT INTO domain_events(entity_kind,entity_id,event_type,occurred_at,details_json) VALUES ('conversation',?1,'conversation_model_dispatch',?2,?3)",params![request.conversation_id,now,details])?;
        tx.commit()?;
        Ok(())
    }
    pub fn conversation_invocation_outcome(
        &mut self,
        request_id: &str,
        step: u8,
        result: Result<&serde_json::Value, &str>,
    ) -> Result<(), StoreError> {
        let (status, outcome) = match result {
            Ok(output) => ("completed", encode(output)?),
            Err(error) => (
                "failed",
                encode(&json!({"error":error.chars().take(2048).collect::<String>()}))?,
            ),
        };
        let changed=self.connection.execute("UPDATE conversation_invocations SET status=?3,outcome_json=?4 WHERE request_id=?1 AND ordinal=?2 AND status='dispatched'",params![request_id,step,status,outcome])?;
        if changed != 1 {
            return Err(StoreError::Conflict(
                "Invocation outcome was already recorded or interrupted".into(),
            ));
        }
        Ok(())
    }
    pub fn conversation_model_dispatch_count(&self) -> Result<i64, StoreError> {
        Ok(self.connection.query_row("SELECT COUNT(*) FROM domain_events WHERE entity_kind='conversation' AND event_type='conversation_model_dispatch'", [], |r|r.get(0))?)
    }
    pub fn conversation_record_output(
        &mut self,
        request_id: &str,
        output: &ConversationOperation,
    ) -> Result<(), StoreError> {
        let raw = encode(output)?;
        if raw.len() > 32768 {
            return Err(StoreError::Invalid("model output exceeds bound".into()));
        }
        self.connection.execute("UPDATE conversation_requests SET output_json=?2 WHERE command_id=?1 AND status='running'",params![request_id,raw])?;
        Ok(())
    }
    pub fn conversation_finish(
        &mut self,
        request: &ConversationTurnRequest,
        message: &str,
        error: Option<&str>,
        now: i64,
    ) -> Result<(), StoreError> {
        let message: String = message.chars().take(8192).collect();
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed=tx.execute("UPDATE conversation_requests SET status=?2,error=?3 WHERE command_id=?1 AND status='running'",params![request.command_id,if error.is_some(){"failed"}else{"complete"},error.map(|s|s.chars().take(2048).collect::<String>())])?;
        if changed > 0 {
            tx.execute("INSERT INTO conversation_messages(conversation_id,request_id,role,text,created_at) VALUES (?1,?2,'assistant',?3,?4)",params![request.conversation_id,request.command_id,message,now])?;
            tx.execute(
                "UPDATE conversations SET revision=revision+1,updated_at=?2 WHERE id=?1",
                params![request.conversation_id, now],
            )?;
            event(&tx, &request.conversation_id, now)?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn conversation_candidates(
        &mut self,
        id: &str,
        entries: &[ManagedCatalogueEntry],
        now: i64,
    ) -> Result<(), StoreError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "UPDATE conversations SET candidates_json=?2,updated_at=?3 WHERE id=?1",
            params![id, encode(&entries)?, now],
        )?;
        event(&tx, id, now)?;
        tx.commit()?;
        Ok(())
    }
    pub fn conversation_bind_saved(
        &mut self,
        id: &str,
        receipt: &ManagedTaskReceipt,
        now: i64,
    ) -> Result<(), StoreError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute("UPDATE conversations SET selected_task_id=?2,receipt_json=?3,revision=revision+1,updated_at=?4 WHERE id=?1",params![id,receipt.task_id,encode(receipt)?,now])?;
        event(&tx, id, now)?;
        tx.commit()?;
        Ok(())
    }
    pub fn conversation_select(
        &mut self,
        request: &ConversationSelectRequest,
        now: i64,
    ) -> Result<(), StoreError> {
        bounded(&request.command_id, 128)?;
        bounded(&request.conversation_id, 128)?;
        bounded(&request.task_id, 256)?;
        // An exact catalogue lookup validates the selected identity, including legacy kinds.
        let page = self.managed_catalogue(&request.task_id, None, 50)?;
        if !page.items.iter().any(|e| e.id == request.task_id) {
            return Err(StoreError::NotFound(request.task_id.clone()));
        }
        let raw = encode(request)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(old) = tx
            .query_row(
                "SELECT payload_json FROM conversation_commands WHERE command_id=?1",
                [&request.command_id],
                |r| r.get::<_, String>(0),
            )
            .optional()?
        {
            return if old == raw {
                Ok(())
            } else {
                Err(StoreError::Conflict("command ID reused".into()))
            };
        }
        tx.execute(
            "INSERT OR IGNORE INTO conversations(id,updated_at) VALUES (?1,?2)",
            params![request.conversation_id, now],
        )?;
        let changed=tx.execute("UPDATE conversations SET selected_task_id=?2,revision=revision+1,proposal_id=NULL,candidates_json='[]',receipt_json=NULL,updated_at=?4 WHERE id=?1 AND revision=?3 AND NOT EXISTS(SELECT 1 FROM conversation_requests WHERE conversation_id=?1 AND status='running')",params![request.conversation_id,request.task_id,request.expected_revision,now])?;
        if changed != 1 {
            return Err(StoreError::Conflict(
                "conversation changed or is busy".into(),
            ));
        }
        tx.execute(
            "INSERT INTO conversation_commands VALUES (?1,?2,'null')",
            params![request.command_id, raw],
        )?;
        tx.execute("INSERT INTO conversation_messages(conversation_id,request_id,role,text,created_at) VALUES (?1,?2,'system',?3,?4)",params![request.conversation_id,request.command_id,format!("Selected task {}",request.task_id),now])?;
        event(&tx, &request.conversation_id, now)?;
        tx.commit()?;
        Ok(())
    }
    pub fn conversation_review(
        &mut self,
        id: &str,
        review: &ConversationReview,
        profiles: &[ManagedCapabilityProfile],
        now: i64,
    ) -> Result<(), StoreError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT INTO conversation_proposals VALUES (?1,?2,?3,?4,?5)",
            params![review.id, id, encode(review)?, encode(&profiles)?, now],
        )?;
        tx.execute(
            "UPDATE conversations SET proposal_id=?2,updated_at=?3 WHERE id=?1",
            params![id, review.id, now],
        )?;
        event(&tx, id, now)?;
        tx.commit()?;
        Ok(())
    }
    pub fn conversation_confirmation(
        &self,
        request: &ConversationConfirmRequest,
        profiles: &[ManagedCapabilityProfile],
    ) -> Result<ConversationReview, StoreError> {
        bounded(&request.command_id, 128)?;
        let row:Option<(String,String)>=self.connection.query_row("SELECT p.review_json,p.profiles_json FROM conversation_proposals p JOIN conversations c ON c.id=p.conversation_id WHERE p.id=?1 AND c.id=?2 AND c.proposal_id=p.id AND c.selected_task_id=json_extract(p.review_json,'$.task_id') AND NOT EXISTS(SELECT 1 FROM conversation_requests r WHERE r.conversation_id=c.id AND r.status='running')",params![request.proposal_id,request.conversation_id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        let (raw, stored) = row.ok_or_else(|| {
            StoreError::Conflict("review is no longer current; request a fresh preview".into())
        })?;
        let review: ConversationReview = decode(&raw)?;
        if review.session_id != request.session_id || stored != encode(&profiles)? {
            return Err(StoreError::Conflict(
                "service session or capability profile changed; review again".into(),
            ));
        }
        if !review.blockers.is_empty() {
            return Err(StoreError::Conflict(
                "proposal has activation blockers".into(),
            ));
        }
        Ok(review)
    }
    /// Review validation, task mutation, both receipts and invalidation commit together.
    pub fn conversation_confirm(
        &mut self,
        request: &ConversationConfirmRequest,
        profiles: &[ManagedCapabilityProfile],
        now: i64,
    ) -> Result<(), StoreError> {
        let tx =
            rusqlite::Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        if self.conversation_confirmed_receipt(request)?.is_some() {
            return Ok(());
        }
        let review = self.conversation_confirmation(request, profiles)?;
        let receipt = match review.action {
            ConversationAction::Activate => crate::store::managed::activate_in_transaction(
                &tx,
                &request.command_id,
                &review.preview.ok_or_else(|| {
                    StoreError::Invalid("activation requires exact preview".into())
                })?,
                &request.session_id,
                profiles,
                now,
            )?,
            ConversationAction::Pause => crate::store::managed::pause_in_transaction(
                &tx,
                &request.command_id,
                &review.task_id,
                review.configuration_revision,
                now,
            )?,
            ConversationAction::Resume => crate::store::managed::resume_in_transaction(
                &tx,
                &request.command_id,
                &review.task_id,
                review.configuration_revision,
                profiles,
                now,
            )?,
        };
        tx.execute(
            "INSERT INTO conversation_commands VALUES (?1,?2,?3)",
            params![request.command_id, encode(request)?, encode(&receipt)?],
        )?;
        tx.execute("UPDATE conversations SET receipt_json=?2,revision=revision+1,updated_at=?3 WHERE id=?1",params![request.conversation_id,encode(&receipt)?,now])?;
        tx.execute("INSERT INTO conversation_messages(conversation_id,request_id,role,text,created_at) VALUES (?1,?2,'system',?3,?4)",params![request.conversation_id,request.command_id,format!("Confirmed change saved: task {}, configuration revision {}. This receipt is not a completed occurrence.",receipt.task_id,receipt.configuration_revision),now])?;
        event(&tx, &request.conversation_id, now)?;
        tx.commit()?;
        Ok(())
    }
    pub fn conversation_confirmed_receipt(
        &self,
        request: &ConversationConfirmRequest,
    ) -> Result<Option<ManagedTaskReceipt>, StoreError> {
        let row: Option<(String, String)> = self
            .connection
            .query_row(
                "SELECT payload_json,result_json FROM conversation_commands WHERE command_id=?1",
                [&request.command_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        row.map(|(payload, result)| {
            if payload != encode(request)? {
                return Err(StoreError::Conflict(
                    "confirmation command ID reused with changed payload".into(),
                ));
            }
            decode(&result)
        })
        .transpose()
    }
}
