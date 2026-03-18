from __future__ import annotations

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

