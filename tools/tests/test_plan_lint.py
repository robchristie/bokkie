from __future__ import annotations

import importlib.util
import subprocess
import sys
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
LINTER = ROOT / "tools" / "plan_lint.py"
FIXTURES = ROOT / "tools" / "tests" / "fixtures"
SPEC = importlib.util.spec_from_file_location("plan_lint", LINTER)
assert SPEC and SPEC.loader
PLAN_LINT = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = PLAN_LINT
SPEC.loader.exec_module(PLAN_LINT)


class PlanLintTests(unittest.TestCase):
    def messages(self, name: str, kind: str) -> list[str]:
        return [problem.message for problem in PLAN_LINT.lint_plan(FIXTURES / name, kind)]

    def test_valid_completed_plan(self) -> None:
        self.assertEqual(self.messages("valid-completed.md", "completed"), [])

    def test_valid_active_plan(self) -> None:
        self.assertEqual(self.messages("valid-active.md", "active"), [])

    def test_completed_metadata_and_terminal_state_are_enforced(self) -> None:
        messages = self.messages("invalid-completed.md", "completed")
        self.assertTrue(any("delivery state" in message for message in messages))
        self.assertTrue(any("landed commit" in message for message in messages))
        self.assertTrue(any("landed date" in message for message in messages))
        self.assertTrue(any("terminal plan items" in message for message in messages))
        self.assertTrue(any("as pending" in message for message in messages))
        self.assertTrue(any("active phase" in message for message in messages))
        self.assertTrue(any("labelled historical" in message for message in messages))

    def test_active_budget_phase_and_landed_pr_truth_are_enforced(self) -> None:
        messages = self.messages("invalid-active.md", "active")
        self.assertTrue(any("budget cannot exceed" in message for message in messages))
        self.assertTrue(any("exactly one" in message for message in messages))
        self.assertTrue(any("recorded landed pull request" in message for message in messages))

    def test_repository_fixture_is_accepted_by_cli(self) -> None:
        result = subprocess.run(
            [sys.executable, str(LINTER), str(FIXTURES / "repository")],
            check=False,
            capture_output=True,
            text=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("2 plan(s)", result.stdout)


if __name__ == "__main__":
    unittest.main()
