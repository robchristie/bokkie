# Bokkie Agent Rules

- Treat the control plane as the source of truth. Workers should be thin and disposable.
- Preserve the run-stage state machine and explicit review gates.
- Prefer structured JSON outputs over scraping free-form text from Codex.
- Do not assume a single worker host; repo state must be reconstructible without shared writable storage.
- Keep operator steering at safe boundaries, not arbitrary process interruption.
- Use `uv` for local commands.
- Validation commands for this repo are:
  - `uv run ruff format --check src tests`
  - `uv run ruff check src tests`
  - `uv run pytest -q`
