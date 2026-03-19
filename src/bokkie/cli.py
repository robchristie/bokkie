from __future__ import annotations

import typer
import uvicorn

from .app import create_app
from .config import get_settings
from .db import init_db
from .schemas import WorkerCapabilities
from .telegram_bot import TelegramBotRunner
from .worker import WorkerRunner

app = typer.Typer(no_args_is_help=True)


@app.command("init-db")
def init_db_command() -> None:
    init_db()


@app.command("api")
def api_command(
    host: str | None = None,
    port: int | None = None,
) -> None:
    settings = get_settings()
    uvicorn.run(
        create_app(),
        host=host or settings.api_host,
        port=port or settings.api_port,
    )


@app.command("worker")
def worker_command(
    worker_id: str = typer.Option(..., "--worker-id"),
    host: str = typer.Option(..., "--host"),
    pool: list[str] = typer.Option([], "--pool"),
    label: list[str] = typer.Option([], "--label"),
    secret: list[str] = typer.Option([], "--secret"),
    executor_name: str | None = typer.Option(None, "--executor-name"),
    target_phase_attempt_id: str | None = typer.Option(None, "--target-phase-attempt-id"),
    once: bool = typer.Option(False, "--once"),
    cpu_cores: int | None = typer.Option(None, "--cpu-cores"),
    ram_gb: int | None = typer.Option(None, "--ram-gb"),
    gpu_model: str | None = typer.Option(None, "--gpu-model"),
    gpu_vram_gb: int | None = typer.Option(None, "--gpu-vram-gb"),
) -> None:
    settings = get_settings()
    worker = WorkerCapabilities(
        id=worker_id,
        host=host,
        pools=pool,
        labels=label,
        secrets=secret,
        cpu_cores=cpu_cores,
        ram_gb=ram_gb,
        gpu_model=gpu_model,
        gpu_vram_gb=gpu_vram_gb,
        metadata={"executor_name": executor_name} if executor_name else {},
    )
    runner = WorkerRunner(settings=settings, worker=worker)
    if once:
        runner.run_once(target_phase_attempt_id=target_phase_attempt_id)
        return
    runner.run_forever()


@app.command("telegram")
def telegram_command() -> None:
    TelegramBotRunner(get_settings()).run_forever()
