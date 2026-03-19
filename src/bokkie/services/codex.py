from __future__ import annotations

import json
import os
import select
import shutil
import subprocess
import time
from collections.abc import Callable
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from pydantic import BaseModel

from ..config import Settings


class CodexExecutionError(RuntimeError):
    pass


@dataclass
class CodexRunResult:
    thread_id: str
    turn_id: str
    final_output: dict[str, Any]
    raw_last_message: str


def _closed_json_schema(schema: dict[str, Any]) -> dict[str, Any]:
    if not isinstance(schema, dict):
        return schema
    cloned: dict[str, Any] = {}
    for key, value in schema.items():
        if isinstance(value, dict):
            cloned[key] = _closed_json_schema(value)
        elif isinstance(value, list):
            cloned[key] = [
                _closed_json_schema(item) if isinstance(item, dict) else item for item in value
            ]
        else:
            cloned[key] = value
    if cloned.get("type") == "object" and "additionalProperties" not in cloned:
        cloned["additionalProperties"] = False
    return cloned


class AppServerSession:
    def __init__(self, settings: Settings) -> None:
        self.settings = settings
        self.process: subprocess.Popen[str] | None = None
        self._request_id = 0
        self._runtime_home: Path | None = None

    def __enter__(self) -> AppServerSession:
        env = os.environ.copy()
        self._runtime_home = self._prepare_runtime_home()
        if self._runtime_home is not None:
            env["HOME"] = str(self._runtime_home)
        self.process = subprocess.Popen(
            [self.settings.codex_app_server_bin, "app-server"],
            env=env,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            bufsize=1,
        )
        self.request(
            "initialize",
            {
                "clientInfo": {
                    "name": "bokkie",
                    "version": "0.1.0",
                },
                "capabilities": {},
            },
        )
        return self

    def __exit__(self, exc_type, exc, tb) -> None:
        if self.process is not None and self.process.poll() is None:
            self.process.kill()
            self.process.wait(timeout=5)

    def request(
        self,
        method: str,
        params: dict[str, Any],
        *,
        on_notification: Callable[[dict[str, Any]], None] | None = None,
        timeout_seconds: int = 30,
    ) -> dict[str, Any]:
        assert self.process is not None
        assert self.process.stdin is not None
        self._request_id += 1
        request_id = self._request_id
        self.process.stdin.write(
            json.dumps({"id": request_id, "method": method, "params": params}) + "\n"
        )
        self.process.stdin.flush()
        end = time.time() + timeout_seconds
        while time.time() < end:
            message = self._read_message(timeout_seconds=0.5)
            if message is None:
                continue
            if "id" in message:
                if message["id"] != request_id:
                    continue
                if "error" in message:
                    raise CodexExecutionError(message["error"]["message"])
                return message["result"]
            if on_notification:
                on_notification(message)
        raise CodexExecutionError(f"Timed out waiting for {method} response")

    def _read_message(self, timeout_seconds: float) -> dict[str, Any] | None:
        assert self.process is not None
        assert self.process.stdout is not None
        streams = [self.process.stdout]
        if self.process.stderr is not None:
            streams.append(self.process.stderr)
        ready, _, _ = select.select(streams, [], [], timeout_seconds)
        for stream in ready:
            line = stream.readline()
            if not line:
                continue
            if stream is self.process.stderr:
                continue
            return json.loads(line)
        return None

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


class CodexAppServerBackend:
    def __init__(self, settings: Settings) -> None:
        self.settings = settings

    def run(
        self,
        worktree: Path,
        prompt: str,
        schema_model: type[BaseModel],
        *,
        writable: bool,
        internet: bool,
        on_event: Callable[[dict[str, Any]], None] | None = None,
        steering_supplier: Callable[[], list[str]] | None = None,
    ) -> CodexRunResult:
        schema = _closed_json_schema(schema_model.model_json_schema())
        with AppServerSession(self.settings) as session:
            thread_result = session.request(
                "thread/start",
                {
                    "cwd": str(worktree),
                    "approvalPolicy": "never",
                    "ephemeral": False,
                    **({"model": self.settings.default_codex_model} if self.settings.default_codex_model else {}),
                },
                on_notification=on_event,
            )
            thread_id = thread_result["thread"]["id"]
            turn_result = session.request(
                "turn/start",
                {
                    "threadId": thread_id,
                    "cwd": str(worktree),
                    "input": [{"type": "text", "text": prompt}],
                    "outputSchema": schema,
                    "sandboxPolicy": self._sandbox_policy(
                        writable=writable,
                        internet=internet,
                        worktree=worktree,
                    ),
                    **({"model": self.settings.default_codex_model} if self.settings.default_codex_model else {}),
                },
                on_notification=on_event,
            )
            turn_id = turn_result["turn"]["id"]
            raw_last_message = self._wait_for_turn_completion(
                session,
                thread_id=thread_id,
                turn_id=turn_id,
                on_event=on_event,
                steering_supplier=steering_supplier,
            )
            try:
                final_output = json.loads(raw_last_message)
            except json.JSONDecodeError as exc:
                raise CodexExecutionError(
                    f"Failed to parse structured output: {raw_last_message}"
                ) from exc
            return CodexRunResult(
                thread_id=thread_id,
                turn_id=turn_id,
                final_output=final_output,
                raw_last_message=raw_last_message,
            )

    def _wait_for_turn_completion(
        self,
        session: AppServerSession,
        *,
        thread_id: str,
        turn_id: str,
        on_event: Callable[[dict[str, Any]], None] | None,
        steering_supplier: Callable[[], list[str]] | None,
    ) -> str:
        last_message = ""
        last_note_poll = 0.0
        deadline = time.time() + self.settings.codex_turn_timeout_seconds
        while time.time() < deadline:
            message = session._read_message(timeout_seconds=0.5)
            if message is not None:
                if "method" in message and on_event:
                    on_event(message)
                method = message.get("method")
                params = message.get("params", {})
                if method == "item/completed":
                    item = params.get("item", {})
                    if item.get("type") == "agentMessage" and item.get("phase") == "final_answer":
                        last_message = item.get("text", "")
                if method == "error":
                    error = params.get("error", {})
                    raise CodexExecutionError(error.get("message", "codex app-server error"))
                if method == "turn/completed" and params.get("turn", {}).get("id") == turn_id:
                    turn = params["turn"]
                    if turn.get("status") != "completed":
                        error = turn.get("error") or {}
                        raise CodexExecutionError(error.get("message", "Codex turn failed"))
                    if not last_message:
                        raise CodexExecutionError("Codex completed without a final structured message")
                    return last_message
            if (
                steering_supplier is not None
                and time.time() - last_note_poll >= 1.0
            ):
                notes = [note.strip() for note in steering_supplier() if note.strip()]
                last_note_poll = time.time()
                if notes:
                    session.request(
                        "turn/steer",
                        {
                            "threadId": thread_id,
                            "expectedTurnId": turn_id,
                            "input": [
                                {
                                    "type": "text",
                                    "text": "Operator steering note:\n" + "\n".join(f"- {note}" for note in notes),
                                }
                            ],
                        },
                        on_notification=on_event,
                    )
        raise CodexExecutionError("Timed out waiting for Codex turn completion")

    def _sandbox_policy(self, *, writable: bool, internet: bool, worktree: Path) -> dict[str, Any]:
        if writable:
            return {
                "type": "workspaceWrite",
                "writableRoots": [str(worktree)],
                "networkAccess": internet,
            }
        return {
            "type": "readOnly",
            "networkAccess": internet,
        }
