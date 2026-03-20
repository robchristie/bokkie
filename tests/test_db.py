from __future__ import annotations

from sqlalchemy import create_engine, text

import bokkie.db as db_module


def test_detect_schema_issues_reports_stale_leases_table(tmp_path, monkeypatch) -> None:
    db_path = tmp_path / "stale.db"
    engine = create_engine(f"sqlite:///{db_path}", future=True)
    with engine.begin() as connection:
        connection.execute(
            text(
                """
                CREATE TABLE leases (
                    id TEXT PRIMARY KEY,
                    worker_id TEXT,
                    acquired_at TEXT,
                    expires_at TEXT,
                    released_at TEXT,
                    release_reason TEXT
                )
                """
            )
        )

    monkeypatch.setattr(db_module, "engine", engine)
    issues = db_module.detect_schema_issues()
    assert issues
    assert issues[0].table == "leases"
    assert "phase_attempt_id" in issues[0].missing_columns
