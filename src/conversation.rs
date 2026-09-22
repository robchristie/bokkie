//! Durable drafting and operator reviews. Models never receive mutation authority.
use crate::{Store, StoreError};
use bokkie_operator_api::*;
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

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
    Discuss {
        message: String,
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
            Ok(ConversationView{service,id:id.into(),revision,selected_task_id,messages,candidates:decode(&candidates)?,review,task,busy,request_error,runtime_available,notes_available,receipt:receipt.map(|r|decode(&r)).transpose()?})
        })
    }
    /// Durable dispatch precedes model execution. Identical retries do not launch again.
    pub fn conversation_begin(
        &mut self,
        request: &ConversationTurnRequest,
        session: &str,
        now: i64,
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
        tx.execute("INSERT INTO conversation_requests(command_id,conversation_id,payload_json,session_id,status,created_at) VALUES (?1,?2,?3,?4,'running',?5)",params![request.command_id,request.conversation_id,raw,session,now])?;
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
        if step > 1 {
            return Err(StoreError::Invalid(
                "conversation continuation bound exceeded".into(),
            ));
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let running: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM conversation_requests WHERE command_id=?1 AND conversation_id=?2 AND status='running')", params![request.command_id, request.conversation_id], |r|r.get(0))?;
        if !running {
            return Err(StoreError::Conflict(
                "conversation request is no longer running".into(),
            ));
        }
        let details = encode(&json!({"request_id":request.command_id,"step":step}))?;
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

/// A deliberately small model output grammar. No actor, SQL, shell, credentials or confirmation tool.
pub fn operation_schema() -> Value {
    let text = json!({"type":"string"});
    let object = |properties: Value, required: Vec<&str>| json!({"type":"object","properties":properties,"required":required,"additionalProperties":false});
    let trigger = json!({"anyOf":[object(json!({"kind":{"type":"string","const":"immediate"}}),vec!["kind"]),object(json!({"kind":{"type":"string","const":"once"},"local_datetime":text,"timezone":text}),vec!["kind","local_datetime","timezone"]),object(json!({"kind":{"type":"string","const":"recurring"},"cron":text,"timezone":text}),vec!["kind","cron","timezone"])]});
    let definition = object(
        json!({"name":text,"purpose":text,"instructions":text,"context_refs":{"type":"array","items":text},"trigger":trigger,"capability":text,"profile_revision":text,"effects":{"type":"array","items":text},"max_attempts":{"type":"integer"},"max_output_chars":{"type":"integer"},"destination":text}),
        vec![
            "name",
            "purpose",
            "instructions",
            "context_refs",
            "trigger",
            "capability",
            "profile_revision",
            "effects",
            "max_attempts",
            "max_output_chars",
            "destination",
        ],
    );
    let operation = json!({"anyOf":[object(json!({"operation":{"type":"string","const":"discuss"},"message":text}),vec!["operation","message"]),object(json!({"operation":{"type":"string","const":"lookup"},"query":text}),vec!["operation","query"]),object(json!({"operation":{"type":"string","const":"save_definition"},"definition":definition,"message":text}),vec!["operation","definition","message"]),object(json!({"operation":{"type":"string","const":"preview"}}),vec!["operation"]),object(json!({"operation":{"type":"string","const":"propose"},"action":{"type":"string","enum":["activate","pause","resume"]}}),vec!["operation","action"])]});
    object(json!({"proposal":operation}), vec!["proposal"])
}

/// Advertise only operations legal for the current trusted selection. Validation
/// still fences every proposal after the model returns.
pub fn operation_schema_for(managed_selected: bool, legacy_selected: bool) -> Value {
    let mut schema = operation_schema();
    if let Some(operations) = schema
        .pointer_mut("/properties/proposal/anyOf")
        .and_then(Value::as_array_mut)
    {
        operations.retain(|operation| {
            match operation
                .pointer("/properties/operation/const")
                .and_then(Value::as_str)
            {
                Some("preview" | "propose") => managed_selected,
                Some("save_definition") => !legacy_selected,
                _ => true,
            }
        });
    }
    schema
}
