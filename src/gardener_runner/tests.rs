use std::{
    fs,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

use tempfile::TempDir;

use super::*;
use crate::{
    ApprovalDecision, CANONICAL_DEFAULT_BRANCH, GardenerPublicationState, ManualClock,
    NewObligation, NewRepositoryRegistration, ObligationState, Recurrence, RetryPolicy,
};

const GOAL: &str = "Add a durable gardener marker file and test it.";
const CANONICAL_HTTPS_URL: &str = "https://github.com/robchristie/bokkie.git";

struct Fixture {
    root: TempDir,
    checkout: PathBuf,
    origin: PathBuf,
    worktrees: PathBuf,
    database: PathBuf,
    codex: PathBuf,
    codex_log: PathBuf,
    git_executable: PathBuf,
    git_log: PathBuf,
    gh: PathBuf,
    gh_log: PathBuf,
    github_public_observer: PathBuf,
    github_public_observer_log: PathBuf,
    candidate_sandbox: PathBuf,
    candidate_check: PathBuf,
    clock: ManualClock,
}

impl Fixture {
    fn new(verdict: &str, mismatched_head: bool, changed_pr_head: bool) -> Self {
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
        git(
            &checkout,
            ["remote", "set-url", "origin", CANONICAL_HTTPS_URL],
        );
        let git_log = root.path().join("git.log");
        let git_executable = local_git_transport(root.path(), &origin, &git_log);

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

if sys.argv[1:] == ["--version"]:
    print("fake-codex 1.0")
    raise SystemExit(0)
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
            message = {{"summary": "implemented and checked", "changed_paths": ["gardened.txt"], "checks": ["fake check passed"]}}
        elif "independent, read-only verifier" in prompt:
            head = mismatch or subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip()
            message = {{"verdict": verdict, "head": head, "summary": "fake exact-head verification", "blocking_findings": [], "validation": ["fake exact-head check"]}}
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
if args == ["--version"]:
    print("fake-gh 1.0")
    raise SystemExit(0)
log = pathlib.Path({log})
draft_path = pathlib.Path({draft})
with log.open("a") as output:
    output.write(json.dumps(args) + "\n")
if args[:2] == ["pr", "create"]:
    draft_path.write_text("draft")
elif args[:2] == ["pr", "ready"]:
    draft_path.unlink(missing_ok=True)
"#,
            log = serde_json::to_string(gh_log.to_str().unwrap()).unwrap(),
            draft = serde_json::to_string(root.path().join("gh.draft").to_str().unwrap()).unwrap(),
        );
        write_executable(&gh, &gh_script);

        let observer_count = root.path().join("observer.count");
        let github_public_observer_log = root.path().join("observer.log");
        let github_public_observer = root.path().join("fake-curl.py");
        let observer_script = format!(
            r#"#!/usr/bin/env python3
import json
import pathlib
import subprocess
import sys
import urllib.parse

args = sys.argv[1:]
if args == ["--version"]:
    print("fake-curl 1.0")
    raise SystemExit(0)
count_path = pathlib.Path({count})
log = pathlib.Path({log})
draft_path = pathlib.Path({draft})
with log.open("a") as output:
    output.write(json.dumps(args) + "\n")
count = int(count_path.read_text()) + 1 if count_path.exists() else 1
count_path.write_text(str(count))
query = urllib.parse.parse_qs(urllib.parse.urlparse(args[-1]).query)
branch = query["head"][0].split(":", 1)[1]
head = subprocess.check_output(["git", "rev-parse", branch], text=True).strip()
if {changed_pr_head} and count >= 2:
    head = "b" * 40
base = subprocess.check_output(["git", "rev-parse", "main"], text=True).strip()
print(json.dumps([{{
    "number": 42,
    "html_url": "https://github.com/robchristie/bokkie/pull/42",
    "state": "open",
    "draft": draft_path.exists(),
    "head": {{"ref": branch, "sha": head, "repo": {{"full_name": "robchristie/bokkie"}}}},
    "base": {{"ref": "main", "sha": base, "repo": {{"full_name": "robchristie/bokkie"}}}}
}}]))
"#,
            count = serde_json::to_string(observer_count.to_str().unwrap()).unwrap(),
            log = serde_json::to_string(github_public_observer_log.to_str().unwrap()).unwrap(),
            draft = serde_json::to_string(root.path().join("gh.draft").to_str().unwrap()).unwrap(),
            changed_pr_head = if changed_pr_head { "True" } else { "False" },
        );
        write_executable(&github_public_observer, &observer_script);

        let candidate_sandbox = root.path().join("fake-bwrap.py");
        write_executable(
            &candidate_sandbox,
            r#"#!/usr/bin/env python3
import os
import subprocess
import sys

args = sys.argv[1:]
if args == ["--version"]:
    print("fake-bwrap 1.0")
    raise SystemExit(0)
mounts = {}
child_env = {}
child_cwd = None
index = 0
while index < len(args):
    argument = args[index]
    if argument in ("--die-with-parent", "--new-session", "--unshare-all", "--clearenv"):
        index += 1
    elif argument in ("--proc", "--dev", "--tmpfs", "--dir"):
        index += 2
    elif argument in ("--bind", "--ro-bind"):
        mounts[args[index + 2]] = args[index + 1]
        index += 3
    elif argument == "--symlink":
        index += 3
    elif argument == "--setenv":
        child_env[args[index + 1]] = args[index + 2]
        index += 3
    elif argument == "--chdir":
        child_cwd = args[index + 1]
        index += 2
    else:
        break
command = mounts.get(args[index], args[index])
cwd = mounts.get(child_cwd, child_cwd)
raise SystemExit(subprocess.run([command, *args[index + 1:]], cwd=cwd, env=child_env).returncode)
"#,
        );

        let candidate_check = root.path().join("fake-check.sh");
        write_executable(
            &candidate_check,
            "#!/bin/sh\nif [ \"$1\" = --version ]; then echo 'fake-check 1.0'; exit 0; fi\nexit 0\n",
        );

        Self {
            database: root.path().join("bokkie.sqlite"),
            root,
            checkout,
            origin,
            worktrees,
            codex,
            codex_log,
            git_executable,
            git_log,
            gh,
            gh_log,
            github_public_observer,
            github_public_observer_log,
            candidate_sandbox,
            candidate_check,
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
        GardenerRuntimeConfig::new(&self.worktrees, &self.codex, &self.git_executable, &self.gh)
            .with_heartbeat_interval(Duration::from_millis(5))
            .with_github_public_observer(&self.github_public_observer)
            .with_candidate_sandbox(&self.candidate_sandbox)
            .with_github_credential(GitHubCredential::new("fake-test-token").unwrap())
            .with_candidate_checks(&self.candidate_check, [["check"]])
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
    let fixture = Fixture::new("pass", false, false);
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
    assert_eq!(run.publication_state, GardenerPublicationState::Ready);
    assert!(run.pull_request_ready_at.is_some());
    let manifest = store
        .gardener_reproducibility_manifest(&run.id)
        .unwrap()
        .unwrap();
    assert_eq!(manifest.source_commit, run.source_commit);
    assert!(manifest.bokkie_build.contains("\"sha256\""));
    assert!(manifest.executable_manifest_json.contains("fake-codex 1.0"));
    let qualification = store
        .gardener_candidate_qualification(&run.id)
        .unwrap()
        .unwrap();
    assert_eq!(qualification.head, run.git_commit.as_deref().unwrap());
    assert!(qualification.checks_json.contains("\"kind\":\"passed\""));
    let events = store.gardener_run_events(&run.id).unwrap();
    let sequence = |event_type: &str| {
        events
            .iter()
            .position(|event| event.event_type == event_type)
            .unwrap()
    };
    assert!(sequence("candidate_qualified") < sequence("push_observed"));
    assert!(sequence("pull_request_draft_recorded") < sequence("verification_started"));
    assert!(sequence("pull_request_ready_requested") < sequence("pull_request_ready_recorded"));
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
    assert!(gh_log.contains("\"--draft\""));
    assert!(gh_log.contains("\"ready\""));
    assert!(!gh_log.contains("\"view\""));
    assert!(gh_log.lines().all(|line| {
        let arguments: Vec<String> = serde_json::from_str(line).unwrap();
        arguments.get(1).is_none_or(|command| command != "merge")
    }));
    let observer_log = fs::read_to_string(&fixture.github_public_observer_log).unwrap();
    assert!(observer_log.contains("https://api.github.com/repos/robchristie/bokkie/pulls"));
}

#[test]
fn blocking_verdict_preserves_draft_pr_and_enters_attention() {
    let fixture = Fixture::new("blocking", false, false);
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
    assert_eq!(run.publication_state, GardenerPublicationState::Draft);
    assert!(run.pull_request_ready_at.is_none());
    assert_eq!(
        store.get(&claim.obligation_id).unwrap().unwrap().state,
        ObligationState::Attention
    );
}

#[test]
fn failed_candidate_check_is_durable_and_prevents_publication() {
    let fixture = Fixture::new("pass", false, false);
    let mut store = fixture.store();
    let claim = fixture.inspect_and_approve(&mut store);
    write_executable(
        &fixture.candidate_check,
        "#!/bin/sh\nif [ \"$1\" = --version ]; then echo 'fake-check 2.0'; exit 0; fi\necho 'bounded failure' >&2\nexit 7\n",
    );
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
    let run = store.gardener_implementation_runs().unwrap().remove(0);
    assert!(run.git_commit.is_some());
    assert!(run.pushed_head.is_none());
    assert!(run.pull_request_number.is_none());
    assert_eq!(run.publication_state, GardenerPublicationState::NotCreated);
    let qualification = store
        .gardener_candidate_qualification(&run.id)
        .unwrap()
        .unwrap();
    assert!(qualification.checks_json.contains("\"kind\":\"failed\""));
    assert!(
        !fs::read_to_string(&fixture.git_log)
            .unwrap()
            .lines()
            .any(|line| line.starts_with("push "))
    );
    assert!(!fixture.gh_log.exists());
}

#[test]
fn mismatched_reported_head_is_non_retryable_and_never_records_a_verdict() {
    let fixture = Fixture::new("pass", true, false);
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
fn changed_pr_head_on_final_observation_enters_attention_without_a_verdict() {
    let fixture = Fixture::new("pass", false, true);
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
    assert_eq!(run.pull_request_head, run.verification_head);
    let obligation = store.get(&claim.obligation_id).unwrap().unwrap();
    assert_eq!(obligation.state, ObligationState::Attention);
    assert!(
        obligation
            .last_error
            .as_deref()
            .unwrap()
            .contains("observed bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
    );
    let observations = fs::read_to_string(&fixture.github_public_observer_log)
        .unwrap()
        .lines()
        .count();
    assert_eq!(observations, 2);
}

#[test]
fn unconfigured_ordinary_claims_cannot_take_gardener_work() {
    let fixture = Fixture::new("pass", false, false);
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

#[test]
fn inspection_rejects_a_noncanonical_effective_origin_before_remote_or_codex_work() {
    let fixture = Fixture::new("pass", false, false);
    let noncanonical = fixture.root.path().join("noncanonical.git");
    git(
        fixture.root.path(),
        [
            "init",
            "--bare",
            "--initial-branch=main",
            noncanonical.to_str().unwrap(),
        ],
    );
    git(
        &fixture.checkout,
        ["remote", "set-url", "origin", "hidden-destination:"],
    );
    git(
        &fixture.checkout,
        [
            "config",
            &format!("url.file://{}.insteadOf", noncanonical.display()),
            "hidden-destination:",
        ],
    );
    fs::write(&fixture.git_log, "").unwrap();
    let mut store = fixture.store();
    let claim = store
        .claim_due_gardener(fixture.clock.now(), 30, 1)
        .unwrap()
        .remove(0);
    let config = fixture.config();
    let runner = GardenerRunner::new(&config, 30, &fixture.clock).unwrap();

    let result = runner.execute(&mut store, &claim);

    assert!(matches!(
        result.completion,
        Completion::Failed {
            retryable: true,
            ref error,
            ..
        } if error.contains("noncanonical effective origin")
    ));
    let git_log = fs::read_to_string(&fixture.git_log).unwrap();
    assert!(git_log.contains("remote get-url --all origin"));
    assert!(!git_log.lines().any(|line| line.starts_with("fetch ")));
    assert!(!fixture.codex_log.exists());
}

fn write_executable(path: &Path, contents: &str) {
    static NEXT_TEMPORARY_FILE: AtomicUsize = AtomicUsize::new(0);

    let temporary_path = path.with_file_name(format!(
        ".{}.{}.tmp",
        path.file_name().unwrap().to_string_lossy(),
        NEXT_TEMPORARY_FILE.fetch_add(1, Ordering::Relaxed),
    ));
    let mut temporary = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary_path)
        .unwrap();
    temporary.write_all(contents.as_bytes()).unwrap();
    let mut permissions = temporary.metadata().unwrap().permissions();
    permissions.set_mode(0o755);
    temporary.set_permissions(permissions).unwrap();
    temporary.sync_all().unwrap();
    drop(temporary);
    fs::rename(temporary_path, path).unwrap();
}

fn local_git_transport(root: &Path, origin: &Path, log: &Path) -> PathBuf {
    let script = root.join("git-with-local-canonical-transport");
    let rewrite = format!(
        "url.file://{}.insteadOf={CANONICAL_HTTPS_URL}",
        origin.display()
    );
    let contents = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\ncase \"$1\" in\n  fetch|push|ls-remote) exec git -c '{}' \"$@\" ;;\n  *) exec git \"$@\" ;;\nesac\n",
        shell_single_quote(log),
        shell_single_quote(Path::new(&rewrite)),
    );
    write_executable(&script, &contents);
    script
}

fn shell_single_quote(path: &Path) -> String {
    path.to_string_lossy().replace('\'', "'\\''")
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
