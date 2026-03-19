from __future__ import annotations

import tomllib
from pathlib import Path
from typing import Any

from pydantic import BaseModel, Field

from ..config import Settings


class ExecutorConfig(BaseModel):
    driver: str
    host: str | None = None
    pools: list[str] = Field(default_factory=list)
    labels: list[str] = Field(default_factory=list)
    secrets: list[str] = Field(default_factory=list)
    image: str | None = None
    workdir: str | None = None
    worker_command: str | None = None
    max_workers: int = 1


class RunTypeConfig(BaseModel):
    phases: list[str]
    auto_continue: bool = False


class TaskConfig(BaseModel):
    name: str
    run_type: str = "change"
    executor_labels: list[str] = Field(default_factory=list)
    evaluator_commands: list[str] = Field(default_factory=list)
    timeout_seconds: int = 1800
    auto_continue: bool = False


class JobConfig(BaseModel):
    name: str
    task: str
    schedule: str | None = None
    params: dict[str, Any] = Field(default_factory=dict)


class BokkieRepoConfig(BaseModel):
    run_types: dict[str, RunTypeConfig] = Field(default_factory=dict)
    executors: dict[str, ExecutorConfig] = Field(default_factory=dict)
    tasks: dict[str, TaskConfig] = Field(default_factory=dict)
    jobs: dict[str, JobConfig] = Field(default_factory=dict)


def _load_toml(path: Path) -> dict[str, Any]:
    if not path.exists():
        return {}
    return tomllib.loads(path.read_text())


def load_repo_config(settings: Settings) -> BokkieRepoConfig:
    root = settings.resolved_repo_root()
    data = _load_toml(settings.resolved_bokkie_config_path())

    tasks: dict[str, TaskConfig] = {}
    tasks_dir = root / "tasks"
    if tasks_dir.exists():
        for path in sorted(tasks_dir.glob("*.toml")):
            raw = _load_toml(path)
            tasks[path.stem] = TaskConfig(name=path.stem, **raw)

    jobs: dict[str, JobConfig] = {}
    jobs_dir = root / "jobs"
    if jobs_dir.exists():
        for path in sorted(jobs_dir.glob("*.toml")):
            raw = _load_toml(path)
            jobs[path.stem] = JobConfig(name=path.stem, **raw)

    run_types = {
        name: RunTypeConfig(**config)
        for name, config in data.get("run_types", {}).items()
    }
    executors = {
        name: ExecutorConfig(**config)
        for name, config in data.get("executors", {}).items()
    }
    return BokkieRepoConfig(
        run_types=run_types,
        executors=executors,
        tasks=tasks,
        jobs=jobs,
    )
