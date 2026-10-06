//! Hand-offs prepare instructions. They never admit obligations or dispatch workers.
use crate::{
    Store, StoreError,
    conversation::{decode, encode},
};
use bokkie_operator_api::*;
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Serialize, de::DeserializeOwned};
use uuid::Uuid;

fn text(value: &str, name: &str, max: usize, required: bool) -> Result<(), StoreError> {
    if (required && value.trim().is_empty())
        || value.len() > max
        || value
            .chars()
            .any(|c| c == '\0' || (c.is_control() && c != '\n' && c != '\t'))
    {
        return Err(StoreError::Invalid(format!(
            "{name} must contain {}..={max} bytes without control characters",
            if required { 1 } else { 0 }
        )));
    }
    Ok(())
}
fn identity(value: &str, name: &str) -> Result<(), StoreError> {
    if Uuid::parse_str(value)
        .ok()
        .is_none_or(|id| id.is_nil() || id.to_string() != value)
    {
        return Err(StoreError::Invalid(format!(
            "{name} must be a canonical non-nil UUID"
        )));
    }
    Ok(())
}
pub fn validate_registration(value: &ProjectRegistration) -> Result<(), StoreError> {
    text(&value.name, "Project name", 160, true)?;
    text(&value.host, "Workspace host", 160, true)?;
    text(&value.workspace, "Workspace path", 1024, true)?;
    text(&value.context, "Project context", 1024, false)?;
    let path = &value.workspace;
    let windows = path.as_bytes().get(0..3).is_some_and(|p| {
        p[0].is_ascii_alphabetic() && p[1] == b':' && (p[2] == b'\\' || p[2] == b'/')
    });
    if !(path.starts_with('/') || windows)
        || path.contains("://")
        || path.chars().any(char::is_control)
        || path.split(['/', '\\']).any(|p| p == "..")
    {
        return Err(StoreError::Invalid("Workspace must be an absolute host path, not a browser URL, command or relative path. It is an address only; Bokkie does not inspect it".into()));
    }
    match (&value.codex_project_id, &value.codex_host_id) {
        (Some(project), Some(host)) => {
            identity(project, "Codex project identity")?;
            text(host, "Codex host identity", 256, true)?;
            if host.chars().any(char::is_whitespace) || host.contains("://") {
                return Err(StoreError::Invalid("Codex host identity must be an identifier, not a URL".into()));
            }
        }
        (None, None) => {},
        _ => return Err(StoreError::Invalid("Supply both Codex project and host identities, or leave both empty for manual identification".into())),
    }
    Ok(())
}
pub fn validate_brief(brief: &HandoffBrief) -> Result<(), StoreError> {
    text(&brief.outcome, "Requested outcome", 2048, true)?;
    text(&brief.context, "Relevant context", 4096, false)?;
    text(&brief.constraints, "Constraints and scope", 4096, true)?;
    text(
        &brief.acceptance,
        "Checkable acceptance criteria",
        4096,
        true,
    )?;
    if brief.references.len() > 12 {
        return Err(StoreError::Invalid(
            "Use at most 12 relevant reference links".into(),
        ));
    }
    for link in &brief.references {
        text(link, "Reference link", 1024, true)?;
        let uri: axum::http::Uri = link.parse().map_err(|_| {
            StoreError::Invalid("Reference links must be HTTP or HTTPS URLs".into())
        })?;
        if !matches!(uri.scheme_str(), Some("https" | "http"))
            || uri.host().is_none()
            || uri.authority().is_some_and(|a| a.as_str().contains('@'))
            || link.chars().any(char::is_whitespace)
        {
            return Err(StoreError::Invalid("Reference links must be HTTP or HTTPS URLs without credentials; links are data and are never fetched".into()));
        }
    }
    Ok(())
}
fn replay<T: DeserializeOwned>(
    tx: &Transaction<'_>,
    command: &str,
    payload: &str,
) -> Result<Option<T>, StoreError> {
    text(command, "Command identity", 128, true)?;
    let row: Option<(String, String)> = tx
        .query_row(
            "SELECT payload_json,result_json FROM workspace_handoff_commands WHERE command_id=?1",
            [command],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    match row {
        Some((old, result)) if old == payload => Ok(Some(decode(&result)?)),
        Some(_) => Err(StoreError::Conflict(
            "Hand-off command identity reused with changed payload".into(),
        )),
        None => Ok(None),
    }
}
fn receipt(
    tx: &Transaction<'_>,
    command: &str,
    payload: &str,
    result: &impl Serialize,
) -> Result<(), StoreError> {
    tx.execute("INSERT INTO workspace_handoff_commands(command_id,payload_json,result_json) VALUES (?1,?2,?3)",params![command,payload,encode(result)?])?;
    Ok(())
}
fn event(tx: &Transaction<'_>, kind: &str, id: &str, now: i64) -> Result<(), StoreError> {
    tx.execute("INSERT INTO domain_events(entity_kind,entity_id,event_type,occurred_at,details_json) VALUES (?1,?2,'workspace_handoff_changed',?3,'{}')",params![kind,id,now])?;
    Ok(())
}
fn project(connection: &Connection, id: &str) -> Result<ProjectDestination, StoreError> {
    let row: Option<(i64, String)> = connection
        .query_row(
            "SELECT revision,registration_json FROM workspace_projects WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let (revision, raw) = row.ok_or_else(|| StoreError::NotFound(format!("project {id}")))?;
    Ok(ProjectDestination {
        id: id.into(),
        revision,
        registration: decode(&raw)?,
    })
}
fn latest(connection: &Connection, id: &str) -> Result<i64, StoreError> {
    Ok(connection.query_row(
        "SELECT COALESCE(MAX(revision),0) FROM workspace_handoff_snapshots WHERE handoff_id=?1",
        [id],
        |r| r.get(0),
    )?)
}
fn snapshot(
    connection: &Connection,
    id: &str,
    revision: i64,
) -> Result<HandoffSnapshot, StoreError> {
    let raw: Option<String> = connection.query_row("SELECT snapshot_json FROM workspace_handoff_snapshots WHERE handoff_id=?1 AND revision=?2",params![id,revision],|r|r.get(0)).optional()?;
    decode(&raw.ok_or_else(|| StoreError::NotFound(format!("hand-off {id} revision {revision}")))?)
}
fn complete_brief(project: &ProjectDestination, brief: &HandoffBrief, return_url: &str) -> String {
    let p = &project.registration;
    format!(
        "# Development hand-off: {}\n\nWorkspace: {} on {}\n{}\n\n## Requested outcome\n{}\n\n## Relevant decisions and context\n{}\n\n## Constraints and scope\n{}\n\n## Checkable acceptance criteria\n{}\n\n## References\n{}\n\n## Receiving workspace\nRead this workspace's AGENTS.md and relevant maintained guidance, then use its established development workflow. This brief does not grant new permissions or authorise deployment or publication. Resolve missing authority in the receiving workspace.\n\nBokkie has prepared and saved this brief. Execution has not been acknowledged.\n\nReturn to this exact saved hand-off: {}\n",
        p.name,
        p.workspace,
        p.host,
        p.codex_project_id
            .as_ref()
            .map(|id| format!(
                "Codex project identity: {id}; host identity: {}",
                p.codex_host_id.as_deref().unwrap_or("")
            ))
            .unwrap_or_default(),
        brief.outcome,
        if brief.context.is_empty() {
            "No additional context supplied"
        } else {
            &brief.context
        },
        brief.constraints,
        brief.acceptance,
        if brief.references.is_empty() {
            "No reference links supplied".into()
        } else {
            brief.references.join("\n")
        },
        return_url
    )
}

impl Store {
    pub fn workspace_projects(&self) -> Result<Vec<ProjectDestination>, StoreError> {
        let mut q = self.connection.prepare(
            "SELECT id,revision,registration_json FROM workspace_projects ORDER BY id LIMIT 100",
        )?;
        let rows = q
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|(id, revision, raw)| {
                Ok(ProjectDestination {
                    id,
                    revision,
                    registration: decode(&raw)?,
                })
            })
            .collect()
    }
    pub fn workspace_project_save(
        &mut self,
        request: &ProjectSaveRequest,
        now: i64,
    ) -> Result<ProjectDestination, StoreError> {
        identity(&request.project_id, "Project identity")?;
        validate_registration(&request.registration)?;
        let raw = encode(request)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(result) = replay(&tx, &request.command_id, &raw)? {
            return Ok(result);
        }
        let current = tx
            .query_row(
                "SELECT revision FROM workspace_projects WHERE id=?1",
                [&request.project_id],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
            .unwrap_or(0);
        if current != request.expected_revision {
            return Err(StoreError::Conflict(
                "Project registration changed; reload and review the destination".into(),
            ));
        }
        if current == 0
            && tx.query_row("SELECT COUNT(*) FROM workspace_projects", [], |r| {
                r.get::<_, i64>(0)
            })? >= 100
        {
            return Err(StoreError::Invalid(
                "The address book supports at most 100 projects".into(),
            ));
        }
        let result = ProjectDestination {
            id: request.project_id.clone(),
            revision: current + 1,
            registration: request.registration.clone(),
        };
        tx.execute("INSERT INTO workspace_projects(id,revision,registration_json) VALUES (?1,?2,?3) ON CONFLICT(id) DO UPDATE SET revision=excluded.revision,registration_json=excluded.registration_json",params![result.id,result.revision,encode(&result.registration)?])?;
        receipt(&tx, &request.command_id, &raw, &result)?;
        event(&tx, "workspace_project", &result.id, now)?;
        tx.commit()?;
        Ok(result)
    }
    pub fn handoff_prepare(
        &mut self,
        request: &ConversationTurnRequest,
        project_query: &str,
        brief: &HandoffBrief,
        now: i64,
    ) -> Result<(), StoreError> {
        text(project_query, "Project query", 160, true)?;
        validate_brief(brief)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let prior: Option<(String,String)> = tx.query_row("SELECT project_query,brief_json FROM workspace_handoff_drafts WHERE source_request_id=?1",[&request.command_id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        let raw = encode(brief)?;
        if let Some((query, old)) = prior {
            if query != project_query || old != raw {
                return Err(StoreError::Conflict(
                    "Accepted request already prepared a different hand-off".into(),
                ));
            }
            return Ok(());
        }
        let running: bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM conversation_requests WHERE command_id=?1 AND conversation_id=?2 AND status='running')",params![request.command_id,request.conversation_id],|r|r.get(0))?;
        if !running {
            return Err(StoreError::Conflict(
                "Hand-off drafting requires its accepted running conversation request".into(),
            ));
        }
        let id = Uuid::new_v4().to_string();
        tx.execute("INSERT INTO workspace_handoff_drafts(id,conversation_id,source_request_id,project_query,brief_json) VALUES (?1,?2,?3,?4,?5)",params![id,request.conversation_id,request.command_id,project_query,raw])?;
        tx.execute("UPDATE conversations SET handoff_draft_id=?2,revision=revision+1,updated_at=?3 WHERE id=?1",params![request.conversation_id,id,now])?;
        event(&tx, "conversation", &request.conversation_id, now)?;
        tx.commit()?;
        Ok(())
    }
    pub fn conversation_handoff(
        &self,
        conversation: &str,
    ) -> Result<Option<HandoffDraft>, StoreError> {
        let row:Option<(String,String,String,String)>=self.connection.query_row("SELECT d.id,d.source_request_id,d.project_query,d.brief_json FROM conversations c JOIN workspace_handoff_drafts d ON d.id=c.handoff_draft_id WHERE c.id=?1",[conversation],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
        row.map(|(id, source_request_id, project_query, raw)| {
            let saved_revision = latest(&self.connection, &id)?;
            let brief = if saved_revision > 0 {
                snapshot(&self.connection, &id, saved_revision)?.brief
            } else {
                decode(&raw)?
            };
            let query = project_query.to_lowercase();
            let candidates = self
                .workspace_projects()?
                .into_iter()
                .filter(|p| p.id == query || p.registration.name.to_lowercase().contains(&query))
                .collect();
            Ok(HandoffDraft {
                saved_revision,
                id,
                conversation_id: conversation.into(),
                source_request_id,
                project_query,
                brief,
                candidates,
            })
        })
        .transpose()
    }
    pub fn handoff_save(
        &mut self,
        request: &HandoffSaveRequest,
        origin: &str,
        now: i64,
    ) -> Result<HandoffSnapshot, StoreError> {
        identity(&request.draft_id, "Hand-off identity")?;
        identity(&request.project_id, "Project identity")?;
        validate_brief(&request.brief)?;
        let raw = encode(request)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(result) = replay(&tx, &request.command_id, &raw)? {
            return Ok(result);
        }
        let draft: Option<(String,String)>=tx.query_row("SELECT conversation_id,source_request_id FROM workspace_handoff_drafts WHERE id=?1",[&request.draft_id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        let (conversation_id, source_request_id) =
            draft.ok_or_else(|| StoreError::NotFound(request.draft_id.clone()))?;
        let destination = project(&tx, &request.project_id)?;
        if destination.revision != request.project_revision {
            return Err(StoreError::Conflict(
                "Selected workspace registration changed; reload and select it again".into(),
            ));
        }
        validate_registration(&destination.registration)?;
        let last = latest(&tx, &request.draft_id)?;
        // Repeated Save clicks with fresh command IDs do not duplicate identical snapshots.
        if last > 0 {
            let old = snapshot(&tx, &request.draft_id, last)?;
            if old.brief == request.brief && old.project == destination {
                receipt(&tx, &request.command_id, &raw, &old)?;
                tx.commit()?;
                return Ok(old);
            }
        }
        if last != request.expected_revision {
            return Err(StoreError::Conflict(
                "Hand-off changed; read the latest saved revision before saving edits".into(),
            ));
        }
        let revision = last + 1;
        let return_path = format!("/ui/?handoff={}&revision={revision}", request.draft_id);
        let result = HandoffSnapshot {
            id: request.draft_id.clone(),
            revision,
            conversation_id: conversation_id.clone(),
            source_request_id,
            project: destination.clone(),
            brief: request.brief.clone(),
            created_at: now,
            complete_brief: complete_brief(
                &destination,
                &request.brief,
                &format!("{origin}{return_path}"),
            ),
            return_path,
        };
        tx.execute("INSERT INTO workspace_handoff_snapshots(handoff_id,revision,snapshot_json) VALUES (?1,?2,?3)",params![result.id,revision,encode(&result)?])?;
        receipt(&tx, &request.command_id, &raw, &result)?;
        event(&tx, "conversation", &conversation_id, now)?;
        tx.commit()?;
        Ok(result)
    }
    pub fn handoff_view(
        &self,
        id: &str,
        revision: Option<i64>,
        service: ServiceIdentity,
    ) -> Result<HandoffView, StoreError> {
        identity(id, "Hand-off identity")?;
        self.with_deferred_read(|store| {
            let latest_revision=latest(&store.connection,id)?;
            let revision=revision.unwrap_or(latest_revision);
            let snapshot=snapshot(&store.connection,id,revision)?;
            let mut q=store.connection.prepare("SELECT activity_json FROM workspace_handoff_activities WHERE handoff_id=?1 AND revision=?2 ORDER BY sequence LIMIT 200")?;
            let rows=q.query_map(params![id,revision],|r|r.get::<_,String>(0))?.collect::<Result<Vec<_>,_>>()?;
            let activities=rows.iter().map(|s|decode(s)).collect::<Result<Vec<_>,_>>()?;
            Ok(HandoffView{service,snapshot,activities,previous_revision:if revision>1{Some(revision-1)}else{None},latest_revision})
        })
    }
    pub fn handoff_list(
        &self,
        after: Option<&str>,
        service: ServiceIdentity,
    ) -> Result<HandoffList, StoreError> {
        if let Some(after) = after {
            identity(after, "Hand-off cursor")?;
        }
        self.with_deferred_read(|store| {
            let mut q=store.connection.prepare("SELECT s.snapshot_json FROM workspace_handoff_snapshots s WHERE s.revision=(SELECT MAX(x.revision) FROM workspace_handoff_snapshots x WHERE x.handoff_id=s.handoff_id) AND (?1 IS NULL OR s.handoff_id<?1) ORDER BY s.handoff_id DESC LIMIT 21")?;
            let rows=q.query_map([after],|r|r.get::<_,String>(0))?.collect::<Result<Vec<_>,_>>()?;
            let mut items=rows.iter().map(|raw|decode::<HandoffSnapshot>(raw)).collect::<Result<Vec<_>,_>>()?;
            let next_after=if items.len()>20 {items.truncate(20);items.last().map(|s|s.id.clone())}else{None};
            Ok(HandoffList{service,items,next_after})
        })
    }
    pub fn handoff_activity(
        &mut self,
        request: &HandoffActivityRequest,
        now: i64,
    ) -> Result<(), StoreError> {
        identity(&request.handoff_id, "Hand-off identity")?;
        text(
            &request.note,
            "Result or action note",
            4096,
            matches!(
                request.kind,
                HandoffActivityKind::ResultNote | HandoffActivityKind::OpeningProblemReported
            ),
        )?;
        let raw = encode(request)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if replay::<()>(&tx, &request.command_id, &raw)?.is_some() {
            return Ok(());
        }
        let saved = snapshot(&tx, &request.handoff_id, request.revision)?;
        let count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM workspace_handoff_activities WHERE handoff_id=?1 AND revision=?2",
            params![request.handoff_id, request.revision],
            |r| r.get(0),
        )?;
        if count >= 200 {
            return Err(StoreError::Invalid(
                "This revision has reached its 200-note/action limit".into(),
            ));
        }
        let provenance = match request.kind {
            HandoffActivityKind::ResultNote | HandoffActivityKind::OpeningProblemReported => {
                "Operator-entered report; not independently verified"
            }
            _ => "Client-reported action; no execution acknowledgement",
        };
        let activity = HandoffActivity {
            kind: request.kind,
            note: request.note.clone(),
            provenance: provenance.into(),
            created_at: now,
        };
        tx.execute("INSERT INTO workspace_handoff_activities(handoff_id,revision,activity_json) VALUES (?1,?2,?3)",params![request.handoff_id,request.revision,encode(&activity)?])?;
        receipt(&tx, &request.command_id, &raw, &())?;
        event(&tx, "conversation", &saved.conversation_id, now)?;
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
