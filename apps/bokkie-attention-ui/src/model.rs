use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet},
};

use bokkie_operator_api::{
    ActionCapability, ActionPrecondition, ApprovalSubject, ObligationTopic, OperatorObligation,
    OperatorObligationState, OperatorSnapshot, ProjectionChangePage,
};
use polyorama_core::{DockNode, DockNodeId, LAYOUT_SCHEMA_VERSION, PaneId, SplitAxis, Workspace};
use serde::Serialize;

pub const INBOX_PANE_ID: PaneId = PaneId(1);
pub const OBLIGATIONS_PANE_ID: PaneId = PaneId(2);
pub const TIMELINE_PANE_ID: PaneId = PaneId(3);

#[derive(Debug)]
pub(crate) struct SnapshotAssembly {
    pub generation: u64,
    captured_at: Option<i64>,
    watermark: Option<i64>,
    service: Option<bokkie_operator_api::ServiceIdentity>,
    seen_cursors: BTreeSet<String>,
    seen_obligations: BTreeSet<String>,
    obligations: Vec<OperatorObligation>,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum PageProgress<T> {
    Continue { cursor: String, watermark: i64 },
    Complete(T),
}

impl SnapshotAssembly {
    pub fn new(generation: u64) -> Self {
        Self {
            generation,
            captured_at: None,
            watermark: None,
            service: None,
            seen_cursors: BTreeSet::new(),
            seen_obligations: BTreeSet::new(),
            obligations: Vec::new(),
        }
    }

    pub fn push(
        &mut self,
        mut page: OperatorSnapshot,
    ) -> Result<PageProgress<OperatorSnapshot>, String> {
        if page.watermark < 0 {
            return Err("Snapshot page returned a negative global watermark".to_owned());
        }
        let service = page
            .service
            .clone()
            .ok_or_else(|| "Snapshot page omitted its service identity".to_owned())?;
        match (self.captured_at, self.watermark, self.service.as_ref()) {
            (None, None, None) => {
                self.captured_at = Some(page.captured_at);
                self.watermark = Some(page.watermark);
                self.service = Some(service);
            }
            (Some(captured_at), Some(watermark), Some(expected_service))
                if captured_at == page.captured_at
                    && watermark == page.watermark
                    && expected_service == &service => {}
            _ => {
                return Err(
                    "Snapshot continuation changed its capture, watermark, or service identity"
                        .to_owned(),
                );
            }
        }
        for obligation in page.obligations.drain(..) {
            if !self.seen_obligations.insert(obligation.id.clone()) {
                return Err(format!(
                    "Snapshot continuation duplicated obligation {}",
                    obligation.id
                ));
            }
            self.obligations.push(obligation);
        }
        if let Some(cursor) = page.next_cursor {
            if !self.seen_cursors.insert(cursor.clone()) {
                return Err("Snapshot continuation repeated a cursor".to_owned());
            }
            return Ok(PageProgress::Continue {
                cursor,
                watermark: self.watermark.expect("initialised snapshot watermark"),
            });
        }
        Ok(PageProgress::Complete(OperatorSnapshot {
            captured_at: self.captured_at.expect("initialised snapshot capture"),
            service: self.service.clone(),
            next_cursor: None,
            watermark: self.watermark.expect("initialised snapshot watermark"),
            obligations: std::mem::take(&mut self.obligations),
        }))
    }
}

#[derive(Debug)]
pub(crate) struct TopicAssembly {
    pub generation: u64,
    obligation_id: String,
    captured_at: Option<i64>,
    watermark: Option<i64>,
    service: Option<bokkie_operator_api::ServiceIdentity>,
    seen_cursors: BTreeSet<String>,
    seen_items: BTreeSet<String>,
    items: Vec<bokkie_operator_api::TopicItem>,
}

impl TopicAssembly {
    pub fn new(generation: u64, obligation_id: String) -> Self {
        Self {
            generation,
            obligation_id,
            captured_at: None,
            watermark: None,
            service: None,
            seen_cursors: BTreeSet::new(),
            seen_items: BTreeSet::new(),
            items: Vec::new(),
        }
    }

    pub fn push(
        &mut self,
        mut page: ObligationTopic,
    ) -> Result<PageProgress<ObligationTopic>, String> {
        if page.obligation_id != self.obligation_id {
            return Err("Topic continuation returned a different obligation".to_owned());
        }
        if page.watermark < 0 {
            return Err("Topic page returned a negative global watermark".to_owned());
        }
        let service = page
            .service
            .clone()
            .ok_or_else(|| "Topic page omitted its service identity".to_owned())?;
        match (self.captured_at, self.watermark, self.service.as_ref()) {
            (None, None, None) => {
                self.captured_at = Some(page.captured_at);
                self.watermark = Some(page.watermark);
                self.service = Some(service);
            }
            (Some(captured_at), Some(watermark), Some(expected_service))
                if captured_at == page.captured_at
                    && watermark == page.watermark
                    && expected_service == &service => {}
            _ => {
                return Err(
                    "Topic continuation changed its capture, watermark, or service identity"
                        .to_owned(),
                );
            }
        }
        for item in page.items.drain(..) {
            if !self.seen_items.insert(item.stable_id.clone()) {
                return Err(format!(
                    "Topic continuation duplicated item {}",
                    item.stable_id
                ));
            }
            self.items.push(item);
        }
        if let Some(cursor) = page.next_cursor {
            if !self.seen_cursors.insert(cursor.clone()) {
                return Err("Topic continuation repeated a cursor".to_owned());
            }
            return Ok(PageProgress::Continue {
                cursor,
                watermark: self.watermark.expect("initialised topic watermark"),
            });
        }
        Ok(PageProgress::Complete(ObligationTopic {
            captured_at: self.captured_at.expect("initialised topic capture"),
            obligation_id: self.obligation_id.clone(),
            service: self.service.clone(),
            next_cursor: None,
            watermark: self.watermark.expect("initialised topic watermark"),
            items: std::mem::take(&mut self.items),
        }))
    }
}

#[derive(Debug)]
pub(crate) struct ChangeAssembly {
    pub generation: u64,
    base_after: i64,
    request_after: i64,
    watermark: Option<i64>,
    affected: BTreeSet<String>,
    ambiguous: bool,
    seen_revisions: BTreeSet<i64>,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum ChangeProgress {
    Continue {
        after: i64,
        through: i64,
    },
    Complete {
        watermark: i64,
        affected: BTreeSet<String>,
        ambiguous: bool,
    },
}

impl ChangeAssembly {
    pub fn new(generation: u64, after: i64) -> Self {
        Self {
            generation,
            base_after: after,
            request_after: after,
            watermark: None,
            affected: BTreeSet::new(),
            ambiguous: false,
            seen_revisions: BTreeSet::new(),
        }
    }

    pub fn push(&mut self, page: ProjectionChangePage) -> Result<ChangeProgress, String> {
        if page.requested_after != self.request_after {
            return Err("Change page did not echo the requested cursor".to_owned());
        }
        if page.watermark < self.base_after {
            return Err("Change page watermark moved behind applied state".to_owned());
        }
        match self.watermark {
            None => {
                if page.requested_through.is_some() {
                    return Err(
                        "Initial change page unexpectedly reported a pinned walk".to_owned()
                    );
                }
                self.watermark = Some(page.watermark);
            }
            Some(watermark) => {
                if page.requested_through != Some(watermark) || page.watermark != watermark {
                    return Err("Change continuation changed its pinned watermark".to_owned());
                }
            }
        }
        let watermark = self.watermark.expect("initialised change watermark");
        let mut previous = self.request_after;
        for change in page.changes {
            if change.revision <= previous || change.revision > watermark {
                return Err(
                    "Change page revisions were not strictly ordered within the pinned watermark"
                        .to_owned(),
                );
            }
            if !self.seen_revisions.insert(change.revision) {
                return Err(format!(
                    "Change page duplicated revision {}",
                    change.revision
                ));
            }
            previous = change.revision;
            if let Some(obligation_id) = change.obligation_id {
                self.affected.insert(obligation_id);
            } else {
                self.ambiguous = true;
            }
        }
        if let Some(next_after) = page.next_after {
            if next_after != previous || next_after <= self.request_after {
                return Err("Change continuation returned an invalid next cursor".to_owned());
            }
            self.request_after = next_after;
            Ok(ChangeProgress::Continue {
                after: next_after,
                through: watermark,
            })
        } else {
            Ok(ChangeProgress::Complete {
                watermark,
                affected: std::mem::take(&mut self.affected),
                ambiguous: self.ambiguous,
            })
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub enum LifecycleAction {
    Approve,
    Reject,
    Retry,
    Cancel,
    ApproveGardenerProposal,
    RejectGardenerProposal,
}

impl LifecycleAction {
    pub const ALL: [Self; 6] = [
        Self::Approve,
        Self::Reject,
        Self::Retry,
        Self::Cancel,
        Self::ApproveGardenerProposal,
        Self::RejectGardenerProposal,
    ];

    pub fn capability(self, obligation: &OperatorObligation) -> &ActionCapability {
        match self {
            Self::Approve => &obligation.capabilities.approve,
            Self::Reject => &obligation.capabilities.reject,
            Self::Retry => &obligation.capabilities.retry,
            Self::Cancel => &obligation.capabilities.cancel,
            Self::ApproveGardenerProposal => &obligation.capabilities.approve_gardener_proposal,
            Self::RejectGardenerProposal => &obligation.capabilities.reject_gardener_proposal,
        }
    }

    pub const fn requires_decision_body(self) -> bool {
        matches!(
            self,
            Self::Approve
                | Self::Reject
                | Self::ApproveGardenerProposal
                | Self::RejectGardenerProposal
        )
    }

    pub const fn is_gardener(self) -> bool {
        matches!(
            self,
            Self::ApproveGardenerProposal | Self::RejectGardenerProposal
        )
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum StateFilter {
    #[default]
    All,
    Live,
    Attention,
    Terminal,
}

impl StateFilter {
    pub const OPTIONS: [(Self, &'static str); 4] = [
        (Self::All, "All states"),
        (Self::Live, "Live"),
        (Self::Attention, "Needs attention"),
        (Self::Terminal, "Terminal"),
    ];

    fn includes(self, state: OperatorObligationState) -> bool {
        match self {
            Self::All => true,
            Self::Live => !matches!(
                state,
                OperatorObligationState::Completed | OperatorObligationState::Cancelled
            ),
            Self::Attention => matches!(
                state,
                OperatorObligationState::AwaitingApproval | OperatorObligationState::Attention
            ),
            Self::Terminal => matches!(
                state,
                OperatorObligationState::Completed | OperatorObligationState::Cancelled
            ),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConnectionState {
    Loading,
    Current,
    Stale { reason: String },
}

impl ConnectionState {
    pub const fn decisions_safe(&self) -> bool {
        matches!(self, Self::Current)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Confirmation {
    pub action: LifecycleAction,
    pub obligation_id: String,
    pub occurrence: u32,
    pub precondition: ActionPrecondition,
    pub consequence: String,
    pub gardener: Option<GardenerConfirmation>,
    pub actor: String,
    pub note: String,
    pub conflict: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GardenerConfirmation {
    pub repository: String,
    /// Stable goal identity retained across source-bound generations.
    pub fingerprint: String,
    pub instance_id: String,
    pub generation: u32,
    pub source_commit: String,
    pub source_observation_id: i64,
    pub source_inspection_id: String,
    pub prompt: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AppModel {
    pub snapshot: Option<OperatorSnapshot>,
    pub selected_obligation: Option<String>,
    pub topic: Option<ObligationTopic>,
    pub topic_error: Option<String>,
    pub search: String,
    pub state_filter: StateFilter,
    pub connection: ConnectionState,
    pub last_successful_refresh: Option<i64>,
    pub status: String,
    pub confirmation: Option<Confirmation>,
    pub snapshot_busy: bool,
    pub topic_busy: bool,
    pub action_busy: bool,
}

impl Default for AppModel {
    fn default() -> Self {
        Self {
            snapshot: None,
            selected_obligation: None,
            topic: None,
            topic_error: None,
            search: String::new(),
            state_filter: StateFilter::All,
            connection: ConnectionState::Loading,
            last_successful_refresh: None,
            status: "Connecting to Bokkie".to_owned(),
            confirmation: None,
            snapshot_busy: false,
            topic_busy: false,
            action_busy: false,
        }
    }
}

impl AppModel {
    pub fn obligations(&self) -> &[OperatorObligation] {
        self.snapshot
            .as_ref()
            .map_or(&[], |snapshot| snapshot.obligations.as_slice())
    }

    pub fn exceptions(&self) -> impl Iterator<Item = &OperatorObligation> {
        self.obligations()
            .iter()
            .filter(|obligation| obligation.exception.is_some())
    }

    pub fn filtered_obligations(&self) -> Vec<&OperatorObligation> {
        let query = self.search.trim().to_lowercase();
        self.obligations()
            .iter()
            .filter(|obligation| self.state_filter.includes(obligation.state))
            .filter(|obligation| {
                query.is_empty()
                    || obligation.id.to_lowercase().contains(&query)
                    || obligation.description.to_lowercase().contains(&query)
                    || obligation
                        .last_error
                        .as_deref()
                        .is_some_and(|value| value.to_lowercase().contains(&query))
            })
            .collect()
    }

    pub fn selected(&self) -> Option<&OperatorObligation> {
        let selected = self.selected_obligation.as_deref()?;
        self.obligations()
            .iter()
            .find(|obligation| obligation.id == selected)
    }

    /// Apply a backend-ordered snapshot without disturbing a surviving selection.
    pub fn apply_snapshot(&mut self, snapshot: OperatorSnapshot) {
        let selection_survives = self.selected_obligation.as_ref().is_some_and(|selected| {
            snapshot
                .obligations
                .iter()
                .any(|obligation| &obligation.id == selected)
        });
        if !selection_survives {
            self.selected_obligation = snapshot
                .obligations
                .iter()
                .find(|obligation| obligation.exception.is_some())
                .or_else(|| snapshot.obligations.first())
                .map(|obligation| obligation.id.clone());
            self.topic = None;
        }
        self.last_successful_refresh = Some(snapshot.captured_at);
        self.snapshot = Some(snapshot);
        self.connection = ConnectionState::Current;
        self.status = "Current operator state".to_owned();
        self.snapshot_busy = false;
    }

    pub fn applied_watermark(&self) -> Option<i64> {
        self.snapshot.as_ref().map(|snapshot| snapshot.watermark)
    }

    /// Apply only the projections invalidated by a completed change walk.
    ///
    /// The durable watermark advances after every supplied projection has been
    /// collected by the caller. Backend semantic ordering is restored after
    /// each batch, while selection and any operator draft remain untouched.
    pub fn apply_incremental(
        &mut self,
        watermark: i64,
        refreshed_at: i64,
        projections: Vec<OperatorObligation>,
    ) -> Result<(), String> {
        let snapshot = self
            .snapshot
            .as_mut()
            .ok_or_else(|| "Incremental state requires a completed snapshot".to_owned())?;
        if watermark < snapshot.watermark {
            return Err("Incremental watermark moved behind applied state".to_owned());
        }
        let mut replacements = BTreeMap::new();
        for obligation in projections {
            if replacements
                .insert(obligation.id.clone(), obligation)
                .is_some()
            {
                return Err("Incremental refresh duplicated an obligation".to_owned());
            }
        }
        for obligation in &mut snapshot.obligations {
            if let Some(replacement) = replacements.remove(&obligation.id) {
                *obligation = replacement;
            }
        }
        snapshot.obligations.extend(replacements.into_values());
        snapshot.obligations.sort_by(operator_semantic_order);
        snapshot.watermark = watermark;
        snapshot.captured_at = refreshed_at.max(snapshot.captured_at);
        self.last_successful_refresh = Some(snapshot.captured_at);
        self.connection = ConnectionState::Current;
        self.status = "Current operator state".to_owned();
        self.snapshot_busy = false;
        Ok(())
    }

    pub fn mark_stale(&mut self, reason: impl Into<String>) {
        let reason = reason.into();
        self.connection = ConnectionState::Stale {
            reason: reason.clone(),
        };
        self.status = "Showing retained state — decisions disabled".to_owned();
        self.snapshot_busy = false;
    }

    pub fn record_transition_conflict(&mut self, message: &str) {
        if let Some(confirmation) = self.confirmation.as_mut() {
            confirmation.conflict = Some(format!(
                "The state changed before this action could be applied: {message}. Bokkie has refreshed the obligation; your actor and note are retained. Review the current state before trying again."
            ));
        }
        self.action_busy = false;
        self.mark_stale("Transition conflict; refreshing current state");
    }

    pub fn record_action_accepted(&mut self, action_label: &str) {
        self.action_busy = false;
        self.confirmation = None;
        self.status = format!("{action_label} accepted; refreshing durable state");
    }

    pub fn record_session_change(&mut self, message: &str) {
        self.confirmation = None;
        self.action_busy = false;
        self.mark_stale(format!(
            "Bokkie process session changed: {message}; review current state before deciding"
        ));
    }

    pub fn select(&mut self, obligation_id: String) -> bool {
        if self.selected_obligation.as_deref() == Some(&obligation_id) {
            return false;
        }
        self.selected_obligation = Some(obligation_id);
        self.topic = None;
        self.topic_error = None;
        true
    }

    pub fn begin_confirmation(&mut self, action: LifecycleAction) -> Result<(), String> {
        if !self.connection.decisions_safe() || self.snapshot_busy || self.topic_busy {
            return Err("Refresh to current state before making a decision".to_owned());
        }
        let obligation = self
            .selected()
            .ok_or_else(|| "Select an obligation first".to_owned())?;
        let capability = action.capability(obligation);
        if !capability.available {
            return Err(disabled_reason(capability).to_owned());
        }
        let precondition = capability
            .precondition
            .clone()
            .ok_or_else(|| "Backend action precondition is unavailable".to_owned())?;
        let gardener = if action.is_gardener() {
            match obligation.exception.as_ref() {
                Some(bokkie_operator_api::ExceptionReason::AwaitingApproval {
                    subject:
                        ApprovalSubject::GardenerProposal {
                            repository,
                            fingerprint,
                            instance_id,
                            generation,
                            source_commit,
                            source_observation_id,
                            source_inspection_id,
                            prompt,
                            obligation_id,
                            occurrence,
                        },
                }) if obligation_id == &obligation.id && *occurrence == obligation.occurrence => {
                    Some(GardenerConfirmation {
                        repository: repository.clone(),
                        fingerprint: fingerprint.clone(),
                        instance_id: instance_id.clone(),
                        generation: *generation,
                        source_commit: source_commit.clone(),
                        source_observation_id: *source_observation_id,
                        source_inspection_id: source_inspection_id.clone(),
                        prompt: prompt.clone(),
                    })
                }
                _ => return Err("Exact gardener proposal identity is unavailable".to_owned()),
            }
        } else {
            None
        };
        self.confirmation = Some(Confirmation {
            action,
            obligation_id: obligation.id.clone(),
            occurrence: obligation.occurrence,
            precondition,
            consequence: consequence_label(capability).to_owned(),
            gardener,
            actor: "operator".to_owned(),
            note: String::new(),
            conflict: None,
        });
        Ok(())
    }

    pub fn action_availability(
        &self,
        action: LifecycleAction,
        obligation: &OperatorObligation,
    ) -> Result<(), String> {
        if self.action_busy {
            return Err("Another lifecycle request is in progress".to_owned());
        }
        if self.snapshot_busy {
            return Err("A current-state refresh is in progress".to_owned());
        }
        if self.topic_busy {
            return Err("The selected evidence topic is still refreshing".to_owned());
        }
        if !self.connection.decisions_safe() {
            return Err("Retained data may be stale; refresh before deciding".to_owned());
        }
        let capability = action.capability(obligation);
        capability
            .available
            .then_some(())
            .ok_or_else(|| disabled_reason(capability).to_owned())
    }

    pub fn confirmation_matches_current_state(&self, confirmation: &Confirmation) -> bool {
        self.selected().is_some_and(|current| {
            current.id == confirmation.obligation_id
                && current.occurrence == confirmation.occurrence
                && confirmation
                    .action
                    .capability(current)
                    .precondition
                    .as_ref()
                    == Some(&confirmation.precondition)
        })
    }
}

fn operator_semantic_order(left: &OperatorObligation, right: &OperatorObligation) -> Ordering {
    operator_order_key(left).cmp(&operator_order_key(right))
}

fn operator_order_key(obligation: &OperatorObligation) -> (i64, i64, i64, i64, &str) {
    let exception_rank = if obligation.exception.is_some() { 0 } else { 1 };
    let state_rank = match obligation.state {
        OperatorObligationState::AwaitingApproval => 0,
        OperatorObligationState::Attention => 1,
        OperatorObligationState::Running => 2,
        OperatorObligationState::RetryScheduled => 3,
        OperatorObligationState::Pending => 4,
        OperatorObligationState::Completed => 5,
        OperatorObligationState::Cancelled => 6,
    };
    (
        exception_rank,
        state_rank,
        obligation.next_wake_at.unwrap_or(i64::MAX),
        obligation.updated_at,
        obligation.id.as_str(),
    )
}

pub fn operator_workspace() -> Workspace {
    Workspace {
        schema_version: LAYOUT_SCHEMA_VERSION,
        root: DockNode::Split {
            id: DockNodeId(1),
            axis: SplitAxis::Horizontal,
            fraction: 0.62,
            first: Box::new(DockNode::Split {
                id: DockNodeId(2),
                axis: SplitAxis::Horizontal,
                fraction: 0.43,
                first: Box::new(DockNode::Tabs {
                    id: DockNodeId(3),
                    tabs: vec![INBOX_PANE_ID],
                    active: 0,
                }),
                second: Box::new(DockNode::Tabs {
                    id: DockNodeId(4),
                    tabs: vec![OBLIGATIONS_PANE_ID],
                    active: 0,
                }),
            }),
            second: Box::new(DockNode::Tabs {
                id: DockNodeId(5),
                tabs: vec![TIMELINE_PANE_ID],
                active: 0,
            }),
        },
        active_pane: INBOX_PANE_ID,
        closed_optional_panes: BTreeSet::new(),
        next_node_id: 6,
    }
}

pub fn disabled_reason(capability: &ActionCapability) -> &'static str {
    use bokkie_operator_api::DisabledReason;
    match capability.disabled_reason {
        Some(DisabledReason::StateDoesNotPermit) => "Current state does not permit this action",
        Some(DisabledReason::RunningClaimOwnsObligation) => {
            "A running claim currently owns this obligation"
        }
        Some(DisabledReason::TerminalObligation) => "The obligation is already terminal",
        Some(DisabledReason::GardenerProposalRequiresExactDecision) => {
            "Use the exact gardener proposal decision"
        }
        Some(DisabledReason::NotGardenerProposal) => "This is not a gardener proposal",
        None if capability.available => "",
        None => "The backend did not authorise this action",
    }
}

pub fn consequence_label(capability: &ActionCapability) -> &'static str {
    use bokkie_operator_api::ActionConsequence;
    match capability.consequence {
        ActionConsequence::ScheduleCurrentOccurrence => "Schedule the current occurrence",
        ActionConsequence::MoveToAttention => "Move the current occurrence to attention",
        ActionConsequence::ReopenForRetry => "Reopen this obligation for retry",
        ActionConsequence::CancelObligation => "Permanently cancel this obligation",
        ActionConsequence::ScheduleExactGardenerProposal => {
            "Schedule implementation of this exact immutable proposal"
        }
        ActionConsequence::RejectExactGardenerProposal => {
            "Reject this exact immutable proposal into attention"
        }
    }
}

pub trait OperatorStateLabel {
    fn label(self) -> &'static str;
}

impl OperatorStateLabel for OperatorObligationState {
    fn label(self) -> &'static str {
        match self {
            Self::Pending => "Pending",
            Self::AwaitingApproval => "Awaiting approval",
            Self::Running => "Running",
            Self::RetryScheduled => "Retry scheduled",
            Self::Attention => "Attention",
            Self::Completed => "Completed",
            Self::Cancelled => "Cancelled",
        }
    }
}

#[cfg(test)]
mod tests {
    use bokkie_operator_api::{
        ActionConsequence, ActionPrecondition, DisabledReason, DurableLiveness, ExceptionReason,
        OperatorCapabilities, ProjectionChange, ProjectionChangePage, ProjectionEventProvenance,
        ProjectionEventSource, ServiceIdentity, TopicItem, TopicSource,
    };

    use super::*;

    fn capability(available: bool, consequence: ActionConsequence) -> ActionCapability {
        ActionCapability {
            available,
            disabled_reason: (!available).then_some(DisabledReason::StateDoesNotPermit),
            consequence,
            precondition: available.then(|| ActionPrecondition {
                obligation_id: "fixture".to_owned(),
                occurrence: 3,
                state_revision: 1,
                gardener_fingerprint: None,
                gardener_proposal_instance_id: None,
                gardener_source_commit: None,
                gardener_source_observation_id: None,
                gardener_source_inspection_id: None,
                gardener_generation: None,
            }),
        }
    }

    fn obligation(id: &str, state: OperatorObligationState) -> OperatorObligation {
        let approve = capability(false, ActionConsequence::ScheduleCurrentOccurrence);
        OperatorObligation {
            id: id.to_owned(),
            description: format!("A deliberately long obligation description for {id}"),
            state,
            occurrence: 3,
            scheduled_at: 90,
            next_wake_at: None,
            recurrence_cron: None,
            recurrence_timezone: None,
            approval_required: false,
            attempts_made: 2,
            max_attempts: 5,
            retry_base_seconds: 1,
            retry_max_seconds: 30,
            last_error: None,
            last_evidence: Some("retained evidence".to_owned()),
            failure_disposition: None,
            created_at: 80,
            updated_at: 100,
            exception: None,
            liveness: Some(DurableLiveness::FutureWake { wake_at: 110 }),
            capabilities: OperatorCapabilities {
                approve: approve.clone(),
                reject: capability(false, ActionConsequence::MoveToAttention),
                retry: capability(false, ActionConsequence::ReopenForRetry),
                cancel: capability(true, ActionConsequence::CancelObligation),
                approve_gardener_proposal: capability(
                    false,
                    ActionConsequence::ScheduleExactGardenerProposal,
                ),
                reject_gardener_proposal: capability(
                    false,
                    ActionConsequence::RejectExactGardenerProposal,
                ),
            },
        }
    }

    fn service(session_id: &str) -> ServiceIdentity {
        ServiceIdentity {
            build: bokkie_operator_api::BOKKIE_BUILD_ID.to_owned(),
            api_contract_version: bokkie_operator_api::API_CONTRACT_VERSION,
            schema_version: bokkie_operator_api::SUPPORTED_SCHEMA_VERSION,
            process_id: 42,
            session_id: session_id.to_owned(),
        }
    }

    fn snapshot_page(
        obligations: Vec<OperatorObligation>,
        next_cursor: Option<&str>,
        watermark: i64,
    ) -> OperatorSnapshot {
        OperatorSnapshot {
            captured_at: 120,
            service: Some(service("session-one")),
            next_cursor: next_cursor.map(ToOwned::to_owned),
            watermark,
            obligations,
        }
    }

    fn topic_page(items: Vec<TopicItem>, next_cursor: Option<&str>) -> ObligationTopic {
        ObligationTopic {
            captured_at: 120,
            obligation_id: "selected".to_owned(),
            service: Some(service("session-one")),
            next_cursor: next_cursor.map(ToOwned::to_owned),
            watermark: 21,
            items,
        }
    }

    fn topic_item(revision: i64) -> TopicItem {
        TopicItem {
            occurred_at: 100 + revision,
            source: TopicSource::AuditEvent,
            source_sequence: revision.to_string(),
            stable_id: format!("envelope:{revision}"),
            occurrence: Some(1),
            event_type: "updated".to_owned(),
            evidence: serde_json::json!({"revision": revision}),
        }
    }

    fn change(revision: i64, obligation_id: Option<&str>) -> ProjectionChange {
        ProjectionChange {
            revision,
            provenance: ProjectionEventProvenance::LiveAppend,
            source: ProjectionEventSource::AuditEvent { sequence: revision },
            event_type: "updated".to_owned(),
            occurred_at: 100 + revision,
            obligation_id: obligation_id.map(ToOwned::to_owned),
            occurrence: Some(1),
            repository: None,
            inspection_id: None,
            proposal_fingerprint: None,
            proposal_instance_id: None,
            run_id: None,
        }
    }

    #[test]
    fn snapshot_and_topic_pages_assemble_at_one_stable_capture_and_watermark() {
        let mut snapshots = SnapshotAssembly::new(7);
        assert_eq!(
            snapshots
                .push(snapshot_page(
                    vec![obligation("first", OperatorObligationState::Pending)],
                    Some("snapshot-next"),
                    20,
                ))
                .unwrap(),
            PageProgress::Continue {
                cursor: "snapshot-next".to_owned(),
                watermark: 20,
            }
        );
        let PageProgress::Complete(snapshot) = snapshots
            .push(snapshot_page(
                vec![obligation("second", OperatorObligationState::Running)],
                None,
                20,
            ))
            .unwrap()
        else {
            panic!("snapshot should be complete")
        };
        assert_eq!(snapshot.captured_at, 120);
        assert_eq!(snapshot.watermark, 20);
        assert_eq!(snapshot.obligations.len(), 2);

        let mut topics = TopicAssembly::new(8, "selected".to_owned());
        assert!(matches!(
            topics
                .push(topic_page(vec![topic_item(1)], Some("topic-next")))
                .unwrap(),
            PageProgress::Continue { watermark: 21, .. }
        ));
        let PageProgress::Complete(topic) =
            topics.push(topic_page(vec![topic_item(2)], None)).unwrap()
        else {
            panic!("topic should be complete")
        };
        assert_eq!(topic.items.len(), 2);
        assert_eq!(topic.watermark, 21);
    }

    #[test]
    fn page_assembly_rejects_duplicates_and_capture_or_watermark_mismatch() {
        let first = obligation("first", OperatorObligationState::Pending);
        let mut duplicate = SnapshotAssembly::new(1);
        duplicate
            .push(snapshot_page(vec![first.clone()], Some("next"), 20))
            .unwrap();
        assert!(
            duplicate
                .push(snapshot_page(vec![first], None, 20))
                .unwrap_err()
                .contains("duplicated obligation")
        );

        let mut mismatch = SnapshotAssembly::new(2);
        mismatch
            .push(snapshot_page(Vec::new(), Some("next"), 20))
            .unwrap();
        let mut changed = snapshot_page(Vec::new(), None, 21);
        changed.captured_at = 121;
        assert!(mismatch.push(changed).unwrap_err().contains("changed"));

        let mut topic = TopicAssembly::new(3, "selected".to_owned());
        topic
            .push(topic_page(vec![topic_item(1)], Some("next")))
            .unwrap();
        assert!(
            topic
                .push(topic_page(vec![topic_item(1)], None))
                .unwrap_err()
                .contains("duplicated item")
        );
    }

    #[test]
    fn change_walk_drains_one_pinned_watermark_without_misses_or_duplicates() {
        let mut walk = ChangeAssembly::new(4, 10);
        assert_eq!(
            walk.push(ProjectionChangePage {
                service: service("session-one"),
                requested_after: 10,
                requested_through: None,
                next_after: Some(13),
                watermark: 15,
                changes: vec![change(11, Some("first")), change(13, Some("first"))],
            })
            .unwrap(),
            ChangeProgress::Continue {
                after: 13,
                through: 15,
            }
        );
        assert_eq!(
            walk.push(ProjectionChangePage {
                service: service("session-one"),
                requested_after: 13,
                requested_through: Some(15),
                next_after: None,
                watermark: 15,
                changes: vec![change(15, Some("second"))],
            })
            .unwrap(),
            ChangeProgress::Complete {
                watermark: 15,
                affected: BTreeSet::from(["first".to_owned(), "second".to_owned()]),
                ambiguous: false,
            }
        );
    }

    #[test]
    fn change_walk_rejects_repeated_or_mismatched_continuations_and_marks_ambiguity() {
        let mut duplicate = ChangeAssembly::new(1, 10);
        duplicate
            .push(ProjectionChangePage {
                service: service("session-one"),
                requested_after: 10,
                requested_through: None,
                next_after: Some(11),
                watermark: 12,
                changes: vec![change(11, Some("first"))],
            })
            .unwrap();
        assert!(
            duplicate
                .push(ProjectionChangePage {
                    service: service("session-one"),
                    requested_after: 11,
                    requested_through: Some(12),
                    next_after: None,
                    watermark: 12,
                    changes: vec![change(11, Some("first"))],
                })
                .unwrap_err()
                .contains("strictly ordered")
        );

        let mut ambiguous = ChangeAssembly::new(2, 12);
        assert!(matches!(
            ambiguous
                .push(ProjectionChangePage {
                    service: service("session-one"),
                    requested_after: 12,
                    requested_through: None,
                    next_after: None,
                    watermark: 13,
                    changes: vec![change(13, None)],
                })
                .unwrap(),
            ChangeProgress::Complete {
                ambiguous: true,
                ..
            }
        ));
    }

    #[test]
    fn workspace_has_three_stable_valid_panes() {
        let workspace = operator_workspace();
        workspace.validate().unwrap();
        let mut panes = Vec::new();
        workspace.root.pane_ids(&mut panes);
        assert_eq!(
            panes,
            [INBOX_PANE_ID, OBLIGATIONS_PANE_ID, TIMELINE_PANE_ID]
        );
    }

    #[test]
    fn snapshot_preserves_selection_and_backend_order() {
        let mut model = AppModel {
            selected_obligation: Some("second".to_owned()),
            ..AppModel::default()
        };
        model.apply_snapshot(OperatorSnapshot {
            captured_at: 120,
            service: None,
            next_cursor: None,
            watermark: 10,
            obligations: vec![
                obligation("first", OperatorObligationState::Pending),
                obligation("second", OperatorObligationState::Running),
            ],
        });
        assert_eq!(model.selected_obligation.as_deref(), Some("second"));
        assert_eq!(model.obligations()[0].id, "first");
        assert_eq!(model.last_successful_refresh, Some(120));
        assert_eq!(model.connection, ConnectionState::Current);
    }

    #[test]
    fn stale_retained_state_disables_actions_without_dropping_data() {
        let mut model = AppModel::default();
        model.apply_snapshot(OperatorSnapshot {
            captured_at: 120,
            service: None,
            next_cursor: None,
            watermark: 10,
            obligations: vec![obligation("kept", OperatorObligationState::Pending)],
        });
        model.mark_stale("service disconnected");
        assert_eq!(model.obligations()[0].id, "kept");
        assert!(
            model
                .action_availability(LifecycleAction::Cancel, model.selected().unwrap())
                .unwrap_err()
                .contains("stale")
        );
    }

    #[test]
    fn exact_gardener_confirmation_carries_immutable_identity_and_draft_survives_conflict() {
        let mut proposal = obligation("implementation", OperatorObligationState::AwaitingApproval);
        proposal.exception = Some(ExceptionReason::AwaitingApproval {
            subject: ApprovalSubject::GardenerProposal {
                repository: "robchristie/bokkie".to_owned(),
                fingerprint: "f".repeat(64),
                instance_id: "proposal-instance-2".to_owned(),
                generation: 2,
                source_commit: "c".repeat(40),
                source_observation_id: 42,
                source_inspection_id: "inspection-2".to_owned(),
                prompt: "Implement exactly this long prompt without inference".to_owned(),
                obligation_id: "implementation".to_owned(),
                occurrence: 3,
            },
        });
        proposal.capabilities.approve_gardener_proposal =
            capability(true, ActionConsequence::ScheduleExactGardenerProposal);
        let exact_precondition = proposal
            .capabilities
            .approve_gardener_proposal
            .precondition
            .as_mut()
            .unwrap();
        exact_precondition.obligation_id = "implementation".to_owned();
        exact_precondition.gardener_fingerprint = Some("f".repeat(64));
        exact_precondition.gardener_proposal_instance_id = Some("proposal-instance-2".to_owned());
        exact_precondition.gardener_source_commit = Some("c".repeat(40));
        exact_precondition.gardener_source_observation_id = Some(42);
        exact_precondition.gardener_source_inspection_id = Some("inspection-2".to_owned());
        exact_precondition.gardener_generation = Some(2);
        let mut model = AppModel::default();
        model.apply_snapshot(OperatorSnapshot {
            captured_at: 120,
            service: None,
            next_cursor: None,
            watermark: 10,
            obligations: vec![proposal],
        });
        model
            .begin_confirmation(LifecycleAction::ApproveGardenerProposal)
            .unwrap();
        let confirmation = model.confirmation.as_mut().unwrap();
        confirmation.actor = "rob".to_owned();
        confirmation.note = "Checked exact evidence".to_owned();
        confirmation.conflict = Some("State changed; refreshed before retrying".to_owned());
        assert_eq!(confirmation.note, "Checked exact evidence");
        assert_eq!(
            confirmation.gardener.as_ref().unwrap().prompt,
            "Implement exactly this long prompt without inference"
        );
        assert_eq!(confirmation.occurrence, 3);
        let gardener = confirmation.gardener.as_ref().unwrap();
        assert_eq!(gardener.instance_id, "proposal-instance-2");
        assert_eq!(gardener.generation, 2);
        assert_eq!(gardener.source_commit, "c".repeat(40));
        assert_eq!(gardener.source_observation_id, 42);
        assert_eq!(
            confirmation.precondition.gardener_fingerprint.as_deref(),
            Some("f".repeat(64).as_str())
        );
    }

    #[test]
    fn later_proposal_generation_stales_an_open_confirmation() {
        let mut proposal = obligation("implementation", OperatorObligationState::AwaitingApproval);
        proposal.exception = Some(ExceptionReason::AwaitingApproval {
            subject: ApprovalSubject::GardenerProposal {
                repository: "robchristie/bokkie".to_owned(),
                fingerprint: "f".repeat(64),
                instance_id: "proposal-instance-1".to_owned(),
                generation: 1,
                source_commit: "a".repeat(40),
                source_observation_id: 7,
                source_inspection_id: "inspection-1".to_owned(),
                prompt: "Implement the reviewed goal".to_owned(),
                obligation_id: "implementation".to_owned(),
                occurrence: 3,
            },
        });
        proposal.capabilities.approve_gardener_proposal =
            capability(true, ActionConsequence::ScheduleExactGardenerProposal);
        let precondition = proposal
            .capabilities
            .approve_gardener_proposal
            .precondition
            .as_mut()
            .unwrap();
        precondition.obligation_id = "implementation".to_owned();
        precondition.gardener_fingerprint = Some("f".repeat(64));
        precondition.gardener_proposal_instance_id = Some("proposal-instance-1".to_owned());
        precondition.gardener_source_commit = Some("a".repeat(40));
        precondition.gardener_source_observation_id = Some(7);
        precondition.gardener_source_inspection_id = Some("inspection-1".to_owned());
        precondition.gardener_generation = Some(1);

        let mut model = AppModel::default();
        model.apply_snapshot(OperatorSnapshot {
            captured_at: 120,
            service: None,
            next_cursor: None,
            watermark: 10,
            obligations: vec![proposal.clone()],
        });
        model
            .begin_confirmation(LifecycleAction::ApproveGardenerProposal)
            .unwrap();
        let confirmation = model.confirmation.clone().unwrap();

        if let Some(ExceptionReason::AwaitingApproval {
            subject:
                ApprovalSubject::GardenerProposal {
                    instance_id,
                    generation,
                    source_commit,
                    source_observation_id,
                    source_inspection_id,
                    ..
                },
        }) = proposal.exception.as_mut()
        {
            *instance_id = "proposal-instance-2".to_owned();
            *generation = 2;
            *source_commit = "b".repeat(40);
            *source_observation_id = 8;
            *source_inspection_id = "inspection-2".to_owned();
        }
        let current = proposal
            .capabilities
            .approve_gardener_proposal
            .precondition
            .as_mut()
            .unwrap();
        current.gardener_proposal_instance_id = Some("proposal-instance-2".to_owned());
        current.gardener_source_commit = Some("b".repeat(40));
        current.gardener_source_observation_id = Some(8);
        current.gardener_source_inspection_id = Some("inspection-2".to_owned());
        current.gardener_generation = Some(2);
        model.apply_snapshot(OperatorSnapshot {
            captured_at: 121,
            service: None,
            next_cursor: None,
            watermark: 11,
            obligations: vec![proposal],
        });

        assert!(!model.confirmation_matches_current_state(&confirmation));
        assert_eq!(
            confirmation.gardener.unwrap().instance_id,
            "proposal-instance-1"
        );
    }

    #[test]
    fn fixture_covers_all_lifecycle_states_and_empty_filtering() {
        let states = [
            OperatorObligationState::Pending,
            OperatorObligationState::AwaitingApproval,
            OperatorObligationState::Running,
            OperatorObligationState::RetryScheduled,
            OperatorObligationState::Attention,
            OperatorObligationState::Completed,
            OperatorObligationState::Cancelled,
        ];
        let mut model = AppModel::default();
        model.apply_snapshot(OperatorSnapshot {
            captured_at: 120,
            service: None,
            next_cursor: None,
            watermark: 10,
            obligations: states
                .into_iter()
                .enumerate()
                .map(|(index, state)| obligation(&format!("fixture-{index}"), state))
                .collect(),
        });
        assert_eq!(model.filtered_obligations().len(), 7);
        model.search = "no match".to_owned();
        assert!(model.filtered_obligations().is_empty());
    }

    #[test]
    fn generic_approval_confirmation_and_post_action_refresh_state_are_explicit() {
        let mut approval = obligation("approval", OperatorObligationState::AwaitingApproval);
        approval.exception = Some(ExceptionReason::AwaitingApproval {
            subject: ApprovalSubject::Generic,
        });
        approval.capabilities.approve =
            capability(true, ActionConsequence::ScheduleCurrentOccurrence);
        let mut model = AppModel::default();
        assert_eq!(model.connection, ConnectionState::Loading);
        model.apply_snapshot(OperatorSnapshot {
            captured_at: 120,
            service: None,
            next_cursor: None,
            watermark: 10,
            obligations: vec![approval],
        });
        model.begin_confirmation(LifecycleAction::Approve).unwrap();
        assert_eq!(model.confirmation.as_ref().unwrap().occurrence, 3);
        model.record_action_accepted("Approve");
        assert!(model.confirmation.is_none());
        assert_eq!(model.status, "Approve accepted; refreshing durable state");
    }

    #[test]
    fn transition_conflict_keeps_the_operator_draft_and_retained_snapshot() {
        let mut approval = obligation("approval", OperatorObligationState::AwaitingApproval);
        approval.exception = Some(ExceptionReason::AwaitingApproval {
            subject: ApprovalSubject::Generic,
        });
        approval.capabilities.approve =
            capability(true, ActionConsequence::ScheduleCurrentOccurrence);
        let mut model = AppModel::default();
        model.apply_snapshot(OperatorSnapshot {
            captured_at: 120,
            service: None,
            next_cursor: None,
            watermark: 10,
            obligations: vec![approval],
        });
        model.begin_confirmation(LifecycleAction::Approve).unwrap();
        model.confirmation.as_mut().unwrap().note = "keep this draft".to_owned();
        model.action_busy = true;
        model.record_transition_conflict("current occurrence already changed");
        assert_eq!(model.confirmation.as_ref().unwrap().note, "keep this draft");
        assert!(model.confirmation.as_ref().unwrap().conflict.is_some());
        assert_eq!(model.obligations()[0].id, "approval");
        assert!(matches!(model.connection, ConnectionState::Stale { .. }));
        assert!(!model.action_busy);

        let mut refreshed = model.obligations()[0].clone();
        refreshed
            .capabilities
            .approve
            .precondition
            .as_mut()
            .unwrap()
            .state_revision += 1;
        model.apply_snapshot(OperatorSnapshot {
            captured_at: 121,
            service: None,
            next_cursor: None,
            watermark: 11,
            obligations: vec![refreshed],
        });
        let confirmation = model.confirmation.as_ref().unwrap();
        assert_eq!(confirmation.note, "keep this draft");
        assert!(!model.confirmation_matches_current_state(confirmation));
        assert!(
            model
                .action_availability(LifecycleAction::Approve, model.selected().unwrap())
                .is_ok(),
            "the refreshed state is eligible, but the older confirmation must remain disabled"
        );
    }

    #[test]
    fn process_session_change_invalidates_confirmation_and_retains_snapshot_stale() {
        let mut approval = obligation("approval", OperatorObligationState::AwaitingApproval);
        approval.exception = Some(ExceptionReason::AwaitingApproval {
            subject: ApprovalSubject::Generic,
        });
        approval.capabilities.approve =
            capability(true, ActionConsequence::ScheduleCurrentOccurrence);
        let mut model = AppModel::default();
        model.apply_snapshot(OperatorSnapshot {
            captured_at: 120,
            service: None,
            next_cursor: None,
            watermark: 10,
            obligations: vec![approval],
        });
        model.begin_confirmation(LifecycleAction::Approve).unwrap();
        model.action_busy = true;

        model.record_session_change("service restarted");

        assert!(model.confirmation.is_none());
        assert_eq!(model.obligations()[0].id, "approval");
        assert!(matches!(model.connection, ConnectionState::Stale { .. }));
        assert!(!model.action_busy);
        assert!(model.status.contains("decisions disabled"));
    }

    #[test]
    fn five_thousand_row_snapshot_applies_bounded_incremental_upserts_and_reorders() {
        let obligations = (0..5_000)
            .map(|index| {
                obligation(
                    &format!("obligation-{index:04}"),
                    OperatorObligationState::Pending,
                )
            })
            .collect::<Vec<_>>();
        let mut model = AppModel {
            selected_obligation: Some("obligation-2500".to_owned()),
            ..AppModel::default()
        };
        model.apply_snapshot(OperatorSnapshot {
            captured_at: 120,
            service: Some(service("session-one")),
            next_cursor: None,
            watermark: 100,
            obligations,
        });
        model.begin_confirmation(LifecycleAction::Cancel).unwrap();
        let retained_confirmation = model.confirmation.clone().unwrap();
        let retained_topic = topic_page(vec![topic_item(99)], None);
        model.topic = Some(retained_topic.clone());

        let mut unrelated = model.obligations()[4_000].clone();
        unrelated.description = "Incrementally refreshed unrelated row".to_owned();
        model.apply_incremental(101, 121, vec![unrelated]).unwrap();
        assert_eq!(model.obligations().len(), 5_000);
        assert_eq!(
            model.selected_obligation.as_deref(),
            Some("obligation-2500")
        );
        assert_eq!(model.topic.as_ref(), Some(&retained_topic));
        assert_eq!(model.confirmation.as_ref(), Some(&retained_confirmation));
        assert_eq!(model.applied_watermark(), Some(101));

        let mut selected = model.selected().unwrap().clone();
        selected.state = OperatorObligationState::Attention;
        selected.exception = Some(ExceptionReason::Attention {
            cause: bokkie_operator_api::AttentionCause::PersistedFailure,
            error: Some("new durable failure".to_owned()),
            evidence: None,
        });
        selected
            .capabilities
            .cancel
            .precondition
            .as_mut()
            .unwrap()
            .state_revision += 1;
        model.apply_incremental(102, 122, vec![selected]).unwrap();
        assert_eq!(model.obligations()[0].id, "obligation-2500");
        assert_eq!(
            model.selected_obligation.as_deref(),
            Some("obligation-2500")
        );
        assert!(!model.confirmation_matches_current_state(model.confirmation.as_ref().unwrap()));
        assert_eq!(model.applied_watermark(), Some(102));
    }

    #[test]
    fn global_watermark_and_action_state_revision_remain_independent() {
        let mut model = AppModel::default();
        model.apply_snapshot(OperatorSnapshot {
            captured_at: 120,
            service: Some(service("process-session")),
            next_cursor: None,
            watermark: 8_000,
            obligations: vec![obligation("selected", OperatorObligationState::Pending)],
        });
        model.begin_confirmation(LifecycleAction::Cancel).unwrap();
        let confirmation = model.confirmation.as_ref().unwrap();
        assert_eq!(confirmation.precondition.state_revision, 1);
        assert_eq!(model.applied_watermark(), Some(8_000));
        assert_eq!(
            model
                .snapshot
                .as_ref()
                .unwrap()
                .service
                .as_ref()
                .unwrap()
                .session_id,
            "process-session"
        );
    }

    #[test]
    fn empty_database_becomes_current_and_disabled_reasons_are_observable() {
        let mut model = AppModel::default();
        model.apply_snapshot(OperatorSnapshot {
            captured_at: 120,
            service: None,
            next_cursor: None,
            watermark: 10,
            obligations: Vec::new(),
        });
        assert!(model.obligations().is_empty());
        assert!(model.exceptions().next().is_none());
        assert_eq!(model.connection, ConnectionState::Current);

        for reason in [
            DisabledReason::StateDoesNotPermit,
            DisabledReason::RunningClaimOwnsObligation,
            DisabledReason::TerminalObligation,
            DisabledReason::GardenerProposalRequiresExactDecision,
            DisabledReason::NotGardenerProposal,
        ] {
            let capability = ActionCapability {
                available: false,
                disabled_reason: Some(reason),
                consequence: ActionConsequence::CancelObligation,
                precondition: None,
            };
            assert!(!disabled_reason(&capability).is_empty());
        }
    }
}
