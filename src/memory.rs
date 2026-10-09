//! Bounded, sourced recall. Task history and runtime authority remain separate.
use crate::{
    Store, StoreError,
    conversation::{decode, encode},
};
use bokkie_operator_api::*;
use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};

const COLUMNS: &str =
    "entry_id,revision,kind,provenance,content,sources_json,task_id,created_at,updated_at,removed";
const PAGE_SIZE: usize = 40;

fn invalid(message: &str) -> StoreError {
    StoreError::Invalid(message.into())
}
fn bounded(value: &str, max: usize) -> bool {
    !value.trim().is_empty()
        && value.len() <= max
        && !value
            .chars()
            .any(|c| c.is_control() && c != '\n' && c != '\t')
}
fn kind_name(kind: MemoryKind) -> &'static str {
    match kind {
        MemoryKind::Preference => "preference",
        MemoryKind::TaskOutcome => "task_outcome",
        MemoryKind::Decision => "decision",
        MemoryKind::OperationalKnowledge => "operational_knowledge",
    }
}
fn provenance_name(provenance: MemoryProvenance) -> &'static str {
    match provenance {
        MemoryProvenance::Explicit => "explicit",
        MemoryProvenance::Inferred => "inferred",
    }
}
fn row_entry(row: &Row<'_>) -> rusqlite::Result<MemoryEntry> {
    let parse = |index: usize| -> rusqlite::Result<serde_json::Value> {
        let value: String = row.get(index)?;
        Ok(serde_json::Value::String(value))
    };
    let conversion = |e: serde_json::Error| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    };
    let revision: i64 = row.get(1)?;
    let sources: String = row.get(5)?;
    Ok(MemoryEntry {
        id: row.get(0)?,
        revision,
        kind: serde_json::from_value(parse(2)?).map_err(conversion)?,
        provenance: serde_json::from_value(parse(3)?).map_err(conversion)?,
        content: row.get(4)?,
        sources: serde_json::from_str(&sources).map_err(conversion)?,
        task_id: row.get(6)?,
        corrected: revision > 1 && !row.get::<_, bool>(9)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
        removed: row.get(9)?,
    })
}
fn entry(conn: &Connection, id: &str) -> Result<MemoryEntry, StoreError> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM memory_entries WHERE entry_id=?1"),
        [id],
        row_entry,
    )
    .optional()?
    .ok_or_else(|| StoreError::NotFound(id.into()))
}

impl Store {
    pub fn memory_list(
        &self,
        after: Option<&str>,
        service: ServiceIdentity,
    ) -> Result<MemoryList, StoreError> {
        if after.is_some_and(|s| !bounded(s, 200)) {
            return Err(invalid("Memory page cursor is invalid"));
        }
        let mut statement = self.connection.prepare(&format!("SELECT {COLUMNS} FROM memory_entries WHERE removed=0 AND (?1 IS NULL OR entry_id>?1) ORDER BY entry_id LIMIT ?2"))?;
        let mut entries = statement
            .query_map(params![after, PAGE_SIZE + 1], row_entry)?
            .collect::<Result<Vec<_>, _>>()?;
        let next_after = if entries.len() > PAGE_SIZE {
            entries.truncate(PAGE_SIZE);
            entries.last().map(|e| e.id.clone())
        } else {
            None
        };
        Ok(MemoryList {
            service,
            entries,
            next_after,
        })
    }

    pub fn memory_command(
        &mut self,
        request: &MemoryCommandRequest,
        now: i64,
    ) -> Result<MemoryEntry, StoreError> {
        if !bounded(&request.command_id, 128)
            || request.entry_id.as_ref().is_some_and(|s| !bounded(s, 200))
        {
            return Err(invalid("Memory command identity is invalid"));
        }
        let payload = encode(request)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some((saved, result)) = tx
            .query_row(
                "SELECT payload_json,result_json FROM memory_commands WHERE command_id=?1",
                [&request.command_id],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .optional()?
        {
            if saved != payload {
                return Err(StoreError::Conflict(
                    "Memory command reused with changed values".into(),
                ));
            }
            return decode(&result);
        }
        let id = match &request.mutation {
            MemoryMutation::Create {
                kind,
                provenance,
                content,
                sources,
                task_id,
            } => {
                if request.entry_id.is_some() || request.expected_revision != 0 {
                    return Err(invalid(
                        "New memory requires no entry identity and revision zero",
                    ));
                }
                if !bounded(content, 2048)
                    || sources.is_empty()
                    || sources.len() > 4
                    || sources
                        .iter()
                        .any(|s| !bounded(&s.reference, 256) || !bounded(&s.context, 512))
                    || task_id.as_ref().is_some_and(|s| !bounded(s, 200))
                {
                    return Err(invalid(
                        "Memory needs content within 2 KiB and 1..=4 bounded source references and contexts",
                    ));
                }
                if let Some(task_id) = task_id {
                    let exists: bool = tx.query_row(
                        "SELECT EXISTS(SELECT 1 FROM managed_tasks WHERE id=?1)",
                        [task_id],
                        |r| r.get(0),
                    )?;
                    if !exists {
                        return Err(invalid(
                            "Memory task context must name an existing managed task",
                        ));
                    }
                }
                let count: i64 = tx.query_row(
                    "SELECT count(*) FROM memory_entries WHERE removed=0 AND source_key IS NULL",
                    [],
                    |r| r.get(0),
                )?;
                if count >= 100 {
                    return Err(invalid(
                        "This modest memory store supports 100 active manually saved entries. Remove an unused entry first",
                    ));
                }
                let id = format!("manual:{}", request.command_id);
                tx.execute("INSERT INTO memory_entries(entry_id,revision,kind,provenance,content,sources_json,task_id,created_at,updated_at) VALUES(?1,1,?2,?3,?4,?5,?6,?7,?7)", params![id, kind_name(*kind), provenance_name(*provenance), content, encode(sources)?, task_id, now])?;
                id
            }
            MemoryMutation::Correct { .. } | MemoryMutation::Remove => {
                let id = request
                    .entry_id
                    .as_deref()
                    .ok_or_else(|| invalid("Choose the memory entry to change"))?;
                let previous = entry(&tx, id)?;
                if previous.removed || previous.revision != request.expected_revision {
                    return Err(StoreError::Conflict("Memory changed elsewhere. Reload and inspect its current revision before changing it".into()));
                }
                let content = match &request.mutation {
                    MemoryMutation::Correct { content } => {
                        if !bounded(content, 2048) {
                            return Err(invalid(
                                "Memory content must fit within 2 KiB and contain no control characters",
                            ));
                        }
                        Some(content.as_str())
                    }
                    _ => None,
                };
                tx.execute("UPDATE memory_entries SET revision=revision+1,content=?2,removed=?3,updated_at=?4 WHERE entry_id=?1 AND revision=?5", params![id, content, content.is_none(), now, request.expected_revision])?;
                id.to_owned()
            }
        };
        let result = entry(&tx, &id)?;
        tx.execute(
            "INSERT INTO memory_commands VALUES(?1,?2,?3)",
            params![request.command_id, payload, encode(&result)?],
        )?;
        // Audit carries identity/revision only; corrections do not duplicate recalled text.
        tx.execute("INSERT INTO domain_events(entity_kind,entity_id,event_type,occurred_at,details_json) VALUES('memory',?1,?2,?3,?4)", params![id, if result.removed { "memory_removed" } else { "memory_saved" }, now, encode(&serde_json::json!({"revision":result.revision}))?])?;
        tx.commit()?;
        Ok(result)
    }

    /// Recall an accepted outcome only when its task is selected. The durable
    /// source key also survives corrections/removal and suppresses regeneration.
    fn memory_capture_task(&mut self, task_id: &str, now: i64) -> Result<(), StoreError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let rows = {
            let mut statement = tx.prepare("SELECT e.execution_id,r.result,r.created_at FROM workspace_executions e JOIN managed_bindings b ON b.obligation_id=e.obligation_id JOIN managed_results r ON r.obligation_id=e.obligation_id WHERE b.task_id=?1 AND e.status='completed' ORDER BY r.created_at DESC,e.execution_id DESC LIMIT 3")?;
            statement
                .query_map([task_id], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?
        };
        for (execution, summary, accepted_at) in rows {
            let content = truncate_bytes(&summary, 1536);
            if content.trim().is_empty() {
                continue;
            }
            let source = MemorySource {
                reference: format!("workspace_execution:{execution}"),
                context: format!(
                    "Accepted outcome for task {task_id} at Unix time {accepted_at}. Recall excerpt; inspect the task's authoritative result and delivery evidence for complete detail."
                ),
            };
            tx.execute("INSERT OR IGNORE INTO memory_entries(entry_id,revision,kind,provenance,content,sources_json,task_id,source_key,created_at,updated_at) VALUES(?1,1,'task_outcome','inferred',?2,?3,?4,?5,?6,?6)", params![format!("workspace:{execution}"), content, encode(&vec![source])?, task_id, format!("workspace_execution:{execution}"), now])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Relevance is deliberately simple: preferences, selected-task context and
    /// up to four identifying words. No model call or embedding infrastructure.
    pub fn memory_context(
        &mut self,
        task_id: Option<&str>,
        query: &str,
        byte_limit: usize,
        now: i64,
    ) -> Result<Vec<MemoryEntry>, StoreError> {
        if let Some(task) = task_id {
            self.memory_capture_task(task, now)?;
        }
        let mut words = query
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| w.len() >= 3)
            .take(4)
            .map(str::to_lowercase)
            .collect::<Vec<_>>();
        words.resize(4, String::new());
        let mut statement = self.connection.prepare(&format!("SELECT {COLUMNS} FROM memory_entries WHERE removed=0 AND (kind='preference' OR task_id=?1 OR (?2!='' AND instr(lower(content),?2)>0) OR (?3!='' AND instr(lower(content),?3)>0) OR (?4!='' AND instr(lower(content),?4)>0) OR (?5!='' AND instr(lower(content),?5)>0)) ORDER BY CASE WHEN task_id=?1 THEN 0 WHEN kind='preference' THEN 1 ELSE 2 END,updated_at DESC,entry_id LIMIT 32"))?;
        let candidates = statement
            .query_map(
                params![task_id, words[0], words[1], words[2], words[3]],
                row_entry,
            )?
            .collect::<Result<Vec<_>, _>>()?;
        let mut selected = Vec::new();
        for candidate in candidates {
            if selected.len() >= 6 {
                break;
            }
            selected.push(candidate);
            if encode(&selected)?.len() > byte_limit.min(4096) {
                selected.pop();
            }
        }
        Ok(selected)
    }
}

fn truncate_bytes(value: &str, limit: usize) -> String {
    let mut end = value.len().min(limit);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

#[cfg(test)]
mod tests;
