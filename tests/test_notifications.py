from __future__ import annotations

from bokkie.config import Settings
from bokkie.enums import RunStatus
from bokkie.schemas import RunRead
from bokkie.services.notifications import TelegramNotifier


class FakeClient:
    def __init__(self) -> None:
        self.messages: list[dict[str, object]] = []

    def post(self, url, json):  # noqa: A002
        self.messages.append({"url": url, "json": json})
        return self

    def raise_for_status(self) -> None:
        return None


def build_run(status: str) -> RunRead:
    return RunRead.model_validate(
        {
            "id": "run12345",
            "project_id": "project1",
            "type": "change",
            "task_name": "change",
            "objective": "Ship web-first workflow",
            "success_criteria": "UI covers operator flow",
            "risk_level": "medium",
            "budget": {},
            "resource_profile": {},
            "current_stage": "plan_review",
            "current_session_id": None,
            "status": status,
            "base_ref": "main",
            "branch_name": "bokkie/run-1234",
            "run_root": "/tmp/.bokkie/runs/run12345",
            "latest_summary": "Summary text",
            "current_worker_id": None,
            "latest_verifier_result": None,
            "next_action": "Wait",
            "blockers": [],
            "risk_flags": [],
            "preferred_pool": None,
            "requires_internet": False,
            "required_secrets": [],
            "publish_strategy": "none",
            "created_at": "2026-03-18T00:00:00Z",
            "updated_at": "2026-03-18T00:00:00Z",
            "started_at": None,
            "completed_at": None,
            "phase_attempts": [],
            "reviews": [],
            "artifacts": [],
            "events": [],
        }
    )


def test_notifier_only_sends_for_key_checkpoints() -> None:
    notifier = TelegramNotifier(
        Settings(
            telegram_bot_token="token",
            telegram_default_chat_id="8760389896",
        )
    )
    notifier.client = FakeClient()

    notifier.notify_run_checkpoint(build_run(RunStatus.QUEUED.value))
    notifier.notify_run_checkpoint(build_run(RunStatus.WAITING_REVIEW.value))
    notifier.notify_run_checkpoint(build_run(RunStatus.DONE.value))
    notifier.notify_run_checkpoint(build_run(RunStatus.FAILED.value))

    assert len(notifier.client.messages) == 3
