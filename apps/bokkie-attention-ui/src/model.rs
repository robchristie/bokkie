use std::collections::BTreeSet;

use polyorama_core::{DockNode, DockNodeId, LAYOUT_SCHEMA_VERSION, PaneId, Workspace};
use serde::{Deserialize, Serialize};

pub use bokkie_operator_api::{OperatorObligation as ObligationReadModel, OperatorObligationState};

pub const ATTENTION_PANE_ID: PaneId = PaneId(1);

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AttentionIntent {
    Refresh,
    Cancel { obligation_id: String },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObligationState {
    Pending,
    AwaitingApproval,
    Running,
    RetryScheduled,
    Attention,
    Completed,
    Cancelled,
}

impl ObligationState {
    pub const fn label(self) -> &'static str {
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AuditEventReadModel {
    pub sequence: i64,
    pub obligation_id: String,
    pub occurrence: u32,
    pub event_type: String,
    pub occurred_at: i64,
    pub from_state: Option<ObligationState>,
    pub to_state: ObligationState,
    pub details_json: String,
}

pub fn fixed_workspace() -> Workspace {
    Workspace {
        schema_version: LAYOUT_SCHEMA_VERSION,
        root: DockNode::Tabs {
            id: DockNodeId(1),
            tabs: vec![ATTENTION_PANE_ID],
            active: 0,
        },
        active_pane: ATTENTION_PANE_ID,
        closed_optional_panes: BTreeSet::new(),
        next_node_id: 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_workspace_has_one_stable_valid_pane() {
        let workspace = fixed_workspace();
        workspace.validate().unwrap();
        assert_eq!(workspace.active_pane, ATTENTION_PANE_ID);
        assert_eq!(
            workspace.root,
            DockNode::Tabs {
                id: DockNodeId(1),
                tabs: vec![ATTENTION_PANE_ID],
                active: 0,
            }
        );
    }
}
