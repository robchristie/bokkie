from __future__ import annotations

import json
import time
from pathlib import Path
from typing import Any

import httpx

from .config import Settings
from .enums import ArtifactKind, WorkItemKind
from .prompts import build_prompt, summarize_event_line
from .schemas import (
    ImplementResult,
    PlanResult,
    ProjectRead,
    RunRead,
    VerifyResult,
    WorkerCapabilities,
    WorkerHeartbeatIn,
    WorkItemLeaseResponse,
)
from .services.codex import CodexCliBackend, CodexExecutionError
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
        self.client = httpx.Client(base_url=self.api_base_url, timeout=120)
        self.codex = CodexCliBackend(settings)
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

    def register(self) -> None:
        self.client.post("/api/workers/register", json=self.worker.model_dump()).raise_for_status()

    def heartbeat(self, observed_load: int) -> None:
        payload = WorkerHeartbeatIn(observed_load=observed_load).model_dump()
        self.client.post(
            f"/api/workers/{self.worker.id}/heartbeat", json=payload
        ).raise_for_status()

    def lease(self) -> WorkItemLeaseResponse:
        response = self.client.post(f"/api/workers/{self.worker.id}/lease")
        response.raise_for_status()
        return WorkItemLeaseResponse.model_validate(response.json())

    def execute_assignment(self, lease: WorkItemLeaseResponse) -> None:
        assert lease.work_item is not None
        assert lease.project is not None
        assert lease.run is not None
        work_item = lease.work_item
        run = lease.run
        project = lease.project
        self.heartbeat(1)
        worktree = None
        try:
            patch_paths = [self._download_patch(url) for url in lease.prior_patch_downloads]
            worktree = self.git.prepare_worktree(
                self._project_model(project),
                self._run_model(run),
                self._work_item_model(work_item),
                patch_paths,
            )
            if work_item.kind == WorkItemKind.PUBLISH:
                self._publish(worktree, project, run, work_item)
                return
            prompt = build_prompt(
                self._project_model(project),
                self._run_model(run),
                self._work_item_model(work_item),
                work_item.payload.get("operator_notes", []),
            )
            schema_model = {
                WorkItemKind.PLAN: PlanResult,
                WorkItemKind.IMPLEMENT: ImplementResult,
                WorkItemKind.VERIFY: VerifyResult,
            }[work_item.kind]
            result = self.codex.run(
                worktree,
                prompt,
                schema_model=schema_model,
                kind=work_item.kind,
                on_event=lambda event: self._post_event(work_item.id, event),
            )
            self._upload_artifact(
                work_item.id,
                kind=ArtifactKind.FINAL_MESSAGE.value,
                name="last-message.json",
                content=result.raw_last_message.encode(),
                metadata={"kind": work_item.kind},
            )
            if work_item.kind == WorkItemKind.IMPLEMENT:
                patch = self.git.create_patch(worktree)
                if patch:
                    self._upload_artifact(
                        work_item.id,
                        kind=ArtifactKind.PATCH.value,
                        name="changes.patch",
                        content=patch,
                        metadata={"branch_name": run.branch_name},
                    )
            completion_payload = {
                "success": True,
                "worker_id": self.worker.id,
                "summary": result.final_output.get("summary"),
                "result": result.final_output,
            }
            self.client.post(
                f"/api/work-items/{work_item.id}/complete",
                json=completion_payload,
            ).raise_for_status()
        except CodexExecutionError as exc:
            self.client.post(
                f"/api/work-items/{work_item.id}/complete",
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

    def _publish(self, worktree: Path, project: ProjectRead, run: RunRead, work_item: Any) -> None:
        if not project.push_remote:
            raise RuntimeError("Project has no push remote configured")
        self.git.push_branch(worktree, project.push_remote, run.branch_name)
        self.client.post(
            f"/api/work-items/{work_item.id}/complete",
            json={
                "success": True,
                "worker_id": self.worker.id,
                "summary": f"Pushed branch {run.branch_name} to {project.push_remote}",
                "result": {"summary": f"Pushed branch {run.branch_name}"},
            },
        ).raise_for_status()

    def _post_event(self, work_item_id: str, event: dict[str, Any]) -> None:
        summary = summarize_event_line(event)
        self.client.post(
            f"/api/work-items/{work_item_id}/events",
            json={
                "event_type": event.get("type", "codex.event"),
                "summary": summary,
                "payload": event,
                "worker_id": self.worker.id,
            },
        ).raise_for_status()

    def _upload_artifact(
        self, work_item_id: str, kind: str, name: str, content: bytes, metadata: dict[str, Any]
    ) -> None:
        files = {"file": (name, content, "application/octet-stream")}
        data = {"kind": kind, "metadata": json.dumps(metadata)}
        self.client.post(
            f"/api/work-items/{work_item_id}/artifacts", files=files, data=data
        ).raise_for_status()

    def _download_patch(self, url: str) -> Path:
        request_url = url
        client_base_url = getattr(self.client, "base_url", None)
        if client_base_url is not None:
            base_url_text = str(client_base_url).rstrip("/")
            if url.startswith(base_url_text):
                request_url = url.removeprefix(base_url_text) or "/"
        response = self.client.get(request_url)
        response.raise_for_status()
        patch_dir = self.settings.worker_cache_dir / "downloaded-patches"
        patch_dir.mkdir(parents=True, exist_ok=True)
        file_path = patch_dir / f"{hash(url)}.patch"
        file_path.write_bytes(response.content)
        return file_path

    def _project_model(self, project: ProjectRead) -> Any:
        from .models import Project

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

    def _run_model(self, run: RunRead) -> Any:
        from .models import Run

        return Run(
            id=run.id,
            project_id=run.project_id,
            type=run.type.value,
            objective=run.objective,
            success_criteria=run.success_criteria,
            risk_level=run.risk_level.value,
            budget=run.budget,
            resource_profile=run.resource_profile,
            current_stage=run.current_stage.value,
            status=run.status.value,
            base_ref=run.base_ref,
            branch_name=run.branch_name,
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

    def _work_item_model(self, work_item: Any) -> Any:
        from .models import WorkItem

        return WorkItem(
            id=work_item.id,
            run_id="",
            sequence_no=work_item.sequence_no,
            kind=work_item.kind.value,
            status=work_item.status.value,
            prompt_template=work_item.prompt_template,
            requested_pool=work_item.requested_pool,
            requires_internet=False,
            required_secrets=[],
            timeout_seconds=1800,
            retry_limit=1,
            retry_count=0,
            base_ref=None,
            branch_name=None,
            payload=work_item.payload,
        )
