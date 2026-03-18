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
    artifacts_dir: Path = Path(".artifacts")
    worker_cache_dir: Path = Path(".worker-cache")
    worker_worktree_dir: Path = Path(".worker-worktrees")
    lease_ttl_seconds: int = 300
    worker_poll_seconds: int = 5
    worker_cleanup_worktrees: bool = False
    telegram_bot_token: str | None = None
    telegram_default_chat_id: str | None = None
    default_codex_model: str | None = None


@lru_cache(maxsize=1)
def get_settings() -> Settings:
    return Settings()
