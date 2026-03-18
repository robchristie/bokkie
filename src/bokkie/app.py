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
from .services.orchestrator import OrchestrationError, OrchestratorService

settings = get_settings()
templates = Jinja2Templates(directory=str(Path(__file__).parent / "templates"))


def get_service(db: Session = Depends(get_db)) -> OrchestratorService:
    return OrchestratorService(db=db, settings=settings)


def create_app() -> FastAPI:
    artifact_store = ArtifactStore(settings.artifacts_dir)

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
        return templates.TemplateResponse(
            request,
            "runs.html",
            {"runs": runs, "title": "Runs"},
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
        return templates.TemplateResponse(
            request,
            "run_detail.html",
            {"run": run, "title": f"Run {run.id[:8]}"},
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

    return app


app = create_app()
