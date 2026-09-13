//! Typed projection of the existing host verifier, not a second GitHub verifier.
use super::*;

#[derive(Deserialize)]
struct CiRun {
    id: u64,
    url: String,
    attempt: u64,
    head: String,
    status: String,
    conclusion: Option<String>,
}
#[derive(Deserialize)]
struct CiJob {
    id: u64,
    url: String,
    run_id: u64,
    attempt: u64,
    head: String,
    name: String,
    status: String,
    conclusion: Option<String>,
}
#[derive(Deserialize)]
struct CiObservation {
    state: String,
    head: String,
    run: CiRun,
    jobs: Vec<CiJob>,
}
#[derive(Deserialize)]
struct DeliveryReceipt {
    repo: String,
    base: String,
    branch: String,
    pr: u64,
    head: String,
    head_tree: String,
    merge_commit: String,
    merge_tree: String,
    merged: bool,
    tree_equal: bool,
    post_merge_verified: bool,
    pre_merge_ci: CiObservation,
    post_merge_ci: CiObservation,
}
impl CiObservation {
    fn valid(&self, head: &str, repo: &str) -> bool {
        let run = &self.run;
        let url = format!("https://github.com/{repo}/actions/runs/{}", run.id);
        self.state == "success"
            && self.head == head
            && run.id > 0
            && run.attempt > 0
            && run.url == url
            && run.head == head
            && run.status == "completed"
            && run.conclusion.as_deref() == Some("success")
            && self.jobs.len() <= 100
            && self
                .jobs
                .iter()
                .filter(|j| j.name == "Fresh-checkout verification")
                .count()
                == 1
            && self.jobs.iter().all(|j| {
                j.id > 0
                    && j.run_id == run.id
                    && j.attempt == run.attempt
                    && j.head == head
                    && (j.url == format!("{url}/job/{}", j.id)
                        || j.url
                            == format!("https://github.com/{repo}/runs/{}/jobs/{}", run.id, j.id))
            })
            && self
                .jobs
                .iter()
                .filter(|j| j.name == "Fresh-checkout verification")
                .all(|j| j.status == "completed" && j.conclusion.as_deref() == Some("success"))
    }
}
impl EngineeringRuntime {
    pub(super) fn validate_delivery_receipt(&self, value: &Value) -> RuntimeResult<()> {
        let receipt: DeliveryReceipt = serde_json::from_value(value.clone())?;
        let scope = self
            .profile
            .github_delivery
            .as_ref()
            .ok_or("delivery not enabled")?;
        let identity = |s: &str| s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit());
        if receipt.repo != scope.repo
            || receipt.base != scope.base
            || receipt.branch != scope.branch
            || receipt.pr == 0
            || !receipt.merged
            || !receipt.tree_equal
            || !receipt.post_merge_verified
            || ![
                &receipt.head,
                &receipt.head_tree,
                &receipt.merge_commit,
                &receipt.merge_tree,
            ]
            .iter()
            .all(|s| identity(s))
            || receipt.head_tree != receipt.merge_tree
            || !receipt.pre_merge_ci.valid(&receipt.head, &scope.repo)
            || !receipt
                .post_merge_ci
                .valid(&receipt.merge_commit, &scope.repo)
        {
            return Err(
                "delivery receipt lacks exact reviewed/merged trees and attributable successful CI"
                    .into(),
            );
        }
        Ok(())
    }
    pub(super) fn validate_cleanup_scope(
        &self,
        state: &EngineeringOutcomeSnapshot,
        args: &Value,
    ) -> RuntimeResult<()> {
        if state
            .executions
            .iter()
            .any(|e| e.role == EngineeringRole::Worker && !e.cessation_verified)
        {
            return Err("cleanup blocked by live or uncertain workspace execution".into());
        }
        let op = state
            .delivery_operations
            .iter()
            .rev()
            .find(|op| {
                op.operation == "merge"
                    && op.contract_revision == state.contract_revision
                    && op.post_merge_verified
            })
            .ok_or("cleanup requires a completed merge receipt")?;
        let receipt: Value = serde_json::from_slice(
            &self.evidence(
                op.evidence_digest
                    .as_deref()
                    .ok_or("merge receipt missing")?,
            )?,
        )?;
        self.validate_delivery_receipt(&receipt)?;
        if args["pr"] != receipt["pr"]
            || args["head"] != receipt["head"]
            || args["tree"] != receipt["head_tree"]
            || args["merge_commit"] != receipt["merge_commit"]
        {
            return Err("cleanup arguments do not bind the retained reviewed merge".into());
        }
        Ok(())
    }
}
