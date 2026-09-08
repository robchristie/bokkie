from __future__ import annotations

import importlib.util
import subprocess
import sys
import tempfile
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

    def test_acceptance_complete_needs_no_future_merge_identity(self) -> None:
        self.assertEqual(self.messages("valid-acceptance-complete.md", "completed"), [])

    def test_acceptance_complete_requires_acceptance_and_owning_pr(self) -> None:
        candidate = (FIXTURES / "valid-acceptance-complete.md").read_text()
        mutations = (
            ("- Acceptance state: passed", "", "Acceptance State"),
            ("- Acceptance state: passed", "- Acceptance state: pending", "passed acceptance"),
            ("- Acceptance evidence: [Verification results](../../evidence/README.md)", "", "Acceptance Evidence"),
            ("[Verification results](../../evidence/README.md)", "checks passed", "must link"),
            ("- Landing evidence: https://github.com/robchristie/bokkie/pull/23", "", "Landing Evidence"),
            ("https://github.com/robchristie/bokkie/pull/23", "this PR", "owning Bokkie"),
            ("https://github.com/robchristie/bokkie/pull/23", "https://github.com/other/repo/pull/23", "owning Bokkie"),
            ("acceptance-complete", "complete", "delivery state"),
            ("- Acceptance state: passed", "- Acceptance state: passed\n- Acceptance state: passed", "duplicate lifecycle"),
        )
        for original, replacement, expected in mutations:
            with self.subTest(replacement=replacement), tempfile.TemporaryDirectory() as directory:
                path = Path(directory) / "candidate.md"
                path.write_text(candidate.replace(original, replacement))
                messages = [problem.message for problem in PLAN_LINT.lint_plan(path, "completed")]
                self.assertTrue(any(expected in message for message in messages), messages)

    def test_acceptance_complete_preserves_unfinished_work_guards(self) -> None:
        candidate = (FIXTURES / "valid-acceptance-complete.md").read_text()
        mutations = (
            ("- [x]", "- [ ]", "terminal plan items"),
            ("- [x]", "- [~]", "terminal plan items"),
            ("## Acceptance", "## Current phase", "active phase"),
            ("## Acceptance", "## Next action", "active phase"),
            ("Required product behaviour verified.", "Product checks still need to pass.", "as pending"),
            ("Required product behaviour verified.", "Review is pending.", "as pending"),
            ("Required product behaviour verified.", "Worktree: /tmp/worktrees/current", "worktree"),
        )
        for original, replacement, expected in mutations:
            with self.subTest(replacement=replacement), tempfile.TemporaryDirectory() as directory:
                path = Path(directory) / "candidate.md"
                path.write_text(candidate.replace(original, replacement))
                messages = [problem.message for problem in PLAN_LINT.lint_plan(path, "completed")]
                self.assertTrue(any(expected in message for message in messages), messages)

    def test_acceptance_complete_cannot_claim_future_delivery_facts(self) -> None:
        candidate = (FIXTURES / "valid-acceptance-complete.md").read_text()
        for field in ("Review state: passed", "CI state: passed", "Merge state: landed",
                      "Landed commit: " + "a" * 40, "Landed date: 2026-09-08"):
            with self.subTest(field=field), tempfile.TemporaryDirectory() as directory:
                path = Path(directory) / "candidate.md"
                path.write_text(candidate + "\n- " + field + "\n")
                messages = [problem.message for problem in PLAN_LINT.lint_plan(path, "completed")]
                self.assertTrue(any("in PR landing evidence" in message for message in messages), messages)

    def test_valid_active_plan(self) -> None:
        self.assertEqual(self.messages("valid-active.md", "active"), [])

    def test_completed_metadata_and_terminal_state_are_enforced(self) -> None:
        messages = self.messages("invalid-completed.md", "completed")
        self.assertTrue(any("delivery state" in message for message in messages))
        self.assertTrue(any("landed commit" in message for message in messages))
        self.assertTrue(any("landed date" in message for message in messages))
        self.assertTrue(any("Review State" in message for message in messages))
        self.assertTrue(any("Ci State" in message for message in messages))
        self.assertTrue(any("Merge State" in message for message in messages))
        self.assertTrue(any("terminal plan items" in message for message in messages))
        self.assertTrue(any("as pending" in message for message in messages))
        self.assertTrue(any("active phase" in message for message in messages))
        self.assertTrue(any("worktree" in message for message in messages))

    def test_active_budget_phase_and_landed_pr_truth_are_enforced(self) -> None:
        messages = self.messages("invalid-active.md", "active")
        self.assertTrue(any("budget cannot exceed" in message for message in messages))
        self.assertTrue(any("exactly one" in message for message in messages))
        self.assertTrue(any("recorded landed pull request" in message for message in messages))

    def test_active_landed_prs_are_absent_from_imperative_and_review_text(self) -> None:
        for fixture in ("invalid-active-imperative.md", "invalid-active-needs-review.md"):
            with self.subTest(fixture=fixture):
                messages = self.messages(fixture, "active")
                self.assertTrue(any("recorded landed pull request" in message for message in messages))

    def test_active_next_action_is_unique(self) -> None:
        messages = self.messages("invalid-active-duplicate-next.md", "active")
        self.assertTrue(any("duplicate lifecycle field 'next action'" in message for message in messages))

    def test_completed_commonmark_task_marker_is_enforced(self) -> None:
        messages = self.messages("invalid-completed-star-task.md", "completed")
        self.assertTrue(any("terminal plan items" in message for message in messages))

    def test_completed_unfinished_ci_wording_is_enforced(self) -> None:
        messages = self.messages("invalid-completed-needs-ci.md", "completed")
        self.assertTrue(any("as pending" in message for message in messages))

    def test_completed_worktree_metadata_is_structural(self) -> None:
        messages = self.messages("invalid-completed-worktree-field.md", "completed")
        self.assertTrue(any("worktree metadata" in message for message in messages))

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
