//! Generated worker-operation interleavings over a real shared SQLite store.

use proptest::{prelude::*, test_runner::RngSeed};
use tempfile::TempDir;

use super::*;
use crate::RetryPolicy;

fn current_claim(store: &Store, claim: &Claim, now: i64) -> bool {
    let obligation = store.get(&claim.obligation_id).unwrap().unwrap();
    obligation.state == ObligationState::Running
        && obligation.occurrence == claim.occurrence
        && obligation.lease_generation == claim.lease_generation
        && obligation.lease_token.as_deref() == Some(claim.lease_token.as_str())
        && obligation
            .lease_expires_at
            .is_some_and(|expiry| expiry > now)
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 16,
        failure_persistence: None,
        rng_seed: RngSeed::Fixed(0xB0_66_1E),
        ..ProptestConfig::default()
    })]

    #[test]
    fn generated_worker_interleavings_preserve_authority_and_liveness(
        operations in prop::collection::vec((0_usize..2, 0_u8..9, -6_i64..16), 40..100),
    ) {
        let directory = TempDir::new().unwrap();
        let database = directory.path().join("interleavings.sqlite");
        let mut stores = [Store::open(&database).unwrap(), Store::open(&database).unwrap()];
        let mut claims: [Vec<Claim>; 2] = [Vec::new(), Vec::new()];
        let mut now = 1_000_i64;
        let mut identities = Vec::new();
        for (index, (worker, operation, clock_change)) in operations.into_iter().enumerate() {
            now = now.saturating_add(clock_change);
            // Keep live work in the population after generated terminal actions.
            if index % 10 == 0 {
                let id = format!("model-{index}");
                stores[worker].create(NewObligation {
                    id: id.clone(), description: "generated interleaving".to_owned(),
                    scheduled_at: now, recurrence: None, approval_required: index % 20 == 0,
                    retry: RetryPolicy { max_attempts: 5, base_delay_seconds: 1, max_delay_seconds: 8 },
                }, now).unwrap();
                identities.push(id);
            }
            let id = &identities[index % identities.len()];
            match operation {
                0 => { let _ = stores[worker].decide_approval(id, ApprovalDecision::Approved, "model", None, now); }
                1 => {
                    let acquired = stores[worker].claim_due(now, 12, 1).unwrap();
                    for claim in acquired {
                        // A runner can only receive an identity whose intent is already durable
                        // and independently visible from the other connection.
                        let attempts = stores[1 - worker].attempts(&claim.obligation_id).unwrap();
                        prop_assert!(attempts.iter().any(|attempt| attempt.lease_token == claim.lease_token
                            && attempt.lease_generation == claim.lease_generation
                            && attempt.attempt_number == claim.attempt_number
                            && attempt.outcome == AttemptOutcome::Running));
                        claims[worker].push(claim);
                    }
                }
                2..=4 => {
                    if !claims[worker].is_empty() {
                        let claim = &claims[worker][index % claims[worker].len()];
                        let was_current = current_claim(&stores[worker], claim, now);
                        let before = stores[worker].events(&claim.obligation_id).unwrap();
                        let result = if operation == 2 {
                            let prior = stores[worker].get(&claim.obligation_id).unwrap().unwrap().lease_expires_at;
                            stores[worker].renew_lease(claim, now, 12).map(|expiry| {
                                assert_eq!(expiry, prior.unwrap().max(now.saturating_add(12)));
                            })
                        } else {
                            let completion = if operation == 3 {
                                Completion::Succeeded { evidence: Some("generated success".to_owned()) }
                            } else {
                                Completion::Failed { disposition: FailureDisposition::NeedsReconciliation,
                                    error: "ambiguous generated effect".to_owned(), evidence: None }
                            };
                            stores[worker].complete(claim, completion, now).map(|_| ())
                        };
                        if was_current {
                            prop_assert!(result.is_ok(), "current claim rejected: {result:?}");
                            if operation == 4 {
                                let obligation = stores[worker].get(&claim.obligation_id).unwrap().unwrap();
                                prop_assert_eq!(obligation.state, ObligationState::Attention);
                                prop_assert_eq!(obligation.failure_disposition, Some(FailureDisposition::NeedsReconciliation));
                            }
                        } else {
                            prop_assert!(matches!(result, Err(StoreError::Fenced)), "stale claim result: {result:?}");
                            prop_assert_eq!(stores[worker].events(&claim.obligation_id).unwrap(), before);
                        }
                    }
                }
                5 => { stores[worker].recover_expired_leases(now).unwrap(); }
                6 => { let _ = stores[worker].cancel(id, now); }
                7 => { let _ = stores[worker].retry_attention(id, now); }
                8 => { let _ = stores[worker].decide_approval(id, ApprovalDecision::Rejected, "model", None, now); }
                _ => unreachable!(),
            }
            for id in &identities {
                let obligation = stores[1 - worker].get(id).unwrap().unwrap();
                let events = stores[1 - worker].events(id).unwrap();
                prop_assert_eq!(events.last().unwrap().to_state, obligation.state);
                let current = claims.iter().flatten().filter(|claim|
                    claim.obligation_id == *id && current_claim(&stores[1 - worker], claim, now)
                ).count();
                prop_assert!(current <= 1, "multiple valid claims for {id}");
                if !obligation.state.is_terminal() {
                    prop_assert!(obligation.next_wake_at.is_some()
                        || (obligation.state == ObligationState::Running
                            && obligation.lease_token.is_some() && obligation.lease_expires_at.is_some())
                        || matches!(obligation.state, ObligationState::AwaitingApproval | ObligationState::Attention));
                }
            }
        }
    }
}
