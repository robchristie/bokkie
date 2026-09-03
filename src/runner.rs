use std::collections::VecDeque;

use crate::{Claim, Completion, Store, StoreError, UnixClock};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunResult {
    pub completion: Completion,
}

/// External work boundary. Implementations must not receive a database transaction.
pub trait Runner {
    fn execute(&mut self, claim: &Claim) -> RunResult;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FakeOutcome {
    Succeed {
        evidence: Option<String>,
    },
    Fail {
        retryable: bool,
        error: String,
        evidence: Option<String>,
    },
}

/// A deterministic runner driven by a caller-provided sequence.
#[derive(Debug)]
pub struct FakeRunner {
    outcomes: VecDeque<FakeOutcome>,
    invocations: Vec<Claim>,
}

impl FakeRunner {
    pub fn new(outcomes: impl IntoIterator<Item = FakeOutcome>) -> Self {
        Self {
            outcomes: outcomes.into_iter().collect(),
            invocations: Vec::new(),
        }
    }

    pub fn invocations(&self) -> &[Claim] {
        &self.invocations
    }
}

impl Runner for FakeRunner {
    fn execute(&mut self, claim: &Claim) -> RunResult {
        self.invocations.push(claim.clone());
        let outcome = self.outcomes.pop_front().unwrap_or(FakeOutcome::Succeed {
            evidence: Some("fake runner default success".to_owned()),
        });
        RunResult {
            completion: match outcome {
                FakeOutcome::Succeed { evidence } => Completion::Succeeded { evidence },
                FakeOutcome::Fail {
                    retryable,
                    error,
                    evidence,
                } => Completion::Failed {
                    retryable,
                    error,
                    evidence,
                },
            },
        }
    }
}

/// Execute one already-durable claim, then reconcile its result transactionally.
pub fn run_one(
    store: &mut Store,
    runner: &mut impl Runner,
    clock: &impl UnixClock,
    claim: &Claim,
) -> Result<(), StoreError> {
    let result = runner.execute(claim);
    store.complete(claim, result.completion, clock.now())
}
