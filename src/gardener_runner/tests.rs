use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

use tempfile::TempDir;

use super::*;
use crate::{
    ApprovalDecision, CANONICAL_DEFAULT_BRANCH, ManualClock, NewObligation,
    NewRepositoryRegistration, ObligationState, Recurrence, RetryPolicy,
};

const GOAL: &str = "Add a durable gardener marker file and test it.";

struct Fixture {
    root: TempDir,
    checkout: PathBuf,
    origin: PathBuf,
    worktrees: PathBuf,
    database: PathBuf,
    codex: PathBuf,
    codex_log: PathBuf,
    gh: PathBuf,
    gh_log: PathBuf,
    clock: ManualClock,
}

impl Fixture {
    fn new(verdict: &str, mismatched_head: bool) -> Self {
        let root = tempfile::tempdir().unwrap();
        let origin = root.path().join("origin.git");
        let checkout = root.path().join("checkout");
        let worktrees = root.path().join("worktrees");
        fs::create_dir(&worktrees).unwrap();
        git(
            root.path(),
            [
                "init",
                "--bare",
                "--initial-branch=main",
                origin.to_str().unwrap(),
            ],
        );
        git(
            root.path(),
            ["init", "--initial-branch=main", checkout.to_str().unwrap()],
        );
        git(&checkout, ["config", "user.name", "Gardener Test"]);
        git(
            &checkout,
            ["config", "user.email", "gardener@example.invalid"],
        );
        fs::write(checkout.join("AGENTS.md"), "# Test guidance\n").unwrap();
        fs::write(checkout.join("README.md"), "# Test repository\n").unwrap();
        fs::create_dir_all(checkout.join("docs/plans")).unwrap();
        fs::write(checkout.join("docs/plans/test.md"), "# Plan\n").unwrap();
        git(&checkout, ["add", "."]);
        git(&checkout, ["commit", "-m", "Initial"]);
        git(
            &checkout,
            ["remote", "add", "origin", origin.to_str().unwrap()],
        );
        git(&checkout, ["push", "-u", "origin", "main"]);

        let codex_log = root.path().join("codex.jsonl");
        let codex_count = root.path().join("codex.count");
        let codex = root.path().join("fake-codex.py");
        let mismatch = if mismatched_head {
            "a".repeat(40)
        } else {
            String::new()
        };
        let codex_script = format!(
            r#"#!/usr/bin/env python3
import json
import pathlib
import subprocess
import sys

if sys.argv[1:] != ["app-server"]:
    raise SystemExit("expected exactly the app-server subcommand")

log = pathlib.Path({log})
count_path = pathlib.Path({count})
verdict = {verdict}
mismatch = {mismatch}
thread_id = None
turn_id = None

for raw in sys.stdin:
    with log.open("a") as output:
        output.write(raw)
    request = json.loads(raw)
    method = request.get("method")
    if method == "initialize":
        print(json.dumps({{"id": request["id"], "result": {{}}}}), flush=True)
    elif method == "thread/start":
        count = int(count_path.read_text()) + 1 if count_path.exists() else 1
        count_path.write_text(str(count))
        thread_id = "thread-" + str(count)
        print(json.dumps({{"id": request["id"], "result": {{"thread": {{"id": thread_id}}}}}}), flush=True)
    elif method == "turn/start":
        turn_id = "turn-" + thread_id.split("-")[-1]
        print(json.dumps({{"id": request["id"], "result": {{"turn": {{"id": turn_id}}}}}}), flush=True)
        prompt = request["params"]["input"][0]["text"]
        sandbox = request["params"]["sandboxPolicy"]["type"]
        if sandbox == "workspaceWrite":
            pathlib.Path.cwd().joinpath("gardened.txt").write_text("implemented by fake Codex\n")
            message = {{"summary": "implemented and checked"}}
        elif "independent, read-only verifier" in prompt:
            head = mismatch or subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip()
            message = {{"verdict": verdict, "head": head, "summary": "fake exact-head verification"}}
        else:
            message = {{"summary": "found one bounded improvement", "proposed_goal_prompts": [{goal}]}}
        text = json.dumps(message)
        print(json.dumps({{"method": "item/completed", "params": {{"threadId": thread_id, "turnId": turn_id, "item": {{"type": "agentMessage", "id": "final", "text": text}}}}}}), flush=True)
        print(json.dumps({{"method": "turn/completed", "params": {{"threadId": thread_id, "turn": {{"id": turn_id, "status": "completed", "items": []}}}}}}), flush=True)
"#,
            log = serde_json::to_string(codex_log.to_str().unwrap()).unwrap(),
            count = serde_json::to_string(codex_count.to_str().unwrap()).unwrap(),
            verdict = serde_json::to_string(verdict).unwrap(),
            mismatch = serde_json::to_string(&mismatch).unwrap(),
            goal = serde_json::to_string(GOAL).unwrap(),
        );
        write_executable(&codex, &codex_script);

        let gh_log = root.path().join("gh.log");
        let gh = root.path().join("fake-gh.py");
        let gh_script = format!(
            r#"#!/usr/bin/env python3
import json
import pathlib
import subprocess
import sys

args = sys.argv[1:]
with pathlib.Path({log}).open("a") as output:
    output.write(json.dumps(args) + "\n")
if args[:2] == ["pr", "view"]:
    branch = args[2]
    head = subprocess.check_output(["git", "rev-parse", branch], text=True).strip()
    print(json.dumps({{"number": 42, "url": "https://github.com/robchristie/bokkie/pull/42", "headRefOid": head, "state": "OPEN", "isDraft": False}}))
"#,
            log = serde_json::to_string(gh_log.to_str().unwrap()).unwrap(),
        );
        write_executable(&gh, &gh_script);

        Self {
            database: root.path().join("bokkie.sqlite"),
            root,
            checkout,
            origin,
            worktrees,
            codex,
            codex_log,
            gh,
            gh_log,
            clock: ManualClock::new(1_000),
        }
    }

    fn store(&self) -> Store {
        let mut store = Store::open(&self.database).unwrap();
        store
            .register_gardener_repository(
                NewRepositoryRegistration {
                    repository: CANONICAL_REPOSITORY.to_owned(),
                    default_branch: CANONICAL_DEFAULT_BRANCH.to_owned(),
                    checkout_path: self.checkout.to_str().unwrap().to_owned(),
                    inspection_recurrence: Recurrence::new("0 0 * * *", "UTC").unwrap(),
                    first_inspection_at: self.clock.now(),
                },
                self.clock.now() - 1,
            )
            .unwrap();
        store
    }

    fn config(&self) -> GardenerRuntimeConfig {
        GardenerRuntimeConfig::new(&self.worktrees, &self.codex, "git", &self.gh)
            .with_heartbeat_interval(Duration::from_millis(5))
    }

    fn inspect_and_approve(&self, store: &mut Store) -> Claim {
        let claim = store
            .claim_due_gardener(self.clock.now(), 30, 1)
            .unwrap()
            .remove(0);
        let config = self.config();
        let runner = GardenerRunner::new(&config, 30, &self.clock).unwrap();
        let result = runner.execute(store, &claim);
        assert!(matches!(result.completion, Completion::Succeeded { .. }));
        store
            .complete(&claim, result.completion, self.clock.now())
            .unwrap();

        let fingerprint = crate::proposal_fingerprint(CANONICAL_REPOSITORY, GOAL);
        let proposal = store.gardener_proposal(&fingerprint).unwrap().unwrap();
        assert_eq!(proposal.obligation_state, ObligationState::AwaitingApproval);
        assert!(
            store
                .claim_due_gardener(self.clock.now(), 30, 10)
                .unwrap()
                .is_empty()
        );
        store
            .decide_gardener_proposal(
                &fingerprint,
                ApprovalDecision::Approved,
                "test operator",
                None,
                self.clock.now() + 1,
            )
            .unwrap();
        self.clock.advance(1);
        store
            .claim_due_gardener(self.clock.now(), 30, 1)
            .unwrap()
            .remove(0)
    }
}

#[test]
fn complete_process_flow_persists_exact_identities_and_passes_verification() {
    let fixture = Fixture::new("pass", false);
    let mut store = fixture.store();
    let implementation_claim = fixture.inspect_and_approve(&mut store);

    let config = fixture.config();
    let runner = GardenerRunner::new(&config, 30, &fixture.clock).unwrap();
    let result = runner.execute(&mut store, &implementation_claim);
    assert!(matches!(result.completion, Completion::Succeeded { .. }));
    store
        .complete(
            &implementation_claim,
            result.completion,
            fixture.clock.now(),
        )
        .unwrap();

    let runs = store.gardener_implementation_runs().unwrap();
    assert_eq!(runs.len(), 1);
    let run = &runs[0];
    assert_eq!(
        run.verification_verdict,
        Some(GardenerVerificationVerdict::Pass)
    );
    assert_eq!(run.git_commit, run.pushed_head);
    assert_eq!(run.git_commit, run.pull_request_head);
    assert_eq!(run.pull_request_head, run.verification_reported_head);
    assert_ne!(run.implementation_thread_id, run.verification_thread_id);
    assert_ne!(run.implementation_turn_id, run.verification_turn_id);
    assert_eq!(run.pull_request_number, Some(42));
    assert!(!Path::new(&run.implementation_worktree_path).exists());
    assert!(!Path::new(run.verification_worktree_path.as_ref().unwrap()).exists());
    assert_eq!(
        git_stdout(
            fixture.root.path(),
            [
                "--git-dir",
                fixture.origin.to_str().unwrap(),
                "rev-parse",
                &format!("refs/heads/{}", run.branch),
            ],
        ),
        run.git_commit.as_deref().unwrap()
    );
    assert_eq!(
        store
            .get(&implementation_claim.obligation_id)
            .unwrap()
            .unwrap()
            .state,
        ObligationState::Completed
    );

    let transcript = fs::read_to_string(&fixture.codex_log).unwrap();
    assert!(transcript.lines().count() >= 12);
    assert!(transcript.contains("\"type\":\"readOnly\""));
    assert!(transcript.contains("\"type\":\"workspaceWrite\""));
    assert!(transcript.contains("\"networkAccess\":false"));
    assert!(transcript.contains("exact pull-request head"));
    let gh_log = fs::read_to_string(&fixture.gh_log).unwrap();
    assert!(gh_log.contains("\"create\""));
    assert!(gh_log.contains("\"view\""));
    assert!(gh_log.lines().all(|line| {
        let arguments: Vec<String> = serde_json::from_str(line).unwrap();
        arguments.get(1).is_none_or(|command| command != "merge")
    }));
}

#[test]
fn blocking_verdict_preserves_ready_pr_and_enters_attention() {
    let fixture = Fixture::new("blocking", false);
    let mut store = fixture.store();
    let claim = fixture.inspect_and_approve(&mut store);
    let config = fixture.config();
    let runner = GardenerRunner::new(&config, 30, &fixture.clock).unwrap();
    let result = runner.execute(&mut store, &claim);
    assert!(matches!(
        result.completion,
        Completion::Failed {
            retryable: false,
            ..
        }
    ));
    store
        .complete(&claim, result.completion, fixture.clock.now())
        .unwrap();

    let run = store.gardener_implementation_runs().unwrap().remove(0);
    assert_eq!(
        run.verification_verdict,
        Some(GardenerVerificationVerdict::Blocking)
    );
    assert_eq!(run.pull_request_number, Some(42));
    assert_eq!(
        store.get(&claim.obligation_id).unwrap().unwrap().state,
        ObligationState::Attention
    );
}

#[test]
fn mismatched_reported_head_is_non_retryable_and_never_records_a_verdict() {
    let fixture = Fixture::new("pass", true);
    let mut store = fixture.store();
    let claim = fixture.inspect_and_approve(&mut store);
    let config = fixture.config();
    let runner = GardenerRunner::new(&config, 30, &fixture.clock).unwrap();
    let result = runner.execute(&mut store, &claim);
    assert!(matches!(
        result.completion,
        Completion::Failed {
            retryable: false,
            ..
        }
    ));
    store
        .complete(&claim, result.completion, fixture.clock.now())
        .unwrap();

    let run = store.gardener_implementation_runs().unwrap().remove(0);
    assert_eq!(run.verification_verdict, None);
    assert_eq!(run.pull_request_number, Some(42));
    let obligation = store.get(&claim.obligation_id).unwrap().unwrap();
    assert_eq!(obligation.state, ObligationState::Attention);
    assert!(
        obligation
            .last_error
            .as_deref()
            .unwrap()
            .contains("verification reported head aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
    );
}

#[test]
fn unconfigured_ordinary_claims_cannot_take_gardener_work() {
    let fixture = Fixture::new("pass", false);
    let mut store = fixture.store();
    store
        .create(
            NewObligation {
                id: "ordinary".to_owned(),
                description: "ordinary fake work".to_owned(),
                scheduled_at: fixture.clock.now(),
                recurrence: None,
                approval_required: false,
                retry: RetryPolicy::default(),
            },
            fixture.clock.now() - 1,
        )
        .unwrap();
    let claims = store.claim_due(fixture.clock.now(), 30, 10).unwrap();
    assert_eq!(claims.len(), 1);
    assert_eq!(claims[0].obligation_id, "ordinary");
    let gardener = store
        .claim_due_gardener(fixture.clock.now(), 30, 10)
        .unwrap();
    assert_eq!(gardener.len(), 1);
    assert!(gardener[0].obligation_id.starts_with("gardener:inspect:"));
}

fn write_executable(path: &Path, contents: &str) {
    fs::write(path, contents).unwrap();
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).unwrap();
}

fn git<I, S>(cwd: &Path, arguments: I)
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let output = Command::new("git")
        .args(arguments)
        .current_dir(cwd)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_stdout<I, S>(cwd: &Path, arguments: I) -> String
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let output = Command::new("git")
        .args(arguments)
        .current_dir(cwd)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}
