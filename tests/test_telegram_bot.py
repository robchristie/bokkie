from __future__ import annotations

import httpx

from bokkie.config import Settings
from bokkie.telegram_bot import TelegramBotRunner


def test_telegram_bot_allows_only_matching_chat_and_sender_ids() -> None:
    runner = TelegramBotRunner(
        Settings(
            telegram_bot_token="token",
            telegram_allowed_chat_ids="8760389896",
            telegram_default_chat_id="8760389896",
        )
    )

    assert runner._is_allowed_chat(8760389896, 8760389896) is True
    assert runner._is_allowed_chat(8760389896, 1234) is False
    assert runner._is_allowed_chat(1234, 8760389896) is False


class FakeResponse:
    def __init__(self, payload: dict, status_code: int = 200) -> None:
        self._payload = payload
        self.status_code = status_code

    def json(self) -> dict:
        return self._payload

    def raise_for_status(self) -> None:
        if self.status_code >= 400:
            raise httpx.HTTPStatusError("error", request=None, response=None)


class FakeClient:
    def __init__(self, *args, **kwargs) -> None:
        self.base_url = kwargs.get("base_url")

    def post(self, path: str, json: dict | None = None) -> FakeResponse:
        if path == "/api/campaign-drafts":
            return FakeResponse(
                {
                    "id": "draft-1234",
                    "draft": {
                        "inferred_project_name": "Demo",
                        "campaign_type": "research",
                        "first_run_type": "experiment",
                        "preferred_pool": "cpu-large",
                        "continuation_policy": {"auto_continue": True},
                    },
                }
            )
        return FakeResponse({}, status_code=404)

    def get(self, path: str) -> FakeResponse:
        return FakeResponse({}, status_code=404)


def test_telegram_new_creates_campaign_draft(monkeypatch) -> None:
    monkeypatch.setattr("bokkie.telegram_bot.httpx.Client", FakeClient)
    runner = TelegramBotRunner(
        Settings(telegram_bot_token="token", api_base_url="http://testserver")
    )

    reply = runner.handle_command("/new In demo, continue event research autonomously.")

    assert "Draft draft-1234" in reply
    assert "Campaign type: research" in reply
