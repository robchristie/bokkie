//! Stable identities for independently supervised execution capacity.

use std::fmt;

use serde::{Deserialize, Serialize};

/// A bounded class of work with its own claim and execution capacity.
///
/// `Outbox` is an explicit contract extension point only. The current service
/// does not start an outbox worker or claim outbox work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionLane {
    Ordinary,
    Gardener,
    Outbox,
}

impl fmt::Display for ExecutionLane {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Ordinary => "ordinary",
            Self::Gardener => "gardener",
            Self::Outbox => "outbox",
        })
    }
}
