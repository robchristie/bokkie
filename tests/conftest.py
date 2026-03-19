from __future__ import annotations

from collections.abc import Generator

import pytest
from sqlalchemy import create_engine
from sqlalchemy.orm import Session, sessionmaker
from sqlalchemy.pool import StaticPool

from bokkie import models  # noqa: F401
from bokkie.config import Settings
from bokkie.db import Base


@pytest.fixture()
def settings(tmp_path) -> Settings:
    return Settings(
        database_url="sqlite:///:memory:",
        api_base_url="http://testserver",
        runs_root=tmp_path / ".bokkie" / "runs",
        artifacts_dir=tmp_path / ".bokkie" / "runs",
        worker_cache_dir=tmp_path / "cache",
        worker_worktree_dir=tmp_path / "worktrees",
        worker_cleanup_worktrees=True,
    )


@pytest.fixture()
def session(settings: Settings) -> Generator[Session, None, None]:
    engine = create_engine(
        settings.database_url,
        connect_args={"check_same_thread": False},
        future=True,
        poolclass=StaticPool,
    )
    Base.metadata.create_all(bind=engine)
    session_local = sessionmaker(bind=engine, autoflush=False, autocommit=False, future=True)
    with session_local() as db:
        yield db
