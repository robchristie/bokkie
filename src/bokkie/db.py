from __future__ import annotations

from collections.abc import Generator
from dataclasses import dataclass

from sqlalchemy import create_engine, inspect, text
from sqlalchemy.orm import DeclarativeBase, Session, sessionmaker

from .config import get_settings


class Base(DeclarativeBase):
    pass


@dataclass(frozen=True)
class SchemaIssue:
    table: str
    missing_columns: tuple[str, ...]


def _engine_kwargs(database_url: str) -> dict[str, object]:
    if database_url.startswith("sqlite"):
        return {"connect_args": {"check_same_thread": False}}
    return {}


settings = get_settings()
engine = create_engine(settings.database_url, future=True, **_engine_kwargs(settings.database_url))
SessionLocal = sessionmaker(bind=engine, autoflush=False, autocommit=False, future=True)

REQUIRED_COLUMNS: dict[str, tuple[str, ...]] = {
    "leases": (
        "id",
        "phase_attempt_id",
        "worker_id",
        "acquired_at",
        "expires_at",
        "released_at",
        "release_reason",
    ),
    "phase_attempts": (
        "id",
        "run_id",
        "phase_name",
        "phase_index",
        "attempt_no",
    ),
}


def detect_schema_issues() -> list[SchemaIssue]:
    inspector = inspect(engine)
    issues: list[SchemaIssue] = []
    for table_name, required_columns in REQUIRED_COLUMNS.items():
        if not inspector.has_table(table_name):
            continue
        actual_columns = {column["name"] for column in inspector.get_columns(table_name)}
        missing = tuple(column for column in required_columns if column not in actual_columns)
        if missing:
            issues.append(SchemaIssue(table=table_name, missing_columns=missing))
    return issues


def _raise_if_schema_stale() -> None:
    issues = detect_schema_issues()
    if not issues:
        return
    details = "; ".join(
        f"{issue.table} missing columns {', '.join(issue.missing_columns)}" for issue in issues
    )
    raise RuntimeError(
        "Database schema is stale for the current Bokkie version: "
        f"{details}. This project does not have migrations yet. "
        "Reset the database with `uv run bokkie reset-db --yes` or remove the persisted "
        "Postgres data directory under `run/postgres` and start the stack again."
    )


def init_db() -> None:
    from . import models  # noqa: F401

    Base.metadata.create_all(bind=engine)
    _raise_if_schema_stale()


def reset_db() -> None:
    from . import models  # noqa: F401

    Base.metadata.drop_all(bind=engine)
    Base.metadata.create_all(bind=engine)


def database_healthcheck() -> None:
    with engine.connect() as connection:
        connection.execute(text("SELECT 1"))
    _raise_if_schema_stale()


def get_db() -> Generator[Session, None, None]:
    db = SessionLocal()
    try:
        yield db
    finally:
        db.close()
