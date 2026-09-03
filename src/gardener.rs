use sha2::{Digest, Sha256};

use crate::{ApprovalDecision, ObligationState, Recurrence};

pub const CANONICAL_REPOSITORY: &str = "robchristie/bokkie";
pub const CANONICAL_DEFAULT_BRANCH: &str = "main";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewRepositoryRegistration {
    pub repository: String,
    pub default_branch: String,
    pub checkout_path: String,
    pub inspection_recurrence: Recurrence,
    pub first_inspection_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RepositoryRegistration {
    pub repository: String,
    pub default_branch: String,
    pub checkout_path: String,
    pub inspection_cron: String,
    pub inspection_timezone: String,
    pub first_inspection_at: i64,
    pub inspection_obligation_id: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GardenerObligationKind {
    Inspection,
    Implementation,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NewGardenerInspection {
    pub id: String,
    pub source_commit: String,
    pub worktree_path: String,
    pub prompt_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GardenerInspection {
    pub id: String,
    pub repository: String,
    pub obligation_id: String,
    pub occurrence: u32,
    pub lease_generation: u64,
    pub source_commit: String,
    pub worktree_path: String,
    pub prompt_digest: String,
    pub codex_thread_id: Option<String>,
    pub codex_turn_id: Option<String>,
    pub result_json: Option<String>,
    pub started_at: i64,
    pub completed_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct InspectionResult {
    pub summary: String,
    pub proposed_goal_prompts: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Proposal {
    pub fingerprint: String,
    pub repository: String,
    pub prompt: String,
    pub implementation_obligation_id: String,
    pub obligation_state: ObligationState,
    pub approval_decision: Option<ApprovalDecision>,
    pub observation_count: u64,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ProposalObservation {
    pub id: i64,
    pub proposal_fingerprint: String,
    pub inspection_id: String,
    pub source_commit: String,
    pub observed_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GardenerEvent {
    pub sequence: i64,
    pub repository: String,
    pub inspection_id: Option<String>,
    pub proposal_fingerprint: Option<String>,
    pub event_type: String,
    pub occurred_at: i64,
    pub details_json: String,
}

/// Canonicalise goal content before storing, approving, and fingerprinting it.
///
/// Line endings become LF, trailing horizontal whitespace is removed from every
/// line, and surrounding blank/whitespace-only lines are discarded. Internal
/// indentation and blank lines remain significant.
pub fn normalise_goal_prompt(prompt: &str) -> String {
    let line_endings = prompt.replace("\r\n", "\n").replace('\r', "\n");
    let lines = line_endings
        .lines()
        .map(|line| line.trim_end_matches([' ', '\t']))
        .collect::<Vec<_>>();
    let first = lines.iter().position(|line| !line.trim().is_empty());
    let last = lines.iter().rposition(|line| !line.trim().is_empty());
    match (first, last) {
        (Some(first), Some(last)) => lines[first..=last].join("\n"),
        _ => String::new(),
    }
}

/// Stable content identity. Source revisions deliberately do not participate.
pub fn proposal_fingerprint(repository: &str, normalised_prompt: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(repository.as_bytes());
    digest.update([0]);
    digest.update(normalised_prompt.as_bytes());
    format!("{:x}", digest.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_normalisation_is_stable_but_preserves_internal_content() {
        let prompt = "\r\n  First line  \r\n\tsecond line\t\r\n\r\n";
        assert_eq!(normalise_goal_prompt(prompt), "  First line\n\tsecond line");
        assert_eq!(
            proposal_fingerprint(CANONICAL_REPOSITORY, &normalise_goal_prompt(prompt)),
            proposal_fingerprint(CANONICAL_REPOSITORY, "  First line\n\tsecond line")
        );
    }
}
