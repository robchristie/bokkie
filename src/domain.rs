use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};

use crate::recurrence::Recurrence;

/// The complete set of durable obligation projection states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Cancelled)
    }
}

impl fmt::Display for ObligationState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Pending => "pending",
            Self::AwaitingApproval => "awaiting_approval",
            Self::Running => "running",
            Self::RetryScheduled => "retry_scheduled",
            Self::Attention => "attention",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
        })
    }
}

impl FromStr for ObligationState {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "pending" => Ok(Self::Pending),
            "awaiting_approval" => Ok(Self::AwaitingApproval),
            "running" => Ok(Self::Running),
            "retry_scheduled" => Ok(Self::RetryScheduled),
            "attention" => Ok(Self::Attention),
            "completed" => Ok(Self::Completed),
            "cancelled" => Ok(Self::Cancelled),
            other => Err(format!("unknown obligation state {other:?}")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    Approved,
    Rejected,
}

impl fmt::Display for ApprovalDecision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Approved => "approved",
            Self::Rejected => "rejected",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptOutcome {
    Running,
    Succeeded,
    Failed,
    LeaseExpired,
}

impl fmt::Display for AttemptOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::LeaseExpired => "lease_expired",
        })
    }
}

impl FromStr for AttemptOutcome {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "running" => Ok(Self::Running),
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            "lease_expired" => Ok(Self::LeaseExpired),
            other => Err(format!("unknown attempt outcome {other:?}")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub base_delay_seconds: i64,
    pub max_delay_seconds: i64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            base_delay_seconds: 30,
            max_delay_seconds: 3_600,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewObligation {
    pub id: String,
    pub description: String,
    pub scheduled_at: i64,
    pub recurrence: Option<Recurrence>,
    pub approval_required: bool,
    pub retry: RetryPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Obligation {
    pub id: String,
    pub description: String,
    pub state: ObligationState,
    pub occurrence: u32,
    pub scheduled_at: i64,
    pub next_wake_at: Option<i64>,
    pub recurrence_cron: Option<String>,
    pub recurrence_timezone: Option<String>,
    pub approval_required: bool,
    pub attempts_made: u32,
    pub max_attempts: u32,
    pub retry_base_seconds: i64,
    pub retry_max_seconds: i64,
    pub lease_token: Option<String>,
    pub lease_generation: u64,
    pub lease_expires_at: Option<i64>,
    pub last_error: Option<String>,
    pub last_evidence: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claim {
    pub obligation_id: String,
    pub occurrence: u32,
    pub attempt_number: u32,
    pub lease_token: String,
    pub lease_generation: u64,
    pub lease_expires_at: i64,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Completion {
    Succeeded {
        evidence: Option<String>,
    },
    Failed {
        retryable: bool,
        error: String,
        evidence: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attempt {
    pub id: i64,
    pub obligation_id: String,
    pub occurrence: u32,
    pub attempt_number: u32,
    pub lease_generation: u64,
    pub lease_token: String,
    pub claimed_at: i64,
    pub completed_at: Option<i64>,
    pub outcome: AttemptOutcome,
    pub retryable: Option<bool>,
    pub error: Option<String>,
    pub evidence: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditEvent {
    pub sequence: i64,
    pub obligation_id: String,
    pub occurrence: u32,
    pub event_type: String,
    pub occurred_at: i64,
    pub from_state: Option<ObligationState>,
    pub to_state: ObligationState,
    pub details_json: String,
}
