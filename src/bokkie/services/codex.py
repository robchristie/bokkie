from __future__ import annotations

import json
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

            process = subprocess.Popen(
                command,
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
