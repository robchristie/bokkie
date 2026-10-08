from __future__ import annotations

import importlib.util
import shutil
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
CHECKER = ROOT / "tools" / "toolchain_contract.py"
SPEC = importlib.util.spec_from_file_location("toolchain_contract", CHECKER)
assert SPEC and SPEC.loader
CONTRACT = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = CONTRACT
SPEC.loader.exec_module(CONTRACT)


class ToolchainContractTests(unittest.TestCase):
    def test_repository_contract_is_exact(self) -> None:
        self.assertEqual(CONTRACT.validate(ROOT), [])

    def changed_contract(self, relative: str, old: str, new: str) -> list[str]:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for name in (
                "rust-toolchain.toml", "apps/bokkie-attention-ui/rust-toolchain.toml",
                "Cargo.toml", "crates/bokkie-operator-api/Cargo.toml",
                "apps/bokkie-attention-ui/Cargo.toml", "Cargo.lock",
                *CONTRACT.SELECTOR_FILES,
            ):
                destination = root / name
                destination.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(ROOT / name, destination)
            path = root / relative
            text = path.read_text()
            self.assertIn(old, text)
            path.write_text(text.replace(old, new, 1))
            return CONTRACT.validate(root)

    def test_minimum_compiler_is_independent_of_build_pin(self) -> None:
        for relative, minimum in (("Cargo.toml", "1.85"),
                                  ("crates/bokkie-operator-api/Cargo.toml", "1.85"),
                                  ("apps/bokkie-attention-ui/Cargo.toml", "1.97")):
            with self.subTest(relative=relative):
                problems = self.changed_contract(relative, f'rust-version = "{minimum}"',
                                                 'rust-version = "1.99"')
                self.assertTrue(any("package.rust-version" in problem for problem in problems))

    def test_script_and_ci_selector_drift_is_rejected(self) -> None:
        for relative, old in (("tools/check-ui.sh", "+1.99.0"),
                              ("tools/qualify-ui.sh", "+1.99.0"),
                              (".github/workflows/ci.yml", "RUSTUP_TOOLCHAIN: 1.99.0")):
            with self.subTest(relative=relative):
                problems = self.changed_contract(relative, old, old.replace("1.99.0", "1.97.1"))
                self.assertTrue(any(relative in problem for problem in problems))

    def test_builder_digest_drift_is_rejected(self) -> None:
        for relative in ("deploy/Dockerfile", "tools/container-probe/Dockerfile"):
            with self.subTest(relative=relative):
                problems = self.changed_contract(relative, CONTRACT.RUST_IMAGE,
                                                 CONTRACT.RUST_IMAGE.replace("b36c", "0000", 1))
                self.assertTrue(any("digest-pinned" in problem for problem in problems))

    def test_wasm_bindgen_cli_must_match_locked_library(self) -> None:
        problems = self.changed_contract("deploy/Dockerfile", "--version 0.2.127", "--version 0.2.126")
        self.assertTrue(any("CLI must match" in problem for problem in problems))


if __name__ == "__main__":
    unittest.main()
