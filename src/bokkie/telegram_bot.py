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
        self.allowed_chat_ids = settings.telegram_allowed_chat_id_set()
        if (
            settings.telegram_default_chat_id
            and self.allowed_chat_ids
            and settings.telegram_default_chat_id not in self.allowed_chat_ids
        ):
            raise RuntimeError(
                "BOKKIE_TELEGRAM_DEFAULT_CHAT_ID must be in BOKKIE_TELEGRAM_ALLOWED_CHAT_IDS"
            )

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
                from_user_id = message.get("from", {}).get("id")
                if not chat_id or not text.startswith("/"):
                    continue
                if not self._is_allowed_chat(chat_id, from_user_id):
                    continue
                reply = self.handle_command(text)
                self.client.post(
                    f"https://api.telegram.org/bot{self.token}/sendMessage",
                    json={"chat_id": chat_id, "text": reply},
                ).raise_for_status()
            time.sleep(1)

    def _is_allowed_chat(self, chat_id: int | str | None, from_user_id: int | str | None) -> bool:
        if not self.allowed_chat_ids:
            return True
        chat_text = str(chat_id) if chat_id is not None else None
        user_text = str(from_user_id) if from_user_id is not None else None
        return chat_text in self.allowed_chat_ids and user_text in self.allowed_chat_ids

    def handle_command(self, text: str) -> str:
        stripped = text.strip()
        parts = stripped.split(maxsplit=2)
        command = parts[0]
        remainder = stripped[len(command) :].strip()
        api = httpx.Client(base_url=self.settings.api_base_url, timeout=60)
        if command == "/new" and remainder:
            response = api.post("/api/campaign-drafts", json={"prompt": remainder})
            response.raise_for_status()
            draft = response.json()
            return (
                f"Draft {draft['id']}\n"
                f"Project: {draft['draft'].get('inferred_project_name') or 'unresolved'}\n"
                f"Campaign type: {draft['draft']['campaign_type']}\n"
                f"First run: {draft['draft']['first_run_type']}\n"
                f"Pool: {draft['draft'].get('preferred_pool') or 'n/a'}\n"
                f"Auto continue: {draft['draft']['continuation_policy']['auto_continue']}\n"
                f"Use /approve {draft['id']} to create the campaign or /reject {draft['id']} <reason>."
            )
        if command == "/campaigns":
            response = api.get("/api/campaigns")
            response.raise_for_status()
            campaigns = response.json()
            if not campaigns:
                return "No campaigns found."
            return "\n".join(
                f"{campaign['id'][:8]} {campaign['status']} iter={campaign['current_iteration_no']} {campaign['title']}"
                for campaign in campaigns[:10]
            )
        if command == "/campaign" and len(parts) >= 2:
            response = api.get(f"/api/campaigns/{parts[1]}")
            response.raise_for_status()
            campaign = response.json()
            return (
                f"Campaign {campaign['id']}\n"
                f"Status: {campaign['status']}\n"
                f"Iteration: {campaign['current_iteration_no']}\n"
                f"Summary: {campaign.get('latest_summary') or 'n/a'}\n"
                f"Next: {campaign.get('next_action') or 'n/a'}"
            )
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
            if command == "/approve":
                target_id = parts[1]
                response = api.post(f"/api/campaign-drafts/{target_id}/approve", json={})
                if response.status_code < 400:
                    campaign = response.json()
                    return f"Draft approved. Campaign {campaign['id']} created."
                response = api.post(
                    f"/api/campaigns/{target_id}/approve",
                    json={"reason": None, "actor": "telegram"},
                )
                if response.status_code < 400:
                    return f"Campaign approval accepted for {target_id}"
                response = api.post(
                    f"/api/runs/{target_id}/approve", json={"reason": None, "actor": "telegram"}
                )
            elif command == "/reject":
                target_id = parts[1]
                reason = parts[2] if len(parts) > 2 else "Rejected from Telegram"
                response = api.post(
                    f"/api/campaign-drafts/{target_id}/reject",
                    json={"reason": reason, "actor": "telegram"},
                )
                if response.status_code < 400:
                    return f"Draft {target_id} rejected"
                response = api.post(
                    f"/api/campaigns/{target_id}/reject",
                    json={"reason": reason, "actor": "telegram"},
                )
                if response.status_code < 400:
                    return f"Campaign gate rejected for {target_id}"
                response = api.post(
                    f"/api/runs/{target_id}/reject", json={"reason": reason, "actor": "telegram"}
                )
            elif command == "/pause":
                run_id = parts[1]
                response = api.post(f"/api/runs/{run_id}/pause")
            else:
                run_id = parts[1]
                response = api.post(f"/api/runs/{run_id}/resume")
            response.raise_for_status()
            return f"{command[1:]} accepted for {parts[1]}"
        if command == "/steer_campaign" and len(parts) >= 3:
            campaign_id = parts[1]
            note = parts[2]
            response = api.post(
                f"/api/campaigns/{campaign_id}/steer",
                json={"note": note, "created_by": "telegram"},
            )
            response.raise_for_status()
            return f"Campaign steering note stored for {campaign_id}"
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
