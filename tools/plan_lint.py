#!/usr/bin/env python3
"""Validate durable plan lifecycle metadata without network access."""

from __future__ import annotations

import argparse
import re
import sys
from dataclasses import dataclass
from datetime import date
from pathlib import Path


FIELD_RE = re.compile(r"^- ([A-Za-z][A-Za-z -]+):\s*(.+?)\s*$")
COMMIT_RE = re.compile(r"^[0-9a-f]{40}$")
CHECKBOX_RE = re.compile(r"^\s*- \[([^]])\]\s*(.*)$")
WORKTREE_PATH_RE = re.compile(r"/(?:[^\s`|]*/)*[^\s`|]*worktrees?/[^\s`|]+")
PR_RE = re.compile(r"(?:PR\s*)?#(\d+)", re.IGNORECASE)
PENDING_RE = re.compile(
    r"\b(?:pending|next|await(?:s|ing)?|"
    r"remaining\s+(?:delivery|review|ci|checks?|merge|landing)|"
    r"(?:review|ci|checks?|merge|landing)\s+remains?)\b",
    re.IGNORECASE,
)
TERMINAL_RE = re.compile(r"\b(?:review|CI|check(?:s)?|merge|landing|landed)\b", re.IGNORECASE)


@dataclass(frozen=True)
class Problem:
    path: Path
    line: int
    message: str

    def render(self) -> str:
        return f"{self.path}:{self.line}: {self.message}"


def fields(lines: list[str]) -> dict[str, tuple[str, int]]:
    result: dict[str, tuple[str, int]] = {}
    for number, line in enumerate(lines, 1):
        match = FIELD_RE.match(line)
        if match:
            result[match.group(1).lower()] = (match.group(2), number)
    return result


def section(lines: list[str], heading: str) -> tuple[str, int] | None:
    marker = f"## {heading}"
    indexes = [index for index, line in enumerate(lines) if line == marker]
    if len(indexes) != 1:
        return None
    start = indexes[0]
    end = next(
        (index for index in range(start + 1, len(lines)) if lines[index].startswith("## ")),
        len(lines),
    )
    return "\n".join(lines[start + 1 : end]), start + 1


def lint_plan(path: Path, kind: str) -> list[Problem]:
    lines = path.read_text(encoding="utf-8").splitlines()
    metadata = fields(lines)
    problems: list[Problem] = []

    def require_field(name: str) -> tuple[str, int] | None:
        value = metadata.get(name)
        if value is None:
            problems.append(Problem(path, 1, f"missing '- {name.title()}: …' metadata"))
        return value

    status = require_field("status")
    expected_status = "active" if kind == "active" else "complete"
    if status and status[0].lower() != expected_status:
        problems.append(Problem(path, status[1], f"status must be {expected_status!r} in {kind}/"))

    for number, line in enumerate(lines, 1):
        if WORKTREE_PATH_RE.search(line) and "historical" not in line.lower():
            problems.append(
                Problem(path, number, "retained worktree paths must be explicitly labelled historical")
            )

    for number, line in enumerate(lines, 1):
        checkbox = CHECKBOX_RE.match(line)
        if not checkbox:
            continue
        marker, text = checkbox.groups()
        if marker.lower() == "x":
            continue
        if marker == "~" and re.search(r"\bWaived:\s*\S", text, re.IGNORECASE):
            continue
        problems.append(
            Problem(path, number, "terminal plan items must be checked or '[~] … Waived: <reason>'")
        )

    if kind == "completed":
        delivery = require_field("delivery state")
        if delivery and delivery[0].lower() != "landed":
            problems.append(Problem(path, delivery[1], "completed-plan delivery state must be 'landed'"))

        landed_commit = require_field("landed commit")
        if landed_commit and not COMMIT_RE.fullmatch(landed_commit[0].strip("`")):
            problems.append(Problem(path, landed_commit[1], "landed commit must be a full lowercase Git commit"))

        landed_date = require_field("landed date")
        if landed_date:
            try:
                if not re.fullmatch(r"\d{4}-\d{2}-\d{2}", landed_date[0]):
                    raise ValueError("date is not dashed ISO form")
                date.fromisoformat(landed_date[0])
            except ValueError:
                problems.append(Problem(path, landed_date[1], "landed date must be YYYY-MM-DD"))

        for number, line in enumerate(lines, 1):
            if line in {"## Current phase", "## Next action"}:
                problems.append(Problem(path, number, "completed plans cannot retain active phase headings"))
            if PENDING_RE.search(line) and TERMINAL_RE.search(line) and "historical" not in line.lower():
                problems.append(
                    Problem(
                        path,
                        number,
                        "completed plans cannot describe review, CI, checks, merge or landing as pending",
                    )
                )
    else:
        budget = require_field("reorientation budget")
        if budget:
            try:
                maximum = int(budget[0])
            except ValueError:
                problems.append(Problem(path, budget[1], "reorientation budget must be an integer"))
            else:
                if maximum > 200:
                    problems.append(Problem(path, budget[1], "active-plan budget cannot exceed 200 lines"))
                if len(lines) > maximum:
                    problems.append(
                        Problem(path, len(lines), f"active plan has {len(lines)} lines, exceeding budget {maximum}")
                    )

        phase_headers = [number for number, line in enumerate(lines, 1) if line == "## Current phase"]
        if len(phase_headers) != 1:
            problems.append(Problem(path, 1, "active plan must have exactly one '## Current phase'"))

        next_action = require_field("next action")
        landed = require_field("landed pull requests")
        current = section(lines, "Current phase")
        if next_action and landed:
            landed_prs = {int(number) for number in re.findall(r"#(\d+)", landed[0])}
            surfaces = [(next_action[0], next_action[1])]
            if current:
                surfaces.append(current)
            for surface, line_number in surfaces:
                for paragraph in re.split(r"\n\s*\n", surface):
                    mentioned = {int(match) for match in PR_RE.findall(paragraph)}
                    if mentioned & landed_prs and PENDING_RE.search(paragraph):
                        problems.append(
                            Problem(
                                path,
                                line_number,
                                "current phase/next action treats a recorded landed pull request as pending",
                            )
                        )
                        break

    return problems


def discover(root: Path) -> list[tuple[Path, str]]:
    plans: list[tuple[Path, str]] = []
    for kind in ("active", "completed"):
        plans.extend((path, kind) for path in sorted((root / kind).glob("*.md")))
    return plans


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "plan_root",
        nargs="?",
        type=Path,
        default=Path("docs/plans"),
        help="directory containing active/ and completed/ (default: docs/plans)",
    )
    args = parser.parse_args(argv)
    plans = discover(args.plan_root)
    if not plans:
        print(f"no plan files found beneath {args.plan_root}", file=sys.stderr)
        return 2
    problems = [problem for path, kind in plans for problem in lint_plan(path, kind)]
    for problem in problems:
        print(problem.render(), file=sys.stderr)
    if problems:
        print(f"plan lint failed with {len(problems)} problem(s)", file=sys.stderr)
        return 1
    print(f"plan lint passed for {len(plans)} plan(s)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
