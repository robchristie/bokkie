from __future__ import annotations

from functools import lru_cache
from pathlib import Path

from pydantic import Field
from pydantic_settings import BaseSettings, SettingsConfigDict


class Settings(BaseSettings):
    model_config = SettingsConfigDict(env_prefix="BOKKIE_", env_file=".env", extra="ignore")

    app_name: str = "bokkie"
    database_url: str = Field(
        default="sqlite:///./bokkie.db",
        description="SQLAlchemy database URL.",
    )
    api_host: str = "127.0.0.1"
    api_port: int = 8000
    api_base_url: str = "http://127.0.0.1:8000"
    repo_root: Path = Path(".")
    bokkie_config_path: Path = Path("bokkie.toml")
    runs_root: Path = Path(".bokkie/runs")
    artifacts_dir: Path = Path(".bokkie/runs")
    worker_cache_dir: Path = Path(".worker-cache")
    worker_worktree_dir: Path = Path(".worker-worktrees")
    lease_ttl_seconds: int = 300
    worker_poll_seconds: int = 5
    worker_cleanup_worktrees: bool = False
    dispatcher_enabled: bool = False
    dispatcher_poll_seconds: int = 10
    executor_launch_cooldown_seconds: int = 30
    telegram_bot_token: str | None = None
    telegram_default_chat_id: str | None = None
    telegram_allowed_chat_ids: str | None = None
    default_codex_model: str | None = None
    codex_home_seed_dir: Path | None = None
    codex_auth_json_path: Path | None = None
    codex_config_toml_path: Path | None = None
    codex_runtime_home_dir: Path | None = None
    codex_app_server_bin: str = "codex"
    codex_turn_timeout_seconds: int = 1800

    def telegram_allowed_chat_id_set(self) -> set[str]:
        raw = self.telegram_allowed_chat_ids
        if not raw:
            return set()
        return {part.strip() for part in raw.split(",") if part.strip()}

    def resolved_repo_root(self) -> Path:
        return self.repo_root.resolve()

    def resolved_bokkie_config_path(self) -> Path:
        path = self.bokkie_config_path
        if path.is_absolute():
            return path
        return self.resolved_repo_root() / path


@lru_cache(maxsize=1)
def get_settings() -> Settings:
    return Settings()
