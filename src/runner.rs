use std::collections::VecDeque;

use crate::{
    Claim, Completion, FailureDisposition, MAX_COMPLETION_ERROR_CHARS,
    MAX_COMPLETION_EVIDENCE_CHARS, Store, StoreError, UnixClock,
};

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
        disposition: FailureDisposition,
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
                FakeOutcome::Succeed { evidence } => Completion::Succeeded {
                    evidence: evidence
                        .map(|value| bounded_runtime_text(value, MAX_COMPLETION_EVIDENCE_CHARS)),
                },
                FakeOutcome::Fail {
                    disposition,
                    error,
                    evidence,
                } => Completion::Failed {
                    disposition,
                    error: bounded_runtime_text(error, MAX_COMPLETION_ERROR_CHARS),
                    evidence: evidence
                        .map(|value| bounded_runtime_text(value, MAX_COMPLETION_EVIDENCE_CHARS)),
                },
            },
        }
    }
}

/// Bound adapter-created diagnostic text before it reaches the authoritative
/// Store boundary. Invalid NUL is replaced visibly and Unicode is truncated by
/// scalar value, never through the middle of an encoded character.
pub(crate) fn bounded_runtime_text(value: String, max_chars: usize) -> String {
    value
        .chars()
        .map(|character| {
            if character == '\0' {
                '\u{fffd}'
            } else {
                character
            }
        })
        .take(max_chars)
        .collect()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_evidence_is_unicode_safe_bounded_and_nul_free() {
        let claim = Claim {
            obligation_id: "bounded".to_owned(),
            occurrence: 1,
            attempt_number: 1,
            lease_token: "lease".to_owned(),
            lease_generation: 1,
            lease_expires_at: 100,
            description: "bounded runtime evidence".to_owned(),
        };
        let mut runner = FakeRunner::new([FakeOutcome::Fail {
            disposition: FailureDisposition::NeedsReconciliation,
            error: format!("\0{}", "界".repeat(MAX_COMPLETION_ERROR_CHARS + 20)),
            evidence: Some(format!(
                "\0{}",
                "証".repeat(MAX_COMPLETION_EVIDENCE_CHARS + 20)
            )),
        }]);
        let result = runner.execute(&claim);
        let Completion::Failed {
            disposition,
            error,
            evidence,
        } = result.completion
        else {
            panic!("fake failure returned success");
        };
        assert_eq!(disposition, FailureDisposition::NeedsReconciliation);
        assert_eq!(error.chars().count(), MAX_COMPLETION_ERROR_CHARS);
        assert!(!error.contains('\0'));
        let evidence = evidence.unwrap();
        assert_eq!(evidence.chars().count(), MAX_COMPLETION_EVIDENCE_CHARS);
        assert!(!evidence.contains('\0'));
    }
}
