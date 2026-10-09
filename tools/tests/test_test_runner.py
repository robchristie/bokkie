"""Exercise check entry points' selection, failure and report boundaries."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
STUB = r'''
import json
import os
from pathlib import Path
import sys

command = [Path(sys.argv[0]).name, *sys.argv[1:]]
with Path(os.environ["COMMAND_LOG"]).open("a") as log:
    log.write(json.dumps(command) + "\n")
args = command[1:]
if args and args[0] == "+1.99.0":
    args = args[1:]
failure = os.environ.get("FAIL_STAGE")
if args == ["nextest", "show-config", "version"] and failure == "preflight":
    sys.exit(91)
if command[0] == "python3" and failure == "setup":
    sys.exit(77)
if args[:2] == ["nextest", "run"]:
    profile = args[args.index("--profile") + 1]
    report = Path(".bokkie/nextest") / profile / "junit.xml"
    report.parent.mkdir(parents=True, exist_ok=True)
    report.write_text("fresh test report")
    if failure == "tests":
        sys.exit(100)
if args and args[0] == "test" and "--doc" in args and failure == "doctests":
    sys.exit(101)
'''


class TestRunnerTests(unittest.TestCase):
    def setUp(self) -> None:
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        (self.root / "tools").mkdir()
        (self.root / "bin").mkdir()
        (self.root / "apps/bokkie-attention-ui/web").mkdir(parents=True)
        (self.root / "apps/bokkie-attention-ui/web/fixture.test.mjs").touch()
        for name in ("check.sh", "check-ui.sh"):
            shutil.copyfile(ROOT / "tools" / name, self.root / "tools" / name)
        for name in ("cargo", "python3", "node"):
            self.stub(self.root / "bin" / name)
        self.stub(self.root / "tools/prepare-web-font.sh")
        self.log = self.root / "commands.jsonl"

    def stub(self, path: Path) -> None:
        path.write_text(f"#!{sys.executable}\n" + STUB)
        path.chmod(0o755)

    def report(self, boundary: str) -> Path:
        return self.root / f".bokkie/nextest/{boundary}-ci/junit.xml"

    def run_check(self, boundary: str, failure: str = "") -> subprocess.CompletedProcess:
        return subprocess.run(
            ["bash", f"tools/{'check-ui' if boundary == 'ui' else 'check'}.sh"],
            cwd=self.root, capture_output=True, text=True,
            env={**os.environ, "PATH": f"{self.root / 'bin'}:{os.environ['PATH']}",
                 "COMMAND_LOG": str(self.log), "FAIL_STAGE": failure},
        )

    def commands(self) -> list[list[str]]:
        return [json.loads(line) for line in self.log.read_text().splitlines()]

    def seed_reports(self) -> None:
        for boundary in ("backend", "ui"):
            report = self.report(boundary)
            report.parent.mkdir(parents=True, exist_ok=True)
            report.write_text("stale report")

    def test_locked_default_backend_and_scoped_ui_selections(self) -> None:
        for boundary, prefix, selection in (
            ("backend", ["cargo"], ["--all-targets", "--locked"]),
            ("ui", ["cargo", "+1.99.0"],
             ["--locked", "-p", "bokkie-attention-ui", "--all-targets"]),
        ):
            with self.subTest(boundary=boundary):
                self.log.unlink(missing_ok=True)
                result = self.run_check(boundary)
                self.assertEqual(result.returncode, 0, result.stderr)
                commands = self.commands()
                self.assertIn(prefix + ["nextest", "run", *selection,
                                        "--profile", f"{boundary}-ci"], commands)
                docs = (["--doc", "--locked"] if boundary == "backend" else
                        ["--locked", "-p", "bokkie-attention-ui", "--doc"])
                self.assertIn(prefix + ["test", *docs], commands)
                self.assertEqual(sum("nextest" in c and "run" in c for c in commands), 1)
                self.assertFalse(any("--workspace" in c or "--all-features" in c
                                     for c in commands if "nextest" in c))
                self.assertTrue(any("clippy" in c for c in commands))
                self.assertTrue(any("fmt" in c for c in commands))
                if boundary == "ui":
                    self.assertTrue(any(c[0] == "node" for c in commands))
                    self.assertTrue(any("wasm32-unknown-unknown" in c for c in commands))

    def test_preflight_or_setup_failure_removes_only_its_stale_report(self) -> None:
        for boundary in ("backend", "ui"):
            for failure, status in (("preflight", 91), ("setup", 77)):
                with self.subTest(boundary=boundary, failure=failure):
                    self.log.unlink(missing_ok=True)
                    self.seed_reports()
                    result = self.run_check(boundary, failure)
                    self.assertEqual(result.returncode, 1 if failure == "preflight" else status)
                    self.assertFalse(self.report(boundary).exists())
                    other = "ui" if boundary == "backend" else "backend"
                    self.assertEqual(self.report(other).read_text(), "stale report")
                    self.assertFalse(any("run" in c or "test" in c for c in self.commands()))

    def test_failed_tests_and_doctests_keep_fresh_report_and_fail_the_check(self) -> None:
        for boundary in ("backend", "ui"):
            for failure, status in (("tests", 100), ("doctests", 101)):
                with self.subTest(boundary=boundary, failure=failure):
                    self.log.unlink(missing_ok=True)
                    self.seed_reports()
                    result = self.run_check(boundary, failure)
                    self.assertEqual(result.returncode, status)
                    self.assertEqual(self.report(boundary).read_text(), "fresh test report")
                    self.assertFalse(any("clippy" in c for c in self.commands()))


if __name__ == "__main__":
    unittest.main()
