from __future__ import annotations

import time

import httpx

from .config import Settings


class TelegramBotRunner:
    def __init__(self, settings: Settings) -> None:
        if not settings.telegram_bot_token:
            raise RuntimeError("BOKKIE_TELEGRAM_BOT_TOKEN is required")
        self.settings = settings
        self.token = settings.telegram_bot_token
        self.client = httpx.Client(timeout=60)
        self.offset = 0

    def run_forever(self) -> None:
        while True:
            updates = self.client.get(
                f"https://api.telegram.org/bot{self.token}/getUpdates",
                params={"timeout": 30, "offset": self.offset},
            )
            updates.raise_for_status()
            for update in updates.json().get("result", []):
                self.offset = update["update_id"] + 1
                message = update.get("message", {})
                text = message.get("text", "")
                chat_id = message.get("chat", {}).get("id")
                if not chat_id or not text.startswith("/"):
                    continue
                reply = self.handle_command(text)
                self.client.post(
                    f"https://api.telegram.org/bot{self.token}/sendMessage",
                    json={"chat_id": chat_id, "text": reply},
                ).raise_for_status()
            time.sleep(1)

    def handle_command(self, text: str) -> str:
        parts = text.strip().split(maxsplit=2)
        command = parts[0]
        api = httpx.Client(base_url=self.settings.api_base_url, timeout=60)
        if command == "/runs":
            response = api.get("/api/runs")
            response.raise_for_status()
            runs = response.json()
            if not runs:
                return "No runs found."
            return "\n".join(
                f"{run['id'][:8]} {run['status']} {run['current_stage']} {run['objective']}"
                for run in runs[:10]
            )
        if command == "/run" and len(parts) >= 2:
            response = api.get(f"/api/runs/{parts[1]}")
            response.raise_for_status()
            run = response.json()
            return (
                f"Run {run['id']}\n"
                f"Status: {run['status']}\n"
                f"Stage: {run['current_stage']}\n"
                f"Summary: {run.get('latest_summary') or 'n/a'}\n"
                f"Next: {run.get('next_action') or 'n/a'}"
            )
        if command in {"/approve", "/reject", "/pause", "/resume"} and len(parts) >= 2:
            run_id = parts[1]
            if command == "/approve":
                response = api.post(
                    f"/api/runs/{run_id}/approve", json={"reason": None, "actor": "telegram"}
                )
            elif command == "/reject":
                reason = parts[2] if len(parts) > 2 else "Rejected from Telegram"
                response = api.post(
                    f"/api/runs/{run_id}/reject", json={"reason": reason, "actor": "telegram"}
                )
            elif command == "/pause":
                response = api.post(f"/api/runs/{run_id}/pause")
            else:
                response = api.post(f"/api/runs/{run_id}/resume")
            response.raise_for_status()
            return f"{command[1:]} accepted for {run_id}"
        if command == "/steer" and len(parts) >= 3:
            run_id = parts[1]
            note = parts[2]
            response = api.post(
                f"/api/runs/{run_id}/steer", json={"note": note, "created_by": "telegram"}
            )
            response.raise_for_status()
            return f"Steering note stored for {run_id}"
        if command == "/promote" and len(parts) >= 3:
            run_id = parts[1]
            pool = parts[2]
            response = api.post(f"/api/runs/{run_id}/promote", json={"pool": pool})
            response.raise_for_status()
            return f"Promoted {run_id} to {pool}"
        return "Unknown command."
