from __future__ import annotations

import json
import os
import shutil
import subprocess
import tempfile
from collections.abc import Callable
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from pydantic import BaseModel

from ..config import Settings
from ..enums import WorkItemKind


class CodexExecutionError(RuntimeError):
    pass


@dataclass
class CodexRunResult:
    final_output: dict[str, Any]
    raw_last_message: str


class CodexCliBackend:
    def __init__(self, settings: Settings) -> None:
        self.settings = settings

    def run(
        self,
        worktree: Path,
        prompt: str,
        schema_model: type[BaseModel],
        kind: WorkItemKind,
        on_event: Callable[[dict[str, Any]], None] | None = None,
    ) -> CodexRunResult:
        with tempfile.TemporaryDirectory(prefix="bokkie-codex-") as temp_dir_name:
            temp_dir = Path(temp_dir_name)
            schema_path = temp_dir / "schema.json"
            output_path = temp_dir / "last-message.json"
            schema_path.write_text(json.dumps(schema_model.model_json_schema(), indent=2))

            command = [
                "codex",
                "exec",
                "--json",
                "--ephemeral",
                "--skip-git-repo-check",
                "--cd",
                str(worktree),
                "--output-schema",
                str(schema_path),
                "--output-last-message",
                str(output_path),
            ]
            if self.settings.default_codex_model:
                command.extend(["--model", self.settings.default_codex_model])

            if kind == WorkItemKind.IMPLEMENT:
                command.append("--full-auto")
            else:
                command.extend(["--sandbox", "read-only"])

            env = self._build_subprocess_env()
            process = subprocess.Popen(
                command,
                env=env,
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
            )
            stdout_lines: list[str] = []
            stderr_text = ""
            assert process.stdin is not None
            process.stdin.write(prompt)
            process.stdin.close()
            assert process.stdout is not None
            for line in process.stdout:
                stdout_lines.append(line)
                try:
                    event = json.loads(line)
                except json.JSONDecodeError:
                    continue
                if on_event:
                    on_event(event)
            if process.stderr is not None:
                stderr_text = process.stderr.read()
            return_code = process.wait()
            if return_code != 0:
                raise CodexExecutionError(stderr_text.strip() or "".join(stdout_lines).strip())
            raw_message = output_path.read_text().strip() if output_path.exists() else "{}"
            try:
                final_output = json.loads(raw_message)
            except json.JSONDecodeError as exc:
                raise CodexExecutionError(
                    f"Failed to parse structured output: {raw_message}"
                ) from exc
            return CodexRunResult(final_output=final_output, raw_last_message=raw_message)

    def _build_subprocess_env(self) -> dict[str, str]:
        env = os.environ.copy()
        runtime_home = self._prepare_runtime_home()
        if runtime_home is not None:
            env["HOME"] = str(runtime_home)
        return env

    def _prepare_runtime_home(self) -> Path | None:
        if not any(
            [
                self.settings.codex_home_seed_dir,
                self.settings.codex_auth_json_path,
                self.settings.codex_config_toml_path,
            ]
        ):
            return None
        runtime_home = self.settings.codex_runtime_home_dir or (
            self.settings.worker_cache_dir / "codex-runtime-home"
        )
        codex_dir = runtime_home / ".codex"
        codex_dir.mkdir(parents=True, exist_ok=True)
        if self.settings.codex_home_seed_dir:
            self._copy_seed_dir(self.settings.codex_home_seed_dir, codex_dir)
        if self.settings.codex_auth_json_path:
            self._copy_file(self.settings.codex_auth_json_path, codex_dir / "auth.json")
        if self.settings.codex_config_toml_path:
            self._copy_file(self.settings.codex_config_toml_path, codex_dir / "config.toml")
        return runtime_home

    def _copy_seed_dir(self, source_dir: Path, target_dir: Path) -> None:
        self._copy_if_present(source_dir / "auth.json", target_dir / "auth.json")
        self._copy_if_present(source_dir / "config.toml", target_dir / "config.toml")
        source_skills = source_dir / "skills"
        target_skills = target_dir / "skills"
        if source_skills.exists():
            shutil.copytree(source_skills, target_skills, dirs_exist_ok=True)

    def _copy_if_present(self, source: Path, destination: Path) -> None:
        if source.exists():
            self._copy_file(source, destination)

    def _copy_file(self, source: Path, destination: Path) -> None:
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source, destination)
        destination.chmod(0o600)
