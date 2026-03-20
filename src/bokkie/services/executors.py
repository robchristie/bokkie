from __future__ import annotations

import shlex
import subprocess
from datetime import UTC, datetime, timedelta

from sqlalchemy import select
from sqlalchemy.orm import Session, selectinload

from ..config import Settings
from ..models import PhaseAttempt, Run, Worker
from ..schemas import ExecutorRead, WorkerRead
from .repo_config import ExecutorConfig, load_repo_config


class ExecutorLauncherService:
    def __init__(self, db: Session, settings: Settings) -> None:
        self.db = db
        self.settings = settings
        self.repo_config = load_repo_config(settings)

    def list_executors(self) -> list[ExecutorRead]:
        workers = list(self.db.scalars(select(Worker).order_by(Worker.host, Worker.id)))
        queued_phases = list(
            self.db.scalars(
                select(PhaseAttempt)
                .join(Run)
                .where(PhaseAttempt.status == "queued", Run.status.in_(["queued", "running"]))
            )
        )
        results: list[ExecutorRead] = []
        for name, config in self.repo_config.executors.items():
            active_workers = [
                WorkerRead.model_validate(worker)
                for worker in workers
                if worker.metadata_json.get("executor_name") == name
                and worker.last_seen_at >= self._worker_cutoff()
            ]
            pending_count = sum(
                1 for phase in queued_phases if self._executor_matches_phase(config, phase)
            )
            results.append(
                ExecutorRead(
                    name=name,
                    driver=config.driver,
                    host=config.host,
                    pools=config.pools,
                    labels=config.labels,
                    secrets=config.secrets,
                    image=config.image,
                    workdir=config.workdir,
                    worker_command=config.worker_command,
                    max_workers=config.max_workers,
                    active_workers=active_workers,
                    pending_phase_count=pending_count,
                )
            )
        return results

    def dispatch_once(self) -> list[str]:
        launches: list[str] = []
        workers = list(self.db.scalars(select(Worker)))
        statement = (
            select(PhaseAttempt)
            .join(Run)
            .options(selectinload(PhaseAttempt.run))
            .where(PhaseAttempt.status == "queued", Run.status.in_(["queued", "running"]))
            .order_by(PhaseAttempt.created_at, PhaseAttempt.phase_index, PhaseAttempt.attempt_no)
        )
        for phase in self.db.scalars(statement):
            if phase.last_dispatch_at and phase.last_dispatch_at >= self._dispatch_cutoff():
                continue
            executor_name, config = self._choose_executor(phase, workers)
            if config is None:
                continue
            self._launch_worker(executor_name, config, phase)
            phase.assigned_executor_name = executor_name
            phase.dispatch_attempts += 1
            phase.last_dispatch_at = self._now()
            launches.append(phase.id)
        self.db.commit()
        return launches

    def _choose_executor(
        self, phase: PhaseAttempt, workers: list[Worker]
    ) -> tuple[str | None, ExecutorConfig | None]:
        for name, config in self.repo_config.executors.items():
            if not self._executor_matches_phase(config, phase):
                continue
            active_count = sum(
                1
                for worker in workers
                if worker.metadata_json.get("executor_name") == name
                and worker.last_seen_at >= self._worker_cutoff()
            )
            if active_count >= config.max_workers:
                continue
            return name, config
        return None, None

    def _executor_matches_phase(self, config: ExecutorConfig, phase: PhaseAttempt) -> bool:
        if phase.requested_pool and config.pools and phase.requested_pool not in config.pools:
            return False
        if any(label not in config.labels for label in phase.required_labels):
            return False
        return not any(secret not in config.secrets for secret in phase.required_secrets)

    def _launch_worker(
        self, executor_name: str, config: ExecutorConfig, phase: PhaseAttempt
    ) -> None:
        worker_id = f"{executor_name}-{phase.id[:8]}-{phase.dispatch_attempts + 1}"
        host = config.host or executor_name
        command = config.worker_command or self._default_worker_command(
            worker_id=worker_id,
            host=host,
            executor_name=executor_name,
            target_phase_attempt_id=phase.id,
            pools=config.pools or ([phase.requested_pool] if phase.requested_pool else []),
            labels=config.labels,
            secrets=config.secrets,
        )
        formatted = command.format(
            worker_id=worker_id,
            host=host,
            executor_name=executor_name,
            target_phase_attempt_id=phase.id,
            api_base_url=self.settings.api_base_url,
        )
        if config.driver == "local":
            subprocess.Popen(
                ["zsh", "-lc", formatted],
                cwd=config.workdir or str(self.settings.resolved_repo_root()),
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                start_new_session=True,
            )
            return
        if config.driver == "ssh-docker":
            remote = formatted
            if config.workdir:
                remote = f"cd {shlex.quote(config.workdir)} && {remote}"
            subprocess.Popen(
                ["ssh", config.host or executor_name, remote],
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                start_new_session=True,
            )
            return
        raise RuntimeError(f"Unsupported executor driver: {config.driver}")

    def _default_worker_command(
        self,
        *,
        worker_id: str,
        host: str,
        executor_name: str,
        target_phase_attempt_id: str,
        pools: list[str],
        labels: list[str],
        secrets: list[str],
    ) -> str:
        command = [
            "uv",
            "run",
            "bokkie",
            "worker",
            "--once",
            "--worker-id",
            worker_id,
            "--host",
            host,
            "--executor-name",
            executor_name,
            "--target-phase-attempt-id",
            target_phase_attempt_id,
        ]
        for pool in pools:
            command.extend(["--pool", pool])
        for label in labels:
            command.extend(["--label", label])
        for secret in secrets:
            command.extend(["--secret", secret])
        return shlex.join(command)

    def _worker_cutoff(self) -> datetime:
        return self._now() - timedelta(seconds=self.settings.lease_ttl_seconds)

    def _dispatch_cutoff(self) -> datetime:
        return self._now() - timedelta(seconds=self.settings.executor_launch_cooldown_seconds)

    def _now(self) -> datetime:
        return datetime.now(tz=UTC)
