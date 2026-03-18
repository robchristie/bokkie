from __future__ import annotations

import httpx

from ..config import Settings
from ..enums import RunStatus
from ..schemas import RunRead


class TelegramNotifier:
    def __init__(self, settings: Settings) -> None:
        self.settings = settings
        self.enabled = bool(settings.telegram_bot_token and settings.telegram_default_chat_id)
        self.client = httpx.Client(timeout=30) if self.enabled else None

    def notify_run_checkpoint(self, run: RunRead) -> None:
        if not self.enabled:
            return
        if run.status == RunStatus.WAITING_REVIEW:
            text = (
                f"Review required for run {run.id[:8]}\n"
                f"Stage: {run.current_stage}\n"
                f"Objective: {run.objective}\n"
                f"Summary: {run.latest_summary or 'n/a'}\n"
                f"Next: {run.next_action or 'n/a'}"
            )
        elif run.status == RunStatus.DONE:
            text = (
                f"Run completed {run.id[:8]}\n"
                f"Objective: {run.objective}\n"
                f"Summary: {run.latest_summary or 'n/a'}"
            )
        elif run.status == RunStatus.FAILED:
            text = (
                f"Run failed {run.id[:8]}\n"
                f"Objective: {run.objective}\n"
                f"Next: {run.next_action or 'n/a'}"
            )
        else:
            return
        self.send(text)

    def send(self, text: str) -> None:
        if not self.enabled or self.client is None:
            return
        try:
            self.client.post(
                f"https://api.telegram.org/bot{self.settings.telegram_bot_token}/sendMessage",
                json={
                    "chat_id": self.settings.telegram_default_chat_id,
                    "text": text,
                },
            ).raise_for_status()
        except httpx.HTTPError:
            return
