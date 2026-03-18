from __future__ import annotations

import asyncio
import json
from contextlib import asynccontextmanager
from pathlib import Path

from fastapi import Depends, FastAPI, File, Form, HTTPException, Request, UploadFile
from fastapi.responses import FileResponse, HTMLResponse, RedirectResponse, StreamingResponse
from fastapi.templating import Jinja2Templates
from sqlalchemy.orm import Session

from .config import get_settings
from .db import SessionLocal, get_db, init_db
from .enums import RiskLevel, RunType
from .models import WorkItem
from .schemas import (
    OperatorDecision,
    OperatorNoteIn,
    ProjectCreate,
    ProjectRead,
    PromoteRunIn,
    RunCreate,
    RunRead,
    WorkerCapabilities,
    WorkerHeartbeatIn,
    WorkerRead,
    WorkItemCompletionIn,
    WorkItemEventIn,
    WorkItemLeaseResponse,
)
from .services.artifacts import ArtifactStore
from .services.notifications import TelegramNotifier
from .services.orchestrator import OrchestrationError, OrchestratorService

settings = get_settings()
templates = Jinja2Templates(directory=str(Path(__file__).parent / "templates"))


def get_service(db: Session = Depends(get_db)) -> OrchestratorService:
    return OrchestratorService(db=db, settings=settings)


def create_app() -> FastAPI:
    artifact_store = ArtifactStore(settings.artifacts_dir)
    notifier = TelegramNotifier(settings)

    @asynccontextmanager
    async def lifespan(_: FastAPI):
        settings.artifacts_dir.mkdir(parents=True, exist_ok=True)
        init_db()
        yield

    app = FastAPI(title="Bokkie", lifespan=lifespan)
    @app.get("/", response_class=HTMLResponse)
    def root() -> RedirectResponse:
        return RedirectResponse(url="/ui/runs")

    @app.post("/api/projects", response_model=ProjectRead)
    def create_project(
        data: ProjectCreate,
        service: OrchestratorService = Depends(get_service),
    ) -> ProjectRead:
        try:
            return ProjectRead.model_validate(service.create_project(data))
        except OrchestrationError as exc:
            raise HTTPException(status_code=400, detail=str(exc)) from exc

    @app.post("/api/runs", response_model=RunRead)
    def create_run(data: RunCreate, service: OrchestratorService = Depends(get_service)) -> RunRead:
        try:
            return service.serialize_run(service.create_run(data))
        except OrchestrationError as exc:
            raise HTTPException(status_code=400, detail=str(exc)) from exc

    @app.get("/api/runs", response_model=list[RunRead])
    def list_runs(service: OrchestratorService = Depends(get_service)) -> list[RunRead]:
        return [service.serialize_run(run) for run in service.list_runs()]

    @app.get("/api/runs/{run_id}", response_model=RunRead)
    def get_run(run_id: str, service: OrchestratorService = Depends(get_service)) -> RunRead:
        try:
            return service.serialize_run(service.get_run(run_id))
        except OrchestrationError as exc:
            raise HTTPException(status_code=404, detail=str(exc)) from exc

    @app.post("/api/runs/{run_id}/approve", response_model=RunRead)
    def approve_run(
        run_id: str,
        decision: OperatorDecision,
        service: OrchestratorService = Depends(get_service),
    ) -> RunRead:
        try:
            return service.serialize_run(service.approve_run(run_id, decision))
        except OrchestrationError as exc:
            raise HTTPException(status_code=400, detail=str(exc)) from exc

    @app.post("/api/runs/{run_id}/reject", response_model=RunRead)
    def reject_run(
        run_id: str,
        decision: OperatorDecision,
        service: OrchestratorService = Depends(get_service),
    ) -> RunRead:
        try:
            return service.serialize_run(service.reject_run(run_id, decision))
        except OrchestrationError as exc:
            raise HTTPException(status_code=400, detail=str(exc)) from exc

    @app.post("/api/runs/{run_id}/pause", response_model=RunRead)
    def pause_run(run_id: str, service: OrchestratorService = Depends(get_service)) -> RunRead:
        try:
            return service.serialize_run(service.pause_run(run_id))
        except OrchestrationError as exc:
            raise HTTPException(status_code=404, detail=str(exc)) from exc

    @app.post("/api/runs/{run_id}/resume", response_model=RunRead)
    def resume_run(run_id: str, service: OrchestratorService = Depends(get_service)) -> RunRead:
        try:
            return service.serialize_run(service.resume_run(run_id))
        except OrchestrationError as exc:
            raise HTTPException(status_code=404, detail=str(exc)) from exc

    @app.post("/api/runs/{run_id}/steer", response_model=RunRead)
    def steer_run(
        run_id: str,
        data: OperatorNoteIn,
        service: OrchestratorService = Depends(get_service),
    ) -> RunRead:
        try:
            return service.serialize_run(service.steer_run(run_id, data))
        except OrchestrationError as exc:
            raise HTTPException(status_code=404, detail=str(exc)) from exc

    @app.post("/api/runs/{run_id}/promote", response_model=RunRead)
    def promote_run(
        run_id: str,
        data: PromoteRunIn,
        service: OrchestratorService = Depends(get_service),
    ) -> RunRead:
        try:
            return service.serialize_run(service.promote_run(run_id, data))
        except OrchestrationError as exc:
            raise HTTPException(status_code=404, detail=str(exc)) from exc

    @app.post("/api/workers/register", response_model=WorkerRead)
    def register_worker(
        data: WorkerCapabilities,
        service: OrchestratorService = Depends(get_service),
    ) -> WorkerRead:
        try:
            return service.serialize_worker(service.register_worker(data))
        except OrchestrationError as exc:
            raise HTTPException(status_code=400, detail=str(exc)) from exc

    @app.post("/api/workers/{worker_id}/heartbeat", response_model=WorkerRead)
    def heartbeat_worker(
        worker_id: str,
        data: WorkerHeartbeatIn,
        service: OrchestratorService = Depends(get_service),
    ) -> WorkerRead:
        try:
            return service.serialize_worker(service.heartbeat_worker(worker_id, data))
        except OrchestrationError as exc:
            raise HTTPException(status_code=404, detail=str(exc)) from exc

    @app.post("/api/workers/{worker_id}/lease", response_model=WorkItemLeaseResponse)
    def lease_work_item(
        worker_id: str,
        service: OrchestratorService = Depends(get_service),
    ) -> WorkItemLeaseResponse:
        try:
            return service.claim_work_item(worker_id)
        except OrchestrationError as exc:
            raise HTTPException(status_code=404, detail=str(exc)) from exc

    @app.post("/api/work-items/{work_item_id}/events")
    def add_event(
        work_item_id: str,
        data: WorkItemEventIn,
        service: OrchestratorService = Depends(get_service),
    ) -> dict[str, int]:
        try:
            event = service.add_event(work_item_id, data)
        except OrchestrationError as exc:
            raise HTTPException(status_code=404, detail=str(exc)) from exc
        return {"event_id": event.id}

    @app.post("/api/work-items/{work_item_id}/complete")
    def complete_work_item(
        work_item_id: str,
        data: WorkItemCompletionIn,
        service: OrchestratorService = Depends(get_service),
    ) -> dict[str, str]:
        try:
            item = service.complete_work_item(work_item_id, data)
            run = service.serialize_run(service.get_run(item.run_id))
            notifier.notify_run_checkpoint(run)
        except OrchestrationError as exc:
            raise HTTPException(status_code=400, detail=str(exc)) from exc
        return {"work_item_id": item.id, "status": item.status}

    @app.post("/api/work-items/{work_item_id}/artifacts")
    async def upload_artifact(
        work_item_id: str,
        file: UploadFile = File(...),
        kind: str = Form(...),
        metadata: str = Form("{}"),
        service: OrchestratorService = Depends(get_service),
    ) -> dict[str, str]:
        work_item = service.db.get(WorkItem, work_item_id)
        if work_item is None:
            raise HTTPException(status_code=404, detail="Work item not found")
        raw_bytes = await file.read()
        filename = file.filename or "artifact.bin"
        stored = artifact_store.put_bytes(work_item.run_id, work_item_id, filename, raw_bytes)
        artifact = service.create_artifact(
            run_id=work_item.run_id,
            work_item_id=work_item_id,
            kind=kind,
            name=filename,
            storage_path=stored.storage_path,
            content_type=file.content_type or "application/octet-stream",
            sha256=stored.sha256,
            size_bytes=stored.size_bytes,
            metadata=json.loads(metadata),
        )
        return {"artifact_id": artifact.id}

    @app.get("/api/artifacts/{artifact_id}/download")
    def download_artifact(
        artifact_id: str,
        service: OrchestratorService = Depends(get_service),
    ) -> FileResponse:
        try:
            artifact = service.get_artifact(artifact_id)
        except OrchestrationError as exc:
            raise HTTPException(status_code=404, detail=str(exc)) from exc
        return FileResponse(
            artifact_store.resolve(artifact.storage_path),
            media_type=artifact.content_type,
            filename=artifact.name,
        )

    @app.get("/ui/runs", response_class=HTMLResponse)
    def runs_page(
        request: Request,
        service: OrchestratorService = Depends(get_service),
    ) -> HTMLResponse:
        runs = [service.serialize_run(run) for run in service.list_runs()]
        projects = [ProjectRead.model_validate(project) for project in service.list_projects()]
        return templates.TemplateResponse(
            request,
            "runs.html",
            {
                "runs": runs,
                "projects": projects,
                "title": "Runs",
                "run_types": [value.value for value in RunType],
                "risk_levels": [value.value for value in RiskLevel],
            },
        )

    @app.get("/ui/runs/{run_id}", response_class=HTMLResponse)
    def run_detail_page(
        request: Request,
        run_id: str,
        service: OrchestratorService = Depends(get_service),
    ) -> HTMLResponse:
        try:
            run = service.serialize_run(service.get_run(run_id))
        except OrchestrationError as exc:
            raise HTTPException(status_code=404, detail=str(exc)) from exc
        pending_review = next((review for review in reversed(run.reviews) if review.status == "pending"), None)
        return templates.TemplateResponse(
            request,
            "run_detail.html",
            {"run": run, "pending_review": pending_review, "title": f"Run {run.id[:8]}"},
        )

    @app.get("/ui/reviews", response_class=HTMLResponse)
    def reviews_page(
        request: Request,
        service: OrchestratorService = Depends(get_service),
    ) -> HTMLResponse:
        reviews = service.pending_reviews()
        runs = {
            review.run_id: service.serialize_run(service.get_run(review.run_id))
            for review in reviews
        }
        return templates.TemplateResponse(
            request,
            "reviews.html",
            {"reviews": reviews, "runs": runs, "title": "Approvals"},
        )

    @app.get("/ui/workers", response_class=HTMLResponse)
    def workers_page(
        request: Request,
        service: OrchestratorService = Depends(get_service),
    ) -> HTMLResponse:
        workers = [service.serialize_worker(worker) for worker in service.list_workers()]
        return templates.TemplateResponse(
            request,
            "workers.html",
            {"workers": workers, "title": "Workers"},
        )

    @app.get("/ui/runs/{run_id}/stream")
    async def run_stream(run_id: str) -> StreamingResponse:
        async def event_source():
            last_seen = None
            while True:
                with SessionLocal() as db:
                    service = OrchestratorService(db=db, settings=settings)
                    try:
                        run = service.serialize_run(service.get_run(run_id))
                    except OrchestrationError:
                        yield "event: end\ndata: {}\n\n"
                        return
                    marker = (run.updated_at.isoformat(), len(run.events))
                    if marker != last_seen:
                        payload = run.model_dump(mode="json")
                        yield f"data: {json.dumps(payload)}\n\n"
                        last_seen = marker
                await asyncio.sleep(2)

        return StreamingResponse(event_source(), media_type="text/event-stream")

    @app.post("/ui/projects")
    def create_project_form(
        slug: str = Form(...),
        name: str = Form(...),
        repo_url: str = Form(...),
        default_branch: str = Form("main"),
        push_remote: str | None = Form(None),
        service: OrchestratorService = Depends(get_service),
    ) -> RedirectResponse:
        try:
            service.create_project(
                ProjectCreate(
                    slug=slug,
                    name=name,
                    repo_url=repo_url,
                    default_branch=default_branch,
                    push_remote=push_remote or None,
                )
            )
        except OrchestrationError as exc:
            raise HTTPException(status_code=400, detail=str(exc)) from exc
        return RedirectResponse(url="/ui/runs", status_code=303)

    @app.post("/ui/runs")
    def create_run_form(
        project_id: str = Form(...),
        objective: str = Form(...),
        success_criteria: str = Form(...),
        run_type: str = Form("feature"),
        risk_level: str = Form("medium"),
        pool: str | None = Form(None),
        internet: bool = Form(False),
        service: OrchestratorService = Depends(get_service),
    ) -> RedirectResponse:
        try:
            run = service.create_run(
                RunCreate(
                    project_id=project_id,
                    type=run_type,
                    objective=objective,
                    success_criteria=success_criteria,
                    risk_level=risk_level,
                    resource_profile={"pool": pool or None, "internet": internet, "secrets": []},
                )
            )
        except OrchestrationError as exc:
            raise HTTPException(status_code=400, detail=str(exc)) from exc
        return RedirectResponse(url=f"/ui/runs/{run.id}", status_code=303)

    @app.post("/ui/runs/{run_id}/approve")
    def approve_run_form(
        run_id: str,
        actor: str = Form("web-ui"),
        reason: str | None = Form(None),
        service: OrchestratorService = Depends(get_service),
    ) -> RedirectResponse:
        try:
            run = service.serialize_run(
                service.approve_run(run_id, OperatorDecision(actor=actor, reason=reason or None))
            )
            notifier.notify_run_checkpoint(run)
        except OrchestrationError as exc:
            raise HTTPException(status_code=400, detail=str(exc)) from exc
        return RedirectResponse(url=f"/ui/runs/{run_id}", status_code=303)

    @app.post("/ui/runs/{run_id}/reject")
    def reject_run_form(
        run_id: str,
        actor: str = Form("web-ui"),
        reason: str = Form(...),
        service: OrchestratorService = Depends(get_service),
    ) -> RedirectResponse:
        try:
            service.reject_run(run_id, OperatorDecision(actor=actor, reason=reason))
        except OrchestrationError as exc:
            raise HTTPException(status_code=400, detail=str(exc)) from exc
        return RedirectResponse(url=f"/ui/runs/{run_id}", status_code=303)

    @app.post("/ui/runs/{run_id}/pause")
    def pause_run_form(
        run_id: str,
        service: OrchestratorService = Depends(get_service),
    ) -> RedirectResponse:
        try:
            service.pause_run(run_id)
        except OrchestrationError as exc:
            raise HTTPException(status_code=404, detail=str(exc)) from exc
        return RedirectResponse(url=f"/ui/runs/{run_id}", status_code=303)

    @app.post("/ui/runs/{run_id}/resume")
    def resume_run_form(
        run_id: str,
        service: OrchestratorService = Depends(get_service),
    ) -> RedirectResponse:
        try:
            service.resume_run(run_id)
        except OrchestrationError as exc:
            raise HTTPException(status_code=404, detail=str(exc)) from exc
        return RedirectResponse(url=f"/ui/runs/{run_id}", status_code=303)

    @app.post("/ui/runs/{run_id}/steer")
    def steer_run_form(
        run_id: str,
        note: str = Form(...),
        created_by: str = Form("web-ui"),
        service: OrchestratorService = Depends(get_service),
    ) -> RedirectResponse:
        try:
            service.steer_run(run_id, OperatorNoteIn(note=note, created_by=created_by))
        except OrchestrationError as exc:
            raise HTTPException(status_code=404, detail=str(exc)) from exc
        return RedirectResponse(url=f"/ui/runs/{run_id}", status_code=303)

    @app.post("/ui/runs/{run_id}/promote")
    def promote_run_form(
        run_id: str,
        pool: str = Form(...),
        service: OrchestratorService = Depends(get_service),
    ) -> RedirectResponse:
        try:
            service.promote_run(run_id, PromoteRunIn(pool=pool))
        except OrchestrationError as exc:
            raise HTTPException(status_code=404, detail=str(exc)) from exc
        return RedirectResponse(url=f"/ui/runs/{run_id}", status_code=303)

    return app


app = create_app()
