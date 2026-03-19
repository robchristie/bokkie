from __future__ import annotations

import json
import subprocess
import time
from pathlib import Path
from typing import Any

import httpx

from .config import Settings
from .enums import ArtifactKind, PhaseName
from .models import PhaseAttempt, Project, Run
from .prompts import build_phase_prompt, summarize_event_line
from .schemas import (
    ExecutePhaseResult,
    PhaseLeaseResponse,
    PlanPhaseResult,
    ProjectRead,
    ReviewPhaseResult,
    RunRead,
    SpecPhaseResult,
    VerifyCommandResult,
    VerifyPhaseResult,
    WorkerCapabilities,
    WorkerHeartbeatIn,
)
from .services.codex import CodexAppServerBackend, CodexExecutionError
from .services.gitops import RepoWorkspaceManager


class WorkerRunner:
    def __init__(
        self,
        settings: Settings,
        worker: WorkerCapabilities,
        api_base_url: str | None = None,
    ) -> None:
        self.settings = settings
        self.worker = worker
        self.api_base_url = api_base_url or settings.api_base_url
        if not self.api_base_url.startswith(("http://", "https://")):
            raise ValueError(
                "BOKKIE_API_BASE_URL must be a full http(s) URL, "
                f"got {self.api_base_url!r}."
            )
        self.client = httpx.Client(base_url=self.api_base_url, timeout=120)
        self.codex = CodexAppServerBackend(settings)
        self.git = RepoWorkspaceManager(settings.worker_cache_dir, settings.worker_worktree_dir)

    def run_forever(self) -> None:
        self.register()
        while True:
            self.heartbeat(0)
            lease = self.lease()
            if not lease.leased:
                time.sleep(self.settings.worker_poll_seconds)
                continue
            self.execute_assignment(lease)

    def run_once(self, target_phase_attempt_id: str | None = None) -> bool:
        self.register()
        self.heartbeat(0)
        lease = self.lease(target_phase_attempt_id=target_phase_attempt_id)
        if not lease.leased:
            return False
        self.execute_assignment(lease)
        return True

    def register(self) -> None:
        self.client.post("/api/workers/register", json=self.worker.model_dump()).raise_for_status()

    def heartbeat(self, observed_load: int) -> None:
        payload = WorkerHeartbeatIn(observed_load=observed_load).model_dump()
        self.client.post(
            f"/api/workers/{self.worker.id}/heartbeat", json=payload
        ).raise_for_status()

    def lease(self, target_phase_attempt_id: str | None = None) -> PhaseLeaseResponse:
        params = {}
        if target_phase_attempt_id:
            params["target_phase_attempt_id"] = target_phase_attempt_id
        response = self.client.post(f"/api/workers/{self.worker.id}/lease", params=params)
        response.raise_for_status()
        return PhaseLeaseResponse.model_validate(response.json())

    def execute_assignment(self, lease: PhaseLeaseResponse) -> None:
        assert lease.phase_attempt is not None
        assert lease.project is not None
        assert lease.run is not None
        phase_attempt = lease.phase_attempt
        run = lease.run
        project = lease.project
        self.heartbeat(1)
        worktree = None
        try:
            patch_paths = [self._download_patch(url) for url in lease.prior_patch_downloads]
            worktree = self.git.prepare_worktree(
                self._project_model(project),
                self._run_model(run),
                self._phase_model(phase_attempt),
                patch_paths,
            )
            self._materialize_input_artifacts(worktree, lease.input_artifacts)
            phase_model = self._phase_model(phase_attempt)
            if phase_attempt.phase_name == PhaseName.VERIFY:
                command_results = self._run_evaluator_commands(worktree, lease.evaluator_commands)
                phase_model.payload["command_results"] = [
                    result.model_dump() for result in command_results
                ]
            prompt = build_phase_prompt(
                self.settings,
                self._project_model(project),
                self._run_model(run),
                phase_model,
                worktree,
                lease.operator_notes,
                lease.input_artifacts,
                lease.evaluator_commands,
            )
            schema_model = {
                PhaseName.PLAN: PlanPhaseResult,
                PhaseName.PLAN_REVIEW: ReviewPhaseResult,
                PhaseName.SPEC: SpecPhaseResult,
                PhaseName.SPEC_REVIEW: ReviewPhaseResult,
                PhaseName.EXECUTE: ExecutePhaseResult,
                PhaseName.VERIFY: VerifyPhaseResult,
                PhaseName.FINAL_REVIEW: ReviewPhaseResult,
            }[phase_attempt.phase_name]
            result = self.codex.run(
                worktree,
                prompt,
                schema_model=schema_model,
                writable=phase_attempt.phase_name == PhaseName.EXECUTE,
                internet=self._phase_internet(phase_attempt.phase_name),
                on_event=lambda event: self._post_event(phase_attempt.id, event),
                steering_supplier=lambda: self._claim_notes(phase_attempt.id),
            )
            final_output = result.final_output
            if phase_attempt.phase_name == PhaseName.VERIFY:
                command_results = phase_model.payload.get("command_results", [])
                final_output["command_results"] = command_results
            log_relative_path = self._log_relative_path(phase_attempt.phase_name, phase_attempt.id)
            self._upload_artifact(
                phase_attempt.id,
                kind=ArtifactKind.FINAL_MESSAGE.value,
                name=Path(log_relative_path).name,
                content=result.raw_last_message.encode(),
                metadata={"phase": phase_attempt.phase_name.value},
                relative_path=log_relative_path,
            )
            if phase_attempt.phase_name == PhaseName.EXECUTE:
                patch = self.git.create_patch(worktree)
                if patch:
                    self._upload_artifact(
                        phase_attempt.id,
                        kind=ArtifactKind.PATCH.value,
                        name=f"{phase_attempt.id}.patch",
                        content=patch,
                        metadata={"branch_name": run.branch_name},
                        relative_path=f"exec/patches/{phase_attempt.id}.patch",
                    )
            completion_payload = {
                "success": True,
                "worker_id": self.worker.id,
                "summary": final_output.get("summary"),
                "result": final_output,
            }
            self.client.post(
                f"/api/phase-attempts/{phase_attempt.id}/complete",
                json=completion_payload,
            ).raise_for_status()
        except CodexExecutionError as exc:
            self.client.post(
                f"/api/phase-attempts/{phase_attempt.id}/complete",
                json={
                    "success": False,
                    "worker_id": self.worker.id,
                    "error_text": str(exc),
                    "result": {},
                },
            ).raise_for_status()
        finally:
            if worktree is not None and self.settings.worker_cleanup_worktrees:
                self.git.cleanup(worktree)
            self.heartbeat(0)

    def _run_evaluator_commands(
        self, worktree: Path, commands: list[str]
    ) -> list[VerifyCommandResult]:
        results: list[VerifyCommandResult] = []
        for command in commands:
            completed = subprocess.run(
                ["zsh", "-lc", command],
                cwd=worktree,
                check=False,
                capture_output=True,
                text=True,
            )
            results.append(
                VerifyCommandResult(
                    command=command,
                    exit_code=completed.returncode,
                    stdout=completed.stdout,
                    stderr=completed.stderr,
                )
            )
        return results

    def _phase_internet(self, phase_name: PhaseName) -> bool:
        return phase_name in {
            PhaseName.PLAN,
            PhaseName.PLAN_REVIEW,
            PhaseName.SPEC_REVIEW,
            PhaseName.FINAL_REVIEW,
        }

    def _claim_notes(self, phase_attempt_id: str) -> list[str]:
        response = self.client.post(f"/api/phase-attempts/{phase_attempt_id}/notes/claim")
        response.raise_for_status()
        return response.json().get("notes", [])

    def _post_event(self, phase_attempt_id: str, event: dict[str, Any]) -> None:
        summary = summarize_event_line(event)
        self.client.post(
            f"/api/phase-attempts/{phase_attempt_id}/events",
            json={
                "event_type": event.get("method", event.get("type", "codex.event")),
                "summary": summary,
                "payload": event,
                "worker_id": self.worker.id,
            },
        ).raise_for_status()

    def _upload_artifact(
        self,
        phase_attempt_id: str,
        *,
        kind: str,
        name: str,
        content: bytes,
        metadata: dict[str, Any],
        relative_path: str | None = None,
    ) -> None:
        files = {"file": (name, content, "application/octet-stream")}
        data = {"kind": kind, "metadata": json.dumps(metadata), "relative_path": relative_path or ""}
        self.client.post(
            f"/api/phase-attempts/{phase_attempt_id}/artifacts", files=files, data=data
        ).raise_for_status()

    def _materialize_input_artifacts(self, worktree: Path, input_artifacts: dict[str, str]) -> None:
        for relative_path, url in input_artifacts.items():
            response = self.client.get(self._request_url(url))
            response.raise_for_status()
            self.git.materialize_artifact(worktree, relative_path, response.content)

    def _download_patch(self, url: str) -> Path:
        response = self.client.get(self._request_url(url))
        response.raise_for_status()
        patch_dir = self.settings.worker_cache_dir / "downloaded-patches"
        patch_dir.mkdir(parents=True, exist_ok=True)
        file_path = patch_dir / f"{hash(url)}.patch"
        file_path.write_bytes(response.content)
        return file_path

    def _request_url(self, url: str) -> str:
        client_base_url = getattr(self.client, "base_url", None)
        if client_base_url is None:
            return url
        base_url_text = str(client_base_url).rstrip("/")
        if url.startswith(base_url_text):
            return url.removeprefix(base_url_text) or "/"
        return url

    def _project_model(self, project: ProjectRead) -> Project:
        return Project(
            id=project.id,
            slug=project.slug,
            name=project.name,
            repo_url=project.repo_url,
            default_branch=project.default_branch,
            push_remote=project.push_remote,
            allowed_pools=project.allowed_pools,
            required_secrets=project.required_secrets,
            command_profiles=project.command_profiles,
            settings=project.settings,
        )

    def _run_model(self, run: RunRead) -> Run:
        return Run(
            id=run.id,
            project_id=run.project_id,
            type=run.type.value,
            task_name=run.task_name,
            objective=run.objective,
            success_criteria=run.success_criteria,
            risk_level=run.risk_level.value,
            budget=run.budget,
            resource_profile=run.resource_profile,
            current_stage=run.current_stage.value,
            current_session_id=run.current_session_id,
            status=run.status.value,
            base_ref=run.base_ref,
            branch_name=run.branch_name,
            run_root=run.run_root,
            latest_summary=run.latest_summary,
            current_worker_id=run.current_worker_id,
            latest_verifier_result=run.latest_verifier_result,
            next_action=run.next_action,
            blockers=run.blockers,
            risk_flags=run.risk_flags,
            preferred_pool=run.preferred_pool,
            requires_internet=run.requires_internet,
            required_secrets=run.required_secrets,
            publish_strategy=run.publish_strategy.value,
        )

    def _phase_model(self, phase_attempt: Any) -> PhaseAttempt:
        return PhaseAttempt(
            id=phase_attempt.id,
            run_id="",
            phase_name=phase_attempt.phase_name.value,
            phase_index=phase_attempt.phase_index,
            attempt_no=phase_attempt.attempt_no,
            role=phase_attempt.role.value,
            status=phase_attempt.status.value,
            requested_pool=phase_attempt.requested_pool,
            required_labels=[],
            requires_internet=False,
            required_secrets=[],
            timeout_seconds=1800,
            retry_limit=phase_attempt.retry_limit,
            retry_count=phase_attempt.retry_count,
            thread_id=phase_attempt.thread_id,
            last_turn_id=phase_attempt.last_turn_id,
            worker_id=phase_attempt.worker_id,
            branch_name=None,
            input_artifact_refs={},
            payload=dict(phase_attempt.payload),
        )

    def _log_relative_path(self, phase_name: PhaseName, phase_attempt_id: str) -> str:
        if phase_name in {PhaseName.PLAN, PhaseName.PLAN_REVIEW}:
            directory = "plan/logs"
        elif phase_name in {PhaseName.SPEC, PhaseName.SPEC_REVIEW, PhaseName.EXECUTE}:
            directory = "exec/logs"
        else:
            directory = "verify/logs"
        return f"{directory}/{phase_attempt_id}-last-message.json"
